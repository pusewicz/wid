//! Explicit overload sets (`overload :name, :a, :b`), package-level operator
//! functions, `using` fields and modules mixed in with `include`.

use std::rc::Rc;

use wid_diagnostics::{Applicability, Diagnostic, Span, and_list, codes, did_you_mean};
use wid_syntax::Name;
use wid_syntax::ast::{self, splice_index};

use super::expr::is_untyped;
use super::generics::{collect_params, lookup};
use super::macros::MacroCall;
use super::{Checker, DeclId, DeclKind, FnSig};
use crate::ir::{self, ExprKind};
use crate::types::{TyId, TyKind};

/// How well an argument fits a parameter. A macro's `Code` parameter takes
/// any argument (`Code`), below every typed fit.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fit {
    No,
    Code,
    Converts,
    Exact,
}

/// How well a member fits a call: its exact parameter matches, then its
/// typed ones (exact or converting), so a member whose `Code` parameter
/// takes an argument ranks below one with a typed parameter for it.
type Score = (usize, usize);

/// An overload member that fits a call: its declaration, bindings,
/// instantiated signature and [`Score`].
type Scored = (DeclId, Vec<(Name, TyId)>, FnSig, Score);

/// The score of a member whose parameters fit the arguments like `fits`,
/// or `None` when one doesn't fit.
fn score(fits: &[Fit]) -> Option<Score> {
    if fits.contains(&Fit::No) {
        return None;
    }
    let exact = fits.iter().filter(|f| **f == Fit::Exact).count();
    let typed = fits.iter().filter(|f| **f != Fit::Code).count();
    Some((exact, typed))
}

/// Keeps the members that fit best.
fn keep_best(scored: &mut Vec<Scored>) {
    let best = scored.iter().map(|s| s.3).max();
    scored.retain(|s| Some(s.3) == best);
}

/// What choosing among an overload set's members learned about one
/// argument of a call that may expand a macro (see
/// [`Checker::choose_with_macros`]).
#[derive(Clone)]
enum Probe {
    /// The argument names a type.
    Type(TyId),
    /// The argument's value, lowered without keeping its code.
    Value(ir::Expr),
}

/// The fix for an ambiguous `using` member: write `replacement` over
/// `span`, shown in the help as `like`.
pub struct UsingFix {
    pub like: String,
    pub span: Span,
    pub replacement: String,
}

impl Checker<'_> {
    /// The functions an overload set names, resolved in its scope. Problems
    /// with the set are reported once, the first time it is resolved.
    pub fn overload_members(&mut self, set: DeclId) -> Vec<DeclId> {
        if let Some(members) = self.overload_sets.get(&set) {
            return members.clone();
        }
        self.overload_sets.insert(set, Vec::new());
        let d = self.decls[set.0 as usize].clone();
        let DeclKind::Overload(o) = d.kind else { return Vec::new() };
        let mut out: Vec<DeclId> = Vec::new();
        for m in &o.members {
            let found = match d.owner {
                Some(owner) => self.members.get(&owner).and_then(|s| s.get(&m.name)).copied(),
                None => self.lookup_pkg(d.loc.pkg, m.name),
            };
            let Some(decl) = found else {
                let candidates = self.scope_method_names(d.owner, d.loc.pkg);
                let mut diag = Diagnostic::error(
                    codes::UNDEFINED_NAME,
                    format!("overload set `{}` names unknown method `{}`", o.name.as_str(), m.as_str()),
                )
                .primary(m.span, "no method with this name next to the set")
                .note("an overload set lists methods declared in the same scope as the `overload` line");
                if let Some(best) = did_you_mean(m.as_str(), candidates.iter().copied()) {
                    diag = diag.suggest_replace(
                        format!("a similar method exists: `{best}`"),
                        m.span,
                        format!(":{best}"),
                        Applicability::MaybeIncorrect,
                    );
                }
                self.report(diag);
                continue;
            };
            self.note_ref(m.span, decl, crate::uses::RefKind::Read);
            let DeclKind::Fn(f) = self.decls[decl.0 as usize].kind else {
                let what = self.decls[decl.0 as usize].kind.a_describe();
                let def_span = self.decls[decl.0 as usize].span;
                self.report(
                    Diagnostic::error(codes::NOT_CALLABLE, format!("`{}` is {what}, not a method", m.as_str()))
                        .primary(m.span, "overload sets list methods")
                        .secondary(def_span, format!("`{}` is declared here", m.as_str()))
                        .help("list only `def`s declared next to the `overload` line"),
                );
                continue;
            };
            if out.contains(&decl) {
                self.report(
                    Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("`{}` is listed twice in `{}`", m.as_str(), o.name.as_str()),
                    )
                    .primary(m.span, "listed again here")
                    .help("remove the repeated name"),
                );
                continue;
            }
            if f.is_macro && is_operator(o.name.as_str()) {
                let def_span = self.decls[decl.0 as usize].span;
                self.report(
                    Diagnostic::error(
                        codes::OPERATOR_MACRO,
                        format!("the macro `{}` can't be part of the operator set `{}`", m.as_str(), o.name.as_str()),
                    )
                    .primary(m.span, "a macro in an operator's set")
                    .secondary(def_span, "declared with `macro def` here")
                    .note("operators are methods of types, which the program runs; a macro only expands where it is called by name")
                    .help(format!(
                        "list `def`s in `{}`, which may call the macro, or call the macro by its own name",
                        o.name.as_str()
                    )),
                );
                continue;
            }
            let mut own = Vec::new();
            for p in &f.params {
                collect_params(&p.ty, &mut own);
            }
            let splat = f.params.iter().any(|p| p.splat);
            if f.block.is_some() || !own.is_empty() || splat {
                let reason = if f.block.is_some() {
                    "it takes a block"
                } else if splat {
                    "it collects arguments with a `*` parameter"
                } else {
                    "it has `$` type parameters"
                };
                let def_span = self.decls[decl.0 as usize].span;
                self.report(
                    Diagnostic::error(
                        codes::NO_MATCHING_OVERLOAD,
                        format!("`{}` cannot be part of an overload set because {reason}", m.as_str()),
                    )
                    .primary(m.span, "not allowed in an overload set")
                    .secondary(def_span, "declared here")
                    .note(if splat {
                        "a call picks a member by the number and types of its arguments, which a `*` parameter leaves open"
                    } else {
                        "a call picks a member by its argument types, which needs concrete parameter types"
                    })
                    .help(format!("remove `:{}` from the set and call it by its own name", m.as_str())),
                );
                continue;
            }
            // Two members taking the same types could never be chosen
            // between; only the first is kept, so calls are not reported too.
            let sig = self.fn_sig(decl);
            let same = out.iter().copied().find(|other| {
                let s = self.sigs.get(other).cloned();
                s.is_some_and(|s| {
                    s.receiver.is_some() == sig.receiver.is_some()
                        && s.params.len() == sig.params.len()
                        && s.params.iter().zip(&sig.params).all(|(a, b)| a.ty == b.ty)
                })
            });
            if let Some(other) = same {
                let other_name = self.decls[other.0 as usize].name;
                let other_span = o.members.iter().find(|x| x.name == other_name).map_or(o.name.span, |x| x.span);
                let types: Vec<String> = sig.params.iter().map(|p| self.types.display(p.ty)).collect();
                self.report(
                    Diagnostic::error(
                        codes::NO_MATCHING_OVERLOAD,
                        format!(
                            "`{other_name}` and `{}` in `{}` both take ({})",
                            m.as_str(),
                            o.name.as_str(),
                            types.join(", ")
                        ),
                    )
                    .primary(m.span, "no call could choose between these")
                    .secondary(other_span, "this member takes the same types")
                    .help("change one member's parameter types, or remove it from the set"),
                );
                continue;
            }
            out.push(decl);
        }
        self.overload_sets.insert(set, out.clone());
        out
    }

    /// Checks an overload set's declaration: its members resolve, and no two
    /// of them take the same parameter types. Resolving the members reports
    /// every problem once.
    pub fn check_overload_set(&mut self, set: DeclId) {
        self.overload_members(set);
    }

    /// Method names declared in a type, or names in a package, for "did you
    /// mean" hints, sorted, since ties in a suggestion go to the first.
    fn scope_method_names(&self, owner: Option<DeclId>, pkg: crate::input::PackageId) -> Vec<&'static str> {
        match owner {
            Some(o) => {
                let mut names: Vec<&'static str> =
                    self.members.get(&o).map(|m| m.keys().map(|n| n.as_str()).collect()).unwrap_or_default();
                names.sort_unstable();
                names
            }
            None => self.package_names(pkg),
        }
    }

    /// The bindings a member of an overload set is instantiated with: the
    /// receiver type's generic arguments and `Self`.
    fn overload_subst(&self, decl: DeclId, owner: Option<TyId>) -> Vec<(Name, TyId)> {
        let Some(owner) = owner else { return Vec::new() };
        let mut bindings = self.instance_bindings(owner);
        bindings.push((Name::new("Self"), owner));
        self.generic_names(decl).into_iter().filter_map(|n| lookup(&bindings, n).map(|t| (n, t))).collect()
    }

    fn fit(&mut self, arg: &ast::Expr, value: &ir::Expr, param: TyId) -> Fit {
        if value.ty == param {
            return Fit::Exact;
        }
        if is_untyped(arg) {
            let base = self.types.base(param);
            return match (&value.kind, self.types.kind(base)) {
                (ExprKind::Int(_), TyKind::Int(_) | TyKind::Float(_)) => Fit::Converts,
                (ExprKind::Float(_), TyKind::Float(_)) => Fit::Converts,
                (ExprKind::Nil, _) if self.types.is_nilable(param) => Fit::Converts,
                (ExprKind::Zero, _) => Fit::Converts,
                _ => Fit::No,
            };
        }
        match (self.types.kind(value.ty).clone(), self.types.kind(param).clone()) {
            (_, TyKind::Optional(inner)) if inner == value.ty => Fit::Converts,
            (TyKind::Array(a, _) | TyKind::Dynamic(a), TyKind::Slice(b)) if a == b => Fit::Converts,
            (TyKind::Nil, _) if self.types.is_nilable(param) => Fit::Converts,
            (_, TyKind::Union(_)) if self.union_variant(param, value.ty).is_some() => Fit::Converts,
            _ => Fit::No,
        }
    }

    /// Calls the member of an overload set that fits the arguments best.
    /// `receiver` is the `^Self` pointer for instance methods, and `owner`
    /// the type whose methods the set groups.
    pub fn call_overloaded(
        &mut self,
        set: DeclId,
        receiver: Option<ir::Expr>,
        owner: Option<TyId>,
        args: &[ast::Arg],
        name_span: Span,
        span: Span,
    ) -> ir::Expr {
        self.reject_named_args(args);
        let values = self.lower_overload_args(args);
        let exprs: Vec<&ast::Expr> = args.iter().map(|a| &a.value).collect();
        self.call_with_values(set, receiver, owner, &exprs, values, name_span, span)
    }

    /// Calls a package-level overload set, written `name(args)` or
    /// `pkg.name(args)` (`shown`). When macros are among its members, the
    /// member is chosen before the arguments are lowered
    /// ([`Checker::choose_with_macros`]), since a macro takes its
    /// arguments as code, symbols, types or compile-time values. A macro
    /// chosen expands with the expected type.
    pub fn call_package_set(
        &mut self,
        set: DeclId,
        shown: &str,
        args: &[ast::Arg],
        (name_span, span): (Span, Span),
        expected: Option<TyId>,
    ) -> ir::Expr {
        self.note_ref(name_span, set, crate::uses::RefKind::Call);
        if !self.set_has_macros(set) {
            return self.call_overloaded(set, None, None, args, name_span, span);
        }
        let call = MacroCall { decl: set, shown: shown.to_string(), args, block: None, name_span, span };
        let Some((chosen, sig)) = self.choose_for_call(&call) else {
            // The macro it was meant to call may have declared variables.
            self.failed_expansion(span);
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        };
        if self.is_macro(chosen) {
            let call = self.chosen_macro_call(call, chosen);
            return self.call_macro(call, expected);
        }
        let values = self.lower_overload_args(args);
        let exprs: Vec<&ast::Expr> = args.iter().map(|a| &a.value).collect();
        self.call_member(chosen, (Vec::new(), sig), (None, None), &exprs, values, (name_span, span))
    }

    /// Whether a declaration is an overload set with macros among its
    /// members.
    pub(super) fn set_has_macros(&mut self, decl: DeclId) -> bool {
        matches!(self.decls[decl.0 as usize].kind, DeclKind::Overload(_))
            && self.overload_members(decl).iter().any(|&m| self.is_macro(m))
    }

    /// Chooses the member that a call of an overload set with macros among
    /// its members (`call.decl`) runs, with its signature, before any
    /// argument is lowered for the call ([`Checker::choose_with_macros`]).
    /// `None` means an error was reported.
    pub(super) fn choose_for_call(&mut self, call: &MacroCall<'_>) -> Option<(DeclId, FnSig)> {
        if self.reject_named_args(call.args) {
            return None;
        }
        let members = self.overload_members(call.decl);
        let (chosen, sig) = self.choose_with_macros(call.decl, &members, call.args, call.span)?;
        if self.is_macro(chosen) {
            self.check_visible(chosen, call.name_span);
        }
        Some((chosen, sig))
    }

    /// The call of the macro that a call of an overload set chose, shown by
    /// the macro's name (qualified like the call's).
    pub(super) fn chosen_macro_call<'e>(&self, call: MacroCall<'e>, member: DeclId) -> MacroCall<'e> {
        let name = self.decls[member.0 as usize].name;
        let shown = match call.shown.rsplit_once('.') {
            Some((pkg, _)) => format!("{pkg}.{name}"),
            None => name.to_string(),
        };
        MacroCall { decl: member, shown, ..call }
    }

    /// Reports named arguments in a call of an overload set, whose members
    /// are chosen by the arguments' types. Returns whether there were any.
    fn reject_named_args(&mut self, args: &[ast::Arg]) -> bool {
        let Some(named) = args.iter().find_map(|a| a.name) else { return false };
        self.report(
            Diagnostic::error(codes::BAD_NAMED_ARG, "overloaded calls take positional arguments")
                .primary(named.span, "the argument types choose the method, not the names")
                .help(format!("remove `{}:`", named.as_str())),
        );
        true
    }

    /// Lowers the arguments of an overloaded call in order, spilling earlier
    /// ones that a later one could change.
    fn lower_overload_args(&mut self, args: &[ast::Arg]) -> Vec<ir::Expr> {
        let mut values = Vec::new();
        for a in args {
            self.begin_block();
            let v = self.expr(&a.value, None);
            let stmts = self.end_block().stmts;
            if !stmts.is_empty() || !v.is_pure() {
                self.spill_impure(&mut values);
            }
            for s in stmts {
                self.emit(s);
            }
            values.push(v);
        }
        values
    }

    /// Chooses the member of a package-level overload set with macros among
    /// its members, before any argument is lowered for the call. A macro's
    /// parameters take arguments by kind: a `Code` parameter any argument
    /// (ranking below every typed fit), a `Symbol` parameter a symbol
    /// literal and a `Type` parameter a type. Its other parameters, and a
    /// `def`'s, take values by type as usual; an argument is lowered to
    /// find its type, without keeping the code, only when a member needs
    /// it. Reports a call that no member, or several, fit. `None` means an
    /// error was reported.
    fn choose_with_macros(
        &mut self,
        set: DeclId,
        members: &[DeclId],
        args: &[ast::Arg],
        span: Span,
    ) -> Option<(DeclId, FnSig)> {
        let exprs: Vec<&ast::Expr> = args.iter().map(|a| &a.value).collect();
        let mut probes: Vec<Option<Probe>> = vec![None; exprs.len()];
        let mut scored: Vec<Scored> = Vec::new();
        for &c in members {
            let sig = self.fn_sig(c);
            if sig.params.len() != exprs.len() || sig.receiver.is_some() {
                continue;
            }
            let is_macro = self.is_macro(c);
            let mut fits = Vec::with_capacity(exprs.len());
            for (i, p) in sig.params.iter().enumerate() {
                fits.push(self.probe_fit(&mut probes, &exprs, i, p.ty, is_macro));
            }
            if let Some(score) = score(&fits) {
                scored.push((c, Vec::new(), sig, score));
            }
        }
        // An argument with errors was reported while it was lowered.
        if probes.iter().flatten().any(|p| self.probe_failed(p)) {
            return None;
        }
        keep_best(&mut scored);
        if scored.len() == 1 {
            let (chosen, _, sig, _) = scored.pop().expect("invariant: exactly one member fits best");
            return Some((chosen, sig));
        }
        // The report shows every argument's type.
        let mut values = Vec::with_capacity(exprs.len());
        for i in 0..exprs.len() {
            match self.probe(&mut probes, &exprs, i) {
                Probe::Value(v) => values.push(v),
                Probe::Type(t) => values.push(ir::Expr::new(ExprKind::Int(i128::from(t.0)), self.types.type_ty())),
            }
        }
        if values.iter().any(|v| matches!(self.types.kind(v.ty), TyKind::Unknown)) {
            return None;
        }
        let set_name = self.decls[set.0 as usize].name;
        self.report_overload_failure(set_name, members, &scored, &exprs, &values, None, false, span);
        None
    }

    /// How argument `i` fits a parameter of type `param` of a member that
    /// is a macro (`is_macro`) or not, probing the argument once if needed.
    fn probe_fit(
        &mut self,
        probes: &mut [Option<Probe>],
        exprs: &[&ast::Expr],
        i: usize,
        param: TyId,
        is_macro: bool,
    ) -> Fit {
        let kind = self.types.kind(self.types.base(param)).clone();
        if is_macro {
            match kind {
                TyKind::Code => return Fit::Code,
                TyKind::Symbol => {
                    let literal = matches!(exprs[i].kind, ast::ExprKind::Symbol(n) if splice_index(n).is_none());
                    return if literal { Fit::Exact } else { Fit::No };
                }
                _ => {}
            }
        }
        match self.probe(probes, exprs, i) {
            Probe::Type(_) if matches!(kind, TyKind::Type) => Fit::Exact,
            Probe::Type(_) => Fit::No,
            Probe::Value(v) if matches!(self.types.kind(v.ty), TyKind::Unknown) => Fit::No,
            Probe::Value(v) => self.fit(exprs[i], &v, param),
        }
    }

    /// What argument `i` of a call is: a type it names, or its value,
    /// lowered without keeping the code (once).
    fn probe(&mut self, probes: &mut [Option<Probe>], exprs: &[&ast::Expr], i: usize) -> Probe {
        if let Some(p) = &probes[i] {
            return p.clone();
        }
        let p = match self.named_type(exprs[i]) {
            Some(t) => Probe::Type(t),
            None => {
                self.begin_block();
                let v = self.expr(exprs[i], None);
                self.end_block();
                Probe::Value(v)
            }
        };
        probes[i] = Some(p.clone());
        p
    }

    /// Whether probing an argument reported an error.
    fn probe_failed(&self, probe: &Probe) -> bool {
        let ty = match probe {
            Probe::Type(t) => *t,
            Probe::Value(v) => v.ty,
        };
        matches!(self.types.kind(ty), TyKind::Unknown)
    }

    /// Calls a method, or the best-fitting member of an overload set, with
    /// arguments that are already lowered. `args` are the argument
    /// expressions the values came from, used to treat untyped literals and
    /// for suggestions. Overload resolution picks the member with the most
    /// exact parameter matches, where an untyped literal matches its default
    /// type exactly and converts to other number types.
    #[expect(clippy::too_many_arguments, reason = "mirrors the parts of a call expression")]
    pub fn call_with_values(
        &mut self,
        decl: DeclId,
        receiver: Option<ir::Expr>,
        owner: Option<TyId>,
        args: &[&ast::Expr],
        values: Vec<ir::Expr>,
        name_span: Span,
        span: Span,
    ) -> ir::Expr {
        self.note_ref(name_span, decl, crate::uses::RefKind::Call);
        if values.iter().any(|v| matches!(self.types.kind(v.ty), TyKind::Unknown)) {
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        let (chosen, subst, sig) = match self.decls[decl.0 as usize].kind {
            DeclKind::Overload(_) => {
                let candidates = self.overload_members(decl);
                let mut scored: Vec<Scored> = Vec::new();
                for &c in &candidates {
                    let subst = self.overload_subst(c, owner);
                    let sig = self.fn_sig_inst(c, &subst);
                    if sig.params.len() != values.len() || sig.receiver.is_some() != receiver.is_some() {
                        continue;
                    }
                    let fits: Vec<Fit> =
                        args.iter().zip(&values).zip(&sig.params).map(|((a, v), p)| self.fit(a, v, p.ty)).collect();
                    if let Some(score) = score(&fits) {
                        scored.push((c, subst, sig, score));
                    }
                }
                keep_best(&mut scored);
                if scored.len() != 1 {
                    let set_name = self.decls[decl.0 as usize].name;
                    let has_receiver = receiver.is_some();
                    self.report_overload_failure(
                        set_name,
                        &candidates,
                        &scored,
                        args,
                        &values,
                        owner,
                        has_receiver,
                        span,
                    );
                    return ir::Expr::new(ExprKind::Zero, self.types.unknown());
                }
                let (chosen, subst, sig, _) = scored.pop().expect("invariant: exactly one member fits best");
                (chosen, subst, sig)
            }
            _ => {
                let subst = self.overload_subst(decl, owner);
                let sig = self.fn_sig_inst(decl, &subst);
                if sig.params.len() != values.len() {
                    let name = self.decls[decl.0 as usize].name;
                    let def_span = self.decls[decl.0 as usize].span;
                    self.report(
                        Diagnostic::error(
                            codes::ARG_COUNT,
                            format!("`{name}` takes {} arguments here, but gets {}", sig.params.len(), values.len()),
                        )
                        .primary(span, format!("passes {} arguments", values.len()))
                        .secondary(def_span, format!("`{}`", self.signature_text(name, &sig.params))),
                    );
                    return ir::Expr::new(ExprKind::Zero, self.types.unknown());
                }
                (decl, subst, sig)
            }
        };
        self.call_member(chosen, (subst, sig), (receiver, owner), args, values, (name_span, span))
    }

    /// Calls a method chosen for a call, with its bindings and instantiated
    /// signature, the receiver and owner as for
    /// [`Checker::call_with_values`], and arguments that are already
    /// lowered.
    fn call_member(
        &mut self,
        chosen: DeclId,
        (subst, sig): (Vec<(Name, TyId)>, FnSig),
        (receiver, owner): (Option<ir::Expr>, Option<TyId>),
        args: &[&ast::Expr],
        values: Vec<ir::Expr>,
        (name_span, span): (Span, Span),
    ) -> ir::Expr {
        let mut lowered: Vec<ir::Expr> = receiver.into_iter().collect();
        for ((a, v), p) in args.iter().zip(values).zip(&sig.params) {
            let v = if is_untyped(a) { self.expr_coerced(a, p.ty) } else { self.coerce(v, p.ty, a.span) };
            lowered.push(v);
        }
        self.check_visible(chosen, name_span);
        self.note_ref(name_span, chosen, crate::uses::RefKind::Call);
        if let Some(owner) = owner {
            self.check_private_method(chosen, owner, name_span);
        }
        let func = if subst.is_empty() {
            self.fn_instance(chosen)
        } else {
            self.fn_instance_with(chosen, Rc::new(subst), span)
        };
        ir::Expr::new(ExprKind::Call { func, args: lowered }, sig.ret)
    }

    /// Applies a binary operator to a lowered left operand and the right
    /// operand `rhs` (lowered as `r`): an operator method of the left type,
    /// a package-level operator function, or the builtin operator.
    pub fn combine_values(
        &mut self,
        op: ast::BinOp,
        l: ir::Expr,
        rhs: &ast::Expr,
        r: ir::Expr,
        lhs_span: Span,
        span: Span,
    ) -> ir::Expr {
        if let Some(decl) = self.operator_method(l.ty, op.as_str()) {
            let owner = match self.types.kind(l.ty) {
                TyKind::Pointer(t) => *t,
                _ => l.ty,
            };
            let recv = self.address_of(l);
            return self.call_with_values(decl, Some(recv), Some(owner), &[rhs], vec![r], lhs_span, span);
        }
        if let Some(decl) = self.package_operator(op.as_str(), l.ty, r.ty) {
            self.note_ref(span, decl, crate::uses::RefKind::Call);
            return self.call_operator_fn(decl, l, r);
        }
        self.binary_values(op, l, r, lhs_span, rhs.span, span)
    }

    /// The type to lower the right operand of `l op rhs` with: the parameter
    /// type of a single operator method or package operator for the left
    /// type, nothing for an overload set (its members decide), and the left
    /// type for builtin operators.
    pub fn rhs_expected(&mut self, op: ast::BinOp, l: TyId) -> Option<TyId> {
        if let Some(decl) = self.operator_method(l, op.as_str()) {
            return match self.decls[decl.0 as usize].kind {
                DeclKind::Fn(_) => {
                    let owner = match self.types.kind(l) {
                        TyKind::Pointer(t) => *t,
                        _ => l,
                    };
                    let subst = self.overload_subst(decl, Some(owner));
                    self.fn_sig_inst(decl, &subst).params.first().map(|p| p.ty)
                }
                _ => None,
            };
        }
        if self.types.is_numeric(l) {
            return Some(l);
        }
        Some(self.package_operand_type(op.as_str(), l, true).unwrap_or(l))
    }

    /// Reports an overloaded call that no member, or several members, fit.
    #[expect(clippy::too_many_arguments, reason = "the parts of the failed call")]
    fn report_overload_failure(
        &mut self,
        set_name: Name,
        candidates: &[DeclId],
        tied: &[Scored],
        args: &[&ast::Expr],
        values: &[ir::Expr],
        owner: Option<TyId>,
        has_receiver: bool,
        span: Span,
    ) {
        let arg_types: Vec<String> = args
            .iter()
            .zip(values)
            .map(|(a, v)| match (&v.kind, is_untyped(a)) {
                (ExprKind::Int(_), true) => "integer literal".to_string(),
                (ExprKind::Float(_), true) => "float literal".to_string(),
                _ => self.types.display(v.ty),
            })
            .collect();
        let mut sigs: Vec<(DeclId, FnSig)> = Vec::new();
        for c in candidates {
            let subst = self.overload_subst(*c, owner);
            sigs.push((*c, self.fn_sig_inst(*c, &subst)));
        }
        let listing: Vec<String> = sigs
            .iter()
            .map(|(c, sig)| format!("`{}`", self.signature_text(self.decls[c.0 as usize].name, &sig.params)))
            .collect();
        let shown_args = arg_types.join(", ");
        let mut diag = if tied.is_empty() {
            Diagnostic::error(codes::NO_MATCHING_OVERLOAD, format!("no member of `{set_name}` takes ({shown_args})"))
                .primary(span, format!("called with ({shown_args})"))
                .note(format!("the members are {}", listing.join(", ")))
        } else {
            let names: Vec<String> = tied.iter().map(|t| format!("`{}`", self.decls[t.0.0 as usize].name)).collect();
            Diagnostic::error(
                codes::NO_MATCHING_OVERLOAD,
                format!("several members of `{set_name}` take ({shown_args})"),
            )
            .primary(span, format!("{} fit equally well", and_list(&names)))
            .note(format!("the members are {}", listing.join(", ")))
        };
        let conversion = if tied.is_empty() {
            let same_shape: Vec<&FnSig> = sigs
                .iter()
                .map(|(_, s)| s)
                .filter(|s| s.params.len() == values.len() && s.receiver.is_some() == has_receiver)
                .collect();
            match same_shape.as_slice() {
                [only] => values
                    .iter()
                    .zip(&only.params)
                    .position(|(v, p)| v.ty != p.ty)
                    .filter(|&i| self.types.is_numeric(values[i].ty) && self.types.is_numeric(only.params[i].ty))
                    .map(|i| (i, only.params[i].ty)),
                _ => None,
            }
        } else {
            // A macro's `Code`, `Symbol` or `Type` parameter takes an
            // argument by its kind, which no conversion changes.
            let by_kind = |t: TyId| matches!(self.types.kind(t), TyKind::Code | TyKind::Symbol | TyKind::Type);
            (0..values.len())
                .find(|&i| tied.iter().any(|t| t.2.params[i].ty != tied[0].2.params[i].ty))
                .filter(|&i| !tied.iter().any(|t| by_kind(t.2.params[i].ty)))
                .map(|i| (i, tied[0].2.params[i].ty))
        };
        diag = match conversion {
            Some((i, ty)) => {
                let shown = self.types.display(ty);
                let src = self.source_text(args[i].span);
                let simple =
                    matches!(args[i].kind, ast::ExprKind::Int(_) | ast::ExprKind::Float(_) | ast::ExprKind::Ident(_));
                let replacement = if simple { format!("{src}.to({shown})") } else { format!("({src}).to({shown})") };
                diag.suggest_replace(
                    format!("convert the argument to `{shown}`"),
                    args[i].span,
                    replacement,
                    Applicability::MaybeIncorrect,
                )
            }
            None if tied.is_empty() => {
                diag.help("convert an argument with `.to(T)` so that one member's parameter types match")
            }
            None => {
                let first = self.decls[tied[0].0.0 as usize].name;
                diag.help(format!("call one member by name, like `{first}(…)`"))
            }
        };
        self.report(diag);
    }

    /// Finds a package-level operator function, like `def *(s: F32, v: Vec2)`,
    /// for operands of these types.
    pub fn package_operator(&mut self, op: &str, l: TyId, r: TyId) -> Option<DeclId> {
        self.package_operator_candidates(op)
            .into_iter()
            .find(|(_, sig)| sig.params[0].ty == l && sig.params[1].ty == r)
            .map(|(d, _)| d)
    }

    /// The type one operand of a package-level operator needs when the other
    /// operand's type is known, if exactly one operator takes that type.
    /// `known_left` says whether `known` is the left operand.
    pub fn package_operand_type(&mut self, op: &str, known: TyId, known_left: bool) -> Option<TyId> {
        let (k, u) = if known_left { (0, 1) } else { (1, 0) };
        let matching: Vec<TyId> = self
            .package_operator_candidates(op)
            .into_iter()
            .filter(|(_, sig)| sig.params[k].ty == known)
            .map(|(_, sig)| sig.params[u].ty)
            .collect();
        (matching.len() == 1).then(|| matching[0])
    }

    /// Two-parameter package-level functions named `op`, alone or in an
    /// overload set, visible from the current package.
    fn package_operator_candidates(&mut self, op: &str) -> Vec<(DeclId, FnSig)> {
        let loc = self.loc();
        let name = Name::new(op);
        let Some(decl) = self.lookup_pkg(loc.pkg, name).or_else(|| self.lookup_prelude(name)) else {
            return Vec::new();
        };
        let candidates = match self.decls[decl.0 as usize].kind {
            DeclKind::Fn(_) => vec![decl],
            DeclKind::Overload(_) => self.overload_members(decl),
            _ => return Vec::new(),
        };
        let mut out = Vec::new();
        for c in candidates {
            // A macro named like an operator is reported where it is
            // declared (E0915); operators are never macros.
            if !self.generic_names(c).is_empty() || self.is_macro(c) {
                continue;
            }
            let sig = self.fn_sig(c);
            if sig.receiver.is_none() && sig.params.len() == 2 {
                out.push((c, sig));
            }
        }
        out
    }

    // ----- using fields ---------------------------------------------------------

    /// Finds `name` through a struct's `using` fields: a field of the used
    /// struct, one of its methods, or something it promotes in turn. Returns
    /// the index and type of the `using` field to go through.
    pub fn using_lookup(&mut self, ty: TyId, name: Name, span: Span) -> Option<(u32, TyId)> {
        let hits = self.using_hits(ty, name);
        if let [(_, _, first), _, ..] = hits[..] {
            let fix = UsingFix { like: format!(".{first}.{name}"), span, replacement: format!("{first}.{name}") };
            self.ambiguous_using(ty, name, span, &hits, fix);
        }
        hits.first().map(|(i, t, _)| (*i, *t))
    }

    /// The `using` fields of `ty` that provide `name`, in declaration order:
    /// the index, type and name of each.
    pub fn using_hits(&mut self, ty: TyId, name: Name) -> Vec<(u32, TyId, Name)> {
        let TyKind::Struct(id) = *self.types.kind(ty) else { return Vec::new() };
        let info = self.types.struct_info(id).clone();
        let mut hits = Vec::new();
        for (i, f) in info.fields.iter().enumerate() {
            if !f.using {
                continue;
            }
            let mut visited = vec![ty];
            if self.provides(f.ty, name, &mut visited) {
                hits.push((i as u32, f.ty, f.name));
            }
        }
        hits
    }

    /// Reports `name` written at `span` when several `using` fields of `ty`
    /// provide it (`hits`, at least two), with `fix` reaching it through the
    /// first.
    pub fn ambiguous_using(&mut self, ty: TyId, name: Name, span: Span, hits: &[(u32, TyId, Name)], fix: UsingFix) {
        let names: Vec<String> = hits.iter().map(|(_, _, n)| format!("`{n}`")).collect();
        let label = match names.as_slice() {
            [a, b] => format!("both {a} and {b} provide `{name}`"),
            _ => format!("{} all provide `{name}`", and_list(&names)),
        };
        let shown = self.types.display(ty);
        self.report(
            Diagnostic::error(codes::NO_SUCH_MEMBER, format!("`{name}` is ambiguous in `{shown}`"))
                .primary(span, label)
                .note("`using` promotes the members of several fields here, and more than one has this name")
                .suggest_replace(
                    format!("name the field to read it through, like `{}`", fix.like),
                    fix.span,
                    fix.replacement,
                    Applicability::MaybeIncorrect,
                ),
        );
    }

    /// Returns true when a value of type `ty` (or the struct it points to)
    /// has a field or method `name`, directly or through its own `using`
    /// fields.
    fn provides(&mut self, ty: TyId, name: Name, visited: &mut Vec<TyId>) -> bool {
        let inner = match self.types.kind(ty) {
            TyKind::Pointer(t) => *t,
            _ => ty,
        };
        if visited.contains(&inner) {
            return false;
        }
        visited.push(inner);
        if self.field_index(inner, name).is_some()
            || self.find_method(inner, name).is_some()
            || self.find_included(inner, name).is_some()
        {
            return true;
        }
        let TyKind::Struct(id) = *self.types.kind(inner) else { return false };
        let fields: Vec<TyId> = self.types.struct_info(id).fields.iter().filter(|f| f.using).map(|f| f.ty).collect();
        fields.into_iter().any(|f| self.provides(f, name, visited))
    }

    // ----- modules ----------------------------------------------------------------

    /// Finds a method that a struct or enum mixes in with `include`, searching
    /// included modules in order, and the modules they include in turn.
    pub fn find_included(&mut self, ty: TyId, name: Name) -> Option<DeclId> {
        let decl = self.type_decl(ty)?;
        let mut visited = Vec::new();
        self.module_member(decl, name, &mut visited)
    }

    /// Finds `name` in the modules `owner` includes.
    pub fn module_member(&mut self, owner: DeclId, name: Name, visited: &mut Vec<DeclId>) -> Option<DeclId> {
        for m in self.includes_of(owner) {
            if visited.contains(&m) {
                continue;
            }
            visited.push(m);
            if let Some(found) = self.members.get(&m).and_then(|s| s.get(&name)).copied() {
                return Some(found);
            }
            if let Some(found) = self.module_member(m, name, visited) {
                return Some(found);
            }
        }
        None
    }

    /// The declaration of a struct or enum type.
    pub(super) fn type_decl(&self, ty: TyId) -> Option<DeclId> {
        match self.types.kind(ty) {
            TyKind::Struct(id) => self.struct_decls.get(id).copied(),
            TyKind::Enum(id) => self.enum_decls.get(id).copied(),
            _ => None,
        }
    }

    /// The modules a struct, enum, module or `extend` includes, resolved once.
    pub fn includes_of(&mut self, decl: DeclId) -> Vec<DeclId> {
        if let Some(m) = self.includes.get(&decl) {
            return m.clone();
        }
        self.includes.insert(decl, Vec::new());
        let items = self.include_items.get(&decl).cloned().unwrap_or_default();
        for item in items {
            self.resolve_include(decl, item);
        }
        self.includes.get(&decl).cloned().unwrap_or_default()
    }

    /// Resolves one `include` of a struct, enum, module or `extend` and adds
    /// the module to what it includes. The name resolves where the `include`
    /// is written, which for one a macro generated is the macro's package.
    pub(super) fn resolve_include(&mut self, decl: DeclId, item: &ast::Item) {
        let ast::ItemKind::Include(t) = &item.kind else { return };
        let d = self.decls[decl.0 as usize].clone();
        let loc = self.virtual_file(t.span.file).map_or(d.loc, |v| v.loc);
        {
            let ast::TypeKind::Path { segments, .. } = &t.kind else {
                self.report(
                    Diagnostic::error(codes::NOT_A_TYPE, "`include` takes a module name")
                        .primary(t.span, "not a module name")
                        .help("write `include Name`, where `Name` is declared with `module Name … end`"),
                );
                return;
            };
            let module = match segments.as_slice() {
                [only] => self.lookup_pkg(loc.pkg, only.name).or_else(|| self.lookup_prelude(only.name)),
                [pkg, name] => self.lookup_import(loc, pkg.name).and_then(|p| self.lookup_pkg(p, name.name)),
                _ => None,
            };
            match module {
                Some(m) if matches!(self.decls[m.0 as usize].kind, DeclKind::Module) => {
                    if m == decl {
                        self.report(
                            Diagnostic::error(codes::RECURSIVE_TYPE, "a module cannot include itself")
                                .primary(t.span, "this is the module being declared")
                                .help("remove this `include`"),
                        );
                        return;
                    }
                    if self.includes.get(&decl).is_some_and(|out| out.contains(&m)) {
                        self.report(
                            Diagnostic::error(codes::DUPLICATE_DEFINITION, "this module is already included")
                                .primary(t.span, "included again here")
                                .help("remove the repeated `include`"),
                        );
                        return;
                    }
                    self.check_visible_from(m, loc.pkg, t.span);
                    self.note_ref(segments.last().map_or(t.span, |s| s.span), m, crate::uses::RefKind::Type);
                    self.includes.entry(decl).or_default().push(m);
                }
                Some(m) => {
                    let what = self.decls[m.0 as usize].kind.a_describe();
                    let def_span = self.decls[m.0 as usize].span;
                    let diag =
                        Diagnostic::error(codes::NOT_A_TYPE, format!("`include` takes a module, but this is {what}"))
                            .primary(t.span, "not a module")
                            .secondary(
                                def_span,
                                format!("the {} is declared here", self.decls[m.0 as usize].kind.describe()),
                            );
                    let diag = if matches!(self.decls[m.0 as usize].kind, DeclKind::Struct(_)) {
                        let field = snake_case(self.decls[m.0 as usize].name.as_str());
                        let ty = self.source_text(t.span);
                        diag.suggest_replace(
                            "to reuse a struct's fields and methods, add a `using` field instead",
                            item.span,
                            format!("using {field}: {ty}"),
                            Applicability::MaybeIncorrect,
                        )
                    } else {
                        diag.help("declare the shared methods in a `module … end` and include that")
                    };
                    self.report(diag);
                }
                None => {
                    let name = segments.last().map_or_else(|| Name::new("?"), |s| s.name);
                    let candidates = self.package_names(loc.pkg);
                    self.undefined(name, t.span, candidates, "module");
                }
            }
        }
    }

    /// Reports calling a `private` method from outside its type. A private
    /// method can be called from methods of the type the call goes to,
    /// including methods mixed in from modules and added by `extend`.
    pub fn check_private_method(&mut self, decl: DeclId, recv_ty: TyId, span: Span) {
        let d = &self.decls[decl.0 as usize];
        if !d.private || d.owner.is_none() {
            return;
        }
        let owner = d.owner;
        let name = d.name;
        let def_span = d.span;
        let self_ty = self.body.frames.last().and_then(|f| f.self_ty);
        let inside = self_ty == Some(recv_ty) || self_ty.and_then(|t| self.type_decl(t)) == owner;
        if !inside {
            let shown = self.types.display(recv_ty);
            self.report(
                Diagnostic::error(codes::PRIVATE_ITEM, format!("`{name}` is private to `{shown}`"))
                    .primary(span, format!("called from outside `{shown}`"))
                    .secondary(def_span, "declared `private` here")
                    .help("call a public method instead, or remove `private` from the declaration"),
            );
        }
    }
}

/// Whether a method name is an operator's, like `+`, `<=>` or `[]=`.
pub(super) fn is_operator(name: &str) -> bool {
    super::operator_name(name, false).is_some() || super::operator_name(name, true).is_some()
}

/// `PlayerState` → `player_state`, for suggested field names.
fn snake_case(name: &str) -> String {
    let mut out = String::new();
    for (i, c) in name.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

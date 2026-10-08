//! Optionals and flow typing: narrowing facts, `guard`, `if v = …`, `||` on
//! optionals, `&.`, multiple return values and destructuring assignment.

use wid_diagnostics::{Applicability, Diagnostic, Edit, Span, codes};
use wid_syntax::Name;
use wid_syntax::ast::{self, ExprKind as E};

use super::Checker;
use super::body::Dest;
use super::runtime::holds_parse_error;
use crate::ir::{self, ExprKind, LocalId, Stmt};
use crate::types::{TyId, TyKind};

/// What a condition proves about optional locals.
#[derive(Default, Debug, Clone)]
pub(crate) struct Facts {
    /// Locals known to hold a value when the condition is true.
    pub when_true: Vec<LocalId>,
    /// Locals known to hold a value when the condition is false.
    pub when_false: Vec<LocalId>,
}

impl Facts {
    fn swap(self) -> Facts {
        Facts { when_true: self.when_false, when_false: self.when_true }
    }
}

/// How a guarded value signals failure.
enum GuardShape {
    /// `T?`: nil means failure; binds the value (or each tuple element).
    Optional { inner: TyId },
    /// `(A, …, E)` where `E` is nil-able: binds the other elements.
    TupleWithError { elems: Vec<TyId> },
    /// A bare `Error` (or other nil-able error value).
    Error,
    /// A plain `Bool` condition.
    Bool,
}

impl<'a> Checker<'a> {
    /// Returns the optional local an expression names, if any.
    fn optional_local(&mut self, e: &ast::Expr) -> Option<LocalId> {
        let E::Ident(name) = e.kind else { return None };
        let var = self.find_var_at(name, e.span)?;
        let (local, ty) = (var.local, var.ty);
        self.types.is_nilable(ty).then_some(local)
    }

    /// Computes the narrowing facts of a condition.
    pub fn facts(&mut self, cond: &ast::Expr) -> Facts {
        match &cond.kind {
            E::Paren(inner) => self.facts(inner),
            E::Ident(_) => match self.optional_local(cond) {
                Some(l) => Facts { when_true: vec![l], when_false: Vec::new() },
                None => Facts::default(),
            },
            E::Unary { op: ast::UnOp::Not, expr } => self.facts(expr).swap(),
            E::Member { .. } | E::Call(_) if self.is_nil_check(cond).is_some() => {
                match self.is_nil_check(cond).and_then(|e| self.optional_local(e)) {
                    Some(l) => Facts { when_true: Vec::new(), when_false: vec![l] },
                    None => Facts::default(),
                }
            }
            E::Binary { op: ast::BinOp::Eq | ast::BinOp::Ne, lhs, rhs } => {
                let (other, is_eq) = match (&lhs.kind, &rhs.kind) {
                    (_, E::Nil) => (lhs, matches!(cond.kind, E::Binary { op: ast::BinOp::Eq, .. })),
                    (E::Nil, _) => (rhs, matches!(cond.kind, E::Binary { op: ast::BinOp::Eq, .. })),
                    _ => return Facts::default(),
                };
                match self.optional_local(other) {
                    Some(l) if is_eq => Facts { when_true: Vec::new(), when_false: vec![l] },
                    Some(l) => Facts { when_true: vec![l], when_false: Vec::new() },
                    None => Facts::default(),
                }
            }
            E::Binary { op: ast::BinOp::And, lhs, rhs } => {
                let mut f = self.facts(lhs);
                f.when_true.extend(self.facts(rhs).when_true);
                Facts { when_true: f.when_true, when_false: Vec::new() }
            }
            E::Binary { op: ast::BinOp::Or, lhs, rhs } => {
                let mut f = self.facts(lhs);
                f.when_false.extend(self.facts(rhs).when_false);
                Facts { when_true: Vec::new(), when_false: f.when_false }
            }
            _ => Facts::default(),
        }
    }

    /// Returns the receiver of `x.nil?`.
    fn is_nil_check<'e>(&self, cond: &'e ast::Expr) -> Option<&'e ast::Expr> {
        match &cond.kind {
            E::Member { recv, name, .. } if name.as_str() == "nil?" => Some(recv),
            E::Call(call) if call.args.is_empty() => match &call.callee {
                ast::Callee::Method { recv, name, .. } if name.as_str() == "nil?" => Some(recv),
                _ => None,
            },
            _ => None,
        }
    }

    /// Narrows every local in `facts`.
    pub fn apply_facts(&mut self, locals: &[LocalId]) {
        for l in locals {
            self.narrow(*l);
        }
    }

    /// Forgets narrowing of every variable assigned anywhere in `body`, so
    /// a loop never reads a value that a later iteration replaced with nil.
    pub fn invalidate_assigned(&mut self, body: &[ast::Stmt]) {
        let mut names = Vec::new();
        collect_assigned(body, &mut names);
        for n in names {
            if let Some(local) = self.find_var(n).map(|v| v.local) {
                self.invalidate(local);
            }
        }
    }

    // ----- optional values ------------------------------------------------

    /// Returns the inner type of an optional.
    pub fn optional_inner(&self, ty: TyId) -> Option<TyId> {
        match self.types.kind(ty) {
            TyKind::Optional(inner) => Some(*inner),
            _ => None,
        }
    }

    /// Builds `has value` for any nil-able value.
    pub fn is_some(&mut self, v: ir::Expr) -> ir::Expr {
        let bool_ty = self.types.bool();
        match self.types.kind(self.types.base(v.ty)) {
            TyKind::Optional(_) => ir::Expr::new(ExprKind::OptIsSome(Box::new(v)), bool_ty),
            TyKind::Union(_) => {
                let u32_ty = self.types.intern(TyKind::Int(crate::types::IntTy::U32));
                let tag = ir::Expr::new(ExprKind::UnionTag(Box::new(v)), u32_ty);
                let zero = ir::Expr::new(ExprKind::Int(0), u32_ty);
                let span = Span::default();
                ir::Expr::new(
                    ExprKind::Binary { op: ir::BinaryOp::Ne, lhs: Box::new(tag), rhs: Box::new(zero), span },
                    bool_ty,
                )
            }
            _ => {
                let nil = ir::Expr::new(ExprKind::Nil, v.ty);
                let span = Span::default();
                ir::Expr::new(
                    ExprKind::Binary { op: ir::BinaryOp::Ne, lhs: Box::new(v), rhs: Box::new(nil), span },
                    bool_ty,
                )
            }
        }
    }

    /// Unwraps an optional known to hold a value.
    pub fn opt_get(&mut self, v: ir::Expr) -> ir::Expr {
        let inner = self.optional_inner(v.ty).unwrap_or(v.ty);
        ir::Expr::new(ExprKind::OptGet(Box::new(v)), inner)
    }

    /// Wraps a value into `T?`.
    pub fn opt_some(&mut self, v: ir::Expr, opt: TyId) -> ir::Expr {
        ir::Expr::new(ExprKind::OptSome(Box::new(v)), opt)
    }

    /// Reports use of a possibly-nil value where a value is required.
    pub fn maybe_nil(&mut self, v: &ir::Expr, span: Span) {
        let shown = self.types.display(v.ty);
        let local = match &v.kind {
            ExprKind::Local(l) => Some(*l),
            ExprKind::Deref(inner) => match inner.kind {
                ExprKind::Local(l) => Some(l),
                _ => None,
            },
            _ => None,
        };
        let (name, origin, decl_stmt) = match local {
            Some(l) => {
                let found = self
                    .body
                    .frames
                    .last()
                    .and_then(|f| f.scopes.iter().flat_map(|s| s.vars.iter()).find(|var| var.local == l))
                    .map(|var| (Some(var.name), var.origin.clone(), var.decl_stmt));
                found.unwrap_or((None, None, None))
            }
            _ => (None, None, None),
        };
        let subject = match name {
            Some(n) => format!("`{n}`"),
            None => "this value".to_string(),
        };
        let label = match &origin {
            Some(o) => format!("`{o}` returns `{shown}`"),
            None => format!("this has type `{shown}`"),
        };
        let mut diag = Diagnostic::error(codes::MAYBE_NIL, format!("{subject} may be nil here")).primary(span, label);
        match (name, origin, decl_stmt) {
            (Some(n), Some(o), Some(stmt_span)) => {
                let indent = self.indent_at(stmt_span);
                let exit = self.guard_exit_text();
                let replacement = format!("guard {n} = {o} else\n{indent}  {exit}\n{indent}end");
                diag = diag.suggest(
                    "unwrap it and handle the nil case",
                    vec![Edit { span: stmt_span, replacement }],
                    Applicability::HasPlaceholders,
                );
            }
            (Some(n), _, _) => {
                diag =
                    diag.help(format!("check it first: `if {n} … end`, `return if {n}.nil?`, or use `{n} || default`"));
            }
            _ => {
                diag = diag.help("unwrap it with `guard v = … else … end`, `if v = … end`, or `value || default`");
            }
        }
        self.report(diag);
    }

    /// Widens the span of a name inside `|name|` to cover the pipes and the
    /// space before them, so removing it leaves `else` intact.
    fn pipes_around(&self, span: Span) -> Span {
        let text = self.source_texts.get(&span.file).cloned().unwrap_or_default();
        let before = &text[..span.start as usize];
        let after = &text[span.end as usize..];
        match (before.trim_end().strip_suffix('|'), after.trim_start().strip_prefix('|')) {
            (Some(open), Some(rest)) => {
                let start = open.trim_end().len() as u32;
                let end = (text.len() - rest.len()) as u32;
                Span { start, end, ..span }
            }
            _ => span,
        }
    }

    /// The whitespace at the start of the line containing `span`.
    pub fn indent_at(&self, span: Span) -> String {
        let text = self.source_texts.get(&span.file).cloned().unwrap_or_default();
        let start = text[..span.start as usize].rfind('\n').map_or(0, |i| i + 1);
        text[start..].chars().take_while(|c| *c == ' ' || *c == '\t').collect()
    }

    /// Suggests how the current method could leave in a guard's else-branch.
    fn guard_exit_text(&mut self) -> String {
        let ret = self.frame().ret;
        match self.types.kind(ret).clone() {
            TyKind::Void => "return".into(),
            TyKind::Optional(_) => "return nil".into(),
            TyKind::Error => "return :some_error".into(),
            TyKind::Tuple(elems) if elems.last().is_some_and(|t| matches!(self.types.kind(*t), TyKind::Error)) => {
                let zeros = vec!["{}"; elems.len() - 1].join(", ");
                format!("return {zeros}, :some_error")
            }
            _ => "return {}".into(),
        }
    }

    /// Lowers `a || b` where `a` is optional: `b` is used when `a` is nil.
    pub fn or_default(&mut self, l: ir::Expr, rhs: &ast::Expr, span: Span) -> ir::Expr {
        let opt_ty = l.ty;
        let inner = self.optional_inner(opt_ty).unwrap_or(opt_ty);
        let l = if l.is_pure() { l } else { self.spill(l) };
        self.begin_block();
        let r = self.expr(rhs, Some(inner));
        let result_ty = if r.ty == opt_ty { opt_ty } else { inner };
        let r = self.coerce(r, result_ty, rhs.span);
        let stmts = self.end_block().stmts;
        let some = self.is_some(l.clone());
        let got = if result_ty == opt_ty { l.clone() } else { self.opt_get(l.clone()) };
        if stmts.is_empty() && r.is_pure() {
            return ir::Expr::new(
                ExprKind::Select { cond: Box::new(some), then: Box::new(got), else_: Box::new(r) },
                result_ty,
            );
        }
        let _ = span;
        let result = self.new_local(None, result_ty);
        self.emit(Stmt::Let { local: result, init: None });
        let target = ir::Expr::new(ExprKind::Local(result), result_ty);
        let mut else_stmts = stmts;
        else_stmts.push(Stmt::Assign { target: target.clone(), value: r });
        self.emit(Stmt::If {
            cond: some,
            then: ir::Block { stmts: vec![Stmt::Assign { target: target.clone(), value: got }] },
            else_: ir::Block { stmts: else_stmts },
        });
        target
    }

    /// Lowers `recv&.name(args)`: nil when the receiver is nil.
    pub fn safe_member(
        &mut self,
        recv: &ast::Expr,
        name: ast::Ident,
        args: Option<&[ast::Arg]>,
        span: Span,
    ) -> ir::Expr {
        let v = self.nilable_operand(recv);
        let Some(_) = self.optional_inner(v.ty) else {
            let shown = self.types.display(v.ty);
            if !matches!(self.types.kind(v.ty), TyKind::Unknown) {
                self.report(
                    Diagnostic::error(codes::TYPE_MISMATCH, format!("`&.` needs an optional, but this is `{shown}`"))
                        .primary(recv.span, "this can never be nil")
                        .suggest(
                            "use a plain `.`",
                            vec![Edit {
                                span: Span::new(name.span.file, recv.span.end, name.span.start),
                                replacement: ".".into(),
                            }],
                            Applicability::MachineApplicable,
                        ),
                );
            }
            return self.value_member(v, recv.span, name, args, None, span, None);
        };
        let tmp = self.spill(v);
        let some = self.is_some(tmp.clone());
        let got = self.opt_get(tmp);
        self.begin_block();
        let value = self.value_member(got, recv.span, name, args, None, span, None);
        let value_ty = value.ty;
        let stmts = self.end_block().stmts;
        if matches!(self.types.kind(value_ty), TyKind::Void) {
            let mut then = stmts;
            then.push(Stmt::Expr(value));
            self.emit(Stmt::If { cond: some, then: ir::Block { stmts: then }, else_: ir::Block::default() });
            return ir::Expr::new(ExprKind::Zero, value_ty);
        }
        let result_ty = self.types.optional(value_ty);
        let result = self.new_local(None, result_ty);
        self.emit(Stmt::Let { local: result, init: Some(ir::Expr::new(ExprKind::Nil, result_ty)) });
        let target = ir::Expr::new(ExprKind::Local(result), result_ty);
        let wrapped = if self.optional_inner(value_ty).is_some() { value } else { self.opt_some(value, result_ty) };
        let mut then = stmts;
        then.push(Stmt::Assign { target: target.clone(), value: wrapped });
        self.emit(Stmt::If { cond: some, then: ir::Block { stmts: then }, else_: ir::Block::default() });
        target
    }

    // ----- if v = maybe ---------------------------------------------------

    /// Lowers `if v = maybe` and `elsif v = maybe`: the value is unwrapped
    /// into `v` for the then-branch.
    pub fn lower_if_bind(
        &mut self,
        name: ast::Ident,
        value: &ast::Expr,
        then: &[ast::Stmt],
        negate: bool,
        dest: Dest,
    ) -> (ir::Expr, ir::Block) {
        let v = self.expr(value, None);
        let Some(inner) = self.optional_inner(v.ty) else {
            let shown = self.types.display(v.ty);
            if !matches!(self.types.kind(v.ty), TyKind::Unknown) {
                self.report(
                    Diagnostic::error(
                        codes::GUARD_NOT_FALLIBLE,
                        format!("`if {} = …` needs an optional, found `{shown}`", name.as_str()),
                    )
                    .primary(value.span, "this always has a value")
                    .help("to test a condition, compare with `==`; to assign, do it on its own line"),
                );
            }
            let bool_ty = self.types.bool();
            self.begin_block();
            self.push_scope();
            let local = self.declare_var(name.name, v.ty, name.span, true);
            self.emit(Stmt::Let { local, init: Some(v) });
            self.lower_stmts(then, dest);
            self.pop_scope();
            return (ir::Expr::new(ExprKind::Zero, bool_ty), self.end_block());
        };
        let tmp = self.spill(v);
        let some = self.is_some(tmp.clone());
        let cond = if negate { self.not(some) } else { some };
        self.begin_block();
        self.push_scope();
        let local = self.declare_var(name.name, inner, name.span, false);
        let got = self.opt_get(tmp);
        self.emit(Stmt::Let { local, init: Some(got) });
        self.lower_stmts(then, dest);
        self.pop_scope();
        (cond, self.end_block())
    }

    // ----- guard ------------------------------------------------------------

    /// Lowers `guard names = value else |err| … end`.
    pub fn lower_guard(
        &mut self,
        names: &[ast::Ident],
        value: &ast::Expr,
        err: Option<ast::Ident>,
        else_body: &[ast::Stmt],
        span: Span,
    ) {
        let v = self.expr(value, None);
        if matches!(self.types.kind(v.ty), TyKind::Unknown) {
            for n in names {
                let unknown = self.types.unknown();
                let local = self.declare_var(n.name, unknown, n.span, true);
                self.emit(Stmt::Let { local, init: None });
            }
            return;
        }
        let shape = match self.types.kind(self.types.base(v.ty)).clone() {
            TyKind::Optional(inner) => GuardShape::Optional { inner },
            TyKind::Tuple(elems) if elems.last().is_some_and(|t| self.types.is_nilable(*t)) => {
                GuardShape::TupleWithError { elems }
            }
            TyKind::Error | TyKind::Union(_) => GuardShape::Error,
            TyKind::Bool => GuardShape::Bool,
            _ => {
                let shown = self.types.display(v.ty);
                self.report(
                    Diagnostic::error(
                        codes::GUARD_NOT_FALLIBLE,
                        format!("`guard` needs a value that can fail, found `{shown}`"),
                    )
                    .primary(value.span, "this always succeeds")
                    .note("guard works on optionals (`T?`), results ending in an `Error`, and `Bool` conditions")
                    .help("assign the value directly, like `name = value`, and drop the `else` branch"),
                );
                // Bind the names anyway, so their uses are not reported too.
                let elem_tys: Vec<TyId> = match self.types.kind(v.ty).clone() {
                    TyKind::Tuple(elems) if elems.len() == names.len() => elems,
                    _ => vec![if names.len() == 1 { v.ty } else { self.types.unknown() }; names.len()],
                };
                let tmp = self.spill(v);
                for (i, (n, ty)) in names.iter().zip(elem_tys).enumerate() {
                    let local = self.declare_var(n.name, ty, n.span, true);
                    let init = if names.len() == 1 {
                        tmp.clone()
                    } else {
                        ir::Expr::new(ExprKind::Field { base: Box::new(tmp.clone()), index: i as u32 }, ty)
                    };
                    self.emit(Stmt::Let { local, init: Some(init) });
                }
                return;
            }
        };
        let tmp = if matches!(shape, GuardShape::Bool) { v } else { self.spill(v) };
        let (ok, bound): (ir::Expr, Vec<(ast::Ident, ir::Expr)>) = match &shape {
            GuardShape::Bool => {
                self.expect_guard_names(names, 0, value.span, "a `Bool` condition binds no names");
                (tmp.clone(), Vec::new())
            }
            GuardShape::Error => {
                self.expect_guard_names(names, 0, value.span, "an `Error` result has no value to bind");
                let some = self.is_some(tmp.clone());
                (self.not(some), Vec::new())
            }
            GuardShape::Optional { inner } => {
                let ok = self.is_some(tmp.clone());
                let got = self.opt_get(tmp.clone());
                let bound = match self.types.kind(*inner).clone() {
                    TyKind::Tuple(elems) if names.len() == elems.len() => names
                        .iter()
                        .enumerate()
                        .map(|(i, n)| {
                            (
                                *n,
                                ir::Expr::new(
                                    ExprKind::Field { base: Box::new(got.clone()), index: i as u32 },
                                    elems[i],
                                ),
                            )
                        })
                        .collect(),
                    _ => {
                        self.expect_guard_names(names, 1, value.span, "an optional binds one name");
                        names.first().map(|n| vec![(*n, got)]).unwrap_or_default()
                    }
                };
                (ok, bound)
            }
            GuardShape::TupleWithError { elems } => {
                let last = elems.len() - 1;
                let err_e =
                    ir::Expr::new(ExprKind::Field { base: Box::new(tmp.clone()), index: last as u32 }, elems[last]);
                let some = self.is_some(err_e);
                let ok = self.not(some);
                self.expect_guard_names(names, last, value.span, "bind every value before the error");
                let bound = names
                    .iter()
                    .take(last)
                    .enumerate()
                    .map(|(i, n)| {
                        (*n, ir::Expr::new(ExprKind::Field { base: Box::new(tmp.clone()), index: i as u32 }, elems[i]))
                    })
                    .collect();
                (ok, bound)
            }
        };

        // The else-branch runs when the value failed and must leave the scope.
        self.begin_block();
        self.push_scope();
        if let Some(e) = err {
            let err_value = match &shape {
                GuardShape::TupleWithError { elems } => {
                    let last = elems.len() - 1;
                    Some(ir::Expr::new(
                        ExprKind::Field { base: Box::new(tmp.clone()), index: last as u32 },
                        elems[last],
                    ))
                }
                GuardShape::Error => Some(tmp.clone()),
                _ => None,
            };
            match err_value {
                Some(ev) => {
                    let local = self.declare_var(e.name, ev.ty, e.span, false);
                    self.emit(Stmt::Let { local, init: Some(ev) });
                }
                None => self.report(
                    Diagnostic::error(codes::GUARD_NOT_FALLIBLE, "there is no error value to bind")
                        .primary(e.span, "only results ending in an `Error` carry one")
                        .suggest(
                            "remove the binding",
                            vec![Edit { span: self.pipes_around(e.span), replacement: String::new() }],
                            Applicability::MachineApplicable,
                        ),
                ),
            }
        }
        self.lower_stmts(else_body, Dest::Discard);
        let diverges = self.current_block_diverges();
        self.pop_scope();
        let else_block = self.end_block();
        if !diverges {
            let exit = self.guard_exit_text();
            self.report(
                Diagnostic::error(codes::GUARD_FALLTHROUGH, "the `else` of a `guard` must leave the scope")
                    .primary(span, "control can continue past this guard without a value")
                    .note("end the else-branch with `return`, `break`, `next` or `panic`")
                    .help(format!("for example, end it with `{exit}`")),
            );
        }
        let not_ok = self.not(ok);
        self.emit(Stmt::If { cond: not_ok, then: else_block, else_: ir::Block::default() });
        for (name, value) in bound {
            if name.as_str() == "_" {
                continue;
            }
            let local = self.declare_var(name.name, value.ty, name.span, false);
            self.emit(Stmt::Let { local, init: Some(value) });
        }
    }

    fn expect_guard_names(&mut self, names: &[ast::Ident], want: usize, span: Span, what: &str) {
        if names.len() != want {
            let target = names.first().map_or(span, |n| n.span.to(names[names.len() - 1].span));
            self.report(
                Diagnostic::error(
                    codes::ARG_COUNT,
                    format!("`guard` expected {want} name{}, found {}", if want == 1 { "" } else { "s" }, names.len()),
                )
                .primary(target, what.to_string()),
            );
        }
    }

    // ----- several values ----------------------------------------------------

    /// Lowers `a, b = f()` and `a, b = x, y`.
    pub fn lower_multi_assign(&mut self, targets: &[ast::Expr], values: &[ast::Expr], span: Span) {
        let parts: Vec<ir::Expr> = if values.len() == 1 {
            let v = self.expr(&values[0], None);
            match self.types.kind(v.ty).clone() {
                TyKind::Tuple(elems) if elems.len() == targets.len() => {
                    let tmp = self.spill(v);
                    elems
                        .iter()
                        .enumerate()
                        .map(|(i, t)| {
                            ir::Expr::new(ExprKind::Field { base: Box::new(tmp.clone()), index: i as u32 }, *t)
                        })
                        .collect()
                }
                TyKind::Unknown => vec![v; targets.len()],
                other => {
                    let count = if let TyKind::Tuple(e) = &other { e.len() } else { 1 };
                    let shown = self.types.display(v.ty);
                    let help = if count > targets.len() {
                        "add a name for every value; use `_` for values you don't need"
                    } else {
                        "remove the extra names"
                    };
                    self.report(
                        Diagnostic::error(
                            codes::ARG_COUNT,
                            format!("{} names but {count} value{}", targets.len(), if count == 1 { "" } else { "s" }),
                        )
                        .primary(values[0].span, format!("this produces `{shown}`"))
                        .help(help),
                    );
                    self.assign_unknown(targets);
                    return;
                }
            }
        } else if values.len() == targets.len() {
            // Evaluate every value before assigning any, so `a, b = b, a` swaps.
            let mut out = Vec::new();
            for (t, v) in targets.iter().zip(values) {
                let expected = self.target_type(t);
                let lowered = match expected {
                    Some(ty) => self.expr_coerced(v, ty),
                    None => self.expr(v, None),
                };
                out.push(self.spill(lowered));
            }
            out
        } else {
            self.report(
                Diagnostic::error(
                    codes::ARG_COUNT,
                    format!(
                        "{} names but {} value{}",
                        targets.len(),
                        values.len(),
                        if values.len() == 1 { "" } else { "s" }
                    ),
                )
                .primary(span, "the counts on both sides of `=` must match")
                .help("give every name exactly one value"),
            );
            for v in values {
                let lowered = self.expr(v, None);
                self.emit_value_stmt(lowered);
            }
            self.assign_unknown(targets);
            return;
        };
        for (i, (target, value)) in targets.iter().zip(parts).enumerate() {
            let new = matches!(target.kind, E::Ident(n) if self.find_var_at(n, target.span).is_none());
            self.assign_to(target, value);
            // A new name whose value failed to parse is poisoned (see `declare_var`).
            let source = if values.len() == 1 { &values[0] } else { &values[i] };
            if new
                && holds_parse_error(source)
                && let E::Ident(n) = target.kind
                && let Some(var) = self.find_var_at(n, target.span)
            {
                var.allow_unused = true;
            }
        }
    }

    /// After a failed multiple assignment, gives each new name an unknown
    /// value so later uses are not reported as undefined.
    fn assign_unknown(&mut self, targets: &[ast::Expr]) {
        let unknown = self.types.unknown();
        for t in targets {
            if let E::Ident(name) = t.kind
                && self.find_var_at(name, t.span).is_none()
            {
                let local = self.declare_var(name, unknown, t.span, true);
                self.emit(Stmt::Let { local, init: None });
            }
        }
    }

    /// Returns the type of an existing assignment target, if it has one.
    fn target_type(&mut self, target: &ast::Expr) -> Option<TyId> {
        match &target.kind {
            E::Ident(n) => self.find_var_at(*n, target.span).map(|v| v.ty),
            _ => None,
        }
    }

    /// Assigns an already-lowered value to a target, declaring new names.
    pub fn assign_to(&mut self, target: &ast::Expr, value: ir::Expr) {
        if let E::Ident(name) = target.kind {
            if name.as_str() == "_" {
                if !value.is_pure() {
                    self.emit(Stmt::Expr(value));
                }
                return;
            }
            let existing = self.find_var_at(name, target.span).map(|v| (v.local, v.ty, v.indirect));
            match existing {
                Some((local, ty, indirect)) => {
                    let v = self.coerce(value, ty, target.span);
                    self.invalidate(local);
                    let target = self.var_place(local, ty, indirect);
                    self.emit(Stmt::Assign { target, value: v });
                }
                None => {
                    let ty = self.value_type(value.ty, target.span);
                    let v = self.coerce(value, ty, target.span);
                    let local = self.declare_var(name, ty, target.span, false);
                    self.emit(Stmt::Let { local, init: Some(v) });
                }
            }
            return;
        }
        let place = self.place(target);
        let v = self.coerce(value, place.ty, target.span);
        self.emit(Stmt::Assign { target: place, value: v });
    }

    /// Lowers `return a, b`.
    pub fn lower_multi_return(&mut self, values: &[ast::Expr], span: Span) {
        let ret = self.frame().ret;
        let elems = match self.types.kind(ret).clone() {
            TyKind::Tuple(e) => e,
            TyKind::Optional(inner) => match self.types.kind(inner).clone() {
                TyKind::Tuple(e) => e,
                _ => Vec::new(),
            },
            _ => Vec::new(),
        };
        if elems.len() != values.len() {
            let want = self.types.display(ret);
            self.report(
                Diagnostic::error(
                    codes::RETURN_MISMATCH,
                    format!("this method returns `{want}`, but {} values are returned", values.len()),
                )
                .primary(
                    span,
                    format!("expected {} value{}", elems.len().max(1), if elems.len() <= 1 { "" } else { "s" }),
                )
                .help(format!(
                    "return exactly {} value{}, or change the declared return type",
                    elems.len().max(1),
                    if elems.len() <= 1 { "" } else { "s" }
                )),
            );
            for v in values {
                self.begin_block();
                let _ = self.expr(v, None);
                let _ = self.end_block();
            }
            // The method still leaves here, so it is not also reported as
            // falling off its end.
            self.emit(Stmt::Unreachable);
            return;
        }
        let mut lowered: Vec<ir::Expr> = Vec::new();
        for (v, ty) in values.iter().zip(&elems) {
            self.begin_block();
            let e = self.expr_coerced(v, *ty);
            let stmts = self.end_block().stmts;
            if !stmts.is_empty() || !e.is_pure() {
                self.spill_impure(&mut lowered);
            }
            for s in stmts {
                self.emit(s);
            }
            lowered.push(e);
        }
        let tuple_ty = self.types.tuple(elems);
        let tuple = ir::Expr::new(ExprKind::Aggregate(lowered), tuple_ty);
        let value = if tuple_ty == ret { tuple } else { self.opt_some(tuple, ret) };
        self.emit_return(Some(value), span);
    }

    /// Registers `name` in the builtin `Error` set.
    pub fn error_tag(&mut self, name: Name) {
        if !self.errors.contains(&name) {
            self.errors.push(name);
        }
    }
}

/// Collects names assigned anywhere in a statement list, including nested
/// blocks.
fn collect_assigned(stmts: &[ast::Stmt], out: &mut Vec<Name>) {
    for s in stmts {
        match &s.kind {
            ast::StmtKind::Assign { targets, .. } => {
                for t in targets {
                    if let E::Ident(n) = t.kind {
                        out.push(n);
                    }
                }
            }
            ast::StmtKind::Expr(e) => collect_assigned_expr(e, out),
            ast::StmtKind::Defer(body) => collect_assigned(body, out),
            ast::StmtKind::Guard { else_body, .. } => collect_assigned(else_body, out),
            _ => {}
        }
    }
}

fn collect_assigned_expr(e: &ast::Expr, out: &mut Vec<Name>) {
    match &e.kind {
        E::If(i) => {
            collect_assigned(&i.then, out);
            for (_, b) in &i.elifs {
                collect_assigned(b, out);
            }
            if let Some(b) = &i.else_ {
                collect_assigned(b, out);
            }
        }
        E::While { body, .. } | E::Loop(body) => collect_assigned(body, out),
        E::For(f) => collect_assigned(&f.body, out),
        E::Case(c) => {
            for w in &c.whens {
                collect_assigned(&w.body, out);
            }
            if let Some(b) = &c.else_ {
                collect_assigned(b, out);
            }
        }
        E::Call(call) => {
            if let Some(b) = &call.block {
                collect_assigned(&b.body, out);
            }
        }
        _ => {}
    }
}

//! Generics: type parameters (`$T`), generic structs, inference by
//! unification, instantiation, and `extend` blocks that add methods to
//! existing types.
//!
//! Generic code follows template semantics: signatures are resolved once with
//! `TyKind::Param` placeholders, and bodies are checked per instantiation. A
//! signature whose types read a value parameter (`[N]T`, `Pool(T, N + 1)`)
//! is resolved again for each instance, with the value bound to `N`.

use std::rc::Rc;

use wid_diagnostics::{Diagnostic, Span, codes};
use wid_syntax::Name;
use wid_syntax::ast::{self, ItemKind};
use wid_syntax::visit::{VisitMut, walk_expr, walk_type};

use super::ty::TyCtx;
use super::{Checker, DeclId, DeclKind, FnSig, ParamSig, mangle_ident};
use crate::ir::FnId;
use crate::types::{FieldInfo, ProcSig, StructInfo, TyId, TyKind, offsets};

/// Type-parameter bindings, in a stable order.
pub(crate) type Subst = Rc<Vec<(Name, TyId)>>;

/// The most instances one generic declaration may have before the checker
/// assumes runaway recursion.
const MAX_INSTANCES: usize = 512;

/// Looks up a binding.
pub(crate) fn lookup(subst: &[(Name, TyId)], name: Name) -> Option<TyId> {
    subst.iter().find(|(n, _)| *n == name).map(|(_, t)| *t)
}

impl<'a> Checker<'a> {
    /// The generic parameter names of a declaration: its owner's (for
    /// methods of generic structs) and its own `$T` parameters.
    pub fn generic_names(&self, decl: DeclId) -> Vec<Name> {
        let d = &self.decls[decl.0 as usize];
        let mut names = Vec::new();
        if let Some(owner) = d.owner {
            match self.decls[owner.0 as usize].kind {
                DeclKind::Struct(s) => names.extend(s.generics.iter().map(|g| g.name.name)),
                DeclKind::Module => names.push(Name::new("Self")),
                DeclKind::Extend(e) => {
                    names.push(Name::new("Self"));
                    for t in &e.targets {
                        collect_params(t, &mut names);
                    }
                }
                _ => {}
            }
        }
        match d.kind {
            DeclKind::Fn(f) => {
                for p in &f.params {
                    collect_params(&p.ty, &mut names);
                }
            }
            DeclKind::Struct(s) => names.extend(s.generics.iter().map(|g| g.name.name)),
            DeclKind::Union(u) => names.extend(u.generics.iter().map(|g| g.name.name)),
            _ => {}
        }
        let mut seen = Vec::new();
        names.retain(|n| {
            if seen.contains(n) {
                false
            } else {
                seen.push(*n);
                true
            }
        });
        names
    }

    /// Bindings that map every generic name to its placeholder.
    pub fn template_subst(&mut self, decl: DeclId) -> Subst {
        let names = self.generic_names(decl);
        Rc::new(names.into_iter().map(|n| (n, self.types.intern(TyKind::Param(n)))).collect())
    }

    /// Returns true when a type still contains placeholders.
    pub fn has_params(&self, ty: TyId) -> bool {
        match self.types.kind(ty) {
            TyKind::Param(_) => true,
            TyKind::Pointer(t)
            | TyKind::MultiPointer(t)
            | TyKind::Slice(t)
            | TyKind::Dynamic(t)
            | TyKind::Optional(t)
            | TyKind::Array(t, _)
            | TyKind::Matrix(t, _, _)
            | TyKind::TypeValue(t) => self.has_params(*t),
            TyKind::Map(k, v) => self.has_params(*k) || self.has_params(*v),
            TyKind::Tuple(elems) => elems.iter().any(|e| self.has_params(*e)),
            TyKind::Proc(sig) => sig.params.iter().any(|p| self.has_params(*p)) || self.has_params(sig.ret),
            TyKind::Struct(id) => self.struct_args.get(id).is_some_and(|s| s.iter().any(|(_, t)| self.has_params(*t))),
            TyKind::Union(id) => {
                self.union_args.get(id).is_some_and(|(_, s)| s.iter().any(|(_, t)| self.has_params(*t)))
            }
            _ => false,
        }
    }

    /// Replaces placeholders in `ty` with their bindings.
    pub fn subst_type(&mut self, ty: TyId, subst: &[(Name, TyId)]) -> TyId {
        if subst.is_empty() {
            return ty;
        }
        let kind = self.types.kind(ty).clone();
        match kind {
            TyKind::Param(n) => lookup(subst, n).unwrap_or(ty),
            TyKind::Pointer(t) => {
                let t = self.subst_type(t, subst);
                self.types.pointer(t)
            }
            TyKind::MultiPointer(t) => {
                let t = self.subst_type(t, subst);
                self.types.intern(TyKind::MultiPointer(t))
            }
            TyKind::Slice(t) => {
                let t = self.subst_type(t, subst);
                self.types.slice(t)
            }
            TyKind::Dynamic(t) => {
                let t = self.subst_type(t, subst);
                self.types.intern(TyKind::Dynamic(t))
            }
            TyKind::Optional(t) => {
                let t = self.subst_type(t, subst);
                self.types.optional(t)
            }
            TyKind::TypeValue(t) => {
                let t = self.subst_type(t, subst);
                self.types.intern(TyKind::TypeValue(t))
            }
            TyKind::Array(t, n) => {
                let t = self.subst_type(t, subst);
                self.types.intern(TyKind::Array(t, n))
            }
            TyKind::Matrix(t, r, c) => {
                let t = self.subst_type(t, subst);
                self.types.intern(TyKind::Matrix(t, r, c))
            }
            TyKind::Map(k, v) => {
                let k = self.subst_type(k, subst);
                let v = self.subst_type(v, subst);
                self.types.intern(TyKind::Map(k, v))
            }
            TyKind::Tuple(elems) => {
                let elems = elems.iter().map(|e| self.subst_type(*e, subst)).collect();
                self.types.tuple(elems)
            }
            TyKind::Proc(sig) => {
                let params = sig.params.iter().map(|p| self.subst_type(*p, subst)).collect();
                let ret = self.subst_type(sig.ret, subst);
                self.types.intern(TyKind::Proc(ProcSig { params, ret, abi: sig.abi, variadic: sig.variadic }))
            }
            TyKind::Struct(id) => {
                let Some(args) = self.struct_args.get(&id).cloned() else { return ty };
                if !args.iter().any(|(_, t)| self.has_params(*t)) {
                    return ty;
                }
                let Some(&decl) = self.struct_decls.get(&id) else { return ty };
                let new_args: Vec<TyId> = args.iter().map(|(_, t)| self.subst_type(*t, subst)).collect();
                self.struct_instance(decl, new_args, Span::default())
            }
            TyKind::Union(id) => {
                let Some((decl, args)) = self.union_args.get(&id).cloned() else { return ty };
                if !args.iter().any(|(_, t)| self.has_params(*t)) {
                    return ty;
                }
                let new_args: Vec<TyId> = args.iter().map(|(_, t)| self.subst_type(*t, subst)).collect();
                self.union_instance(decl, new_args, Span::default())
            }
            _ => ty,
        }
    }

    /// Matches `pattern` (which may contain placeholders) against `actual`,
    /// extending `bindings`. Returns false on a mismatch.
    pub fn unify(&mut self, pattern: TyId, actual: TyId, bindings: &mut Vec<(Name, TyId)>) -> bool {
        if pattern == actual {
            return true;
        }
        let pk = self.types.kind(pattern).clone();
        let ak = self.types.kind(actual).clone();
        match (pk, ak) {
            (TyKind::Param(n), _) => match lookup(bindings, n) {
                Some(bound) => bound == actual || matches!(self.types.kind(actual), TyKind::Unknown),
                None => {
                    if matches!(self.types.kind(actual), TyKind::Nil | TyKind::Void | TyKind::Never | TyKind::Symbol) {
                        return false;
                    }
                    bindings.push((n, actual));
                    true
                }
            },
            (_, TyKind::Unknown) => true,
            (TyKind::Pointer(p), TyKind::Pointer(a))
            | (TyKind::MultiPointer(p), TyKind::MultiPointer(a))
            | (TyKind::Slice(p), TyKind::Slice(a))
            | (TyKind::Dynamic(p), TyKind::Dynamic(a))
            | (TyKind::Optional(p), TyKind::Optional(a)) => self.unify(p, a, bindings),
            (TyKind::Array(p, pn), TyKind::Array(a, an)) => pn == an && self.unify(p, a, bindings),
            (TyKind::Slice(p), TyKind::Array(a, _) | TyKind::Dynamic(a)) => self.unify(p, a, bindings),
            (TyKind::Optional(p), _) if !matches!(self.types.kind(actual), TyKind::Nil) => {
                self.unify(p, actual, bindings)
            }
            (TyKind::Map(pk2, pv), TyKind::Map(ak2, av)) => {
                self.unify(pk2, ak2, bindings) && self.unify(pv, av, bindings)
            }
            (TyKind::Tuple(ps), TyKind::Tuple(as_)) => {
                ps.len() == as_.len() && ps.iter().zip(&as_).all(|(p, a)| self.unify(*p, *a, bindings))
            }
            (TyKind::Proc(ps), TyKind::Proc(as_)) => {
                ps.params.len() == as_.params.len()
                    && ps.params.iter().zip(&as_.params).all(|(p, a)| self.unify(*p, *a, bindings))
                    && self.unify(ps.ret, as_.ret, bindings)
            }
            (TyKind::Union(p), TyKind::Union(a)) => {
                let (Some((pd, pa)), Some((ad, aa))) =
                    (self.union_args.get(&p).cloned(), self.union_args.get(&a).cloned())
                else {
                    return false;
                };
                pd == ad
                    && pa.len() == aa.len()
                    && pa.iter().zip(aa.iter()).all(|((_, p), (_, a))| self.unify(*p, *a, bindings))
            }
            (TyKind::Struct(p), TyKind::Struct(a)) => {
                let same_decl =
                    self.struct_decls.contains_key(&p) && self.struct_decls.get(&p) == self.struct_decls.get(&a);
                if !same_decl {
                    return false;
                }
                let pa = self.struct_args.get(&p).cloned().unwrap_or_default();
                let aa = self.struct_args.get(&a).cloned().unwrap_or_default();
                pa.len() == aa.len() && pa.iter().zip(aa.iter()).all(|((_, p), (_, a))| self.unify(*p, *a, bindings))
            }
            _ => false,
        }
    }

    /// Checks the arguments of a generic struct or union against its
    /// parameters: the count, and that `$T` gets a type while `$N: Int` gets
    /// a constant. `spans` holds each argument's span when known.
    pub fn check_generic_args(&mut self, decl: DeclId, args: &[TyId], spans: &[Span], span: Span) -> bool {
        let d = self.decls[decl.0 as usize].clone();
        let generics: &[ast::GenericParam] = match d.kind {
            DeclKind::Struct(s) => &s.generics,
            DeclKind::Union(u) => &u.generics,
            _ => return true,
        };
        let shape: Vec<&str> = generics.iter().map(|g| g.name.as_str()).collect();
        let values: Vec<String> = generics
            .iter()
            .filter_map(|g| {
                g.ty.as_ref().map(|t| format!("`{}` is a constant `{}`", g.name.as_str(), self.source_text(t.span)))
            })
            .collect();
        if generics.len() != args.len() {
            let given = args.len();
            let diag = Diagnostic::error(
                codes::GENERIC_ARGS,
                format!(
                    "`{}` takes {} generic argument{}, but {given} {} given",
                    d.name,
                    generics.len(),
                    if generics.len() == 1 { "" } else { "s" },
                    if given == 1 { "was" } else { "were" }
                ),
            )
            .primary(span, format!("write it like `{}({})`", d.name, shape.join(", ")))
            .secondary(d.span, format!("`{}` is declared here", d.name));
            let diag = if values.is_empty() { diag } else { diag.note(values.join(", ")) };
            self.report(diag);
            return false;
        }
        let mut ok = true;
        for (i, (g, &arg)) in generics.iter().zip(args).enumerate() {
            let at = spans.get(i).copied().unwrap_or(span);
            let kind = self.types.kind(arg).clone();
            match (&g.ty, kind) {
                // A value argument that was reported: the instance is too.
                (Some(_), TyKind::Unknown) => ok = false,
                (_, TyKind::Unknown | TyKind::Param(_)) => {}
                (Some(_), TyKind::ConstValue(v)) if v < 0 => {
                    if let DeclKind::Struct(s) = d.kind
                        && let Some((field, array)) = sized_field(&s.body, g.name.name)
                    {
                        ok = false;
                        let (n, shown) = (g.name.as_str(), self.source_text(array));
                        self.report(
                            Diagnostic::error(codes::TYPE_MISMATCH, "array length cannot be negative")
                                .primary(at, format!("`{n}` is {v} here"))
                                .secondary(array, format!("`{n}` is the length of this array"))
                                .note(format!(
                                    "`{n}` sizes `{}`'s field `{}: {shown}`, so it can't be negative",
                                    d.name,
                                    field.as_str()
                                ))
                                .help(format!("pass a length of 0 or more for `{n}`")),
                        );
                    }
                }
                (None, TyKind::ConstValue(v)) => {
                    ok = false;
                    self.report(
                        Diagnostic::error(
                            codes::GENERIC_ARGS,
                            format!("`{}` takes a type for `{}`, but `{v}` is a value", d.name, g.name.as_str()),
                        )
                        .primary(at, "expected a type like `F32` or `Vec2`")
                        .secondary(g.span, format!("`{}` is a type parameter", g.name.as_str())),
                    );
                }
                (Some(t), k) if !matches!(k, TyKind::ConstValue(_)) => {
                    ok = false;
                    let shown = self.types.display(arg);
                    let want = self.source_text(t.span);
                    self.report(
                        Diagnostic::error(
                            codes::GENERIC_ARGS,
                            format!(
                                "`{}` takes a constant `{want}` for `{}`, but `{shown}` is a type",
                                d.name,
                                g.name.as_str()
                            ),
                        )
                        .primary(at, format!("expected a constant `{want}`, like `4`"))
                        .secondary(g.span, format!("`{}` is a value parameter", g.name.as_str())),
                    );
                }
                _ => {}
            }
        }
        ok
    }

    /// Resolves the argument for generic parameter `index` of `decl` when
    /// that is a value parameter (`$N: Int`) and the argument is a value: a
    /// constant (`N`, `geo.N`, a `comptime`-computed one) or constant
    /// arithmetic (`SIZE * 2`), like the literal `4`. Returns `None` when
    /// the parameter takes a type, or when the argument names a type or a
    /// generic parameter, for the caller to resolve as a type.
    pub(super) fn value_generic_arg(
        &mut self,
        decl: DeclId,
        index: usize,
        e: &ast::Expr,
        loc: super::DeclLoc,
        subst: &[(Name, TyId)],
    ) -> Option<TyId> {
        let name = self.decls[decl.0 as usize].name;
        let generics: &[ast::GenericParam] = match self.decls[decl.0 as usize].kind {
            DeclKind::Struct(s) => &s.generics,
            DeclKind::Union(u) => &u.generics,
            _ => return None,
        };
        let param = generics.get(index)?;
        let want = param.ty.as_ref()?;
        // A parameter declared with another type than `Int` is reported at
        // the declaration; its arguments aren't checked against that type.
        if !self.value_param_is_int(decl, param) {
            return Some(self.types.unknown());
        }
        // A generic parameter that is still a placeholder, like `N` in
        // `other: Pool(T, N)` while a signature is resolved for every instance.
        if let ast::ExprKind::Const(n) = e.kind
            && let Some(t) = lookup(subst, n)
            && matches!(self.types.kind(t), TyKind::Param(_))
        {
            return Some(t);
        }
        // A macro's own code names constants where the macro is.
        let loc = self.virtual_file(e.span.file).map_or(loc, |v| v.loc);
        if let ast::ExprKind::Ident(var_name) = e.kind
            && !self.body.frames.is_empty()
            && let Some(var) = self.find_var_at(var_name, e.span)
        {
            // Read here, so not also reported unused.
            var.read = true;
            let var_span = var.span;
            let upper = var_name.as_str().to_uppercase();
            self.report(
                Diagnostic::error(codes::GENERIC_ARGS, "a generic value argument must be a constant integer")
                    .primary(e.span, format!("`{var_name}` is a variable, so its value is only known at run time"))
                    .secondary(var_span, format!("`{var_name}` is declared here"))
                    .secondary(param.span, format!("`{}` is a value parameter", param.name.as_str()))
                    .help(format!("declare a package-level constant like `{upper} = …` and pass `{upper}`")),
            );
            return Some(self.types.unknown());
        }
        match self.value_arg_kind(e, loc, subst) {
            ValueArg::Type => return None,
            ValueArg::Undefined(name) => {
                let candidates = self
                    .package_names(loc.pkg)
                    .into_iter()
                    .filter(|n| n.chars().next().is_some_and(char::is_uppercase))
                    .collect();
                self.undefined(name, e.span, candidates, "constant");
                return Some(self.types.unknown());
            }
            ValueArg::Value => {}
        }
        // `N + 1` in a signature resolved with placeholders: each instance
        // resolves it with its own `N`.
        if self.reads_placeholder(e, subst) {
            self.value_deferrals += 1;
            return Some(self.types.unknown());
        }
        let errors = self.diags.error_count();
        let value = self.eval_const_in(e, loc, subst);
        let want = self.source_text(want.span);
        match value {
            Some(super::items::ConstValue::Int(v)) => Some(self.types.intern(TyKind::ConstValue(v))),
            Some(other) => {
                let found = self.default_const(other);
                let shown = self.types.display(found.ty);
                let text = self.source_text(e.span);
                let diag = Diagnostic::error(
                    codes::GENERIC_ARGS,
                    format!(
                        "`{name}` takes a constant `{want}` for `{}`, but `{text}` has type `{shown}`",
                        param.name.as_str()
                    ),
                )
                .primary(e.span, format!("expected a constant `{want}`"))
                .secondary(param.span, format!("`{}` is a value parameter", param.name.as_str()));
                let diag = if self.types.is_numeric(found.ty) {
                    super::items::conversion_help(diag, e.span, &text, &want)
                } else {
                    diag.help("pass an integer constant, like `4`, `SIZE` or `SIZE * 2`")
                };
                self.report(diag);
                Some(self.types.unknown())
            }
            None => {
                if self.diags.error_count() == errors && !super::runtime::holds_parse_error(e) {
                    self.report(
                        Diagnostic::error(codes::GENERIC_ARGS, "a generic value argument must be a constant integer")
                            .primary(e.span, "not a constant")
                            .secondary(param.span, format!("`{}` is a value parameter", param.name.as_str()))
                            .help("pass a literal like `4`, a constant like `SIZE`, or arithmetic on them"),
                    );
                }
                Some(self.types.unknown())
            }
        }
    }

    /// Checks that a value parameter of a generic struct or union is
    /// declared `$N: Int`, reporting another type at the declaration: a value
    /// parameter is a constant integer. Returns true for type parameters.
    pub fn value_param_is_int(&mut self, decl: DeclId, param: &ast::GenericParam) -> bool {
        let Some(t) = &param.ty else { return true };
        let ctx = TyCtx { loc: self.decls[decl.0 as usize].loc, self_ty: None, subst: Default::default() };
        let ty = self.resolve_type(t, &ctx);
        if ty == self.types.int() {
            return true;
        }
        if !matches!(self.types.kind(ty), TyKind::Unknown) {
            let name = param.name.as_str();
            let shown = self.source_text(t.span);
            self.report(
                Diagnostic::error(codes::GENERIC_ARGS, format!("value parameter `{name}` must be an `Int`"))
                    .primary(t.span, format!("`{name}` is declared `{shown}`"))
                    .note(format!(
                        "a value parameter is a constant integer, like the `64` in `Pool(Ball, 64)`: `{name}` can be an array length (`[{name}]T`) or a count"
                    ))
                    .suggest_replace("declare it `Int`", t.span, "Int", wid_diagnostics::Applicability::MachineApplicable),
            );
        }
        false
    }

    /// Reports a type parameter, like `T` in a method of `Box(Int)`, used
    /// as a value (`def f -> Int = T`), where it is bound to `ty`. Where an
    /// integer is expected, the fix takes the type's size.
    pub(super) fn type_param_as_value(&mut self, name: Name, ty: TyId, span: Span, expected: Option<TyId>) {
        let shown = self.types.display(ty);
        let mut diag = Diagnostic::error(codes::NOT_A_VALUE, format!("`{name}` is a type, not a value"))
            .primary(span, "expected a value here");
        let decl = self.body.frames.last().and_then(|f| f.decl);
        if let Some(at) = decl.and_then(|d| self.generic_param_span(d, name)) {
            diag = diag.secondary(at, format!("`{name}` is a type parameter, `{shown}` here"));
        }
        let help = format!(
            "`size_of({name})` and `type_info({name})` are values about a type; a `Type` parameter, like `t: Type`, takes the type itself"
        );
        diag = match expected {
            Some(t) if self.types.is_int(t) => diag
                .suggest_replace(
                    format!("for the size of `{name}` in bytes, write `size_of({name})`"),
                    span,
                    format!("size_of({name})"),
                    wid_diagnostics::Applicability::MaybeIncorrect,
                )
                .note(help),
            _ => diag.help(help),
        };
        self.report(diag);
    }

    /// Where generic parameter `name` of a declaration is introduced: in
    /// the header of its struct or `extend`, or in one of its parameters.
    fn generic_param_span(&self, decl: DeclId, name: Name) -> Option<Span> {
        let d = &self.decls[decl.0 as usize];
        let mut types: Vec<&ast::TypeExpr> = Vec::new();
        if let Some(owner) = d.owner {
            match self.decls[owner.0 as usize].kind {
                DeclKind::Struct(s) => {
                    if let Some(g) = s.generics.iter().find(|g| g.name.name == name) {
                        return Some(g.span);
                    }
                }
                DeclKind::Extend(e) => types.extend(&e.targets),
                _ => {}
            }
        }
        if let DeclKind::Fn(f) = d.kind {
            types.extend(f.params.iter().map(|p| &p.ty));
        }
        types.into_iter().find_map(|t| {
            let mut find = FindParam { name, span: None };
            find.visit_type(&mut t.clone());
            find.span
        })
    }

    /// Whether a value parameter's argument is a value, or names a type or
    /// generic parameter, or an undefined constant.
    fn value_arg_kind(&mut self, e: &ast::Expr, loc: super::DeclLoc, subst: &[(Name, TyId)]) -> ValueArg {
        use ast::ExprKind as E;
        let decl = match &e.kind {
            E::Type(_) => return ValueArg::Type,
            E::Paren(inner) => return self.value_arg_kind(inner, loc, subst),
            E::Const(n) => {
                // A value parameter bound in an instance, like `N` in
                // `Pool(T, N)` in a method of `Pool(Int, 4)`, is a value.
                if let Some(t) = lookup(subst, *n) {
                    let value = matches!(self.types.kind(t), TyKind::ConstValue(_));
                    return if value { ValueArg::Value } else { ValueArg::Type };
                }
                if n.as_str() == "Self" {
                    return ValueArg::Type;
                }
                match self.lookup_pkg(loc.pkg, *n).or_else(|| self.lookup_prelude(*n)) {
                    Some(decl) => decl,
                    None if super::ty::PRIMITIVE_NAMES.contains(&n.as_str()) => return ValueArg::Type,
                    None if self.import_failed(loc, *n) => return ValueArg::Type,
                    None => return ValueArg::Undefined(*n),
                }
            }
            E::Member { recv, name, safe: false } => {
                let pkg = match recv.kind {
                    E::Ident(p) | E::Const(p) => self.lookup_import(loc, p),
                    _ => None,
                };
                let Some(pkg) = pkg else { return ValueArg::Value };
                if self.input.packages[pkg.0 as usize].path == "core:c" {
                    return ValueArg::Type;
                }
                match self.lookup_pkg(pkg, name.name) {
                    Some(decl) => decl,
                    None => return ValueArg::Type,
                }
            }
            E::Call(call) => {
                let ast::Callee::Name(n) = &call.callee else { return ValueArg::Value };
                match self.lookup_pkg(loc.pkg, n.name).or_else(|| self.lookup_prelude(n.name)) {
                    Some(decl) => decl,
                    None => return ValueArg::Value,
                }
            }
            _ => return ValueArg::Value,
        };
        let d = &self.decls[decl.0 as usize];
        match d.kind {
            DeclKind::Const(c) if !self.is_type_alias_value(&c.value, d.loc, 0) => ValueArg::Value,
            DeclKind::Fn(_) => ValueArg::Value,
            _ => ValueArg::Type,
        }
    }

    /// Whether a constant expression names a generic parameter that `subst`
    /// binds to its placeholder, like `N` in `N + 1` while a signature or a
    /// struct's fields are resolved once for every instance: its value is
    /// only known per instance.
    pub(super) fn reads_placeholder(&self, e: &ast::Expr, subst: &[(Name, TyId)]) -> bool {
        if !subst.iter().any(|(_, t)| matches!(self.types.kind(*t), TyKind::Param(_))) {
            return false;
        }
        let mut names = ConstNames(Vec::new());
        names.visit_expr(&mut e.clone());
        names.0.iter().any(|n| lookup(subst, *n).is_some_and(|t| matches!(self.types.kind(t), TyKind::Param(_))))
    }

    /// Whether a method belongs to an `extend` of a generic struct with a
    /// value parameter, like `extend Pool($T, $M)`: the template can't tell
    /// `M` from a type parameter, so each instance resolves the signature
    /// again, with `M` bound to its value.
    pub(super) fn extends_value_params(&mut self, decl: DeclId) -> bool {
        let Some(owner) = self.decls[decl.0 as usize].owner else { return false };
        if !matches!(self.decls[owner.0 as usize].kind, DeclKind::Extend(_)) {
            return false;
        }
        self.extend_targets(owner).into_iter().any(|t| {
            let decl = match self.types.kind(t) {
                TyKind::Struct(id) => self.struct_decls.get(id).copied(),
                _ => None,
            };
            decl.is_some_and(|d| {
                matches!(self.decls[d.0 as usize].kind, DeclKind::Struct(s) if s.generics.iter().any(|g| g.ty.is_some()))
            })
        })
    }

    /// Returns the instance of a generic struct for `args`, creating it and
    /// resolving its fields the first time.
    pub fn struct_instance(&mut self, decl: DeclId, args: Vec<TyId>, span: Span) -> TyId {
        let key = (decl, args.clone());
        if let Some(&t) = self.struct_insts.get(&key) {
            return t;
        }
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Struct(s) = d.kind else { unreachable!("struct_instance on a non-struct") };
        if !self.check_generic_args(decl, &args, &[], span) {
            return self.types.unknown();
        }
        let instances = self.struct_insts.keys().filter(|(k, _)| *k == decl).count();
        if instances >= MAX_INSTANCES {
            self.report(
                Diagnostic::error(
                    codes::GENERIC_ARGS,
                    format!("`{}` was instantiated with too many different types", d.name),
                )
                .primary(span, "this looks like unbounded recursion through type arguments"),
            );
            return self.types.unknown();
        }
        let subst: Vec<(Name, TyId)> = s.generics.iter().map(|g| g.name.name).zip(args.iter().copied()).collect();
        let shown: Vec<String> = args.iter().map(|a| self.types.display(*a)).collect();
        let prefix = self.pkg_prefix(d.loc.pkg);
        let ty = self.types.new_struct(StructInfo {
            opaque: false,
            name: format!("{}({})", d.name, shown.join(", ")),
            c_name: format!("{prefix}__{}__{}", mangle_ident(d.name.as_str()), instances + 1),
            fields: Vec::new(),
            size: 0,
            align: 1,
            complete: false,
            foreign: false,
            span: d.span,
        });
        self.struct_insts.insert(key, ty);
        let TyKind::Struct(sid) = *self.types.kind(ty) else { unreachable!() };
        self.struct_decls.insert(sid, decl);
        let subst: Subst = Rc::new(subst);
        self.struct_args.insert(sid, subst.clone());
        let ctx = TyCtx { loc: d.loc, self_ty: Some(ty), subst };
        // Each instance resolves its own fields, so a `[N]T` field left for
        // the instances doesn't make the type that names this one wait too.
        let deferrals = self.value_deferrals;
        let mut fields: Vec<FieldInfo> = Vec::new();
        let mut spans = Vec::new();
        for item in &s.body {
            let ItemKind::Field(f) = &item.kind else { continue };
            spans.push(f.ty.span);
            let mut fty = self.resolve_type(&f.ty, &ctx);
            if self.by_value_incomplete(fty).is_some() {
                let shown = self.types.display(fty);
                self.report(
                    Diagnostic::error(codes::RECURSIVE_TYPE, format!("`{}` contains itself", d.name))
                        .primary(f.name.span, format!("this field's `{shown}` is stored by value"))
                        .note("a type cannot contain a full copy of itself, so it would have no finite size")
                        .suggest_replace(
                            "store a pointer instead",
                            f.ty.span,
                            format!("^{}", self.source_text(f.ty.span)),
                            wid_diagnostics::Applicability::MaybeIncorrect,
                        ),
                );
                fty = self.types.unknown();
            }
            fields.push(FieldInfo {
                name: f.name.name,
                ty: fty,
                offset: 0,
                using: f.using,
                span: f.name.span,
                c_conv: None,
                c_name: None,
            });
        }
        self.value_deferrals = deferrals;
        let name = self.types.display(ty);
        self.check_fields_size(&name, &mut fields, &spans);
        let parts: Vec<(u64, u64)> = fields.iter().map(|f| self.types.layout(f.ty)).collect();
        for (f, off) in fields.iter_mut().zip(offsets(&parts)) {
            f.offset = off;
        }
        let (size, align) = crate::types::aggregate(&parts);
        let info = &mut self.types.structs[sid.0 as usize];
        info.fields = fields;
        info.size = size;
        info.align = align;
        info.complete = true;
        ty
    }

    /// The bindings a struct instance was created with.
    pub fn instance_bindings(&self, ty: TyId) -> Vec<(Name, TyId)> {
        match self.types.kind(ty) {
            TyKind::Struct(id) => self.struct_args.get(id).map(|s| s.as_ref().clone()).unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    /// The signature of a declaration with `subst` applied.
    pub fn fn_sig_inst(&mut self, decl: DeclId, subst: &[(Name, TyId)]) -> FnSig {
        let template = self.fn_sig(decl);
        if subst.is_empty() {
            return template;
        }
        if template.per_instance {
            // `[N]T` or `Pool(T, N + 1)` reads the value bound to `N`, which
            // the template only has a placeholder for.
            let mut bindings = self.template_subst(decl).as_ref().clone();
            for (name, ty) in bindings.iter_mut() {
                if let Some(bound) = lookup(subst, *name) {
                    *ty = bound;
                }
            }
            for &(name, ty) in subst {
                if lookup(&bindings, name).is_none() {
                    bindings.push((name, ty));
                }
            }
            return self.resolve_sig(decl, Rc::new(bindings));
        }
        let params = template
            .params
            .iter()
            .map(|p| ParamSig { name: p.name, ty: self.subst_type(p.ty, subst), span: p.span })
            .collect();
        let ret = self.subst_type(template.ret, subst);
        let receiver = template.receiver.map(|r| self.subst_type(r, subst));
        let block = template.block.map(|b| super::inline::BlockSig {
            params: b.params.iter().map(|p| self.subst_type(*p, subst)).collect(),
            ret: self.subst_type(b.ret, subst),
        });
        FnSig { params, ret, receiver, block, c_variadic: template.c_variadic, per_instance: false }
    }

    /// Returns the function id of an instance, queuing it for lowering.
    pub fn fn_instance_with(&mut self, decl: DeclId, subst: Subst, origin: Span) -> FnId {
        let key: Vec<TyId> = subst.iter().map(|(_, t)| *t).collect();
        if let Some(&id) = self.fn_insts.get(&(decl, key.clone())) {
            return id;
        }
        let id = FnId(self.functions.len() as u32);
        self.functions.push(None);
        self.fn_insts.insert((decl, key), id);
        self.queue.push_back(super::PendingFn { id, decl, subst, origin });
        id
    }

    /// Infers bindings for a generic call and reports unbound parameters.
    /// `bindings` may already hold the receiver's or owner's bindings.
    pub fn finish_bindings(&mut self, decl: DeclId, bindings: Vec<(Name, TyId)>, span: Span) -> Option<Subst> {
        let names = self.generic_names(decl);
        let mut ordered = Vec::new();
        for n in names {
            match lookup(&bindings, n) {
                Some(t) => ordered.push((n, t)),
                None => {
                    let fname = self.decls[decl.0 as usize].name;
                    self.report(
                        Diagnostic::error(codes::GENERIC_ARGS, format!("cannot infer `{n}` in this call to `{fname}`"))
                            .primary(span, format!("nothing here determines `{n}`"))
                            .help("pass an argument whose type mentions it, or give a typed value"),
                    );
                    return None;
                }
            }
        }
        Some(Rc::new(ordered))
    }

    // ----- extensions -----------------------------------------------------------

    /// The receiver patterns of an `extend` declaration, resolved once.
    pub(super) fn extend_targets(&mut self, decl: DeclId) -> Vec<TyId> {
        if let Some(t) = self.extend_patterns.get(&decl) {
            return t.clone();
        }
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Extend(e) = d.kind else { return Vec::new() };
        let subst = self.template_subst_for_targets(&e.targets);
        let ctx = TyCtx { loc: d.loc, self_ty: None, subst };
        let targets: Vec<TyId> = e.targets.iter().map(|t| self.resolve_type(t, &ctx)).collect();
        self.extend_patterns.insert(decl, targets.clone());
        targets
    }

    fn template_subst_for_targets(&mut self, targets: &[ast::TypeExpr]) -> Subst {
        let mut names = Vec::new();
        for t in targets {
            collect_params(t, &mut names);
        }
        Rc::new(names.into_iter().map(|n| (n, self.types.intern(TyKind::Param(n)))).collect())
    }

    /// Finds an extension method `name` for `receiver`, returning the method
    /// and the bindings (including `Self`) that apply.
    pub fn find_extension(&mut self, receiver: TyId, name: Name, span: Span) -> Option<(DeclId, Vec<(Name, TyId)>)> {
        if matches!(self.types.kind(receiver), TyKind::Unknown) {
            return None;
        }
        let mut found: Vec<(DeclId, Vec<(Name, TyId)>)> = Vec::new();
        let extends = self.extends.clone();
        for ext in extends {
            let own = self.members.get(&ext).and_then(|m| m.get(&name)).copied();
            let Some(method) = own.or_else(|| self.module_member(ext, name, &mut Vec::new())) else { continue };
            if let Some(bindings) = self.extend_bindings(ext, receiver) {
                found.push((method, bindings));
            }
        }
        if found.len() > 1 {
            let spans: Vec<Span> = found.iter().map(|(d, _)| self.decls[d.0 as usize].span).collect();
            let shown = self.types.display(receiver);
            let mut diag = Diagnostic::error(
                codes::NO_MATCHING_OVERLOAD,
                format!("`{name}` is defined for `{shown}` by more than one `extend`"),
            )
            .primary(span, "ambiguous method");
            for s in spans {
                diag = diag.secondary(s, "one definition");
            }
            diag = diag
                .note("`extend` blocks do not override each other, even when one is more specific")
                .help("remove or rename one of the definitions");
            self.report(diag);
        }
        found.into_iter().next()
    }

    /// The `extend` blocks whose methods `receiver` has.
    pub fn extends_of(&mut self, receiver: TyId) -> Vec<DeclId> {
        if matches!(self.types.kind(receiver), TyKind::Unknown) {
            return Vec::new();
        }
        let extends = self.extends.clone();
        extends.into_iter().filter(|&ext| self.extend_bindings(ext, receiver).is_some()).collect()
    }

    /// The bindings, `Self` among them, under which the first target of
    /// `extend` block `ext` that matches `receiver` applies to it.
    fn extend_bindings(&mut self, ext: DeclId, receiver: TyId) -> Option<Vec<(Name, TyId)>> {
        for pattern in self.extend_targets(ext) {
            if matches!(self.types.kind(pattern), TyKind::Slice(_))
                && !matches!(self.types.kind(receiver), TyKind::Slice(_))
            {
                continue;
            }
            let mut bindings = vec![(Name::new("Self"), receiver)];
            if self.unify(pattern, receiver, &mut bindings) {
                return Some(bindings);
            }
        }
        None
    }

    /// Whether `decl` is a method of an `extend` whose targets are all
    /// concrete types (`extend P`, `extend Int, String`), with no `$`
    /// parameters of its own: a method of each target, which only `Self`
    /// tells apart, rather than generic code. A method of an `extend` over a
    /// pattern (`extend []$T`) or with `$` parameters is generic.
    pub(super) fn is_concrete_extension(&self, decl: DeclId) -> bool {
        let Some(owner) = self.decls[decl.0 as usize].owner else { return false };
        let DeclKind::Extend(e) = self.decls[owner.0 as usize].kind else { return false };
        let mut params = Vec::new();
        for t in &e.targets {
            collect_params(t, &mut params);
        }
        params.is_empty() && self.generic_names(decl) == [Name::new("Self")]
    }

    /// For a method of an `extend` of concrete types, the bindings of
    /// `Self` to each target that resolved, which it is checked with
    /// whether or not anything calls it; empty for any other method.
    pub(super) fn concrete_extension_substs(&mut self, decl: DeclId) -> Vec<Subst> {
        if !self.is_concrete_extension(decl) {
            return Vec::new();
        }
        let Some(owner) = self.decls[decl.0 as usize].owner else { return Vec::new() };
        let targets = self.extend_targets(owner);
        let mut substs: Vec<Subst> = Vec::new();
        for t in targets {
            let known = !matches!(self.types.kind(t), TyKind::Unknown) && !self.has_params(t);
            if known && !substs.iter().any(|s| s[0].1 == t) {
                substs.push(Rc::new(vec![(Name::new("Self"), t)]));
            }
        }
        substs
    }

    /// For a method of an `extend` of several concrete types checked with
    /// `Self` bound by `subst`: that type, as shown, and where the `extend`
    /// line names it, which errors in the method point at.
    pub(super) fn extension_target_site(&mut self, decl: DeclId, subst: &[(Name, TyId)]) -> Option<(String, Span)> {
        let owner = self.decls[decl.0 as usize].owner?;
        let DeclKind::Extend(e) = self.decls[owner.0 as usize].kind else { return None };
        if e.targets.len() < 2 {
            return None;
        }
        let self_ty = lookup(subst, Name::new("Self"))?;
        let at = self.extend_targets(owner).iter().position(|t| *t == self_ty)?;
        Some((self.types.display(self_ty), e.targets.get(at)?.span))
    }

    /// Records the `extend` declarations of all packages.
    pub fn register_extend(&mut self, decl: DeclId) {
        self.extends.push(decl);
    }

    /// Instance display name for functions, like `max[Int]`.
    pub fn instance_display(&self, base: &str, subst: &[(Name, TyId)]) -> String {
        let shown: Vec<String> =
            subst.iter().filter(|(n, _)| n.as_str() != "Self").map(|(_, t)| self.types.display(*t)).collect();
        if shown.is_empty() { base.to_string() } else { format!("{base}[{}]", shown.join(", ")) }
    }

    /// Instance counter per declaration, used for C names.
    pub fn instance_number(&mut self, decl: DeclId) -> usize {
        let n = self.instance_counts.entry(decl).or_insert(0);
        *n += 1;
        *n
    }
}

/// What a value parameter's argument is (see
/// [`Checker::value_generic_arg`]).
enum ValueArg {
    /// A value to evaluate as a constant.
    Value,
    /// A type or a generic parameter, resolved as a type.
    Type,
    /// A name nothing declares.
    Undefined(Name),
}

/// Collects the constant names an expression reads: `N` in `N + 1`, and
/// one-segment type names, like `T` in `comptime size_of(T)`.
struct ConstNames(Vec<Name>);

impl VisitMut for ConstNames {
    fn visit_expr(&mut self, expr: &mut ast::Expr) {
        if let ast::ExprKind::Const(n) = expr.kind {
            self.0.push(n);
        }
        walk_expr(self, expr);
    }

    fn visit_type(&mut self, ty: &mut ast::TypeExpr) {
        if let ast::TypeKind::Path { segments, .. } = &ty.kind
            && let [only] = segments.as_slice()
        {
            self.0.push(only.name);
        }
        walk_type(self, ty);
    }
}

/// The first field of a struct's body whose type has an array of length
/// `name` exactly (`[N]T`, also nested): the field's name and the array.
fn sized_field(body: &[ast::Item], name: Name) -> Option<(Name, Span)> {
    body.iter().find_map(|item| {
        let ItemKind::Field(f) = &item.kind else { return None };
        let mut find = FindLength { name, span: None };
        find.visit_type(&mut f.ty.clone());
        find.span.map(|s| (f.name.name, s))
    })
}

/// Finds an array type whose length is the constant `name`.
struct FindLength {
    name: Name,
    span: Option<Span>,
}

impl VisitMut for FindLength {
    fn visit_type(&mut self, ty: &mut ast::TypeExpr) {
        if let ast::TypeKind::Array(len, _) = &ty.kind
            && matches!(len.kind, ast::ExprKind::Const(n) if n == self.name)
            && self.span.is_none()
        {
            self.span = Some(ty.span);
        }
        walk_type(self, ty);
    }
}

/// Finds where a type introduces generic parameter `$name`.
struct FindParam {
    name: Name,
    span: Option<Span>,
}

impl VisitMut for FindParam {
    fn visit_type(&mut self, ty: &mut ast::TypeExpr) {
        if let ast::TypeKind::Param(n) = &ty.kind
            && n.name == self.name
            && self.span.is_none()
        {
            self.span = Some(ty.span);
        }
        walk_type(self, ty);
    }
}

/// Collects `$T` names mentioned in a type expression.
pub(crate) fn collect_params(t: &ast::TypeExpr, out: &mut Vec<Name>) {
    use ast::TypeKind as K;
    match &t.kind {
        K::Param(n) => {
            if !out.contains(&n.name) {
                out.push(n.name);
            }
        }
        K::Path { args, .. } => {
            for a in args {
                if let ast::GenericArg::Type(t) = a {
                    collect_params(t, out);
                }
            }
        }
        K::Pointer(i) | K::MultiPointer(i) | K::Slice(i) | K::Dynamic(i) | K::Optional(i) | K::Distinct(i) => {
            collect_params(i, out)
        }
        K::Array(len, i) => {
            if let ast::ExprKind::Type(t) = &len.kind {
                collect_params(t, out);
            }
            collect_params(i, out);
        }
        K::Map(k, v) => {
            collect_params(k, out);
            collect_params(v, out);
        }
        K::Proc { params, ret, .. } | K::Block { params, ret } => {
            for p in params {
                collect_params(p, out);
            }
            if let Some(r) = ret {
                collect_params(r, out);
            }
        }
        K::Tuple(elems) => {
            for e in elems {
                collect_params(e, out);
            }
        }
        K::Matrix { elem, .. } => collect_params(elem, out),
        K::Error | K::Splice(_) | K::Spliced(_) => {}
    }
}

//! Generics: type parameters (`$T`), generic structs, inference by
//! unification, instantiation, and `extend` blocks that add methods to
//! existing types.
//!
//! Generic code follows template semantics: signatures are resolved once with
//! `TyKind::Param` placeholders, and bodies are checked per instantiation.

use std::rc::Rc;

use wid_diagnostics::{Diagnostic, Span, codes};
use wid_syntax::Name;
use wid_syntax::ast::{self, ItemKind};

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
                DeclKind::Module(_) => names.push(Name::new("Self")),
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
                (_, TyKind::Unknown | TyKind::Param(_)) => {}
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
        let mut fields: Vec<FieldInfo> = Vec::new();
        for item in &s.body {
            let ItemKind::Field(f) = &item.kind else { continue };
            let mut fty = self.resolve_type(&f.ty, &ctx);
            if self.by_value_incomplete(fty).is_some() {
                let shown = self.types.display(fty);
                self.report(
                    Diagnostic::error(codes::RECURSIVE_TYPE, format!("`{}` contains itself", d.name))
                        .primary(
                            f.name.span,
                            format!("this field stores {} `{shown}` by value", wid_diagnostics::a_or_an(&shown)),
                        )
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
        FnSig { params, ret, receiver, block, c_variadic: template.c_variadic }
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
    fn extend_targets(&mut self, decl: DeclId) -> Vec<TyId> {
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
            for pattern in self.extend_targets(ext) {
                if matches!(self.types.kind(pattern), TyKind::Slice(_))
                    && !matches!(self.types.kind(receiver), TyKind::Slice(_))
                {
                    continue;
                }
                let mut bindings = vec![(Name::new("Self"), receiver)];
                if self.unify(pattern, receiver, &mut bindings) {
                    found.push((method, bindings));
                    break;
                }
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
        K::Error => {}
    }
}

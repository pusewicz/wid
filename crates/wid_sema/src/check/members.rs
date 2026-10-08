//! Member access: fields, methods, `self`, type-level members and package
//! members.

use wid_diagnostics::{Applicability, Diagnostic, Span, codes, did_you_mean};
use wid_syntax::Name;
use wid_syntax::ast::{self, ExprKind as E, Ident};

use super::macros::MacroCall;
use super::{Checker, DeclId, DeclKind};
use crate::input::PackageId;
use crate::ir::{self, ExprKind};
use crate::types::{TyId, TyKind};

/// What the left side of `a.b` turned out to be.
pub(crate) enum Receiver {
    /// An imported package.
    Package(PackageId),
    /// A type, for `Type.new` and type-level methods.
    Type(TyId),
    /// An ordinary value.
    Value,
}

/// Method names people reach for that Wid spells differently.
const SYNONYMS: &[(&str, &str)] = &[
    ("length", "size"),
    ("len", "size"),
    ("count", "size"),
    ("is_nil", "nil?"),
    ("null?", "nil?"),
    ("to_int", "to_i"),
    ("to_float", "to_f"),
    ("to_string", "to_s"),
    ("toString", "to_s"),
    ("push", "<<"),
    ("append", "<<"),
];

/// Returns true when `e` denotes a storage location that can be assigned to
/// or have its address taken.
pub(crate) fn is_place(e: &ir::Expr) -> bool {
    match &e.kind {
        ExprKind::Local(_) | ExprKind::Global(_) | ExprKind::Deref(_) | ExprKind::Index { .. } => true,
        ExprKind::Field { base, .. } | ExprKind::OptGet(base) | ExprKind::UnionGet { value: base, .. } => {
            is_place(base)
        }
        _ => false,
    }
}

impl<'a> Checker<'a> {
    /// Returns `self` as a place of type `Self`, reporting when there is no
    /// receiver.
    pub fn self_place(&mut self, span: Span, what: &str) -> ir::Expr {
        let frame = self.frame();
        match (frame.self_ty, frame.self_local) {
            (Some(ty), Some(local)) => {
                let ptr_ty = self.local_ty(local);
                ir::Expr::new(ExprKind::Deref(Box::new(ir::Expr::new(ExprKind::Local(local), ptr_ty))), ty)
            }
            (Some(_), None) => {
                self.report(
                    Diagnostic::error(
                        codes::SELF_OUTSIDE_METHOD,
                        format!("{what} is not available in a type-level method"),
                    )
                    .primary(span, "`def self.…` methods have no receiver")
                    .help("remove `self.` from the method name to make it an instance method"),
                );
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
            (None, _) => {
                self.report(
                    Diagnostic::error(
                        codes::SELF_OUTSIDE_METHOD,
                        format!("{what} only exists inside a method of a type"),
                    )
                    .primary(span, "there is no receiver here")
                    .help("define this method inside a `struct` or `enum` to use `self` and `@fields`"),
                );
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
        }
    }

    /// Lowers `@name`.
    pub fn ivar(&mut self, name: Name, span: Span) -> ir::Expr {
        let base = self.self_place(span, &format!("`@{name}`"));
        if matches!(self.types.kind(base.ty), TyKind::Unknown) {
            return base;
        }
        match self.field_index(base.ty, name) {
            Some((index, ty)) => ir::Expr::new(ExprKind::Field { base: Box::new(base), index }, ty),
            None => {
                if self.using_lookup(base.ty, name, span).is_some() {
                    let mut inner = base;
                    while self.field_index(inner.ty, name).is_none()
                        && let Some((index, using_ty)) = self.using_lookup(inner.ty, name, span)
                    {
                        inner = ir::Expr::new(ExprKind::Field { base: Box::new(inner), index }, using_ty);
                        if let TyKind::Pointer(t) = *self.types.kind(using_ty) {
                            inner = ir::Expr::new(ExprKind::Deref(Box::new(inner)), t);
                        }
                    }
                    if let Some((i, t)) = self.field_index(inner.ty, name) {
                        return ir::Expr::new(ExprKind::Field { base: Box::new(inner), index: i }, t);
                    }
                    let shown = self.types.display(inner.ty);
                    self.report(
                        Diagnostic::error(
                            codes::NO_SUCH_MEMBER,
                            format!("`@{name}` names a method of `{shown}`, not a field"),
                        )
                        .primary(span, "instance variables only reach fields")
                        .suggest_replace(
                            "call the method",
                            span,
                            name.as_str().to_string(),
                            Applicability::MaybeIncorrect,
                        ),
                    );
                    return ir::Expr::new(ExprKind::Zero, self.types.unknown());
                }
                self.no_member(base.ty, name, span, true);
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
        }
    }

    /// Classifies the left side of a member access without lowering values.
    pub fn classify_receiver(&mut self, recv: &ast::Expr) -> Receiver {
        match &recv.kind {
            E::Ident(n) if self.find_var_at(*n, recv.span).is_none() => {
                match self.lookup_import(self.loc_at(recv.span), *n) {
                    Some(p) => Receiver::Package(p),
                    None => Receiver::Value,
                }
            }
            E::Const(n) => {
                if n.as_str() == "Self"
                    && let Some(t) = self.body.frames.last().and_then(|f| f.self_ty)
                {
                    return Receiver::Type(t);
                }
                // A value parameter, like `N` in `N.times`, is a value.
                if let Some(t) = self.body.frames.last().and_then(|f| super::generics::lookup(&f.subst, *n))
                    && !matches!(self.types.kind(t), TyKind::ConstValue(_))
                {
                    return Receiver::Type(t);
                }
                let loc = self.loc_at(recv.span);
                if let Some(decl) = self.lookup_pkg(loc.pkg, *n).or_else(|| self.lookup_prelude(*n)) {
                    if self.missing_generic_args(decl, n.as_str(), recv.span) {
                        return Receiver::Type(self.types.unknown());
                    }
                    return match self.decls[decl.0 as usize].kind {
                        DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Union(_) => {
                            Receiver::Type(self.decl_as_type(decl, recv.span))
                        }
                        DeclKind::Const(c)
                            if self.is_type_alias_value(&c.value, self.decls[decl.0 as usize].loc, 0) =>
                        {
                            Receiver::Type(self.decl_as_type(decl, recv.span))
                        }
                        _ => Receiver::Value,
                    };
                }
                if let Some(t) = self.primitive(n.as_str()) {
                    return Receiver::Type(t);
                }
                match self.lookup_import(loc, *n) {
                    Some(p) => Receiver::Package(p),
                    None => Receiver::Value,
                }
            }
            E::Type(t) => {
                let ctx = self.body_ctx();
                Receiver::Type(self.resolve_type(t, &ctx))
            }
            // `Pool(Ball, 64)`, `geo.Grid(Point, 3)` or `Outcome(Int)`.
            E::Call(call) => match self.generic_instance(call, recv.span) {
                Some(t) => Receiver::Type(t),
                None => Receiver::Value,
            },
            E::Member { recv: inner, name, .. } if name.as_str().starts_with(char::is_uppercase) => {
                if let Receiver::Package(p) = self.classify_receiver(inner)
                    && let Some(decl) = self.lookup_pkg(p, name.name)
                    && matches!(
                        self.decls[decl.0 as usize].kind,
                        DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Union(_)
                    )
                {
                    self.check_visible(decl, name.span);
                    let shown = format!("{}.{}", self.source_text(inner.span), name.as_str());
                    if self.missing_generic_args(decl, &shown, recv.span) {
                        return Receiver::Type(self.types.unknown());
                    }
                    return Receiver::Type(self.decl_as_type(decl, recv.span));
                }
                Receiver::Value
            }
            _ => Receiver::Value,
        }
    }

    /// Reports a generic struct or union named without its arguments where
    /// a type is needed, like `Pool.new` or `geo.Grid.size`, and returns
    /// true for one.
    fn missing_generic_args(&mut self, decl: DeclId, shown: &str, span: Span) -> bool {
        let generics = match self.decls[decl.0 as usize].kind {
            DeclKind::Struct(s) => &s.generics,
            DeclKind::Union(u) => &u.generics,
            _ => return false,
        };
        if generics.is_empty() {
            return false;
        }
        let names: Vec<&str> = generics.iter().map(|g| g.name.as_str()).collect();
        self.report(
            Diagnostic::error(codes::GENERIC_ARGS, format!("`{shown}` needs type arguments"))
                .primary(span, format!("write it like `{shown}({})`", names.join(", "))),
        );
        true
    }

    /// Lowers `recv.name` or `recv.name(args)`.
    #[expect(clippy::too_many_arguments, reason = "mirrors the parts of a call expression")]
    pub fn member_call(
        &mut self,
        recv: &ast::Expr,
        name: Ident,
        args: Option<&[ast::Arg]>,
        block: Option<&ast::BlockArg>,
        safe: bool,
        span: Span,
        expected: Option<TyId>,
    ) -> ir::Expr {
        if safe {
            return self.safe_member(recv, name, args, span);
        }
        match self.classify_receiver(recv) {
            Receiver::Package(pkg) => {
                self.package_member(pkg, recv.span, name, args.unwrap_or(&[]), block, span, expected)
            }
            Receiver::Type(ty) => self.type_member(ty, name, args.unwrap_or(&[]), block, span, recv.span),
            Receiver::Value => {
                let v = if name.as_str() == "nil?" { self.nilable_operand(recv) } else { self.expr(recv, None) };
                self.value_member(v, recv.span, name, args, block, span, expected)
            }
        }
    }

    #[expect(clippy::too_many_arguments, reason = "mirrors the parts of a call expression")]
    fn package_member(
        &mut self,
        pkg: PackageId,
        pkg_span: Span,
        name: Ident,
        args: &[ast::Arg],
        block: Option<&ast::BlockArg>,
        span: Span,
        expected: Option<TyId>,
    ) -> ir::Expr {
        let Some(decl) = self.lookup_pkg(pkg, name.name) else {
            if self.pkg_incomplete(pkg) || self.report_not_imported(pkg, name.name, name.span) {
                return ir::Expr::new(ExprKind::Zero, self.types.unknown());
            }
            let candidates = self.package_names(pkg);
            let pkg_name = self.input.packages[pkg.0 as usize].name.clone();
            self.undefined(name.name, name.span, candidates, &format!("member of `{pkg_name}`"));
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        };
        self.check_visible(decl, name.span);
        match self.decls[decl.0 as usize].kind {
            DeclKind::Fn(f) if f.is_macro => {
                let shown = format!("{}.{}", self.source_text(pkg_span), name.as_str());
                let call = MacroCall { decl, shown, args, block, name_span: name.span, span };
                self.call_macro(call, expected)
            }
            DeclKind::Fn(_) => self.call_fn(decl, None, args, block, name.span, span),
            DeclKind::Const(_) => self.const_ref_decl(decl, span, expected),
            DeclKind::Overload(_) => self.call_overloaded(decl, None, None, args, name.span, span),
            ref other => {
                let what = other.a_describe();
                let mut diag =
                    Diagnostic::error(codes::NOT_A_VALUE, format!("`{}` is {what}, not a value", name.as_str()))
                        .primary(name.span, "expected a method or constant");
                // `geo.Grid(Int, 3)` names a type of the package.
                if let DeclKind::Struct(s) = other
                    && !s.generics.is_empty()
                    && !args.is_empty()
                {
                    let text = self.source_text(span);
                    diag = diag.suggest_replace(
                        format!("`{text}` is a type: build a value of it with `new`"),
                        span,
                        format!("{text}.new"),
                        Applicability::MachineApplicable,
                    );
                }
                self.report(diag);
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
        }
    }

    /// Reports access to a `private` declaration from another package.
    pub fn check_visible(&mut self, decl: DeclId, span: Span) {
        let d = &self.decls[decl.0 as usize];
        if !d.private || d.owner.is_some() {
            return;
        }
        let from = self.loc_at(span).pkg;
        self.check_visible_from(decl, from, span);
    }

    /// Reports a use of a private declaration from package `from`.
    pub fn check_visible_from(&mut self, decl: DeclId, from: crate::input::PackageId, span: Span) {
        let d = &self.decls[decl.0 as usize];
        if d.private && d.owner.is_none() && d.loc.pkg != from {
            let pkg = self.input.packages[d.loc.pkg.0 as usize].name.clone();
            let def_span = d.span;
            self.report(
                Diagnostic::error(codes::PRIVATE_ITEM, format!("`{}` is private to package `{pkg}`", d.name))
                    .primary(span, "not visible from here")
                    .secondary(def_span, "declared `private` here")
                    .help(format!("use a public method of `{pkg}` instead, or remove `private` from the declaration")),
            );
        }
    }

    /// Lowers `Type.name(args)`.
    fn type_member(
        &mut self,
        ty: TyId,
        name: Ident,
        args: &[ast::Arg],
        block: Option<&ast::BlockArg>,
        span: Span,
        recv_span: Span,
    ) -> ir::Expr {
        let kind = self.types.kind(ty).clone();
        if matches!(kind, TyKind::Matrix(..)) && name.as_str() == "identity" {
            return self.matrix_identity(ty, span);
        }
        if ty == self.types.context_ty && name.as_str() == "default" {
            return ir::Expr::new(ExprKind::Builtin { op: ir::Builtin::DefaultContext, args: Vec::new(), span }, ty);
        }
        if matches!(name.as_str(), "size" | "align") && args.is_empty() && !matches!(kind, TyKind::Unknown) {
            let (size, align) = self.types.layout(ty);
            let int = self.types.int();
            let v = if name.as_str() == "size" { size } else { align };
            return ir::Expr::new(ExprKind::Int(i128::from(v)), int);
        }
        if name.as_str() == "new" {
            return match kind {
                TyKind::Struct(_) => self.struct_new(ty, args, span),
                TyKind::Dynamic(_) | TyKind::Map(..) => self.container_new(ty, args, span),
                TyKind::Unknown => ir::Expr::new(ExprKind::Zero, ty),
                _ => {
                    let shown = self.types.display(ty);
                    self.report(
                        Diagnostic::error(codes::NO_SUCH_MEMBER, format!("`{shown}` has no `new`"))
                            .primary(name.span, "only structs and containers are built with `new`"),
                    );
                    ir::Expr::new(ExprKind::Zero, self.types.unknown())
                }
            };
        }
        let enum_has = |this: &Self| match this.types.kind(ty) {
            TyKind::Enum(id) => this.types.enum_info(*id).members.iter().any(|(n, _)| *n == name.name),
            _ => false,
        };
        if args.is_empty()
            && !matches!(kind, TyKind::Unknown)
            && self.find_method(ty, name.name).is_none()
            && !enum_has(self)
            && let Some(v) = self.type_reflection(ty, &name, span)
        {
            return v;
        }
        if let TyKind::Enum(_) = kind
            && args.is_empty()
            && self.find_method(ty, name.name).is_none()
        {
            if !enum_has(self) && self.members_incomplete(ty) {
                return ir::Expr::new(ExprKind::Zero, self.types.unknown());
            }
            return self.enum_member(ty, name.name, name.span);
        }
        if let Some(decl) = self.find_method(ty, name.name) {
            match self.decls[decl.0 as usize].kind {
                DeclKind::Overload(_) => {
                    if let Some(b) = block {
                        self.reject_block(b, "overloaded methods do not take blocks");
                    }
                    return self.call_overloaded(decl, None, Some(ty), args, name.span, span);
                }
                DeclKind::Fn(f) if f.is_static => {
                    self.owner_bindings = self.instance_bindings(ty);
                    return self.call_fn(decl, None, args, block, name.span, span);
                }
                DeclKind::Fn(_) => {
                    let shown = self.types.display(ty);
                    self.report(
                        Diagnostic::error(
                            codes::NO_SUCH_MEMBER,
                            format!(
                                "`{}` is an instance method; call it on {} `{shown}` value",
                                name.as_str(),
                                wid_diagnostics::a_or_an(&shown)
                            ),
                        )
                        .primary(name.span, "needs a receiver")
                        .help(format!(
                            "to make it callable as `{shown}.{}`, declare it `def self.{}`",
                            name.as_str(),
                            name.as_str()
                        )),
                    );
                    return ir::Expr::new(ExprKind::Zero, self.types.unknown());
                }
                DeclKind::Const(_) => return self.const_ref_decl(decl, span, None),
                _ => {}
            }
        }
        if let Some(decl) = self.find_included(ty, name.name)
            && let DeclKind::Fn(f) = self.decls[decl.0 as usize].kind
            && f.is_static
        {
            self.check_private_method(decl, ty, name.span);
            self.owner_bindings = vec![(Name::new("Self"), ty)];
            return self.call_fn(decl, None, args, block, name.span, span);
        }
        if matches!(kind, TyKind::Unknown) {
            return ir::Expr::new(ExprKind::Zero, ty);
        }
        let _ = recv_span;
        self.no_member(ty, name.name, name.span, false);
        ir::Expr::new(ExprKind::Zero, self.types.unknown())
    }

    /// Finds a method or member constant declared on a type.
    pub fn find_method(&mut self, ty: TyId, name: Name) -> Option<DeclId> {
        let decl = match self.types.kind(ty) {
            TyKind::Struct(id) => self.struct_decls.get(id).copied(),
            TyKind::Enum(id) => self.enum_decls.get(id).copied(),
            _ => None,
        }?;
        self.members.get(&decl).and_then(|m| m.get(&name)).copied()
    }

    /// Returns a pointer to `v`, spilling rvalues into a temporary.
    pub fn address_of(&mut self, v: ir::Expr) -> ir::Expr {
        let ptr = self.types.pointer(v.ty);
        if is_place(&v) {
            if let ExprKind::Deref(inner) = v.kind {
                return *inner;
            }
            return ir::Expr::new(ExprKind::AddrOf(Box::new(v)), ptr);
        }
        let tmp = self.spill(v);
        ir::Expr::new(ExprKind::AddrOf(Box::new(tmp)), ptr)
    }

    /// Lowers `value.name` or `value.name(args)`.
    #[expect(clippy::too_many_arguments, reason = "mirrors the parts of a call expression")]
    pub fn value_member(
        &mut self,
        v: ir::Expr,
        recv_span: Span,
        name: Ident,
        args: Option<&[ast::Arg]>,
        block: Option<&ast::BlockArg>,
        span: Span,
        expected: Option<TyId>,
    ) -> ir::Expr {
        if matches!(self.types.kind(v.ty), TyKind::Type) && super::comptime::is_type_query(name.as_str()) {
            return self.type_value_member(v, &name);
        }
        if name.as_str() == "nil?" && self.types.is_nilable(v.ty) {
            if let Some(args) = args {
                self.no_args(args, name);
            }
            let some = self.is_some(v);
            return self.not(some);
        }
        if self.optional_inner(v.ty).is_some() {
            let inner = self.optional_inner(v.ty).unwrap_or(v.ty);
            let is_member =
                self.field_index(inner, name.name).is_some() || self.find_method(inner, name.name).is_some();
            if is_member {
                self.maybe_nil(&v, recv_span);
                let got = self.opt_get(v);
                return self.value_member(got, recv_span, name, args, block, span, expected);
            }
        }
        if name.as_str() == "to" && self.pointer_shape(v.ty).is_some() {
            return self.builtin_method(&v, name, args, span, expected).expect("`to` applies to every value");
        }
        let base = match self.types.kind(v.ty).clone() {
            TyKind::Pointer(inner) => ir::Expr::new(ExprKind::Deref(Box::new(v)), inner),
            _ => v,
        };
        let ty = base.ty;
        if matches!(self.types.kind(ty), TyKind::Unknown) {
            if let Some(args) = args {
                for a in args.iter().filter(|a| !is_type_like(&a.value)) {
                    self.expr(&a.value, None);
                }
            }
            return ir::Expr::new(ExprKind::Zero, ty);
        }
        if let Some((index, fty)) = self.field_index(ty, name.name) {
            let field = ir::Expr::new(ExprKind::Field { base: Box::new(base.clone()), index }, fty);
            match args {
                None => {
                    self.reject_builtin_block(block, name, "a field");
                    return field;
                }
                Some(args) if matches!(self.types.kind(fty), TyKind::Proc(_)) => {
                    self.reject_builtin_block(block, name, "a proc");
                    return self.call_proc(field, args, span);
                }
                Some(_) => {}
            }
        }
        if name.as_str() == "call" && matches!(self.types.kind(ty), TyKind::Proc(_)) {
            self.reject_builtin_block(block, name, "a proc");
            return self.call_proc(base, args.unwrap_or(&[]), span);
        }
        let own = self.find_method(ty, name.name);
        if let Some(decl) = own
            && let DeclKind::Overload(_) = self.decls[decl.0 as usize].kind
        {
            if let Some(b) = block {
                self.reject_block(b, "overloaded methods do not take blocks");
            }
            let recv_ptr = self.address_of(base);
            return self.call_overloaded(decl, Some(recv_ptr), Some(ty), args.unwrap_or(&[]), name.span, span);
        }
        if own.is_none()
            && let Some(decl) = self.find_included(ty, name.name)
            && let DeclKind::Fn(f) = self.decls[decl.0 as usize].kind
            && !f.is_static
        {
            self.check_private_method(decl, ty, name.span);
            let recv_ptr = self.address_of(base);
            self.owner_bindings = vec![(Name::new("Self"), ty)];
            return self.call_fn(decl, Some(recv_ptr), args.unwrap_or(&[]), block, name.span, span);
        }
        if own.is_none()
            && self.field_index(ty, name.name).is_none()
            && let Some((index, using_ty)) = self.using_lookup(ty, name.name, name.span)
        {
            let field = ir::Expr::new(ExprKind::Field { base: Box::new(base), index }, using_ty);
            return self.value_member(field, recv_span, name, args, block, span, expected);
        }
        if let Some(decl) = own
            && let DeclKind::Fn(f) = self.decls[decl.0 as usize].kind
        {
            self.check_private_method(decl, ty, name.span);
            if f.is_static {
                let shown = self.types.display(ty);
                self.report(
                    Diagnostic::error(
                        codes::NO_SUCH_MEMBER,
                        format!("`{}` is a type-level method; call it on the type", name.as_str()),
                    )
                    .primary(name.span, "has no receiver")
                    .suggest_replace(
                        format!("call it as `{shown}.{}`", name.as_str()),
                        recv_span,
                        shown.clone(),
                        Applicability::MaybeIncorrect,
                    ),
                );
                return ir::Expr::new(ExprKind::Zero, self.types.unknown());
            }
            let recv_ptr = self.address_of(base);
            return self.call_fn(decl, Some(recv_ptr), args.unwrap_or(&[]), block, name.span, span);
        }
        if let Some(v) = self.container_method(&base, name, args.unwrap_or(&[]), span) {
            self.reject_builtin_block(block, name, "a built-in method");
            return v;
        }
        if let Some(v) = self.builtin_method(&base, name, args, span, expected) {
            self.reject_builtin_block(block, name, "a built-in method");
            return v;
        }
        if let Some((decl, bindings)) = self.find_extension(ty, name.name, name.span) {
            let recv_ptr = self.address_of(base);
            self.owner_bindings = bindings;
            return self.call_fn(decl, Some(recv_ptr), args.unwrap_or(&[]), block, name.span, span);
        }
        if let TyKind::Array(elem, _) | TyKind::Dynamic(elem) = self.types.kind(self.types.base(ty)).clone() {
            let slice_ty = self.types.slice(elem);
            if let Some((decl, bindings)) = self.find_extension(slice_ty, name.name, name.span) {
                let slice = self.slice_of_container(base, slice_ty);
                let slice = self.spill(slice);
                let recv_ptr = self.address_of(slice);
                self.owner_bindings = bindings;
                return self.call_fn(decl, Some(recv_ptr), args.unwrap_or(&[]), block, name.span, span);
            }
        }
        if let Some((index, fty)) = self.field_index(ty, name.name) {
            let shown = self.types.display(fty);
            let call_span = Span { start: name.span.end, ..span };
            let mut diag =
                Diagnostic::error(codes::NOT_CALLABLE, format!("`{}` is a field, not a method", name.as_str()))
                    .primary(name.span, format!("this field holds {} `{shown}`", wid_diagnostics::a_or_an(&shown)));
            if args.is_some_and(|a| a.is_empty()) && block.is_none() && call_span.end > call_span.start {
                diag = diag.suggest_replace(
                    "read the field without `()`",
                    call_span,
                    "",
                    Applicability::MachineApplicable,
                );
            } else {
                diag = diag.help("read the field without arguments or a block");
            }
            self.report(diag);
            if let Some(args) = args {
                for a in args {
                    self.expr(&a.value, None);
                }
            }
            return ir::Expr::new(ExprKind::Field { base: Box::new(base), index }, fty);
        }
        self.no_member(ty, name.name, name.span, false);
        if let Some(args) = args {
            for a in args {
                self.expr(&a.value, None);
            }
        }
        ir::Expr::new(ExprKind::Zero, self.types.unknown())
    }

    /// Reports a block passed to a field, proc or built-in method, none of
    /// which run blocks.
    pub fn reject_builtin_block(&mut self, block: Option<&ast::BlockArg>, name: Ident, what: &str) {
        let Some(block) = block else { return };
        self.report(
            Diagnostic::error(codes::BLOCK_MISMATCH, format!("`{}` does not take a block", name.as_str()))
                .primary(block.span, "this block is never called")
                .secondary(name.span, format!("`{}` is {what}", name.as_str()))
                .help("remove the block"),
        );
    }

    /// Methods every value of a suitable type has.
    fn builtin_method(
        &mut self,
        v: &ir::Expr,
        name: Ident,
        args: Option<&[ast::Arg]>,
        span: Span,
        _expected: Option<TyId>,
    ) -> Option<ir::Expr> {
        let ty = v.ty;
        let args = args.unwrap_or(&[]);
        let base = self.types.base(ty);
        let kind = self.types.kind(base).clone();
        let text = name.as_str();
        let result = match (text, &kind) {
            ("to_s", _) => {
                self.no_args(args, name);
                self.lower_to_s(v.clone(), span)
            }
            ("inspect", _) => {
                self.no_args(args, name);
                self.inspect_string(v.clone(), span)
            }
            ("size", TyKind::String) => {
                self.no_args(args, name);
                let int = self.types.int();
                ir::Expr::new(ExprKind::Builtin { op: ir::Builtin::Len, args: vec![v.clone()], span }, int)
            }
            ("empty?", TyKind::String) => {
                self.no_args(args, name);
                let int = self.types.int();
                let bool_ty = self.types.bool();
                let len = ir::Expr::new(ExprKind::Builtin { op: ir::Builtin::Len, args: vec![v.clone()], span }, int);
                let zero = ir::Expr::new(ExprKind::Int(0), int);
                ir::Expr::new(
                    ExprKind::Binary { op: ir::BinaryOp::Eq, lhs: Box::new(len), rhs: Box::new(zero), span },
                    bool_ty,
                )
            }
            ("to_sym", TyKind::String) => {
                self.no_args(args, name);
                let symbol = self.types.symbol();
                ir::Expr::new(ExprKind::Builtin { op: ir::Builtin::ToSymbol, args: vec![v.clone()], span }, symbol)
            }
            ("to_cstr", TyKind::String) => {
                self.no_args(args, name);
                let cstring = self.types.cstring();
                let temp = self.context_field("temp_allocator");
                ir::Expr::new(
                    ExprKind::Builtin { op: ir::Builtin::ToCString, args: vec![v.clone(), temp], span },
                    cstring,
                )
            }
            ("to_i" | "to_f", TyKind::Int(_) | TyKind::Float(_) | TyKind::Enum(_) | TyKind::Rune) => {
                let target = if text == "to_i" { self.types.int() } else { self.types.f64() };
                self.no_args(args, name);
                self.convert(v.clone(), target, span)
            }
            ("to", _) => {
                let target = match args {
                    [arg] => match &arg.value.kind {
                        E::Const(_) | E::Type(_) | E::Member { .. } if is_type_like(&arg.value) => {
                            let texpr = expr_as_type(&arg.value);
                            let ctx = self.body_ctx();
                            self.resolve_type(&texpr, &ctx)
                        }
                        _ => {
                            self.report(
                                Diagnostic::error(codes::NOT_A_TYPE, "`to` takes the target type")
                                    .primary(arg.value.span, "expected a type like `F32`"),
                            );
                            return Some(ir::Expr::new(ExprKind::Zero, self.types.unknown()));
                        }
                    },
                    _ => {
                        self.report(
                            Diagnostic::error(codes::ARG_COUNT, "`to` takes exactly one type")
                                .primary(span, "write it like `x.to(F32)`"),
                        );
                        return Some(ir::Expr::new(ExprKind::Zero, self.types.unknown()));
                    }
                };
                self.convert(v.clone(), target, span)
            }
            _ => return None,
        };
        Some(result)
    }

    fn no_args(&mut self, args: &[ast::Arg], name: Ident) {
        if let Some(first) = args.first() {
            self.report(
                Diagnostic::error(codes::ARG_COUNT, format!("`{}` takes no arguments", name.as_str()))
                    .primary(first.value.span, "remove this argument"),
            );
        }
    }

    /// Converts between numeric types (and enums), reporting impossible
    /// conversions.
    pub fn convert(&mut self, v: ir::Expr, target: TyId, span: Span) -> ir::Expr {
        if v.ty == target {
            return v;
        }
        let from = self.types.base(v.ty);
        let to = self.types.base(target);
        let numeric_like = |t: &TyKind| matches!(t, TyKind::Int(_) | TyKind::Float(_) | TyKind::Enum(_) | TyKind::Rune);
        let fk = self.types.kind(from).clone();
        let tk = self.types.kind(to).clone();
        if matches!(fk, TyKind::Unknown) || matches!(tk, TyKind::Unknown) {
            return ir::Expr::new(v.kind, target);
        }
        if let Some(converted) = self.convert_view(v.clone(), target, span) {
            return converted;
        }
        let ok = (numeric_like(&fk)
            && numeric_like(&tk)
            && !(matches!(fk, TyKind::Float(_)) && matches!(tk, TyKind::Enum(_))))
            || from == to;
        if !ok {
            let (fs, ts) = (self.types.display(v.ty), self.types.display(target));
            self.report(
                Diagnostic::error(codes::INVALID_CONVERSION, format!("cannot convert `{fs}` to `{ts}`"))
                    .primary(span, "no conversion between these types")
                    .note("`.to(T)` converts between numbers, runes, enums and their distinct types, between pointer types, from arrays, slices and dynamic arrays to `[^]T`, and between `String`, `[]U8` and `CString` views"),
            );
            return ir::Expr::new(ExprKind::Zero, target);
        }
        if let (ExprKind::Int(i), TyKind::Float(_)) = (&v.kind, &tk) {
            return ir::Expr::new(ExprKind::Float(*i as f64), target);
        }
        ir::Expr::new(ExprKind::Cast { kind: ir::CastKind::Numeric, expr: Box::new(v) }, target)
    }

    /// The pointer shape of a type for `.to(T)`: `Some(nilable)` for
    /// `^T`, `[^]T`, `RawPtr`, `CString` and optional pointers.
    fn pointer_shape(&self, ty: TyId) -> Option<bool> {
        let base = self.types.base(ty);
        match self.types.kind(base) {
            TyKind::Pointer(_) | TyKind::MultiPointer(_) => Some(false),
            TyKind::RawPtr | TyKind::CString => Some(true),
            TyKind::Optional(_) if self.types.optional_is_pointer(base) => Some(true),
            _ => None,
        }
    }

    /// Converts between pointer types, pointers and addresses, and the
    /// byte views `String`, `[]U8` and `CString`. Returns `None` when the
    /// conversion is not one of these.
    fn convert_view(&mut self, v: ir::Expr, target: TyId, span: Span) -> Option<ir::Expr> {
        let from = self.types.kind(self.types.base(v.ty)).clone();
        let to = self.types.kind(self.types.base(target)).clone();
        let u8_ty = self.types.u8();
        let is_bytes = |k: &TyKind| matches!(k, TyKind::Slice(e) if *e == u8_ty);
        let builtin = |this: &mut Self, op: ir::Builtin, v: ir::Expr| {
            let v = this.stable(v);
            ir::Expr::new(ExprKind::Builtin { op, args: vec![v], span }, target)
        };
        if let TyKind::Array(e, _) | TyKind::Dynamic(e) = &from
            && (matches!(to, TyKind::String) && *e == u8_ty || matches!(to, TyKind::Slice(t) if t == *e))
        {
            let slice_ty = self.types.slice(*e);
            let slice = self.slice_of_container(v, slice_ty);
            return Some(if matches!(to, TyKind::String) {
                builtin(self, ir::Builtin::BytesString, slice)
            } else {
                slice
            });
        }
        let multi_elem = match &to {
            TyKind::MultiPointer(t) => Some(*t),
            TyKind::Optional(inner) => match self.types.kind(*inner) {
                TyKind::MultiPointer(t) => Some(*t),
                _ => None,
            },
            _ => None,
        };
        if let (TyKind::Array(e, _) | TyKind::Dynamic(e) | TyKind::Slice(e), Some(t)) = (&from, multi_elem)
            && *e == t
        {
            let slice = if matches!(from, TyKind::Slice(_)) {
                v
            } else {
                let slice_ty = self.types.slice(*e);
                self.slice_of_container(v, slice_ty)
            };
            return Some(builtin(self, ir::Builtin::SliceData, slice));
        }
        match (&from, &to) {
            (TyKind::String, k) if is_bytes(k) => return Some(builtin(self, ir::Builtin::StringBytes, v)),
            (k, TyKind::String) if is_bytes(k) => return Some(builtin(self, ir::Builtin::BytesString, v)),
            (TyKind::CString, TyKind::String) => return Some(builtin(self, ir::Builtin::CStringString, v)),
            (TyKind::String, TyKind::CString) => {
                self.report(
                    Diagnostic::error(codes::INVALID_CONVERSION, "a `String` is not NUL-terminated")
                        .primary(span, "cannot reinterpret it as a `CString`")
                        .help("`s.to_cstr` copies it into a NUL-terminated string on `context.temp_allocator`"),
                );
                return Some(ir::Expr::new(ExprKind::Zero, target));
            }
            _ => {}
        }
        let address = |k: &TyKind| matches!(k, TyKind::Int(crate::types::IntTy::Int | crate::types::IntTy::UInt));
        let from_ptr = self.pointer_shape(v.ty);
        let to_ptr = self.pointer_shape(target);
        let fits = match (from_ptr, to_ptr) {
            (Some(_), Some(_)) => true,
            (Some(_), None) => address(&to),
            (None, Some(_)) => address(&from),
            (None, None) => false,
        };
        if !fits {
            return None;
        }
        let source_nilable = from_ptr.unwrap_or(true);
        if source_nilable && to_ptr == Some(false) {
            let (fs, ts) = (self.types.display(v.ty), self.types.display(target));
            self.report(
                Diagnostic::error(
                    codes::INVALID_CONVERSION,
                    format!("{} `{fs}` may be nil, but `{ts}` cannot be", wid_diagnostics::a_or_an(&fs)),
                )
                .primary(span, format!("converting to `{ts}` would hide a nil pointer"))
                .help(format!("convert to `{ts}?` and unwrap it, for example with `guard p = x.to({ts}?) else … end`")),
            );
            return Some(ir::Expr::new(ExprKind::Zero, target));
        }
        let kind = if from_ptr.is_some() && to_ptr.is_some() { ir::CastKind::Pointer } else { ir::CastKind::Numeric };
        Some(ir::Expr::new(ExprKind::Cast { kind, expr: Box::new(v) }, target))
    }

    /// Reports a missing field or method with suggestions.
    pub fn no_member(&mut self, ty: TyId, name: Name, span: Span, is_ivar: bool) {
        if self.report_skipped_field(ty, name, span) {
            return;
        }
        // A macro that failed among the type's declarations may have been
        // meant to generate it; that failure is already reported. Fields
        // are never generated.
        if !is_ivar && self.members_incomplete(ty) {
            return;
        }
        let shown = self.types.display(ty);
        let mut candidates: Vec<&'static str> = Vec::new();
        if let TyKind::Struct(id) = self.types.kind(ty) {
            candidates.extend(self.types.struct_info(*id).fields.iter().map(|f| f.name.as_str()));
        }
        let decl = match self.types.kind(ty) {
            TyKind::Struct(id) => self.struct_decls.get(id).copied(),
            TyKind::Enum(id) => self.enum_decls.get(id).copied(),
            _ => None,
        };
        let method = decl.and_then(|d| self.members.get(&d)).is_some_and(|m| m.contains_key(&name));
        if is_ivar && method {
            self.report(
                Diagnostic::error(codes::NO_SUCH_MEMBER, format!("`@{name}` reads a field, but `{name}` is a method"))
                    .primary(span, format!("`{shown}` has no field `{name}`"))
                    .note("`@name` reads a field of `self`; methods are called by name")
                    .suggest_replace(
                        format!("call the method: `{name}`"),
                        span,
                        name.as_str(),
                        Applicability::MachineApplicable,
                    ),
            );
            return;
        }
        if !is_ivar
            && let Some(d) = decl
            && let Some(m) = self.members.get(&d)
        {
            candidates.extend(m.keys().map(|n| n.as_str()));
        }
        let what = if is_ivar { "field" } else { "field or method" };
        let shown_name = if is_ivar { format!("@{name}") } else { name.as_str().to_string() };
        let mut diag = Diagnostic::error(codes::NO_SUCH_MEMBER, format!("`{shown}` has no {what} `{shown_name}`"))
            .primary(span, format!("not found on `{shown}`"));
        let user_type = matches!(self.types.kind(ty), TyKind::Struct(_) | TyKind::Enum(_));
        if let Some((_, wid)) = SYNONYMS
            .iter()
            .find(|(other, target)| *other == name.as_str() && (!user_type || candidates.contains(target)))
        {
            diag = diag.help(format!("Wid calls this `{wid}`"));
        } else if let Some(best) = did_you_mean(name.as_str(), candidates.iter().copied()) {
            let replacement = if is_ivar { format!("@{best}") } else { best.to_string() };
            diag = diag.suggest_replace(
                format!("did you mean `{replacement}`?"),
                span,
                replacement,
                Applicability::MaybeIncorrect,
            );
        } else if !candidates.is_empty() {
            candidates.sort();
            diag = diag.note(format!("available: {}", candidates.join(", ")));
        }
        self.report(diag);
    }
}

impl Checker<'_> {
    /// Resolves an expression used as generic argument `index` of `decl`: a
    /// type, or a constant integer for value parameters.
    pub fn generic_arg_type(&mut self, decl: super::DeclId, index: usize, e: &ast::Expr) -> TyId {
        let ctx = self.body_ctx();
        if let Some(t) = self.value_generic_arg(decl, index, e, ctx.loc, &ctx.subst) {
            return t;
        }
        if let ast::ExprKind::Int(_) | ast::ExprKind::Unary { .. } | ast::ExprKind::Binary { .. } = e.kind
            && let Some(super::items::ConstValue::Int(v)) = self.fold_const_in(e, self.loc(), &ctx.subst)
        {
            return self.types.intern(TyKind::ConstValue(v));
        }
        self.type_arg(e)
    }
}

/// Returns true for expressions that read as a type: `F32`, `[]U8`, `c.int`.
pub(crate) fn is_type_like(e: &ast::Expr) -> bool {
    match &e.kind {
        E::Const(_) | E::Type(_) => true,
        E::Member { recv, safe: false, .. } => matches!(recv.kind, E::Ident(_) | E::Const(_)),
        _ => false,
    }
}

/// Reinterprets a constant-name expression as a type expression. In
/// `C.int?` the lexer reads `int?` as one name, like a predicate's; here its
/// `?` makes the type optional.
pub(crate) fn expr_as_type(e: &ast::Expr) -> ast::TypeExpr {
    match &e.kind {
        E::Member { recv, name, safe: false } => match recv.kind {
            E::Ident(pkg) | E::Const(pkg) => {
                let (name, optional) = match name.as_str().strip_suffix('?') {
                    Some(base) => {
                        (Ident { name: Name::new(base), span: Span { end: name.span.end - 1, ..name.span } }, true)
                    }
                    None => (*name, false),
                };
                let path = ast::TypeExpr {
                    kind: ast::TypeKind::Path {
                        segments: vec![Ident { name: pkg, span: recv.span }, name],
                        args: Vec::new(),
                    },
                    span: if optional { recv.span.to(name.span) } else { e.span },
                };
                if optional {
                    ast::TypeExpr { kind: ast::TypeKind::Optional(Box::new(path)), span: e.span }
                } else {
                    path
                }
            }
            _ => ast::TypeExpr { kind: ast::TypeKind::Error, span: e.span },
        },
        E::Type(t) => (**t).clone(),
        E::Const(n) => ast::TypeExpr {
            kind: ast::TypeKind::Path { segments: vec![Ident { name: *n, span: e.span }], args: Vec::new() },
            span: e.span,
        },
        _ => ast::TypeExpr { kind: ast::TypeKind::Error, span: e.span },
    }
}

//! Resolving type expressions to interned types.

use wid_diagnostics::{Diagnostic, Span, codes, did_you_mean};
use wid_syntax::Name;
use wid_syntax::ast::{self, TypeKind as T};

use super::items::ConstValue;
use super::{Checker, DeclKind, DeclLoc};
use crate::types::{FloatTy, IntTy, TyId, TyKind};

/// The context a type expression is resolved in.
#[derive(Clone, Debug)]
pub(crate) struct TyCtx {
    pub loc: DeclLoc,
    pub self_ty: Option<TyId>,
    /// Bindings of generic parameters visible here.
    pub subst: super::generics::Subst,
}

/// Names of the builtin types, for lookup and suggestions.
pub(crate) const PRIMITIVE_NAMES: &[&str] = &[
    "Int",
    "UInt",
    "I8",
    "I16",
    "I32",
    "I64",
    "U8",
    "U16",
    "U32",
    "U64",
    "F32",
    "F64",
    "Bool",
    "Rune",
    "String",
    "CString",
    "RawPtr",
    "TypeId",
    "Any",
    "Error",
    "Context",
    "Allocator",
    "AllocMode",
    "Location",
    "Logger",
    "Never",
    "Type",
    "Code",
    "Symbol",
];

/// Builtin types a package may declare its own type under: library types
/// that are predeclared, as opposed to the language's own types. Inside such
/// a package the name means the package's type.
pub(crate) const SHADOWABLE_NAMES: &[&str] =
    &["Context", "Allocator", "AllocMode", "Location", "Logger", "Type", "Code", "Symbol"];

/// Whether a package-level declaration can't use `name`, because it is one
/// of the language's builtin types (`Int`, `String`, `Bool`, …).
pub fn is_reserved_type_name(name: &str) -> bool {
    PRIMITIVE_NAMES.contains(&name) && !SHADOWABLE_NAMES.contains(&name)
}

/// The C types `core:c` names, as in Odin's `core:c`.
pub(crate) const C_TYPE_NAMES: &[&str] = &[
    "char",
    "schar",
    "uchar",
    "short",
    "ushort",
    "int",
    "uint",
    "long",
    "ulong",
    "longlong",
    "ulonglong",
    "float",
    "double",
    "bool",
    "size_t",
    "ssize_t",
    "ptrdiff_t",
    "intptr_t",
    "uintptr_t",
    "int8_t",
    "uint8_t",
    "int16_t",
    "uint16_t",
    "int32_t",
    "uint32_t",
    "int64_t",
    "uint64_t",
    "wchar_t",
];

impl<'a> Checker<'a> {
    /// The Wid type a C type name stands for on the 64-bit Unix targets
    /// (LP64): `long` is 64 bits and `char` is a byte.
    pub fn c_type_alias(&mut self, name: &str) -> Option<TyId> {
        let int = |t: IntTy| TyKind::Int(t);
        let kind = match name {
            "char" | "uchar" | "uint8_t" => int(IntTy::U8),
            "schar" | "int8_t" => int(IntTy::I8),
            "short" | "int16_t" => int(IntTy::I16),
            "ushort" | "uint16_t" => int(IntTy::U16),
            "int" | "int32_t" | "wchar_t" => int(IntTy::I32),
            "uint" | "uint32_t" => int(IntTy::U32),
            "long" | "longlong" | "int64_t" => int(IntTy::I64),
            "ulong" | "ulonglong" | "uint64_t" => int(IntTy::U64),
            "size_t" | "uintptr_t" => int(IntTy::UInt),
            "ssize_t" | "ptrdiff_t" | "intptr_t" => int(IntTy::Int),
            "float" => TyKind::Float(FloatTy::F32),
            "double" => TyKind::Float(FloatTy::F64),
            "bool" => TyKind::Bool,
            _ => return None,
        };
        Some(self.types.intern(kind))
    }

    /// Returns the builtin type with this name.
    pub fn primitive(&mut self, name: &str) -> Option<TyId> {
        let kind = match name {
            "Int" => TyKind::Int(IntTy::Int),
            "UInt" => TyKind::Int(IntTy::UInt),
            "I8" => TyKind::Int(IntTy::I8),
            "I16" => TyKind::Int(IntTy::I16),
            "I32" => TyKind::Int(IntTy::I32),
            "I64" => TyKind::Int(IntTy::I64),
            "U8" => TyKind::Int(IntTy::U8),
            "U16" => TyKind::Int(IntTy::U16),
            "U32" => TyKind::Int(IntTy::U32),
            "U64" => TyKind::Int(IntTy::U64),
            "F32" => TyKind::Float(FloatTy::F32),
            "F64" => TyKind::Float(FloatTy::F64),
            "Bool" => TyKind::Bool,
            "Rune" => TyKind::Rune,
            "String" => TyKind::String,
            "CString" => TyKind::CString,
            "RawPtr" => TyKind::RawPtr,
            "TypeId" => TyKind::TypeId,
            "Type" => TyKind::Type,
            "Code" => TyKind::Code,
            "Symbol" => TyKind::Symbol,
            "Any" => TyKind::Any,
            "Error" => TyKind::Error,
            "Never" => TyKind::Never,
            "Context" => return Some(self.types.context_ty),
            "Allocator" => return Some(self.types.allocator_ty),
            "AllocMode" => return Some(self.types.alloc_mode_ty),
            "Location" => return Some(self.types.location_ty),
            "Logger" => return Some(self.types.logger_ty),
            _ => return None,
        };
        Some(self.types.intern(kind))
    }

    /// Resolves a type expression, reporting errors and returning the
    /// unknown type on failure.
    pub fn resolve_type(&mut self, texpr: &ast::TypeExpr, ctx: &TyCtx) -> TyId {
        let scope = self.type_scope.replace((ctx.subst.clone(), ctx.self_ty));
        let ty = self.resolve_type_inner(texpr, ctx);
        self.type_scope = scope;
        self.fill_pending_types();
        ty
    }

    /// Resolves a type behind an indirection: structs and unions it names
    /// need not be complete yet.
    fn resolve_shallow(&mut self, texpr: &ast::TypeExpr, ctx: &TyCtx) -> TyId {
        self.shallow += 1;
        let ty = self.resolve_type_inner(texpr, ctx);
        self.shallow -= 1;
        ty
    }

    fn resolve_type_inner(&mut self, texpr: &ast::TypeExpr, ctx: &TyCtx) -> TyId {
        let ty = self.resolve_type_here(texpr, ctx);
        self.note_type(texpr.span, ty, crate::uses::TypedKind::Type);
        ty
    }

    fn resolve_type_here(&mut self, texpr: &ast::TypeExpr, ctx: &TyCtx) -> TyId {
        let pointee = std::mem::take(&mut self.pointee);
        match &texpr.kind {
            // A splice outside generated code is a parse error, reported.
            T::Error | T::Splice(_) => self.types.unknown(),
            T::Spliced(id) if (*id as usize) < self.types.len() => TyId(*id),
            T::Spliced(_) => self.types.unknown(),
            T::Path { segments, args } => {
                let ty = self.resolve_path_type(segments, args, texpr.span, ctx);
                if !pointee {
                    self.check_not_opaque(ty, texpr.span);
                }
                ty
            }
            T::Param(name) => match super::generics::lookup(&ctx.subst, name.name) {
                Some(t) => t,
                None => {
                    self.report(
                        Diagnostic::error(
                            codes::UNKNOWN_TYPE,
                            format!("`${}` is not a type parameter here", name.as_str()),
                        )
                        .primary(name.span, "type parameters are introduced in method parameters, like `a: $T`")
                        .help(format!("after it is introduced, refer to it as plain `{}`", name.as_str())),
                    );
                    self.types.unknown()
                }
            },
            T::Pointer(inner) => {
                self.pointee = true;
                let t = self.resolve_shallow(inner, ctx);
                self.pointee = false;
                self.types.pointer(t)
            }
            T::MultiPointer(inner) => {
                let t = self.resolve_shallow(inner, ctx);
                self.types.intern(TyKind::MultiPointer(t))
            }
            T::Slice(inner) => {
                let t = self.resolve_shallow(inner, ctx);
                self.types.slice(t)
            }
            T::Dynamic(inner) => {
                let t = self.resolve_shallow(inner, ctx);
                self.types.intern(TyKind::Dynamic(t))
            }
            T::Map(key, v) => {
                let k = self.resolve_shallow(key, ctx);
                self.note_map_key(k, key.span, self.resolving_instance(ctx.self_ty));
                let v = self.resolve_shallow(v, ctx);
                self.types.intern(TyKind::Map(k, v))
            }
            T::Array(len, elem) => {
                let elem = self.resolve_type_inner(elem, ctx);
                let bound = match &len.kind {
                    ast::ExprKind::Const(n) => {
                        super::generics::lookup(&ctx.subst, *n).and_then(|t| match self.types.kind(t) {
                            TyKind::ConstValue(v) => Some(ConstValue::Int(*v)),
                            _ => None,
                        })
                    }
                    _ => None,
                };
                if let ast::ExprKind::Const(n) = &len.kind
                    && let Some(t) = super::generics::lookup(&ctx.subst, *n)
                    && matches!(self.types.kind(t), TyKind::Unknown)
                {
                    // A value argument that was reported.
                    return self.types.unknown();
                }
                // `[N]T` or `[N + 1]T` with `N` a placeholder: each instance
                // resolves the type with its own `N`.
                if self.reads_placeholder(len, &ctx.subst) {
                    self.value_deferrals += 1;
                    return self.types.intern(TyKind::Array(elem, 0));
                }
                // A length that failed to parse (like `[$N]`) was reported.
                if super::runtime::holds_parse_error(len) {
                    return self.types.unknown();
                }
                let loc = self.virtual_file(len.span.file).map_or(ctx.loc, |v| v.loc);
                let errors = self.diags.error_count();
                match bound.or_else(|| self.eval_const_in(len, loc, &ctx.subst)) {
                    Some(ConstValue::Int(n)) if n >= 0 => {
                        let within = self.resolving_instance(ctx.self_ty);
                        let Ok(n) = u64::try_from(n) else {
                            self.report_long_array(elem, n, texpr.span, within);
                            return self.types.unknown();
                        };
                        let ty = self.types.intern(TyKind::Array(elem, n));
                        self.check_type_size(ty, texpr.span, within)
                    }
                    Some(ConstValue::Int(n)) => {
                        self.report(
                            Diagnostic::error(codes::TYPE_MISMATCH, "array length cannot be negative")
                                .primary(len.span, format!("this length is {n}")),
                        );
                        self.types.unknown()
                    }
                    // Evaluating it reported why, like a call without `comptime`.
                    None if self.diags.error_count() > errors => self.types.unknown(),
                    _ => {
                        let mut diag =
                            Diagnostic::error(codes::COMPTIME_ONLY, "array length must be a compile-time integer")
                                .primary(len.span, "not a constant integer");
                        let var = match len.kind {
                            ast::ExprKind::Ident(name) if !self.body.frames.is_empty() => {
                                self.find_var(name).map(|v| {
                                    v.read = true;
                                    (name, v.span)
                                })
                            }
                            _ => None,
                        };
                        if let Some((name, decl_span)) = var {
                            let upper = name.as_str().to_uppercase();
                            let elem_shown = self.types.display(elem);
                            diag = diag
                                .secondary(decl_span, format!("`{name}` is a variable, so its value is only known at run time"))
                                .help(format!(
                                    "declare a package-level constant like `{upper} = …` and write `[{upper}]`, or allocate at run time with `alloc([]{elem_shown}, {name})`"
                                ));
                        } else {
                            diag = diag.help("use a literal like `[4]F32` or a constant like `[MAX]F32`; for a runtime size use `[]T` or `[dynamic]T`");
                        }
                        self.report(diag);
                        self.types.unknown()
                    }
                }
            }
            T::Proc { params, ret, c_abi, variadic } => {
                let params = params.iter().map(|p| self.resolve_shallow(p, ctx)).collect();
                let ret = match ret {
                    Some(r) => self.resolve_shallow(r, ctx),
                    None => self.types.void(),
                };
                if *variadic && !*c_abi {
                    self.report(
                        Diagnostic::error(codes::TYPE_MISMATCH, "only C procs take variadic arguments")
                            .primary(texpr.span, "`...` needs the C calling convention")
                            .suggest(
                                "mark the proc type as C",
                                vec![wid_diagnostics::Edit {
                                    span: texpr.span.shrink_to_start(),
                                    replacement: "@[c] ".into(),
                                }],
                                wid_diagnostics::Applicability::MachineApplicable,
                            ),
                    );
                }
                self.types.intern(TyKind::Proc(crate::types::ProcSig {
                    params,
                    ret,
                    abi: if *c_abi { crate::types::Abi::C } else { crate::types::Abi::Wid },
                    variadic: *variadic && *c_abi,
                }))
            }
            T::Block { .. } => {
                self.report(
                    Diagnostic::error(codes::BLOCK_MISMATCH, "`block(…)` types only describe `&block` parameters")
                        .primary(texpr.span, "a block cannot be stored or passed as a value")
                        .help("for a storable callback, use a `proc(…)` type"),
                );
                self.types.unknown()
            }
            T::Optional(inner) => {
                let t = self.resolve_type(inner, ctx);
                if matches!(self.types.kind(t), TyKind::Optional(_)) {
                    self.report(
                        Diagnostic::error(codes::TYPE_MISMATCH, "optional of an optional")
                            .primary(texpr.span, "`T??` is the same as `T?`"),
                    );
                }
                if self.optional_union(inner, t, texpr.span, ctx) {
                    return t;
                }
                let ty = self.types.optional(t);
                self.check_type_size(ty, texpr.span, self.resolving_instance(ctx.self_ty))
            }
            T::Tuple(elems) => {
                let elems = elems.iter().map(|e| self.resolve_type(e, ctx)).collect();
                let ty = self.types.tuple(elems);
                self.check_type_size(ty, texpr.span, self.resolving_instance(ctx.self_ty))
            }
            T::Distinct(inner) => {
                let base = self.source_text(inner.span);
                self.report(
                    Diagnostic::error(codes::NOT_A_TYPE, "`distinct` types need a name")
                        .primary(texpr.span, "a `distinct` type is declared once, as a constant")
                        .note("each `distinct` declaration creates a new type, so an unnamed one could never match another")
                        .help(format!("declare it at package level, like `Meters = distinct {base}`, then use `Meters`")),
                );
                self.types.unknown()
            }
            T::Matrix { rows, cols, elem } => self.resolve_matrix(rows, cols, elem, ctx),
        }
    }

    /// Reports `U?` written for a union `U` (E0301), which is nil-able
    /// already, with the fix that removes the `?`. A type parameter bound
    /// to a union (`T?`) is fine: the type is written for every `T`.
    fn optional_union(&mut self, inner: &ast::TypeExpr, t: TyId, span: Span, ctx: &TyCtx) -> bool {
        if !matches!(self.types.kind(t), TyKind::Union(_)) {
            return false;
        }
        if let T::Path { segments, .. } = &inner.kind
            && let [only] = segments.as_slice()
            && (only.as_str() == "Self" || super::generics::lookup(&ctx.subst, only.name).is_some())
        {
            return false;
        }
        if !matches!(inner.kind, T::Path { .. }) {
            return false;
        }
        let shown = self.types.display(t);
        self.report(
            Diagnostic::error(codes::TYPE_MISMATCH, format!("`{shown}` is a union, which can be nil already"))
                .primary(span, format!("`{shown}?` adds nothing to `{shown}`"))
                .note("a union's zero value is `nil`: it holds one of its variants or nothing, and works as an error value")
                .suggest(
                    "remove the `?`",
                    vec![wid_diagnostics::Edit { span: Span::new(span.file, inner.span.end, span.end), replacement: String::new() }],
                    wid_diagnostics::Applicability::MachineApplicable,
                ),
        );
        true
    }

    fn resolve_path_type(
        &mut self,
        segments: &[ast::Ident],
        args: &[ast::GenericArg],
        span: Span,
        ctx: &TyCtx,
    ) -> TyId {
        // A type a macro's own code names resolves where the macro is
        // defined; one spliced from the call site, where it was called.
        let at_macro;
        let ctx = match segments.first().and_then(|s| self.virtual_file(s.span.file)) {
            Some(v) => {
                at_macro = TyCtx { loc: v.loc, ..ctx.clone() };
                &at_macro
            }
            None => ctx,
        };
        let (pkg, name) = match segments {
            [only] => (ctx.loc.pkg, *only),
            [pkg, name] => match self.lookup_import(ctx.loc, pkg.name) {
                Some(p) => (p, *name),
                None => {
                    if !self.import_failed(ctx.loc, pkg.name) {
                        self.undefined(pkg.name, pkg.span, Vec::new(), "package");
                    }
                    return self.types.unknown();
                }
            },
            _ => {
                self.report(
                    Diagnostic::error(codes::UNKNOWN_TYPE, "type paths have at most one `.`")
                        .primary(span, "write `package.Type`"),
                );
                return self.types.unknown();
            }
        };
        if let [qualifier, _] = segments {
            self.note_package(qualifier.span, pkg);
        }
        let text = name.name.as_str();
        if segments.len() == 2 && self.input.packages[pkg.0 as usize].path == "core:c" {
            if let Some(t) = self.c_type_alias(text) {
                return t;
            }
            let mut diag = Diagnostic::error(codes::UNKNOWN_TYPE, format!("unknown C type `{text}`"))
                .primary(name.span, "`core:c` has no type with this name");
            if let Some(best) = did_you_mean(text, C_TYPE_NAMES.iter().copied()) {
                diag = diag.suggest_replace(
                    format!("did you mean `{best}`?"),
                    name.span,
                    best,
                    wid_diagnostics::Applicability::MaybeIncorrect,
                );
            } else {
                diag = diag.note(format!("C types: {}", C_TYPE_NAMES.join(", ")));
            }
            self.report(diag);
            return self.types.unknown();
        }
        if segments.len() == 1
            && args.is_empty()
            && let Some(t) = super::generics::lookup(&ctx.subst, name.name)
        {
            // A value parameter: bound to its value in an instance, or a
            // placeholder of the struct's own value parameter.
            let value = match self.types.kind(t) {
                TyKind::ConstValue(_) => true,
                TyKind::Param(_) => self.value_param_of(ctx, name.name).is_some(),
                _ => false,
            };
            if value {
                self.value_param_as_type(&name, ctx);
                return self.types.unknown();
            }
            return t;
        }
        if segments.len() == 1 {
            if text == "Self" {
                if let Some(s) = ctx.self_ty {
                    return s;
                }
                self.report(
                    Diagnostic::error(codes::SELF_OUTSIDE_METHOD, "`Self` is only available inside a type")
                        .primary(name.span, "not inside a struct, enum, union or extend"),
                );
                return self.types.unknown();
            }
            if let Some(t) = self.primitive(text)
                && !(SHADOWABLE_NAMES.contains(&text) && self.lookup_pkg(pkg, name.name).is_some())
            {
                self.note_builtin(name.span, text);
                return t;
            }
        }
        if args.is_empty()
            && let Some(t) = self.c_type_override(pkg, name.name)
        {
            return t;
        }
        let found = self
            .lookup_pkg(pkg, name.name)
            .or_else(|| if segments.len() == 1 { self.lookup_prelude(name.name) } else { None });
        if let Some(decl) = found {
            self.check_visible_from(decl, ctx.loc.pkg, name.span);
            self.note_ref(name.span, decl, crate::uses::RefKind::Type);
            if !args.is_empty() {
                let deferrals = self.value_deferrals;
                let arg_tys: Vec<TyId> = args
                    .iter()
                    .enumerate()
                    .map(|(i, a)| match a {
                        // A name for a value parameter may be a constant.
                        ast::GenericArg::Type(t) => match path_as_expr(t)
                            .and_then(|e| self.value_generic_arg(decl, i, &e, ctx.loc, &ctx.subst))
                        {
                            Some(ty) => ty,
                            None => self.resolve_type(t, ctx),
                        },
                        ast::GenericArg::Expr(e) => match self.value_generic_arg(decl, i, e, ctx.loc, &ctx.subst) {
                            Some(ty) => ty,
                            None => self.generic_expr_arg(e, ctx),
                        },
                    })
                    .collect();
                // `Pool(T, N + 1)` with `N` a placeholder: each instance
                // resolves the type with its own `N`.
                if self.value_deferrals > deferrals {
                    return self.types.unknown();
                }
                let arg_spans: Vec<Span> = args
                    .iter()
                    .map(|a| match a {
                        ast::GenericArg::Type(t) => t.span,
                        ast::GenericArg::Expr(e) => e.span,
                    })
                    .collect();
                let generic = matches!(self.decls[decl.0 as usize].kind, DeclKind::Struct(s) if !s.generics.is_empty())
                    || matches!(self.decls[decl.0 as usize].kind, DeclKind::Union(u) if !u.generics.is_empty());
                if generic && !self.check_generic_args(decl, &arg_tys, &arg_spans, span) {
                    return self.types.unknown();
                }
                return match self.decls[decl.0 as usize].kind {
                    DeclKind::Struct(s) if !s.generics.is_empty() => self.struct_instance(decl, arg_tys, span),
                    DeclKind::Union(u) if !u.generics.is_empty() => self.union_instance(decl, arg_tys, span),
                    _ => {
                        self.report(
                            Diagnostic::error(codes::GENERIC_ARGS, format!("`{text}` takes no type arguments"))
                                .primary(span, "remove the arguments"),
                        );
                        self.types.unknown()
                    }
                };
            }
            if let DeclKind::Union(u) = self.decls[decl.0 as usize].kind
                && !u.generics.is_empty()
            {
                let names: Vec<&str> = u.generics.iter().map(|g| g.name.as_str()).collect();
                self.report(
                    Diagnostic::error(codes::GENERIC_ARGS, format!("`{text}` needs type arguments"))
                        .primary(name.span, format!("write it like `{text}({})`", names.join(", "))),
                );
                return self.types.unknown();
            }
            if matches!(self.decls[decl.0 as usize].kind, DeclKind::Struct(s) if !s.generics.is_empty()) {
                let DeclKind::Struct(s) = self.decls[decl.0 as usize].kind else { unreachable!() };
                let names: Vec<&str> = s.generics.iter().map(|g| g.name.as_str()).collect();
                self.report(
                    Diagnostic::error(codes::GENERIC_ARGS, format!("`{text}` needs type arguments"))
                        .primary(name.span, format!("write it like `{text}({})`", names.join(", "))),
                );
                return self.types.unknown();
            }
            return self.decl_as_type(decl, name.span);
        }
        if self.pkg_incomplete(pkg)
            || ((segments.len() == 2 || self.merged_cimports.contains_key(&pkg))
                && self.report_not_imported(pkg, name.name, name.span))
        {
            return self.types.unknown();
        }
        if segments.len() == 1
            && self.body.frames.last().is_some_and(|f| f.loc.pkg == pkg)
            && let Some(var) = self.find_var_at(name.name, name.span)
        {
            var.read = true;
            let (var_ty, var_span) = (var.ty, var.span);
            let shown = self.types.display(var_ty);
            let mut diag = Diagnostic::error(codes::NOT_A_TYPE, format!("`{text}` is a variable, not a type"))
                .primary(name.span, "expected a type here")
                .secondary(var_span, format!("`{text}` is declared here"));
            if !matches!(self.types.kind(var_ty), TyKind::Unknown) {
                diag = diag.suggest_replace(
                    format!("to declare another `{shown}`, write its type"),
                    name.span,
                    shown,
                    wid_diagnostics::Applicability::MaybeIncorrect,
                );
            }
            self.report(diag);
            return self.types.unknown();
        }
        let mut candidates: Vec<&'static str> = PRIMITIVE_NAMES.to_vec();
        candidates
            .extend(self.package_names(pkg).into_iter().filter(|n| n.chars().next().is_some_and(char::is_uppercase)));
        let mut diag = Diagnostic::error(codes::UNKNOWN_TYPE, format!("unknown type `{text}`"))
            .primary(name.span, "no type with this name is in scope");
        if let Some(best) = did_you_mean(text, candidates) {
            diag = diag.suggest_replace(
                format!("a similar type exists: `{best}`"),
                name.span,
                best,
                wid_diagnostics::Applicability::MaybeIncorrect,
            );
        } else if let Some(hint) = common_type_hint(text) {
            diag = diag.help(hint);
        }
        self.report(diag);
        self.types.unknown()
    }

    /// The value parameter `name` of the generic struct whose fields or
    /// methods are being resolved, like `$N: Int` in `struct Pool($T, $N: Int)`.
    pub(super) fn value_param_of(&self, ctx: &TyCtx, name: Name) -> Option<&'a ast::GenericParam> {
        let decl = match self.types.kind(ctx.self_ty?) {
            TyKind::Struct(id) => *self.struct_decls.get(id)?,
            _ => return None,
        };
        match self.decls[decl.0 as usize].kind {
            DeclKind::Struct(s) => s.generics.iter().find(|g| g.name.name == name && g.ty.is_some()),
            _ => None,
        }
    }

    /// Reports a generic value parameter, like `N` in a method of
    /// `Pool(Int, 4)`, written where a type is expected (`y: N`,
    /// `size_of(N)`, `-> N`), with a fix that writes its declared type.
    fn value_param_as_type(&mut self, ident: &ast::Ident, ctx: &TyCtx) {
        let name = ident.name;
        let mut diag = Diagnostic::error(codes::NOT_A_TYPE, format!("`{name}` is a value, not a type"))
            .primary(ident.span, "expected a type here");
        if let Some(g) = self.value_param_of(ctx, name)
            && let Some(t) = &g.ty
        {
            let ty_text = self.source_text(t.span);
            diag = diag
                .secondary(g.span, format!("`{name}` is a value parameter, a constant `{ty_text}`"))
                .suggest_replace(
                    format!("for the type of `{name}`, write `{ty_text}`"),
                    ident.span,
                    ty_text,
                    wid_diagnostics::Applicability::MaybeIncorrect,
                );
        }
        self.report(diag.note(format!(
            "a value parameter is a constant: it can be an array length, like `[{name}]T`, or a value, like `{name}.times`"
        )));
    }

    /// A generic argument written as an expression for a parameter that
    /// takes a type, or of a declaration that isn't generic: its constant
    /// integer value, which the caller reports as misplaced.
    fn generic_expr_arg(&mut self, e: &ast::Expr, ctx: &TyCtx) -> TyId {
        let errors = self.diags.error_count();
        match self.eval_const_in(e, ctx.loc, &ctx.subst) {
            Some(ConstValue::Int(v)) => self.types.intern(TyKind::ConstValue(v)),
            // Evaluating it reported why, like a call without `comptime`.
            None if self.diags.error_count() > errors => self.types.unknown(),
            _ => {
                self.report(
                    Diagnostic::error(codes::GENERIC_ARGS, "a generic value argument must be a constant integer")
                        .primary(e.span, "not a constant"),
                );
                self.types.unknown()
            }
        }
    }

    /// Returns the type a declaration names, reporting when it isn't a type.
    pub fn decl_as_type(&mut self, decl: super::DeclId, span: Span) -> TyId {
        self.note_ref(span, decl, crate::uses::RefKind::Type);
        let is_record = matches!(self.decls[decl.0 as usize].kind, DeclKind::Struct(_) | DeclKind::Union(_));
        if !is_record && let Some(&t) = self.decl_types.get(&decl) {
            return t;
        }
        let d = self.decls[decl.0 as usize].clone();
        match d.kind {
            DeclKind::Struct(_) => self.struct_type(decl),
            DeclKind::Enum(_) => self.enum_type(decl),
            DeclKind::Union(_) => self.union_type(decl),
            DeclKind::Const(c) => {
                // Parentheses group a type: `X = (Int)`.
                let mut value = &c.value;
                while let ast::ExprKind::Paren(inner) = &value.kind {
                    value = inner;
                }
                if let ast::ExprKind::Type(t) = &value.kind {
                    let ctx = TyCtx { loc: d.loc, self_ty: None, subst: Default::default() };
                    if let ast::TypeKind::Distinct(inner) = &t.kind {
                        let base = self.resolve_type(inner, &ctx);
                        let prefix = self.pkg_prefix(d.loc.pkg);
                        let ty = self.types.new_distinct(crate::types::DistinctInfo {
                            name: d.name.as_str().to_string(),
                            c_name: format!("{prefix}__{}", super::mangle_ident(d.name.as_str())),
                            base,
                        });
                        self.decl_types.insert(decl, ty);
                        return ty;
                    }
                    self.pointee = true;
                    let ty = self.resolve_type(t, &ctx);
                    self.pointee = false;
                    self.decl_types.insert(decl, ty);
                    return ty;
                }
                if let ast::ExprKind::Member { recv, safe: false, .. } = &value.kind
                    && matches!(recv.kind, ast::ExprKind::Ident(p) | ast::ExprKind::Const(p) if self.lookup_import(d.loc, p).is_some())
                {
                    let ctx = TyCtx { loc: d.loc, self_ty: None, subst: Default::default() };
                    let texpr = super::members::expr_as_type(value);
                    self.pointee = true;
                    let ty = self.resolve_type(&texpr, &ctx);
                    self.pointee = false;
                    self.decl_types.insert(decl, ty);
                    return ty;
                }
                if let ast::ExprKind::Const(name) = value.kind {
                    // `X = Foo` with `Foo` undefined: resolving the constant
                    // reports `Foo` (E0201) and poisons `X`, so using `X` as a
                    // type says nothing more. A constant without a value
                    // reports why, now or when it was first resolved.
                    if !self.is_type_alias_value(value, d.loc, 0)
                        && !d.item.has_attr("extern")
                        && self.const_value(decl).is_none()
                    {
                        let ty = self.types.unknown();
                        self.decl_types.insert(decl, ty);
                        return ty;
                    }
                    let ctx = TyCtx { loc: d.loc, self_ty: None, subst: Default::default() };
                    let texpr = ast::TypeExpr {
                        kind: ast::TypeKind::Path {
                            segments: vec![ast::Ident { name, span: value.span }],
                            args: Vec::new(),
                        },
                        span: value.span,
                    };
                    let ty = self.resolve_type(&texpr, &ctx);
                    self.decl_types.insert(decl, ty);
                    return ty;
                }
                self.not_a_type(d.name, span, "a constant");
                self.types.unknown()
            }
            ref other => {
                let what = other.a_describe();
                self.not_a_type(d.name, span, &what);
                self.types.unknown()
            }
        }
    }

    fn not_a_type(&mut self, name: Name, span: Span, what: &str) {
        self.report(
            Diagnostic::error(codes::NOT_A_TYPE, format!("`{name}` is {what}, not a type"))
                .primary(span, "expected a type here")
                .help("types are primitives like `Int`, structs, enums, unions, and constants like `Meters = distinct F64`"),
        );
    }
}

/// A generic argument parsed as a type name (`N`, `geo.N`) as the
/// expression it also reads as, for a value parameter.
fn path_as_expr(t: &ast::TypeExpr) -> Option<ast::Expr> {
    let T::Path { segments, args } = &t.kind else { return None };
    if !args.is_empty() {
        return None;
    }
    let kind = match segments.as_slice() {
        [name] => ast::ExprKind::Const(name.name),
        [pkg, name] => {
            let recv = if pkg.as_str().starts_with(|c: char| c.is_ascii_uppercase()) {
                ast::ExprKind::Const(pkg.name)
            } else {
                ast::ExprKind::Ident(pkg.name)
            };
            ast::ExprKind::Member { recv: Box::new(ast::Expr { kind: recv, span: pkg.span }), name: *name, safe: false }
        }
        _ => return None,
    };
    Some(ast::Expr { kind, span: t.span })
}

/// Hints for type names people often bring from other languages.
fn common_type_hint(name: &str) -> Option<&'static str> {
    Some(match name {
        "Integer" | "Fixnum" | "int" | "i64" | "isize" => "Wid's default integer type is `Int`",
        "Float" | "float" | "f32" => "Wid has `F32` and `F64`",
        "Double" | "double" | "f64" => "a 64-bit float is `F64`",
        "Boolean" | "bool" => "booleans are `Bool`",
        "Str" | "string" | "str" => "strings are `String`",
        "Array" | "Vec" | "List" => {
            "use `[N]T` for fixed arrays, `[]T` for slices and `[dynamic]T` for growable arrays"
        }
        "Hash" | "HashMap" | "Dict" => "use `map[K]V`",
        "Void" | "Unit" | "Nil" => "a method that returns nothing simply has no `-> Type`",
        _ => return None,
    })
}

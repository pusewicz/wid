//! `type_info(T)` and `type_info(x)`: a pointer to the read-only table that
//! describes a type at run time (and, inside `comptime` code, to the same
//! table built by the interpreter).

use wid_diagnostics::{Applicability, Diagnostic, Span, codes};
use wid_syntax::Name;
use wid_syntax::ast::{self, ExprKind as E};

use super::members::expr_as_type;
use super::{Checker, DeclKind};
use crate::ir::{self, Builtin, ExprKind};
use crate::type_info::{self, Undescribable};
use crate::types::{IntTy, TyId, TyKind};

impl<'a> Checker<'a> {
    /// Lowers `type_info(T)` or `type_info(x)`. Only the static type of `x`
    /// matters, so `x` is checked but never evaluated.
    pub fn builtin_type_info(&mut self, args: &[ast::Arg], span: Span) -> ir::Expr {
        let unknown = self.types.unknown();
        let fail = ir::Expr::new(ExprKind::Zero, unknown);
        let [arg] = args else {
            self.report(
                Diagnostic::error(codes::ARG_COUNT, "`type_info` takes one type or value")
                    .primary(span, "like `type_info(Vec2)` or `type_info(x)`"),
            );
            for a in args {
                self.type_info_operand(&a.value);
            }
            return fail;
        };
        if let Some(n) = arg.name {
            let name_part = Span { end: arg.value.span.start, ..n.span };
            self.report(
                Diagnostic::error(codes::BAD_NAMED_ARG, "`type_info` takes no named arguments")
                    .primary(n.span, format!("`type_info` has no parameter called `{}`", n.as_str()))
                    .suggest_replace(
                        "pass the type or value by position",
                        name_part,
                        "",
                        Applicability::MaybeIncorrect,
                    ),
            );
        }
        let (ty, is_type) = self.type_info_operand(&arg.value);
        if matches!(self.types.kind(ty), TyKind::Unknown) {
            return fail;
        }
        // Enum tables point at their backing integer type, which the code
        // generator looks up without interning it.
        for i in IntTy::ALL {
            self.types.intern(TyKind::Int(i));
        }
        match type_info::check(&self.types, &self.errors, ty) {
            Ok(()) => {}
            Err(Undescribable::Poisoned) => return fail,
            Err(Undescribable::NoValues) => {
                self.report_no_values(ty, is_type, arg.value.span);
                return fail;
            }
            Err(Undescribable::CompileTimeOnly { culprit, path }) => {
                self.report_compile_time_only(ty, culprit, &path, is_type, arg.value.span);
                return fail;
            }
        }
        let Some(info) = self.prelude_struct("TypeInfo") else {
            self.report(
                Diagnostic::error(codes::UNDEFINED_NAME, "`TypeInfo` is missing from the prelude")
                    .primary(span, "`type_info` returns a `^TypeInfo`")
                    .help("`core:builtin` declares `TypeInfo`; check that the `core` collection is complete"),
            );
            return fail;
        };
        let ptr = self.types.pointer(info);
        let described = ir::Expr::new(ExprKind::Zero, ty);
        ir::Expr::new(ExprKind::Builtin { op: Builtin::TypeInfo, args: vec![described], span }, ptr)
    }

    /// The type `type_info` describes: the type an argument names, or the
    /// static type of a value. The second part is true for a type.
    fn type_info_operand(&mut self, e: &ast::Expr) -> (TyId, bool) {
        if let Some(t) = self.named_type(e) {
            return (t, true);
        }
        // The value is never computed: drop the statements that would. The
        // names a macro's code declares there are the operand's own.
        let operand = super::macros::Operand { kind: super::macros::OperandKind::TypeInfo, span: e.span };
        let (_, v) = self.lower_operand(operand, |this| this.expr(e, None));
        let ty = match self.types.kind(v.ty) {
            TyKind::Never => v.ty,
            _ => self.value_type(v.ty, e.span),
        };
        (ty, false)
    }

    /// The type an expression names, when it names one rather than a value:
    /// `Ball`, `[]Int`, `Int?`, `Pool(Ball, 64)`, `geo.Pool(Ball, 64)`,
    /// `C.int`, `C.int?`, `rl.Color`, a type alias or a generic parameter.
    pub(super) fn named_type(&mut self, e: &ast::Expr) -> Option<TyId> {
        let names_type = match &e.kind {
            E::Type(_) => true,
            E::Const(n) => {
                let frame = self.frame();
                if (n.as_str() == "Self" && frame.self_ty.is_some())
                    || super::generics::lookup(&frame.subst, *n)
                        .is_some_and(|t| !matches!(self.types.kind(t), TyKind::ConstValue(_)))
                {
                    true
                } else {
                    let loc = self.loc();
                    match self.lookup_pkg(loc.pkg, *n).or_else(|| self.lookup_prelude(*n)) {
                        Some(decl) => {
                            let d = &self.decls[decl.0 as usize];
                            match d.kind {
                                DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Union(_) => true,
                                DeclKind::Const(c) => self.is_type_alias_value(&c.value, d.loc, 0),
                                _ => false,
                            }
                        }
                        None => super::ty::PRIMITIVE_NAMES.contains(&n.as_str()),
                    }
                }
            }
            E::Call(call) => return self.generic_instance(call, e.span),
            E::Member { recv, name, safe: false } => {
                let pkg = match recv.kind {
                    E::Ident(p) if self.find_var_at(p, recv.span).is_none() => {
                        self.lookup_import(self.loc_at(recv.span), p)
                    }
                    E::Const(p) => self.lookup_import(self.loc(), p),
                    _ => None,
                };
                match pkg {
                    Some(p) if self.input.packages[p.0 as usize].path == "core:c" => true,
                    Some(p) => match self.lookup_pkg(p, name.name) {
                        Some(decl) => {
                            let d = &self.decls[decl.0 as usize];
                            match d.kind {
                                DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Union(_) => true,
                                DeclKind::Const(c) => self.is_type_alias_value(&c.value, d.loc, 0),
                                _ => false,
                            }
                        }
                        None => false,
                    },
                    None => false,
                }
            }
            _ => false,
        };
        if !names_type {
            return None;
        }
        let texpr = expr_as_type(e);
        let ctx = self.body_ctx();
        Some(self.resolve_type(&texpr, &ctx))
    }

    /// The instance a call like `Pool(Ball, 64)` or `geo.Pool(Ball, 64)`
    /// names, or `None` when the callee is not a generic struct or union.
    /// Wrong type arguments are reported and give the unknown type.
    pub(super) fn generic_instance(&mut self, call: &ast::Call, span: Span) -> Option<TyId> {
        if call.block.is_some() {
            return None;
        }
        let loc = self.loc();
        let (decl, name) = match &call.callee {
            ast::Callee::Name(n) => {
                let loc = self.loc_at(n.span);
                (self.lookup_pkg(loc.pkg, n.name).or_else(|| self.lookup_prelude(n.name))?, *n)
            }
            ast::Callee::Method { recv, name, safe: false } => {
                let pkg = match recv.kind {
                    E::Ident(p) if self.find_var_at(p, recv.span).is_none() => {
                        self.lookup_import(self.loc_at(recv.span), p)
                    }
                    E::Const(p) => self.lookup_import(loc, p),
                    _ => None,
                }?;
                (self.lookup_pkg(pkg, name.name)?, *name)
            }
            ast::Callee::Method { .. } | ast::Callee::IVar(_) => return None,
        };
        let union = match self.decls[decl.0 as usize].kind {
            DeclKind::Struct(s) if !s.generics.is_empty() => false,
            DeclKind::Union(u) if !u.generics.is_empty() => true,
            _ => return None,
        };
        self.check_visible(decl, name.span);
        let args: Vec<TyId> =
            call.args.iter().enumerate().map(|(i, a)| self.generic_arg_type(decl, i, &a.value)).collect();
        let spans: Vec<Span> = call.args.iter().map(|a| a.value.span).collect();
        if !self.check_generic_args(decl, &args, &spans, span) {
            return Some(self.types.unknown());
        }
        Some(if union { self.union_instance(decl, args, span) } else { self.struct_instance(decl, args, span) })
    }

    // ----- the tables are read-only -------------------------------------------------

    /// Whether a type is one of the records `type_info` tables are made
    /// of: the prelude's `TypeInfo`, `TypeInfoField` or `TypeInfoMember`.
    fn is_table_record(&self, ty: TyId) -> bool {
        let TyKind::Struct(id) = self.types.kind(ty) else { return false };
        let Some(&decl) = self.struct_decls.get(id) else { return false };
        ["TypeInfo", "TypeInfoField", "TypeInfoMember"].iter().any(|n| self.lookup_prelude(Name::new(n)) == Some(decl))
    }

    /// Whether storage reached by `e` may be in a `type_info` table: it is
    /// reached through a pointer to (or a slice of) a table record, or
    /// through a pointer or slice read out of a table, which points into
    /// the tables too.
    fn in_type_table(&self, e: &ir::Expr) -> bool {
        match &e.kind {
            ExprKind::Field { base, .. } | ExprKind::OptGet(base) | ExprKind::UnionGet { value: base, .. } => {
                self.in_type_table(base)
            }
            ExprKind::Index { base, .. } => match self.types.kind(self.types.base(base.ty)) {
                TyKind::Array(..) | TyKind::Matrix(..) => self.in_type_table(base),
                TyKind::Slice(elem) | TyKind::MultiPointer(elem) | TyKind::Dynamic(elem) => {
                    self.is_table_record(*elem) || self.in_type_table(base)
                }
                _ => false,
            },
            ExprKind::Deref(ptr) => {
                let pointee = match self.types.kind(ptr.ty) {
                    TyKind::Pointer(t) => Some(*t),
                    _ => None,
                };
                pointee.is_some_and(|t| self.is_table_record(t)) || self.in_type_table(ptr)
            }
            _ => false,
        }
    }

    /// Reports an assignment whose target `place`, written at `span`, is
    /// in a `type_info` table (E0309), and returns whether it was.
    pub(super) fn report_type_table_write(&mut self, place: &ir::Expr, span: Span) -> bool {
        if !self.in_type_table(place) {
            return false;
        }
        let text = self.source_text(span);
        let word = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_alphanumeric() || c == '_');
        let help = if word(&text) {
            // A `for &f in t.fields` variable.
            format!("`{text}` points into the table: loop over copies, `for {text} in …`, and change the copy")
        } else {
            let name = match text.rsplit_once('.') {
                Some((_, field)) if word(field) => field.to_string(),
                _ => "copy".to_string(),
            };
            format!("copy the value into a variable and change the copy, like `{name} = {text}`")
        };
        self.report(
            Diagnostic::error(codes::NOT_ASSIGNABLE, "`type_info` tables are read-only")
                .primary(span, "this is part of a `type_info` table")
                .note("`type_info` points at a table the compiler emits as constant data, shared by every use of the type")
                .help(help),
        );
        true
    }

    /// Reports `&` of a place in a `type_info` table (E0309) when the
    /// pointer would allow writes the checker can't see: a pointer to a
    /// table record is fine, since writes through it are reported.
    pub(super) fn report_type_table_address(&mut self, place: &ir::Expr, span: Span) -> bool {
        if self.is_table_record(place.ty) || !self.in_type_table(place) {
            return false;
        }
        let (label, help) = match self.source_text(span).starts_with('&') {
            true => (
                "this pointer would let code change a `type_info` table",
                "read the value instead, or copy it into a variable and take the variable's address",
            ),
            false => (
                "binding these elements by reference would let code change a `type_info` table",
                "drop the `&` to loop over copies of the elements",
            ),
        };
        self.report(
            Diagnostic::error(codes::NOT_ASSIGNABLE, "`type_info` tables are read-only")
                .primary(span, label)
                .note("`type_info` points at a table the compiler emits as constant data, shared by every use of the type")
                .help(help),
        );
        true
    }

    /// `type_info(Never)`, or of code that never finishes.
    fn report_no_values(&mut self, ty: TyId, is_type: bool, span: Span) {
        let shown = self.types.display(ty);
        let label = if is_type {
            format!("`{shown}` is the type of code that never finishes")
        } else {
            "this never finishes, so it never produces a value".to_string()
        };
        self.report(
            Diagnostic::error(
                codes::NOT_A_VALUE,
                format!("`type_info` has nothing to describe: no value has type `{shown}`"),
            )
            .primary(span, label)
            .note("`type_info` describes the types of values the program holds")
            .help("remove the call, or pass a type that has values, like `type_info(Int)`"),
        );
    }

    /// `type_info` of `Type`, or of a type whose table would reach one.
    fn report_compile_time_only(&mut self, ty: TyId, culprit: TyId, path: &[String], is_type: bool, span: Span) {
        let shown = self.types.display(ty);
        let culprit_shown = self.types.display(culprit);
        let diag = if path.is_empty() {
            let (label, help) = if is_type {
                (
                    format!("`{shown}` values exist only while compiling"),
                    "describe a run-time type instead, like `type_info(Ball)` or `type_info(x)`",
                )
            } else {
                (
                    format!("this `{shown}` value exists only while compiling"),
                    "to describe the type it holds, read it while compiling with `t.name`, `t.size`, `t.align` and `t.fields`, or pass the type itself, like `type_info(Ball)`",
                )
            };
            Diagnostic::error(
                codes::COMPTIME_AT_RUNTIME,
                format!("`type_info` can't describe `{shown}`, which exists only while compiling"),
            )
            .primary(span, label)
            .note("`type_info` returns a table the program keeps while it runs")
            .help(help)
        } else {
            Diagnostic::error(
                codes::COMPTIME_AT_RUNTIME,
                format!("`type_info` can't describe `{shown}`: it holds a value of type `{culprit_shown}`"),
            )
            .primary(span, format!("`{shown}` {}", path.join(", which ")))
            .note(format!(
                "`{culprit_shown}` values exist only while compiling, but `type_info` returns a table the program keeps while it runs"
            ))
            .help(format!("read `{shown}` in `comptime` code instead, where its `{culprit_shown}` values exist"))
        };
        self.report(diag);
    }
}

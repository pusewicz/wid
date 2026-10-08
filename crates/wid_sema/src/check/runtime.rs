//! The implicit context, allocation builtins and string building.

use wid_diagnostics::{Applicability, Diagnostic, Span, codes};
use wid_syntax::ast::{self, ExprKind as E};

use super::Checker;
use crate::ir::{self, Builtin, ExprKind, Stmt};
use crate::types::{TyId, TyKind};

impl<'a> Checker<'a> {
    /// Returns `context` as a place of type `Context`.
    pub fn context_place(&mut self) -> ir::Expr {
        let ctx_ty = self.types.context_ty;
        let ptr = self.types.pointer(ctx_ty);
        ir::Expr::new(ExprKind::Deref(Box::new(ir::Expr::new(ExprKind::Context, ptr))), ctx_ty)
    }

    /// Returns `context.<field>` for one of the context's allocators.
    pub fn context_field(&mut self, name: &str) -> ir::Expr {
        let base = self.context_place();
        let ctx_ty = self.types.context_ty;
        let (index, ty) = self.field_index(ctx_ty, wid_syntax::Name::new(name)).expect("context has this field");
        ir::Expr::new(ExprKind::Field { base: Box::new(base), index }, ty)
    }

    /// Returns true when a place is the context or part of it.
    pub fn is_context_place(e: &ir::Expr) -> bool {
        match &e.kind {
            ExprKind::Context => true,
            ExprKind::Field { base, .. } | ExprKind::Deref(base) => Self::is_context_place(base),
            _ => false,
        }
    }

    /// Gives the innermost scope a private copy of the context before it is
    /// changed, so the change ends with the scope.
    pub fn shadow_context(&mut self) {
        let already = self.frame().scopes.last().is_some_and(|s| s.context_shadowed);
        if already {
            return;
        }
        if let Some(scope) = self.frame_mut().scopes.last_mut() {
            scope.context_shadowed = true;
        }
        self.begin_block();
    }

    /// Lowers an interpolated string into a builder on the temp allocator.
    pub fn interpolate(&mut self, parts: &[ast::StrPart], span: Span) -> ir::Expr {
        let string = self.types.string();
        let mut values = Vec::new();
        for part in parts {
            match part {
                ast::StrPart::Text(t) => values.push((ir::Expr::new(ExprKind::Str(t.clone()), string), false)),
                ast::StrPart::Interp(e) => {
                    let v = self.expr(e, None);
                    let ty = self.value_type(v.ty, e.span);
                    let v = ir::Expr::new(v.kind, ty);
                    let v = if v.is_pure() { v } else { self.spill(v) };
                    values.push((v, false));
                }
            }
        }
        self.build_string(values, span)
    }

    /// Lowers `value.to_s`.
    pub fn lower_to_s(&mut self, v: ir::Expr, span: Span) -> ir::Expr {
        if matches!(self.types.kind(self.types.base(v.ty)), TyKind::String) {
            return v;
        }
        let v = if v.is_pure() { v } else { self.spill(v) };
        self.build_string(vec![(v, false)], span)
    }

    /// Lowers `value.inspect`.
    pub fn inspect_string(&mut self, v: ir::Expr, span: Span) -> ir::Expr {
        let v = if v.is_pure() { v } else { self.spill(v) };
        self.build_string(vec![(v, true)], span)
    }

    fn build_string(&mut self, values: Vec<(ir::Expr, bool)>, span: Span) -> ir::Expr {
        let writer_ty = self.types.writer_ty;
        let string = self.types.string();
        let void = self.types.void();
        let temp = self.context_field("temp_allocator");
        let builder = self.new_local(None, writer_ty);
        self.emit(Stmt::Let {
            local: builder,
            init: Some(ir::Expr::new(ExprKind::Builtin { op: Builtin::BuilderNew, args: vec![temp], span }, writer_ty)),
        });
        let ptr_ty = self.types.pointer(writer_ty);
        let ptr = ir::Expr::new(ExprKind::AddrOf(Box::new(ir::Expr::new(ExprKind::Local(builder), writer_ty))), ptr_ty);
        for (v, inspect) in values {
            if let ExprKind::Str(s) = &v.kind
                && s.is_empty()
            {
                continue;
            }
            self.emit(Stmt::Expr(ir::Expr::new(
                ExprKind::Builtin { op: Builtin::Write { inspect }, args: vec![ptr.clone(), v], span },
                void,
            )));
        }
        ir::Expr::new(ExprKind::Builtin { op: Builtin::BuilderString, args: vec![ptr], span }, string)
    }

    /// Lowers `alloc(T)`, `alloc([]T, n)` and their `allocator:` forms.
    pub fn builtin_alloc(&mut self, args: &[ast::Arg], span: Span) -> ir::Expr {
        let (positional, allocator) = self.split_allocator_arg(args);
        let Some(first) = positional.first() else {
            self.report(
                Diagnostic::error(codes::ARG_COUNT, "`alloc` needs the type to allocate")
                    .primary(span, "write `alloc(T)` for one value or `alloc([]T, count)` for many"),
            );
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        };
        let ty = self.type_arg(&first.value);
        if matches!(self.types.kind(ty), TyKind::Unknown) {
            return ir::Expr::new(ExprKind::Zero, ty);
        }
        let allocator = allocator.unwrap_or_else(|| self.context_field("allocator"));
        match self.types.kind(ty).clone() {
            TyKind::Slice(_) => {
                let int = self.types.int();
                let count = match positional.get(1) {
                    Some(a) => self.expr_coerced(&a.value, int),
                    None => {
                        self.report(
                            Diagnostic::error(codes::ARG_COUNT, "`alloc` of a slice needs the element count")
                                .primary(span, "write it like `alloc([]Int, 16)`"),
                        );
                        ir::Expr::new(ExprKind::Int(0), int)
                    }
                };
                let count = if count.is_pure() { count } else { self.spill(count) };
                ir::Expr::new(ExprKind::Builtin { op: Builtin::AllocSlice, args: vec![count, allocator], span }, ty)
            }
            _ => {
                if let Some(extra) = positional.get(1) {
                    self.report(
                        Diagnostic::error(codes::ARG_COUNT, "`alloc(T)` allocates exactly one value")
                            .primary(extra.value.span, "unexpected count")
                            .help("to allocate many values, allocate a slice: `alloc([]T, count)`"),
                    );
                }
                let ptr = self.types.pointer(ty);
                ir::Expr::new(ExprKind::Builtin { op: Builtin::Alloc, args: vec![allocator], span }, ptr)
            }
        }
    }

    /// Lowers `free(x)` and `free(x, allocator: a)`.
    pub fn builtin_free(&mut self, args: &[ast::Arg], span: Span) -> ir::Expr {
        let void = self.types.void();
        let (positional, allocator) = self.split_allocator_arg(args);
        let [arg] = positional.as_slice() else {
            self.report(
                Diagnostic::error(codes::ARG_COUNT, "`free` takes the value to release")
                    .primary(span, "write it like `free(ptr)`"),
            );
            return ir::Expr::new(ExprKind::Zero, void);
        };
        let v = self.expr(&arg.value, None);
        let kind = self.types.kind(v.ty).clone();
        match kind {
            TyKind::Dynamic(_) | TyKind::Map(..) => {
                if allocator.is_some() {
                    self.report(
                        Diagnostic::error(
                            codes::BAD_NAMED_ARG,
                            "containers are freed with the allocator they grew with",
                        )
                        .primary(span, "remove `allocator:`"),
                    );
                }
                if !super::members::is_place(&v) {
                    self.report(
                        Diagnostic::error(codes::NOT_ASSIGNABLE, "`free` needs the container itself, not a copy")
                            .primary(arg.value.span, "pass a variable or field"),
                    );
                    return ir::Expr::new(ExprKind::Zero, void);
                }
                let ptr = self.address_of(v);
                ir::Expr::new(ExprKind::Builtin { op: Builtin::Free, args: vec![ptr], span }, void)
            }
            TyKind::Pointer(_) | TyKind::Slice(_) | TyKind::String | TyKind::CString => {
                let allocator = allocator.unwrap_or_else(|| self.context_field("allocator"));
                ir::Expr::new(ExprKind::Builtin { op: Builtin::Free, args: vec![v, allocator], span }, void)
            }
            TyKind::Unknown => ir::Expr::new(ExprKind::Zero, void),
            _ => {
                let shown = self.types.display(v.ty);
                self.report(
                    Diagnostic::error(codes::TYPE_MISMATCH, format!("`free` cannot release a value of type `{shown}`"))
                        .primary(arg.value.span, "not memory from an allocator")
                        .note("`free` takes pointers, slices, strings, dynamic arrays and maps"),
                );
                ir::Expr::new(ExprKind::Zero, void)
            }
        }
    }

    /// Lowers `free_all(allocator)`.
    pub fn builtin_free_all(&mut self, args: &[ast::Arg], span: Span) -> ir::Expr {
        let void = self.types.void();
        let allocator_ty = self.types.allocator_ty;
        let a = match args {
            [arg] => self.expr_coerced(&arg.value, allocator_ty),
            _ => {
                self.report(
                    Diagnostic::error(codes::ARG_COUNT, "`free_all` takes one allocator")
                        .primary(span, "write it like `free_all(context.temp_allocator)`"),
                );
                return ir::Expr::new(ExprKind::Zero, void);
            }
        };
        ir::Expr::new(ExprKind::Builtin { op: Builtin::FreeAll, args: vec![a], span }, void)
    }

    /// Lowers `size_of(T)` and `align_of(T)`.
    pub fn builtin_size(&mut self, args: &[ast::Arg], span: Span, align: bool) -> ir::Expr {
        let int = self.types.int();
        let [arg] = args else {
            self.report(Diagnostic::error(codes::ARG_COUNT, "expected one type").primary(span, "like `size_of(Vec2)`"));
            return ir::Expr::new(ExprKind::Int(0), int);
        };
        let builtin = if align { "align_of" } else { "size_of" };
        let ty = match self.variable_as_type_arg(&arg.value, builtin) {
            Some(t) => t,
            None => self.type_arg(&arg.value),
        };
        let (size, al) = self.types.layout(ty);
        ir::Expr::new(ExprKind::Int(i128::from(if align { al } else { size })), int)
    }

    /// Resolves an argument that names a type, like `alloc(Ball)`: a type
    /// written in place (`Int?`, `[]Ball`), a constant, a package's type or
    /// type alias (`geo.Shape`, `C.int?`), or a generic instance
    /// (`Pool(Ball, 64)`, `geo.Pool(Ball, 64)`).
    pub fn type_arg(&mut self, e: &ast::Expr) -> TyId {
        match &e.kind {
            E::Const(_) | E::Type(_) => {
                let texpr = super::members::expr_as_type(e);
                let ctx = self.body_ctx();
                self.resolve_type(&texpr, &ctx)
            }
            E::Member { recv, .. }
                if matches!(recv.kind, E::Ident(pkg) | E::Const(pkg) if self.lookup_import(self.loc(), pkg)
                    .is_some_and(|p| self.input.packages[p.0 as usize].path == "core:c")) =>
            {
                let texpr = super::members::expr_as_type(e);
                let ctx = self.body_ctx();
                self.resolve_type(&texpr, &ctx)
            }
            E::Member { .. } => match self.classify_receiver(e) {
                super::members::Receiver::Type(t) => t,
                _ => match self.named_type(e) {
                    Some(t) => t,
                    None => self.not_a_type_arg(e),
                },
            },
            E::Call(_) => match self.named_type(e) {
                Some(t) => t,
                None => self.not_a_type_arg(e),
            },
            _ => self.not_a_type_arg(e),
        }
    }

    fn not_a_type_arg(&mut self, e: &ast::Expr) -> TyId {
        if holds_parse_error(e) {
            // Like `Int ?`, which the parser reported as an unfinished `x ? a : b`.
            return self.types.unknown();
        }
        // A variable is read here, so don't also report it unused.
        if let E::Ident(name) = e.kind
            && let Some(var) = self.find_var_at(name, e.span)
        {
            var.read = true;
        }
        self.report(
            Diagnostic::error(codes::NOT_A_TYPE, "expected a type")
                .primary(e.span, "this is a value")
                .help("types are written like `Vec2`, `[]Int`, `Int?`, `^Node` or `proc(Int) -> Int`"),
        );
        self.types.unknown()
    }

    /// `size_of(count)` for a variable `count`: reports it with a fix that
    /// writes the variable's type, and marks it read so it isn't also
    /// reported unused.
    fn variable_as_type_arg(&mut self, e: &ast::Expr, builtin: &str) -> Option<TyId> {
        let E::Ident(name) = e.kind else { return None };
        let var = self.find_var_at(name, e.span)?;
        var.read = true;
        let (ty, decl_span) = (var.ty, var.span);
        let mut diag = Diagnostic::error(codes::NOT_A_TYPE, format!("`{name}` is a variable, not a type"))
            .primary(e.span, format!("`{builtin}` takes a type"))
            .secondary(decl_span, format!("`{name}` is declared here"));
        if !matches!(self.types.kind(ty), TyKind::Unknown) {
            diag = diag.suggest_replace(
                format!("to use the type of `{name}`, write it"),
                e.span,
                self.types.display(ty),
                Applicability::MaybeIncorrect,
            );
        }
        self.report(diag);
        Some(self.types.unknown())
    }

    /// Separates an `allocator:` argument from the others.
    fn split_allocator_arg<'e>(&mut self, args: &'e [ast::Arg]) -> (Vec<&'e ast::Arg>, Option<ir::Expr>) {
        let mut positional = Vec::new();
        let mut allocator = None;
        let allocator_ty = self.types.allocator_ty;
        for a in args {
            match a.name {
                Some(n) if n.as_str() == "allocator" => allocator = Some(self.expr_coerced(&a.value, allocator_ty)),
                Some(n) => {
                    self.report(
                        Diagnostic::error(codes::BAD_NAMED_ARG, format!("unknown argument `{}`", n.as_str()))
                            .primary(n.span, "the only named argument here is `allocator:`")
                            .suggest_replace(
                                "did you mean `allocator`?",
                                n.span,
                                "allocator",
                                Applicability::MaybeIncorrect,
                            ),
                    );
                }
                None => positional.push(a),
            }
        }
        (positional, allocator)
    }
}

/// Whether an operand of `e` failed to parse, so the parser has reported it.
pub(super) fn holds_parse_error(e: &ast::Expr) -> bool {
    match &e.kind {
        E::Error => true,
        E::Ternary { cond, then, else_ } => [cond, then, else_].iter().any(|x| holds_parse_error(x)),
        E::Binary { lhs, rhs, .. } => holds_parse_error(lhs) || holds_parse_error(rhs),
        E::Unary { expr, .. } | E::Paren(expr) => holds_parse_error(expr),
        _ => false,
    }
}

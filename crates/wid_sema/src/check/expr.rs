//! Checking and lowering expressions.

use wid_diagnostics::{Applicability, Diagnostic, Span, codes, did_you_mean};
use wid_syntax::Name;
use wid_syntax::ast::{self, ExprKind as E};

use super::body::Dest;
use super::items::{ConstValue, conversion_help};
use super::macros::{MacroCall, Operand, OperandKind};
use super::{Checker, DeclKind};
use crate::ir::{self, Builtin, ExprKind, Stmt};
use crate::types::{TyId, TyKind};

/// Returns true for expressions whose type comes from context: literals and
/// arithmetic on literals.
pub(crate) fn is_untyped(e: &ast::Expr) -> bool {
    match &e.kind {
        E::Int(_) | E::Float(_) | E::Nil | E::Zero => true,
        E::Paren(inner) => is_untyped(inner),
        E::Unary { op: ast::UnOp::Neg | ast::UnOp::BitNot, expr } => is_untyped(expr),
        E::Binary { op, lhs, rhs } if !op.is_comparison() && !matches!(op, ast::BinOp::And | ast::BinOp::Or) => {
            is_untyped(lhs) && is_untyped(rhs)
        }
        _ => false,
    }
}

/// Builtin functions callable by name anywhere.
pub(super) const BUILTINS: &[&str] = &[
    "method",
    "puts",
    "print",
    "p",
    "panic",
    "assert",
    "unreachable",
    "alloc",
    "free",
    "free_all",
    "size_of",
    "align_of",
    "context",
    "caller_location",
    "embed",
    "config",
    "type_info",
];

/// What a name reaches through `self`: a field of the struct `owner` (its
/// own, or one `using` promotes), or a method of the type `owner` (its own
/// or mixed in with `include`).
pub(super) enum SelfMember {
    Field { owner: TyId, is_proc: bool },
    Method { owner: TyId },
}

/// A field of `self` called like a method without `@`: the whole call,
/// whether it passes arguments, in parentheses or not, or a block, and
/// whether the field holds a proc.
#[derive(Clone, Copy)]
struct FieldCall {
    span: Span,
    args: bool,
    parens: bool,
    block: bool,
    is_proc: bool,
}

impl<'a> Checker<'a> {
    /// Checks `e` against `expected` and coerces the result to it.
    pub fn expr_coerced(&mut self, e: &ast::Expr, ty: TyId) -> ir::Expr {
        let v = self.expr(e, Some(ty));
        self.coerce(v, ty, e.span)
    }

    /// Checks and lowers an expression. `expected` guides literal typing but
    /// is not enforced; use [`Checker::coerce`] for that.
    pub fn expr(&mut self, e: &ast::Expr, expected: Option<TyId>) -> ir::Expr {
        // Code a macro generated resolves names by whose code it is.
        let site = self.enter_site(e.span);
        let v = self.expr_here(e, expected);
        self.leave_site(site);
        self.note_expr(e, &v);
        v
    }

    fn expr_here(&mut self, e: &ast::Expr, expected: Option<TyId>) -> ir::Expr {
        // A type that reads as a call or a member (`Pool(Ball, 64)`,
        // `C.int`, `rl.Color`) is a `Type` value where one is expected.
        if matches!(e.kind, E::Call(_) | E::Member { .. })
            && (self.comptime_depth > 0 || expected.is_some_and(|t| matches!(self.types.kind(t), TyKind::Type)))
            && let Some(v) = self.type_as_value(e)
        {
            return v;
        }
        match &e.kind {
            E::Int(v) => self.int_literal(*v, expected, e.span),
            E::Float(v) => self.float_literal(*v, expected, e.span),
            E::Str(parts) => self.string_literal(parts, expected, e.span),
            E::True | E::False => ir::Expr::new(ExprKind::Bool(matches!(e.kind, E::True)), self.types.bool()),
            E::Nil => match expected {
                Some(ty) if self.types.is_nilable(ty) => ir::Expr::new(ExprKind::Nil, ty),
                _ => ir::Expr::new(ExprKind::Nil, self.types.nil()),
            },
            E::Zero => match expected {
                Some(ty) => ir::Expr::new(ExprKind::Zero, ty),
                None => {
                    self.report(
                        Diagnostic::error(codes::CANNOT_INFER, "cannot tell which type `{}` is the zero value of")
                            .primary(e.span, "no type is expected here")
                            .help("use `Type.new` or give the variable a type, like `x: Vec2 = {}`"),
                    );
                    ir::Expr::new(ExprKind::Zero, self.types.unknown())
                }
            },
            E::Paren(inner) => self.expr(inner, expected),
            E::Ident(name) => self.ident(*name, e.span, expected),
            E::Const(name) => self.const_ref(*name, e.span, expected),
            E::Unary { op, expr } => self.unary(*op, expr, expected, e.span),
            E::Binary { op, lhs, rhs } => self.binary(*op, lhs, rhs, expected, e.span),
            E::Call(call) => self.call(call, expected, e.span),
            E::Member { recv, name, safe } => self.member_call(recv, *name, None, None, *safe, e.span, expected),
            E::Yield(args) => self.lower_yield(args, e.span),
            E::Lambda(lambda) => self.lower_lambda(lambda, expected, e.span),
            E::SelfRef => self.self_place(e.span, "`self`"),
            E::IVar(name) => self.ivar(*name, e.span),
            E::Symbol(name) => self.symbol(*name, e.span, expected),
            E::Case(case) => self.case_value(case, expected, e.span),
            E::AddrOf(inner) => self.address_of_expr(inner, e.span),
            E::Array(elems) => self.array_literal(elems, expected, e.span),
            E::Index { recv, args } => self.index_expr(recv, args, e.span),
            E::Deref(inner) => {
                let v = self.expr(inner, None);
                match self.types.kind(v.ty).clone() {
                    TyKind::Pointer(t) => ir::Expr::new(ExprKind::Deref(Box::new(v)), t),
                    TyKind::Unknown => v,
                    _ => {
                        let shown = self.types.display(v.ty);
                        self.report(
                            Diagnostic::error(
                                codes::TYPE_MISMATCH,
                                format!("`^` dereferences a pointer, but this is `{shown}`"),
                            )
                            .primary(inner.span, "not a pointer"),
                        );
                        ir::Expr::new(ExprKind::Zero, self.types.unknown())
                    }
                }
            }
            E::If(if_expr) => self.if_value(if_expr, expected, e.span),
            E::Ternary { cond, then, else_ } => self.ternary(cond, then, else_, expected, e.span),
            E::While { .. } | E::Loop(_) | E::For(_) => {
                self.report(
                    Diagnostic::error(codes::NOT_A_VALUE, "loops do not produce a value")
                        .primary(e.span, "used as a value here"),
                );
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
            E::Error => ir::Expr::new(ExprKind::Zero, self.types.unknown()),
            E::Range { .. } => {
                self.report(
                    Diagnostic::error(codes::NOT_A_VALUE, "a range is not a value by itself")
                        .primary(e.span, "ranges work in `for`, `case` and indexing")
                        .help("loop with `for i in a...b`, or slice with `xs[a...b]`"),
                );
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
            E::Uninit => {
                self.report(
                    Diagnostic::error(codes::NOT_A_VALUE, "`---` is not a value")
                        .primary(e.span, "only allowed as the initial value of a declaration")
                        .help("declare the variable with its type and opt out of zeroing: `buf: [256]U8 = ---`"),
                );
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
            E::Type(t) => {
                // Where a `Type` is expected, or in `comptime` code, a type
                // written in place is a `Type` value.
                let wants_type = expected.is_some_and(|t| matches!(self.types.kind(t), TyKind::Type));
                if (wants_type || self.comptime_depth > 0)
                    && let Some(v) = self.type_as_value(e)
                {
                    return v;
                }
                let text = self.source_text(e.span);
                let help = self.make_value_help(t, &text);
                self.report(
                    Diagnostic::error(codes::NOT_A_VALUE, format!("`{text}` is a type, not a value"))
                        .primary(e.span, "a type cannot be used as a value here")
                        .help(help),
                );
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
            E::Comptime(body) => self.comptime_expr(body, expected, e.span),
            E::ComptimeIf(if_expr) => self.comptime_if_value(if_expr, expected, e.span),
            E::Quote(quote) => self.lower_quote(quote, e.span),
            // Expansion replaces every splice of a `quote`; the parser
            // reports one anywhere else.
            E::Splice(_) => ir::Expr::new(ExprKind::Zero, self.types.unknown()),
        }
    }

    /// Converts `v` to `ty`, reporting a mismatch when no implicit conversion
    /// exists.
    pub fn coerce(&mut self, v: ir::Expr, ty: TyId, span: Span) -> ir::Expr {
        if v.ty == ty {
            return v;
        }
        let from = self.types.kind(v.ty).clone();
        let to = self.types.kind(ty).clone();
        match (&from, &to) {
            (TyKind::Unknown, _) | (_, TyKind::Unknown) | (TyKind::Never, _) => {
                return ir::Expr::new(v.kind, ty);
            }
            (TyKind::Nil, _) if self.types.is_nilable(ty) => return ir::Expr::new(ExprKind::Nil, ty),
            (_, TyKind::Optional(inner)) if *inner == v.ty => return self.opt_some(v, ty),
            (TyKind::Array(a, _) | TyKind::Dynamic(a), TyKind::Slice(b)) if a == b => {
                return self.slice_of_container(v, ty);
            }
            (_, TyKind::Union(_)) if self.union_variant(ty, v.ty).is_some() => {
                let variant = self.union_variant(ty, v.ty).unwrap_or(0);
                return ir::Expr::new(ExprKind::UnionWrap { variant, value: Box::new(v) }, ty);
            }
            (TyKind::Optional(inner), _) if *inner == ty => {
                self.maybe_nil(&v, span);
                return ir::Expr::new(ExprKind::OptGet(Box::new(v)), ty);
            }
            (TyKind::Pointer(_) | TyKind::MultiPointer(_), TyKind::RawPtr) => {
                return ir::Expr::new(ExprKind::Cast { kind: ir::CastKind::Pointer, expr: Box::new(v) }, ty);
            }
            (TyKind::Optional(_), TyKind::RawPtr) if self.types.optional_is_pointer(v.ty) => {
                return ir::Expr::new(ExprKind::Cast { kind: ir::CastKind::Pointer, expr: Box::new(v) }, ty);
            }
            (TyKind::Pointer(_) | TyKind::MultiPointer(_), TyKind::Optional(inner))
                if matches!(self.types.kind(*inner), TyKind::RawPtr) =>
            {
                return ir::Expr::new(ExprKind::Cast { kind: ir::CastKind::Pointer, expr: Box::new(v) }, ty);
            }
            (TyKind::Optional(from_inner), TyKind::Optional(inner))
                if matches!(self.types.kind(*inner), TyKind::RawPtr)
                    && matches!(self.types.kind(*from_inner), TyKind::Pointer(_) | TyKind::MultiPointer(_)) =>
            {
                return ir::Expr::new(ExprKind::Cast { kind: ir::CastKind::Pointer, expr: Box::new(v) }, ty);
            }
            _ => {}
        }
        self.mismatch(&v, ty, span);
        ir::Expr::new(v.kind, ty)
    }

    /// Reports that `v` is not a `want`.
    pub fn mismatch(&mut self, v: &ir::Expr, want: TyId, span: Span) {
        let found = self.types.display(v.ty);
        let want_s = self.types.display(want);
        if matches!(self.types.kind(v.ty), TyKind::Void) {
            self.report(
                Diagnostic::error(codes::TYPE_MISMATCH, format!("expected `{want_s}`, but this has no value"))
                    .primary(span, "this returns nothing")
                    .help(format!(
                        "pass a value of type `{want_s}` here, or give the method a return type with `-> {want_s}`"
                    )),
            );
            return;
        }
        if matches!(self.types.kind(v.ty), TyKind::Nil) {
            self.report(
                Diagnostic::error(codes::TYPE_MISMATCH, format!("`nil` is not a valid `{want_s}`"))
                    .primary(span, format!("`{want_s}` can never be nil"))
                    .help(format!("if the value is optional, use the type `{want_s}?`")),
            );
            return;
        }
        let mut diag = Diagnostic::error(codes::TYPE_MISMATCH, format!("expected `{want_s}`, found `{found}`"))
            .primary(span, format!("this has type `{found}`"));
        if self.types.is_numeric(v.ty) && self.types.is_numeric(want) {
            let text = self.source_text(span);
            diag = conversion_help(diag, span, &text, &want_s);
            diag = diag.note("Wid never converts between numeric types implicitly");
        }
        if matches!(self.types.kind(want), TyKind::Code) && matches!(self.types.kind(v.ty), TyKind::String) {
            diag = self.string_as_code(diag, span);
        }
        self.report(diag);
    }

    /// Advice for a string where a macro's `Code` goes, as Ruby builds code
    /// for `eval`: a macro builds code with `quote do … end`. A string
    /// literal on one line is written out as that `quote`, where its
    /// interpolations become splices.
    fn string_as_code(&self, diag: Diagnostic, span: Span) -> Diagnostic {
        let help =
            "a macro builds code with `quote do … end`, not with a string; in a `quote`, `#{…}` splices a value in";
        let text = self.source_text(span);
        match text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
            Some(code) if !code.trim().is_empty() && !code.contains(['"', '\\', '\n']) => {
                let indent = self.indent_at(span);
                let replacement = format!("quote do\n{indent}  {}\n{indent}end", code.trim());
                diag.suggest_replace(help, span, replacement, Applicability::MaybeIncorrect)
            }
            _ => diag.help(help),
        }
    }

    /// Returns the type a variable gets when initialized with a value of
    /// type `ty`, reporting values that cannot be stored.
    pub fn value_type(&mut self, ty: TyId, span: Span) -> TyId {
        match self.types.kind(ty) {
            TyKind::Nil => {
                self.report(
                    Diagnostic::error(codes::CANNOT_INFER, "cannot infer the type of `nil`")
                        .primary(span, "`nil` alone doesn't say which optional type this is")
                        .help("declare the type, like `x: Int? = nil`"),
                );
                self.types.unknown()
            }
            TyKind::Void => {
                self.report(
                    Diagnostic::error(codes::NOT_A_VALUE, "this expression has no value")
                        .primary(span, "it returns nothing, so there is nothing to store"),
                );
                self.types.unknown()
            }
            TyKind::Never => self.types.unknown(),
            // Compile-time code keeps names as `Symbol` values, and a
            // `Symbol` that is not a literal is E0906's to report.
            TyKind::Symbol
                if self.comptime_depth > 0
                    || self.macros.in_macro
                    || !self.source_text(span).trim_start().starts_with(':') =>
            {
                ty
            }
            TyKind::Symbol => {
                self.report(
                    Diagnostic::error(codes::CANNOT_INFER, "cannot infer which enum this symbol belongs to")
                        .primary(span, "symbols need an expected enum type")
                        .help("declare the variable's type, like `dir: Dir = :north`"),
                );
                self.types.unknown()
            }
            _ => ty,
        }
    }

    // ----- literals ------------------------------------------------------

    fn int_literal(&mut self, v: u128, expected: Option<TyId>, span: Span) -> ir::Expr {
        let Ok(v) = i128::try_from(v) else {
            self.report(
                Diagnostic::error(codes::CONSTANT_OVERFLOW, "integer literal is too large").primary(span, "too large"),
            );
            return ir::Expr::new(ExprKind::Int(0), self.types.unknown());
        };
        self.const_with_expected(ConstValue::Int(v), expected, span)
    }

    fn float_literal(&mut self, v: f64, expected: Option<TyId>, span: Span) -> ir::Expr {
        self.const_with_expected(ConstValue::Float(v), expected, span)
    }

    /// Types an untyped constant using the expected type when it fits.
    pub fn const_with_expected(&mut self, v: ConstValue, expected: Option<TyId>, span: Span) -> ir::Expr {
        if let Some(ty) = expected {
            let base = self.types.base(ty);
            let fits = matches!(
                (&v, self.types.kind(base)),
                (ConstValue::Int(_), TyKind::Int(_) | TyKind::Float(_))
                    | (ConstValue::Float(_), TyKind::Float(_))
                    | (ConstValue::Bool(_), TyKind::Bool)
                    | (ConstValue::Str(_), TyKind::String | TyKind::CString)
            ) || matches!((&v, self.types.kind(base)), (ConstValue::Str(s), TyKind::Rune) if s.chars().count() == 1);
            if fits {
                if let Some(e) = self.typed_const(v, ty, span) {
                    return e;
                }
                return ir::Expr::new(ExprKind::Zero, self.types.unknown());
            }
            if let ConstValue::Int(i) = v
                && self.types.is_float(base)
            {
                return ir::Expr::new(ExprKind::Float(i as f64), ty);
            }
        }
        if let ConstValue::Int(i) = v {
            let (lo, hi) = crate::types::IntTy::Int.range();
            if i < lo || i > hi {
                self.report(
                    Diagnostic::error(codes::CONSTANT_OVERFLOW, format!("`{i}` does not fit in `Int`"))
                        .primary(span, "too large for a 64-bit integer")
                        .help("give it an unsigned type, like `x: U64 = …`"),
                );
            }
        }
        self.default_const(v)
    }

    fn string_literal(&mut self, parts: &[ast::StrPart], expected: Option<TyId>, span: Span) -> ir::Expr {
        let mut text = String::new();
        for part in parts {
            match part {
                ast::StrPart::Text(t) => text.push_str(t),
                ast::StrPart::Interp(_) => return self.interpolate(parts, span),
            }
        }
        if let Some(ty) = expected
            && let TyKind::Optional(inner) = *self.types.kind(ty)
            && matches!(self.types.kind(inner), TyKind::CString)
        {
            let v = self.const_with_expected(ConstValue::Str(text), Some(inner), span);
            return ir::Expr::new(ExprKind::OptSome(Box::new(v)), ty);
        }
        self.const_with_expected(ConstValue::Str(text), expected, span)
    }

    // ----- names ---------------------------------------------------------

    /// Lowers `:name`, which selects an enum member when one is expected,
    /// and is otherwise a `Symbol` value (an error at run time).
    fn symbol(&mut self, name: Name, span: Span, expected: Option<TyId>) -> ir::Expr {
        if let Some(ty) = expected {
            let base = self.types.base(ty);
            match self.types.kind(base) {
                TyKind::Enum(_) => return self.enum_member(base, name, span),
                TyKind::Error => {
                    self.error_tag(name);
                    return ir::Expr::new(ExprKind::ErrorTag(name), ty);
                }
                _ => {}
            }
        }
        ir::Expr::new(ExprKind::Int(i128::from(name.index())), self.types.symbol())
    }

    fn ident(&mut self, name: Name, span: Span, expected: Option<TyId>) -> ir::Expr {
        if let Some(var) = self.find_var_at(name, span) {
            var.read = true;
            let (local, ty, indirect, binding) = (var.local, var.ty, var.indirect, var.span);
            self.note_local(span, binding);
            let read = self.var_place(local, ty, indirect);
            if self.is_narrowed(local) && self.optional_inner(ty).is_some() {
                return self.opt_get(read);
            }
            if let Some(variant) = self.narrowed_variant(local)
                && let TyKind::Union(id) = *self.types.kind(self.types.base(ty))
            {
                let vty = self.types.union_info(id).variants[variant as usize];
                return ir::Expr::new(ExprKind::UnionGet { value: Box::new(read), variant }, vty);
            }
            return read;
        }
        if let Some(v) = self.implicit_self_call(name, &[], None, span, span) {
            return v;
        }
        let loc = self.loc_at(span);
        if let Some(decl) = self.lookup_pkg(loc.pkg, name).or_else(|| self.lookup_prelude(name))
            && let DeclKind::Fn(_) = self.decls[decl.0 as usize].kind
        {
            if self.is_macro(decl) {
                self.check_visible(decl, span);
                let call = MacroCall { decl, shown: name.to_string(), args: &[], block: None, name_span: span, span };
                return self.call_macro(call, expected);
            }
            return self.call_fn(decl, None, &[], None, span, span);
        }
        if BUILTINS.contains(&name.as_str()) {
            return self.builtin_call(name, &[], span, expected);
        }
        if self.lookup_import(loc, name).is_some() {
            self.report(
                Diagnostic::error(codes::NOT_A_VALUE, format!("`{name}` is a package, not a value"))
                    .primary(span, "use one of its members, like `pkg.name`"),
            );
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        if self.report_capture(name, span) {
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        // Names resolve to a variable, then a method of `self`, then a
        // declaration of the package; ties in a suggestion go the same way.
        let vars = self.visible_var_names();
        let own = self.self_names(false);
        let mut candidates = vars.clone();
        candidates.extend(&own.methods);
        candidates.extend(self.package_names(loc.pkg));
        candidates.extend(BUILTINS.iter().copied());
        if let Some(best) = own.closest(name.as_str(), &candidates)
            && vars.contains(&best)
            && let Some(var) = self.find_var(Name::new(best))
        {
            var.read = true;
        }
        if !self.declared_by_failed_macro(true, true) && !self.report_scoped_out(name, span) {
            match self.self_field(name) {
                Some((owner, _)) => self.undefined_field_name(name, span, owner, None),
                None => self.undefined_near(name, span, &candidates, &own, "name"),
            }
        }
        ir::Expr::new(ExprKind::Zero, self.types.unknown())
    }

    /// What a name without a receiver may have meant here, for "did you
    /// mean" hints: the methods a call without a receiver reaches, and the
    /// fields `@name` reads; for a call (`calls`), only the fields that hold
    /// a proc, which `@name(…)` calls.
    pub(super) fn self_names(&mut self, calls: bool) -> super::SelfNames {
        super::SelfNames { methods: self.self_method_names(), fields: self.self_field_names(calls) }
    }

    /// The names of the methods a call without a receiver reaches in a
    /// method of a type, as [`Self::implicit_self_call`] finds them: the
    /// type's own, those its modules mix in and those `extend` blocks add,
    /// then in an instance method those each `using` field promotes. A
    /// type-level method reaches only type-level methods. Sorted, since
    /// ties in a suggestion go to the first.
    fn self_method_names(&mut self) -> Vec<&'static str> {
        let Some(self_ty) = self.frame().self_ty else { return Vec::new() };
        let instance = self.frame().self_local.is_some();
        let mut names = Vec::new();
        let mut types = vec![self_ty];
        let mut next = 0;
        while let Some(&ty) = types.get(next) {
            next += 1;
            // A promoted method is called on the field's value.
            let promoted = next > 1;
            for (name, decl) in self.method_decls(ty) {
                let callable = match self.decls[decl.0 as usize].kind {
                    DeclKind::Fn(f) if promoted => !f.is_static,
                    DeclKind::Fn(f) => instance || f.is_static,
                    DeclKind::Overload(_) => instance,
                    _ => false,
                };
                let word = name.as_str().starts_with(|c: char| c.is_alphabetic() || c == '_');
                if callable && word && !names.contains(&name.as_str()) {
                    names.push(name.as_str());
                }
            }
            if !instance {
                break;
            }
            let TyKind::Struct(id) = *self.types.kind(ty) else { continue };
            for f in &self.types.struct_info(id).fields {
                let used = match *self.types.kind(f.ty) {
                    TyKind::Pointer(t) => t,
                    _ => f.ty,
                };
                if f.using && !types.contains(&used) {
                    types.push(used);
                }
            }
        }
        names.sort_unstable();
        names
    }

    /// The methods of `ty` by name: its own, then those its modules mix in,
    /// then those `extend` blocks add.
    fn method_decls(&mut self, ty: TyId) -> Vec<(Name, super::DeclId)> {
        let mut owners: Vec<super::DeclId> = self.type_decl(ty).into_iter().collect();
        owners.extend(self.extends_of(ty));
        let mut next = 0;
        while let Some(&owner) = owners.get(next) {
            next += 1;
            for m in self.includes_of(owner) {
                if !owners.contains(&m) {
                    owners.push(m);
                }
            }
        }
        let mut methods = Vec::new();
        for owner in owners {
            if let Some(members) = self.members.get(&owner) {
                methods.extend(members.iter().map(|(n, d)| (*n, *d)));
            }
        }
        methods
    }

    /// The struct declaring `name` when it is a field of `self` in an
    /// instance method, its own or promoted by `using`, which a bare name
    /// doesn't read, and whether the field holds a proc.
    fn self_field(&mut self, name: Name) -> Option<(TyId, bool)> {
        let self_ty = self.frame().self_ty?;
        self.frame().self_local?;
        match self.self_member(self_ty, name, &mut Vec::new())? {
            SelfMember::Field { owner, is_proc } => Some((owner, is_proc)),
            SelfMember::Method { .. } => None,
        }
    }

    /// The names of the fields that `@name` reads in an instance method:
    /// the fields of `self`, then those each `using` field promotes; with
    /// `procs`, only those that hold a proc.
    fn self_field_names(&self, procs: bool) -> Vec<&'static str> {
        let (Some(self_ty), Some(_)) = (self.frame().self_ty, self.frame().self_local) else { return Vec::new() };
        let (mut names, mut types) = (Vec::new(), vec![self_ty]);
        let mut next = 0;
        while let Some(&ty) = types.get(next) {
            next += 1;
            let TyKind::Struct(id) = *self.types.kind(ty) else { continue };
            for f in &self.types.struct_info(id).fields {
                if !procs || matches!(self.types.kind(f.ty), TyKind::Proc(_)) {
                    names.push(f.name.as_str());
                }
                let used = match *self.types.kind(f.ty) {
                    TyKind::Pointer(t) => t,
                    _ => f.ty,
                };
                if f.using && !types.contains(&used) {
                    types.push(used);
                }
            }
        }
        names
    }

    /// Finds what `name` reaches in `ty`, as `self.name` does: the struct's
    /// own members first, then each `using` field in order.
    pub(super) fn self_member(&mut self, ty: TyId, name: Name, visited: &mut Vec<TyId>) -> Option<SelfMember> {
        let ty = match *self.types.kind(ty) {
            TyKind::Pointer(t) => t,
            _ => ty,
        };
        if visited.contains(&ty) {
            return None;
        }
        visited.push(ty);
        if let Some((_, fty)) = self.field_index(ty, name) {
            let is_proc = matches!(self.types.kind(fty), TyKind::Proc(_));
            return Some(SelfMember::Field { owner: ty, is_proc });
        }
        if self.find_method(ty, name).is_some() || self.find_included(ty, name).is_some() {
            return Some(SelfMember::Method { owner: ty });
        }
        let TyKind::Struct(id) = *self.types.kind(ty) else { return None };
        let used: Vec<TyId> = self.types.struct_info(id).fields.iter().filter(|f| f.using).map(|f| f.ty).collect();
        used.into_iter().find_map(|t| self.self_member(t, name, visited))
    }

    /// Reports a field of `self` named without `@`, which needs it: read as
    /// a bare name (`call` is `None`) or called like a method.
    fn undefined_field_name(&mut self, name: Name, span: Span, owner: TyId, call: Option<FieldCall>) {
        let shown = self.types.display(owner);
        let self_ty = self.frame().self_ty;
        let note = match self_ty {
            Some(t) if t != owner => {
                let promoted = self.types.display(t);
                format!("`{name}` is a field of `{shown}`, promoted into `{promoted}` by `using`")
            }
            _ => format!("`{name}` is a field of `{shown}`"),
        };
        let what = if call.is_some() { "method" } else { "name" };
        let diag = Diagnostic::error(codes::UNDEFINED_NAME, format!("undefined {what} `{name}`"))
            .primary(span, "not found in this scope")
            .note(format!("{note}; a bare name in a method is a variable or a method call, never a field"));
        let read = format!("read the field with `@{name}`, which means `self.{name}`");
        let diag = match call {
            None => diag.suggest_replace(read, span, format!("@{name}"), Applicability::MachineApplicable),
            // `on_hit(3)` is `@on_hit(3)`, and `on_hit 3` too.
            Some(FieldCall { is_proc: true, block: false, args, parens, span: call_span }) => {
                let (at, text) = if parens || !args {
                    (span, format!("@{name}"))
                } else {
                    let written = self.source_text(Span { start: span.end, ..call_span });
                    (call_span, format!("@{name}({})", written.trim()))
                };
                let help = format!("call the proc the field holds with `@{name}(…)`");
                diag.suggest_replace(help, at, text, Applicability::MachineApplicable)
            }
            Some(FieldCall { is_proc: true, .. }) => {
                diag.help(format!("call the proc the field holds with `@{name}(…)`, without a block"))
            }
            // `hp()` reads the field: `@hp`.
            Some(FieldCall { span, args: false, block: false, .. }) => {
                diag.suggest_replace(read, span, format!("@{name}"), Applicability::MachineApplicable)
            }
            Some(_) => diag.help(format!("{read}, without arguments or a block")),
        };
        self.report(diag);
    }

    /// Reports a name without a receiver that several `using` fields of
    /// `self` provide (`hits`), with the fix to reach it through the first
    /// with `@`, and checks the call's arguments.
    fn ambiguous_self_member(
        &mut self,
        name: Name,
        hits: &[(u32, TyId, Name)],
        args: &[ast::Arg],
        block: bool,
        name_span: Span,
        span: Span,
    ) {
        let Some(self_ty) = self.frame().self_ty else { return };
        let Some(&(_, first_ty, first)) = hits.first() else { return };
        let at = format!("@{first}.{name}");
        // A field called with nothing, `hp()`, is read as `@a.hp`; a method
        // keeps its arguments, `@a.heal(1)`.
        let reads =
            matches!(self.self_member(first_ty, name, &mut Vec::new()), Some(SelfMember::Field { is_proc: false, .. }));
        let fix_span = if reads && args.is_empty() && !block { span } else { name_span };
        let fix = super::overloads::UsingFix { like: at.clone(), span: fix_span, replacement: at };
        self.ambiguous_using(self_ty, name, name_span, hits, fix);
        for a in args {
            self.expr(&a.value, None);
        }
    }

    /// Calls a method of the current type without an explicit receiver,
    /// like Ruby's implicit `self`.
    fn implicit_self_call(
        &mut self,
        name: Name,
        args: &[ast::Arg],
        block: Option<&ast::BlockArg>,
        name_span: Span,
        span: Span,
    ) -> Option<ir::Expr> {
        let self_ty = self.frame().self_ty?;
        let own = self.find_method(self_ty, name);
        let included = if own.is_none() { self.find_included(self_ty, name) } else { None };
        let (decl, bindings) = match own {
            Some(d) => (d, Vec::new()),
            None if included.is_some() => (included.expect("checked"), vec![(Name::new("Self"), self_ty)]),
            None => match self.find_extension(self_ty, name, name_span) {
                Some((d, b)) => (d, b),
                None => {
                    self.frame().self_local?;
                    let member = self.self_member(self_ty, name, &mut Vec::new());
                    // A bare name never reads a field, its own or promoted:
                    // `ident` and `call` report it with the fix to write
                    // `@name`. A promoted proc field can be called.
                    let own_field = matches!(member, Some(SelfMember::Field { owner, .. }) if owner == self_ty);
                    let hits = if own_field { Vec::new() } else { self.using_hits(self_ty, name) };
                    if hits.len() > 1 {
                        self.ambiguous_self_member(name, &hits, args, block.is_some(), name_span, span);
                        return Some(ir::Expr::new(ExprKind::Zero, self.types.unknown()));
                    }
                    if own_field || matches!(member, Some(SelfMember::Field { is_proc: false, .. })) {
                        return None;
                    }
                    let recv = self.self_place(name_span, "this");
                    let ident = ast::Ident { name, span: name_span };
                    if !hits.is_empty() {
                        return Some(self.value_member(recv, name_span, ident, Some(args), block, span, None));
                    }
                    let v = self.container_method(&recv, ident, args, span);
                    if v.is_some() {
                        self.reject_builtin_block(block, ident, "a built-in method");
                    }
                    return v;
                }
            },
        };
        if let DeclKind::Overload(_) = self.decls[decl.0 as usize].kind {
            if let Some(b) = block {
                self.reject_block(b, "overloaded methods do not take blocks");
            }
            let recv = self.self_place(name_span, &format!("calling `{name}` without a receiver"));
            if matches!(self.types.kind(recv.ty), TyKind::Unknown) {
                return Some(recv);
            }
            let ptr = self.address_of(recv);
            return Some(self.call_overloaded(decl, Some(ptr), Some(self_ty), args, name_span, span));
        }
        let DeclKind::Fn(f) = self.decls[decl.0 as usize].kind else { return None };
        if f.is_static {
            self.owner_bindings = if bindings.is_empty() { self.instance_bindings(self_ty) } else { bindings };
            return Some(self.call_fn(decl, None, args, block, name_span, span));
        }
        let recv = self.self_place(name_span, &format!("calling `{name}` without a receiver"));
        if matches!(self.types.kind(recv.ty), TyKind::Unknown) {
            return Some(recv);
        }
        let ptr = self.address_of(recv);
        self.owner_bindings = bindings;
        Some(self.call_fn(decl, Some(ptr), args, block, name_span, span))
    }

    /// Reads a constant declaration's value, typed by `expected` when untyped.
    pub fn const_ref_decl(&mut self, decl: super::DeclId, span: Span, expected: Option<TyId>) -> ir::Expr {
        self.note_const(span, decl);
        if self.is_extern_const(decl) {
            return self.extern_const(decl, span);
        }
        if let DeclKind::Const(c) = self.decls[decl.0 as usize].kind
            && c.ty.is_none()
            && let Some(v) = self.const_untyped(decl)
        {
            return self.const_with_expected(v, expected, span);
        }
        match self.const_value(decl) {
            Some(v) => v,
            None => ir::Expr::new(ExprKind::Zero, self.types.unknown()),
        }
    }

    fn const_ref(&mut self, name: Name, span: Span, expected: Option<TyId>) -> ir::Expr {
        // A value parameter of the generic struct whose method this is, like
        // `N` in `Pool(Ball, 64)`: an untyped integer constant.
        if let Some(t) = self.body.frames.last().and_then(|f| super::generics::lookup(&f.subst, name)) {
            match *self.types.kind(t) {
                TyKind::ConstValue(v) => return self.const_with_expected(ConstValue::Int(v), expected, span),
                // An argument that was reported.
                TyKind::Unknown => return ir::Expr::new(ExprKind::Zero, t),
                _ => {}
            }
        }
        let loc = self.loc();
        let wants_type = expected.is_some_and(|t| matches!(self.types.kind(t), TyKind::Type));
        if (wants_type || self.comptime_depth > 0)
            && let Some(v) = self.type_as_value(&ast::Expr { kind: E::Const(name), span })
        {
            return v;
        }
        // A type parameter, like `T` in a method of `Box(Int)`.
        if let Some(t) = self.body.frames.last().and_then(|f| super::generics::lookup(&f.subst, name)) {
            self.type_param_as_value(name, t, span, expected);
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        if let Some(self_ty) = self.frame().self_ty
            && let Some(decl) = self.find_method(self_ty, name)
            && let DeclKind::Const(_) = self.decls[decl.0 as usize].kind
        {
            return self.const_ref_decl(decl, span, expected);
        }
        if let Some(decl) = self.lookup_pkg(loc.pkg, name).or_else(|| self.lookup_prelude(name)) {
            if let DeclKind::Const(_) = self.decls[decl.0 as usize].kind {
                return self.const_ref_decl(decl, span, expected);
            }
            let what = self.decls[decl.0 as usize].kind.a_describe();
            self.report(
                Diagnostic::error(codes::NOT_A_VALUE, format!("`{name}` is {what}, not a value"))
                    .primary(span, "expected a value here"),
            );
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        if self.primitive(name.as_str()).is_some() {
            self.report(
                Diagnostic::error(codes::NOT_A_VALUE, format!("`{name}` is a type, not a value"))
                    .primary(span, "expected a value here"),
            );
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        if name.as_str() == "Self" && self.frame().self_ty.is_some() {
            self.self_as_value(span);
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        let candidates = self
            .package_names(loc.pkg)
            .into_iter()
            .filter(|n| n.chars().next().is_some_and(char::is_uppercase))
            .collect();
        if !self.declared_by_failed_macro(false, true) {
            self.undefined(name, span, candidates, "constant");
        }
        ir::Expr::new(ExprKind::Zero, self.types.unknown())
    }

    /// Reports `Self` written where a value goes in a method, as in
    /// `"#{Self}"`: it names a type. Its name is `type_info(Self).name`, or,
    /// in code a macro generated, a string the macro computes.
    fn self_as_value(&mut self, span: Span) {
        let mut diag = Diagnostic::error(codes::NOT_A_VALUE, "`Self` is a type, not a value")
            .primary(span, "expected a value here")
            .note("`Self` names the type whose method this is")
            .help("for the type's name, use `type_info(Self).name`");
        if span.file.expansion_index().is_some() {
            diag = diag.help(
                "or compute the name in the macro, before the `quote` (`name = Self.name`), and splice it as a string: `#{name}`",
            );
        }
        self.report(diag);
    }

    // ----- operators -----------------------------------------------------

    fn unary(&mut self, op: ast::UnOp, operand: &ast::Expr, expected: Option<TyId>, span: Span) -> ir::Expr {
        match op {
            ast::UnOp::Not => {
                let bool_ty = self.types.bool();
                let v = self.expr(operand, Some(bool_ty));
                let v = self.truthy_in(v, operand.span, Some(span));
                self.not(v)
            }
            ast::UnOp::Neg | ast::UnOp::BitNot => {
                if op == ast::UnOp::Neg
                    && let E::Int(_) | E::Float(_) = operand.kind
                    && let Some(v) = self.fold_const(
                        &ast::Expr { kind: E::Unary { op, expr: Box::new(operand.clone()) }, span },
                        self.loc(),
                    )
                {
                    return self.const_with_expected(v, expected, span);
                }
                let v = self.expr(operand, expected);
                let method = Name::new(if op == ast::UnOp::Neg { "-@" } else { "~@" });
                if matches!(self.types.kind(v.ty), TyKind::Struct(_) | TyKind::Enum(_))
                    && let Some(decl) = self.find_method(v.ty, method)
                    && matches!(self.decls[decl.0 as usize].kind, DeclKind::Fn(f) if !f.is_static)
                {
                    let recv = self.address_of(v);
                    return self.call_fn(decl, Some(recv), &[], None, operand.span, span);
                }
                if op == ast::UnOp::Neg
                    && let Some(elem) = self.numeric_array_elem(v.ty)
                {
                    let ty = v.ty;
                    let zero = ir::Expr::new(ExprKind::Zero, elem);
                    return ir::Expr::new(
                        ExprKind::Binary { op: ir::BinaryOp::Sub, lhs: Box::new(zero), rhs: Box::new(v), span },
                        ty,
                    );
                }
                let ok = if op == ast::UnOp::Neg {
                    self.types.is_numeric(v.ty) && !self.is_unsigned(v.ty)
                } else {
                    self.types.is_int(v.ty)
                };
                if !ok && !matches!(self.types.kind(v.ty), TyKind::Unknown) {
                    let shown = self.types.display(v.ty);
                    self.report(
                        Diagnostic::error(
                            codes::NO_OPERATOR,
                            format!("cannot apply unary `{}` to `{shown}`", op.as_str()),
                        )
                        .primary(operand.span, format!("this has type `{shown}`")),
                    );
                }
                let ty = v.ty;
                let op = if op == ast::UnOp::Neg { ir::UnaryOp::Neg } else { ir::UnaryOp::BitNot };
                ir::Expr::new(ExprKind::Unary { op, expr: Box::new(v) }, ty)
            }
        }
    }

    fn is_unsigned(&self, ty: TyId) -> bool {
        matches!(self.types.kind(self.types.base(ty)), TyKind::Int(i) if !i.signed())
    }

    fn binary(
        &mut self,
        op: ast::BinOp,
        lhs: &ast::Expr,
        rhs: &ast::Expr,
        expected: Option<TyId>,
        span: Span,
    ) -> ir::Expr {
        if is_untyped(lhs)
            && is_untyped(rhs)
            && let Some(v) = self.fold_const(
                &ast::Expr { kind: E::Binary { op, lhs: Box::new(lhs.clone()), rhs: Box::new(rhs.clone()) }, span },
                self.loc(),
            )
        {
            return self.const_with_expected(v, if op.is_comparison() { None } else { expected }, span);
        }
        if matches!(op, ast::BinOp::And | ast::BinOp::Or) {
            return self.logical(op, lhs, rhs, span);
        }
        let operand_expected = if op.is_comparison() || op == ast::BinOp::Cmp { None } else { expected };
        if !is_untyped(lhs) {
            let l = self.expr(lhs, operand_expected);
            if op == ast::BinOp::Shl
                && let TyKind::Dynamic(elem) = self.types.kind(self.types.base(l.ty)).clone()
            {
                let void = self.types.void();
                if !super::members::is_place(&l) {
                    self.report(
                        Diagnostic::error(codes::NOT_ASSIGNABLE, "`<<` appends to a dynamic array variable or field")
                            .primary(lhs.span, "this is a temporary copy"),
                    );
                    return ir::Expr::new(ExprKind::Zero, void);
                }
                self.dyn_push(l, elem, rhs, span);
                return ir::Expr::new(ExprKind::Zero, void);
            }
            if let Some(elem) = self.numeric_array_elem(l.ty)
                && !op.is_comparison()
            {
                let rhs_expected = match *self.types.kind(self.types.base(l.ty)) {
                    TyKind::Matrix(e, _, c) if op == ast::BinOp::Mul && super::matrix::is_literal_of_len(rhs, c) => {
                        self.types.intern(TyKind::Array(e, u64::from(c)))
                    }
                    _ => l.ty,
                };
                let r = if is_untyped(rhs) { self.expr(rhs, Some(elem)) } else { self.expr(rhs, Some(rhs_expected)) };
                return self.binary_values(op, l, r, lhs.span, rhs.span, span);
            }
            if matches!(self.types.kind(self.types.base(l.ty)), TyKind::MultiPointer(_))
                && matches!(op, ast::BinOp::Add | ast::BinOp::Sub)
            {
                let int = self.types.int();
                let (l, r) =
                    self.sequenced(l, |this, _| this.expr(rhs, if is_untyped(rhs) { Some(int) } else { None }));
                return self.pointer_arith(op, l, r, span);
            }
            if let Some(decl) = self.operator_method(l.ty, op.as_str()) {
                let recv = self.address_of(l);
                let arg = ast::Arg { name: None, value: rhs.clone(), splat: false };
                let args = std::slice::from_ref(&arg);
                if let DeclKind::Overload(_) = self.decls[decl.0 as usize].kind {
                    let owner = self.types.base(recv.ty);
                    let owner = match self.types.kind(owner) {
                        TyKind::Pointer(t) => *t,
                        _ => owner,
                    };
                    return self.call_overloaded(decl, Some(recv), Some(owner), args, lhs.span.to(rhs.span), span);
                }
                return self.call_fn(decl, Some(recv), args, None, lhs.span.to(rhs.span), span);
            }
            let rhs_expected =
                if self.types.is_numeric(l.ty) { None } else { self.package_operand_type(op.as_str(), l.ty, true) };
            let (l, r) = self.sequenced(l, |this, l_ty| this.expr(rhs, Some(rhs_expected.unwrap_or(l_ty))));
            if let Some(decl) = self.package_operator(op.as_str(), l.ty, r.ty) {
                self.note_ref(span, decl, crate::uses::RefKind::Call);
                return self.call_operator_fn(decl, l, r);
            }
            return self.binary_values(op, l, r, lhs.span, rhs.span, span);
        }
        let (l, r) = if is_untyped(lhs) && !is_untyped(rhs) {
            let r = self.expr(rhs, operand_expected);
            let l_expected = match self.numeric_array_elem(r.ty) {
                Some(elem) => elem,
                None if !self.types.is_numeric(r.ty) => {
                    self.package_operand_type(op.as_str(), r.ty, false).unwrap_or(r.ty)
                }
                None => r.ty,
            };
            let l = self.expr(lhs, Some(l_expected));
            if let Some(decl) = self.package_operator(op.as_str(), l.ty, r.ty) {
                self.note_ref(span, decl, crate::uses::RefKind::Call);
                return self.call_operator_fn(decl, l, r);
            }
            (l, r)
        } else {
            let l = self.expr(lhs, operand_expected);
            let (l, r) = self.sequenced(l, |this, l_ty| this.expr(rhs, Some(l_ty)));
            (l, r)
        };
        self.binary_values(op, l, r, lhs.span, rhs.span, span)
    }

    /// Lowers `p + n` and `p - n` on a `[^]T` (moving by whole elements) and
    /// `p - q` between two of them (the distance in elements).
    fn pointer_arith(&mut self, op: ast::BinOp, l: ir::Expr, r: ir::Expr, span: Span) -> ir::Expr {
        let ir_op = if op == ast::BinOp::Add { ir::BinaryOp::Add } else { ir::BinaryOp::Sub };
        let r_kind = self.types.kind(self.types.base(r.ty)).clone();
        match r_kind {
            TyKind::Int(_) => {
                let ty = l.ty;
                ir::Expr::new(ExprKind::Binary { op: ir_op, lhs: Box::new(l), rhs: Box::new(r), span }, ty)
            }
            TyKind::MultiPointer(_) if op == ast::BinOp::Sub && self.types.base(r.ty) == self.types.base(l.ty) => {
                let int = self.types.int();
                ir::Expr::new(ExprKind::Binary { op: ir_op, lhs: Box::new(l), rhs: Box::new(r), span }, int)
            }
            TyKind::Unknown => ir::Expr::new(ExprKind::Zero, self.types.unknown()),
            _ => {
                let (ls, rs) = (self.types.display(l.ty), self.types.display(r.ty));
                self.report(
                    Diagnostic::error(codes::NO_OPERATOR, format!("cannot apply `{}` to `{ls}` and `{rs}`", op.as_str()))
                        .primary(span, "unsupported pointer arithmetic")
                        .note("a `[^]T` moves by whole elements with `p + n` and `p - n`, and `p - q` counts the elements between two of them"),
                );
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
        }
    }

    /// Lowers the right operand after the left one, spilling the left value
    /// when the right side has effects, so evaluation stays left to right.
    pub fn sequenced(
        &mut self,
        left: ir::Expr,
        right: impl FnOnce(&mut Self, TyId) -> ir::Expr,
    ) -> (ir::Expr, ir::Expr) {
        let left_ty = left.ty;
        self.begin_block();
        let r = right(self, left_ty);
        let stmts = self.end_block().stmts;
        let left = if (!stmts.is_empty() || !r.is_pure()) && !left.is_pure() { self.spill(left) } else { left };
        for s in stmts {
            self.emit(s);
        }
        (left, r)
    }

    /// Applies a binary operator to already-lowered operands.
    pub fn binary_values(
        &mut self,
        op: ast::BinOp,
        l: ir::Expr,
        r: ir::Expr,
        lspan: Span,
        rspan: Span,
        span: Span,
    ) -> ir::Expr {
        let bool_ty = self.types.bool();
        let unwrap_get = |e: ir::Expr| match e.kind {
            ExprKind::OptGet(inner) => *inner,
            _ => e,
        };
        let (l, r) = if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne)
            && (matches!(r.kind, ExprKind::Nil) || matches!(l.kind, ExprKind::Nil))
        {
            (unwrap_get(l), unwrap_get(r))
        } else {
            (l, r)
        };
        if matches!(op, ast::BinOp::Eq | ast::BinOp::Ne) {
            let nil_side = if matches!(r.kind, ExprKind::Nil) && self.types.is_nilable(l.ty) {
                Some(l.clone())
            } else if matches!(l.kind, ExprKind::Nil) && self.types.is_nilable(r.ty) {
                Some(r.clone())
            } else {
                None
            };
            if let Some(v) = nil_side {
                let some = self.is_some(v);
                return if op == ast::BinOp::Eq { self.not(some) } else { some };
            }
        }
        let unknown =
            matches!(self.types.kind(l.ty), TyKind::Unknown) || matches!(self.types.kind(r.ty), TyKind::Unknown);
        if unknown {
            let ty = if op.is_comparison() { bool_ty } else { self.types.unknown() };
            return ir::Expr::new(ExprKind::Zero, ty);
        }
        let ir_op = match op {
            ast::BinOp::Add => ir::BinaryOp::Add,
            ast::BinOp::Sub => ir::BinaryOp::Sub,
            ast::BinOp::Mul => ir::BinaryOp::Mul,
            ast::BinOp::Div => ir::BinaryOp::Div,
            ast::BinOp::Rem => ir::BinaryOp::Rem,
            ast::BinOp::BitAnd => ir::BinaryOp::BitAnd,
            ast::BinOp::BitOr => ir::BinaryOp::BitOr,
            ast::BinOp::BitXor => ir::BinaryOp::BitXor,
            ast::BinOp::Shl => ir::BinaryOp::Shl,
            ast::BinOp::Shr => ir::BinaryOp::Shr,
            ast::BinOp::Eq => ir::BinaryOp::Eq,
            ast::BinOp::Ne => ir::BinaryOp::Ne,
            ast::BinOp::Lt => ir::BinaryOp::Lt,
            ast::BinOp::Le => ir::BinaryOp::Le,
            ast::BinOp::Gt => ir::BinaryOp::Gt,
            ast::BinOp::Ge => ir::BinaryOp::Ge,
            ast::BinOp::Pow => ir::BinaryOp::Pow,
            ast::BinOp::Cmp => {
                let int = self.types.int();
                let comparable = l.ty == r.ty
                    && matches!(
                        self.types.kind(self.types.base(l.ty)),
                        TyKind::Int(_) | TyKind::Float(_) | TyKind::Rune | TyKind::String
                    );
                if !comparable {
                    let (ls, rs) = (self.types.display(l.ty), self.types.display(r.ty));
                    self.report(
                        Diagnostic::error(codes::NO_OPERATOR, format!("cannot compare `{ls}` and `{rs}` with `<=>`"))
                            .primary(span, "`<=>` works on numbers, runes and strings of the same type"),
                    );
                    return ir::Expr::new(ExprKind::Zero, int);
                }
                let (l, r) = if matches!(self.types.kind(self.types.base(l.ty)), TyKind::String) {
                    let i32_ty = self.types.i32();
                    let cmp =
                        ir::Expr::new(ExprKind::Builtin { op: ir::Builtin::StringCmp, args: vec![l, r], span }, i32_ty);
                    (self.spill(cmp), ir::Expr::new(ExprKind::Int(0), i32_ty))
                } else {
                    let l = if l.is_pure() { l } else { self.spill(l) };
                    let r = if r.is_pure() { r } else { self.spill(r) };
                    (l, r)
                };
                return ir::Expr::new(
                    ExprKind::Binary { op: ir::BinaryOp::Cmp, lhs: Box::new(l), rhs: Box::new(r), span },
                    int,
                );
            }
            ast::BinOp::And | ast::BinOp::Or => {
                return ir::Expr::new(ExprKind::Zero, self.types.unknown());
            }
        };
        if op == ast::BinOp::Mul
            && let Some(result) = self.matrix_product(l.ty, r.ty, span)
        {
            return ir::Expr::new(
                ExprKind::Binary { op: ir::BinaryOp::Mul, lhs: Box::new(l), rhs: Box::new(r), span },
                result,
            );
        }
        if let Some(result) = self.vector_result(op, l.ty, r.ty) {
            return ir::Expr::new(ExprKind::Binary { op: ir_op, lhs: Box::new(l), rhs: Box::new(r), span }, result);
        }
        if l.ty != r.ty {
            if self.optional_inner(l.ty) == Some(r.ty) {
                self.maybe_nil(&l, lspan);
                return ir::Expr::new(ExprKind::Zero, if op.is_comparison() { bool_ty } else { r.ty });
            }
            if self.optional_inner(r.ty) == Some(l.ty) {
                self.maybe_nil(&r, rspan);
                return ir::Expr::new(ExprKind::Zero, if op.is_comparison() { bool_ty } else { l.ty });
            }
            let (ls, rs) = (self.types.display(l.ty), self.types.display(r.ty));
            let mut diag =
                Diagnostic::error(codes::NO_OPERATOR, format!("cannot apply `{}` to `{ls}` and `{rs}`", op.as_str()))
                    .primary(span, format!("`{ls}` {} `{rs}`", op.as_str()))
                    .secondary(lspan, format!("`{ls}`"))
                    .secondary(rspan, format!("`{rs}`"));
            if self.types.is_numeric(l.ty) && self.types.is_numeric(r.ty) {
                let text = self.source_text(rspan);
                diag = conversion_help(diag, rspan, &text, &ls)
                    .note("both sides of an arithmetic operator must have the same type");
            }
            self.report(diag);
            let ty = if op.is_comparison() { bool_ty } else { self.types.unknown() };
            return ir::Expr::new(ExprKind::Zero, ty);
        }
        let ty = l.ty;
        let base = self.types.base(ty);
        let kind = self.types.kind(base).clone();
        if matches!(kind, TyKind::String)
            && matches!(ir_op, ir::BinaryOp::Lt | ir::BinaryOp::Le | ir::BinaryOp::Gt | ir::BinaryOp::Ge)
        {
            let i32_ty = self.types.i32();
            let cmp = ir::Expr::new(ExprKind::Builtin { op: ir::Builtin::StringCmp, args: vec![l, r], span }, i32_ty);
            let zero = ir::Expr::new(ExprKind::Int(0), i32_ty);
            return ir::Expr::new(
                ExprKind::Binary { op: ir_op, lhs: Box::new(cmp), rhs: Box::new(zero), span },
                bool_ty,
            );
        }
        let ok = match ir_op {
            ir::BinaryOp::Add
            | ir::BinaryOp::Sub
            | ir::BinaryOp::Mul
            | ir::BinaryOp::Div
            | ir::BinaryOp::Rem
            | ir::BinaryOp::Pow => matches!(kind, TyKind::Int(_) | TyKind::Float(_)),
            ir::BinaryOp::BitAnd
            | ir::BinaryOp::BitOr
            | ir::BinaryOp::BitXor
            | ir::BinaryOp::Shl
            | ir::BinaryOp::Shr => {
                matches!(kind, TyKind::Int(_))
                    || (matches!(kind, TyKind::Bool)
                        && matches!(ir_op, ir::BinaryOp::BitAnd | ir::BinaryOp::BitOr | ir::BinaryOp::BitXor))
            }
            ir::BinaryOp::Eq | ir::BinaryOp::Ne => {
                self.is_comparable(base)
                    || matches!(
                        kind,
                        TyKind::Int(_)
                            | TyKind::Float(_)
                            | TyKind::Bool
                            | TyKind::Rune
                            | TyKind::Enum(_)
                            | TyKind::Pointer(_)
                            | TyKind::RawPtr
                            | TyKind::Error
                    )
            }
            ir::BinaryOp::Lt | ir::BinaryOp::Le | ir::BinaryOp::Gt | ir::BinaryOp::Ge => {
                matches!(kind, TyKind::Int(_) | TyKind::Float(_) | TyKind::Rune)
            }
            ir::BinaryOp::And | ir::BinaryOp::Or | ir::BinaryOp::Cmp => false,
        };
        if !ok {
            let shown = self.types.display(ty);
            let mut diag =
                Diagnostic::error(codes::NO_OPERATOR, format!("`{}` is not defined for `{shown}`", op.as_str()))
                    .primary(span, format!("both sides are `{shown}`"));
            if matches!(kind, TyKind::String) && op == ast::BinOp::Add {
                diag = diag.help("build strings with interpolation: `\"#{a}#{b}\"`");
            } else if matches!(kind, TyKind::Struct(_)) {
                let ret = if op.is_comparison() { "Bool".to_string() } else { shown.clone() };
                diag = diag
                    .help(format!("define the operator on `{shown}`: `def {}(other: {shown}) -> {ret}`", op.as_str()));
            }
            self.report(diag);
        }
        let result_ty = if op.is_comparison() { bool_ty } else { ty };
        ir::Expr::new(ExprKind::Binary { op: ir_op, lhs: Box::new(l), rhs: Box::new(r), span }, result_ty)
    }

    /// Returns the element type of a numeric fixed array.
    pub fn numeric_array_elem(&self, ty: TyId) -> Option<TyId> {
        match self.types.kind(self.types.base(ty)) {
            TyKind::Array(e, _) | TyKind::Matrix(e, _, _) if self.types.is_numeric(*e) => Some(*e),
            _ => None,
        }
    }

    /// Returns the result type of element-wise arithmetic between numeric
    /// arrays, or an array and a scalar of its element type.
    fn vector_result(&self, op: ast::BinOp, l: TyId, r: TyId) -> Option<TyId> {
        if !matches!(
            op,
            ast::BinOp::Add
                | ast::BinOp::Sub
                | ast::BinOp::Mul
                | ast::BinOp::Div
                | ast::BinOp::Rem
                | ast::BinOp::BitAnd
                | ast::BinOp::BitOr
                | ast::BinOp::BitXor
        ) {
            return None;
        }
        let matrix = |t: TyId| matches!(self.types.kind(self.types.base(t)), TyKind::Matrix(..));
        if (matrix(l) || matrix(r))
            && !(matches!(op, ast::BinOp::Add | ast::BinOp::Sub) && l == r
                || matches!(op, ast::BinOp::Mul) && (self.types.is_numeric(l) || self.types.is_numeric(r))
                || matches!(op, ast::BinOp::Div) && self.types.is_numeric(r))
        {
            return None;
        }
        match (self.numeric_array_elem(l), self.numeric_array_elem(r)) {
            (Some(_), Some(_)) if l == r => Some(l),
            (Some(e), None) if e == r => Some(l),
            (None, Some(e)) if e == l => Some(r),
            _ => None,
        }
    }

    /// Finds a user-defined operator method on the left operand's type.
    pub fn operator_method(&mut self, ty: TyId, op: &str) -> Option<super::DeclId> {
        let base = match self.types.kind(ty) {
            TyKind::Pointer(inner) => *inner,
            _ => ty,
        };
        if !matches!(self.types.kind(base), TyKind::Struct(_) | TyKind::Enum(_)) {
            return None;
        }
        let decl = self.find_method(base, Name::new(op))?;
        matches!(self.decls[decl.0 as usize].kind, DeclKind::Fn(f) if !f.is_static)
            .then_some(decl)
            .or_else(|| matches!(self.decls[decl.0 as usize].kind, DeclKind::Overload(_)).then_some(decl))
    }

    /// Calls a package-level operator function, like `def *(s: F32, v: Vec2)`,
    /// with operands that match its parameter types exactly.
    pub fn call_operator_fn(&mut self, decl: super::DeclId, l: ir::Expr, r: ir::Expr) -> ir::Expr {
        let sig = self.fn_sig(decl);
        let func = self.fn_instance(decl);
        ir::Expr::new(ExprKind::Call { func, args: vec![l, r] }, sig.ret)
    }

    /// Returns true when `==` compares values of the type member by member.
    pub fn is_comparable(&self, ty: TyId) -> bool {
        match self.types.kind(self.types.base(ty)) {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Rune
            | TyKind::String
            | TyKind::Enum(_)
            | TyKind::Error
            | TyKind::Pointer(_)
            | TyKind::MultiPointer(_)
            | TyKind::RawPtr
            | TyKind::TypeId
            | TyKind::Type
            | TyKind::Symbol => true,
            TyKind::Struct(id) => self.types.struct_info(*id).fields.iter().all(|f| self.is_comparable(f.ty)),
            TyKind::Array(elem, _) | TyKind::Matrix(elem, _, _) => self.is_comparable(*elem),
            TyKind::Tuple(elems) => elems.iter().all(|e| self.is_comparable(*e)),
            TyKind::Optional(inner) => self.is_comparable(*inner),
            _ => false,
        }
    }

    fn logical(&mut self, op: ast::BinOp, lhs: &ast::Expr, rhs: &ast::Expr, span: Span) -> ir::Expr {
        let bool_ty = self.types.bool();
        let l = if op == ast::BinOp::Or { self.nilable_operand(lhs) } else { self.expr(lhs, None) };
        if op == ast::BinOp::Or && self.optional_inner(l.ty).is_some() {
            return self.or_default(l, rhs, span);
        }
        let l = self.truthy(l, lhs.span);
        let operand = Operand { kind: OperandKind::Logical { or: op == ast::BinOp::Or }, span: rhs.span };
        let (stmts, r) = self.lower_operand(operand, |this| {
            let r = this.expr(rhs, Some(bool_ty));
            this.truthy(r, rhs.span)
        });
        let ir_op = if op == ast::BinOp::And { ir::BinaryOp::And } else { ir::BinaryOp::Or };
        if stmts.is_empty() {
            return ir::Expr::new(ExprKind::Binary { op: ir_op, lhs: Box::new(l), rhs: Box::new(r), span }, bool_ty);
        }
        // The right side needs statements, so short-circuit with a branch.
        let result = self.new_local(None, bool_ty);
        self.emit(Stmt::Let { local: result, init: Some(l) });
        let result_e = ir::Expr::new(ExprKind::Local(result), bool_ty);
        let mut then = stmts;
        then.push(Stmt::Assign { target: result_e.clone(), value: r });
        let cond = if op == ast::BinOp::And { result_e.clone() } else { self.not(result_e.clone()) };
        self.emit(Stmt::If { cond, then: ir::Block { stmts: then }, else_: ir::Block::default() });
        result_e
    }

    fn ternary(
        &mut self,
        cond: &ast::Expr,
        then: &ast::Expr,
        else_: &ast::Expr,
        expected: Option<TyId>,
        span: Span,
    ) -> ir::Expr {
        let if_expr = ast::IfExpr {
            cond: ast::Cond::Expr(cond.clone()),
            then: vec![ast::Stmt { kind: ast::StmtKind::Expr(then.clone()), span: then.span, attrs: Vec::new() }],
            elifs: Vec::new(),
            else_: Some(vec![ast::Stmt {
                kind: ast::StmtKind::Expr(else_.clone()),
                span: else_.span,
                attrs: Vec::new(),
            }]),
            unless: false,
        };
        self.if_value(&if_expr, expected, span)
    }

    /// Returns the storage of a variable as a place: the local itself, or
    /// what it points to for by-reference bindings.
    pub fn var_place(&mut self, local: crate::ir::LocalId, ty: TyId, indirect: bool) -> ir::Expr {
        if indirect {
            let ptr = self.local_ty(local);
            return ir::Expr::new(ExprKind::Deref(Box::new(ir::Expr::new(ExprKind::Local(local), ptr))), ty);
        }
        ir::Expr::new(ExprKind::Local(local), ty)
    }

    /// Lowers an operand that is tested for nil. A narrowed optional local is
    /// read as the optional itself, not as its unwrapped value.
    pub fn nilable_operand(&mut self, e: &ast::Expr) -> ir::Expr {
        if let E::Ident(n) = e.kind
            && let Some(var) = self.find_var_at(n, e.span)
        {
            let (local, ty, indirect) = (var.local, var.ty, var.indirect);
            if self.types.is_nilable(ty) {
                if let Some(var) = self.find_var_at(n, e.span) {
                    var.read = true;
                }
                return self.var_place(local, ty, indirect);
            }
        }
        self.expr(e, None)
    }

    /// Lowers `&expr`: a pointer to a variable, field or element.
    fn address_of_expr(&mut self, inner: &ast::Expr, span: Span) -> ir::Expr {
        if let E::Ident(name) = inner.kind
            && let Some(var) = self.find_var_at(name, inner.span)
        {
            var.address_taken = true;
            var.read = true;
            let (local, ty, indirect) = (var.local, var.ty, var.indirect);
            self.invalidate(local);
            if indirect {
                let ptr = self.local_ty(local);
                return ir::Expr::new(ExprKind::Local(local), ptr);
            }
            let ptr = self.types.pointer(ty);
            return ir::Expr::new(ExprKind::AddrOf(Box::new(ir::Expr::new(ExprKind::Local(local), ty))), ptr);
        }
        let v = self.expr(inner, None);
        if matches!(self.types.kind(v.ty), TyKind::Unknown) {
            return v;
        }
        if !super::members::is_place(&v) {
            self.report(
                Diagnostic::error(codes::NOT_ASSIGNABLE, "cannot take the address of a temporary value")
                    .primary(span, "`&` needs a variable, field or element")
                    .help("store the value in a variable first, then take its address"),
            );
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        if self.report_type_table_address(&v, span) {
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        let ptr = self.types.pointer(v.ty);
        ir::Expr::new(ExprKind::AddrOf(Box::new(v)), ptr)
    }

    /// Lowers a `case` used as a value into a temporary.
    fn case_value(&mut self, case: &ast::CaseExpr, expected: Option<TyId>, span: Span) -> ir::Expr {
        let ty = expected.unwrap_or_else(|| self.types.unknown());
        let local = self.new_local(None, ty);
        self.emit(Stmt::Let { local, init: None });
        self.lower_case(case, Dest::Local(local, ty), span);
        let ty = self.local_ty(local);
        ir::Expr::new(ExprKind::Local(local), ty)
    }

    /// Lowers an `if` used as a value into a temporary.
    fn if_value(&mut self, if_expr: &ast::IfExpr, expected: Option<TyId>, span: Span) -> ir::Expr {
        let ty = match expected {
            Some(t) => t,
            None => self.types.unknown(),
        };
        let local = self.new_local(None, ty);
        self.emit(Stmt::Let { local, init: None });
        self.lower_if(if_expr, Dest::Local(local, ty), span);
        let ty = self.local_ty(local);
        ir::Expr::new(ExprKind::Local(local), ty)
    }

    // ----- calls ---------------------------------------------------------

    fn call(&mut self, call: &ast::Call, expected: Option<TyId>, span: Span) -> ir::Expr {
        let block = call.block.as_ref();
        match &call.callee {
            ast::Callee::Name(name) => {
                if self.find_var_at(name.name, name.span).is_some() {
                    let callee = self.ident(name.name, name.span, None);
                    if let Some(b) = block {
                        self.reject_block(b, &format!("procs like `{}` cannot take a block", name.as_str()));
                    }
                    return self.call_proc(callee, &call.args, span);
                }
                if let Some(v) = self.implicit_self_call(name.name, &call.args, block, name.span, span) {
                    return v;
                }
                let loc = self.loc_at(name.span);
                if let Some(decl) = self.lookup_pkg(loc.pkg, name.name).or_else(|| self.lookup_prelude(name.name)) {
                    if self.is_macro(decl) {
                        self.check_visible(decl, name.span);
                        let shown = name.as_str().to_string();
                        let (name_span, args) = (name.span, &call.args);
                        let call = MacroCall { decl, shown, args, block, name_span, span };
                        return self.call_macro(call, expected);
                    }
                    if let DeclKind::Overload(_) = self.decls[decl.0 as usize].kind {
                        if let Some(b) = block {
                            self.reject_block(b, "overloaded methods do not take blocks");
                        }
                        let spans = (name.span, span);
                        return self.call_package_set(decl, name.as_str(), &call.args, spans, expected);
                    }
                    if let DeclKind::Fn(_) = self.decls[decl.0 as usize].kind {
                        return self.call_fn(decl, None, &call.args, block, name.span, span);
                    }
                    let what = self.decls[decl.0 as usize].kind.a_describe();
                    let mut diag =
                        Diagnostic::error(codes::NOT_CALLABLE, format!("`{}` is {what}, not a method", name.as_str()))
                            .primary(name.span, "cannot be called");
                    if let DeclKind::Struct(s) = self.decls[decl.0 as usize].kind
                        && !s.generics.is_empty()
                    {
                        // `Local(Int)` names an instance of a generic struct.
                        let text = self.source_text(span);
                        let shape: Vec<&str> = s.generics.iter().map(|g| g.name.as_str()).collect();
                        diag = if call.args.is_empty() || block.is_some() {
                            diag.help(format!("build a value with `{}({}).new`", name.as_str(), shape.join(", ")))
                        } else {
                            diag.suggest_replace(
                                format!("`{text}` is a type: build a value of it with `new`"),
                                span,
                                format!("{text}.new"),
                                Applicability::MachineApplicable,
                            )
                        };
                    } else if matches!(self.decls[decl.0 as usize].kind, DeclKind::Struct(_)) {
                        diag = diag.suggest_replace(
                            "build a value with `new`",
                            name.span,
                            format!("{}.new", name.as_str()),
                            Applicability::MachineApplicable,
                        );
                    } else if matches!(self.decls[decl.0 as usize].kind, DeclKind::Enum(_))
                        && let [arg] = call.args.as_slice()
                    {
                        let text = self.source_text(arg.value.span);
                        let operand = if super::items::is_simple_operand(&text) { text } else { format!("({text})") };
                        diag = diag.suggest_replace(
                            "convert a number to the enum with `.to`",
                            span,
                            format!("{operand}.to({})", name.as_str()),
                            Applicability::MachineApplicable,
                        );
                    }
                    self.report(diag);
                    return ir::Expr::new(ExprKind::Zero, self.types.unknown());
                }
                if BUILTINS.contains(&name.as_str()) {
                    if let Some(b) = block {
                        self.reject_block(b, &format!("`{}` does not take a block", name.as_str()));
                    }
                    return self.builtin_call(name.name, &call.args, span, expected);
                }
                if let Some((owner, is_proc)) = self.self_field(name.name) {
                    if !self.declared_by_failed_macro(false, true) {
                        let (args, parens, block) = (!call.args.is_empty(), call.parens, block.is_some());
                        let field_call = FieldCall { span, args, parens, block, is_proc };
                        self.undefined_field_name(name.name, name.span, owner, Some(field_call));
                    }
                    for arg in &call.args {
                        self.expr(&arg.value, None);
                    }
                    return ir::Expr::new(ExprKind::Zero, self.types.unknown());
                }
                let own = self.self_names(true);
                let mut candidates = own.methods.clone();
                candidates.extend(self.package_names(loc.pkg));
                candidates.extend(BUILTINS.iter().copied());
                self.undefined_call(*name, &call.args, span, &candidates, &own)
            }
            ast::Callee::Method { recv, name, safe } => {
                self.member_call(recv, *name, Some(&call.args), block, *safe, span, expected)
            }
            ast::Callee::IVar(name) => self.ivar_call(*name, &call.args, block, span),
        }
    }

    /// Reports a block passed where none is accepted.
    pub fn reject_block(&mut self, block: &ast::BlockArg, message: &str) {
        self.report(
            Diagnostic::error(codes::BLOCK_MISMATCH, message.to_string())
                .primary(block.span, "this block is never called")
                .help("methods receive blocks by declaring `&blk: block(…)` and calling `yield`"),
        );
    }

    /// Calls a function declaration; `receiver` is the `^Self` pointer for
    /// instance methods.
    pub fn call_fn(
        &mut self,
        decl: super::DeclId,
        receiver: Option<ir::Expr>,
        args: &[ast::Arg],
        block: Option<&ast::BlockArg>,
        name_span: Span,
        span: Span,
    ) -> ir::Expr {
        let fname = self.decls[decl.0 as usize].name;
        self.check_visible(decl, name_span);
        self.note_ref(name_span, decl, crate::uses::RefKind::Call);
        if self.is_macro(decl) {
            let call = MacroCall { decl, shown: fname.to_string(), args, block, name_span, span };
            return self.call_macro(call, None);
        }
        let sig = self.fn_sig(decl);
        if let Some(init) = self.const_init {
            self.report(
                Diagnostic::error(codes::COMPTIME_ONLY, format!("a constant can't call `{fname}` without `comptime`"))
                    .primary(name_span, "this method would run while compiling")
                    .note("constants are computed at compile time; `comptime` marks code that runs then")
                    .suggest(
                        "run the initializer with `comptime`",
                        vec![super::comptime::insert_comptime(init)],
                        Applicability::MachineApplicable,
                    ),
            );
            self.const_init = None;
        }
        match (&sig.block, block) {
            (Some(_), Some(_)) => {}
            (Some(bsig), None) => {
                let params: Vec<String> = bsig.params.iter().map(|t| self.types.display(*t)).collect();
                let names: Vec<String> = (0..params.len())
                    .map(|i| ["x", "y", "z", "w"].get(i).map_or_else(|| format!("p{i}"), |s| s.to_string()))
                    .collect();
                let bars = if names.is_empty() { String::new() } else { format!(" |{}|", names.join(", ")) };
                self.report(
                    Diagnostic::error(codes::BLOCK_MISMATCH, format!("`{fname}` needs a block"))
                        .primary(
                            name_span,
                            format!(
                                "it yields {}",
                                if params.is_empty() { "nothing".to_string() } else { params.join(", ") }
                            ),
                        )
                        .suggest(
                            "pass a block",
                            vec![wid_diagnostics::Edit {
                                span: span.shrink_to_end(),
                                replacement: {
                                    let indent = self.indent_at(span);
                                    format!(" do{bars}\n{indent}  # …\n{indent}end")
                                },
                            }],
                            Applicability::HasPlaceholders,
                        ),
                );
                return ir::Expr::new(ExprKind::Zero, sig.ret);
            }
            // A method that yields without declaring its block was reported
            // there (E0320); the block passed here is what it meant to take.
            (None, Some(_)) if matches!(self.decls[decl.0 as usize].kind, DeclKind::Fn(f) if f.yields) => {}
            (None, Some(b)) => self.reject_block(b, &format!("`{fname}` does not take a block")),
            (None, None) => {}
        }
        let generic = !self.generic_names(decl).is_empty();
        let mut bindings = std::mem::take(&mut self.owner_bindings);
        if let (Some(r), Some(recv_ty)) = (&receiver, sig.receiver) {
            let actual = match self.types.kind(r.ty) {
                TyKind::Pointer(t) => *t,
                _ => r.ty,
            };
            self.unify(recv_ty, actual, &mut bindings);
        }
        let errors_before_args = self.diags.error_count();
        let ordered = self.match_args(fname, &sig.params, args, name_span, span, decl);
        let mut lowered: Vec<ir::Expr> = receiver.into_iter().collect();
        for (i, param) in sig.params.iter().enumerate() {
            let value = match &ordered[i] {
                ArgSource::Given(e) => {
                    // `xs: [N]T` needs the value bound to `N` so far.
                    let pattern = if sig.per_instance {
                        self.fn_sig_inst(decl, &bindings).params.get(i).map_or(param.ty, |p| p.ty)
                    } else {
                        param.ty
                    };
                    let expected = if generic {
                        let t = self.subst_type(pattern, &bindings);
                        if self.has_params(t) { None } else { Some(t) }
                    } else {
                        Some(param.ty)
                    };
                    self.begin_block();
                    let v = match expected {
                        Some(t) => self.expr_coerced(e, t),
                        None => {
                            let v = self.expr(e, None);
                            let vt = self.value_type(v.ty, e.span);
                            let v = ir::Expr::new(v.kind, vt);
                            if !self.unify(pattern, v.ty, &mut bindings) {
                                let want = self.types.display(pattern);
                                let found = self.types.display(v.ty);
                                self.report(
                                    Diagnostic::error(
                                        codes::TYPE_MISMATCH,
                                        format!("expected `{want}`, found `{found}`"),
                                    )
                                    .primary(e.span, format!("this has type `{found}`"))
                                    .note("the parameter's type parameters must match the argument's type"),
                                );
                            }
                            v
                        }
                    };
                    let stmts = self.end_block().stmts;
                    if !stmts.is_empty() || !v.is_pure() {
                        self.spill_impure(&mut lowered);
                    }
                    for s in stmts {
                        self.emit(s);
                    }
                    v
                }
                ArgSource::Default(v) => v.clone(),
                ArgSource::Missing => ir::Expr::new(ExprKind::Zero, param.ty),
            };
            lowered.push(value);
        }
        if sig.c_variadic {
            for arg in args.iter().filter(|a| a.name.is_none()).skip(sig.params.len()) {
                self.begin_block();
                let v = self.variadic_arg(&arg.value);
                let stmts = self.end_block().stmts;
                if !stmts.is_empty() || !v.is_pure() {
                    self.spill_impure(&mut lowered);
                }
                for s in stmts {
                    self.emit(s);
                }
                lowered.push(v);
            }
        }
        let (subst, inst) = if generic {
            // An argument that failed to check (like an untyped `[]`) already
            // explains why a type parameter is unknown.
            if self.diags.error_count() > errors_before_args
                && self.generic_names(decl).iter().any(|n| super::generics::lookup(&bindings, *n).is_none())
            {
                return ir::Expr::new(ExprKind::Zero, self.types.unknown());
            }
            let Some(subst) = self.finish_bindings(decl, bindings, name_span) else {
                return ir::Expr::new(ExprKind::Zero, self.types.unknown());
            };
            let inst = self.fn_sig_inst(decl, &subst);
            // `def dup(x: $T) -> [4]T` with a large `T`: the instance would
            // return a type over the size limit.
            if self.report_too_large_return(inst.ret, fname, span) {
                return ir::Expr::new(ExprKind::Zero, self.types.unknown());
            }
            let offset = lowered.len() - inst.params.len();
            let d = self.decls[decl.0 as usize].clone();
            let ast_params: &[ast::Param] = match d.kind {
                super::DeclKind::Fn(f) => &f.params,
                _ => &[],
            };
            for (i, p) in inst.params.iter().enumerate() {
                if matches!(ordered[i], ArgSource::Default(_))
                    && let Some(default) = ast_params.get(i).and_then(|ap| ap.default.as_ref())
                {
                    lowered[offset + i] = self.default_value(default, p.ty, d.loc, span);
                    continue;
                }
                let v = std::mem::replace(&mut lowered[offset + i], ir::Expr::new(ExprKind::Zero, p.ty));
                lowered[offset + i] = self.coerce(v, p.ty, span);
            }
            let existing = self.fn_insts.keys().filter(|(k, _)| *k == decl).count();
            let key: Vec<TyId> = subst.iter().map(|(_, t)| *t).collect();
            if existing >= 512 && !self.fn_insts.contains_key(&(decl, key)) {
                self.report(
                    Diagnostic::error(codes::GENERIC_ARGS, format!("`{fname}` was instantiated with too many different types"))
                        .primary(name_span, "this looks like unbounded recursion through type arguments")
                        .help("a generic method that calls itself with a bigger type (like `f([x])`) never stops instantiating"),
                );
                return ir::Expr::new(ExprKind::Zero, self.types.unknown());
            }
            (subst, inst)
        } else {
            (Default::default(), sig)
        };
        if let (Some(b), Some(_)) = (block, &inst.block) {
            return self.inline_call(decl, subst, lowered, b, name_span, span);
        }
        let func = self.fn_instance_with(decl, subst, span);
        ir::Expr::new(ExprKind::Call { func, args: lowered }, inst.ret)
    }

    /// Lowers an argument passed through C's `...`. Untyped numbers become
    /// `C.int` and `C.double`, as in C; values C can't receive are errors.
    fn variadic_arg(&mut self, e: &ast::Expr) -> ir::Expr {
        let v = match e.kind {
            E::Int(_) => {
                let int = self.types.intern(TyKind::Int(crate::types::IntTy::I32));
                self.expr_coerced(e, int)
            }
            E::Float(_) => {
                let double = self.types.intern(TyKind::Float(crate::types::FloatTy::F64));
                self.expr_coerced(e, double)
            }
            E::Str(_) => {
                let cstring = self.types.intern(TyKind::CString);
                self.expr_coerced(e, cstring)
            }
            _ => self.expr(e, None),
        };
        let base = self.types.base(v.ty);
        let ok = match self.types.kind(base) {
            TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::Rune
            | TyKind::Enum(_)
            | TyKind::Pointer(_)
            | TyKind::MultiPointer(_)
            | TyKind::CString
            | TyKind::RawPtr
            | TyKind::Struct(_)
            | TyKind::Unknown => true,
            TyKind::Proc(sig) => sig.abi == crate::types::Abi::C,
            TyKind::Optional(_) => self.types.optional_is_pointer(base),
            _ => false,
        };
        if !ok {
            let shown = self.types.display(v.ty);
            let mut diag = Diagnostic::error(codes::C_VARIADIC, format!("`{shown}` can't be passed through C's `...`"))
                .primary(e.span, format!("this is `{shown}`"))
                .note("C variadic arguments take numbers, pointers, C strings and C structs");
            diag = if matches!(self.types.kind(base), TyKind::String) {
                diag.suggest(
                    "pass a C string",
                    vec![wid_diagnostics::Edit { span: e.span.shrink_to_end(), replacement: ".to_cstr".into() }],
                    Applicability::MaybeIncorrect,
                )
            } else {
                diag.help("pass a pointer to it, or convert it to a number")
            };
            self.report(diag);
        }
        v
    }

    /// Lowers a parameter's default value, written in the callee's package,
    /// for a parameter of type `ty`.
    /// `caller_location` as a default becomes the location of `call_span`.
    pub fn default_value(&mut self, default: &ast::Expr, ty: TyId, loc: super::DeclLoc, call_span: Span) -> ir::Expr {
        if let E::Ident(n) = default.kind
            && n.as_str() == "caller_location"
            && ty == self.types.location_ty
        {
            return ir::Expr::new(
                ExprKind::Builtin { op: Builtin::CallerLocation, args: Vec::new(), span: call_span },
                ty,
            );
        }
        if self.has_params(ty) {
            return ir::Expr::new(ExprKind::Zero, ty);
        }
        if let E::Comptime(_) = default.kind {
            return match self.interpret_const(default, Some(ty), loc) {
                Some(v) => self.coerce(v, ty, default.span),
                None => ir::Expr::new(ExprKind::Zero, self.types.unknown()),
            };
        }
        match self.fold_const(default, loc) {
            Some(c) => {
                let v = self.const_with_expected(c, Some(ty), default.span);
                self.coerce(v, ty, default.span)
            }
            None => self.lower_in_package(default, ty, loc),
        }
    }

    pub(crate) fn match_args<'e>(
        &mut self,
        fname: Name,
        params: &[super::ParamSig],
        args: &'e [ast::Arg],
        name_span: Span,
        span: Span,
        decl: super::DeclId,
    ) -> Vec<ArgSource<'e>> {
        let mut slots: Vec<ArgSource<'e>> = vec![ArgSource::Missing; params.len()];
        let mut positional = 0usize;
        let mut extra = Vec::new();
        let mut unknown_named = false;
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Fn(f) = d.kind else { unreachable!() };
        // A `*` parameter outside a macro was reported by the parser (E0112);
        // it takes the remaining positional arguments, which are not checked,
        // so its calls add no errors of their own.
        let splat_at = f.params.iter().position(|p| p.splat);
        for arg in args {
            if arg.splat {
                let text = self.source_text(arg.value.span);
                self.report(
                    Diagnostic::error(codes::ARG_COUNT, "Wid has no argument spreading")
                        .primary(arg.value.span, "`*` cannot spread a collection into arguments")
                        .note("every method takes a fixed number of arguments")
                        .help(format!("pass `{text}` as one argument to a parameter of type `[]T`, or pass the elements one by one")),
                );
            }
            match arg.name {
                None if splat_at.is_some_and(|at| positional >= at) => {}
                None => {
                    if positional < params.len() {
                        slots[positional] = ArgSource::Given(&arg.value);
                        positional += 1;
                    } else {
                        extra.push(arg.value.span);
                    }
                }
                Some(n) => match params.iter().position(|p| p.name == n.name) {
                    Some(i) => {
                        if matches!(slots[i], ArgSource::Given(_)) {
                            self.report(
                                Diagnostic::error(
                                    codes::BAD_NAMED_ARG,
                                    format!("argument `{}` is given twice", n.as_str()),
                                )
                                .primary(n.span, "already passed"),
                            );
                        }
                        slots[i] = ArgSource::Given(&arg.value);
                    }
                    None => {
                        unknown_named = true;
                        let names: Vec<&'static str> = params.iter().map(|p| p.name.as_str()).collect();
                        let mut diag = Diagnostic::error(
                            codes::BAD_NAMED_ARG,
                            format!("`{fname}` has no parameter named `{}`", n.as_str()),
                        )
                        .primary(n.span, "unknown parameter");
                        if let Some(best) = did_you_mean(n.as_str(), names.iter().copied()) {
                            diag = diag.suggest_replace(
                                format!("did you mean `{best}`?"),
                                n.span,
                                best,
                                Applicability::MaybeIncorrect,
                            );
                        } else if !names.is_empty() {
                            diag = diag.note(format!("parameters: {}", names.join(", ")));
                        }
                        self.report(diag);
                    }
                },
            }
        }
        let mut missing = Vec::new();
        for (i, p) in params.iter().enumerate() {
            if matches!(slots[i], ArgSource::Missing) && splat_at == Some(i) {
                slots[i] = ArgSource::Default(ir::Expr::new(ExprKind::Zero, p.ty));
            } else if matches!(slots[i], ArgSource::Missing) {
                match f.params.get(i).and_then(|ap| ap.default.as_ref()) {
                    Some(default) => {
                        let v = self.default_value(default, p.ty, d.loc, span);
                        slots[i] = ArgSource::Default(v);
                    }
                    None => missing.push(i),
                }
            }
        }
        if f.c_variadic.is_some() {
            extra.clear();
        }
        if !extra.is_empty() || (!missing.is_empty() && !unknown_named) {
            let sig_text = self.signature_text(fname, params);
            let want = params.len();
            let diag = if !extra.is_empty() {
                let given = args.iter().filter(|a| a.name.is_none()).count();
                Diagnostic::error(
                    codes::ARG_COUNT,
                    format!(
                        "`{fname}` takes {want} argument{}, but {given} {} given",
                        plural(want),
                        if given == 1 { "was" } else { "were" }
                    ),
                )
                .primary(
                    extra[0].to(*extra.last().unwrap_or(&extra[0])),
                    if extra.len() == 1 {
                        "unexpected argument".to_string()
                    } else {
                        format!("{} unexpected arguments", extra.len())
                    },
                )
            } else {
                let names: Vec<String> = missing.iter().map(|i| format!("`{}`", params[*i].name)).collect();
                let what = if names.len() == 1 { "argument" } else { "arguments" };
                let names_text = names.join(", ");
                Diagnostic::error(codes::ARG_COUNT, format!("missing {what} {names_text} in call to `{fname}`"))
                    .primary(span, format!("needs {names_text}"))
            };
            let _ = name_span;
            self.report(diag.secondary(d.span, format!("defined as `{sig_text}`")));
        }
        slots
    }

    /// Formats a signature like `add(a: Int, b: Int)`.
    pub fn signature_text(&self, name: Name, params: &[super::ParamSig]) -> String {
        let ps: Vec<String> = params.iter().map(|p| format!("{}: {}", p.name, self.types.display(p.ty))).collect();
        format!("{name}({})", ps.join(", "))
    }

    fn builtin_call(&mut self, name: Name, args: &[ast::Arg], span: Span, expected: Option<TyId>) -> ir::Expr {
        let void = self.types.void();
        match name.as_str() {
            "puts" | "print" | "p" => {
                let newline = name.as_str() != "print";
                let inspect = name.as_str() == "p";
                if args.is_empty() && newline {
                    let empty = ir::Expr::new(ExprKind::Str(String::new()), self.types.string());
                    return ir::Expr::new(
                        ExprKind::Builtin { op: Builtin::Print { newline, inspect: false }, args: vec![empty], span },
                        void,
                    );
                }
                let mut last = None;
                for (i, arg) in args.iter().enumerate() {
                    if let Some(n) = arg.name {
                        let name_part = Span { end: arg.value.span.start, ..n.span };
                        self.report(
                            Diagnostic::error(codes::BAD_NAMED_ARG, format!("`{name}` takes no named arguments"))
                                .primary(n.span, format!("`{name}` has no parameter called `{}`", n.as_str()))
                                .suggest_replace(
                                    "pass the value by position",
                                    name_part,
                                    "",
                                    Applicability::MaybeIncorrect,
                                ),
                        );
                    }
                    let v = self.expr(&arg.value, None);
                    let ty = self.value_type(v.ty, arg.value.span);
                    let v = ir::Expr::new(v.kind, ty);
                    let call = ir::Expr::new(
                        ExprKind::Builtin { op: Builtin::Print { newline, inspect }, args: vec![v], span },
                        void,
                    );
                    if i + 1 < args.len() {
                        self.emit(Stmt::Expr(call));
                    } else {
                        last = Some(call);
                    }
                }
                last.unwrap_or_else(|| ir::Expr::new(ExprKind::Zero, void))
            }
            "method" => self.method_ref(args, span, expected),
            "caller_location" => {
                if let Some(a) = args.first() {
                    self.report(
                        Diagnostic::error(codes::ARG_COUNT, "`caller_location` takes no arguments")
                            .primary(a.value.span, "remove this argument"),
                    );
                }
                let ty = self.types.location_ty;
                ir::Expr::new(ExprKind::Builtin { op: Builtin::CallerLocation, args: Vec::new(), span }, ty)
            }
            "alloc" => self.builtin_alloc(args, span),
            "embed" => self.builtin_embed(args, span),
            "config" => self.builtin_config(args, span, expected),
            "free" => self.builtin_free(args, span),
            "free_all" => self.builtin_free_all(args, span),
            "size_of" => self.builtin_size(args, span, false),
            "type_info" => self.builtin_type_info(args, span),
            "align_of" => self.builtin_size(args, span, true),
            "context" => {
                if let Some(a) = args.first() {
                    self.report(
                        Diagnostic::error(codes::NOT_CALLABLE, "`context` is not a method")
                            .primary(a.value.span, "remove the arguments"),
                    );
                }
                self.context_place()
            }
            "panic" => {
                let string = self.types.string();
                let msg = match args {
                    [arg] => self.expr_coerced(&arg.value, string),
                    _ => {
                        self.report(
                            Diagnostic::error(codes::ARG_COUNT, "`panic` takes one message argument")
                                .primary(span, "like `panic(\"unexpected state\")`"),
                        );
                        ir::Expr::new(ExprKind::Str("panic".into()), string)
                    }
                };
                let never = self.types.never();
                ir::Expr::new(ExprKind::Builtin { op: Builtin::Panic, args: vec![msg], span }, never)
            }
            "unreachable" => {
                let string = self.types.string();
                let msg = ir::Expr::new(ExprKind::Str("reached unreachable code".into()), string);
                let never = self.types.never();
                ir::Expr::new(ExprKind::Builtin { op: Builtin::Panic, args: vec![msg], span }, never)
            }
            "assert" => {
                let bool_ty = self.types.bool();
                let string = self.types.string();
                let (cond, msg) = match args {
                    [c] => {
                        let text = format!("assertion failed: {}", self.source_text(c.value.span));
                        (self.expr_coerced(&c.value, bool_ty), ir::Expr::new(ExprKind::Str(text), string))
                    }
                    [c, m] => (self.expr_coerced(&c.value, bool_ty), self.expr_coerced(&m.value, string)),
                    _ => {
                        self.report(
                            Diagnostic::error(codes::ARG_COUNT, "`assert` takes a condition and an optional message")
                                .primary(span, "like `assert(x > 0, \"x must be positive\")`"),
                        );
                        return ir::Expr::new(ExprKind::Zero, void);
                    }
                };
                ir::Expr::new(ExprKind::Builtin { op: Builtin::Assert, args: vec![cond, msg], span }, void)
            }
            _ => unreachable!("unknown builtin"),
        }
    }

    /// Returns the source text of a span.
    /// How to make a value of a type written where a value goes: `.new`
    /// only for the containers that have it, otherwise `{}` or a literal.
    fn make_value_help(&self, t: &ast::TypeExpr, text: &str) -> String {
        use ast::TypeKind as T;
        let kind = match &t.kind {
            T::Spliced(id) if (*id as usize) < self.types.len() => self.types.kind(TyId(*id)).clone(),
            _ => TyKind::Unknown,
        };
        match (&t.kind, kind) {
            (T::Dynamic(_) | T::Map(..), _) | (_, TyKind::Dynamic(_) | TyKind::Map(..)) => {
                format!("make an empty one with `{text}.new`, or with `{{}}` where `{text}` is expected")
            }
            (T::Optional(_), _) | (_, TyKind::Optional(_)) => {
                format!("for an empty `{text}`, write `nil` where `{text}` is expected")
            }
            (T::Slice(_), _) | (_, TyKind::Slice(_)) => {
                format!("where `{text}` is expected, an array literal like `[1, 2]` makes one, and `{{}}` an empty one")
            }
            (T::Array(..), _) | (_, TyKind::Array(..)) => format!(
                "make a zero value with `{{}}` where `{text}` is expected, like `x: {text} = {{}}`, or write an array literal"
            ),
            (T::Proc { params, ret, .. }, _) => {
                let params: Vec<String> = params
                    .iter()
                    .enumerate()
                    .map(|(i, p)| format!("{}: {}", (b'a' + (i % 26) as u8) as char, self.source_text(p.span)))
                    .collect();
                let ret = ret.as_ref().map(|r| format!(" -> {}", self.source_text(r.span))).unwrap_or_default();
                format!(
                    "a `{text}` value is a proc literal, like `->({}){ret} {{ … }}`, or a method, `method(:name)`",
                    params.join(", ")
                )
            }
            (_, TyKind::Proc(_)) => {
                format!("a `{text}` value is a proc literal or a method, `method(:name)`")
            }
            (T::Pointer(_), _) | (_, TyKind::Pointer(_)) => {
                format!("a `{text}` value is the address of a variable or field, like `&x`")
            }
            (T::MultiPointer(inner), _) => {
                let elem = self.source_text(inner.span);
                format!("a `{text}` value points into an array, a slice or a dynamic array, like `xs.to([^]{elem})`")
            }
            _ => format!("make a zero value of it with `{{}}` where `{text}` is expected, like `x: {text} = {{}}`"),
        }
    }

    pub fn source_text(&self, span: Span) -> String {
        self.source_texts
            .get(&span.file)
            .and_then(|t| t.get(span.start as usize..span.end as usize))
            .unwrap_or_default()
            .to_string()
    }
}

#[derive(Clone, Debug)]
pub(crate) enum ArgSource<'e> {
    Given(&'e ast::Expr),
    Default(ir::Expr),
    Missing,
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

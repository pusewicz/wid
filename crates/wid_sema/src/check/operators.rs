//! Operator methods: the operators a type can't define, and the comparisons
//! that `==` and `<=>` give.

use wid_diagnostics::{Applicability, Diagnostic, Edit, Span, codes};
use wid_syntax::{Name, ast};

use super::{Checker, DeclId};
use crate::ir::{self, ExprKind};
use crate::types::{TyId, TyKind};

/// The operator a comparison comes from: `!=` from `==`, and `<`, `<=`,
/// `>` and `>=` from `<=>`. Those can't be defined themselves, so they
/// always agree with it.
pub(super) fn derived_from(op: &str) -> Option<&'static str> {
    match op {
        "!=" => Some("=="),
        "<" | "<=" | ">" | ">=" => Some("<=>"),
        _ => None,
    }
}

impl<'a> Checker<'a> {
    /// The `==` or `<=>` method of `ty` that `op` comes from, when `op` is
    /// a derived comparison and the type defines no method named `op` (one
    /// that was reported as E0331 and is used as written).
    pub fn derived_operator_method(&mut self, ty: TyId, op: &str) -> Option<DeclId> {
        let base = derived_from(op)?;
        self.operator_method(ty, base)
    }

    /// The package-level operator function for `l op r`: one named `op`, or
    /// the `==` or `<=>` that `op` comes from (then `true`).
    pub fn package_operator_for(&mut self, op: &str, l: TyId, r: TyId) -> Option<(DeclId, bool)> {
        if let Some(decl) = self.package_operator(op, l, r) {
            return Some((decl, false));
        }
        let base = derived_from(op)?;
        self.package_operator(base, l, r).map(|decl| (decl, true))
    }

    /// The type one operand of `op` needs when a package-level operator
    /// function, or the `==` or `<=>` that `op` comes from, takes the other
    /// operand's type `known` (see [`Checker::package_operand_type`]).
    pub fn package_operand_type_for(&mut self, op: &str, known: TyId, known_left: bool) -> Option<TyId> {
        if let Some(ty) = self.package_operand_type(op, known, known_left) {
            return Some(ty);
        }
        let base = derived_from(op)?;
        self.package_operand_type(base, known, known_left)
    }

    /// Calls the package-level operator function that
    /// [`Checker::package_operator_for`] found for `l op r`.
    pub fn call_package_operator(
        &mut self,
        op: ast::BinOp,
        (decl, derived): (DeclId, bool),
        l: ir::Expr,
        r: ir::Expr,
        span: Span,
    ) -> ir::Expr {
        self.note_ref(span, decl, crate::uses::RefKind::Call);
        let call = self.call_operator_fn(decl, l, r);
        if derived { self.derive_comparison(op, call, span) } else { call }
    }

    /// Turns the result of a call of `==` or `<=>` into `op`'s: `!=` negates
    /// `==`, and `<`, `<=`, `>` and `>=` compare `<=>`'s result with 0.
    pub fn derive_comparison(&mut self, op: ast::BinOp, base: ir::Expr, span: Span) -> ir::Expr {
        let bool_ty = self.types.bool();
        if matches!(self.types.kind(base.ty), TyKind::Unknown) {
            return ir::Expr::new(ExprKind::Zero, bool_ty);
        }
        let shown = self.types.display(base.ty);
        if op == ast::BinOp::Ne {
            if base.ty != bool_ty {
                self.report(
                    Diagnostic::error(codes::NO_OPERATOR, "`!=` needs an `==` that returns `Bool`")
                        .primary(span, format!("`!=` negates `==`, which returns `{shown}` here"))
                        .help("make `==` return `Bool`"),
                );
                return ir::Expr::new(ExprKind::Zero, bool_ty);
            }
            return self.not(base);
        }
        if !self.types.is_int(base.ty) {
            self.report(
                Diagnostic::error(
                    codes::NO_OPERATOR,
                    format!("`{}` needs a `<=>` that returns an integer", op.as_str()),
                )
                .primary(
                    span,
                    format!("`{}` compares `<=>`'s result with 0, and it returns `{shown}` here", op.as_str()),
                )
                .help("make `<=>` return an `Int`: negative, 0 or positive"),
            );
            return ir::Expr::new(ExprKind::Zero, bool_ty);
        }
        let ir_op = match op {
            ast::BinOp::Lt => ir::BinaryOp::Lt,
            ast::BinOp::Le => ir::BinaryOp::Le,
            ast::BinOp::Gt => ir::BinaryOp::Gt,
            _ => ir::BinaryOp::Ge,
        };
        let zero = ir::Expr::new(ExprKind::Int(0), base.ty);
        ir::Expr::new(ExprKind::Binary { op: ir_op, lhs: Box::new(base), rhs: Box::new(zero), span }, bool_ty)
    }

    /// Reports a `def` or `overload` named like an operator that can't be
    /// defined (E0331): `!=`, `<`, `<=`, `>`, `>=` and `!`. `owner` is the
    /// type, module or `extend` it is written in.
    pub(super) fn check_definable_operator(&mut self, item: &ast::Item, owner: Option<DeclId>) {
        let (name, def) = match &item.kind {
            ast::ItemKind::Def(f) if !f.is_macro => (f.name, Some(f)),
            ast::ItemKind::Overload(o) => (o.name, None),
            _ => return,
        };
        if !undefinable(name.name) {
            return;
        }
        let op = name.as_str();
        if op == "!" {
            self.report(
                Diagnostic::error(codes::UNDEFINABLE_OPERATOR, "the operator `!` can't be defined")
                    .primary(name.span, "`!` is not an operator method")
                    .note("`!x` is true when `x` is `nil` or `false`, for every type, so no type can change it")
                    .suggest_replace(
                        "give the method a name, and call it by that name",
                        name.span,
                        "not".to_string(),
                        Applicability::MaybeIncorrect,
                    ),
            );
            return;
        }
        let Some(base) = derived_from(op) else { return };
        let what = if base == "==" { "`!=` comes from `==`" } else { "`<`, `<=`, `>` and `>=` come from `<=>`" };
        let mut diag = Diagnostic::error(codes::UNDEFINABLE_OPERATOR, format!("the operator `{op}` can't be defined"))
            .primary(name.span, format!("`{op}` comes from `{base}`"))
            .note(format!("{what}, so the two can't disagree"));
        let written = self.source_text(name.span);
        let symbol = if written.starts_with(':') { ":" } else { "" };
        diag = match (def, self.defines_operator(owner, name.span, base)) {
            // The type defines `==` or `<=>` already: this one only repeats it.
            (Some(_), true) => diag.suggest(
                format!("remove it: `{base}` gives `{op}` already"),
                vec![Edit { span: self.removal_span(item.span), replacement: String::new() }],
                Applicability::MachineApplicable,
            ),
            (Some(f), false) => {
                let mut edits = vec![Edit { span: name.span, replacement: base.to_string() }];
                if base == "<=>"
                    && let Some(ret) = &f.ret
                    && self.source_text(ret.span) == "Bool"
                {
                    edits.push(Edit { span: ret.span, replacement: "Int".to_string() });
                }
                // `def <(o: T) -> Bool = @x < o.x` is `def <=>(o: T) -> Int =
                // @x <=> o.x`, and `!=` comparing two values is `==`
                // comparing them.
                let body_op = self.compared_operator(f, op);
                let applicability = match body_op {
                    Some(span) => {
                        edits.push(Edit { span, replacement: base.to_string() });
                        Applicability::MachineApplicable
                    }
                    None => Applicability::MaybeIncorrect,
                };
                // A method compares `self` with its parameter; a package-level
                // operator its two parameters.
                let names: Vec<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
                let (first, second) = match names.as_slice() {
                    [a, b] if owner.is_none() => (format!("`{a}`"), format!("`{b}`")),
                    [b] => ("`self`".to_string(), format!("`{b}`")),
                    _ => ("the left operand".to_string(), "the right one".to_string()),
                };
                let returns = if base == "==" {
                    "`Bool`, true when the two are equal".to_string()
                } else {
                    format!(
                        "an `Int`: negative when {first} comes first, 0 when the two are equal and positive when {second} comes first"
                    )
                };
                let rewrite = if body_op.is_some() { "" } else { ", and rewrite the body to match" };
                diag.suggest(format!("define `{base}` instead, which returns {returns}{rewrite}"), edits, applicability)
            }
            (None, _) => diag.suggest_replace(
                format!("define `{base}` instead"),
                name.span,
                format!("{symbol}{base}"),
                Applicability::MaybeIncorrect,
            ),
        };
        self.report(diag);
    }

    /// Whether the declarations next to the one at `span` (in `owner`'s
    /// body, or the package's files at package level) define `op`.
    fn defines_operator(&self, owner: Option<DeclId>, span: Span, op: &str) -> bool {
        let named = |items: &[ast::Item]| {
            items.iter().any(|i| matches!(&i.kind, ast::ItemKind::Def(f) if f.name.as_str() == op && !f.is_macro))
        };
        let Some(owner) = owner else {
            return self
                .input
                .packages
                .iter()
                .find(|p| p.files.iter().any(|f| f.ast.file == span.file))
                .is_some_and(|p| p.files.iter().any(|f| named(&f.ast.items)));
        };
        match &self.decls[owner.0 as usize].item.kind {
            ast::ItemKind::Struct(s) => named(&s.body),
            ast::ItemKind::Enum(e) => named(&e.body),
            ast::ItemKind::Module(m) => named(&m.body),
            ast::ItemKind::Extend(e) => named(&e.body),
            _ => false,
        }
    }

    /// Where `op` is written in the body of `f` when the body is only
    /// `a op b`.
    fn compared_operator(&self, f: &ast::FnDecl, op: &str) -> Option<Span> {
        let value = match &f.body {
            ast::FnBody::Expr(e) => e.as_ref(),
            ast::FnBody::Block(stmts) => match stmts.as_slice() {
                [ast::Stmt { kind: ast::StmtKind::Expr(e), .. }] => e,
                _ => return None,
            },
        };
        let ast::ExprKind::Binary { op: written, lhs, rhs } = &value.kind else { return None };
        if written.as_str() != op {
            return None;
        }
        let between = Span::new(lhs.span.file, lhs.span.end, rhs.span.start);
        let at = self.source_text(between).find(op)?;
        let start = lhs.span.end + at as u32;
        Some(Span::new(lhs.span.file, start, start + op.len() as u32))
    }

    /// The span that removes `item` and the blank lines before it: from the
    /// end of the code before it to its end.
    fn removal_span(&self, item: Span) -> Span {
        let Some(text) = self.source_texts.get(&item.file) else { return item };
        if item.end as usize > text.len() {
            return item;
        }
        let before = text[..item.start as usize].trim_end();
        if before.is_empty() {
            let rest = &text[item.end as usize..];
            let end = item.end as usize + rest.len() - rest.trim_start().len();
            return Span::new(item.file, item.start, end as u32);
        }
        Span::new(item.file, before.len() as u32, item.end)
    }
}

/// Adds to E0307 for a struct `shown` the method that gives it `op`: the
/// operator itself, `==` for `!=`, or `<=>` for `<`, `<=`, `>` and `>=`.
pub(super) fn define_operator_help(diag: Diagnostic, op: ast::BinOp, shown: &str) -> Diagnostic {
    match op {
        ast::BinOp::Lt | ast::BinOp::Le | ast::BinOp::Gt | ast::BinOp::Ge | ast::BinOp::Cmp => diag.help(format!(
            "define `<=>` on `{shown}`, which gives `<`, `<=`, `>` and `>=`: `def <=>(other: {shown}) -> Int`, returning a negative number, 0 or a positive number"
        )),
        ast::BinOp::Eq | ast::BinOp::Ne => diag.help(format!(
            "define `==` on `{shown}`, which gives `!=` too: `def ==(other: {shown}) -> Bool`"
        )),
        _ => diag.help(format!("define the operator on `{shown}`: `def {}(other: {shown}) -> {shown}`", op.as_str())),
    }
}

/// Whether `name` is an operator that can't be defined.
pub(super) fn undefinable(name: Name) -> bool {
    matches!(name.as_str(), "!" | "!=" | "<" | "<=" | ">" | ">=")
}

//! Lowering statements.

use wid_diagnostics::{Applicability, Diagnostic, Span, and_list, codes};
use wid_syntax::ast::{self, ExprKind as E, StmtKind as S};

use super::Checker;
use super::body::{Dest, Exit};
use super::macros::{Operand, OperandKind};
use super::runtime::holds_parse_error;
use crate::ir::{self, ExprKind, Stmt};
use crate::types::TyKind;

impl<'a> Checker<'a> {
    /// Lowers a statement list, delivering the value of the last statement
    /// to `dest`.
    pub fn lower_stmts(&mut self, stmts: &[ast::Stmt], dest: Dest) {
        let mut warned_unreachable = false;
        for (i, stmt) in stmts.iter().enumerate() {
            if !warned_unreachable && self.current_block_diverges() {
                warned_unreachable = true;
                self.report(
                    Diagnostic::warning(codes::UNREACHABLE_CODE, "unreachable code")
                        .primary(stmt.span, "this never runs because the code above always leaves")
                        .help("remove it, or move it before the statement that leaves"),
                );
            }
            let last = i + 1 == stmts.len();
            // Unreachable code is still checked, but it never delivers the
            // block's value.
            if last && !matches!(dest, Dest::Discard) && !warned_unreachable {
                self.lower_tail(stmt, dest);
            } else {
                self.lower_stmt(stmt);
            }
        }
    }

    /// Lowers the last statement of a list whose value is wanted.
    fn lower_tail(&mut self, stmt: &ast::Stmt, dest: Dest) {
        match &stmt.kind {
            S::Expr(e) => match &e.kind {
                E::If(if_expr) => self.lower_if(if_expr, dest, e.span),
                E::ComptimeIf(if_expr) => self.lower_comptime_if(if_expr, dest),
                E::Case(case) => self.lower_case(case, dest, e.span),
                E::Paren(inner) if matches!(inner.kind, E::If(_)) => {
                    let wrapped = ast::Stmt { kind: S::Expr((**inner).clone()), span: stmt.span, attrs: Vec::new() };
                    self.lower_tail(&wrapped, dest);
                }
                _ => {
                    let expected = self.dest_type(dest);
                    let value = self.expr(e, expected);
                    self.deliver(value, dest, e.span);
                }
            },
            _ => self.lower_stmt(stmt),
        }
    }

    /// Returns the type a destination expects, if known.
    pub fn dest_type(&self, dest: Dest) -> Option<crate::types::TyId> {
        match dest {
            Dest::Discard => None,
            Dest::Local(_, ty) => Some(ty),
            Dest::Return => Some(self.frame().ret),
        }
    }

    /// Sends a value to its destination.
    pub fn deliver(&mut self, value: ir::Expr, dest: Dest, span: Span) {
        match dest {
            Dest::Discard => self.emit_value_stmt(value),
            Dest::Local(local, _) => {
                let mut ty = self.local_ty(local);
                if matches!(self.types.kind(ty), TyKind::Unknown) {
                    ty = self.value_type(value.ty, span);
                    self.body.locals[local.0 as usize].ty = ty;
                }
                if matches!(self.types.kind(value.ty), TyKind::Never) {
                    self.emit_value_stmt(value);
                } else {
                    let value = self.coerce(value, ty, span);
                    self.emit(Stmt::Assign { target: ir::Expr::new(ExprKind::Local(local), ty), value });
                }
            }
            Dest::Return => {
                let ret = self.frame().ret;
                if matches!(self.types.kind(value.ty), TyKind::Never) {
                    self.emit_value_stmt(value);
                    return;
                }
                let value = self.coerce(value, ret, span);
                self.emit_return(Some(value), span);
            }
        }
    }

    /// Lowers one statement whose value, if any, is discarded.
    pub fn lower_stmt(&mut self, stmt: &ast::Stmt) {
        self.emit(Stmt::Line(stmt.span));
        let site = self.enter_site(stmt.span);
        let line = std::mem::replace(&mut self.macros.line, stmt.span);
        let no_bounds = self.check_statement_attributes(&stmt.attrs);
        let saved_bounds = self.no_bounds_check;
        if no_bounds {
            self.no_bounds_check = true;
        }
        self.lower_stmt_inner(stmt);
        self.no_bounds_check = saved_bounds;
        self.macros.line = line;
        self.leave_site(site);
    }

    fn lower_stmt_inner(&mut self, stmt: &ast::Stmt) {
        match &stmt.kind {
            S::Expr(e) => self.lower_expr_stmt(e),
            S::Decl { names, ty, value, uninit } => self.lower_decl(names, ty, value.as_ref(), *uninit),
            S::Assign { targets, op, values } => self.lower_assign(targets, *op, values, stmt.span),
            S::Return(values) => self.lower_return(values, stmt.span),
            S::Break(value) | S::Next(value) => {
                let is_break = matches!(stmt.kind, S::Break(_));
                self.lower_loop_exit(is_break, value.as_ref(), stmt.span);
            }
            S::Defer(body) => self.lower_defer(body, stmt.span),
            S::Guard { names, value, err, else_body } => self.lower_guard(names, value, *err, else_body, stmt.span),
            S::Item(item) => {
                self.report(
                    Diagnostic::error(codes::UNEXPECTED_TOKEN, "declarations cannot be nested inside methods")
                        .primary(item.span, "move this to the top level of the file"),
                );
            }
            S::Error => {}
        }
    }

    fn lower_expr_stmt(&mut self, e: &ast::Expr) {
        match &e.kind {
            E::If(if_expr) => self.lower_if(if_expr, Dest::Discard, e.span),
            E::ComptimeIf(if_expr) => self.lower_comptime_if(if_expr, Dest::Discard),
            E::Case(case) => self.lower_case(case, Dest::Discard, e.span),
            E::While { cond, body, until } => self.lower_while(cond, body, *until),
            E::Loop(body) => self.lower_loop(body),
            E::For(f) => self.lower_for(f, e.span),
            _ => {
                let value = self.expr(e, None);
                self.check_discarded(&value, e.span);
                self.emit_value_stmt(value);
            }
        }
    }

    /// Reports values that must not be dropped silently.
    fn check_discarded(&mut self, value: &ir::Expr, span: Span) {
        let ty = value.ty;
        let drops_error = match self.types.kind(ty) {
            TyKind::Error => true,
            TyKind::Tuple(elems) => elems.last().is_some_and(|t| matches!(self.types.kind(*t), TyKind::Error)),
            _ => false,
        };
        if drops_error {
            let is_call = matches!(value.kind, ExprKind::Call { .. } | ExprKind::CallIndirect { .. });
            let (message, label) = if is_call {
                ("the `Error` returned here is ignored", "this call can fail")
            } else {
                ("this `Error` is never checked", "evaluating it does not handle it")
            };
            self.report(
                Diagnostic::error(codes::IGNORED_ERROR, message)
                    .primary(span, label)
                    .help("handle it with `guard … else |err| … end`, or discard it explicitly with `_ = …`"),
            );
        }
    }

    fn lower_decl(&mut self, names: &[ast::Ident], ty: &ast::TypeExpr, value: Option<&ast::Expr>, uninit: bool) {
        let ctx = self.body_ctx();
        let ty = self.resolve_type(ty, &ctx);
        if names.len() > 1 && value.is_some() {
            self.report(
                Diagnostic::error(codes::UNEXPECTED_TOKEN, "a declaration of several names cannot have a value")
                    .primary(names[0].span.to(names[names.len() - 1].span), "these are all zero-initialized")
                    .help("declare them separately, or assign them with `a, b = f()`"),
            );
        }
        let init = match value {
            Some(v) if names.len() == 1 => Some(self.expr_coerced(v, ty)),
            _ => None,
        };
        // A value that failed to parse poisons the names (see `declare_var`).
        let poisoned = value.is_some_and(holds_parse_error);
        for name in names {
            if self.check_redeclare(name) {
                continue;
            }
            let local = self.declare_var(name.name, ty, name.span, poisoned);
            if uninit {
                self.emit(Stmt::LetUninit(local));
            } else {
                let known_some = init.as_ref().is_some_and(|i| matches!(i.kind, ExprKind::OptSome(_)));
                self.emit(Stmt::Let { local, init: init.clone() });
                if known_some {
                    self.narrow(local);
                }
            }
        }
    }

    /// Reports a typed declaration of a name that already exists.
    fn check_redeclare(&mut self, name: &ast::Ident) -> bool {
        let existing = self.find_var_at(name.name, name.span).map(|v| v.span);
        if let Some(prev) = existing {
            self.report(
                Diagnostic::error(codes::DUPLICATE_DEFINITION, format!("`{}` is already declared", name.as_str()))
                    .primary(name.span, "declared again here")
                    .secondary(prev, "first declared here")
                    .help("assign to the existing variable with `=` instead of declaring it again"),
            );
            return true;
        }
        false
    }

    fn lower_assign(&mut self, targets: &[ast::Expr], op: Option<ast::BinOp>, values: &[ast::Expr], span: Span) {
        if targets.len() != 1 || values.len() != 1 {
            if let Some(op) = op {
                let op_text = format!("{}=", op.as_str());
                let parts: Vec<String> = values.iter().map(|v| self.source_text(v.span)).collect();
                let value_text = parts.join(", ");
                let lines: Vec<String> =
                    targets.iter().map(|t| format!("`{} {op_text} {value_text}`", self.source_text(t.span))).collect();
                self.report(
                    Diagnostic::error(
                        codes::INVALID_ASSIGN_TARGET,
                        format!("`{op_text}` updates one target at a time"),
                    )
                    .primary(span, format!("{} targets", targets.len()))
                    .help(format!("write {} as separate statements", and_list(&lines))),
                );
                for t in targets {
                    self.begin_block();
                    let _ = self.expr(t, None);
                    let _ = self.end_block();
                }
                return;
            }
            self.lower_multi_assign(targets, values, span);
            return;
        }
        let target = &targets[0];
        let value = &values[0];
        if let Some(op) = op {
            self.lower_op_assign(target, op, value, span);
            return;
        }
        if self.assign_index_method(target, None, value, span) || self.assign_map_index(target, value) {
            return;
        }
        if let E::Ident(name) = target.kind {
            let existing = self.find_var_at(name, target.span).map(|v| (v.local, v.ty, v.indirect));
            match existing {
                Some((local, ty, true)) => {
                    let v = self.expr_coerced(value, ty);
                    let ptr = self.local_ty(local);
                    let target =
                        ir::Expr::new(ExprKind::Deref(Box::new(ir::Expr::new(ExprKind::Local(local), ptr))), ty);
                    self.emit(Stmt::Assign { target, value: v });
                }
                Some((local, ty, false)) => {
                    let v = self.expr_coerced(value, ty);
                    self.invalidate(local);
                    let known_some = matches!(v.kind, ExprKind::OptSome(_));
                    self.emit(Stmt::Assign { target: ir::Expr::new(ExprKind::Local(local), ty), value: v });
                    if known_some {
                        self.narrow(local);
                    }
                }
                None => {
                    let v = self.expr(value, None);
                    let ty = self.value_type(v.ty, value.span);
                    let v = self.coerce(v, ty, value.span);
                    let local = self.declare_var(name, ty, target.span, holds_parse_error(value));
                    if matches!(value.kind, E::Call(_) | E::Member { .. }) && self.types.is_nilable(ty) {
                        let origin = self.source_text(value.span);
                        if let Some(var) = self.find_var_at(name, target.span) {
                            var.origin = Some(origin);
                            var.decl_stmt = Some(span);
                        }
                    }
                    self.emit(Stmt::Let { local, init: Some(v) });
                }
            }
            return;
        }
        let place = self.place(target);
        let v = self.expr_coerced(value, place.ty);
        if Self::is_context_place(&place) {
            self.shadow_context();
        }
        self.emit(Stmt::Assign { target: place, value: v });
    }

    fn lower_op_assign(&mut self, target: &ast::Expr, op: ast::BinOp, value: &ast::Expr, span: Span) {
        if matches!(op, ast::BinOp::And | ast::BinOp::Or) {
            self.lower_logical_assign(target, op, value);
            return;
        }
        if self.assign_index_method(target, Some(op), value, span) {
            return;
        }
        let place = match self.map_index_slot(target) {
            Some(slot) => slot,
            None => self.place(target),
        };
        let place = if place.is_pure() {
            place
        } else {
            let ty = place.ty;
            let ptr = self.address_of(place);
            let ptr = self.spill(ptr);
            ir::Expr::new(ExprKind::Deref(Box::new(ptr)), ty)
        };
        if Self::is_context_place(&place) {
            self.shadow_context();
        }
        let rhs_expected = match self.numeric_array_elem(place.ty) {
            Some(elem) if super::expr::is_untyped(value) => Some(elem),
            Some(_) => Some(place.ty),
            None => self.rhs_expected(op, place.ty),
        };
        let rhs = self.expr(value, rhs_expected);
        let combined = self.combine_values(op, place.clone(), value, rhs, target.span, span);
        let combined = self.coerce(combined, place.ty, span);
        self.emit(Stmt::Assign { target: place, value: combined });
    }

    /// Lowers `a ||= b` and `a &&= b`. On a `Bool` they assign when `a` is
    /// false (`||=`) or true (`&&=`); on an optional, when `a` is nil or has
    /// a value. `b` is only evaluated when the assignment happens.
    fn lower_logical_assign(&mut self, target: &ast::Expr, op: ast::BinOp, value: &ast::Expr) {
        let is_or = op == ast::BinOp::Or;
        let op_text = if is_or { "||=" } else { "&&=" };
        if self.map_logical_assign(target, is_or, value) {
            return;
        }
        let mut local = None;
        let place = match target.kind {
            E::Ident(name) => match self.find_var_at(name, target.span).map(|v| {
                v.read = true;
                (v.local, v.ty, v.indirect)
            }) {
                Some((l, ty, indirect)) => {
                    let var = ir::Expr::new(ExprKind::Local(l), if indirect { self.local_ty(l) } else { ty });
                    if indirect {
                        ir::Expr::new(ExprKind::Deref(Box::new(var)), ty)
                    } else {
                        local = Some(l);
                        var
                    }
                }
                None => self.place(target),
            },
            _ => self.place(target),
        };
        let place = if place.is_pure() {
            place
        } else {
            let ty = place.ty;
            let ptr = self.address_of(place);
            let ptr = self.spill(ptr);
            ir::Expr::new(ExprKind::Deref(Box::new(ptr)), ty)
        };
        let ty = place.ty;
        if matches!(self.types.kind(ty), TyKind::Unknown) {
            return;
        }
        let optional = self.types.is_nilable(ty);
        let cond = if optional {
            let some = self.is_some(place.clone());
            if is_or { self.not(some) } else { some }
        } else if matches!(self.types.kind(self.types.base(ty)), TyKind::Bool) {
            if is_or { self.not(place.clone()) } else { place.clone() }
        } else {
            let shown = self.types.display(ty);
            let target_text = self.source_text(target.span);
            let value_text = self.source_text(value.span);
            let combined = if is_or { "||" } else { "&&" };
            self.report(
                Diagnostic::error(
                    codes::NO_OPERATOR,
                    format!("`{op_text}` needs a `Bool` or an optional, but this is `{shown}`"),
                )
                .primary(target.span, format!("this has type `{shown}`"))
                .note(format!(
                    "`a {op_text} b` assigns `b` only when `a` is {}",
                    if is_or { "false or nil" } else { "true or holds a value" }
                ))
                .help(format!("for other types, write the condition out: `{target_text} = {value_text} if …`"))
                .note(format!("`{combined}` itself only works on `Bool` and optional values")),
            );
            return;
        };
        if Self::is_context_place(&place) {
            self.shadow_context();
        }
        // The value runs only when the assignment happens.
        let operand = Operand { kind: OperandKind::LogicalAssign { or: is_or }, span: value.span };
        let mut known_some = false;
        let (stmts, _) = self.lower_operand(operand, |this| {
            let v = this.expr_coerced(value, ty);
            known_some = matches!(v.kind, ExprKind::OptSome(_));
            this.emit(Stmt::Assign { target: place, value: v });
            ir::Expr::new(ExprKind::Zero, this.types.void())
        });
        self.emit(Stmt::If { cond, then: ir::Block { stmts }, else_: ir::Block::default() });
        if let Some(l) = local {
            self.invalidate(l);
            if optional && is_or && known_some {
                self.narrow(l);
            }
        }
    }

    /// Lowers an expression used as an assignment target.
    pub fn place(&mut self, target: &ast::Expr) -> ir::Expr {
        match &target.kind {
            E::Ident(name) => {
                let found = self.find_var_at(*name, target.span).map(|v| {
                    v.read = true;
                    (v.local, v.ty)
                });
                match found {
                    Some(_) => self.expr(target, None),
                    None => {
                        let candidates = self.visible_var_names();
                        if !self.declared_by_failed_macro(true, false) {
                            self.undefined(*name, target.span, candidates, "variable");
                        }
                        ir::Expr::new(ExprKind::Zero, self.types.unknown())
                    }
                }
            }
            E::Paren(inner) => self.place(inner),
            E::IVar(_) | E::Member { .. } | E::SelfRef | E::Deref(_) | E::Index { .. } => {
                let v = self.expr(target, None);
                if let ExprKind::Index { base, .. } = &v.kind
                    && matches!(self.types.kind(self.types.base(base.ty)), TyKind::String)
                {
                    self.report(
                        Diagnostic::error(codes::NOT_ASSIGNABLE, "strings are immutable")
                            .primary(target.span, "cannot change a byte of a `String`")
                            .help("build a new string, for example with interpolation"),
                    );
                    return ir::Expr::new(ExprKind::Zero, self.types.unknown());
                }
                if matches!(self.types.kind(v.ty), TyKind::Unknown) || super::members::is_place(&v) {
                    return v;
                }
                self.report(
                    Diagnostic::error(codes::NOT_ASSIGNABLE, "cannot assign to this expression")
                        .primary(target.span, "this is a temporary value, not a variable or field")
                        .help("store the value in a variable first, change it, and use the variable"),
                );
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
            // A constant's name spliced into a `quote` (`#{name} = 1` with
            // `:LIMIT`), which declares a constant only among declarations.
            E::Const(name) => {
                if let Some(by) = self.spliced_by(target.span) {
                    self.report(
                        Diagnostic::error(
                            codes::SPLICE_MISMATCH,
                            format!("the macro `{by}` splices the constant name `{name}` where an assignment target goes"),
                        )
                        .primary(target.span, "this is a constant's name")
                        .note("`NAME = value` declares a constant among declarations; in a method, a constant can't be assigned")
                        .help("to assign to a variable, splice a lowercase name"),
                    );
                }
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
            // The parser reports every other target written as it is
            // (E0107), and the macro expander one spliced into a `quote`
            // (E0911, `Splicer::splice_target`). Its parts are still
            // checked, so names it reads are not reported as unused.
            _ => {
                self.begin_block();
                let _ = self.expr(target, None);
                let _ = self.end_block();
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
        }
    }

    fn lower_return(&mut self, values: &[ast::Expr], span: Span) {
        let ret = self.frame().ret;
        if matches!(self.types.kind(ret), TyKind::Never) {
            let name = self.frame().fn_name.clone();
            self.report(
                Diagnostic::error(
                    codes::RETURN_MISMATCH,
                    format!("`{name}` is declared `-> Never`, so it cannot return"),
                )
                .primary(span, "this would return to the caller")
                .help("end the method with `panic`, an endless `loop`, or a call to another `-> Never` method"),
            );
            self.emit(Stmt::Unreachable);
            return;
        }
        let returns_void = matches!(self.types.kind(ret), TyKind::Void);
        match values {
            [] => {
                if !returns_void && !matches!(self.types.kind(ret), TyKind::Unknown) {
                    let want = self.types.display(ret);
                    self.report(
                        Diagnostic::error(codes::RETURN_MISMATCH, format!("this method must return `{want}`"))
                            .primary(span, "`return` without a value"),
                    );
                }
                self.emit_return(None, span);
            }
            [value] => {
                if returns_void {
                    let name = self.frame().fn_name.clone();
                    self.report(
                        Diagnostic::error(codes::RETURN_MISMATCH, format!("`{name}` does not return a value"))
                            .primary(value.span, "this value has nowhere to go")
                            .help("declare a return type with `-> Type`, or remove the value"),
                    );
                    let v = self.expr(value, None);
                    self.emit_value_stmt(v);
                    self.emit_return(None, span);
                    return;
                }
                let v = self.expr_coerced(value, ret);
                self.emit_return(Some(v), span);
            }
            _ => self.lower_multi_return(values, span),
        }
    }

    fn lower_defer(&mut self, body: &[ast::Stmt], span: Span) {
        // In an operand's code no block ends when the `defer` should run;
        // its body is still checked.
        let rejected = self.defer_in_operand(span);
        self.begin_block();
        self.body.exits.push(Exit::Defer);
        self.push_scope();
        self.lower_stmts(body, Dest::Discard);
        self.pop_scope();
        self.body.exits.pop();
        let block = self.end_block();
        if !rejected {
            self.add_defer(block);
        }
    }

    /// Lowers `if`/`unless` delivering each branch's value to `dest`.
    pub fn lower_if(&mut self, if_expr: &ast::IfExpr, dest: Dest, span: Span) {
        let (cond, then, facts) = self.lower_cond_branch(&if_expr.cond, if_expr.unless, &if_expr.then, dest);
        let else_ = self.lower_else_chain(&if_expr.elifs, if_expr.else_.as_deref(), dest, span, &facts.when_false);
        let then_div = super::body::stmts_diverge(&then.stmts);
        let else_div = super::body::stmts_diverge(&else_.stmts);
        self.emit(Stmt::If { cond, then, else_ });
        if if_expr.elifs.is_empty() {
            if then_div && !else_div {
                self.apply_facts(&facts.when_false);
            } else if else_div && !then_div {
                self.apply_facts(&facts.when_true);
            }
        }
    }

    /// Lowers a condition and the branch it guards, narrowing optionals the
    /// condition proves non-nil inside the branch.
    fn lower_cond_branch(
        &mut self,
        cond: &ast::Cond,
        negate: bool,
        body: &[ast::Stmt],
        dest: Dest,
    ) -> (ir::Expr, ir::Block, super::flow::Facts) {
        match cond {
            ast::Cond::Expr(e) => {
                let mut facts = self.facts(e);
                if negate {
                    facts = super::flow::Facts { when_true: facts.when_false, when_false: facts.when_true };
                }
                let c = self.cond_expr(cond, negate);
                let block = self.lower_branch_narrowed(body, dest, &facts.when_true);
                (c, block, facts)
            }
            ast::Cond::Bind { name, value } => {
                let (c, block) = self.lower_if_bind(*name, value, body, negate, dest);
                (c, block, super::flow::Facts::default())
            }
        }
    }

    fn lower_else_chain(
        &mut self,
        elifs: &[(ast::Cond, Vec<ast::Stmt>)],
        else_: Option<&[ast::Stmt]>,
        dest: Dest,
        span: Span,
        narrowed: &[crate::ir::LocalId],
    ) -> ir::Block {
        if let Some(((cond, body), rest)) = elifs.split_first() {
            self.begin_block();
            self.push_scope();
            self.apply_facts(narrowed);
            let (c, then, facts) = self.lower_cond_branch(cond, false, body, dest);
            let mut next = narrowed.to_vec();
            next.extend(facts.when_false);
            let else_b = self.lower_else_chain(rest, else_, dest, span, &next);
            self.emit(Stmt::If { cond: c, then, else_: else_b });
            self.pop_scope();
            return self.end_block();
        }
        match else_ {
            Some(body) => self.lower_branch_narrowed(body, dest, narrowed),
            None => {
                if let Dest::Local(..) = dest {
                    let diag = self.missing_else(
                        codes::MISSING_RETURN,
                        span,
                        "`if`",
                        "when the condition is false there is no value",
                    );
                    self.report(diag);
                    return ir::Block { stmts: vec![Stmt::Unreachable] };
                }
                ir::Block::default()
            }
        }
    }

    fn lower_branch_narrowed(&mut self, body: &[ast::Stmt], dest: Dest, narrowed: &[crate::ir::LocalId]) -> ir::Block {
        self.begin_block();
        self.push_scope();
        self.apply_facts(narrowed);
        self.lower_stmts(body, dest);
        self.pop_scope();
        self.end_block()
    }

    pub fn lower_branch(&mut self, body: &[ast::Stmt], dest: Dest) -> ir::Block {
        self.begin_block();
        self.push_scope();
        self.lower_stmts(body, dest);
        self.pop_scope();
        self.end_block()
    }

    /// Lowers a condition to a `Bool` expression.
    pub fn cond_expr(&mut self, cond: &ast::Cond, negate: bool) -> ir::Expr {
        match cond {
            ast::Cond::Expr(e) => {
                let bool_ty = self.types.bool();
                let v =
                    if matches!(e.kind, E::Ident(_)) { self.nilable_operand(e) } else { self.expr(e, Some(bool_ty)) };
                let v = self.truthy(v, e.span);
                if negate { self.not(v) } else { v }
            }
            ast::Cond::Bind { name, .. } => {
                self.report(
                    Diagnostic::error(codes::UNEXPECTED_TOKEN, "`v = value` binds only in `if`, `elsif` and `unless`")
                        .primary(name.span, "cannot bind here")
                        .help("to compare, use `==`"),
                );
                ir::Expr::new(ExprKind::Zero, self.types.bool())
            }
        }
    }

    /// Converts a condition value to `Bool`, rejecting values that are
    /// always truthy.
    pub fn truthy(&mut self, v: ir::Expr, span: Span) -> ir::Expr {
        self.truthy_in(v, span, None)
    }

    /// Like `truthy`, for the operand of `!`; `not_span` covers the whole
    /// `!x`, so the fix can rewrite it as `x == 0`.
    pub fn truthy_in(&mut self, v: ir::Expr, span: Span, not_span: Option<Span>) -> ir::Expr {
        let ty = v.ty;
        if self.types.is_nilable(ty) {
            return self.is_some(v);
        }
        match self.types.kind(self.types.base(ty)) {
            TyKind::Bool | TyKind::Unknown | TyKind::Never => v,
            _ => {
                let shown = self.types.display(ty);
                let message = match not_span {
                    Some(_) => format!("`!` needs a `Bool`, found `{shown}`"),
                    None => format!("condition must be a `Bool`, found `{shown}`"),
                };
                let mut diag = Diagnostic::error(codes::NON_BOOL_CONDITION, message)
                    .primary(span, format!("this has type `{shown}`"));
                if self.types.is_numeric(ty) {
                    let text = self.source_text(span);
                    let operand = if super::items::is_simple_operand(&text) { text } else { format!("({text})") };
                    let (target, replacement) = match not_span {
                        Some(full) => (full, format!("{operand} == 0")),
                        None => (span, format!("{operand} != 0")),
                    };
                    diag = diag
                        .note("only `nil` and `false` are falsy, so a number would always be true")
                        .suggest_replace("compare it explicitly", target, replacement, Applicability::MaybeIncorrect);
                }
                self.report(diag);
                ir::Expr::new(ExprKind::Zero, self.types.bool())
            }
        }
    }

    /// Builds `!v` for a `Bool`.
    pub fn not(&mut self, v: ir::Expr) -> ir::Expr {
        let ty = self.types.bool();
        if let ExprKind::Bool(b) = v.kind {
            return ir::Expr::new(ExprKind::Bool(!b), ty);
        }
        ir::Expr::new(ExprKind::Unary { op: ir::UnaryOp::Not, expr: Box::new(v) }, ty)
    }

    /// Lowers `case`/`when` into a chain of branches delivering to `dest`.
    pub fn lower_case(&mut self, case: &ast::CaseExpr, dest: Dest, span: Span) {
        let subject = case.subject.as_ref().map(|s| {
            let v = self.expr(s, None);
            let v = match v.kind {
                ExprKind::Local(_) => v,
                _ if v.is_constant() => v,
                _ => self.spill(v),
            };
            (v, s.span)
        });
        let errors_before = self.diags.error_count();
        if let Some((v, sspan)) = &subject {
            self.check_case_exhaustive(case, v.ty, *sspan, dest, span);
        } else if case.else_.is_none() && !matches!(dest, Dest::Discard) {
            let diag =
                self.missing_else(codes::NON_EXHAUSTIVE, span, "`case`", "when no branch matches there is no value");
            self.report(diag);
        }
        // After reporting a missing branch, treat the fallthrough as never
        // happening, so the method is not also said to end without a value.
        let reported_missing = case.else_.is_none() && self.diags.error_count() > errors_before;
        let exhaustive_enum = case.else_.is_none()
            && subject.as_ref().is_some_and(|(v, _)| {
                matches!(self.types.kind(self.types.base(v.ty)), TyKind::Enum(_) | TyKind::Union(_))
            });
        let subject = subject.map(|(v, _)| v);
        let fallback: Vec<ast::Stmt>;
        let else_body = if exhaustive_enum || reported_missing {
            fallback = vec![panic_stmt("`case` matched no branch (an invalid enum value, or a nil union)", span)];
            Some(fallback.as_slice())
        } else {
            case.else_.as_deref()
        };
        let block = self.lower_when_chain(&case.whens, else_body, subject.as_ref(), dest, false);
        for s in block.stmts {
            self.emit(s);
        }
    }

    /// Lowers `when` branches as a chain of `if`s. Every pattern but the
    /// first of the `case` (`later` is false for the chain's first `when`)
    /// runs only when no earlier one matched, as an operand of its own.
    fn lower_when_chain(
        &mut self,
        whens: &[ast::When],
        else_: Option<&[ast::Stmt]>,
        subject: Option<&ir::Expr>,
        dest: Dest,
        later: bool,
    ) -> ir::Block {
        let Some((first, rest)) = whens.split_first() else {
            return match else_ {
                Some(body) => self.lower_branch(body, dest),
                None => ir::Block::default(),
            };
        };
        self.begin_block();
        let mut cond: Option<ir::Expr> = None;
        let bool_ty = self.types.bool();
        for (i, p) in first.patterns.iter().enumerate() {
            if i == 0 && !later {
                cond = Some(self.when_cond(subject, p));
                continue;
            }
            let operand = Operand { kind: OperandKind::WhenPattern, span: p.span };
            let (stmts, c) = self.lower_operand(operand, |this| this.when_cond(subject, p));
            cond = Some(match cond {
                // This block runs only when no earlier `when` matched.
                None => {
                    for s in stmts {
                        self.emit(s);
                    }
                    c
                }
                Some(prev) if stmts.is_empty() => ir::Expr::new(
                    ExprKind::Binary { op: ir::BinaryOp::Or, lhs: Box::new(prev), rhs: Box::new(c), span: p.span },
                    bool_ty,
                ),
                // The pattern needs statements, so test it only when the
                // earlier ones didn't match.
                Some(prev) => {
                    let matched = self.new_local(None, bool_ty);
                    self.emit(Stmt::Let { local: matched, init: Some(prev) });
                    let matched = ir::Expr::new(ExprKind::Local(matched), bool_ty);
                    let mut then = stmts;
                    then.push(Stmt::Assign { target: matched.clone(), value: c });
                    let unmatched = self.not(matched.clone());
                    self.emit(Stmt::If {
                        cond: unmatched,
                        then: ir::Block { stmts: then },
                        else_: ir::Block::default(),
                    });
                    matched
                }
            });
        }
        let cond = cond.unwrap_or_else(|| ir::Expr::new(ExprKind::Bool(false), self.types.bool()));
        let narrow = match (subject, first.patterns.as_slice()) {
            (Some(s @ ir::Expr { kind: ExprKind::Local(local), .. }), [p]) => {
                self.pattern_variant(s.ty, p).map(|v| (*local, v))
            }
            _ => None,
        };
        let then = match narrow {
            Some((local, variant)) => {
                self.begin_block();
                self.push_scope();
                self.narrow_variant(local, variant);
                self.lower_stmts(&first.body, dest);
                self.pop_scope();
                self.end_block()
            }
            None => self.lower_branch(&first.body, dest),
        };
        let else_block = self.lower_when_chain(rest, else_, subject, dest, true);
        self.emit(Stmt::If { cond, then, else_: else_block });
        self.end_block()
    }

    fn when_cond(&mut self, subject: Option<&ir::Expr>, pattern: &ast::Expr) -> ir::Expr {
        let Some(s) = subject else {
            return self.cond_expr(&ast::Cond::Expr(pattern.clone()), false);
        };
        let bool_ty = self.types.bool();
        if matches!(self.types.kind(self.types.base(s.ty)), TyKind::Union(_)) {
            let u32_ty = self.types.intern(TyKind::Int(crate::types::IntTy::U32));
            let tag = ir::Expr::new(ExprKind::UnionTag(Box::new(s.clone())), u32_ty);
            let want = if matches!(pattern.kind, E::Nil) {
                0
            } else {
                match self.pattern_variant(s.ty, pattern) {
                    Some(v) => i128::from(v) + 1,
                    None => {
                        let shown = self.types.display(s.ty);
                        self.report(
                            Diagnostic::error(
                                codes::TYPE_MISMATCH,
                                format!("`when` on `{shown}` takes one of its variant types"),
                            )
                            .primary(pattern.span, "not a variant of this union"),
                        );
                        return ir::Expr::new(ExprKind::Bool(false), bool_ty);
                    }
                }
            };
            let want = ir::Expr::new(ExprKind::Int(want), u32_ty);
            return ir::Expr::new(
                ExprKind::Binary { op: ir::BinaryOp::Eq, lhs: Box::new(tag), rhs: Box::new(want), span: pattern.span },
                bool_ty,
            );
        }
        if let E::Range { lo, hi, inclusive } = &pattern.kind {
            let mut parts = Vec::new();
            if let Some(lo) = lo {
                let l = self.expr_coerced(lo, s.ty);
                parts.push(self.binary_values(ast::BinOp::Ge, s.clone(), l, pattern.span, lo.span, pattern.span));
            }
            if let Some(hi) = hi {
                let h = self.expr_coerced(hi, s.ty);
                let op = if *inclusive { ast::BinOp::Le } else { ast::BinOp::Lt };
                parts.push(self.binary_values(op, s.clone(), h, pattern.span, hi.span, pattern.span));
            }
            return parts
                .into_iter()
                .reduce(|a, b| {
                    ir::Expr::new(
                        ExprKind::Binary {
                            op: ir::BinaryOp::And,
                            lhs: Box::new(a),
                            rhs: Box::new(b),
                            span: pattern.span,
                        },
                        bool_ty,
                    )
                })
                .unwrap_or_else(|| ir::Expr::new(ExprKind::Bool(true), bool_ty));
        }
        let p = self.expr(pattern, Some(s.ty));
        let p = self.coerce(p, s.ty, pattern.span);
        self.binary_values(ast::BinOp::Eq, s.clone(), p, pattern.span, pattern.span, pattern.span)
    }

    /// Resolves a `when` pattern naming a variant of a union subject.
    fn pattern_variant(&mut self, union_ty: crate::types::TyId, pattern: &ast::Expr) -> Option<u32> {
        if !matches!(self.types.kind(self.types.base(union_ty)), TyKind::Union(_)) {
            return None;
        }
        if !matches!(pattern.kind, E::Const(_) | E::Type(_) | E::Member { .. }) {
            return None;
        }
        let ty = match &pattern.kind {
            E::Member { .. } => match self.classify_receiver(pattern) {
                super::members::Receiver::Type(t) => t,
                _ => return None,
            },
            _ => {
                let texpr = super::members::expr_as_type(pattern);
                let ctx = self.body_ctx();
                self.resolve_type(&texpr, &ctx)
            }
        };
        self.union_variant(union_ty, ty)
    }

    fn check_case_exhaustive(
        &mut self,
        case: &ast::CaseExpr,
        ty: crate::types::TyId,
        sspan: Span,
        dest: Dest,
        span: Span,
    ) {
        if case.else_.is_some() {
            return;
        }
        let base = self.types.base(ty);
        if let TyKind::Enum(id) = self.types.kind(base) {
            let info = self.types.enum_info(*id).clone();
            let mut covered = std::collections::HashSet::new();
            for w in &case.whens {
                for p in &w.patterns {
                    let name = match &p.kind {
                        E::Symbol(n) => Some(*n),
                        E::Member { name, .. } => Some(name.name),
                        _ => None,
                    };
                    if let Some(n) = name {
                        covered.insert(n);
                    }
                }
            }
            let missing: Vec<String> =
                info.members.iter().filter(|(n, _)| !covered.contains(n)).map(|(n, _)| format!(":{n}")).collect();
            if !missing.is_empty() {
                self.report(
                    Diagnostic::error(codes::NON_EXHAUSTIVE, format!("`case` does not handle {}", missing.join(", ")))
                        .primary(sspan, format!("`{}` has {} members", info.name, info.members.len()))
                        .help(format!("add `when {}` or an `else` branch", missing.join(", "))),
                );
            }
            return;
        }
        if let TyKind::Union(id) = self.types.kind(base) {
            let info = self.types.union_info(*id).clone();
            let mut covered = std::collections::HashSet::new();
            for w in &case.whens {
                for p in &w.patterns {
                    if let Some(v) = self.pattern_variant(base, p) {
                        covered.insert(v);
                    }
                }
            }
            let missing: Vec<String> = info
                .variants
                .iter()
                .enumerate()
                .filter(|(i, _)| !covered.contains(&(*i as u32)))
                .map(|(_, t)| self.types.display(*t))
                .collect();
            if !missing.is_empty() {
                self.report(
                    Diagnostic::error(codes::NON_EXHAUSTIVE, format!("`case` does not handle {}", missing.join(", ")))
                        .primary(sspan, format!("`{}` has {} variants", info.name, info.variants.len()))
                        .help(format!("add `when {}` or an `else` branch", missing.join(", "))),
                );
            }
            return;
        }
        if !matches!(dest, Dest::Discard) && !matches!(self.types.kind(base), TyKind::Unknown) {
            let diag =
                self.missing_else(codes::NON_EXHAUSTIVE, span, "`case`", "when no branch matches there is no value");
            self.report(diag);
        }
    }

    /// The error for an `if` or `case` used as a value without an `else`,
    /// with a fix that inserts one before the closing `end`.
    fn missing_else(&self, code: wid_diagnostics::Code, span: Span, what: &str, label: &str) -> Diagnostic {
        let diag =
            Diagnostic::error(code, format!("this {what} is used as a value but has no `else`")).primary(span, label);
        let text = self.source_text(span);
        if !text.ends_with("end") || !text.contains('\n') {
            return diag.help("add an `else` branch with the value to use");
        }
        let end_kw = Span { start: span.end - 3, ..span };
        let indent = self.indent_at(end_kw);
        let line_start =
            Span { start: end_kw.start - indent.len() as u32, end: end_kw.start - indent.len() as u32, ..span };
        diag.suggest(
            "add an `else` branch with the value to use",
            vec![wid_diagnostics::Edit { span: line_start, replacement: format!("{indent}else\n{indent}  …\n") }],
            Applicability::HasPlaceholders,
        )
    }

    fn lower_while(&mut self, cond: &ast::Cond, body: &[ast::Stmt], until: bool) {
        self.invalidate_assigned(body);
        let break_label = self.new_label();
        let continue_label = self.new_label();
        // The condition runs on each test, and the names its macro calls
        // declare are its own.
        let span = match cond {
            ast::Cond::Expr(e) => e.span,
            ast::Cond::Bind { value, .. } => value.span,
        };
        let operand = Operand { kind: OperandKind::Condition { until }, span };
        if let ast::Cond::Bind { name, value } = cond {
            self.begin_block();
            self.body.exits.push(Exit::Loop { break_label, continue_label });
            // The body sees the names, as it sees the bound one.
            let mut then = ir::Block::default();
            let (stmts, c) = self.lower_operand(operand, |this| {
                let (c, block) = this.lower_if_bind(*name, value, body, false, Dest::Discard);
                then = block;
                c
            });
            self.body.exits.pop();
            for s in stmts {
                self.emit(s);
            }
            self.emit(Stmt::If { cond: c, then, else_: ir::Block { stmts: vec![Stmt::Goto(break_label)] } });
            let block = self.end_block();
            self.emit(Stmt::Loop { body: block, continue_label, break_label });
            return;
        }
        self.begin_block();
        let (stmts, c) = self.lower_operand(operand, |this| this.cond_expr(cond, !until));
        for s in stmts {
            self.emit(s);
        }
        let skip = matches!(c.kind, ExprKind::Bool(false));
        if !skip {
            self.emit(Stmt::If {
                cond: c,
                then: ir::Block { stmts: vec![Stmt::Goto(break_label)] },
                else_: ir::Block::default(),
            });
        }
        self.body.exits.push(Exit::Loop { break_label, continue_label });
        self.push_scope();
        self.lower_stmts(body, Dest::Discard);
        self.pop_scope();
        self.body.exits.pop();
        let block = self.end_block();
        self.emit(Stmt::Loop { body: block, continue_label, break_label });
    }

    fn lower_loop(&mut self, body: &[ast::Stmt]) {
        self.invalidate_assigned(body);
        let break_label = self.new_label();
        let continue_label = self.new_label();
        self.begin_block();
        self.body.exits.push(Exit::Loop { break_label, continue_label });
        self.push_scope();
        self.lower_stmts(body, Dest::Discard);
        self.pop_scope();
        self.body.exits.pop();
        let block = self.end_block();
        self.emit(Stmt::Loop { body: block, continue_label, break_label });
    }

    fn lower_for(&mut self, f: &ast::ForExpr, span: Span) {
        self.invalidate_assigned(&f.body);
        let E::Range { lo: Some(lo), hi: Some(hi), inclusive } = &f.iter.kind else {
            let iter = self.expr(&f.iter, None);
            self.lower_for_collection(f, iter, span);
            return;
        };
        if f.bindings.len() != 1 {
            let first = f.bindings[0].name.span;
            let last = f.bindings[f.bindings.len() - 1].name.span;
            self.report(
                Diagnostic::error(codes::TYPE_MISMATCH, "a range loop binds one variable")
                    .primary(first.to(last), "the number is the only value a range gives")
                    .help("write `for i in a...b`; `for x, i in xs` gives indexes for arrays and slices"),
            );
            return;
        }
        if f.bindings[0].by_ref {
            let name = f.bindings[0].name;
            let amp = Span::new(name.span.file, name.span.start.saturating_sub(1), name.span.start);
            let mut diag = Diagnostic::error(codes::BY_REF_NOT_PLACE, "a range has no elements to bind by reference")
                .primary(name.span, "each number is a fresh value");
            if self.source_text(amp) == "&" {
                diag = diag.suggest_replace(
                    "bind the number by value",
                    amp,
                    String::new(),
                    wid_diagnostics::Applicability::MachineApplicable,
                );
            }
            self.report(diag);
            return;
        }
        let (lo_v, hi_v) = if super::expr::is_untyped(lo) && !super::expr::is_untyped(hi) {
            let h = self.expr(hi, None);
            let l = self.expr(lo, Some(h.ty));
            (l, h)
        } else {
            let l = self.expr(lo, None);
            let h = self.expr(hi, Some(l.ty));
            (l, h)
        };
        let ty = self.value_type(lo_v.ty, lo.span);
        if !self.types.is_int(ty) && !matches!(self.types.kind(ty), TyKind::Unknown) {
            let shown = self.types.display(ty);
            self.report(
                Diagnostic::error(codes::TYPE_MISMATCH, format!("ranges iterate over integers, found `{shown}`"))
                    .primary(lo.span, "not an integer"),
            );
            return;
        }
        let lo_v = self.coerce(lo_v, ty, lo.span);
        let hi_v = self.coerce(hi_v, ty, hi.span);
        let end = if hi_v.is_pure() { hi_v } else { self.spill(hi_v) };
        let counter = self.new_local(None, ty);
        self.emit(Stmt::Let { local: counter, init: Some(lo_v) });
        let counter_e = ir::Expr::new(ExprKind::Local(counter), ty);
        let bool_ty = self.types.bool();
        let compare = |op, rhs: &ir::Expr| {
            ir::Expr::new(
                ExprKind::Binary {
                    op,
                    lhs: Box::new(counter_e.clone()),
                    rhs: Box::new(rhs.clone()),
                    span: f.iter.span,
                },
                bool_ty,
            )
        };
        let done = compare(if *inclusive { ir::BinaryOp::Gt } else { ir::BinaryOp::Ge }, &end);
        let at_last = compare(ir::BinaryOp::Eq, &end);
        let break_label = self.new_label();
        let next_label = self.new_label();
        let loop_continue = self.new_label();

        // The body runs in a labeled block so `next` can jump to the step.
        self.begin_block();
        self.body.exits.push(Exit::Loop { break_label, continue_label: next_label });
        self.push_scope();
        let binding = f.bindings[0];
        let var = self.declare_var(binding.name.name, ty, binding.name.span, false);
        self.emit(Stmt::Let { local: var, init: Some(counter_e.clone()) });
        self.lower_stmts(&f.body, Dest::Discard);
        self.pop_scope();
        self.body.exits.pop();
        let body = self.end_block();

        let goto_break = || ir::Block { stmts: vec![Stmt::Goto(break_label)] };
        let mut stmts = vec![
            Stmt::If { cond: done, then: goto_break(), else_: ir::Block::default() },
            Stmt::Labeled { body, end_label: next_label },
        ];
        if *inclusive {
            stmts.push(Stmt::If { cond: at_last, then: goto_break(), else_: ir::Block::default() });
        }
        let step = ir::Expr::new(
            ExprKind::Binary {
                op: ir::BinaryOp::Add,
                lhs: Box::new(counter_e.clone()),
                rhs: Box::new(ir::Expr::new(ExprKind::Int(1), ty)),
                span: f.iter.span,
            },
            ty,
        );
        stmts.push(Stmt::Assign { target: counter_e, value: step });
        self.emit(Stmt::Loop { body: ir::Block { stmts }, continue_label: loop_continue, break_label });
    }
}

/// Builds `panic("message")` as a statement, for compiler-inserted checks.
fn panic_stmt(message: &str, span: Span) -> ast::Stmt {
    let callee = ast::Callee::Name(ast::Ident { name: wid_syntax::Name::new("panic"), span });
    let arg = ast::Arg {
        name: None,
        value: ast::Expr { kind: E::Str(vec![ast::StrPart::Text(message.to_string())]), span },
        splat: false,
    };
    let call = ast::Call { callee, args: vec![arg], block: None, parens: true };
    ast::Stmt { kind: S::Expr(ast::Expr { kind: E::Call(Box::new(call)), span }), span, attrs: Vec::new() }
}

//! Blocks and procs: methods that take a block are inlined at every call
//! site, `yield` lowers the caller's block in place, and `break`, `next` and
//! `return` find their targets on the exit stack. Procs (`->(x) { … }` and
//! `method(:name)`) become ordinary functions used through pointers.

use std::rc::Rc;

use wid_diagnostics::{Applicability, Diagnostic, Edit, Span, codes};
use wid_syntax::Name;
use wid_syntax::ast;

use super::body::{Dest, Exit, Frame, Var};
use super::{Checker, DeclId, DeclKind};
use crate::ir::{self, ExprKind, LabelId, LocalId, Stmt};
use crate::types::{Abi, ProcSig, TyId, TyKind};

/// The deepest chain of inlined block calls before the checker gives up.
const MAX_INLINE_DEPTH: usize = 32;

/// The block a frame's `yield` statements call.
#[derive(Clone, Debug)]
pub(crate) struct YieldTarget {
    pub block: Rc<ast::BlockArg>,
    pub params: Vec<TyId>,
    pub ret: TyId,
    /// The frame the block was written in; its body resolves names there.
    pub caller_frame: usize,
    /// The index of the call's `Exit::Region` on the exit stack.
    pub region_exit: usize,
}

/// The shape of a `&blk: block(…) -> R` parameter.
#[derive(Clone, Debug)]
pub(crate) struct BlockSig {
    pub params: Vec<TyId>,
    pub ret: TyId,
}

/// Where a `break` or `next` jumps.
enum LoopTarget {
    Loop { depth: usize, label: LabelId },
    Block { depth: usize, label: LabelId, result: Option<(LocalId, TyId)> },
}

impl<'a> Checker<'a> {
    // ----- exits ---------------------------------------------------------------

    /// Emits a return of `value` from the innermost function or inlined call
    /// of the current frame, running pending defers first.
    pub fn emit_return(&mut self, value: Option<ir::Expr>, span: Span) {
        let frame = self.body.frames.len().saturating_sub(1);
        let target = self.body.exits.iter().rposition(|e| match e {
            Exit::Function { frame: f } | Exit::Region { frame: f, .. } => *f == frame,
            _ => false,
        });
        let depth = target.unwrap_or(0);
        if !self.check_not_in_defer(depth, span, "return") {
            return;
        }
        let has_defers =
            self.body.exits[depth..].iter().any(|e| matches!(e, Exit::Scope { defers } if !defers.is_empty()));
        let value = match value {
            Some(v) if has_defers && !v.is_constant() => Some(self.spill(v)),
            other => other,
        };
        self.emit_defers_down_to(depth + 1);
        match self.body.exits.get(depth).cloned() {
            Some(Exit::Region { end_label, result, .. }) => {
                if let (Some((local, ty)), Some(v)) = (result, value) {
                    self.emit(Stmt::Assign { target: ir::Expr::new(ExprKind::Local(local), ty), value: v });
                }
                self.emit(Stmt::Goto(end_label));
            }
            _ => self.emit(Stmt::Return(value)),
        }
    }

    fn loop_target(&self, is_break: bool) -> Option<LoopTarget> {
        for (i, exit) in self.body.exits.iter().enumerate().rev() {
            match exit {
                Exit::Loop { break_label, continue_label } => {
                    let label = if is_break { *break_label } else { *continue_label };
                    return Some(LoopTarget::Loop { depth: i, label });
                }
                Exit::BlockBody { next_label, result, region, .. } => {
                    if !is_break {
                        return Some(LoopTarget::Block { depth: i, label: *next_label, result: *result });
                    }
                    if let Some(Exit::Region { end_label, result, .. }) = self.body.exits.get(*region) {
                        return Some(LoopTarget::Block { depth: *region, label: *end_label, result: *result });
                    }
                    return None;
                }
                Exit::Region { .. } | Exit::Function { .. } => return None,
                _ => {}
            }
        }
        None
    }

    /// Lowers `break` and `next`, inside loops and blocks.
    pub fn lower_loop_exit(&mut self, is_break: bool, value: Option<&ast::Expr>, span: Span) {
        let word = if is_break { "break" } else { "next" };
        let Some(target) = self.loop_target(is_break) else {
            self.report(
                Diagnostic::error(codes::LOOP_CONTROL_OUTSIDE_LOOP, format!("`{word}` outside of a loop or block"))
                    .primary(
                        span,
                        format!("there is no loop or block to {}", if is_break { "leave" } else { "continue" }),
                    )
                    .help("to leave the method early, use `return`"),
            );
            return;
        };
        match target {
            LoopTarget::Loop { depth, label } => {
                if let Some(v) = value {
                    self.report(
                        Diagnostic::error(
                            codes::RETURN_MISMATCH,
                            format!("`{word}` with a value only works inside a block"),
                        )
                        .primary(v.span, "loops don't produce values"),
                    );
                }
                if !self.check_not_in_defer(depth, span, word) {
                    return;
                }
                self.emit_defers_down_to(depth + 1);
                self.emit(Stmt::Goto(label));
            }
            LoopTarget::Block { depth, label, result } => {
                if !self.check_not_in_defer(depth, span, word) {
                    return;
                }
                match (value, result) {
                    (Some(v), Some((local, ty))) => {
                        let v = self.expr_coerced(v, ty);
                        self.emit(Stmt::Assign { target: ir::Expr::new(ExprKind::Local(local), ty), value: v });
                    }
                    (Some(v), None) => {
                        self.report(
                            Diagnostic::error(
                                codes::RETURN_MISMATCH,
                                format!("this `{word}` has a value, but nothing receives it"),
                            )
                            .primary(v.span, "the block or method returns nothing"),
                        );
                    }
                    (None, _) => {}
                }
                self.emit_defers_down_to(depth + 1);
                self.emit(Stmt::Goto(label));
            }
        }
    }

    // ----- block methods -----------------------------------------------------

    /// Resolves the `&blk: block(…) -> R` parameter of a method signature.
    pub fn block_sig(&mut self, block: &ast::BlockParamDecl, ctx: &super::ty::TyCtx) -> Option<BlockSig> {
        match &block.ty.kind {
            ast::TypeKind::Block { params, ret } => {
                let params = params.iter().map(|p| self.resolve_type(p, ctx)).collect();
                let ret = match ret {
                    Some(r) => self.resolve_type(r, ctx),
                    None => self.types.void(),
                };
                Some(BlockSig { params, ret })
            }
            ast::TypeKind::Error => None,
            _ => {
                self.report(
                    Diagnostic::error(codes::BLOCK_MISMATCH, "a `&` parameter must have a `block(…)` type")
                        .primary(block.ty.span, "expected something like `block(Int)` or `block(T) -> Bool`")
                        .note("blocks are inlined into the method; for a callback you can store, take a `proc(…)` parameter"),
                );
                None
            }
        }
    }

    /// Calls a method that takes a block by inlining its body at the call
    /// site, with `yield` running the block.
    pub fn inline_call(
        &mut self,
        decl: DeclId,
        subst: super::generics::Subst,
        values: Vec<ir::Expr>,
        block: &ast::BlockArg,
        name_span: Span,
        span: Span,
    ) -> ir::Expr {
        let _ = span;
        let sig = self.fn_sig_inst(decl, &subst);
        let fname = self.decls[decl.0 as usize].name;
        if let Some(pos) = self.inline_stack.iter().position(|d| *d == decl) {
            let cycle: Vec<DeclId> = self.inline_stack[pos..].to_vec();
            let mut key = cycle.clone();
            key.sort();
            if self.reported_inline_cycles.insert(key) {
                let diag = if cycle.len() == 1 {
                    Diagnostic::error(codes::RECURSIVE_INLINE, format!("`{fname}` takes a block and calls itself"))
                } else {
                    let mut path: Vec<String> =
                        cycle.iter().map(|d| format!("`{}`", self.decls[d.0 as usize].name)).collect();
                    path.push(format!("`{fname}`"));
                    Diagnostic::error(
                        codes::RECURSIVE_INLINE,
                        format!("methods that take blocks call each other: {}", path.join(" → ")),
                    )
                };
                self.report(
                    diag.primary(name_span, "methods with blocks are inlined at each call, so they cannot recurse")
                        .help("move the recursion into a helper method without a block, or loop instead"),
                );
            }
            return ir::Expr::new(ExprKind::Zero, sig.ret);
        }
        if self.inline_stack.len() >= MAX_INLINE_DEPTH {
            self.report(Diagnostic::error(codes::RECURSIVE_INLINE, "block calls are nested too deeply").primary(
                name_span,
                format!("more than {MAX_INLINE_DEPTH} methods with blocks are inlined into each other here"),
            ));
            return ir::Expr::new(ExprKind::Zero, sig.ret);
        }
        let locals: Vec<LocalId> = values
            .into_iter()
            .map(|v| {
                let ty = v.ty;
                let l = self.new_local(None, ty);
                self.emit(Stmt::Let { local: l, init: Some(v) });
                l
            })
            .collect();
        self.inline_body(decl, &sig, locals, Rc::new(block.clone()), subst, name_span)
    }

    /// Lowers a block method's body with its parameters bound to `locals`
    /// (receiver first, when it has one).
    fn inline_body(
        &mut self,
        decl: DeclId,
        sig: &super::FnSig,
        locals: Vec<LocalId>,
        block: Rc<ast::BlockArg>,
        subst: super::generics::Subst,
        call_site: Span,
    ) -> ir::Expr {
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Fn(f) = d.kind else { unreachable!("inline_body on a non-function") };
        let bsig = sig.block.clone().expect("inline_body on a method without a block");
        let void = self.types.void();
        let ret = sig.ret;
        let result = if ret == void {
            None
        } else {
            let l = self.new_local(None, ret);
            self.emit(Stmt::Let { local: l, init: None });
            Some((l, ret))
        };
        let end_label = self.new_label();
        let caller_frame = self.body.frames.len() - 1;
        let region_exit = self.body.exits.len();
        let target = self.yield_targets.len();
        self.yield_targets.push(YieldTarget { block, params: bsig.params, ret: bsig.ret, caller_frame, region_exit });
        let owner_ty = self.owner_type(decl).map(|t| self.subst_type(t, &subst));
        let (self_local, param_locals) =
            if sig.receiver.is_some() { (locals.first().copied(), &locals[1..]) } else { (None, &locals[..]) };
        let display = match owner_ty {
            Some(t) => format!("{}#{}", self.types.display(t), f.name.as_str()),
            None => f.name.as_str().to_string(),
        };
        let display_name = f.name.as_str().to_string();
        self.body.frames.push(Frame {
            loc: d.loc,
            scopes: Vec::new(),
            ret,
            self_ty: owner_ty,
            self_local,
            fn_name: display,
            block: Some(target),
            subst: subst.clone(),
            no_bounds: d.item.has_attr("no_bounds_check"),
            is_proc: false,
            decl: Some(decl),
            site: None,
        });
        let frame = self.body.frames.len() - 1;
        self.body.exits.push(Exit::Region { end_label, result, frame });
        self.inline_stack.push(decl);
        let generic_context = !subst.is_empty();
        if generic_context {
            let shown = self.instance_display(&display_name, &subst);
            self.instance_stack.push((shown, call_site, d.item.span, None));
        }
        self.begin_block();
        self.push_scope();
        for (p, local) in sig.params.iter().zip(param_locals) {
            let mark = self.mark_at(p.span);
            self.frame_mut().scopes.last_mut().expect("scope").vars.push(Var {
                name: p.name,
                local: *local,
                ty: p.ty,
                span: p.span,
                read: false,
                allow_unused: true,
                origin: None,
                decl_stmt: None,
                address_taken: false,
                indirect: false,
                mark,
                open: mark.is_some(),
            });
        }
        let dest = match result {
            Some((l, t)) => Dest::Local(l, t),
            None => Dest::Discard,
        };
        match &f.body {
            ast::FnBody::Block(stmts) => {
                self.lower_stmts(stmts, dest);
                let ends_with_value = matches!(stmts.last().map(|s| &s.kind), Some(ast::StmtKind::Expr(_)));
                if result.is_some() && !ends_with_value && !self.current_block_diverges() {
                    let ret_name = self.types.display(ret);
                    let span = stmts.last().map_or(f.sig_span, |s| s.span);
                    self.report(
                        Diagnostic::error(
                            codes::MISSING_RETURN,
                            format!("`{}` must return `{ret_name}`", f.name.as_str()),
                        )
                        .primary(span, "the method can reach its end without a value")
                        .secondary(f.sig_span, format!("declared to return `{ret_name}` here"))
                        .help("make the last expression the value to return, or add an explicit `return`"),
                    );
                }
            }
            ast::FnBody::Expr(e) => {
                let v = self.expr(e, result.map(|(_, t)| t));
                self.deliver(v, dest, e.span);
            }
        }
        self.pop_scope();
        let body = self.end_block();
        self.inline_stack.pop();
        if generic_context {
            self.instance_stack.pop();
        }
        self.body.exits.pop();
        self.body.frames.pop();
        self.emit(Stmt::Labeled { body, end_label });
        match result {
            Some((l, t)) => ir::Expr::new(ExprKind::Local(l), t),
            None => ir::Expr::new(ExprKind::Zero, void),
        }
    }

    /// The edit that adds the block parameter `param` to a method's
    /// signature: after its last parameter, or as a new parameter list.
    fn block_param_edit(&self, decl: DeclId, param: &str) -> Option<wid_diagnostics::Edit> {
        let DeclKind::Fn(f) = self.decls[decl.0 as usize].kind else { return None };
        if let Some(last) = f.params.last() {
            return Some(wid_diagnostics::Edit { span: last.span.shrink_to_end(), replacement: format!(", {param}") });
        }
        let after_name = f.name.span.shrink_to_end();
        let rest = self.source_text(Span { start: f.name.span.end, end: f.sig_span.end, ..f.name.span });
        if rest.trim_start().starts_with("()") {
            let open = f.name.span.end + (rest.len() - rest.trim_start().len()) as u32 + 1;
            return Some(wid_diagnostics::Edit {
                span: Span { start: open, end: open, ..f.name.span },
                replacement: param.to_string(),
            });
        }
        Some(wid_diagnostics::Edit { span: after_name, replacement: format!("({param})") })
    }

    /// Lowers `yield args`: runs the caller's block in place, in the
    /// caller's scope, with the block parameters bound to `args`.
    pub fn lower_yield(&mut self, args: &[ast::Expr], span: Span) -> ir::Expr {
        let void = self.types.void();
        let Some(target_index) = self.frame().block else {
            let mut shown = Vec::new();
            for a in args {
                let v = self.expr(a, None);
                shown.push(self.types.display(v.ty));
            }
            let name = self.frame().fn_name.clone();
            let diag = if self.frame().is_proc {
                Diagnostic::error(codes::YIELD_OUTSIDE_BLOCK_METHOD, "a proc cannot `yield`")
                    .primary(span, "procs take no block")
                    .help("pass the code to run as a proc parameter, like `->(f: proc(Int)) { f.call(1) }`")
            } else {
                let diag = Diagnostic::error(
                    codes::YIELD_OUTSIDE_BLOCK_METHOD,
                    format!("`{name}` has no block parameter to `yield` to"),
                )
                .primary(span, "nothing to yield to");
                let param = format!("&blk: block({})", shown.join(", "));
                let decl = self.frame().decl.filter(|d| self.decls[d.0 as usize].name.as_str() != "main");
                match decl.and_then(|d| self.block_param_edit(d, &param)) {
                    Some(edit) => diag.suggest(
                        format!("declare the block in the signature: `{param}`"),
                        vec![edit],
                        Applicability::MaybeIncorrect,
                    ),
                    None if name == "main" => {
                        diag.help("`main` takes no block; call a method that takes a block, or loop here instead")
                    }
                    None => diag.help(format!("declare the block in the signature, like `{param}`")),
                }
            };
            self.report(diag);
            return ir::Expr::new(ExprKind::Zero, void);
        };
        let target = self.yield_targets[target_index].clone();
        if args.len() != target.params.len() {
            self.report(
                Diagnostic::error(
                    codes::ARG_COUNT,
                    format!(
                        "this block takes {} value{}, but {} {} yielded",
                        target.params.len(),
                        plural(target.params.len()),
                        args.len(),
                        if args.len() == 1 { "is" } else { "are" }
                    ),
                )
                .primary(span, "check the block type in the method signature"),
            );
        }
        let block = target.block.clone();
        let mut values = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            let Some(&ty) = target.params.get(i) else {
                self.expr(arg, None);
                continue;
            };
            let by_ref = block.params.get(i).is_some_and(|p| p.by_ref);
            let v = self.expr_coerced(arg, ty);
            if by_ref {
                if !super::members::is_place(&v) {
                    let pname = block.params[i].name;
                    self.report(
                        Diagnostic::error(
                            codes::BY_REF_NOT_PLACE,
                            format!("`|&{}|` needs a variable, field or element", pname.as_str()),
                        )
                        .primary(arg.span, "this yields a temporary value")
                        .secondary(pname.span, "bound by reference here")
                        .suggest(
                            "bind it by value instead",
                            vec![Edit {
                                span: Span::new(pname.span.file, pname.span.start - 1, pname.span.start),
                                replacement: String::new(),
                            }],
                            Applicability::MaybeIncorrect,
                        ),
                    );
                    values.push((v, false));
                    continue;
                }
                let ptr = self.address_of(v);
                values.push((self.spill(ptr), true));
            } else {
                let v = if v.is_constant() { v } else { self.spill(v) };
                values.push((v, false));
            }
        }
        let result = if target.ret == void {
            None
        } else {
            let l = self.new_local(None, target.ret);
            self.emit(Stmt::Let { local: l, init: None });
            Some((l, target.ret))
        };
        let next_label = self.new_label();
        let callee_frames = self.body.frames.split_off(target.caller_frame + 1);
        self.body.exits.push(Exit::BlockBody { next_label, result, region: target.region_exit });
        self.begin_block();
        self.push_scope();
        for (i, p) in block.params.iter().enumerate() {
            let Some((v, by_ref)) = values.get(i).cloned() else {
                // A parameter the block type declares but this `yield` leaves
                // out was already reported at the `yield` (E0302).
                let declared = target.params.get(i).copied();
                if declared.is_none() {
                    let n = target.params.len();
                    self.report(
                        Diagnostic::error(
                            codes::BLOCK_MISMATCH,
                            format!("block parameter `{}` receives nothing", p.name.as_str()),
                        )
                        .primary(p.name.span, format!("the method yields {n} value{}", plural(n))),
                    );
                }
                let ty = declared.unwrap_or_else(|| self.types.unknown());
                let local = self.declare_var(p.name.name, ty, p.name.span, true);
                self.emit(Stmt::Let { local, init: None });
                continue;
            };
            let elem = target.params.get(i).copied().unwrap_or(v.ty);
            let local = self.declare_var(p.name.name, elem, p.name.span, false);
            if by_ref {
                self.body.locals[local.0 as usize].ty = v.ty;
                self.set_indirect(local);
            }
            self.emit(Stmt::Let { local, init: Some(v) });
        }
        let dest = match result {
            Some((l, t)) => Dest::Local(l, t),
            None => Dest::Discard,
        };
        self.lower_stmts(&block.body, dest);
        self.pop_scope();
        let body = self.end_block();
        self.body.exits.pop();
        self.body.frames.extend(callee_frames);
        self.emit(Stmt::Labeled { body, end_label: next_label });
        match result {
            Some((l, t)) => ir::Expr::new(ExprKind::Local(l), t),
            None => ir::Expr::new(ExprKind::Zero, void),
        }
    }

    /// Checks a block method that may never be called, by inlining it once
    /// with an empty block and discarding the code.
    pub fn check_block_method(&mut self, decl: DeclId) {
        let sig = self.fn_sig(decl);
        let Some(bsig) = sig.block.clone() else { return };
        let d = self.decls[decl.0 as usize].clone();
        let span = d.span;
        let saved = std::mem::take(&mut self.body);
        let void = self.types.void();
        self.body.frames.push(Frame {
            loc: d.loc,
            scopes: Vec::new(),
            ret: void,
            self_ty: None,
            self_local: None,
            fn_name: d.name.as_str().to_string(),
            block: None,
            subst: Default::default(),
            no_bounds: d.item.has_attr("no_bounds_check"),
            is_proc: false,
            decl: Some(decl),
            site: None,
        });
        self.body.exits.push(Exit::Function { frame: 0 });
        self.begin_block();
        self.push_scope();
        let mut locals = Vec::new();
        if let Some(recv) = sig.receiver {
            let ptr = self.types.pointer(recv);
            locals.push(self.new_local(None, ptr));
        }
        for p in &sig.params {
            locals.push(self.new_local(None, p.ty));
        }
        let params = (0..bsig.params.len())
            .map(|i| ast::BlockParam {
                name: ast::Ident { name: Name::new(&format!("_block_param_{i}")), span },
                by_ref: false,
            })
            .collect();
        let block = Rc::new(ast::BlockArg { params, body: Vec::new(), span });
        let _ = self.inline_body(decl, &sig, locals, block, Default::default(), Span::default());
        self.pop_scope();
        self.end_block();
        self.body = saved;
    }

    // ----- procs ---------------------------------------------------------------

    /// Lowers `->(x: Int) -> Int { … }` into a separate function.
    pub fn lower_lambda(&mut self, lambda: &ast::Lambda, expected: Option<TyId>, span: Span) -> ir::Expr {
        let loc = self.loc();
        // A proc in a macro's code keeps resolving the code spliced into it
        // where the macro was called.
        let (base_loc, site) = (self.frame().loc, self.frame().site);
        let ctx = self.body_ctx();
        let expected_sig = expected.and_then(|t| match self.types.kind(t) {
            TyKind::Proc(sig) => Some(sig.clone()),
            _ => None,
        });
        // A parameter is declared at its name, which decides the code that
        // sees it: a name spliced into a macro's code is the caller's.
        let params: Vec<(Name, TyId, Span)> =
            lambda.params.iter().map(|p| (p.name.name, self.resolve_type(&p.ty, &ctx), p.name.span)).collect();
        let ret = match &lambda.ret {
            Some(r) => self.resolve_type(r, &ctx),
            None => expected_sig.as_ref().map_or_else(|| self.types.void(), |s| s.ret),
        };
        let visible: Vec<Name> =
            self.body.frames.iter().flat_map(|f| f.scopes.iter().flat_map(|s| s.vars.iter().map(|v| v.name))).collect();
        let no_bounds = self.body.frames.last().is_some_and(|f| f.no_bounds);
        let saved = std::mem::take(&mut self.body);
        let saved_capturable = std::mem::replace(&mut self.capturable, visible);
        let saved_kind = std::mem::replace(&mut self.capture_kind, super::comptime::CaptureKind::Proc);
        self.lambda_count += 1;
        let prefix = self.pkg_prefix(loc.pkg);
        let c_name = format!("{prefix}__proc_{}", self.lambda_count);
        let mut func = self.new_function_shell("proc".into(), c_name, ret, span);
        self.body.frames.push(Frame {
            loc: base_loc,
            scopes: Vec::new(),
            ret,
            self_ty: None,
            self_local: None,
            fn_name: "proc".into(),
            block: None,
            subst: Default::default(),
            no_bounds,
            is_proc: true,
            decl: None,
            site,
        });
        self.body.exits.push(Exit::Function { frame: 0 });
        self.begin_block();
        self.push_scope();
        for (name, ty, pspan) in &params {
            let local = self.declare_var(*name, *ty, *pspan, true);
            func.params.push(local);
        }
        let void = self.types.void();
        let dest = if ret == void { Dest::Discard } else { Dest::Return };
        self.lower_stmts(&lambda.body, dest);
        if ret != void && !self.current_block_diverges() {
            self.report(
                Diagnostic::error(
                    codes::MISSING_RETURN,
                    format!("this proc must return `{}`", self.types.display(ret)),
                )
                .primary(span, "it can reach its end without a value")
                .help("end the proc with the value to return, or add a `return` on every path"),
            );
            self.emit(Stmt::Unreachable);
        }
        self.pop_scope();
        let block = self.end_block();
        let body = std::mem::replace(&mut self.body, saved);
        self.capturable = saved_capturable;
        self.capture_kind = saved_kind;
        for name in std::mem::take(&mut self.captured) {
            if let Some(var) = self.find_var(name) {
                var.read = true;
            }
        }
        func.locals = body.locals;
        func.body = Some(block);
        let id = ir::FnId(self.functions.len() as u32);
        self.functions.push(Some(func));
        let proc_ty = self.types.intern(TyKind::Proc(ProcSig {
            params: params.iter().map(|(_, t, _)| *t).collect(),
            ret,
            abi: Abi::Wid,
            variadic: false,
        }));
        ir::Expr::new(ExprKind::FnRef(id), proc_ty)
    }

    /// Reports a proc body that reads a local of the code around it.
    pub fn report_capture(&mut self, name: Name, span: Span) -> bool {
        if !self.capturable.contains(&name) {
            return false;
        }
        self.captured.push(name);
        if self.capture_kind == super::comptime::CaptureKind::Comptime {
            self.report(
                Diagnostic::error(codes::COMPTIME_ONLY, format!("`comptime` code can't use the variable `{name}`"))
                    .primary(span, "this variable only exists when the program runs")
                    .note("`comptime` code runs while compiling, before any variable has a value")
                    .help("use constants and literals inside `comptime`, or compute the value at run time"),
            );
            return true;
        }
        self.report(
            Diagnostic::error(codes::PROC_CAPTURE, format!("this proc uses `{name}` from the code around it"))
                .primary(span, "procs cannot capture local variables")
                .note(format!("a proc is a plain function pointer; it has nowhere to keep `{name}` alive"))
                .help("pass the value as a parameter, use a block (`do |x| … end`) when the callee yields, or store it in a struct the proc receives"),
        );
        true
    }

    /// Lowers `method(:name)`: a package function as a proc value. A generic
    /// function is instantiated for the expected proc type.
    pub fn method_ref(&mut self, args: &[ast::Arg], span: Span, expected: Option<TyId>) -> ir::Expr {
        let unknown = self.types.unknown();
        let [arg] = args else {
            self.report(
                Diagnostic::error(codes::ARG_COUNT, "`method` takes one symbol")
                    .primary(span, "like `method(:on_hit)`"),
            );
            return ir::Expr::new(ExprKind::Zero, unknown);
        };
        let ast::ExprKind::Symbol(name) = arg.value.kind else {
            self.report(
                Diagnostic::error(codes::TYPE_MISMATCH, "`method` takes the method's name as a symbol")
                    .primary(arg.value.span, "write it like `method(:on_hit)`"),
            );
            return ir::Expr::new(ExprKind::Zero, unknown);
        };
        let loc = self.loc();
        let decl = self
            .frame()
            .self_ty
            .and_then(|t| self.find_method(t, name))
            .filter(|d| matches!(self.decls[d.0 as usize].kind, DeclKind::Fn(f) if f.is_static))
            .or_else(|| self.lookup_pkg(loc.pkg, name))
            .or_else(|| self.lookup_prelude(name));
        let Some(decl) = decl.filter(|d| matches!(self.decls[d.0 as usize].kind, DeclKind::Fn(_))) else {
            let candidates = self.package_names(loc.pkg);
            self.undefined(name, arg.value.span, candidates, "method");
            return ir::Expr::new(ExprKind::Zero, unknown);
        };
        if self.is_macro(decl) {
            self.report(
                Diagnostic::error(codes::NOT_A_VALUE, format!("the macro `{name}` cannot be used as a proc"))
                    .primary(arg.value.span, "a macro runs while compiling and leaves code behind")
                    .secondary(self.decls[decl.0 as usize].span, "declared as a `macro def` here")
                    .note("a proc is a method the program calls while it runs; a macro has no such method")
                    .help(format!("call `{name}` where its code should go, or write a `def` that a proc can name")),
            );
            return ir::Expr::new(ExprKind::Zero, unknown);
        }
        let sig = self.fn_sig(decl);
        if sig.block.is_some() || sig.receiver.is_some() {
            self.report(
                Diagnostic::error(codes::NOT_A_VALUE, format!("`{name}` cannot be used as a proc"))
                    .primary(
                        arg.value.span,
                        if sig.block.is_some() { "it takes a block" } else { "it needs a receiver" },
                    )
                    .help("procs are plain functions: package methods or `def self.` methods"),
            );
            return ir::Expr::new(ExprKind::Zero, unknown);
        }
        let abi = self.fn_abi(decl);
        if !self.decl_is_concrete(decl) {
            let want = expected.map(|t| self.types.kind(self.types.base(t)).clone());
            let Some(TyKind::Proc(want)) = want else {
                let params: Vec<String> = sig.params.iter().map(|p| self.types.display(p.ty)).collect();
                let int = self.types.int();
                let example: Vec<(Name, TyId)> = self.generic_names(decl).into_iter().map(|n| (n, int)).collect();
                let example_tys: Vec<TyId> = sig.params.iter().map(|p| self.subst_type(p.ty, &example)).collect();
                let example_params: Vec<String> = example_tys.iter().map(|t| self.types.display(*t)).collect();
                let example_ret = self.subst_type(sig.ret, &example);
                let ret_text = if matches!(self.types.kind(example_ret), TyKind::Void) {
                    String::new()
                } else {
                    format!(" -> {}", self.types.display(example_ret))
                };
                self.report(
                    Diagnostic::error(codes::CANNOT_INFER, format!("cannot tell which `{name}` to use as a proc"))
                        .primary(arg.value.span, format!("`{name}` is generic: ({})", params.join(", ")))
                        .note("a generic method has one instance per set of type arguments; the proc type picks one")
                        .help(format!(
                            "use it where a proc type is expected, for example `f: proc({}){ret_text} = method(:{name})`",
                            example_params.join(", ")
                        )),
                );
                return ir::Expr::new(ExprKind::Zero, unknown);
            };
            let mut bindings = Vec::new();
            let fits = want.params.len() == sig.params.len()
                && sig.params.iter().zip(&want.params).all(|(p, w)| self.unify(p.ty, *w, &mut bindings))
                && self.unify(sig.ret, want.ret, &mut bindings);
            if !fits {
                let shown = self.types.display(expected.unwrap_or(unknown));
                let params: Vec<String> = sig.params.iter().map(|p| self.types.display(p.ty)).collect();
                self.report(
                    Diagnostic::error(codes::TYPE_MISMATCH, format!("`{name}` does not fit `{shown}`"))
                        .primary(arg.value.span, format!("`{name}` takes ({})", params.join(", ")))
                        .help("change the proc type to match the method's parameters"),
                );
                return ir::Expr::new(ExprKind::Zero, unknown);
            }
            let Some(subst) = self.finish_bindings(decl, bindings, arg.value.span) else {
                return ir::Expr::new(ExprKind::Zero, unknown);
            };
            let inst = self.fn_sig_inst(decl, &subst);
            let func = self.fn_instance_with(decl, subst, span);
            let ty = self.types.intern(TyKind::Proc(ProcSig {
                params: inst.params.iter().map(|p| p.ty).collect(),
                ret: inst.ret,
                abi,
                variadic: false,
            }));
            return ir::Expr::new(ExprKind::FnRef(func), ty);
        }
        let func = self.fn_instance(decl);
        let ty = self.types.intern(TyKind::Proc(ProcSig {
            params: sig.params.iter().map(|p| p.ty).collect(),
            ret: sig.ret,
            abi,
            variadic: false,
        }));
        ir::Expr::new(ExprKind::FnRef(func), ty)
    }

    /// Returns the calling convention a function declaration uses.
    pub fn fn_abi(&self, decl: DeclId) -> Abi {
        let item = self.decls[decl.0 as usize].item;
        if item.has_attr("c") || item.has_attr("extern") { Abi::C } else { Abi::Wid }
    }

    /// Calls a proc value with arguments.
    pub fn call_proc(&mut self, callee: ir::Expr, args: &[ast::Arg], span: Span) -> ir::Expr {
        let TyKind::Proc(sig) = self.types.kind(self.types.base(callee.ty)).clone() else {
            let shown = self.types.display(callee.ty);
            self.report(
                Diagnostic::error(codes::NOT_CALLABLE, format!("a value of type `{shown}` cannot be called"))
                    .primary(span, "not a proc"),
            );
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        };
        if args.len() != sig.params.len() {
            self.report(
                Diagnostic::error(
                    codes::ARG_COUNT,
                    format!(
                        "this proc takes {} argument{}, but {} {} given",
                        sig.params.len(),
                        plural(sig.params.len()),
                        args.len(),
                        if args.len() == 1 { "was" } else { "were" }
                    ),
                )
                .primary(span, format!("its type is `{}`", self.types.display(callee.ty))),
            );
        }
        let callee = if callee.is_pure() { callee } else { self.spill(callee) };
        let mut lowered: Vec<ir::Expr> = Vec::new();
        for (arg, ty) in args.iter().zip(&sig.params) {
            if let Some(n) = arg.name {
                self.report(
                    Diagnostic::error(codes::BAD_NAMED_ARG, "procs take positional arguments only")
                        .primary(n.span, "remove the name"),
                );
            }
            self.begin_block();
            let v = self.expr_coerced(&arg.value, *ty);
            let stmts = self.end_block().stmts;
            if !stmts.is_empty() || !v.is_pure() {
                self.spill_impure(&mut lowered);
            }
            for s in stmts {
                self.emit(s);
            }
            lowered.push(v);
        }
        ir::Expr::new(ExprKind::CallIndirect { callee: Box::new(callee), args: lowered, span }, sig.ret)
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

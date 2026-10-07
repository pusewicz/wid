//! Per-function lowering state: locals, statement buffers, lexical frames
//! and the dynamic exit stack used for `defer`, loops and returns.

use wid_diagnostics::{Applicability, Diagnostic, Edit, Span, codes};
use wid_syntax::Name;
use wid_syntax::ast;

use super::{Checker, DeclId, DeclKind, DeclLoc, mangle_ident};
use crate::ir::{self, LabelId, LocalId, Stmt};
use crate::types::{TyId, TyKind};

/// A variable visible in a lexical scope.
#[derive(Clone, Debug)]
pub(crate) struct Var {
    pub name: Name,
    pub local: LocalId,
    pub ty: TyId,
    pub span: Span,
    pub read: bool,
    /// Parameters and `_`-prefixed names are never reported as unused.
    pub allow_unused: bool,
    /// Source of the initializer when it was a call, for nil diagnostics.
    pub origin: Option<String>,
    /// The statement that declared the variable, for guard suggestions.
    pub decl_stmt: Option<Span>,
    /// Set once `&x` is taken; such variables never narrow again.
    pub address_taken: bool,
    /// The local holds a pointer; reads and writes go through it (`for &x`).
    pub indirect: bool,
    /// The macro expansion whose own code declared the variable, or `None`
    /// for code written in a file. Only code of the same expansion sees it
    /// (hygiene; see `check/macros.rs`).
    pub mark: Option<u32>,
    /// A parameter of a method a macro generated: part of the method's
    /// interface, so code spliced in from the macro's call site sees it
    /// too.
    pub open: bool,
}

/// A lexical scope.
#[derive(Clone, Debug, Default)]
pub(crate) struct Scope {
    pub vars: Vec<Var>,
    /// Optional locals known to hold a value for the rest of this scope.
    pub narrowed: Vec<LocalId>,
    /// Union locals known to hold a particular variant in this scope.
    pub variants: Vec<(LocalId, u32)>,
    /// Whether this scope changed the context and runs on a private copy.
    pub context_shadowed: bool,
}

/// One lexical context: the body of a function being lowered.
#[derive(Clone, Debug)]
pub(crate) struct Frame {
    pub loc: DeclLoc,
    pub scopes: Vec<Scope>,
    pub ret: TyId,
    pub self_ty: Option<TyId>,
    pub self_local: Option<LocalId>,
    /// The function's display name, for messages.
    pub fn_name: String,
    /// For an inlined block method, the block its `yield` runs.
    pub block: Option<usize>,
    /// Bindings of the generic parameters of the code being lowered.
    pub subst: super::generics::Subst,
    /// Whether the code is inside a `@[no_bounds_check]` method.
    pub no_bounds: bool,
    /// Whether this frame is a proc body, which cannot `yield`.
    pub is_proc: bool,
    /// The method whose body this frame lowers, if any.
    pub decl: Option<super::DeclId>,
    /// While code a macro generated is being lowered, the virtual file its
    /// spans are in (an index into `MacroState::files`); `None` for code
    /// written in a file. It decides where names resolve ([`Checker::loc`])
    /// and which variables are visible ([`Checker::find_var`]).
    pub site: Option<u32>,
}

/// An entry of the dynamic exit stack.
#[derive(Clone, Debug)]
pub(crate) enum Exit {
    /// A lexical scope with its pending `defer` bodies.
    Scope { defers: Vec<ir::Block> },
    /// A loop that `break` and `next` target.
    Loop { break_label: LabelId, continue_label: LabelId },
    /// The start of the function; `frame` is always 0.
    Function { frame: usize },
    /// An inlined call of a method that takes a block. `return` in the
    /// callee frame jumps to `end_label`, storing the value in `result`.
    Region { end_label: LabelId, result: Option<(LocalId, TyId)>, frame: usize },
    /// A block body inlined at a `yield`. `next` jumps to `next_label`;
    /// `break` leaves the call whose region is at exit index `region`.
    BlockBody { next_label: LabelId, result: Option<(LocalId, TyId)>, region: usize },
    /// Inside a `defer` body, where control may not leave.
    Defer,
}

/// The state of the function currently being lowered.
#[derive(Debug, Default)]
pub(crate) struct Body {
    pub locals: Vec<ir::Local>,
    pub next_label: u32,
    pub blocks: Vec<Vec<Stmt>>,
    pub frames: Vec<Frame>,
    pub exits: Vec<Exit>,
}

/// Where the value of a statement list goes.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Dest {
    /// The value is discarded.
    Discard,
    /// The value is assigned to a local.
    Local(LocalId, TyId),
    /// The value is returned from the function.
    Return,
}

impl<'a> Checker<'a> {
    // ----- buffers and locals -------------------------------------------

    pub fn emit(&mut self, stmt: Stmt) {
        self.body.blocks.last_mut().expect("a block is open").push(stmt);
    }

    pub fn begin_block(&mut self) {
        self.body.blocks.push(Vec::new());
    }

    pub fn end_block(&mut self) -> ir::Block {
        ir::Block { stmts: self.body.blocks.pop().expect("a block is open") }
    }

    pub fn new_local(&mut self, name: Option<Name>, ty: TyId) -> LocalId {
        let id = LocalId(self.body.locals.len() as u32);
        self.body.locals.push(ir::Local { name, ty });
        id
    }

    pub fn new_label(&mut self) -> LabelId {
        let id = LabelId(self.body.next_label);
        self.body.next_label += 1;
        id
    }

    /// Stores `value` in a fresh temporary and returns an expression that
    /// reads it.
    pub fn spill(&mut self, value: ir::Expr) -> ir::Expr {
        let ty = value.ty;
        let local = self.new_local(None, ty);
        self.emit(Stmt::Let { local, init: Some(value) });
        ir::Expr::new(ir::ExprKind::Local(local), ty)
    }

    pub fn frame(&self) -> &Frame {
        self.body.frames.last().expect("a frame is active")
    }

    pub fn frame_mut(&mut self) -> &mut Frame {
        self.body.frames.last_mut().expect("a frame is active")
    }

    /// Where the names of the code being lowered resolve: the frame's
    /// package and file, or for a macro's own code, the macro's.
    pub fn loc(&self) -> DeclLoc {
        let frame = self.frame();
        frame.site.and_then(|v| self.macros.files.get(v as usize)).map_or(frame.loc, |v| v.loc)
    }

    /// Where a name written at `span` resolves: like [`Checker::loc`], but
    /// for the name's own span, which differs from the code around it for
    /// a name spliced into a macro's code.
    pub fn loc_at(&self, span: Span) -> DeclLoc {
        if let Some(v) = self.virtual_file(span.file) {
            return v.loc;
        }
        match self.body.frames.last() {
            Some(_) if span == Span::default() => self.loc(),
            Some(frame) => frame.loc,
            None => self
                .macros
                .file_locs
                .get(&span.file)
                .copied()
                .unwrap_or(DeclLoc { pkg: crate::input::PackageId(0), file: 0 }),
        }
    }

    /// The type context of the code being lowered.
    pub fn body_ctx(&self) -> super::ty::TyCtx {
        let frame = self.frame();
        super::ty::TyCtx { loc: frame.loc, self_ty: frame.self_ty, subst: frame.subst.clone() }
    }

    // ----- scopes --------------------------------------------------------

    pub fn push_scope(&mut self) {
        self.frame_mut().scopes.push(Scope::default());
        self.body.exits.push(Exit::Scope { defers: Vec::new() });
    }

    /// Closes the innermost scope: runs its defers (unless control already
    /// left) and reports unused variables.
    pub fn pop_scope(&mut self) {
        let exit = self.body.exits.pop();
        if let Some(Exit::Scope { defers }) = exit
            && !self.current_block_diverges()
        {
            for d in defers.into_iter().rev() {
                let d = self.relabel(&d);
                self.emit(Stmt::Scope(d));
            }
        }
        let shadowed = self.frame().scopes.last().is_some_and(|s| s.context_shadowed);
        if shadowed {
            let block = self.end_block();
            self.emit(Stmt::WithContext(block));
        }
        let scope = self.frame_mut().scopes.pop().unwrap_or_default();
        self.report_unused(&scope);
    }

    fn report_unused(&mut self, scope: &Scope) {
        for var in &scope.vars {
            if var.read || var.allow_unused || var.name.as_str().starts_with('_') {
                continue;
            }
            let text = var.name.as_str();
            self.report(
                Diagnostic::error(codes::UNUSED_VARIABLE, format!("`{text}` is assigned but never read"))
                    .primary(var.span, "this value is never used")
                    .suggest(
                        "if this is intentional, prefix the name with an underscore",
                        vec![Edit { span: var.span, replacement: format!("_{text}") }],
                        Applicability::MaybeIncorrect,
                    ),
            );
        }
    }

    /// Declares a variable in the innermost scope. `span` is the name's: it
    /// decides which code sees the variable (see [`Var::mark`]).
    pub fn declare_var(&mut self, name: Name, ty: TyId, span: Span, allow_unused: bool) -> LocalId {
        let local = self.new_local(Some(name), ty);
        let mark = self.mark_at(span);
        let scope = self.frame_mut().scopes.last_mut().expect("a scope is open");
        scope.vars.push(Var {
            name,
            local,
            ty,
            span,
            read: false,
            allow_unused,
            origin: None,
            decl_stmt: None,
            address_taken: false,
            indirect: false,
            mark,
            open: false,
        });
        local
    }

    /// Declares a parameter of the method being lowered. A generated
    /// method's parameters are open to the macro's caller (see
    /// [`Var::open`]).
    pub fn declare_param(&mut self, name: Name, ty: TyId, span: Span) -> LocalId {
        let local = self.declare_var(name, ty, span, true);
        if let Some(var) = self.frame_mut().scopes.last_mut().and_then(|s| s.vars.last_mut()) {
            var.open = var.mark.is_some();
        }
        local
    }

    /// Finds a variable by name in the current frame, innermost first, as
    /// the code being lowered sees it.
    pub fn find_var(&mut self, name: Name) -> Option<&mut Var> {
        let mark = self.site_mark();
        self.find_marked_var(name, mark)
    }

    /// Finds the variable a name written at `span` refers to.
    pub fn find_var_at(&mut self, name: Name, span: Span) -> Option<&mut Var> {
        let mark = self.mark_at(span);
        self.find_marked_var(name, mark)
    }

    fn find_marked_var(&mut self, name: Name, mark: Option<u32>) -> Option<&mut Var> {
        let macros = &self.macros;
        // The hygiene mark of the code that called expansion `e`.
        let caller = |e: u32| {
            let call = macros.expansions.get(e as usize)?.call_site;
            macros.files.get(call.file.expansion_index()? as usize).map(|v| v.expansion)
        };
        let visible = |v: &Var| v.mark == mark || (v.open && v.mark.and_then(caller) == mark);
        let frame = self.body.frames.last_mut()?;
        frame.scopes.iter_mut().rev().find_map(|s| s.vars.iter_mut().rev().find(|v| v.name == name && visible(v)))
    }

    /// The hygiene mark of the code being lowered: its macro expansion.
    fn site_mark(&self) -> Option<u32> {
        let site = self.body.frames.last()?.site?;
        self.macros.files.get(site as usize).map(|v| v.expansion)
    }

    /// The hygiene mark of code written at `span`.
    pub fn mark_at(&self, span: Span) -> Option<u32> {
        if span == Span::default() {
            return self.site_mark();
        }
        self.virtual_file(span.file).map(|v| v.expansion)
    }

    /// Makes the code at `span` the code being lowered (see [`Frame::site`])
    /// and returns what to restore with [`Checker::leave_site`].
    pub fn enter_site(&mut self, span: Span) -> Option<Option<u32>> {
        if span == Span::default() {
            return None;
        }
        let site = span.file.expansion_index().filter(|i| (*i as usize) < self.macros.files.len());
        let frame = self.body.frames.last_mut()?;
        Some(std::mem::replace(&mut frame.site, site))
    }

    /// Undoes [`Checker::enter_site`].
    pub fn leave_site(&mut self, saved: Option<Option<u32>>) {
        if let Some(site) = saved
            && let Some(frame) = self.body.frames.last_mut()
        {
            frame.site = site;
        }
    }

    /// Returns true when an optional local is known to hold a value here.
    pub fn is_narrowed(&self, local: LocalId) -> bool {
        self.body.frames.last().is_some_and(|f| f.scopes.iter().any(|s| s.narrowed.contains(&local)))
    }

    /// Records that `local` holds a value for the rest of the innermost scope.
    pub fn narrow(&mut self, local: LocalId) {
        let taken =
            self.body.frames.last().is_some_and(|f| {
                f.scopes.iter().flat_map(|s| s.vars.iter()).any(|v| v.local == local && v.address_taken)
            });
        if taken {
            return;
        }
        if let Some(scope) = self.frame_mut().scopes.last_mut()
            && !scope.narrowed.contains(&local)
        {
            scope.narrowed.push(local);
        }
    }

    /// Forgets every narrowing of `local`, after it is assigned.
    pub fn invalidate(&mut self, local: LocalId) {
        if let Some(frame) = self.body.frames.last_mut() {
            for scope in &mut frame.scopes {
                scope.narrowed.retain(|l| *l != local);
                scope.variants.retain(|(l, _)| *l != local);
            }
        }
    }

    /// Returns the union variant a local is known to hold, if any.
    pub fn narrowed_variant(&self, local: LocalId) -> Option<u32> {
        self.body
            .frames
            .last()
            .and_then(|f| f.scopes.iter().rev().find_map(|s| s.variants.iter().rev().find(|(l, _)| *l == local)))
            .map(|(_, v)| *v)
    }

    /// Records that a union local holds `variant` for the rest of the scope.
    pub fn narrow_variant(&mut self, local: LocalId, variant: u32) {
        if let Some(scope) = self.frame_mut().scopes.last_mut() {
            scope.variants.push((local, variant));
        }
    }

    /// Lists visible variable names, for suggestions.
    pub fn visible_var_names(&self) -> Vec<&'static str> {
        self.body
            .frames
            .last()
            .map(|f| f.scopes.iter().flat_map(|s| s.vars.iter().map(|v| v.name.as_str())).collect())
            .unwrap_or_default()
    }

    // ----- exits ---------------------------------------------------------

    /// Returns true when the statements emitted so far in the innermost
    /// buffer never fall through.
    pub fn current_block_diverges(&self) -> bool {
        self.body.blocks.last().is_some_and(|b| stmts_diverge(b))
    }

    /// Registers a deferred block on the innermost scope.
    pub fn add_defer(&mut self, block: ir::Block) {
        for exit in self.body.exits.iter_mut().rev() {
            if let Exit::Scope { defers } = exit {
                defers.push(block);
                return;
            }
        }
    }

    /// Emits the defers of every scope above `depth` on the exit stack,
    /// innermost first.
    pub fn emit_defers_down_to(&mut self, depth: usize) {
        let mut pending = Vec::new();
        for exit in self.body.exits[depth..].iter().rev() {
            if let Exit::Scope { defers } = exit {
                for d in defers.iter().rev() {
                    pending.push(d.clone());
                }
            }
        }
        for d in pending {
            let d = self.relabel(&d);
            self.emit(Stmt::Scope(d));
        }
    }

    /// Returns true when leaving to `depth` would cross a `defer` body.
    fn crosses_defer(&self, depth: usize) -> bool {
        self.body.exits[depth..].iter().any(|e| matches!(e, Exit::Defer))
    }

    /// Reports control flow that tries to leave a `defer` body.
    pub fn check_not_in_defer(&mut self, depth: usize, span: Span, what: &str) -> bool {
        if self.crosses_defer(depth) {
            self.report(
                Diagnostic::error(codes::EXIT_IN_DEFER, format!("`{what}` cannot leave a `defer` body"))
                    .primary(span, "deferred code must run to completion")
                    .note("`defer` runs while the scope is already exiting, so it cannot jump elsewhere")
                    .help(format!("instead of `{what}`, wrap the rest of the deferred code in an `if`")),
            );
            return false;
        }
        true
    }

    /// Copies a deferred block, giving every label it defines a fresh id so
    /// the copy can be emitted at another exit of the same function.
    pub fn relabel(&mut self, block: &ir::Block) -> ir::Block {
        let mut map = std::collections::HashMap::new();
        collect_defined_labels(block, &mut map, &mut || {
            let id = LabelId(self.body.next_label);
            self.body.next_label += 1;
            id
        });
        rewrite_labels(block, &map)
    }

    // ----- functions -----------------------------------------------------

    /// Lowers the body of a function declaration.
    pub(super) fn lower_function(&mut self, decl: DeclId, subst: super::generics::Subst, origin: Span) -> ir::Function {
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Fn(f) = d.kind else { unreachable!("lower_function on a non-function") };
        let sig = self.fn_sig_inst(decl, &subst);
        let owner_ty = self.owner_type(decl).map(|t| self.subst_type(t, &subst));
        let prefix = self.pkg_prefix(d.loc.pkg);
        let unary = f.params.is_empty();
        let fn_part = super::operator_name(f.name.as_str(), unary)
            .map(str::to_string)
            .unwrap_or_else(|| mangle_ident(f.name.as_str()));
        let (c_name, display) = match owner_ty {
            Some(t) => {
                let tname = self.types.display(t);
                let sep = if f.is_static { "." } else { "#" };
                (format!("{prefix}__{}__{fn_part}", mangle_ident(&tname)), format!("{tname}{sep}{}", f.name.as_str()))
            }
            None => (format!("{prefix}__{fn_part}"), f.name.as_str().to_string()),
        };
        let (c_name, display) = if subst.is_empty() {
            (c_name, display)
        } else {
            let n = self.instance_number(decl);
            let base = c_name.replace("__", "_").replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "_");
            (
                format!("{prefix}__{}__{n}", base.trim_start_matches(&format!("{prefix}_"))),
                self.instance_display(&display, &subst),
            )
        };
        let mut func = self.new_function_shell(display.clone(), c_name, sig.ret, f.name.span);

        self.apply_fn_attributes(d.item, f, &mut func);
        if let Some(symbol) = func.foreign.clone() {
            func.c_variadic = sig.c_variadic;
            func.c_call = self.header_call(d.loc.pkg, f.name.name, &symbol, &sig);
            let mut params = Vec::new();
            for p in &sig.params {
                let id = LocalId(params.len() as u32);
                params.push(id);
                func.locals.push(ir::Local { name: Some(p.name), ty: p.ty });
            }
            func.params = params;
            return func;
        }
        let generic_context = !subst.is_empty();
        if generic_context {
            let body_span = self.decls[decl.0 as usize].item.span;
            let macro_self = super::generics::lookup(&subst, Name::new("Self"))
                .filter(|_| f.is_macro)
                .map(|t| self.types.display(t));
            self.instance_stack.push((display.clone(), origin, body_span, macro_self));
        }
        let saved = std::mem::take(&mut self.body);
        let saved_macro = std::mem::replace(&mut self.macros.in_macro, f.is_macro);
        self.body.frames.push(Frame {
            loc: d.loc,
            scopes: Vec::new(),
            ret: sig.ret,
            self_ty: owner_ty,
            self_local: None,
            fn_name: display,
            block: None,
            subst: subst.clone(),
            no_bounds: d.item.has_attr("no_bounds_check"),
            is_proc: false,
            decl: Some(decl),
            site: None,
        });
        self.body.exits.push(Exit::Function { frame: 0 });
        self.begin_block();
        self.push_scope();
        if let Some(recv) = sig.receiver {
            let ptr = self.types.pointer(recv);
            let local = self.new_local(Some(Name::new("self")), ptr);
            self.frame_mut().self_local = Some(local);
            func.params.push(local);
        }
        for p in &sig.params {
            let local = self.declare_param(p.name, p.ty, p.span);
            func.params.push(local);
        }
        let ret = sig.ret;
        let wants_value = !matches!(self.types.kind(ret), TyKind::Void | TyKind::Never);
        match &f.body {
            ast::FnBody::Block(stmts) => {
                let dest = if wants_value { Dest::Return } else { Dest::Discard };
                self.lower_stmts(stmts, dest);
                if matches!(self.types.kind(ret), TyKind::Never) && !self.current_block_diverges() {
                    let span = stmts.last().map_or(f.sig_span, |s| s.span);
                    self.report(
                        Diagnostic::error(
                            codes::MISSING_RETURN,
                            format!("`{}` is declared `-> Never` but can reach its end", f.name.as_str()),
                        )
                        .primary(span, "execution can continue past here")
                        .secondary(f.sig_span, "declared `-> Never` here")
                        .help("end the method with `panic`, an endless `loop`, or a call to another `-> Never` method"),
                    );
                    self.emit(Stmt::Unreachable);
                } else if wants_value
                    && !self.current_block_diverges()
                    && !matches!(stmts.last(), Some(ast::Stmt { kind: ast::StmtKind::Error, .. }))
                {
                    let ret_name = self.types.display(ret);
                    let span = match stmts.last() {
                        Some(s) => s.span,
                        None => f.sig_span,
                    };
                    self.report(
                        Diagnostic::error(
                            codes::MISSING_RETURN,
                            format!("`{}` must return `{ret_name}`", f.name.as_str()),
                        )
                        .primary(span, "the method can reach its end without a value")
                        .secondary(f.sig_span, format!("declared to return `{ret_name}` here"))
                        .help("make the last expression the value to return, or add an explicit `return`"),
                    );
                    self.emit(Stmt::Unreachable);
                }
            }
            ast::FnBody::Expr(e) if matches!(self.types.kind(ret), TyKind::Never) => {
                let value = self.expr(e, None);
                self.emit_value_stmt(value);
                if !self.current_block_diverges() {
                    self.report(
                        Diagnostic::error(
                            codes::MISSING_RETURN,
                            format!("`{}` is declared `-> Never` but can reach its end", f.name.as_str()),
                        )
                        .primary(e.span, "this finishes, so the method would return")
                        .secondary(f.sig_span, "declared `-> Never` here")
                        .help("end the method with `panic`, an endless `loop`, or a call to another `-> Never` method"),
                    );
                    self.emit(Stmt::Unreachable);
                }
            }
            ast::FnBody::Expr(e) => {
                if wants_value {
                    let value = self.expr_coerced(e, ret);
                    self.emit_return(Some(value), e.span);
                } else {
                    let value = self.expr(e, None);
                    if !matches!(self.types.kind(value.ty), TyKind::Void | TyKind::Never | TyKind::Unknown) {
                        let shown = self.types.display(value.ty);
                        let name = f.name.as_str();
                        self.report(
                            Diagnostic::error(
                                codes::RETURN_MISMATCH,
                                format!("`{name}` has no return type, so its value is thrown away"),
                            )
                            .primary(e.span, format!("this `{shown}` is discarded"))
                            .suggest(
                                "declare the return type",
                                vec![Edit { span: f.sig_span.shrink_to_end(), replacement: format!(" -> {shown}") }],
                                Applicability::MachineApplicable,
                            ),
                        );
                    }
                    self.emit_value_stmt(value);
                }
            }
        }
        self.pop_scope();
        let block = self.end_block();
        self.body.exits.pop();
        let body = std::mem::replace(&mut self.body, saved);
        self.macros.in_macro = saved_macro;
        if generic_context {
            self.instance_stack.pop();
        }
        func.locals = body.locals;
        func.body = Some(block);
        func
    }

    /// Applies `@[export]`, `@[c]` and `@[extern]` to a function. The
    /// attributes were validated when the declaration was collected.
    fn apply_fn_attributes(&mut self, item: &ast::Item, f: &ast::FnDecl, func: &mut ir::Function) {
        for attr in &item.attrs {
            let symbol = || {
                attr.args
                    .first()
                    .and_then(super::attrs::string_literal)
                    .map_or_else(|| mangle_ident(f.name.as_str()), str::to_string)
            };
            match attr.name.as_str() {
                "export" => func.export = Some(symbol()),
                "c" => func.abi = crate::types::Abi::C,
                "extern" => {
                    func.abi = crate::types::Abi::C;
                    func.foreign = Some(symbol());
                }
                _ => {}
            }
        }
    }

    /// Emits an expression statement, dropping pure values.
    pub fn emit_value_stmt(&mut self, value: ir::Expr) {
        if !value.is_pure() {
            self.emit(Stmt::Expr(value));
        }
    }
}

/// Returns true when a statement list never falls through.
pub(crate) fn stmts_diverge(stmts: &[Stmt]) -> bool {
    stmts.iter().any(stmt_diverges)
}

fn stmt_diverges(stmt: &Stmt) -> bool {
    match stmt {
        Stmt::Return(_) | Stmt::Goto(_) | Stmt::Unreachable => true,
        Stmt::If { then, else_, .. } => stmts_diverge(&then.stmts) && stmts_diverge(&else_.stmts),
        Stmt::Scope(b) | Stmt::WithContext(b) => stmts_diverge(&b.stmts),
        Stmt::Loop { body, break_label, .. } => !contains_goto(&body.stmts, *break_label),
        Stmt::Labeled { body, end_label } => stmts_diverge(&body.stmts) && !contains_goto(&body.stmts, *end_label),
        Stmt::Expr(e) => {
            e.ty == crate::types::NEVER || matches!(e.kind, ir::ExprKind::Builtin { op: ir::Builtin::Panic, .. })
        }
        _ => false,
    }
}

fn contains_goto(stmts: &[Stmt], label: LabelId) -> bool {
    stmts.iter().any(|s| match s {
        Stmt::Goto(l) => *l == label,
        Stmt::If { then, else_, .. } => contains_goto(&then.stmts, label) || contains_goto(&else_.stmts, label),
        Stmt::Scope(b) | Stmt::WithContext(b) => contains_goto(&b.stmts, label),
        Stmt::Loop { body, .. } | Stmt::Labeled { body, .. } => contains_goto(&body.stmts, label),
        _ => false,
    })
}

fn collect_defined_labels(
    block: &ir::Block,
    map: &mut std::collections::HashMap<LabelId, LabelId>,
    fresh: &mut impl FnMut() -> LabelId,
) {
    for s in &block.stmts {
        match s {
            Stmt::Loop { body, continue_label, break_label } => {
                map.insert(*continue_label, fresh());
                map.insert(*break_label, fresh());
                collect_defined_labels(body, map, fresh);
            }
            Stmt::Labeled { body, end_label } => {
                map.insert(*end_label, fresh());
                collect_defined_labels(body, map, fresh);
            }
            Stmt::If { then, else_, .. } => {
                collect_defined_labels(then, map, fresh);
                collect_defined_labels(else_, map, fresh);
            }
            Stmt::Scope(body) | Stmt::WithContext(body) => collect_defined_labels(body, map, fresh),
            _ => {}
        }
    }
}

fn rewrite_labels(block: &ir::Block, map: &std::collections::HashMap<LabelId, LabelId>) -> ir::Block {
    let get = |l: &LabelId| *map.get(l).unwrap_or(l);
    let stmts = block
        .stmts
        .iter()
        .map(|s| match s {
            Stmt::Loop { body, continue_label, break_label } => Stmt::Loop {
                body: rewrite_labels(body, map),
                continue_label: get(continue_label),
                break_label: get(break_label),
            },
            Stmt::Labeled { body, end_label } => {
                Stmt::Labeled { body: rewrite_labels(body, map), end_label: get(end_label) }
            }
            Stmt::If { cond, then, else_ } => {
                Stmt::If { cond: cond.clone(), then: rewrite_labels(then, map), else_: rewrite_labels(else_, map) }
            }
            Stmt::Scope(body) => Stmt::Scope(rewrite_labels(body, map)),
            Stmt::WithContext(body) => Stmt::WithContext(rewrite_labels(body, map)),
            Stmt::Goto(l) => Stmt::Goto(get(l)),
            other => other.clone(),
        })
        .collect();
    ir::Block { stmts }
}

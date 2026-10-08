//! Compile-time evaluation in the checker: `comptime` expressions and blocks,
//! `comptime if`, constant initializers, `embed`, `config`, reflection
//! (`T.fields`, `T.methods`) and the rule that compile-time-only values never
//! reach run time.
//!
//! Compile-time code is lowered like any other code into a function of its
//! own, then run by the interpreter (`crate::interp`) over the IR of
//! everything it calls. Its result crosses back as an IR constant.

use std::collections::{HashMap, HashSet, VecDeque};

use wid_diagnostics::{Applicability, Diagnostic, Edit, Span, codes};
use wid_syntax::{Name, ast};

use super::body::{Dest, Exit, Frame};
use super::items::ConstValue;
use super::{Checker, DeclId, DeclKind, DeclLoc};
use crate::interp::{self, FailKind, Failure, Interp, Limits};
use crate::ir::{self, Builtin, ExprKind, FnId, GlobalId, Stmt};
use crate::types::{TyId, TyKind};

/// How deep `comptime` code may nest `comptime` code.
const MAX_NESTING: u32 = 16;

/// The code a `comptime` evaluation runs.
#[derive(Clone, Copy)]
pub(crate) enum ComptimeCode<'b> {
    /// The statements of `comptime do … end` (or `comptime expr`).
    Stmts(&'b [ast::Stmt]),
    /// A single expression, like a constant's initializer.
    Expr(&'b ast::Expr),
}

/// Why a proc or `comptime` body can't use the locals around it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum CaptureKind {
    /// A proc, which is a plain function pointer.
    #[default]
    Proc,
    /// `comptime` code, which runs before the program does.
    Comptime,
}

/// The outcome of running compile-time code in the interpreter.
pub(crate) struct Run {
    /// The bytes of the result, or why the code stopped.
    pub result: Result<Vec<u8>, Failure>,
    /// What `puts`, `print` and `p` wrote.
    pub output: Vec<u8>,
    /// The `quote`s the code ran, for a macro.
    pub fragments: Vec<interp::Fragment>,
    /// The memory the result's pointers point into.
    pub memory: interp::Memory,
}

/// A declaration-level `comptime if` waiting for its condition.
#[derive(Clone, Copy)]
pub(crate) struct PendingIf<'a> {
    pub loc: DeclLoc,
    pub item: &'a ast::ComptimeIfItem,
    /// The struct, enum or module whose body holds it.
    pub owner: Option<DeclId>,
}

/// A declaration waiting for every declaration outside it to be known: a
/// `comptime if` or a macro call among declarations. They are resolved in
/// source order, after the unconditional declarations.
#[derive(Clone, Copy)]
pub(crate) enum Pending<'a> {
    If(PendingIf<'a>),
    Macro(super::decl_macros::PendingMacro<'a>),
}

impl<'a> Checker<'a> {
    // ----- running code ----------------------------------------------------------

    /// Lowers `code` as a function of its own, runs it inside the compiler
    /// and returns its value as a constant of the run-time program.
    ///
    /// `explicit` is false for constant initializers written without
    /// `comptime`, where calling a method is an error.
    pub fn comptime_value(
        &mut self,
        code: ComptimeCode<'_>,
        expected: Option<TyId>,
        loc: DeclLoc,
        span: Span,
        explicit: bool,
    ) -> Option<ir::Expr> {
        if self.comptime_depth >= MAX_NESTING {
            self.report(
                Diagnostic::error(codes::COMPTIME_LIMIT, "`comptime` code nests too deeply")
                    .primary(span, format!("more than {MAX_NESTING} levels of compile-time code"))
                    .help("look for a constant or `comptime` whose evaluation needs itself"),
            );
            return None;
        }
        let errors_before = self.diags.error_count();
        // In a type, like `[comptime N * 2]T` in a signature or a field, the
        // type's context binds `N`, not the code being lowered.
        let scope = self.type_scope.take();
        let (subst, self_ty) = match &scope {
            Some((subst, self_ty)) => (subst.clone(), *self_ty),
            None => self.body.frames.last().map(|f| (f.subst.clone(), f.self_ty)).unwrap_or_default(),
        };
        let visible: Vec<Name> =
            self.body.frames.iter().flat_map(|f| f.scopes.iter().flat_map(|s| s.vars.iter().map(|v| v.name))).collect();
        let no_bounds = std::mem::replace(&mut self.no_bounds_check, false);
        let saved = std::mem::take(&mut self.body);
        let saved_capturable = std::mem::replace(&mut self.capturable, visible);
        let saved_kind = std::mem::replace(&mut self.capture_kind, CaptureKind::Comptime);
        let saved_const = std::mem::replace(&mut self.const_init, if explicit { None } else { Some(span) });
        self.comptime_depth += 1;
        let unknown = self.types.unknown();
        let result_ty = expected.unwrap_or(unknown);
        self.body.frames.push(Frame {
            loc,
            scopes: Vec::new(),
            ret: result_ty,
            self_ty,
            self_local: None,
            fn_name: "comptime".into(),
            block: None,
            subst,
            no_bounds: false,
            is_proc: true,
            decl: None,
            site: None,
        });
        self.body.exits.push(Exit::Function { frame: 0 });
        self.begin_block();
        self.push_scope();
        let result = self.new_local(None, result_ty);
        match code {
            ComptimeCode::Stmts(stmts) => {
                let dest = if expected.is_some() || stmts.last().is_some_and(produces_value) {
                    Dest::Local(result, result_ty)
                } else {
                    Dest::Discard
                };
                self.lower_stmts(stmts, dest);
            }
            ComptimeCode::Expr(e) => {
                let v = self.expr(e, expected);
                self.deliver(v, Dest::Local(result, result_ty), e.span);
            }
        }
        let mut ty = self.body.locals[result.0 as usize].ty;
        if matches!(self.types.kind(ty), TyKind::Unknown) && self.diags.error_count() == errors_before {
            ty = self.types.void();
            self.body.locals[result.0 as usize].ty = ty;
        }
        if !self.current_block_diverges() {
            self.emit(Stmt::Return(Some(ir::Expr::new(ExprKind::Local(result), ty))));
        }
        self.pop_scope();
        let block = self.end_block();
        let body = std::mem::replace(&mut self.body, saved);
        self.capturable = saved_capturable;
        self.capture_kind = saved_kind;
        self.const_init = saved_const;
        for name in std::mem::take(&mut self.captured) {
            if let Some(var) = self.find_var(name) {
                var.read = true;
            }
        }
        let value = if self.diags.error_count() > errors_before || matches!(self.types.kind(ty), TyKind::Unknown) {
            None
        } else {
            let mut func = self.new_function_shell("comptime".into(), String::new(), ty, span);
            func.locals = body.locals;
            func.body = Some(block);
            let _ = self.lower_needed(&func);
            if self.diags.error_count() > errors_before { None } else { self.run_comptime(&func, ty, span) }
        };
        self.comptime_depth -= 1;
        self.no_bounds_check = no_bounds;
        self.type_scope = scope;
        value
    }

    /// Lowers the queued functions that compile-time code can reach, as
    /// the interpreter needs their IR, without the state of the code being
    /// lowered now. A function still being lowered (one whose body holds
    /// this `comptime`) stays unlowered; calling it fails at run time.
    /// Returns whether a function it reaches had errors in its body.
    pub(super) fn lower_needed(&mut self, root: &ir::Function) -> bool {
        let saved_instances = std::mem::take(&mut self.instance_stack);
        let saved_inline = std::mem::take(&mut self.inline_stack);
        let saved_bindings = std::mem::take(&mut self.owner_bindings);
        let saved_capturable = std::mem::take(&mut self.capturable);
        let saved_const = self.const_init.take();
        let saved_macro = std::mem::take(&mut self.macros.in_macro);
        let mut seen = HashSet::new();
        let mut stack = Vec::new();
        if let Some(body) = &root.body {
            visit_block(body, &mut |e| callee(e, &mut stack));
        }
        let mut failed = false;
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            if self.functions[id.0 as usize].is_none() {
                let Some(pos) = self.queue.iter().position(|p| p.id == id) else { continue };
                let Some(pending) = self.queue.remove(pos) else { continue };
                self.lower_pending(pending);
            }
            failed |= self.macros.failed.contains(&id);
            if let Some(Some(f)) = self.functions.get(id.0 as usize)
                && let Some(body) = &f.body
            {
                visit_block(body, &mut |e| callee(e, &mut stack));
            }
        }
        self.instance_stack = saved_instances;
        self.inline_stack = saved_inline;
        self.owner_bindings = saved_bindings;
        self.capturable = saved_capturable;
        self.const_init = saved_const;
        self.macros.in_macro = saved_macro;
        failed
    }

    /// Runs a lowered compile-time function in the interpreter. A macro's
    /// `Code` arguments are numbered before the run: `first_fragment` says
    /// how many there are.
    pub(super) fn interpret(&mut self, func: &ir::Function, span: Span, first_fragment: u64) -> Run {
        let field_info = self.prelude_struct("FieldInfo");
        let files = &self.file_positions;
        let positions = |s: Span| -> (String, u32, u32) {
            match files.get(&s.file) {
                Some((name, text)) => {
                    let start = (s.start as usize).min(text.len());
                    let before = text.get(..start).unwrap_or_default();
                    let line = before.matches('\n').count() as u32 + 1;
                    let col = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) as u32 + 1;
                    (name.clone(), line, col)
                }
                None => (String::new(), 0, 0),
            }
        };
        let program = interp::Program {
            types: &self.types,
            functions: &self.functions,
            globals: &self.globals,
            errors: &self.errors,
            positions: &positions,
            field_info,
        };
        let mut it = Interp::new(program, Limits::default());
        it.first_fragment = first_fragment;
        let result = it.run(func, span);
        let output = std::mem::take(&mut it.output);
        let fragments = std::mem::take(&mut it.fragments);
        Run { result, output, fragments, memory: it.into_memory() }
    }

    /// Reports what compile-time code printed (E0909). `what` names the
    /// code, like "`comptime` code".
    pub(super) fn report_output(&mut self, output: &[u8], what: &str, span: Span) {
        if output.is_empty() {
            return;
        }
        let text = String::from_utf8_lossy(output).trim_end_matches('\n').to_string();
        self.report(
            Diagnostic::warning(codes::COMPTIME_OUTPUT, format!("{what} printed output"))
                .primary(span, "this ran while compiling")
                .note(format!("it printed:\n{text}"))
                .help(
                    "remove the printing once you are done debugging; compile-time output is not part of the program",
                ),
        );
    }

    /// Runs a lowered compile-time function and converts its result.
    fn run_comptime(&mut self, func: &ir::Function, ty: TyId, span: Span) -> Option<ir::Expr> {
        let Run { result, output, memory, .. } = self.interpret(func, span, 0);
        self.report_output(&output, "`comptime` code", span);
        let bytes = match result {
            Ok(bytes) => bytes,
            Err(failure) => {
                self.report_failure(failure, span, "`comptime` code", "while running this at compile time");
                return None;
            }
        };
        let first = self.globals.len() as u32;
        match interp::to_ir(&mut self.types, &memory, ty, &bytes, first) {
            Ok(converted) => {
                self.globals.extend(converted.globals);
                Some(self.materialize(converted.expr))
            }
            Err(escape) => {
                let shown = self.types.display(ty);
                let mut diag = Diagnostic::error(
                    codes::COMPTIME_ESCAPE,
                    format!("this `{shown}` can't be used at run time: it holds {}", escape.what),
                )
                .primary(span, "computed while compiling")
                .note("values made at compile time are copied into the program; memory the compiler used is not");
                if !escape.path.is_empty() {
                    diag = diag.note(format!("the problem is at `value{}`", escape.path));
                }
                let help = match self.prelude_struct("TypeInfo") {
                    Some(info) if self.reaches(ty, info, 0) => {
                        "a `type_info` table built while compiling stays in the compiler: call `type_info` in code that runs, or keep only what you need, like `comptime type_info(T).size`".to_string()
                    }
                    _ => escape.help,
                };
                self.report(diag.help(help));
                None
            }
        }
    }

    /// Keeps scalars and strings inline, and puts larger values in a
    /// read-only global so every use shares one copy.
    fn materialize(&mut self, expr: ir::Expr) -> ir::Expr {
        let inline = matches!(
            expr.kind,
            ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::Bool(_)
                | ExprKind::Str(_)
                | ExprKind::Nil
                | ExprKind::Zero
                | ExprKind::FnRef(_)
                | ExprKind::SliceOf { .. }
        ) || matches!(&expr.kind, ExprKind::OptSome(inner) if !matches!(inner.kind, ExprKind::Aggregate(_)));
        if inline {
            return expr;
        }
        let ty = expr.ty;
        let id = GlobalId(self.globals.len() as u32);
        self.globals.push(ir::Global {
            c_name: format!("wid_const_{}", id.0),
            ty,
            init: Some(expr),
            constant: true,
            foreign: false,
            c_conv: None,
            embed: None,
            comptime_only: false,
        });
        ir::Expr::new(ExprKind::ConstGlobal(id), ty)
    }

    /// Reports why compile-time code stopped. `what` names the code, like
    /// "`comptime` code", and `running` labels `span`, the code that
    /// started the run.
    pub(super) fn report_failure(&mut self, f: Failure, span: Span, what: &str, running: &str) {
        let (code, title) = match f.kind {
            FailKind::Error => (codes::COMPTIME_FAILED, format!("{what} failed: {}", f.message)),
            FailKind::Limit => (codes::COMPTIME_LIMIT, format!("{what} stopped: {}", f.message)),
            FailKind::Foreign => (codes::COMPTIME_FOREIGN, format!("{what} can't call C: it {}", f.message)),
        };
        let at = if f.span == Span::default() { span } else { f.span };
        let mut diag = Diagnostic::error(code, title).primary(at, "failed here");
        if at != span {
            diag = diag.secondary(span, running.to_string());
        }
        for (name, site) in f.chain.iter().skip(1) {
            if *site != Span::default() && *site != at && *site != span {
                diag = diag.secondary(*site, format!("`{name}` was called here"));
            }
        }
        if f.checked {
            diag = diag.note("compile-time code runs with the checks of a `-debug` build: bounds, nil and overflow");
        }
        if let Some(help) = f.help {
            diag = diag.help(help);
        }
        self.report(diag);
    }

    /// The type of a struct the prelude declares, like `FieldInfo`.
    pub fn prelude_struct(&mut self, name: &str) -> Option<TyId> {
        let decl = self.lookup_prelude(Name::new(name))?;
        if !matches!(self.decls[decl.0 as usize].kind, DeclKind::Struct(_)) {
            return None;
        }
        let span = self.decls[decl.0 as usize].span;
        Some(self.decl_as_type(decl, span))
    }

    // ----- `comptime` in code -------------------------------------------------------

    /// Lowers `comptime …` in a method body: runs it now and uses its value.
    pub fn comptime_expr(&mut self, body: &[ast::Stmt], expected: Option<TyId>, span: Span) -> ir::Expr {
        let loc = self.loc();
        match self.comptime_value(ComptimeCode::Stmts(body), expected, loc, span, true) {
            Some(v) => v,
            None => ir::Expr::new(ExprKind::Zero, self.types.unknown()),
        }
    }

    /// Evaluates the condition of a `comptime if`.
    pub fn comptime_condition(&mut self, cond: &ast::Cond, loc: DeclLoc) -> Option<bool> {
        let e = match cond {
            ast::Cond::Expr(e) => e,
            ast::Cond::Bind { name, .. } => {
                self.report(
                    Diagnostic::error(codes::COMPTIME_ONLY, "`comptime if` can't bind a variable")
                        .primary(name.span, "this binding would hold a run-time value")
                        .help("test a compile-time condition, like `comptime if OS == :windows`"),
                );
                return None;
            }
        };
        self.comptime_bool(e, loc)
    }

    /// Evaluates a `Bool` expression at compile time.
    pub fn comptime_bool(&mut self, e: &ast::Expr, loc: DeclLoc) -> Option<bool> {
        let bool_ty = self.types.bool();
        let v = self.comptime_value(ComptimeCode::Expr(e), Some(bool_ty), loc, e.span, true)?;
        match v.kind {
            ExprKind::Bool(b) => Some(b),
            _ => None,
        }
    }

    /// Lowers a `comptime if` in a method body: only the branch whose
    /// condition holds is checked and compiled.
    pub fn lower_comptime_if(&mut self, if_expr: &ast::IfExpr, dest: Dest) {
        let loc = self.loc();
        let branches = std::iter::once((&if_expr.cond, &if_expr.then)).chain(if_expr.elifs.iter().map(|(c, b)| (c, b)));
        for (i, (cond, body)) in branches.enumerate() {
            let Some(holds) = self.comptime_condition(cond, loc) else { return };
            let holds = if i == 0 && if_expr.unless { !holds } else { holds };
            if holds {
                let block = self.lower_branch(body, dest);
                self.emit(Stmt::Scope(block));
                return;
            }
        }
        if let Some(body) = &if_expr.else_ {
            let block = self.lower_branch(body, dest);
            self.emit(Stmt::Scope(block));
        }
    }

    /// `comptime if` used as a value.
    pub fn comptime_if_value(&mut self, if_expr: &ast::IfExpr, expected: Option<TyId>) -> ir::Expr {
        let ty = expected.unwrap_or_else(|| self.types.unknown());
        let local = self.new_local(None, ty);
        self.emit(Stmt::Let { local, init: None });
        self.lower_comptime_if(if_expr, Dest::Local(local, ty));
        let ty = self.local_ty(local);
        ir::Expr::new(ExprKind::Local(local), ty)
    }

    /// Resolves the pending declarations: chooses the branches of
    /// declaration-level `comptime if`s and expands macro calls among
    /// declarations, collecting the declarations they give. Each runs once
    /// every declaration outside it is known, in source order; what they
    /// give may hold more, which run in the next round.
    pub(super) fn resolve_pending(&mut self) {
        while !self.pending_decls.is_empty() {
            let batch = std::mem::take(&mut self.pending_decls);
            for pending in batch {
                match pending {
                    Pending::If(pending) => self.resolve_comptime_if(pending),
                    Pending::Macro(pending) => self.expand_item_macro(pending),
                }
            }
        }
    }

    /// Chooses the branch of a declaration-level `comptime if` and collects
    /// its declarations.
    fn resolve_comptime_if(&mut self, pending: PendingIf<'a>) {
        let Some(holds) = self.comptime_bool(&pending.item.cond, pending.loc) else { return };
        let chosen = if holds { &pending.item.then } else { &pending.item.else_ };
        self.report_deferred(pending.loc, chosen);
        match pending.owner {
            Some(owner) => {
                for item in chosen {
                    self.collect_member_item(item, pending.loc, owner);
                }
            }
            None => self.collect_conditional(chosen, pending.loc),
        }
    }

    /// Reports the errors of loading imports in a chosen branch.
    fn report_deferred(&mut self, loc: DeclLoc, items: &[ast::Item]) {
        let file = &self.input.packages[loc.pkg.0 as usize].files[loc.file];
        let mut found = Vec::new();
        for item in items {
            // A macro generated it: its offsets are in the macro's file.
            if item.span.file.expansion_index().is_some() {
                continue;
            }
            if let Some(list) = file.deferred.get(&item.span.start) {
                found.extend(list.iter().cloned());
            }
        }
        for d in found {
            self.report(d);
        }
    }

    // ----- builtins -------------------------------------------------------------------

    /// `embed("file")`: the bytes of a file, read when the program is
    /// compiled and stored in it with C23 `#embed`.
    pub fn builtin_embed(&mut self, args: &[ast::Arg], span: Span) -> ir::Expr {
        let u8_ty = self.types.u8();
        let slice_ty = self.types.slice(u8_ty);
        let zero = ir::Expr::new(ExprKind::Zero, slice_ty);
        let [arg] = args else {
            self.report(
                Diagnostic::error(codes::ARG_COUNT, "`embed` takes one file path")
                    .primary(span, "like `embed(\"assets/font.ttf\")`"),
            );
            return zero;
        };
        let loc = self.loc();
        let path = match self.eval_const(&arg.value, loc) {
            Some(ConstValue::Str(s)) => s,
            _ => {
                self.report(
                    Diagnostic::error(codes::EMBED_FAILED, "`embed` needs a constant file path")
                        .primary(arg.value.span, "not a string known at compile time")
                        .help("write the path as a string literal, like `embed(\"assets/font.ttf\")`"),
                );
                return zero;
            }
        };
        let dir = self.input.packages[loc.pkg.0 as usize].dir.clone();
        let full = dir.join(&path);
        let bytes = match std::fs::read(&full) {
            Ok(b) => b,
            Err(e) => {
                let why = match e.kind() {
                    std::io::ErrorKind::NotFound => "the file does not exist".to_string(),
                    std::io::ErrorKind::PermissionDenied => "permission denied".to_string(),
                    std::io::ErrorKind::IsADirectory => "it is a directory".to_string(),
                    _ => e.to_string(),
                };
                let mut diag = Diagnostic::error(codes::EMBED_FAILED, format!("cannot embed `{path}`: {why}"))
                    .primary(arg.value.span, format!("looked for {}", full.display()))
                    .note("paths are relative to the package directory");
                if let Some(best) = similar_file(&full) {
                    diag = diag.suggest_replace(
                        format!("did you mean `{best}`?"),
                        arg.value.span,
                        format!("\"{}\"", std::path::Path::new(&path).with_file_name(&best).display()),
                        Applicability::MaybeIncorrect,
                    );
                }
                self.report(diag);
                return zero;
            }
        };
        let full = full.canonicalize().unwrap_or(full);
        if !self.embedded_files.contains(&full) {
            self.embedded_files.push(full.clone());
        }
        if bytes.is_empty() {
            return zero;
        }
        let n = bytes.len() as u64;
        let array = self.types.intern(TyKind::Array(u8_ty, n));
        let id = match self.embeds.get(&full) {
            Some(&id) => id,
            None => {
                let id = GlobalId(self.globals.len() as u32);
                self.globals.push(ir::Global {
                    c_name: format!("wid_embed_{}", id.0),
                    ty: array,
                    init: None,
                    constant: false,
                    foreign: false,
                    c_conv: None,
                    embed: Some(ir::Embedded { path: full.clone(), bytes: bytes.into() }),
                    comptime_only: false,
                });
                self.embeds.insert(full, id);
                id
            }
        };
        let int = self.types.int();
        ir::Expr::new(
            ExprKind::SliceOf {
                base: Box::new(ir::Expr::new(ExprKind::ConstGlobal(id), array)),
                lo: Box::new(ir::Expr::new(ExprKind::Int(0), int)),
                hi: Box::new(ir::Expr::new(ExprKind::Int(i128::from(n)), int)),
                checked: false,
                span,
            },
            slice_ty,
        )
    }

    /// `config(:name, default)`: the value of `-define:name=value`, or
    /// `default` when the flag is not given.
    pub fn builtin_config(&mut self, args: &[ast::Arg], span: Span, expected: Option<TyId>) -> ir::Expr {
        let unknown = self.types.unknown();
        let [name_arg, default_arg] = args else {
            self.report(
                Diagnostic::error(codes::ARG_COUNT, "`config` takes a name and a default value")
                    .primary(span, "like `config(:debug_draw, false)`"),
            );
            return ir::Expr::new(ExprKind::Zero, unknown);
        };
        let name = match &name_arg.value.kind {
            ast::ExprKind::Symbol(n) => n.as_str().to_string(),
            _ => {
                self.report(
                    Diagnostic::error(codes::TYPE_MISMATCH, "`config` names its setting with a symbol")
                        .primary(name_arg.value.span, "write the name like `:debug_draw`"),
                );
                return ir::Expr::new(ExprKind::Zero, unknown);
            }
        };
        let loc = self.loc();
        let Some(default) = self.fold_const(&default_arg.value, loc) else {
            self.report(
                Diagnostic::error(codes::COMPTIME_ONLY, "the default of `config` must be a constant")
                    .primary(default_arg.value.span, "not a literal or constant")
                    .help("use a literal, like `config(:level, 3)`"),
            );
            return ir::Expr::new(ExprKind::Zero, unknown);
        };
        let Some(text) = self.input.options.defines.get(&name).cloned() else {
            return self.const_with_expected(default, expected, span);
        };
        let parsed = match &default {
            ConstValue::Bool(_) => match text.as_str() {
                "true" | "1" | "" => Some(ConstValue::Bool(true)),
                "false" | "0" => Some(ConstValue::Bool(false)),
                _ => None,
            },
            ConstValue::Int(_) => text.replace('_', "").parse::<i128>().ok().map(ConstValue::Int),
            ConstValue::Float(_) => text.parse::<f64>().ok().map(ConstValue::Float),
            ConstValue::Str(_) => Some(ConstValue::Str(text.clone())),
            ConstValue::Typed(_) => None,
        };
        match parsed {
            Some(v) => self.const_with_expected(v, expected, span),
            None => {
                let (kind, example) = match &default {
                    ConstValue::Bool(b) => ("`true` or `false`", b.to_string()),
                    ConstValue::Int(i) => ("an integer", i.to_string()),
                    ConstValue::Float(f) => ("a number", format!("{f:?}")),
                    ConstValue::Str(s) => ("a string", s.clone()),
                    ConstValue::Typed(_) => ("a constant", String::new()),
                };
                self.report(
                    Diagnostic::error(codes::BAD_DEFINE, format!("`-define:{name}={text}` is not {kind}"))
                        .primary(span, format!("`{name}` is read here, with a default of {kind}"))
                        .help(format!(
                            "pass {kind}, like `-define:{name}={example}`, or drop the flag to use the default"
                        )),
                );
                self.const_with_expected(default, expected, span)
            }
        }
    }

    // ----- reflection -----------------------------------------------------------------

    /// `T.fields`, `T.methods` and `T.name` on a type.
    pub fn type_reflection(&mut self, ty: TyId, name: &ast::Ident, span: Span) -> Option<ir::Expr> {
        match name.as_str() {
            "name" => {
                let shown = self.types.display(ty);
                let string = self.types.string();
                Some(ir::Expr::new(ExprKind::Str(shown), string))
            }
            "fields" => {
                if !matches!(self.types.kind(self.types.base(ty)), TyKind::Struct(_)) {
                    let shown = self.types.display(ty);
                    self.report(
                        Diagnostic::error(codes::NO_SUCH_MEMBER, format!("`{shown}` has no `fields`"))
                            .primary(name.span, "only structs have fields"),
                    );
                    return Some(ir::Expr::new(ExprKind::Zero, self.types.unknown()));
                }
                let type_ty = self.types.type_ty();
                let t = ir::Expr::new(ExprKind::Int(i128::from(ty.0)), type_ty);
                // The whole `T.fields`, so E0906 can offer `type_info(T).fields`.
                Some(self.type_query(Builtin::TypeFields, t, span))
            }
            "methods" => Some(self.type_methods(ty, span)),
            _ => None,
        }
    }

    /// `t.name`, `t.size`, `t.align` and `t.fields` on a `Type` value.
    pub fn type_value_member(&mut self, recv: ir::Expr, name: &ast::Ident) -> ir::Expr {
        let op = match name.as_str() {
            "name" => Builtin::TypeName,
            "size" => Builtin::TypeSize,
            "align" => Builtin::TypeAlign,
            _ => Builtin::TypeFields,
        };
        self.type_query(op, recv, name.span)
    }

    /// Explains a member that a `Type` value doesn't have (E0204): it
    /// answers only the queries in [`is_type_query`]. `.methods` works only
    /// on a type written by name or `Self`, where the methods are known, so
    /// its help offers those.
    pub(super) fn no_type_value_member(&self, name: Name, span: Span, diag: Diagnostic) -> Diagnostic {
        let diag = diag.note("a `Type` value answers only `.name`, `.size`, `.align` and `.fields`");
        if name.as_str() == "methods" {
            return diag
                .note("`.methods` works on a type written by name, like `Vec2.methods`, and on `Self`")
                .help("take the methods instead of the type: a `[]MethodInfo` parameter, given `Vec2.methods`")
                .help("in a macro, call the macro in the type's body and read `Self.methods`");
        }
        match wid_diagnostics::did_you_mean(name.as_str(), TYPE_QUERIES.iter().copied()) {
            Some(best) => {
                diag.suggest_replace(format!("did you mean `{best}`?"), span, best, Applicability::MaybeIncorrect)
            }
            None => diag,
        }
    }

    fn type_query(&mut self, op: Builtin, t: ir::Expr, span: Span) -> ir::Expr {
        let ty = match op {
            Builtin::TypeName => self.types.string(),
            Builtin::TypeFields => match self.prelude_struct("FieldInfo") {
                Some(info) => self.types.slice(info),
                None => self.types.unknown(),
            },
            _ => self.types.int(),
        };
        ir::Expr::new(ExprKind::Builtin { op, args: vec![t], span }, ty)
    }

    /// `T.methods`: the methods a type declares, as compile-time data.
    fn type_methods(&mut self, ty: TyId, span: Span) -> ir::Expr {
        let unknown = self.types.unknown();
        let Some(info_ty) = self.prelude_struct("MethodInfo") else {
            return ir::Expr::new(ExprKind::Zero, unknown);
        };
        let slice_ty = self.types.slice(info_ty);
        let decl = match self.types.kind(self.types.base(ty)) {
            TyKind::Struct(id) => self.struct_decls.get(id).copied(),
            TyKind::Enum(id) => self.enum_decls.get(id).copied(),
            _ => None,
        };
        let Some(decl) = decl else {
            let shown = self.types.display(ty);
            self.report(
                Diagnostic::error(codes::NO_SUCH_MEMBER, format!("`{shown}` has no `methods`"))
                    .primary(span, "only structs and enums declare methods"),
            );
            return ir::Expr::new(ExprKind::Zero, unknown);
        };
        let mut methods: Vec<DeclId> = self
            .members
            .get(&decl)
            .map(|m| m.values().copied().filter(|d| matches!(self.decls[d.0 as usize].kind, DeclKind::Fn(_))).collect())
            .unwrap_or_default();
        methods.retain(|d| self.generic_names(*d).iter().all(|n| n.as_str() == "Self"));
        methods.sort();
        let type_ty = self.types.type_ty();
        let type_slice = self.types.slice(type_ty);
        let bool_ty = self.types.bool();
        let string = self.types.string();
        let fields = match self.types.kind(info_ty) {
            TyKind::Struct(id) => self.types.struct_info(*id).fields.clone(),
            _ => Vec::new(),
        };
        let mut elems = Vec::new();
        for m in methods {
            let sig = self.fn_sig(m);
            let DeclKind::Fn(f) = self.decls[m.0 as usize].kind else { continue };
            let params: Vec<ir::Expr> =
                sig.params.iter().map(|p| ir::Expr::new(ExprKind::Int(i128::from(p.ty.0)), type_ty)).collect();
            let params = self.comptime_array(type_ty, params, type_slice);
            let mut values = Vec::new();
            for field in &fields {
                values.push(match field.name.as_str() {
                    "name" => ir::Expr::new(ExprKind::Str(f.name.as_str().to_string()), string),
                    "params" => params.clone(),
                    "ret" => ir::Expr::new(ExprKind::Int(i128::from(sig.ret.0)), type_ty),
                    "static" => ir::Expr::new(ExprKind::Bool(f.is_static), bool_ty),
                    _ => ir::Expr::new(ExprKind::Zero, field.ty),
                });
            }
            elems.push(ir::Expr::new(ExprKind::Aggregate(values), info_ty));
        }
        self.comptime_array(info_ty, elems, slice_ty)
    }

    /// A slice over compile-time-only static data.
    pub(super) fn comptime_array(&mut self, elem: TyId, elems: Vec<ir::Expr>, slice_ty: TyId) -> ir::Expr {
        if elems.is_empty() {
            return ir::Expr::new(ExprKind::Zero, slice_ty);
        }
        let n = elems.len() as u64;
        let array = self.types.intern(TyKind::Array(elem, n));
        let id = GlobalId(self.globals.len() as u32);
        self.globals.push(ir::Global {
            c_name: format!("wid_const_{}", id.0),
            ty: array,
            init: Some(ir::Expr::new(ExprKind::Aggregate(elems), array)),
            constant: true,
            foreign: false,
            c_conv: None,
            embed: None,
            comptime_only: true,
        });
        let int = self.types.int();
        ir::Expr::new(
            ExprKind::SliceOf {
                base: Box::new(ir::Expr::new(ExprKind::ConstGlobal(id), array)),
                lo: Box::new(ir::Expr::new(ExprKind::Int(0), int)),
                hi: Box::new(ir::Expr::new(ExprKind::Int(i128::from(n)), int)),
                checked: false,
                span: Span::default(),
            },
            slice_ty,
        )
    }

    /// A type written where a `Type` value is expected, like `F32` in
    /// `t == F32`.
    pub fn type_as_value(&mut self, e: &ast::Expr) -> Option<ir::Expr> {
        let ty = match &e.kind {
            ast::ExprKind::Const(name) => {
                let frame = self.body.frames.last()?;
                if let Some(t) = super::generics::lookup(&frame.subst, *name) {
                    t
                } else if let Some(t) = self.primitive(name.as_str()) {
                    t
                } else {
                    let loc = self.loc();
                    let decl = self.lookup_pkg(loc.pkg, *name).or_else(|| self.lookup_prelude(*name))?;
                    let d = &self.decls[decl.0 as usize];
                    match d.kind {
                        DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Union(_) => {
                            let span = e.span;
                            self.decl_as_type(decl, span)
                        }
                        // A type alias, like `Vec2 = [2]F32` or `X = (Int)`.
                        DeclKind::Const(c) if self.is_type_alias_value(&c.value, d.loc, 0) => {
                            let span = e.span;
                            self.decl_as_type(decl, span)
                        }
                        _ => return None,
                    }
                }
            }
            ast::ExprKind::Type(t) => {
                let ctx = self.body_ctx();
                self.resolve_type(t, &ctx)
            }
            // `Pool(Ball, 64)`, `C.int`, `rl.Color`.
            ast::ExprKind::Call(_) | ast::ExprKind::Member { .. } => self.named_type(e)?,
            _ => return None,
        };
        let type_ty = self.types.type_ty();
        Some(ir::Expr::new(ExprKind::Int(i128::from(ty.0)), type_ty))
    }

    // ----- compile-time-only code ------------------------------------------------------

    /// Marks the functions that use compile-time-only values (`Type`,
    /// `Code`, `Symbol`, `T.fields`), or call functions that do, and reports
    /// any that the program would run: at the run-time call that reaches
    /// them.
    pub(super) fn check_comptime_only(&mut self, roots: &[FnId]) {
        let n = self.functions.len();
        let mut direct: Vec<Option<OnlyUse>> = vec![None; n];
        let mut callers: Vec<Vec<usize>> = vec![Vec::new(); n];
        for (i, f) in self.functions.iter().enumerate() {
            let Some(f) = f else { continue };
            direct[i] = self.comptime_only_use(f);
            if let Some(body) = &f.body {
                visit_block(body, &mut |e| {
                    if let ExprKind::Call { func, .. } = &e.kind
                        && (func.0 as usize) < n
                    {
                        callers[func.0 as usize].push(i);
                    }
                });
            }
        }
        let mut only: Vec<bool> = direct.iter().map(Option::is_some).collect();
        let mut queue: VecDeque<usize> = (0..n).filter(|i| only[*i]).collect();
        while let Some(i) = queue.pop_front() {
            for &c in &callers[i] {
                if !only[c] {
                    only[c] = true;
                    queue.push_back(c);
                }
            }
        }
        for (i, is_only) in only.iter().enumerate() {
            if *is_only && let Some(Some(f)) = self.functions.get_mut(i) {
                f.comptime_only = true;
            }
        }
        let display = |this: &Self, i: usize| this.functions[i].as_ref().map(|f| f.display.clone()).unwrap_or_default();
        let mut seen = HashSet::new();
        let mut stack: Vec<usize> = roots.iter().map(|r| r.0 as usize).collect();
        let mut reports = Vec::new();
        while let Some(i) = stack.pop() {
            if i >= n || !seen.insert(i) {
                continue;
            }
            if let Some(found) = &direct[i] {
                reports.push((i, found.span, found.why.clone(), None, found.kind));
                continue;
            }
            let Some(Some(f)) = self.functions.get(i) else { continue };
            let Some(body) = &f.body else { continue };
            let mut calls = Vec::new();
            calls_with_lines(body, f.span, &mut calls);
            let mut reported = HashSet::new();
            for (callee, line) in calls {
                let c = callee.0 as usize;
                if c >= n {
                    continue;
                }
                if only[c] {
                    if reported.insert(c) {
                        let why = self.comptime_only_reason(c, &direct, &only);
                        let kind = why.kind;
                        reports.push((i, line, display(self, c), Some((c, why)), kind));
                    }
                } else {
                    stack.push(c);
                }
            }
        }
        for (i, span, what, why, kind) in reports {
            let caller = display(self, i);
            let diag = match why {
                None => {
                    let diag = Diagnostic::error(
                        codes::COMPTIME_AT_RUNTIME,
                        format!("`{caller}` uses a compile-time-only value"),
                    )
                    .primary(span, what)
                    .note(format!("`{caller}` runs when the program runs, but {}", kind.lifetime()));
                    match self.fields_at_run_time(i) {
                        Some((at, replacement)) if kind == Only::Type => diag.suggest_replace(
                            format!("read the fields while the program runs with `{replacement}`"),
                            at,
                            replacement,
                            Applicability::MaybeIncorrect,
                        ),
                        _ => diag.help(kind.help()),
                    }
                }
                Some((c, reason)) => {
                    let diag =
                        Diagnostic::error(codes::COMPTIME_AT_RUNTIME, format!("`{what}` works only at compile time"))
                            .primary(span, format!("`{caller}` calls it when the program runs"))
                            .secondary(reason.span, reason.why)
                            .help(format!("call it while compiling instead, like `x = comptime {what}(…)`"));
                    match self.type_argument(i, c) {
                        Some(t) if kind == Only::Type => {
                            let shown = self.types.display(t);
                            diag.help(format!(
                                "to describe `{shown}` while the program runs, use `type_info({shown})`: a `^TypeInfo` with its `name`, `size`, `align`, `fields` and more"
                            ))
                        }
                        _ => diag.help(kind.help()),
                    }
                }
            };
            self.report(diag);
        }
    }

    /// Where function `i` reads `T.fields` of a type it names, with the
    /// `type_info(T).fields` that reads them at run time.
    fn fields_at_run_time(&self, i: usize) -> Option<(Span, String)> {
        let body = self.functions.get(i)?.as_ref()?.body.as_ref()?;
        let mut found = None;
        visit_block(body, &mut |e| {
            if found.is_none()
                && let ExprKind::Builtin { op: Builtin::TypeFields, args, span } = &e.kind
                && let [ir::Expr { kind: ExprKind::Int(_), .. }] = args.as_slice()
                && let Some((recv, "fields")) = self.source_text(*span).rsplit_once('.')
                && !recv.trim().is_empty()
            {
                found = Some((*span, format!("type_info({}).fields", recv.trim())));
            }
        });
        found
    }

    /// A type that function `i` passes as a `Type` argument when it calls
    /// function `c`, like the `Point` of `field_count(Point)`.
    fn type_argument(&self, i: usize, c: usize) -> Option<TyId> {
        let body = self.functions.get(i)?.as_ref()?.body.as_ref()?;
        let mut found = None;
        visit_block(body, &mut |e| {
            if found.is_none()
                && let ExprKind::Call { func, args } = &e.kind
                && func.0 as usize == c
            {
                found = args.iter().find_map(|a| match a.kind {
                    ExprKind::Int(v) if matches!(self.types.kind(a.ty), TyKind::Type) => {
                        u32::try_from(v).ok().map(TyId).filter(|t| (t.0 as usize) < self.types.len())
                    }
                    _ => None,
                });
            }
        });
        found
    }

    /// Why a compile-time-only function is one: the use inside it, or the
    /// first compile-time-only function it calls.
    fn comptime_only_reason(&self, mut i: usize, direct: &[Option<OnlyUse>], only: &[bool]) -> OnlyUse {
        for _ in 0..64 {
            if let Some(r) = &direct[i] {
                return r.clone();
            }
            let Some(Some(f)) = self.functions.get(i) else { break };
            let Some(body) = &f.body else { break };
            let mut calls = Vec::new();
            calls_with_lines(body, f.span, &mut calls);
            match calls.into_iter().find(|(c, _)| only.get(c.0 as usize).copied().unwrap_or(false)) {
                Some((c, line)) => {
                    let name = self.functions[c.0 as usize].as_ref().map(|f| f.display.clone()).unwrap_or_default();
                    if let Some(found) = &direct[c.0 as usize] {
                        let why = format!("it calls `{name}`, which uses compile-time-only values");
                        return OnlyUse { span: line, why, kind: found.kind };
                    }
                    i = c.0 as usize;
                }
                None => break,
            }
        }
        OnlyUse { span: Span::default(), why: "it uses compile-time-only values".into(), kind: Only::Type }
    }

    /// Where a function uses a compile-time-only value, if it does.
    fn comptime_only_use(&self, f: &ir::Function) -> Option<OnlyUse> {
        let mut found: Option<OnlyUse> = None;
        if let Some(body) = &f.body {
            visit_block(body, &mut |e| {
                let ExprKind::Builtin { op, span, .. } = &e.kind else { return };
                if found.is_some() {
                    return;
                }
                let (why, kind) = match op {
                    Builtin::TypeFields | Builtin::TypeName | Builtin::TypeSize | Builtin::TypeAlign => {
                        ("reads a `Type`, which exists only while compiling", Only::Type)
                    }
                    Builtin::Quote { .. } => ("builds code with `quote`, which only a macro can do", Only::Code),
                    Builtin::ToSymbol => ("makes a `Symbol`, which exists only while compiling", Only::Symbol),
                    _ => return,
                };
                found = Some(OnlyUse { span: *span, why: why.into(), kind });
            });
        }
        if found.is_some() {
            return found;
        }
        let kind = f
            .locals
            .iter()
            .find_map(|l| self.comptime_only_kind(l.ty, 0))
            .or_else(|| self.comptime_only_kind(f.ret, 0))?;
        let span = f.body.as_ref().and_then(|b| self.line_using(b, f)).unwrap_or(f.span);
        Some(OnlyUse { span, why: kind.uses().into(), kind })
    }

    /// The first line of a body that uses a value of a compile-time-only
    /// type.
    fn line_using(&self, body: &ir::Block, f: &ir::Function) -> Option<Span> {
        let mut line = None;
        let mut found = None;
        visit_lines(body, &mut |stmt| {
            if found.is_some() {
                return;
            }
            if let Stmt::Line(span) = stmt {
                line = Some(*span);
                return;
            }
            let mut uses = matches!(stmt, Stmt::Let { local, .. } | Stmt::LetUninit(local)
                if f.locals.get(local.0 as usize).is_some_and(|l| self.comptime_only_kind(l.ty, 0).is_some()));
            visit_stmt_exprs(stmt, &mut |e| uses |= self.comptime_only_kind(e.ty, 0).is_some());
            if uses {
                found = line;
            }
        });
        found
    }

    /// Whether a value of type `ty` is, or holds, a `target`.
    fn reaches(&self, ty: TyId, target: TyId, depth: u32) -> bool {
        if ty == target {
            return true;
        }
        if depth > 8 {
            return false;
        }
        match self.types.kind(self.types.base(ty)) {
            TyKind::Array(t, _)
            | TyKind::Slice(t)
            | TyKind::Dynamic(t)
            | TyKind::Optional(t)
            | TyKind::Pointer(t)
            | TyKind::MultiPointer(t) => self.reaches(*t, target, depth + 1),
            TyKind::Tuple(ts) => ts.iter().any(|t| self.reaches(*t, target, depth + 1)),
            TyKind::Struct(id) => {
                self.types.struct_info(*id).fields.iter().any(|f| self.reaches(f.ty, target, depth + 1))
            }
            _ => false,
        }
    }

    /// Which compile-time-only value a type is or holds, if any: a `Type`,
    /// `Code` or `Symbol`.
    pub(super) fn comptime_only_kind(&self, ty: TyId, depth: u32) -> Option<Only> {
        if depth > 8 {
            return None;
        }
        match self.types.kind(self.types.base(ty)) {
            TyKind::Type => Some(Only::Type),
            TyKind::Code => Some(Only::Code),
            TyKind::Symbol => Some(Only::Symbol),
            TyKind::Array(t, _)
            | TyKind::Slice(t)
            | TyKind::Dynamic(t)
            | TyKind::Optional(t)
            | TyKind::Pointer(t)
            | TyKind::MultiPointer(t) => self.comptime_only_kind(*t, depth + 1),
            TyKind::Tuple(ts) => ts.iter().find_map(|t| self.comptime_only_kind(*t, depth + 1)),
            TyKind::Struct(id) => {
                self.types.struct_info(*id).fields.iter().find_map(|f| self.comptime_only_kind(f.ty, depth + 1))
            }
            _ => None,
        }
    }

    /// Fills `file_positions` for `caller_location` at compile time.
    pub(super) fn init_file_positions(&mut self) {
        let mut out = HashMap::new();
        for p in &self.input.packages {
            for f in &p.files {
                out.insert(f.ast.file, (f.display.clone(), f.text.clone()));
            }
        }
        self.file_positions = out;
    }
}

/// The kinds of values that exist only while compiling.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Only {
    Type,
    Code,
    Symbol,
}

impl Only {
    /// Says that code uses such values, for labels.
    fn uses(self) -> &'static str {
        match self {
            Only::Type => "uses `Type` values, which exist only while compiling",
            Only::Code => "uses `Code` values, which exist only while macros run",
            Only::Symbol => "uses `Symbol` values, which exist only while compiling",
        }
    }

    /// Says how long such values live, for messages.
    fn lifetime(self) -> &'static str {
        match self {
            Only::Type => "`Type` values and reflection exist only while compiling",
            Only::Code => "`Code` values exist only while macros run",
            Only::Symbol => "`Symbol` values exist only while compiling",
        }
    }

    /// How to do without such a value at run time.
    fn help(self) -> &'static str {
        match self {
            Only::Type => {
                "for type information while the program runs, use `type_info(T)`, or `type_info(x)` for the type of a value"
            }
            Only::Code => {
                "build the code in a `macro def` and call the macro where the code should go; only macros take and return `Code`"
            }
            Only::Symbol => {
                "while the program runs, name things with a `String` or an enum; `sym.to_s` gives a symbol's name as a `String` while compiling"
            }
        }
    }
}

/// Where a function uses a compile-time-only value, and why it is one.
#[derive(Clone, Debug)]
struct OnlyUse {
    span: Span,
    why: String,
    kind: Only,
}

/// Visits every statement of a block, nested ones included, in order.
fn visit_lines(block: &ir::Block, f: &mut impl FnMut(&Stmt)) {
    for stmt in &block.stmts {
        f(stmt);
        match stmt {
            Stmt::If { then, else_, .. } => {
                visit_lines(then, f);
                visit_lines(else_, f);
            }
            Stmt::Loop { body, .. } | Stmt::Labeled { body, .. } | Stmt::Scope(body) | Stmt::WithContext(body) => {
                visit_lines(body, f);
            }
            _ => {}
        }
    }
}

/// Visits the expressions of one statement, not of its nested blocks.
fn visit_stmt_exprs(stmt: &Stmt, f: &mut impl FnMut(&ir::Expr)) {
    match stmt {
        Stmt::Let { init: Some(e), .. } | Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::If { cond: e, .. } => {
            visit_expr(e, f);
        }
        Stmt::Assign { target, value } => {
            visit_expr(target, f);
            visit_expr(value, f);
        }
        _ => {}
    }
}

/// The calls in a block, each with the span of the statement it is in.
fn calls_with_lines(block: &ir::Block, start: Span, out: &mut Vec<(FnId, Span)>) {
    let mut line = start;
    for stmt in &block.stmts {
        if let Stmt::Line(span) = stmt {
            line = *span;
        }
        match stmt {
            Stmt::If { then, else_, .. } => {
                visit_stmt_shallow(stmt, line, out);
                calls_with_lines(then, line, out);
                calls_with_lines(else_, line, out);
            }
            Stmt::Loop { body, .. } | Stmt::Labeled { body, .. } | Stmt::Scope(body) | Stmt::WithContext(body) => {
                calls_with_lines(body, line, out);
            }
            _ => visit_stmt_shallow(stmt, line, out),
        }
    }
}

/// The calls in a statement's own expressions (not its nested blocks).
fn visit_stmt_shallow(stmt: &Stmt, line: Span, out: &mut Vec<(FnId, Span)>) {
    let mut note = |e: &ir::Expr| {
        if let ExprKind::Call { func, .. } = &e.kind {
            out.push((*func, line));
        }
    };
    match stmt {
        Stmt::Let { init: Some(e), .. } | Stmt::Expr(e) | Stmt::Return(Some(e)) | Stmt::If { cond: e, .. } => {
            visit_expr(e, &mut note);
        }
        Stmt::Assign { target, value } => {
            visit_expr(target, &mut note);
            visit_expr(value, &mut note);
        }
        _ => {}
    }
}

/// Collects the functions an expression calls or names.
fn callee(e: &ir::Expr, out: &mut Vec<FnId>) {
    if let ExprKind::Call { func, .. } | ExprKind::FnRef(func) = &e.kind {
        out.push(*func);
    }
}

/// Whether `name` is a query `Type` values answer.
pub(crate) fn is_type_query(name: &str) -> bool {
    TYPE_QUERIES.contains(&name)
}

/// What a `Type` value answers.
const TYPE_QUERIES: [&str; 4] = ["name", "size", "align", "fields"];

/// Whether a statement can give a block its value.
pub(super) fn produces_value(stmt: &ast::Stmt) -> bool {
    match &stmt.kind {
        ast::StmtKind::Expr(e) => {
            !matches!(e.kind, ast::ExprKind::While { .. } | ast::ExprKind::For(_) | ast::ExprKind::Loop(_))
                && !matches!(&e.kind, ast::ExprKind::If(i) if i.else_.is_none())
        }
        _ => false,
    }
}

/// A file next to `path` with a similar name.
fn similar_file(path: &std::path::Path) -> Option<String> {
    let dir = path.parent()?;
    let name = path.file_name()?.to_string_lossy().into_owned();
    let siblings: Vec<String> =
        std::fs::read_dir(dir).ok()?.flatten().map(|e| e.file_name().to_string_lossy().into_owned()).collect();
    wid_diagnostics::did_you_mean(&name, siblings.iter().map(String::as_str)).map(str::to_string)
}

fn visit_block(block: &ir::Block, f: &mut impl FnMut(&ir::Expr)) {
    for s in &block.stmts {
        visit_stmt(s, f);
    }
}

fn visit_stmt(stmt: &Stmt, f: &mut impl FnMut(&ir::Expr)) {
    match stmt {
        Stmt::Let { init: Some(e), .. } | Stmt::Expr(e) | Stmt::Return(Some(e)) => visit_expr(e, f),
        Stmt::Assign { target, value } => {
            visit_expr(target, f);
            visit_expr(value, f);
        }
        Stmt::If { cond, then, else_ } => {
            visit_expr(cond, f);
            visit_block(then, f);
            visit_block(else_, f);
        }
        Stmt::Loop { body, .. } | Stmt::Labeled { body, .. } | Stmt::Scope(body) | Stmt::WithContext(body) => {
            visit_block(body, f);
        }
        _ => {}
    }
}

fn visit_expr(e: &ir::Expr, f: &mut impl FnMut(&ir::Expr)) {
    f(e);
    match &e.kind {
        ExprKind::Field { base, .. } => visit_expr(base, f),
        ExprKind::Deref(x)
        | ExprKind::AddrOf(x)
        | ExprKind::Unary { expr: x, .. }
        | ExprKind::Cast { expr: x, .. }
        | ExprKind::OptSome(x)
        | ExprKind::OptIsSome(x)
        | ExprKind::OptGet(x)
        | ExprKind::UnionTag(x)
        | ExprKind::UnionWrap { value: x, .. }
        | ExprKind::UnionGet { value: x, .. } => visit_expr(x, f),
        ExprKind::Index { base, index, .. } => {
            visit_expr(base, f);
            visit_expr(index, f);
        }
        ExprKind::SliceOf { base, lo, hi, .. } => {
            visit_expr(base, f);
            visit_expr(lo, f);
            visit_expr(hi, f);
        }
        ExprKind::Call { args, .. } | ExprKind::Builtin { args, .. } | ExprKind::Aggregate(args) => {
            for a in args {
                visit_expr(a, f);
            }
        }
        ExprKind::CallIndirect { callee, args, .. } => {
            visit_expr(callee, f);
            for a in args {
                visit_expr(a, f);
            }
        }
        ExprKind::Binary { lhs, rhs, .. } => {
            visit_expr(lhs, f);
            visit_expr(rhs, f);
        }
        ExprKind::Select { cond, then, else_ } => {
            visit_expr(cond, f);
            visit_expr(then, f);
            visit_expr(else_, f);
        }
        _ => {}
    }
}

/// Kept for the edit suggestions of constant initializers.
pub(crate) fn insert_comptime(span: Span) -> Edit {
    Edit { span: span.shrink_to_start(), replacement: "comptime ".into() }
}

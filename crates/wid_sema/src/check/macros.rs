//! Macros: checking `macro def`s, running a macro for a call, and building
//! and lowering the code it generates.
//!
//! # The expansion core
//!
//! A call of a macro (`twice(x)`, `lib.twice x`, `swap a, b`) is found where
//! calls resolve (`call`, `ident` and `package_member`, with `call_fn` as a
//! fallback) and handed to [`Checker::call_macro`], which:
//!
//! 1. converts the arguments by parameter type ([`Checker::macro_args`]): a
//!    `Code` argument becomes an *argument fragment*, the call-site syntax
//!    itself; a `Symbol` argument (a symbol literal) its name's
//!    [`Name::index`]; a `Type` argument the `TyId`; any other argument runs
//!    like `comptime`. A `*names: T` parameter collects the remaining
//!    positional arguments, each converted by `T`'s rule, into a static
//!    `[]T`;
//! 2. runs the macro ([`Checker::run_macro`]): a wrapper function calls it,
//!    `lower_needed` lowers what the wrapper reaches, and the interpreter
//!    runs it with the comptime limits. Each `quote` the macro runs
//!    (`Builtin::Quote`) records an [`interp::Fragment`], its [`Template`]
//!    and splice values, and evaluates to a `Code` value, a fragment number;
//!    argument fragments are numbered first, from one, and zero is no code;
//! 3. builds the code the returned `Code` value stands for ([`Expander`]):
//!    a fragment's template is cloned, its spans moved into a virtual file,
//!    and its splices replaced by their values (a `Code` value is built the
//!    same way, recursively, and an argument fragment is copied as written);
//! 4. lowers the code where the call is ([`Checker::lower_generated`]):
//!    every statement in place, the last one giving the call's value.
//!
//! Generated statements are allocated in `Checker::generated`, an arena
//! that lives as long as the input syntax, so declarations built from them
//! can be collected like written ones. Declaration-level calls (in type
//! bodies and at package level) are the next step: their entry point turns
//! the built statements into items (a `StmtKind::Item` gives its item, a
//! call statement an `ItemKind::MacroCall`; [`Splicer`] already does this
//! for `ItemKind::Splice`) and queues them with the pending `comptime if`s.
//!
//! # Virtual files
//!
//! An expansion copies each template file it uses into a *virtual file*
//! ([`FileId::expansion`]) with the same text and offsets. A
//! [`VirtualFile`] records the template file, the [`Expansion`] (call site,
//! macro name, nesting depth) and the package and file where the
//! template's own names resolve. The list goes to the driver in
//! `ir::Program::expansions`, which registers it in the source map, so a
//! diagnostic about generated code points into the `quote` and lists the
//! calls that led there, and `#line` directives and panic locations name the
//! macro's file.
//!
//! # Names and hygiene
//!
//! A span's file says whose code it is. While the checker lowers code whose
//! span is in a virtual file, the frame's `site` is that file: names resolve
//! where the macro is defined ([`Checker::loc`]), and only variables that
//! code of the same expansion declared are visible ([`Checker::find_var`];
//! each variable records the expansion that declared it). Code spliced in
//! from the call site keeps its own spans, so it resolves and binds at the
//! call site. A name spliced from a `Symbol` takes the span of the symbol
//! argument that named it (or of the call), so it binds the caller's name.
//! This works as if each expansion renamed the locals its `quote` binds,
//! without changing their names in messages or in the generated C. The
//! parameters of a generated method are part of its interface, so they are
//! *open*: code spliced in from the macro's call site sees them too.

use std::collections::{HashMap, HashSet};

use wid_diagnostics::{Applicability, Diagnostic, FileId, Span, codes};
use wid_syntax::ast::{self, ExprKind as E, ItemKind, StmtKind, TypeKind, splice_index};
use wid_syntax::visit::{VisitMut, walk_expr, walk_item, walk_type};
use wid_syntax::{Name, ast::Ident};

use super::body::Dest;
use super::comptime::{ComptimeCode, produces_value};
use super::expr::ArgSource;
use super::members::{Receiver, expr_as_type, is_type_like};
use super::{Checker, DeclId, DeclKind, DeclLoc};
use crate::input::PackageId;
use crate::interp::{Fragment, SpliceValue};
use crate::ir::{self, ExprKind, FnId, Stmt};
use crate::types::{TyId, TyKind};

/// How deep expansions may nest: a macro whose code calls a macro.
pub(crate) const MAX_DEPTH: u32 = 64;
/// How many expansions one build may run.
pub(crate) const MAX_EXPANSIONS: usize = 65_536;

/// What may be spliced, for messages.
const SPLICEABLE: &str =
    "a `quote` splices `Code`, `[]Code`, `Symbol`, `[]Symbol`, `Type`, numbers, `Bool`s and strings";

/// A `quote` in a macro body, kept to build the code it makes.
pub(crate) struct Template {
    /// The syntax.
    pub quote: ast::QuoteExpr,
    /// The file its spans are in.
    pub file: FileId,
}

/// One run of a macro for one call.
pub(crate) struct Expansion {
    /// The call.
    pub call_site: Span,
    /// The macro as the call names it.
    pub name: String,
    /// How many expansions the call is nested in, plus one.
    pub depth: u32,
}

/// A template file as one expansion copied it: the file of the spans of
/// generated code.
pub(crate) struct VirtualFile {
    /// The file the template's spans are in (may be virtual itself).
    pub template: FileId,
    /// The expansion, an index into `MacroState::expansions`.
    pub expansion: u32,
    /// Where the template's own names resolve.
    pub loc: DeclLoc,
}

/// The checker's macro state.
#[derive(Default)]
pub(crate) struct MacroState {
    /// Every `quote` lowered in a macro body, by `Builtin::Quote` number.
    pub templates: Vec<Template>,
    /// The template number of each `quote`, by its span.
    pub template_ids: HashMap<Span, u32>,
    /// Every expansion so far.
    pub expansions: Vec<Expansion>,
    /// Every virtual file: `FileId::expansion(i)` is `files[i]`.
    pub files: Vec<VirtualFile>,
    /// The virtual file of each expansion and template file.
    pub file_ids: HashMap<(u32, FileId), u32>,
    /// Where the names of each real file resolve.
    pub file_locs: HashMap<FileId, DeclLoc>,
    /// Set while a macro body is lowered: `quote` works there.
    pub in_macro: bool,
    /// Macros whose declarations were checked, and whether they are valid.
    pub checked: HashMap<DeclId, bool>,
    /// Functions whose bodies had errors; a macro that reaches one never
    /// runs.
    pub failed: HashSet<FnId>,
    /// The `Line` of the statement being lowered, restored after the
    /// statements of an expansion.
    pub line: Span,
    /// Macros reported for nesting too deep (E0903).
    pub too_deep: HashSet<DeclId>,
    /// Whether the build ran out of expansions (E0903), already reported.
    pub too_many: bool,
}

/// A call of a macro.
pub(crate) struct MacroCall<'e> {
    /// The macro.
    pub decl: DeclId,
    /// The macro as the call names it, like `twice` or `lib.twice`.
    pub shown: String,
    /// The arguments as written.
    pub args: &'e [ast::Arg],
    /// A block passed to the call, which macros don't take.
    pub block: Option<&'e ast::BlockArg>,
    /// The macro's name in the call.
    pub name_span: Span,
    /// The whole call.
    pub span: Span,
}

/// What a call passes to the code builder besides the macro's result.
#[derive(Default)]
struct CallCode {
    /// The `Code` arguments as written: `Code` value `i + 1` is `code[i]`.
    code: Vec<Vec<ast::Stmt>>,
    /// The symbol arguments with their spans, so a name spliced from one
    /// points at it.
    symbols: Vec<(Name, Span)>,
}

impl<'a> Checker<'a> {
    // ----- virtual files ------------------------------------------------------------

    /// The virtual file an id names, if it names one.
    pub fn virtual_file(&self, file: FileId) -> Option<&VirtualFile> {
        self.macros.files.get(file.expansion_index()? as usize)
    }

    /// Records where each file's names resolve, for virtual files made from
    /// it.
    pub(super) fn init_macro_files(&mut self) {
        for (p, pkg) in self.input.packages.iter().enumerate() {
            for (f, file) in pkg.files.iter().enumerate() {
                self.macros.file_locs.insert(file.ast.file, DeclLoc { pkg: PackageId(p as u32), file: f });
            }
        }
    }

    /// The expansions for the driver's source map: one entry per virtual
    /// file (see `ir::Program::expansions`).
    pub(super) fn expansion_files(&self) -> Vec<wid_diagnostics::Expansion> {
        self.macros
            .files
            .iter()
            .map(|v| {
                let e = &self.macros.expansions[v.expansion as usize];
                wid_diagnostics::Expansion { template: v.template, call_site: e.call_site, name: e.name.clone() }
            })
            .collect()
    }

    /// Gives virtual files from number `from` on the text and positions of
    /// the files they copy.
    fn register_virtual_files(&mut self, from: usize) {
        for i in from..self.macros.files.len() {
            let mut real = self.macros.files[i].template;
            while let Some(v) = self.virtual_file(real) {
                real = v.template;
            }
            let id = FileId::expansion(i as u32);
            if let Some(text) = self.source_texts.get(&real).cloned() {
                self.source_texts.insert(id, text);
            }
            if let Some(position) = self.file_positions.get(&real).cloned() {
                self.file_positions.insert(id, position);
            }
        }
    }

    // ----- macro declarations ---------------------------------------------------------

    /// Whether a declaration is a `macro def`.
    pub fn is_macro(&self, decl: DeclId) -> bool {
        matches!(self.decls[decl.0 as usize].kind, DeclKind::Fn(f) if f.is_macro)
    }

    /// Checks what a `macro def` declares, once: it returns `Code` and takes
    /// neither `$T` parameters nor a block. Returns whether it can run.
    pub(super) fn check_macro(&mut self, decl: DeclId) -> bool {
        if let Some(&ok) = self.macros.checked.get(&decl) {
            return ok;
        }
        self.macros.checked.insert(decl, false);
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Fn(f) = d.kind else { return false };
        let mut ok = true;
        if let Some(p) = f.params.iter().find(|p| matches!(p.ty.kind, TypeKind::Param(_))) {
            self.report(
                Diagnostic::error(codes::GENERIC_ARGS, "a macro can't have type parameters")
                    .primary(p.ty.span, "a `$` type parameter")
                    .note("a macro runs once per call, while compiling; it receives types as values")
                    .help(
                        "take the type as a `Type` parameter, like `t: Type`, and splice it where a type goes with `#{t}`; take the value as `Code`",
                    ),
            );
            ok = false;
        }
        if let Some(b) = &f.block {
            self.report(
                Diagnostic::error(codes::BLOCK_MISMATCH, "a macro can't take a block")
                    .primary(b.span, "a `&block` parameter")
                    .help(format!(
                        "take the code as a parameter instead, like `{}: Code`, and pass it as an argument",
                        b.name.as_str()
                    )),
            );
            ok = false;
        }
        if ok {
            let sig = self.fn_sig(decl);
            let code = self.types.code();
            if sig.ret != code && !matches!(self.types.kind(sig.ret), TyKind::Unknown) {
                let diag = Diagnostic::error(codes::RETURN_MISMATCH, "a macro returns `Code`")
                    .note("a macro returns the code a `quote do … end` builds, which replaces the call");
                let diag = match &f.ret {
                    None => diag.primary(f.sig_span, "this macro declares no return type").suggest(
                        "declare that it returns `Code`",
                        vec![wid_diagnostics::Edit {
                            span: f.sig_span.shrink_to_end(),
                            replacement: " -> Code".into(),
                        }],
                        Applicability::MachineApplicable,
                    ),
                    Some(t) => {
                        let shown = self.types.display(sig.ret);
                        diag.primary(t.span, format!("this says it returns `{shown}`")).suggest_replace(
                            "make it return `Code`",
                            t.span,
                            "Code",
                            Applicability::MachineApplicable,
                        )
                    }
                };
                self.report(diag);
                ok = false;
            }
        }
        self.macros.checked.insert(decl, ok);
        ok
    }

    // ----- `quote` --------------------------------------------------------------------

    /// Lowers `quote do … end` in a macro body: evaluates the splices and
    /// records the template, giving a `Code` value.
    pub fn lower_quote(&mut self, quote: &ast::QuoteExpr, span: Span) -> ir::Expr {
        if !self.macros.in_macro {
            let diag = Diagnostic::error(codes::QUOTE_OUTSIDE_MACRO, "`quote` only works inside a `macro def`")
                .primary(span, "this is not inside a macro")
                .note("a `quote` builds code for a macro to return, and the macro's caller gets that code in place of the call");
            let diag = match quote.body.as_slice() {
                [ast::Stmt { kind: StmtKind::Expr(e), attrs, .. }] if attrs.is_empty() && quote.splices.is_empty() => {
                    let text = self.source_text(e.span);
                    diag.suggest_replace("write the code directly", span, text, Applicability::MaybeIncorrect)
                }
                _ => diag.help("write the code directly, or move the `quote` into a `macro def … -> Code` and call the macro where the code should go"),
            };
            self.report(diag);
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        let template = match self.macros.template_ids.get(&span) {
            Some(&t) => t,
            None => {
                let t = self.macros.templates.len() as u32;
                self.macros.templates.push(Template { quote: quote.clone(), file: span.file });
                self.macros.template_ids.insert(span, t);
                t
            }
        };
        let mut args = Vec::with_capacity(quote.splices.len());
        for splice in &quote.splices {
            let v = self.expr(splice, None);
            if !self.spliceable(v.ty) {
                let shown = self.types.display(v.ty);
                self.report(
                    Diagnostic::error(codes::SPLICE_MISMATCH, format!("a `{shown}` can't be spliced into code"))
                        .primary(splice.span, format!("this is {} `{shown}`", wid_diagnostics::a_or_an(&shown)))
                        .note(SPLICEABLE)
                        .help("splice the code that makes the value instead, like `#{code}` with `code: Code`"),
                );
            }
            let v = if v.is_pure() { v } else { self.spill(v) };
            args.push(v);
        }
        let code = self.types.code();
        ir::Expr::new(ExprKind::Builtin { op: ir::Builtin::Quote { template }, args, span }, code)
    }

    /// Whether a value of type `ty` can be spliced.
    fn spliceable(&self, ty: TyId) -> bool {
        let kind = |t: TyId| self.types.kind(self.types.base(t));
        match kind(ty) {
            TyKind::Code
            | TyKind::Symbol
            | TyKind::Type
            | TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Bool
            | TyKind::String
            | TyKind::Unknown => true,
            TyKind::Array(e, _) | TyKind::Slice(e) | TyKind::Dynamic(e) => {
                matches!(kind(*e), TyKind::Code | TyKind::Symbol | TyKind::Unknown)
            }
            _ => false,
        }
    }

    // ----- calls ----------------------------------------------------------------------

    /// Expands a macro call and lowers the code it generates where the call
    /// is. The value is the value of the code's last statement.
    pub fn call_macro(&mut self, call: MacroCall<'_>, expected: Option<TyId>) -> ir::Expr {
        match self.expand(&call) {
            Some(code) => self.lower_generated(code, expected),
            None => ir::Expr::new(ExprKind::Zero, self.types.unknown()),
        }
    }

    /// Runs a macro for a call and builds the code it generates. `None`
    /// means an error was reported.
    fn expand(&mut self, call: &MacroCall<'_>) -> Option<&'a [ast::Stmt]> {
        let errors = self.diags.error_count();
        let parent = self.virtual_file(call.span.file).map(|v| v.expansion as usize);
        let depth = parent.map_or(0, |e| self.macros.expansions[e].depth) + 1;
        if depth > MAX_DEPTH {
            // Report each runaway macro once, not at every call it makes.
            if self.macros.too_deep.insert(call.decl) {
                self.report(
                    Diagnostic::error(
                        codes::COMPTIME_LIMIT,
                        format!("macro expansions nest more than {MAX_DEPTH} deep"),
                    )
                    .primary(call.span, format!("`{}` would expand here, {depth} expansions deep", call.shown))
                    .note("the code a macro generates may call macros, but no deeper than this")
                    .help("look for a macro whose code calls a macro, maybe itself, with nothing to stop it"),
                );
            }
            return None;
        }
        if self.macros.expansions.len() >= MAX_EXPANSIONS {
            if !std::mem::replace(&mut self.macros.too_many, true) {
                self.report(
                    Diagnostic::error(codes::COMPTIME_LIMIT, "this build expands more than 65,536 macro calls")
                        .primary(call.span, format!("`{}` would expand here", call.shown))
                        .help("look for a macro whose code calls macros several times each, which multiplies"),
                );
            }
            return None;
        }
        if !self.check_macro(call.decl) {
            return None;
        }
        if let Some(b) = call.block {
            self.report(
                Diagnostic::error(codes::BLOCK_MISMATCH, format!("the macro `{}` does not take a block", call.shown))
                    .primary(b.span, "this block is never used")
                    .help("pass the code as an argument instead; a `Code` parameter receives it unevaluated"),
            );
        }
        let mut code = CallCode::default();
        let values = self.macro_args(call, &mut code)?;
        if self.diags.error_count() > errors {
            return None;
        }
        let (result, fragments) = self.run_macro(call, values, code.code.len() as u64)?;
        let expansion = self.macros.expansions.len() as u32;
        self.macros.expansions.push(Expansion { call_site: call.span, name: call.shown.clone(), depth });
        let stmts = self.build_code(expansion, result, &code, &fragments, call)?;
        Some(self.generated.alloc(stmts).as_slice())
    }

    /// Converts a call's arguments to the values the macro receives.
    fn macro_args(&mut self, call: &MacroCall<'_>, code: &mut CallCode) -> Option<Vec<ir::Expr>> {
        let sig = self.fn_sig(call.decl);
        let d = self.decls[call.decl.0 as usize].clone();
        let DeclKind::Fn(f) = d.kind else { return None };
        let ordered = self.match_args(d.name, &sig.params, call.args, call.name_span, call.span, call.decl);
        let splat_at = f.params.iter().position(|p| p.splat);
        let mut values = Vec::with_capacity(sig.params.len());
        let mut ok = true;
        for (i, param) in sig.params.iter().enumerate() {
            if splat_at == Some(i) {
                let elem = match self.types.kind(param.ty) {
                    TyKind::Slice(e) => *e,
                    _ => param.ty,
                };
                let mut elems = Vec::new();
                for arg in call.args.iter().filter(|a| a.name.is_none()).skip(i) {
                    match self.macro_arg(call, param, &arg.value, elem, code) {
                        Some(v) => elems.push(v),
                        None => ok = false,
                    }
                }
                values.push(self.comptime_array(elem, elems, param.ty));
                continue;
            }
            match &ordered[i] {
                ArgSource::Given(e) => match self.macro_arg(call, param, e, param.ty, code) {
                    Some(v) => values.push(v),
                    None => ok = false,
                },
                ArgSource::Default(v) => values.push(v.clone()),
                ArgSource::Missing => ok = false,
            }
        }
        ok.then_some(values)
    }

    /// Converts one argument by the type of its parameter.
    fn macro_arg(
        &mut self,
        call: &MacroCall<'_>,
        param: &super::ParamSig,
        e: &ast::Expr,
        ty: TyId,
        code: &mut CallCode,
    ) -> Option<ir::Expr> {
        match self.types.kind(self.types.base(ty)).clone() {
            TyKind::Code => {
                code.code.push(vec![ast::Stmt { kind: StmtKind::Expr(e.clone()), span: e.span, attrs: Vec::new() }]);
                Some(ir::Expr::new(ExprKind::Int(code.code.len() as i128), ty))
            }
            TyKind::Symbol => match e.kind {
                E::Symbol(name) if splice_index(name).is_none() => {
                    code.symbols.push((name, e.span));
                    Some(ir::Expr::new(ExprKind::Int(i128::from(name.index())), ty))
                }
                _ => {
                    self.wrong_symbol_argument(call, param, e);
                    None
                }
            },
            TyKind::Type => match self.macro_type_arg(e) {
                Some(t) => Some(ir::Expr::new(ExprKind::Int(i128::from(t.0)), ty)),
                None => {
                    self.report(
                        Diagnostic::error(
                            codes::MACRO_ARGUMENT,
                            format!("the macro `{}` takes a type for `{}`", call.shown, param.name),
                        )
                        .primary(e.span, "this is not a type")
                        .secondary(param.span, format!("`{}` is a `Type` parameter", param.name))
                        .help("pass a type, like `Int`, `Vec2` or `[]String`"),
                    );
                    None
                }
            },
            TyKind::Unknown => None,
            _ => {
                let loc = self.loc_at(e.span);
                self.comptime_value(ComptimeCode::Expr(e), Some(ty), loc, e.span, true)
            }
        }
    }

    /// Reports a `Symbol` parameter given something other than a symbol
    /// literal.
    fn wrong_symbol_argument(&mut self, call: &MacroCall<'_>, param: &super::ParamSig, e: &ast::Expr) {
        let diag = Diagnostic::error(
            codes::MACRO_ARGUMENT,
            format!("the macro `{}` takes a symbol for `{}`", call.shown, param.name),
        )
        .primary(e.span, "this is not a symbol literal")
        .secondary(param.span, format!("`{}` is a `Symbol` parameter", param.name))
        .note("a `Symbol` parameter receives a name written as a symbol literal, like `:hp`, not the value of an expression");
        let name = match &e.kind {
            E::Ident(n) | E::Const(n) => Some(n.as_str().to_string()),
            E::Str(parts) => match parts.as_slice() {
                [ast::StrPart::Text(t)] if is_identifier(t) => Some(t.clone()),
                _ => None,
            },
            _ => None,
        };
        let diag = match name {
            Some(n) => diag.suggest_replace(
                format!("pass the name `{n}` as a symbol"),
                e.span,
                format!(":{n}"),
                Applicability::MachineApplicable,
            ),
            None => diag.help(
                "pass a symbol literal, like `:hp`; to compute a name, take a `String` and turn it into a `Symbol` with `.to_sym` inside the macro",
            ),
        };
        self.report(diag);
    }

    /// The type a `Type` argument names, or `None` when it names none.
    fn macro_type_arg(&mut self, e: &ast::Expr) -> Option<TyId> {
        match &e.kind {
            E::Call(c) if matches!(&c.callee, ast::Callee::Name(n) if n.as_str().starts_with(char::is_uppercase)) => {
                match self.classify_receiver(e) {
                    Receiver::Type(t) => Some(t),
                    _ => None,
                }
            }
            _ if is_type_like(e) => {
                let texpr = expr_as_type(e);
                let ctx = self.body_ctx();
                Some(self.resolve_type(&texpr, &ctx))
            }
            _ => None,
        }
    }

    /// Runs the macro with its arguments' values. Returns the `Code` value
    /// it returned and the fragments it recorded.
    fn run_macro(&mut self, call: &MacroCall<'_>, values: Vec<ir::Expr>, first: u64) -> Option<(u64, Vec<Fragment>)> {
        let code = self.types.code();
        let func = self.fn_instance(call.decl);
        let mut wrapper = self.new_function_shell(format!("macro {}", call.shown), String::new(), code, call.span);
        let run = ir::Expr::new(ExprKind::Call { func, args: values }, code);
        wrapper.body = Some(ir::Block { stmts: vec![Stmt::Return(Some(run))] });
        let errors = self.diags.error_count();
        let bounds = std::mem::replace(&mut self.no_bounds_check, false);
        let reached_failed = self.lower_needed(&wrapper);
        self.no_bounds_check = bounds;
        if self.diags.error_count() > errors || reached_failed {
            return None;
        }
        let run = self.interpret(&wrapper, call.span, first);
        let what = format!("the macro `{}`", call.shown);
        self.report_output(&run.output, &what, call.span);
        match run.result {
            Ok(bytes) => {
                let mut word = [0u8; 8];
                for (w, b) in word.iter_mut().zip(&bytes) {
                    *w = *b;
                }
                Some((u64::from_le_bytes(word), run.fragments))
            }
            Err(failure) => {
                self.report_failure(failure, call.span, &what, &format!("while `{}` ran for this call", call.shown));
                None
            }
        }
    }

    /// Builds the statements a `Code` value stands for, reporting splices
    /// that don't fit (E0911).
    fn build_code(
        &mut self,
        expansion: u32,
        result: u64,
        code: &CallCode,
        fragments: &[Fragment],
        call: &MacroCall<'_>,
    ) -> Option<Vec<ast::Stmt>> {
        let first_file = self.macros.files.len();
        let MacroState { templates, files, file_ids, file_locs, .. } = &mut self.macros;
        let mut ex = Expander {
            templates,
            args: &code.code,
            fragments,
            expansion,
            call: call.span,
            name: &call.shown,
            symbols: &code.symbols,
            files,
            file_ids,
            file_locs,
            errors: Vec::new(),
        };
        let stmts = ex.code(result);
        let errors = ex.errors;
        self.register_virtual_files(first_file);
        let failed = !errors.is_empty();
        for diag in errors {
            self.report(diag);
        }
        (!failed).then_some(stmts)
    }

    /// Lowers generated statements where the call was: each in place, the
    /// last one giving the value.
    fn lower_generated(&mut self, stmts: &'a [ast::Stmt], expected: Option<TyId>) -> ir::Expr {
        let void = self.types.void();
        let Some((last, init)) = stmts.split_last() else {
            return ir::Expr::new(ExprKind::Zero, void);
        };
        // A lone expression lowers as part of the caller's statement; other
        // code gets the `Line`s of its own statements, and the caller's
        // `Line` back after them.
        let line = self.macros.line;
        self.lower_stmts(init, Dest::Discard);
        let mut lines = !init.is_empty();
        let value = match &last.kind {
            StmtKind::Expr(e) if last.attrs.is_empty() && produces_value(last) && !self.current_block_diverges() => {
                if lines {
                    self.emit(Stmt::Line(last.span));
                }
                let saved = self.enter_site(last.span);
                let v = self.expr(e, expected);
                self.leave_site(saved);
                v
            }
            _ => {
                self.lower_stmts(std::slice::from_ref(last), Dest::Discard);
                lines = true;
                ir::Expr::new(ExprKind::Zero, void)
            }
        };
        if lines && line != Span::default() {
            self.emit(Stmt::Line(line));
        }
        value
    }
}

/// Whether text is a plain identifier, so `:text` is a symbol literal.
fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|c| c.is_alphabetic() || c == '_') && chars.all(|c| c.is_alphanumeric() || c == '_')
}

// ----- building code ---------------------------------------------------------------------

/// Builds the code of `Code` values for one expansion.
struct Expander<'x> {
    templates: &'x [Template],
    /// The call's `Code` arguments: `Code` value `i + 1` is `args[i]`.
    args: &'x [Vec<ast::Stmt>],
    /// The `quote`s the macro ran: `Code` value `args.len() + i + 1`.
    fragments: &'x [Fragment],
    expansion: u32,
    call: Span,
    name: &'x str,
    symbols: &'x [(Name, Span)],
    files: &'x mut Vec<VirtualFile>,
    file_ids: &'x mut HashMap<(u32, FileId), u32>,
    file_locs: &'x HashMap<FileId, DeclLoc>,
    errors: Vec<Diagnostic>,
}

impl Expander<'_> {
    /// The statements a `Code` value stands for.
    fn code(&mut self, value: u64) -> Vec<ast::Stmt> {
        let Some(index) = value.checked_sub(1).map(|i| i as usize) else { return Vec::new() };
        let args = self.args;
        if let Some(stmts) = args.get(index) {
            return stmts.clone();
        }
        let fragments = self.fragments;
        let templates = self.templates;
        let Some(fragment) = fragments.get(index - args.len()) else { return Vec::new() };
        let Some(template) = templates.get(fragment.template as usize) else { return Vec::new() };
        let mut body = template.quote.body.clone();
        let to = self.virtual_file(template.file);
        Respan { from: template.file, to }.visit_stmts(&mut body);
        Splicer { ex: self, values: &fragment.values }.visit_stmts(&mut body);
        body
    }

    /// The virtual file of a template file in this expansion.
    fn virtual_file(&mut self, template: FileId) -> FileId {
        let key = (self.expansion, template);
        if let Some(&i) = self.file_ids.get(&key) {
            return FileId::expansion(i);
        }
        let loc = match template.expansion_index() {
            Some(i) => self.files.get(i as usize).map(|v| v.loc),
            None => self.file_locs.get(&template).copied(),
        };
        let index = self.files.len() as u32;
        let loc = loc.unwrap_or(DeclLoc { pkg: PackageId(0), file: 0 });
        self.files.push(VirtualFile { template, expansion: self.expansion, loc });
        self.file_ids.insert(key, index);
        FileId::expansion(index)
    }
}

/// Moves the spans of a template into its virtual file.
struct Respan {
    from: FileId,
    to: FileId,
}

impl VisitMut for Respan {
    fn visit_span(&mut self, span: &mut Span) {
        if span.file == self.from {
            span.file = self.to;
        }
    }
}

/// Replaces the splices of one template with their values. Substituted
/// code is final: it is not visited again.
struct Splicer<'s, 'x> {
    ex: &'s mut Expander<'x>,
    values: &'s [SpliceValue],
}

/// What a splice found where it was put, for messages.
fn describe(value: &SpliceValue) -> &'static str {
    match value {
        SpliceValue::Code(_) => "code",
        SpliceValue::Codes(_) => "a `[]Code`",
        SpliceValue::Symbol(_) => "a `Symbol`",
        SpliceValue::Symbols(_) => "a `[]Symbol`",
        SpliceValue::Type(_) => "a `Type`",
        SpliceValue::Int(_) => "an integer",
        SpliceValue::Float(_) => "a float",
        SpliceValue::Bool(_) => "a `Bool`",
        SpliceValue::Str(_) => "a `String`",
    }
}

/// An identifier expression: a constant name if it starts with an
/// uppercase letter, like the parser reads it.
fn name_expr(name: Name, span: Span) -> ast::Expr {
    let kind = if name.as_str().starts_with(char::is_uppercase) { E::Const(name) } else { E::Ident(name) };
    ast::Expr { kind, span }
}

/// The one expression of a statement list, if that is all it is.
fn single_expr(stmts: &[ast::Stmt]) -> Option<&ast::Expr> {
    match stmts {
        [ast::Stmt { kind: StmtKind::Expr(e), attrs, .. }] if attrs.is_empty() => Some(e),
        _ => None,
    }
}

impl Splicer<'_, '_> {
    fn value(&self, i: u32) -> Option<SpliceValue> {
        self.values.get(i as usize).cloned()
    }

    /// Reports a splice whose value doesn't fit where it is (E0911).
    fn mismatch(&mut self, at: Span, found: &str, place: &str, help: &str) {
        let name = self.ex.name;
        self.ex.errors.push(
            Diagnostic::error(codes::SPLICE_MISMATCH, format!("the macro `{name}` splices {found} where {place} goes"))
                .primary(self.ex.call, format!("`{name}` expands here"))
                .secondary(at, format!("this splice is {found}"))
                .help(help.to_string()),
        );
    }

    /// Where a name spliced from a `Symbol` is: in the symbol argument that
    /// named it, or the call.
    fn name_span(&self, name: Name) -> Span {
        match self.ex.symbols.iter().find(|(n, _)| *n == name) {
            Some((_, s)) => {
                let len = name.as_str().len() as u32;
                if s.len() > len { Span { start: s.end - len, ..*s } } else { *s }
            }
            None => self.ex.call,
        }
    }

    /// Where a symbol literal spliced from a `Symbol` is.
    fn literal_span(&self, name: Name) -> Span {
        self.ex.symbols.iter().find(|(n, _)| *n == name).map_or(self.ex.call, |(_, s)| *s)
    }

    /// The code of a `Code` value as one expression.
    fn code_expr(&mut self, code: u64, at: Span, place: &str) -> Option<ast::Expr> {
        let stmts = self.ex.code(code);
        if let Some(e) = single_expr(&stmts) {
            return Some(e.clone());
        }
        let found = match stmts.len() {
            0 => "empty code",
            1 => "a statement",
            _ => "several statements",
        };
        self.mismatch(
            at,
            found,
            place,
            "splice statements alone on a line; where a value goes, splice code that is one expression",
        );
        None
    }

    /// The expression a splice stands for.
    fn expr_for(&mut self, i: u32, at: Span) -> ast::Expr {
        let error = ast::Expr { kind: E::Error, span: at };
        let Some(value) = self.value(i) else { return error };
        let kind = match value {
            SpliceValue::Code(c) => return self.code_expr(c, at, "an expression").unwrap_or(error),
            SpliceValue::Symbol(n) => return name_expr(n, self.name_span(n)),
            SpliceValue::Type(t) => E::Type(Box::new(ast::TypeExpr { kind: TypeKind::Spliced(t.0), span: at })),
            SpliceValue::Int(v) => {
                let int = E::Int(v.unsigned_abs());
                if v < 0 { negate(int, at) } else { int }
            }
            SpliceValue::Float(f) if f.is_finite() => {
                if f.is_sign_negative() {
                    negate(E::Float(-f), at)
                } else {
                    E::Float(f)
                }
            }
            SpliceValue::Float(f) => {
                self.mismatch(
                    at,
                    &format!("`{f}`"),
                    "a literal",
                    "a float literal is finite; splice code that computes the value instead",
                );
                return error;
            }
            SpliceValue::Bool(b) => {
                if b {
                    E::True
                } else {
                    E::False
                }
            }
            SpliceValue::Str(s) => E::Str(vec![ast::StrPart::Text(s)]),
            SpliceValue::Codes(_) | SpliceValue::Symbols(_) => {
                let found = describe(&value);
                self.mismatch(
                    at,
                    found,
                    "one expression",
                    "a list splices only where a list goes: alone on a line, or among call arguments or array elements",
                );
                return error;
            }
        };
        ast::Expr { kind, span: at }
    }

    /// The name a splice in a name position stands for.
    fn name_for(&mut self, i: u32, at: Span) -> Option<Name> {
        let value = self.value(i)?;
        match value {
            SpliceValue::Symbol(n) => return Some(n),
            SpliceValue::Code(c) => {
                let stmts = self.ex.code(c);
                if let Some(ast::Expr { kind: E::Ident(n) | E::Const(n), .. }) = single_expr(&stmts) {
                    return Some(*n);
                }
            }
            _ => {}
        }
        let found = describe(&value);
        self.mismatch(
            at,
            found,
            "a name",
            "splice a `Symbol` where a name goes; `\"text\".to_sym` makes one from a string",
        );
        None
    }

    /// The type a splice in a type position stands for.
    fn type_for(&mut self, i: u32, at: Span) -> ast::TypeExpr {
        let error = ast::TypeExpr { kind: TypeKind::Error, span: at };
        let Some(value) = self.value(i) else { return error };
        match &value {
            SpliceValue::Type(t) => return ast::TypeExpr { kind: TypeKind::Spliced(t.0), span: at },
            SpliceValue::Symbol(n) => {
                let segment = Ident { name: *n, span: self.name_span(*n) };
                return ast::TypeExpr { kind: TypeKind::Path { segments: vec![segment], args: Vec::new() }, span: at };
            }
            SpliceValue::Code(c) => {
                let stmts = self.ex.code(*c);
                if let Some(e) = single_expr(&stmts)
                    && is_type_like(e)
                {
                    return expr_as_type(e);
                }
            }
            _ => {}
        }
        let found = describe(&value);
        self.mismatch(at, found, "a type", "splice a `Type` where a type goes, or a `Symbol` that names one");
        error
    }

    /// For a splice standing in a list (arguments, array elements), the
    /// elements a `[]Code` or `[]Symbol` value gives; `None` for any other
    /// expression.
    fn list_for(&mut self, e: &ast::Expr) -> Option<Vec<ast::Expr>> {
        let (i, literal) = match e.kind {
            E::Splice(i) => (i, false),
            E::Symbol(n) => (splice_index(n)?, true),
            _ => return None,
        };
        match self.value(i)? {
            SpliceValue::Codes(codes) if !literal => {
                Some(codes.into_iter().filter_map(|c| self.code_expr(c, e.span, "a list element")).collect())
            }
            SpliceValue::Symbols(names) => Some(
                names
                    .into_iter()
                    .map(|n| match literal {
                        true => ast::Expr { kind: E::Symbol(n), span: self.literal_span(n) },
                        false => name_expr(n, self.name_span(n)),
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Turns generated statements into declarations, for a splice among
    /// declarations.
    fn push_items(&mut self, stmts: Vec<ast::Stmt>, at: Span, items: &mut Vec<ast::Item>) {
        for stmt in stmts {
            match stmt.kind {
                StmtKind::Item(item) => items.push(*item),
                StmtKind::Expr(e) if matches!(e.kind, E::Call(_) | E::Ident(_) | E::Member { .. }) => {
                    items.push(ast::Item {
                        span: stmt.span,
                        kind: ItemKind::MacroCall(Box::new(e)),
                        attrs: stmt.attrs,
                        private: false,
                        doc: None,
                    });
                }
                _ => self.mismatch(
                    at,
                    "a statement",
                    "a declaration",
                    "only declarations go here: `def`, `struct`, constants and the like, or macro calls",
                ),
            }
        }
    }
}

/// `-x` for a literal, in parentheses so it stays one operand.
fn negate(literal: E, span: Span) -> E {
    let inner = ast::Expr { kind: literal, span };
    let neg = ast::Expr { kind: E::Unary { op: ast::UnOp::Neg, expr: Box::new(inner) }, span };
    E::Paren(Box::new(neg))
}

impl VisitMut for Splicer<'_, '_> {
    fn visit_stmts(&mut self, stmts: &mut Vec<ast::Stmt>) {
        for mut stmt in std::mem::take(stmts) {
            if let StmtKind::Expr(ast::Expr { kind: E::Splice(i), .. }) = &stmt.kind
                && stmt.attrs.is_empty()
            {
                match self.value(*i) {
                    Some(SpliceValue::Code(c)) => {
                        stmts.extend(self.ex.code(c));
                        continue;
                    }
                    Some(SpliceValue::Codes(codes)) => {
                        for c in codes {
                            stmts.extend(self.ex.code(c));
                        }
                        continue;
                    }
                    _ => {}
                }
            }
            self.visit_stmt(&mut stmt);
            stmts.push(stmt);
        }
    }

    fn visit_items(&mut self, items: &mut Vec<ast::Item>) {
        for mut item in std::mem::take(items) {
            if let ItemKind::Splice(i) = item.kind {
                match self.value(i) {
                    Some(SpliceValue::Code(c)) => {
                        let stmts = self.ex.code(c);
                        self.push_items(stmts, item.span, items);
                    }
                    Some(SpliceValue::Codes(codes)) => {
                        for c in codes {
                            let stmts = self.ex.code(c);
                            self.push_items(stmts, item.span, items);
                        }
                    }
                    Some(other) => {
                        let found = describe(&other);
                        self.mismatch(
                            item.span,
                            found,
                            "a declaration",
                            "splice `Code` or `[]Code` among declarations; `Symbol`s go among the members of an `enum`",
                        );
                    }
                    None => {}
                }
                continue;
            }
            self.visit_item(&mut item);
            items.push(item);
        }
    }

    fn visit_item(&mut self, item: &mut ast::Item) {
        // In an enum body, `Symbol`s spliced alone on a line are members.
        if let ItemKind::Enum(e) = &mut item.kind {
            let mut body = Vec::with_capacity(e.body.len());
            for it in std::mem::take(&mut e.body) {
                let value = match it.kind {
                    ItemKind::Splice(i) => self.value(i),
                    _ => None,
                };
                let names = match value {
                    Some(SpliceValue::Symbol(n)) => vec![n],
                    Some(SpliceValue::Symbols(ns)) => ns,
                    _ => {
                        body.push(it);
                        continue;
                    }
                };
                for n in names {
                    let span = self.name_span(n);
                    e.members.push(ast::EnumMember { name: Ident { name: n, span }, value: None });
                }
            }
            e.body = body;
        }
        walk_item(self, item);
    }

    fn visit_expr(&mut self, e: &mut ast::Expr) {
        let span = e.span;
        match e.kind {
            E::Splice(i) => *e = self.expr_for(i, span),
            // A `quote` in generated code has splices of its own.
            E::Quote(_) => {}
            E::IVar(n) | E::Symbol(n) | E::Ident(n) | E::Const(n) if splice_index(n).is_some() => {
                let i = splice_index(n).unwrap_or_default();
                let Some(value) = self.value(i) else { return };
                if matches!(e.kind, E::Symbol(_)) {
                    match value {
                        SpliceValue::Symbol(name) => {
                            *e = ast::Expr { kind: E::Symbol(name), span: self.literal_span(name) }
                        }
                        other => {
                            let found = describe(&other);
                            self.mismatch(
                                span,
                                found,
                                "the name of a symbol literal",
                                "`:#{…}` makes a symbol literal from a `Symbol`",
                            );
                            e.kind = E::Error;
                        }
                    }
                    return;
                }
                match (self.name_for(i, span), &e.kind) {
                    (Some(name), E::IVar(_)) => e.kind = E::IVar(name),
                    (Some(name), _) => *e = name_expr(name, self.name_span(name)),
                    (None, _) => e.kind = E::Error,
                }
            }
            _ => walk_expr(self, e),
        }
    }

    fn visit_exprs(&mut self, exprs: &mut Vec<ast::Expr>) {
        for mut e in std::mem::take(exprs) {
            if let Some(list) = self.list_for(&e) {
                exprs.extend(list);
                continue;
            }
            self.visit_expr(&mut e);
            exprs.push(e);
        }
    }

    fn visit_args(&mut self, args: &mut Vec<ast::Arg>) {
        for mut arg in std::mem::take(args) {
            if arg.name.is_none()
                && !arg.splat
                && let Some(list) = self.list_for(&arg.value)
            {
                args.extend(list.into_iter().map(|value| ast::Arg { name: None, value, splat: false }));
                continue;
            }
            if let Some(name) = &mut arg.name {
                self.visit_ident(name);
            }
            self.visit_expr(&mut arg.value);
            args.push(arg);
        }
    }

    fn visit_ident(&mut self, ident: &mut Ident) {
        if let Some(i) = ident.splice_index()
            && let Some(name) = self.name_for(i, ident.span)
        {
            *ident = Ident { name, span: self.name_span(name) };
        }
    }

    fn visit_type(&mut self, ty: &mut ast::TypeExpr) {
        if let TypeKind::Splice(i) = ty.kind {
            *ty = self.type_for(i, ty.span);
            return;
        }
        walk_type(self, ty);
    }
}

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
//!    every statement in place, the last one giving the call's value, or
//!    for a call that is a statement of its own, as a statement too.
//!
//! Generated statements are allocated in `Checker::generated`, an arena
//! that lives as long as the input syntax, so declarations built from them
//! can be collected like written ones. Calls among declarations (in type
//! bodies and at package level) are queued with the pending `comptime if`s
//! and expanded by `decl_macros.rs`, which turns the built statements into
//! declarations ([`lines_to_items`], which [`Splicer`] uses too for code
//! spliced among declarations).
//!
//! A macro whose own code uses `Self` ([`Checker::macro_self_use`]) runs as
//! an instance for the `Self` of each call: the type whose body or method
//! holds it.
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
//! macro's file. Code spliced in from the call site keeps its own spans; the
//! expander records each such [`Splice`], so a diagnostic in that code also
//! points at the splice and lists the calls ([`Checker::splice_context`]).
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
use super::decl_macros::lines_to_items;
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

/// Code that an expansion spliced in from outside it: a `Code` argument's
/// code, or a name from a `Symbol`. It keeps its own span, so an error in it
/// points there; [`Checker::splice_context`] adds where it landed.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Splice {
    /// The spliced code: at the call site, or for a name from a computed
    /// `Symbol` (`computed`), the call.
    pub code: Span,
    /// The splice in the `quote`, in the expansion's virtual file.
    pub site: Span,
    /// The name, for a name.
    pub name: Option<Name>,
    /// Whether the name is one the macro computed, whose span is the call's.
    pub computed: bool,
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
    /// For each macro looked at, the first `Self` its own code uses (not
    /// the code its `quote`s generate), if any.
    pub self_uses: HashMap<DeclId, Option<Span>>,
    /// Packages where a macro call at package level failed to expand or
    /// named no macro: a name missing there may be one it would have
    /// declared, so it isn't reported (like a failed `cimport` merge; see
    /// [`Checker::pkg_incomplete`]). Nor is a missing member of any type
    /// while this isn't empty, since the call may have generated an
    /// `extend` (see [`Checker::members_incomplete`]).
    pub failed_packages: HashSet<PackageId>,
    /// Structs, enums, modules and `extend`s in whose body a macro call
    /// failed to expand or named no macro: a member missing on their types
    /// may be one it would have generated (see
    /// [`Checker::members_incomplete`]).
    pub failed_owners: HashSet<DeclId>,
    /// Fields that a macro call in a struct's body generated and E0913
    /// rejected, by struct: their uses aren't reported missing (see
    /// [`Checker::field_rejected`]).
    pub rejected_fields: HashSet<(DeclId, Name)>,
    /// The code every expansion spliced in from outside it, by the file of
    /// its span.
    pub splices: HashMap<FileId, Vec<Splice>>,
    /// While the last line of an expansion whose call is a statement is
    /// lowered, that line's span: a macro call that is the whole line is a
    /// statement too (see [`Checker::lower_generated`]).
    pub discarded: Option<Span>,
    /// While the last line of an expansion gives the call's value and is an
    /// `if`, `case` or `comptime if`: the expressions that end its branches,
    /// the macro's name and the line's keyword, innermost expansion last
    /// (see [`Checker::call_value_note`]).
    pub value_tails: Vec<(Vec<Span>, String, &'static str)>,
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

    /// Gives a diagnostic about code that an expansion spliced in from
    /// outside it (see [`Splice`]) the context of an error in generated
    /// code: a label on the splice in the `quote`, behind which the
    /// renderers list the macro calls ([`Diagnostic::splice`]). A name the
    /// macro computed has the call's span, so the primary label there says
    /// which name it is (`` `label`, spliced by `wrap`, has type … `` for a
    /// label about "this"), and an edit there, which would replace the call,
    /// is dropped. An error about a value that gives a call's value gets a
    /// note saying so ([`Checker::call_value_note`]).
    pub(super) fn splice_context(&self, diag: Diagnostic) -> Diagnostic {
        let mut diag = self.call_value_note(diag);
        if diag.labels.iter().any(|l| l.splice) {
            return diag;
        }
        let Some(primary) = diag.primary_span() else { return diag };
        let Some(splices) = self.macros.splices.get(&primary.file) else { return diag };
        let within =
            |inner: Span, outer: Span| inner.file == outer.file && outer.start <= inner.start && inner.end <= outer.end;
        let holds = |s: &&Splice| if s.computed { s.code == primary } else { within(primary, s.code) };
        // The innermost spliced code holding the primary span.
        let Some(len) = splices.iter().filter(holds).map(|s| s.code.len()).min() else { return diag };
        let found: Vec<&Splice> = splices.iter().filter(holds).filter(|s| s.code.len() == len).collect();
        // Code spliced more than once, or names a macro computed (which all
        // have the call's span): the name the message mentions, else the
        // splice in the statement or method being checked, else the latest,
        // if they are the same name. Computed names come first.
        let line = self.macros.line;
        let item = self.body.frames.last().and_then(|f| f.decl).map(|d| self.decls[d.0 as usize].item.span);
        let pick = |computed: bool| {
            let tier: Vec<&Splice> = found.iter().copied().filter(|s| s.computed == computed).collect();
            let named =
                tier.iter().find(|s| computed && s.name.is_some_and(|n| diag.message.contains(&format!("`{n}`"))));
            named
                .or_else(|| tier.iter().find(|s| within(s.site, line)))
                .or_else(|| item.and_then(|item| tier.iter().find(|s| within(s.site, item))))
                .or_else(|| {
                    let last = tier.iter().max_by_key(|s| s.site.file)?;
                    tier.iter().all(|s| s.name == last.name).then_some(last)
                })
                .map(|s| **s)
        };
        let picked = pick(true).or_else(|| pick(false));
        if found.iter().any(|s| s.computed) {
            for help in &mut diag.helps {
                help.edits.retain(|e| e.span != primary);
            }
        }
        let Some(splice) = picked else { return diag };
        let Some(v) = self.virtual_file(splice.site.file) else { return diag };
        let by = &self.macros.expansions[v.expansion as usize].name;
        if splice.computed
            && let Some(name) = splice.name
        {
            for label in diag.labels.iter_mut().filter(|l| l.primary && l.span == primary) {
                label.message = match label.message.strip_prefix("this ") {
                    Some(rest) if ["has ", "is ", "returns "].iter().any(|verb| rest.starts_with(verb)) => {
                        format!("`{name}`, spliced by `{by}`, {rest}")
                    }
                    _ => format!("`{name}`, spliced by `{by}`: {}", label.message),
                };
            }
        }
        match diag.labels.iter_mut().find(|l| l.span == splice.site) {
            Some(label) => label.splice = true,
            None => {
                let message = match splice.name {
                    Some(name) => format!("`{name}` is spliced here by `{by}`"),
                    None => format!("spliced here by `{by}`"),
                };
                diag = diag.splice(splice.site, message);
            }
        }
        diag
    }

    /// The splice in a `quote` that put the name at `name` into the code
    /// at `within`, in the expansion's virtual file. A name spliced in from
    /// a macro call keeps its span there (see [`Splice`]), so the code
    /// around it, like the arguments of `@#{name}(…)`, is found from the
    /// splice. `None` for a name that wasn't spliced into `within`.
    pub(super) fn splice_site(&self, name: Span, within: Span) -> Option<Span> {
        let inside = |s: Span| s.file == within.file && within.start <= s.start && s.end <= within.end;
        self.macros.splices.get(&name.file)?.iter().find(|s| s.code == name && inside(s.site)).map(|s| s.site)
    }

    /// Where the name at `name` ends in the code of `call`, the call it
    /// names: its own end, or for a name spliced in from a macro call, the
    /// end of the splice in the `quote` (see [`Self::splice_site`]).
    pub(super) fn name_end(&self, name: Span, call: Span) -> Option<u32> {
        if name.file == call.file {
            return Some(name.end);
        }
        self.splice_site(name, call).map(|s| s.end)
    }

    /// Explains an error about the value that ends a branch of an `if`,
    /// `case` or `comptime if` on the last line of an expansion whose value
    /// is used: that line gives the call's value, where a statement's last
    /// line would need none (see [`Checker::lower_generated`]).
    fn call_value_note(&self, diag: Diagnostic) -> Diagnostic {
        if diag.code != codes::NOT_A_VALUE && diag.code != codes::TYPE_MISMATCH {
            return diag;
        }
        let Some(primary) = diag.primary_span() else { return diag };
        let Some((_, name, keyword)) =
            self.macros.value_tails.iter().rev().find(|(tails, ..)| tails.contains(&primary))
        else {
            return diag;
        };
        let diag = diag.note(format!(
            "this ends a branch of the `{keyword}` on the last line of `{name}`'s code, which gives the call's value, and the code around the call uses that value"
        ));
        if diag.code == codes::NOT_A_VALUE {
            diag.help(format!(
                "if the value isn't needed, call `{name}` as a statement of its own: then its last line needs no value"
            ))
        } else {
            diag
        }
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

    /// Where a macro's own code first uses `Self`, if it does. Its `quote`
    /// bodies don't count, since generated code resolves `Self` where it
    /// lands; their splices, which the macro runs, do. A macro that uses
    /// `Self` runs as an instance for the `Self` of each call.
    pub(super) fn macro_self_use(&mut self, decl: DeclId) -> Option<Span> {
        if let Some(found) = self.macros.self_uses.get(&decl) {
            return *found;
        }
        let found = match self.decls[decl.0 as usize].kind {
            DeclKind::Fn(f) => {
                let mut find = FindSelf::default();
                let mut body = f.body.clone();
                match &mut body {
                    ast::FnBody::Block(stmts) => find.visit_stmts(stmts),
                    ast::FnBody::Expr(e) => find.visit_expr(e),
                }
                for p in &f.params {
                    if let Some(default) = &p.default {
                        find.visit_expr(&mut default.clone());
                    }
                }
                find.found
            }
            _ => None,
        };
        self.macros.self_uses.insert(decl, found);
        found
    }

    /// The function that runs a macro for a call: one shared instance, or
    /// for a macro that uses `Self`, the instance for the call's `Self`,
    /// the type whose body or method holds the call. `None` means an error
    /// was reported.
    fn macro_instance(&mut self, call: &MacroCall<'_>) -> Option<FnId> {
        let Some(at) = self.macro_self_use(call.decl) else { return Some(self.fn_instance(call.decl)) };
        let self_ty = self.body.frames.last().and_then(|f| f.self_ty);
        match self_ty {
            Some(t) if matches!(self.types.kind(t), TyKind::Unknown) => None,
            Some(t) if !self.has_params(t) => {
                let subst = std::rc::Rc::new(vec![(Name::new("Self"), t)]);
                Some(self.fn_instance_with(call.decl, subst, call.span))
            }
            _ => {
                self.report_no_self(&call.shown, call.span, at, self_ty.is_some(), true);
                None
            }
        }
    }

    /// Reports a macro call that needs a `Self` where there is none, or
    /// where `Self` stands for more than one type (E0209). `at` is the
    /// `Self` that needs it: in the macro's code, or else in an argument.
    pub(super) fn report_no_self(&mut self, shown: &str, call: Span, at: Span, many: bool, in_macro: bool) {
        let label = if in_macro { "the macro's code uses `Self` here" } else { "this argument is `Self`" };
        let diag = if many {
            Diagnostic::error(codes::SELF_OUTSIDE_METHOD, format!("`{shown}` needs one `Self`, but here it stands for many types"))
                .primary(call, format!("`{shown}` expands here, where `Self` has no single type"))
                .secondary(at, label.to_string())
                .note("in a `module` or `extend` body, `Self` is each type that includes or is extended by it, and in a generic struct's body it is each instance; a macro runs once for the call, so it can't use all of them")
        } else {
            Diagnostic::error(codes::SELF_OUTSIDE_METHOD, format!("`{shown}` needs a `Self`, but this call has none"))
                .primary(call, format!("`{shown}` expands here, outside any type"))
                .secondary(at, label.to_string())
                .note("a macro's `Self` is the type whose body or method holds the call")
        };
        let help = if in_macro {
            format!(
                "call `{shown}` in the body or a method of a `struct` or `enum`, or give the macro a `Type` parameter and pass the type"
            )
        } else {
            format!("pass a named type instead of `Self`, or call `{shown}` in the body of each type that needs it")
        };
        self.report(diag.help(help));
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
                    Diagnostic::error(
                        codes::SPLICE_MISMATCH,
                        format!("a value of type `{shown}` can't be spliced into code"),
                    )
                    .primary(splice.span, format!("this has type `{shown}`"))
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
    /// is. The value is the value of the code's last statement, unless the
    /// call is a statement of its own: a whole statement, or the whole last
    /// line of the code of a call that is one.
    pub fn call_macro(&mut self, call: MacroCall<'_>, expected: Option<TyId>) -> ir::Expr {
        let statement = self.macros.line == call.span || self.macros.discarded == Some(call.span);
        match self.expand(&call) {
            Some(code) => self.lower_generated(code, expected, &call.shown, statement),
            None => {
                // What the code would have read and declared is unknown.
                self.failed_expansion(call.span);
                ir::Expr::new(ExprKind::Zero, self.types.unknown())
            }
        }
    }

    /// Whether a name the code being lowered doesn't find may be one that a
    /// macro call which failed to expand would have declared, so that
    /// failure already explains it: a local, after a failed call in a scope
    /// around the code (`local`), or a member of the type whose method is
    /// being lowered, after a failed call among its declarations
    /// (`member`).
    pub fn declared_by_failed_macro(&mut self, local: bool, member: bool) -> bool {
        if local && self.after_failed_expansion() {
            return true;
        }
        if !member || (self.macros.failed_owners.is_empty() && self.macros.failed_packages.is_empty()) {
            return false;
        }
        let Some(frame) = self.body.frames.last() else { return false };
        let self_ty = frame.self_ty;
        let owner = frame.decl.and_then(|d| self.decls[d.0 as usize].owner);
        owner.is_some_and(|o| self.owner_failed(o)) || self_ty.is_some_and(|t| self.members_incomplete(t))
    }

    /// Reports a call of a method that doesn't exist (E0201) and checks its
    /// arguments, suggesting one of `candidates` or of `own` (see
    /// [`Checker::undefined_near`]). A call that may have been meant for a
    /// macro counts as a failed expansion, so the variables its code would
    /// have declared aren't reported missing after it: one whose name is
    /// close to a macro's (`countr :hits`, which the error suggests
    /// replacing), or one standing alone as a statement with a symbol
    /// argument whose name is close to no other.
    pub fn undefined_call(
        &mut self,
        name: Ident,
        args: &[ast::Arg],
        span: Span,
        candidates: &[&'static str],
        own: &super::SelfNames,
    ) -> ir::Expr {
        let loc = self.loc_at(name.span);
        let similar = own.closest(name.as_str(), candidates).map(Name::new);
        let near_macro = similar
            .and_then(|n| self.lookup_pkg(loc.pkg, n).or_else(|| self.lookup_prelude(n)))
            .filter(|&d| self.is_macro(d));
        // A name close to a method's is more likely a misspelled call of it.
        let symbols = args.iter().any(|a| matches!(a.value.kind, E::Symbol(_)));
        let macro_like = near_macro.is_some() || (similar.is_none() && symbols && self.macros.line == span);
        if !self.declared_by_failed_macro(false, true) {
            match near_macro {
                Some(m) => {
                    if !self.undefined_explained(name.name, name.span) {
                        let d = &self.decls[m.0 as usize];
                        let (best, at) = (d.name.as_str(), d.span);
                        self.report(
                            Diagnostic::error(codes::UNDEFINED_NAME, format!("undefined method `{}`", name.name))
                                .primary(name.span, "not found in this scope")
                                .secondary(at, format!("the macro `{best}` is defined here"))
                                .suggest_replace(
                                    format!("a macro with a similar name exists: `{best}`"),
                                    name.span,
                                    best,
                                    Applicability::MaybeIncorrect,
                                ),
                        );
                    }
                }
                None => self.undefined_near(name.name, name.span, candidates, own, "method"),
            }
        }
        for arg in args {
            self.expr(&arg.value, None);
        }
        if macro_like {
            // What the macro's code would have read and declared is unknown.
            self.failed_expansion(span);
        }
        ir::Expr::new(ExprKind::Zero, self.types.unknown())
    }

    /// Runs a macro for a call and builds the code it generates, allocated
    /// for as long as the input syntax. `None` means an error was reported.
    fn expand(&mut self, call: &MacroCall<'_>) -> Option<&'a [ast::Stmt]> {
        let stmts = self.expand_code(call)?;
        Some(self.generated.alloc(stmts).as_slice())
    }

    /// Runs a macro for a call and builds the code it generates. `None`
    /// means an error was reported.
    pub(super) fn expand_code(&mut self, call: &MacroCall<'_>) -> Option<Vec<ast::Stmt>> {
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
        self.build_code(expansion, result, &code, &fragments, call)
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
        let func = self.macro_instance(call)?;
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
            splices: Vec::new(),
        };
        let stmts = ex.code(result, None);
        let (errors, splices) = (ex.errors, ex.splices);
        self.register_virtual_files(first_file);
        let failed = !errors.is_empty();
        for diag in errors {
            self.report(diag);
        }
        if failed {
            return None;
        }
        for splice in splices {
            self.macros.splices.entry(splice.code.file).or_default().push(splice);
        }
        Some(stmts)
    }

    /// Lowers generated statements where the call was: each in place, the
    /// last one giving the value. When the call is a statement of its own
    /// (`statement`), so is the last line, the way the caller would have
    /// written it: an `if`, `case` or `comptime if` there needs no value.
    fn lower_generated(
        &mut self,
        stmts: &'a [ast::Stmt],
        expected: Option<TyId>,
        name: &str,
        statement: bool,
    ) -> ir::Expr {
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
        let branching = match &last.kind {
            StmtKind::Expr(e) => branching_keyword(e),
            _ => None,
        };
        let value = match &last.kind {
            StmtKind::Expr(e)
                if last.attrs.is_empty()
                    && produces_value(last)
                    && !(statement && branching.is_some())
                    && !self.current_block_diverges() =>
            {
                if lines {
                    self.emit(Stmt::Line(last.span));
                }
                let saved = self.enter_site(last.span);
                // A macro call that is the whole line is a statement when
                // this call is one; the branches of an `if` that gives the
                // call's value give it a value.
                let outer = std::mem::replace(&mut self.macros.discarded, statement.then_some(e.span));
                if let Some(keyword) = branching {
                    let mut tails = Vec::new();
                    branch_tails(e, &mut tails);
                    self.macros.value_tails.push((tails, name.to_string(), keyword));
                }
                let v = self.expr(e, expected);
                if branching.is_some() {
                    self.macros.value_tails.pop();
                }
                self.macros.discarded = outer;
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

    // ----- operands -------------------------------------------------------------------

    /// Lowers code that runs apart from the statement around it (see
    /// [`Operand`]) in a block and a scope of its own, so the statements a
    /// macro call there generates run with it: the names they declare are
    /// visible only there, and a `defer` there is an error
    /// ([`Checker::defer_in_operand`]). Returns the block's statements and
    /// the value `lower` gives, which may read locals they declare: the
    /// caller puts the statements right before the value's use, in one
    /// block.
    pub(super) fn lower_operand(
        &mut self,
        operand: Operand,
        lower: impl FnOnce(&mut Self) -> ir::Expr,
    ) -> (Vec<Stmt>, ir::Expr) {
        self.begin_block();
        // Code outside any method body (none declares variables) has no
        // scopes.
        if self.body.frames.is_empty() {
            let value = lower(self);
            return (self.end_block().stmts, value);
        }
        self.push_scope();
        if let Some(scope) = self.frame_mut().scopes.last_mut() {
            scope.operand = Some(operand);
        }
        let mut value = lower(self);
        // Code that changed the context ends in a block of its own, whose
        // locals the value can't read after it: a value is stored in a
        // temporary declared before the block, and a call without one runs
        // inside it.
        let shadowed = self.frame().scopes.last().is_some_and(|s| s.context_shadowed);
        let mut kept = None;
        if shadowed && matches!(self.types.kind(value.ty), TyKind::Void) && !value.is_pure() {
            self.emit(Stmt::Expr(value));
            value = ir::Expr::new(ExprKind::Zero, self.types.void());
        } else if shadowed
            && !value.is_constant()
            && !matches!(self.types.kind(value.ty), TyKind::Void | TyKind::Never | TyKind::Unknown)
        {
            let ty = value.ty;
            let local = self.new_local(None, ty);
            let target = ir::Expr::new(ExprKind::Local(local), ty);
            self.emit(Stmt::Assign { target: target.clone(), value });
            value = target;
            kept = Some(local);
        }
        let out = self.scoped_out(operand);
        self.pop_scope();
        if let Some(scope) = self.body.frames.last_mut().and_then(|f| f.scopes.last_mut()) {
            scope.scoped_out.extend(out);
        }
        let mut stmts = self.end_block().stmts;
        if let Some(local) = kept {
            stmts.insert(0, Stmt::Let { local, init: None });
        }
        (stmts, value)
    }

    /// The variables the innermost scope, an operand's, declared, with
    /// those of the operands inside it, as they leave scope.
    fn scoped_out(&self, operand: Operand) -> Vec<ScopedOut> {
        let Some(scope) = self.body.frames.last().and_then(|f| f.scopes.last()) else { return Vec::new() };
        let declared = scope.vars.iter().map(|v| ScopedOut {
            name: v.name,
            mark: v.mark,
            span: v.span,
            operand,
            by: self.declaring_macro(v.name, v.span),
        });
        let mut out: Vec<ScopedOut> = declared.collect();
        out.extend(scope.scoped_out.iter().cloned());
        out
    }

    /// The macro whose code declared a variable at `span`: the expansion
    /// whose `quote` wrote the name, or that spliced it in from a `Symbol`.
    fn declaring_macro(&self, name: Name, span: Span) -> Option<String> {
        let file = match self.virtual_file(span.file) {
            Some(_) => span.file,
            None => {
                let splices = self.macros.splices.get(&span.file)?;
                splices.iter().rev().find(|s| s.code == span && s.name == Some(name))?.site.file
            }
        };
        let v = self.virtual_file(file)?;
        Some(self.macros.expansions[v.expansion as usize].name.clone())
    }

    /// Reports a `defer` at `span` that would join an operand's scope (see
    /// [`Checker::lower_operand`]) and returns whether it did: no block
    /// ends when it should run. Its body is still checked.
    pub(super) fn defer_in_operand(&mut self, span: Span) -> bool {
        let Some(operand) = self.body.frames.last().and_then(|f| f.scopes.last()).and_then(|s| s.operand) else {
            return false;
        };
        let what = operand.describe();
        let why = match operand.kind {
            OperandKind::TypeInfo => {
                "a `defer` runs when its block ends, but `type_info`'s operand is checked and never runs, so nothing would reach the `defer`".to_string()
            }
            OperandKind::Condition { .. } => format!(
                "a `defer` runs when its block ends, but {what} runs again on each test of the loop and has no block of its own"
            ),
            OperandKind::Logical { .. }
            | OperandKind::LogicalAssign { .. }
            | OperandKind::SafeCall
            | OperandKind::WhenPattern => format!(
                "a `defer` runs when its block ends, but code in {what} {}, so the end of the block around it can't tell whether the `defer` was reached",
                operand.runs()
            ),
        };
        let call = self.virtual_file(span.file).map(|v| &self.macros.expansions[v.expansion as usize]);
        let diag = match call {
            Some(e) => {
                let name = e.name.clone();
                let help = match operand.kind {
                    OperandKind::Logical { .. }
                    | OperandKind::LogicalAssign { .. }
                    | OperandKind::SafeCall
                    | OperandKind::WhenPattern => format!(
                        "call `{name}` in a branch of an `if` instead, whose end runs the `defer`; or, if its code may always run, as a statement of its own before this line (`v = {name}(…)`), and use `v` here"
                    ),
                    OperandKind::TypeInfo => format!(
                        "call `{name}` as a statement of its own (`v = {name}(…)`), where the `defer` runs when the block ends, and pass `v` to `type_info`"
                    ),
                    OperandKind::Condition { until } => format!(
                        "test the condition in the loop's body instead: in a `loop`, `break {} {name}(…)` runs the `defer` when each pass ends",
                        if until { "if" } else { "unless" }
                    ),
                };
                Diagnostic::error(
                    codes::DEFER_IN_OPERAND,
                    format!("the code `{name}` generates in {what} has a `defer`"),
                )
                .primary(e.call_site, format!("`{name}` expands here, in {what}"))
                .secondary(span, "this `defer` would wait for the end of the block around it")
                .note(why)
                .help(help)
            }
            None => Diagnostic::error(codes::DEFER_IN_OPERAND, format!("a `defer` in {what}"))
                .primary(span, "this `defer` would wait for the end of the block around it")
                .secondary(operand.span, what)
                .note(why)
                .help("move the `defer` into a statement of its own"),
        };
        self.report(diag);
        true
    }

    /// Reports a name that no code here declares but that a macro's code
    /// declared in an operand around here, whose scope ended with it (see
    /// [`Checker::lower_operand`]). Returns whether it did.
    pub(super) fn report_scoped_out(&mut self, name: Name, span: Span) -> bool {
        let mark = self.mark_at(span);
        let found = self.body.frames.last().and_then(|f| {
            f.scopes.iter().rev().find_map(|s| s.scoped_out.iter().rev().find(|o| o.name == name && o.mark == mark))
        });
        let Some(out) = found.cloned() else { return false };
        if self.undefined_explained(name, span) {
            return true;
        }
        let what = out.operand.describe();
        let (label, note, help) = match &out.by {
            Some(by) => (
                format!("`{by}` declares `{name}` here, but only for {what}"),
                format!(
                    "the code a macro generates in {what} {}, so the names it declares are visible only there",
                    out.operand.runs()
                ),
                format!(
                    "to use `{name}` here, call `{by}` as a statement of its own before this line, so its code always runs"
                ),
            ),
            None => (
                format!("`{name}` is declared here, but only for {what}"),
                format!("code in {what} {}, so the names it declares are visible only there", out.operand.runs()),
                format!("to use `{name}` here, declare it before {what}"),
            ),
        };
        self.report(
            Diagnostic::error(codes::UNDEFINED_NAME, format!("undefined name `{name}`"))
                .primary(span, "not found in this scope")
                .secondary(out.span, label)
                .note(note)
                .help(help),
        );
        true
    }
}

/// Code that runs apart from the statement around it, not once with it:
/// maybe not at all, never, or on each test of a loop. The statements a
/// macro generates there get a scope of their own (see
/// [`Checker::lower_operand`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum OperandKind {
    /// The right side of `&&` (`or` false) or `||`.
    Logical { or: bool },
    /// The value of `&&=` (`or` false) or `||=`.
    LogicalAssign { or: bool },
    /// The arguments of a `&.` call, which run only for a receiver that
    /// isn't nil.
    SafeCall,
    /// A `when` pattern after the first of a `case`, which runs only when
    /// no earlier pattern matched.
    WhenPattern,
    /// `type_info`'s operand, which is checked but never runs.
    TypeInfo,
    /// A `while` (`until` false) or `until` condition.
    Condition { until: bool },
}

/// An operand that runs apart from the statement around it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Operand {
    pub kind: OperandKind,
    /// The operand's code.
    pub span: Span,
}

impl Operand {
    /// The operand, for messages: "the right side of `&&`".
    fn describe(self) -> &'static str {
        match self.kind {
            OperandKind::Logical { or: false } => "the right side of `&&`",
            OperandKind::Logical { or: true } => "the right side of `||`",
            OperandKind::LogicalAssign { or: false } => "the value of `&&=`",
            OperandKind::LogicalAssign { or: true } => "the value of `||=`",
            OperandKind::SafeCall => "the arguments of a `&.` call",
            OperandKind::WhenPattern => "a `when` pattern after the first",
            OperandKind::TypeInfo => "`type_info`'s operand",
            OperandKind::Condition { until: false } => "a `while` condition",
            OperandKind::Condition { until: true } => "an `until` condition",
        }
    }

    /// When its code runs, for messages: "runs only when the left side is
    /// true".
    fn runs(self) -> &'static str {
        match self.kind {
            OperandKind::Logical { or: false } => "runs only when the left side is true",
            OperandKind::Logical { or: true } => "runs only when the left side is false or nil",
            OperandKind::LogicalAssign { or: false } => "runs only when the target is true or holds a value",
            OperandKind::LogicalAssign { or: true } => "runs only when the target is false or nil",
            OperandKind::SafeCall => "runs only when the receiver isn't nil",
            OperandKind::WhenPattern => "runs only when no earlier pattern matched",
            OperandKind::TypeInfo => "is checked but never runs",
            OperandKind::Condition { .. } => "runs again on each test of the loop",
        }
    }
}

/// A variable that macro code declared in an operand (see [`Operand`]),
/// remembered after the operand's scope ends so that a use after it can
/// say why the name is missing.
#[derive(Clone, Debug)]
pub(crate) struct ScopedOut {
    pub name: Name,
    /// The variable's hygiene mark (see `Var::mark`).
    pub mark: Option<u32>,
    /// Where it was declared.
    pub span: Span,
    /// The operand that declared it.
    pub operand: Operand,
    /// The macro whose code declared it, as the call names it.
    pub by: Option<String>,
}

/// The keyword of an expression that chooses among branches of statements
/// (`if`, `unless`, `case`, `comptime if`), which a statement may write
/// without values.
fn branching_keyword(e: &ast::Expr) -> Option<&'static str> {
    match &e.kind {
        E::If(i) if i.unless => Some("unless"),
        E::If(_) => Some("if"),
        E::Case(_) => Some("case"),
        E::ComptimeIf(_) => Some("comptime if"),
        _ => None,
    }
}

/// The spans of the expressions that end the branches of `e` and give its
/// value, through nested `if`s and `case`s.
fn branch_tails(e: &ast::Expr, out: &mut Vec<Span>) {
    let bodies: Vec<&[ast::Stmt]> = match &e.kind {
        E::If(i) | E::ComptimeIf(i) => std::iter::once(i.then.as_slice())
            .chain(i.elifs.iter().map(|(_, b)| b.as_slice()))
            .chain(i.else_.as_deref())
            .collect(),
        E::Case(c) => c.whens.iter().map(|w| w.body.as_slice()).chain(c.else_.as_deref()).collect(),
        _ => {
            out.push(e.span);
            return;
        }
    };
    for body in bodies {
        if let Some(ast::Stmt { kind: StmtKind::Expr(tail), .. }) = body.last() {
            branch_tails(tail, out);
        }
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
    /// The code spliced in from outside the expansion so far.
    splices: Vec<Splice>,
}

impl Expander<'_> {
    /// The statements a `Code` value stands for, spliced at `at` (`None`
    /// for the code the macro returned).
    fn code(&mut self, value: u64, at: Option<Span>) -> Vec<ast::Stmt> {
        let Some(index) = value.checked_sub(1).map(|i| i as usize) else { return Vec::new() };
        let args = self.args;
        if let Some(stmts) = args.get(index) {
            if let Some(site) = at {
                let spliced = stmts.iter().map(|s| Splice { code: s.span, site, name: None, computed: false });
                self.splices.extend(spliced);
            }
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

    /// Where a name spliced from a `Symbol` at `at` is: in the symbol
    /// argument that named it, or the call.
    fn name_span(&mut self, name: Name, at: Span) -> Span {
        let (span, computed) = match self.ex.symbols.iter().find(|(n, _)| *n == name) {
            Some((_, s)) => {
                let len = name.as_str().len() as u32;
                (if s.len() > len { Span { start: s.end - len, ..*s } } else { *s }, false)
            }
            None => (self.ex.call, true),
        };
        self.ex.splices.push(Splice { code: span, site: at, name: Some(name), computed });
        span
    }

    /// Where a symbol literal spliced from a `Symbol` at `at` is.
    fn literal_span(&mut self, name: Name, at: Span) -> Span {
        let (span, computed) = match self.ex.symbols.iter().find(|(n, _)| *n == name) {
            Some((_, s)) => (*s, false),
            None => (self.ex.call, true),
        };
        self.ex.splices.push(Splice { code: span, site: at, name: Some(name), computed });
        span
    }

    /// The code of a `Code` value as one expression.
    fn code_expr(&mut self, code: u64, at: Span, place: &str) -> Option<ast::Expr> {
        let stmts = self.ex.code(code, Some(at));
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
            SpliceValue::Symbol(n) => return name_expr(n, self.name_span(n, at)),
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
                let stmts = self.ex.code(c, None);
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
                let segment = Ident { name: *n, span: self.name_span(*n, at) };
                return ast::TypeExpr { kind: TypeKind::Path { segments: vec![segment], args: Vec::new() }, span: at };
            }
            SpliceValue::Code(c) => {
                let stmts = self.ex.code(*c, Some(at));
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
                        true => ast::Expr { kind: E::Symbol(n), span: self.literal_span(n, e.span) },
                        false => name_expr(n, self.name_span(n, e.span)),
                    })
                    .collect(),
            ),
            _ => None,
        }
    }

    /// Turns generated statements into declarations, for a splice among
    /// declarations.
    fn push_items(&mut self, stmts: Vec<ast::Stmt>, at: Span, items: &mut Vec<ast::Item>) {
        let mut statements = Vec::new();
        items.extend(lines_to_items(stmts, &mut statements));
        if !statements.is_empty() {
            self.mismatch(
                at,
                "a statement",
                "a declaration",
                "only declarations go here: `def`, `struct`, constants and the like, or macro calls",
            );
        }
    }
}

/// Finds the first `Self` in a macro's own code: outside its `quote`
/// bodies, but inside their splices.
#[derive(Default)]
struct FindSelf {
    found: Option<Span>,
}

impl VisitMut for FindSelf {
    fn visit_expr(&mut self, e: &mut ast::Expr) {
        if self.found.is_some() {
            return;
        }
        match &mut e.kind {
            E::Const(n) if n.as_str() == "Self" => self.found = Some(e.span),
            E::Quote(q) => self.visit_exprs(&mut q.splices),
            _ => walk_expr(self, e),
        }
    }

    fn visit_type(&mut self, ty: &mut ast::TypeExpr) {
        if self.found.is_some() {
            return;
        }
        if let TypeKind::Path { segments, .. } = &ty.kind
            && let [only] = segments.as_slice()
            && only.as_str() == "Self"
        {
            self.found = Some(only.span);
            return;
        }
        walk_type(self, ty);
    }
}

/// Where an expression uses `Self`, if it does.
pub(super) fn self_in(e: &ast::Expr) -> Option<Span> {
    let mut find = FindSelf::default();
    find.visit_expr(&mut e.clone());
    find.found
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
                let at = stmt.span;
                match self.value(*i) {
                    Some(SpliceValue::Code(c)) => {
                        stmts.extend(self.ex.code(c, Some(at)));
                        continue;
                    }
                    Some(SpliceValue::Codes(codes)) => {
                        for c in codes {
                            stmts.extend(self.ex.code(c, Some(at)));
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
                        let stmts = self.ex.code(c, Some(item.span));
                        self.push_items(stmts, item.span, items);
                    }
                    Some(SpliceValue::Codes(codes)) => {
                        for c in codes {
                            let stmts = self.ex.code(c, Some(item.span));
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
                    let span = self.name_span(n, it.span);
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
                            *e = ast::Expr { kind: E::Symbol(name), span: self.literal_span(name, span) }
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
                    (Some(name), _) => *e = name_expr(name, self.name_span(name, span)),
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
            *ident = Ident { name, span: self.name_span(name, ident.span) };
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

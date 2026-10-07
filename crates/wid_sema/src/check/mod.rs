//! The checker: resolves names, checks types and lowers bodies to IR.

mod attrs;
mod body;
mod cimport;
mod collections;
mod comptime;
mod decl_macros;
mod expr;
mod flow;
mod generics;
mod inline;
mod items;
mod macros;
mod matrix;
mod members;
mod overloads;
mod runtime;
mod stmt;
mod structs;
mod tests;
mod ty;
mod type_info;

pub use ty::is_reserved_type_name;

use std::collections::{HashMap, VecDeque};

use wid_diagnostics::{Diagnostic, Diagnostics, FileId, Span, codes, did_you_mean};
use wid_syntax::{Name, ast};

use crate::input::{PackageId, ProgramInput};
use crate::ir::{self, FnId, LocalId};
use crate::types::{Abi, TyId, TypeTable};

pub(crate) use body::Body;

/// Identifies a package-level (or member) declaration.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub(crate) struct DeclId(pub u32);

/// Where a declaration lives.
#[derive(Clone, Copy, Debug)]
pub(crate) struct DeclLoc {
    pub pkg: PackageId,
    /// Index of the file within the package.
    pub file: usize,
}

/// A declaration together with its syntax.
#[derive(Clone, Debug)]
pub(crate) struct Decl<'a> {
    pub name: Name,
    pub span: Span,
    pub loc: DeclLoc,
    pub private: bool,
    pub item: &'a ast::Item,
    pub kind: DeclKind<'a>,
    /// The struct, enum or module that declares this member.
    pub owner: Option<DeclId>,
}

#[derive(Clone, Debug)]
pub(crate) enum DeclKind<'a> {
    Fn(&'a ast::FnDecl),
    Const(&'a ast::ConstDecl),
    Struct(&'a ast::StructDecl),
    Enum(&'a ast::EnumDecl),
    Union(&'a ast::UnionDecl),
    Module,
    Overload(&'a ast::OverloadDecl),
    Extend(&'a ast::ExtendDecl),
}

impl DeclKind<'_> {
    /// The kind of declaration with its article, like "an enum".
    pub fn a_describe(&self) -> String {
        let what = self.describe();
        let article = if what.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
        format!("{article} {what}")
    }

    /// The kind of declaration, like "enum".
    pub fn describe(&self) -> &'static str {
        match self {
            DeclKind::Fn(_) => "method",
            DeclKind::Const(_) => "constant",
            DeclKind::Struct(_) => "struct",
            DeclKind::Enum(_) => "enum",
            DeclKind::Union(_) => "union",
            DeclKind::Module => "module",
            DeclKind::Overload(_) => "overload set",
            DeclKind::Extend(_) => "extension",
        }
    }
}

/// A resolved function signature.
#[derive(Clone, Debug)]
pub(crate) struct FnSig {
    pub params: Vec<ParamSig>,
    pub ret: TyId,
    /// The receiver type for instance methods; `self` is passed as `^Self`.
    pub receiver: Option<TyId>,
    /// Takes C variadic arguments (`...`) after the parameters.
    pub c_variadic: bool,
    /// The `&blk` parameter of methods that take a block.
    pub block: Option<inline::BlockSig>,
}

#[derive(Clone, Debug)]
pub(crate) struct ParamSig {
    pub name: Name,
    pub ty: TyId,
    pub span: Span,
}

/// A function instance waiting to be lowered.
#[derive(Clone, Debug)]
pub(crate) struct PendingFn {
    pub id: FnId,
    pub decl: DeclId,
    pub subst: generics::Subst,
    /// The call that first needed this instance, for error context.
    pub origin: Span,
}

/// A value computed for a constant.
#[derive(Clone, Debug)]
pub(crate) enum ConstState {
    Resolving,
    Failed,
    Done { untyped: Option<items::ConstValue>, typed: ir::Expr },
}

pub(crate) struct Checker<'a> {
    pub input: &'a ProgramInput,
    pub diags: Diagnostics,
    pub types: TypeTable,
    pub decls: Vec<Decl<'a>>,
    /// Package-level names, per package.
    pub pkg_scopes: Vec<HashMap<Name, DeclId>>,
    /// Import aliases visible in each file, keyed by (package, file index).
    pub file_imports: HashMap<(PackageId, usize), HashMap<Name, (PackageId, Span)>>,
    /// Names of imports that failed to load, per file, so their uses are
    /// not reported again as undefined.
    pub failed_imports: std::collections::HashSet<(PackageId, usize, Name)>,
    /// The cimport packages whose declarations joined each package's own
    /// namespace (a `cimport` without `as:`).
    pub merged_cimports: HashMap<PackageId, Vec<PackageId>>,
    /// Packages whose `cimport` without `as:` failed, so names it would have
    /// declared are not reported as undefined.
    pub failed_merges: std::collections::HashSet<PackageId>,
    pub functions: Vec<Option<ir::Function>>,
    pub fn_insts: HashMap<(DeclId, Vec<TyId>), FnId>,
    /// Instances of generic structs, by declaration and arguments.
    pub struct_insts: HashMap<(DeclId, Vec<TyId>), TyId>,
    /// The generic bindings each struct instance was created with.
    pub struct_args: HashMap<crate::types::StructId, generics::Subst>,
    /// Instances of generic unions, by declaration and arguments.
    pub union_insts: HashMap<(DeclId, Vec<TyId>), TyId>,
    /// The declaration and generic bindings of each generic union instance.
    pub union_args: HashMap<crate::types::UnionId, (DeclId, generics::Subst)>,
    /// The modules each struct, enum, module or `extend` includes.
    pub includes: HashMap<DeclId, Vec<DeclId>>,
    /// The `include` declarations of each struct, enum, module or `extend`,
    /// in the order they were collected: written ones, then those that
    /// `comptime if` branches and macros add.
    pub include_items: HashMap<DeclId, Vec<&'a ast::Item>>,
    /// The resolved members of each overload set.
    pub overload_sets: HashMap<DeclId, Vec<DeclId>>,
    /// Every `extend` declaration.
    pub extends: Vec<DeclId>,
    /// Resolved receiver patterns of each `extend`.
    pub extend_patterns: HashMap<DeclId, Vec<TyId>>,
    /// Instance counters for C names.
    pub instance_counts: HashMap<DeclId, usize>,
    pub sigs: HashMap<DeclId, FnSig>,
    pub queue: VecDeque<PendingFn>,
    pub consts: HashMap<DeclId, ConstState>,
    /// Constants being evaluated, innermost last, for cycle reports.
    pub const_stack: Vec<DeclId>,
    pub decl_types: HashMap<DeclId, TyId>,
    /// Methods, constants and overloads declared inside a type or module.
    pub members: HashMap<DeclId, HashMap<Name, DeclId>>,
    /// The declaration that created each struct type, with its field defaults.
    pub struct_decls: HashMap<crate::types::StructId, DeclId>,
    /// The declaration that created each enum type.
    pub enum_decls: HashMap<crate::types::EnumId, DeclId>,
    /// Struct and union declarations currently having their fields or
    /// variants resolved.
    pub resolving: Vec<DeclId>,
    /// Struct and union declarations named only behind a pointer (or slice,
    /// dynamic array, map or proc) so far: their type exists but their
    /// fields are resolved later, so `struct A { b: ^B }` and
    /// `struct B { a: A }` are not mistaken for a cycle.
    pub pending_types: Vec<DeclId>,
    /// How many indirections deep the type being resolved is.
    pub shallow: u32,
    pub errors: Vec<Name>,
    pub body: Body,
    pub source_texts: HashMap<FileId, std::sync::Arc<str>>,
    /// Set while lowering a statement marked `@[no_bounds_check]`.
    pub no_bounds_check: bool,
    /// Blocks that inlined calls `yield` to.
    pub yield_targets: Vec<inline::YieldTarget>,
    /// Block methods currently being inlined, to catch recursion.
    pub inline_stack: Vec<DeclId>,
    /// Recursion cycles among block methods already reported, as sorted
    /// member lists, so each cycle is reported once.
    pub reported_inline_cycles: std::collections::HashSet<Vec<DeclId>>,
    /// Locals of the code around a proc being lowered, which it may not use.
    pub capturable: Vec<Name>,
    /// Number of procs lowered so far, for unique C names.
    pub lambda_count: u32,
    /// Generic instances being lowered, outermost first: the instance's
    /// name, the call that created it, the span of its declaration and, for
    /// a macro that runs for the `Self` of its call, that type as shown.
    pub instance_stack: Vec<(String, Span, Span, Option<String>)>,
    /// Bindings for the next `call_fn`: the receiver type's generic arguments
    /// for type-level calls and the `Self` of extension methods.
    pub owner_bindings: Vec<(Name, TyId)>,
    /// Outer locals a proc tried to use, marked read once it is lowered.
    pub captured: Vec<Name>,
    /// Package-level values: `@[extern]` constants read from C.
    pub globals: Vec<ir::Global>,
    /// The global of each `@[extern]` constant.
    pub extern_globals: HashMap<DeclId, ir::GlobalId>,
    /// The Wid types that `types:` maps C records to, per cimport package,
    /// by the record's Wid name.
    pub c_overrides: HashMap<PackageId, HashMap<Name, TyId>>,
    /// Set while resolving the type a `^` points at, where opaque structs
    /// are allowed.
    pub pointee: bool,
    /// How many `comptime` evaluations are running, innermost last.
    pub comptime_depth: u32,
    /// Whether `capturable` belongs to a proc or to `comptime` code.
    pub capture_kind: comptime::CaptureKind,
    /// While lowering a constant's initializer written without `comptime`,
    /// its span: calling a method there needs `comptime`.
    pub const_init: Option<Span>,
    /// Declaration-level `comptime if`s and macro calls waiting for every
    /// declaration outside them to be known.
    pub pending_decls: Vec<comptime::Pending<'a>>,
    /// Files read by `embed`.
    pub embedded_files: Vec<std::path::PathBuf>,
    /// The global holding each embedded file.
    pub embeds: HashMap<std::path::PathBuf, ir::GlobalId>,
    /// Each source file's display name and text, for `caller_location` at
    /// compile time.
    pub file_positions: HashMap<FileId, (String, std::sync::Arc<str>)>,
    /// Owns the code macros generate, which lives as long as the input's
    /// syntax (see `macros.rs`).
    pub generated: &'a typed_arena::Arena<Vec<ast::Stmt>>,
    /// Macro templates, expansions and their virtual files.
    pub macros: macros::MacroState,
}

/// Checks a whole program and lowers it to IR.
pub fn check_program(input: &ProgramInput) -> (ir::Program, Diagnostics) {
    let generated = typed_arena::Arena::new();
    let mut checker = Checker {
        input,
        diags: Diagnostics::new(),
        types: TypeTable::new(),
        decls: Vec::new(),
        pkg_scopes: vec![HashMap::new(); input.packages.len()],
        file_imports: HashMap::new(),
        failed_imports: Default::default(),
        merged_cimports: HashMap::new(),
        failed_merges: Default::default(),
        functions: Vec::new(),
        fn_insts: HashMap::new(),
        struct_insts: HashMap::new(),
        struct_args: HashMap::new(),
        union_insts: HashMap::new(),
        union_args: HashMap::new(),
        extends: Vec::new(),
        includes: HashMap::new(),
        overload_sets: HashMap::new(),
        extend_patterns: HashMap::new(),
        instance_counts: HashMap::new(),
        sigs: HashMap::new(),
        queue: VecDeque::new(),
        consts: HashMap::new(),
        const_stack: Vec::new(),
        decl_types: HashMap::new(),
        members: HashMap::new(),
        struct_decls: HashMap::new(),
        enum_decls: HashMap::new(),
        resolving: Vec::new(),
        pending_types: Vec::new(),
        shallow: 0,
        errors: Vec::new(),
        body: Body::default(),
        no_bounds_check: false,
        yield_targets: Vec::new(),
        inline_stack: Vec::new(),
        reported_inline_cycles: Default::default(),
        capturable: Vec::new(),
        lambda_count: 0,
        owner_bindings: Vec::new(),
        instance_stack: Vec::new(),
        captured: Vec::new(),
        globals: Vec::new(),
        extern_globals: HashMap::new(),
        c_overrides: HashMap::new(),
        pointee: false,
        comptime_depth: 0,
        capture_kind: Default::default(),
        const_init: None,
        pending_decls: Vec::new(),
        include_items: HashMap::new(),
        embedded_files: Vec::new(),
        embeds: HashMap::new(),
        file_positions: HashMap::new(),
        source_texts: input
            .packages
            .iter()
            .flat_map(|p| p.files.iter().map(|f| (f.ast.file, f.text.clone())))
            .collect(),
        generated: &generated,
        macros: Default::default(),
    };
    checker.init_file_positions();
    checker.init_macro_files();
    checker.collect();
    let main = checker.find_main();
    checker.check_all_roots();
    let (tests, test_runner) = checker.collect_tests();
    checker.drain_queue();
    let mut roots: Vec<FnId> = main.into_iter().chain(tests.iter().map(|t| t.func)).chain(test_runner).collect();
    roots.extend(
        checker
            .functions
            .iter()
            .enumerate()
            .filter(|(_, f)| f.as_ref().is_some_and(|f| f.export.is_some() || f.abi == Abi::C && f.body.is_some()))
            .map(|(i, _)| FnId(i as u32)),
    );
    checker.check_comptime_only(&roots);
    let expansions = checker.expansion_files();
    let functions = checker.functions.into_iter().map(|f| f.expect("every queued function is lowered")).collect();
    let mut c = cimport::CBuild::default();
    for p in &input.packages {
        if let Some(b) = &p.cimport {
            c.add(b);
        }
    }
    let program = ir::Program {
        types: checker.types,
        functions,
        globals: checker.globals,
        main,
        tests,
        test_runner,
        errors: checker.errors,
        c_includes: c.includes,
        c_sources: input.packages.iter().flat_map(|p| p.c_sources.clone()).collect(),
        link_libs: c.link_libs,
        c_flags: c.c_flags,
        link_flags: c.link_flags,
        embedded_files: checker.embedded_files,
        checks: ir::Checks { bounds: input.options.bounds_checks, overflow: input.options.overflow_checks },
        debug: input.options.debug,
        expansions,
    };
    let mut diags = checker.diags;
    diags.sort();
    (program, diags)
}

impl<'a> Checker<'a> {
    pub fn report(&mut self, diag: Diagnostic) {
        let mut diag = diag;
        let within = |p: Span, outer: Span| p.file == outer.file && p.start >= outer.start && p.end <= outer.end;
        if diag.severity == wid_diagnostics::Severity::Error
            && let Some((name, site, _, macro_self)) = self.instance_stack.first().cloned()
            && site != Span::default()
            && let Some(primary) = diag.primary_span()
            && !within(primary, site)
            && self.instance_stack.iter().any(|(_, _, body, _)| within(primary, *body))
        {
            diag = match macro_self {
                Some(shown) => diag
                    .secondary(site, format!("`{name}` runs with `Self` as `{shown}` for this call"))
                    .note(format!("the error is inside the macro `{name}`, checked for each `Self` it runs with")),
                None => diag
                    .secondary(site, format!("`{name}` is checked for these types because of this call"))
                    .note(format!("the error is inside the generic method `{name}`")),
            };
        }
        self.diags.push(diag);
    }

    /// Finds `def main` in the root package.
    fn find_main(&mut self) -> Option<FnId> {
        let name = Name::new("main");
        let Some(&decl) = self.pkg_scopes[0].get(&name) else {
            if !self.input.options.testing {
                let first = self.input.packages[0].files.first();
                let span = first.map(|f| Span::new(f.ast.file, 0, 0)).unwrap_or_default();
                let mut diag = Diagnostic::error(codes::NO_MAIN, "this package has no `def main`")
                    .primary(span, "the program starts at `def main`");
                diag = match first {
                    Some(f) => {
                        let end = f.text.len() as u32;
                        let sep = if f.text.ends_with('\n') || f.text.is_empty() { "\n" } else { "\n\n" };
                        diag.suggest(
                            "add an entry point",
                            vec![wid_diagnostics::Edit {
                                span: Span::new(f.ast.file, end, end),
                                replacement: format!("{sep}def main\n  puts \"hello\"\nend\n"),
                            }],
                            wid_diagnostics::Applicability::HasPlaceholders,
                        )
                    }
                    None => diag.help("add an entry point: `def main … end`"),
                };
                self.report(diag);
            }
            return None;
        };
        match self.decls[decl.0 as usize].kind {
            DeclKind::Fn(f) => {
                if !f.params.is_empty() || f.ret.is_some() {
                    self.report(
                        Diagnostic::error(codes::TYPE_MISMATCH, "`main` takes no parameters and returns nothing")
                            .primary(f.sig_span, "change the signature to `def main`")
                            .help("`import \"core:os\"`, then read arguments with `os.args` and exit with a status using `os.exit(code)`"),
                    );
                }
                Some(self.fn_instance(decl))
            }
            ref other => {
                let span = self.decls[decl.0 as usize].span;
                let what = other.a_describe();
                self.report(
                    Diagnostic::error(codes::NO_MAIN, format!("`main` is {what}, not a method"))
                        .primary(span, "the program starts at `def main`"),
                );
                None
            }
        }
    }

    /// Queues every non-generic function of the root package for checking.
    fn check_all_roots(&mut self) {
        let count = self.decls.len();
        for i in 0..count {
            let decl = &self.decls[i];
            let check = decl.loc.pkg == PackageId(0) || self.input.options.check_all_packages;
            if check && matches!(decl.kind, DeclKind::Fn(f) if f.is_macro) {
                // A macro's body is checked even if nothing calls it, except
                // one that uses `Self`: it is checked at each call, for the
                // call's `Self`, like a generic method.
                let id = DeclId(i as u32);
                if self.check_macro(id) && self.macro_self_use(id).is_none() {
                    self.fn_instance(id);
                }
                continue;
            }
            if check && matches!(decl.kind, DeclKind::Fn(f) if !f.is_macro) {
                self.fn_sig(DeclId(i as u32));
            }
            let decl = &self.decls[i];
            if check && matches!(decl.kind, DeclKind::Fn(f) if !f.is_macro) && self.decl_is_concrete(DeclId(i as u32)) {
                if matches!(decl.kind, DeclKind::Fn(f) if f.block.is_some()) {
                    self.check_block_method(DeclId(i as u32));
                } else {
                    self.fn_instance(DeclId(i as u32));
                }
            }
        }
        for i in 0..count {
            let d = &self.decls[i];
            let check = d.loc.pkg == PackageId(0) || self.input.options.check_all_packages;
            let is_type = matches!(d.kind, DeclKind::Struct(s) if s.generics.is_empty())
                || matches!(d.kind, DeclKind::Union(u) if u.generics.is_empty())
                || matches!(d.kind, DeclKind::Enum(_));
            if check && is_type {
                let span = d.span;
                self.decl_as_type(DeclId(i as u32), span);
            }
            let d = &self.decls[i];
            if check && matches!(d.kind, DeclKind::Overload(_)) {
                self.check_overload_set(DeclId(i as u32));
            }
            let d = &self.decls[i];
            if check
                && matches!(d.kind, DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Module | DeclKind::Extend(_))
            {
                self.includes_of(DeclId(i as u32));
            }
        }
        let consts: Vec<DeclId> = (0..count as u32)
            .map(DeclId)
            .filter(|d| matches!(self.decls[d.0 as usize].kind, DeclKind::Const(_)))
            .collect();
        for c in consts {
            self.const_value(c);
        }
    }

    /// Returns the function id for a non-generic function declaration,
    /// queuing it for lowering the first time.
    pub fn fn_instance(&mut self, decl: DeclId) -> FnId {
        self.fn_instance_with(decl, Default::default(), Span::default())
    }

    fn drain_queue(&mut self) {
        while let Some(pending) = self.queue.pop_front() {
            self.lower_pending(pending);
        }
    }

    /// Lowers a queued function, remembering whether its body had errors
    /// (a macro with errors never runs).
    pub(crate) fn lower_pending(&mut self, pending: PendingFn) {
        let errors = self.diags.error_count();
        let func = self.lower_function(pending.decl, pending.subst, pending.origin);
        if self.diags.error_count() > errors {
            self.macros.failed.insert(pending.id);
        }
        self.functions[pending.id.0 as usize] = Some(func);
    }

    /// Returns names visible at package level for "did you mean" hints.
    pub fn package_names(&self, pkg: PackageId) -> Vec<&'static str> {
        self.pkg_scopes[pkg.0 as usize].keys().map(|n| n.as_str()).collect()
    }

    /// Reports an undefined name with suggestions.
    pub fn undefined(&mut self, name: Name, span: Span, candidates: Vec<&'static str>, what: &str) {
        if !self.body.frames.is_empty() {
            // Where the name resolves: for code a macro generated, the
            // macro's file.
            let loc = self.loc_at(span);
            if self.import_failed(loc, name) || self.failed_merges.contains(&loc.pkg) {
                return;
            }
            if self.merged_cimports.contains_key(&loc.pkg) && self.report_not_imported(loc.pkg, name, span) {
                return;
            }
        }
        let text = name.as_str();
        let mut diag = Diagnostic::error(codes::UNDEFINED_NAME, format!("undefined {what} `{text}`"))
            .primary(span, "not found in this scope".to_string());
        if let Some(best) = did_you_mean(text, candidates.iter().copied()) {
            diag = diag.suggest_replace(
                format!("a similar name exists: `{best}`"),
                span,
                best,
                wid_diagnostics::Applicability::MaybeIncorrect,
            );
        }
        self.report(diag);
    }

    /// Whether `name` is an import of this file that failed to load; its
    /// uses were already explained by the import error.
    pub fn import_failed(&self, loc: DeclLoc, name: Name) -> bool {
        self.failed_imports.contains(&(loc.pkg, loc.file, name))
    }

    /// Creates the display/C name prefix for a package.
    pub fn pkg_prefix(&self, pkg: PackageId) -> String {
        mangle_ident(&self.input.packages[pkg.0 as usize].name)
    }

    pub fn new_function_shell(&self, display: String, c_name: String, ret: TyId, span: Span) -> ir::Function {
        ir::Function {
            display,
            c_name,
            params: Vec::new(),
            ret,
            locals: Vec::new(),
            body: None,
            abi: Abi::Wid,
            export: None,
            foreign: None,
            c_call: None,
            c_variadic: false,
            span,
            comptime_only: false,
        }
    }

    pub fn local_ty(&self, local: LocalId) -> TyId {
        self.body.locals[local.0 as usize].ty
    }
}

/// Returns the C-safe name of an operator method, or `None` for plain names.
pub(crate) fn operator_name(name: &str, unary: bool) -> Option<&'static str> {
    Some(match name {
        "+" => "op_add",
        "-" if unary => "op_neg",
        "-" => "op_sub",
        "*" => "op_mul",
        "/" => "op_div",
        "%" => "op_rem",
        "**" => "op_pow",
        "==" => "op_eq",
        "!=" => "op_ne",
        "<" => "op_lt",
        "<=" => "op_le",
        ">" => "op_gt",
        ">=" => "op_ge",
        "<=>" => "op_cmp",
        "[]" => "op_index",
        "[]=" => "op_index_set",
        "<<" => "op_shl",
        ">>" => "op_shr",
        "&" => "op_bit_and",
        "|" => "op_bit_or",
        "~" if unary => "op_bit_not",
        "~" => "op_bit_xor",
        "!" => "op_not",
        _ => return None,
    })
}

/// Turns arbitrary text into a valid C identifier fragment.
pub(crate) fn mangle_ident(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '_' => out.push(ch),
            '?' => out.push_str("_p"),
            '!' => out.push_str("_b"),
            '+' => out.push_str("op_add"),
            '-' => out.push_str("op_sub"),
            '*' => out.push_str("op_mul"),
            '/' => out.push_str("op_div"),
            '%' => out.push_str("op_rem"),
            '=' => out.push_str("op_eq"),
            '<' => out.push_str("op_lt"),
            '>' => out.push_str("op_gt"),
            '[' => out.push_str("op_idx"),
            ']' => {}
            '&' => out.push_str("op_and"),
            '|' => out.push_str("op_or"),
            '~' => out.push_str("op_tilde"),
            _ => out.push_str(&format!("_u{:x}", ch as u32)),
        }
    }
    out
}

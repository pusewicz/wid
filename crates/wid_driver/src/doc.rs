//! `wid doc`: documentation for a package, a type, a method or a C symbol,
//! generated from doc comments.
//!
//! The command loads and checks the package, never generating code, and
//! takes the [`Index`] the checker builds of its declarations. It reads its
//! arguments (see [`DocRequest`]), resolves the symbol path with
//! [`Index::resolve`] and builds a [`Page`], plain data that
//! [`render_text`] and [`render_json`] print. Errors in the package are
//! reported as usual, and the page still documents every declaration the
//! checker collected.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use wid_diagnostics::{Applicability, Diagnostic, Diagnostics, Edit, FileId, SourceMap, Span, codes, did_you_mean};
use wid_sema::PackageId;
use wid_sema::index::{CDoc, Index, MemberGroup, Origin, PathError, PathErrorKind, SymbolId, SymbolKind, Target};
use wid_syntax::docs::first_paragraph;

use crate::{Options, find_wid_root, load_program};

/// What `wid doc` was asked for.
#[derive(Clone, Debug, Default)]
pub struct DocRequest {
    /// The positional arguments as written: none, a package, a symbol, or
    /// a package and a symbol. One argument is the package when it names an
    /// existing directory or file or contains `:` (`core:fmt`), and a
    /// symbol of the package in `.` otherwise.
    pub args: Vec<String>,
    /// Document private declarations too (`-private`).
    pub private: bool,
    /// The directory relative paths are read from, and the package in `.`
    /// is: the current directory when empty.
    pub dir: PathBuf,
}

/// The result of `wid doc`.
pub struct DocOutput {
    /// Every source file read, and the command line.
    pub sources: SourceMap,
    /// The package's diagnostics, and why the request failed if it did.
    pub diags: Diagnostics,
    /// The documentation, unless the request failed.
    pub page: Option<Page>,
}

/// A documentation page: a package overview, or one symbol.
#[derive(Clone, Debug)]
pub struct Page {
    /// The package the page is about (for a symbol, the package it was
    /// looked up in).
    pub package: PackageInfo,
    /// The symbol path asked for; `None` for a package overview.
    pub symbol: Option<String>,
    /// For an overview, every declaration shown; for a symbol, the symbol.
    pub items: Vec<Entry>,
}

/// The package of a page.
#[derive(Clone, Debug)]
pub struct PackageInfo {
    /// The name: `fmt`.
    pub name: String,
    /// The import path: `core:fmt`, `.`, `cimport:raylib.h`.
    pub path: String,
    /// The package doc.
    pub doc: Option<String>,
}

/// A position in a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// The file, as diagnostics show it.
    pub file: String,
    /// The 1-based line.
    pub line: u32,
    /// The 1-based column.
    pub column: u32,
}

/// Where a member listed on a type's page comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginInfo {
    /// `own`, `include`, `extend` or `using`.
    pub kind: &'static str,
    /// What brings the member in, as written: `include Greeter`,
    /// `extend Ball`, `using base: Entity`; empty for the type's own.
    pub via: String,
    /// Where that is declared.
    pub location: Option<Location>,
}

/// One documented declaration, and what is listed under it.
#[derive(Clone, Debug)]
pub struct Entry {
    /// `constant`, `type_alias`, `struct`, `enum`, `union`, `module`,
    /// `method`, `macro`, `overload`, `extension`, `field`, `enum_member`
    /// or `builtin_type`.
    pub kind: &'static str,
    /// The name.
    pub name: String,
    /// The symbol path that documents it: `Ball.update`.
    pub path: String,
    /// The import path of the package that declares it.
    pub package: String,
    /// The declaration line: `def update(dt: F32)`, `vel: Vec2 = [1.0, 0.0]`.
    pub signature: String,
    /// The attributes written before it: `extern("InitWindow")`.
    pub attributes: Vec<String>,
    /// The doc comment.
    pub doc: Option<String>,
    /// Whether it was declared `private`.
    pub private: bool,
    /// `def self.name`.
    pub is_static: bool,
    /// Where it is declared; `None` for C declarations and builtin types.
    pub location: Option<Location>,
    /// The type, module or extension that declares it: its kind (`struct`)
    /// and path (`Ball`) or declaration line (`extend String`).
    pub owner: Option<(&'static str, String)>,
    /// For a C declaration, its C name and header.
    pub c: Option<CDoc>,
    /// For a member listed on a type's page, where it comes from.
    pub origin: Option<OriginInfo>,
    /// For a field asked for through a struct that `using` promotes it
    /// into, that struct (`Player` for `Player.hp`).
    pub promoted_into: Option<String>,
    /// A struct's fields, its own and promoted ones.
    pub fields: Vec<Entry>,
    /// An enum's members.
    pub members: Vec<Entry>,
    /// A union's variant types.
    pub variants: Vec<String>,
    /// An extension's target types.
    pub targets: Vec<String>,
    /// The type a type alias stands for, as written.
    pub aliases: Option<String>,
    /// Methods, type-level constants and overload sets, each with its
    /// origin; for an overload set, its members.
    pub methods: Vec<Entry>,
}

/// The command line as messages show it, with the span of each argument.
struct CommandLine {
    text: String,
    args: Vec<(u32, u32)>,
}

impl CommandLine {
    fn new(args: &[String], file_mode: bool) -> Self {
        let mut text = "wid doc".to_string();
        let mut ranges = Vec::new();
        for arg in args {
            text.push(' ');
            let start = text.len() as u32;
            text.push_str(arg);
            ranges.push((start, text.len() as u32));
        }
        if file_mode {
            text.push_str(" -file");
        }
        CommandLine { text, args: ranges }
    }

    fn add(&self, sources: &mut SourceMap) -> FileId {
        sources.add(PathBuf::from("<command line>"), "command line".to_string(), self.text.clone())
    }

    fn arg(&self, file: FileId, i: usize) -> Span {
        let (start, end) = self.args.get(i).copied().unwrap_or((0, 0));
        Span::new(file, start, end)
    }

    fn end(&self, file: FileId) -> Span {
        let end = self.text.len() as u32;
        Span::new(file, end, end)
    }
}

/// How the arguments were read.
struct Reading {
    /// The package argument, by index.
    package: Option<usize>,
    /// The symbol argument, by index.
    symbol: Option<usize>,
}

/// Runs `wid doc`. `opts.target` and `opts.file_mode` are ignored except
/// for `-file`: the package comes from the request.
pub fn doc(opts: &Options, request: &DocRequest) -> DocOutput {
    let cmd = CommandLine::new(&request.args, opts.file_mode);
    let args = &request.args;
    let dir = if request.dir.as_os_str().is_empty() { Path::new(".") } else { request.dir.as_path() };
    let reading = match args.len() {
        0 => Reading { package: None, symbol: None },
        1 if opts.file_mode || names_package(dir, &args[0]) => Reading { package: Some(0), symbol: None },
        1 => Reading { package: None, symbol: Some(0) },
        _ => Reading { package: Some(0), symbol: Some(1) },
    };
    let target = match package_target(opts, dir, &cmd, &reading, args) {
        Ok(target) => target,
        Err(diag) => {
            let mut sources = SourceMap::new();
            let file = cmd.add(&mut sources);
            let mut diags = Diagnostics::new();
            diags.push(diag(file));
            return DocOutput { sources, diags, page: None };
        }
    };
    let mut opts = opts.clone();
    opts.target = target;
    opts.library = true;
    opts.testing = false;
    opts.check_all_packages = false;
    let (mut sources, input, mut diags) = load_program(&opts);
    let file = cmd.add(&mut sources);
    let Some(input) = input else {
        diags.sort();
        return DocOutput { sources, diags, page: None };
    };
    let (mut program, sema_diags, index) = wid_sema::check_program_indexed(&input);
    sources.set_expansions(std::mem::take(&mut program.expansions));
    diags.extend(sema_diags);
    let root = PackageId(0);
    let builder = PageBuilder { index: &index, sources: &sources, private: request.private };
    let page = match reading.symbol {
        None => Some(builder.package_page(root)),
        Some(i) => {
            let symbol = &args[i];
            match resolve_symbol(&index, root, symbol, request.private) {
                Ok(target) => Some(builder.symbol_page(root, symbol, &target)),
                Err(failure) => {
                    let ctx = ErrorContext {
                        index: &index,
                        cmd: &cmd,
                        file,
                        arg: i,
                        symbol,
                        read_as_symbol: reading.package.is_none() && args.len() == 1,
                        package_arg: reading.package.map(|p| args[p].as_str()),
                    };
                    diags.push(ctx.report(failure));
                    None
                }
            }
        }
    };
    diags.sort();
    DocOutput { sources, diags, page }
}

/// What `wid doc` prints.
pub struct Printed {
    /// The page: text, or JSON with `-json`.
    pub stdout: String,
    /// The diagnostics: rendered, or JSON with `-json-errors`.
    pub stderr: String,
    /// Whether the page was shown and the package had no errors.
    pub success: bool,
}

/// Renders a result the way `wid doc` prints it: the page on stdout (as
/// JSON with `json`) and the diagnostics on stderr (as JSON with
/// `json_errors`).
pub fn print(out: &DocOutput, json: bool, json_errors: bool, color: bool) -> Printed {
    let stderr = if json_errors {
        wid_diagnostics::render_json(&out.diags, &out.sources) + "\n"
    } else if out.diags.is_empty() {
        String::new()
    } else {
        wid_diagnostics::render_all(&out.diags, &out.sources, wid_diagnostics::RenderOptions { color })
    };
    let stdout = match &out.page {
        Some(page) if json => render_json(page) + "\n",
        Some(page) => render_text(page),
        None => String::new(),
    };
    Printed { stdout, stderr, success: out.page.is_some() && !out.diags.has_errors() }
}

/// Whether a lone argument names a package: an existing directory or file,
/// or a collection path such as `core:fmt`.
fn names_package(dir: &Path, arg: &str) -> bool {
    arg.contains(':') || dir.join(arg).exists()
}

/// A diagnostic waiting for the command line's file id.
type Pending = Box<dyn FnOnce(FileId) -> Diagnostic>;

/// The directory (or file, with `-file`) to document.
fn package_target(
    opts: &Options,
    dir: &Path,
    cmd: &CommandLine,
    reading: &Reading,
    args: &[String],
) -> Result<PathBuf, Pending> {
    let Some(i) = reading.package else {
        if has_wid_files(dir) {
            return Ok(dir.to_path_buf());
        }
        let symbol = reading.symbol.map(|s| args[s].clone());
        return Err(Box::new(move |file| no_package_here(file, symbol.as_deref())));
    };
    let text = args[i].clone();
    let span_at = cmd.args[i];
    let span = move |file| Span::new(file, span_at.0, span_at.1);
    let end = cmd.text.len() as u32;
    // An existing path wins over a collection, for `C:\…` on Windows.
    if let Some((collection, rel)) = text.split_once(':').filter(|_| !dir.join(&text).exists()) {
        let root = find_wid_root(opts);
        let base = match collection {
            "core" | "vendor" => root.join(collection),
            other => match opts.collections.get(other) {
                Some(dir) => dir.clone(),
                None => {
                    let mut known = vec!["core".to_string(), "vendor".to_string()];
                    let mut extra: Vec<String> = opts.collections.keys().cloned().collect();
                    extra.sort();
                    known.extend(extra);
                    let collection = collection.to_string();
                    let best = did_you_mean(&collection, known.iter().map(String::as_str)).map(str::to_string);
                    return Err(Box::new(move |file| {
                        let span = Span::new(file, span_at.0, span_at.0 + collection.len() as u32);
                        let mut diag =
                            Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, format!("unknown collection `{collection}`"))
                                .primary(span, "no collection with this name")
                                .note(format!("known collections: {}", known.join(", ")));
                        diag = match best {
                            Some(best) => diag.suggest_replace(
                                format!("a similar collection exists: `{best}`"),
                                span,
                                best,
                                Applicability::MaybeIncorrect,
                            ),
                            None => diag.help(format!("define it with `-collection:{collection}=path/to/dir`")),
                        };
                        diag
                    }));
                }
            },
        };
        let dir = base.join(rel);
        if !rel.is_empty() && has_wid_files(&dir) {
            return Ok(dir);
        }
        let packages: Vec<String> =
            collection_packages(&base).into_iter().map(|p| format!("{collection}:{p}")).collect();
        let best = did_you_mean(&text, packages.iter().map(String::as_str)).map(str::to_string);
        return Err(Box::new(move |file| {
            let span = span(file);
            let mut diag = Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, format!("package `{text}` not found"))
                .primary(span, "no package with this path");
            if let Some(best) = best {
                diag = diag.suggest_replace(
                    format!("a similar package exists: `{best}`"),
                    span,
                    best,
                    Applicability::MaybeIncorrect,
                );
            }
            if !packages.is_empty() {
                diag = diag.note(format!("the packages there are {}", packages.join(", ")));
            }
            diag
        }));
    }
    let path = clean(&dir.join(&text));
    if opts.file_mode {
        if path.is_file() {
            return Ok(path);
        }
        return Err(Box::new(move |file| {
            Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, format!("`{text}` is not a file"))
                .primary(span(file), "`-file` documents a single `.wid` file")
                .help("name an existing file, or drop `-file` to document a package directory")
        }));
    }
    if path.is_file() {
        return Err(Box::new(move |file| {
            Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, format!("`{text}` is a file, not a package directory"))
                .primary(span(file), "a package is a directory of `.wid` files")
                .suggest(
                    "document the file on its own with `-file`",
                    vec![Edit { span: Span::new(file, end, end), replacement: " -file".to_string() }],
                    Applicability::MachineApplicable,
                )
        }));
    }
    if has_wid_files(&path) {
        return Ok(path);
    }
    let exists = path.is_dir();
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(dir).to_path_buf();
    let last = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let siblings: Vec<String> = std::fs::read_dir(&parent)
        .map(|rd| {
            rd.flatten()
                .filter(|e| has_wid_files(&e.path()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    let best = match text.strip_suffix(last.as_str()) {
        Some(prefix) if !exists => {
            did_you_mean(&last, siblings.iter().map(String::as_str)).map(|best| format!("{prefix}{best}"))
        }
        _ => None,
    };
    Err(Box::new(move |file| {
        let span = span(file);
        let (message, label) = if exists {
            (format!("`{text}` has no `.wid` files"), "this directory holds no package")
        } else {
            (format!("package `{text}` not found"), "no directory with this path")
        };
        let mut diag = Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, message).primary(span, label);
        match best {
            Some(best) => {
                diag = diag.suggest_replace(
                    format!("a similar package exists: `{best}`"),
                    span,
                    best,
                    Applicability::MaybeIncorrect,
                )
            }
            None => {
                diag = diag
                    .help("name a package directory, a `.wid` file with `-file`, or a collection path like `core:fmt`")
            }
        }
        diag
    }))
}

/// The error for a symbol asked for outside any package.
fn no_package_here(file: FileId, symbol: Option<&str>) -> Diagnostic {
    let span = Span::new(file, 0, 7);
    let mut diag = Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, "there is no package in the current directory")
        .primary(span, "`wid doc` documents the package in `.` when no package is named");
    if let Some(symbol) = symbol {
        diag = diag.note(format!(
            "`{symbol}` is not a directory, a file or a collection path (like `core:fmt`), so it was read as a symbol of the package in `.`"
        ));
        diag = diag.help(format!("name the package first, like `wid doc path/to/package {symbol}`"));
    } else {
        diag = diag.help("run `wid doc` in a package directory, or name a package, like `wid doc core:fmt`");
    }
    diag
}

/// `dir/./x` without the `.`, so paths print as written.
fn clean(path: &Path) -> PathBuf {
    let out: PathBuf = path.components().filter(|c| !matches!(c, std::path::Component::CurDir)).collect();
    if out.as_os_str().is_empty() { PathBuf::from(".") } else { out }
}

/// Whether a directory holds `.wid` files.
fn has_wid_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| rd.flatten().any(|e| e.path().extension().is_some_and(|x| x == "wid")))
}

/// The packages of a collection, as paths relative to it (`stb/image`),
/// sorted.
fn collection_packages(base: &Path) -> Vec<String> {
    fn walk(dir: &Path, rel: &str, depth: u32, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if has_wid_files(&path) {
                out.push(rel.clone());
            }
            if depth < 3 {
                walk(&path, &rel, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(base, "", 0, &mut out);
    out.sort();
    out
}

/// Why a symbol path failed: a malformed path or a resolution error.
enum Failure {
    /// A segment is empty (`Ball.`, `.x`).
    Malformed,
    Path(PathError),
}

fn resolve_symbol(index: &Index, root: PackageId, symbol: &str, private: bool) -> Result<Target, Failure> {
    let segments: Vec<&str> = symbol.split('.').collect();
    if segments.iter().any(|s| s.is_empty()) {
        return Err(Failure::Malformed);
    }
    index.resolve(root, &segments, private).map_err(Failure::Path)
}

/// What error reports need about the request.
struct ErrorContext<'a> {
    index: &'a Index,
    cmd: &'a CommandLine,
    file: FileId,
    arg: usize,
    symbol: &'a str,
    /// The only argument was read as a symbol of the package in `.`.
    read_as_symbol: bool,
    package_arg: Option<&'a str>,
}

impl ErrorContext<'_> {
    /// The span of segment `i` of the symbol path.
    fn segment(&self, i: usize) -> Span {
        let base = self.cmd.arg(self.file, self.arg);
        let mut start = base.start;
        for (n, seg) in self.symbol.split('.').enumerate() {
            if n == i {
                return Span::new(self.file, start, start + seg.len() as u32);
            }
            start += seg.len() as u32 + 1;
        }
        base
    }

    /// The command that documents the package, for hints.
    fn package_command(&self) -> String {
        match self.package_arg {
            Some(p) => format!("wid doc {p}"),
            None => "wid doc".to_string(),
        }
    }

    fn report(&self, failure: Failure) -> Diagnostic {
        let error = match failure {
            Failure::Malformed => {
                let (message, label) = if self.symbol.is_empty() {
                    ("the symbol path is empty".to_string(), "a name goes here")
                } else {
                    (format!("`{}` is not a symbol path", self.symbol), "a name is missing around a `.`")
                };
                return Diagnostic::error(codes::DOC_UNKNOWN_SYMBOL, message)
                    .primary(self.cmd.arg(self.file, self.arg), label)
                    .help("write a name, a type and its member, or an import name first: `Ball`, `Ball.update`, `rl.draw_circle_v`");
            }
            Failure::Path(error) => error,
        };
        let span = self.segment(error.segment);
        let name = self.symbol.split('.').nth(error.segment).unwrap_or_default();
        match error.kind {
            PathErrorKind::UnknownName { package, via, candidates } => {
                self.unknown_name(span, name, package, via, &candidates)
            }
            PathErrorKind::NoMember { owner, candidates } => self.no_member(span, name, &owner, &candidates),
            PathErrorKind::NoMembers { target } => {
                let before = self.symbol.split('.').take(error.segment).collect::<Vec<_>>().join(".");
                let what = self.describe(&target);
                let dot = Span::new(self.file, span.start.saturating_sub(1), self.cmd.arg(self.file, self.arg).end);
                Diagnostic::error(codes::DOC_NO_MEMBER, format!("`{before}` has no members"))
                    .primary(span, format!("`{before}` is {what}, so nothing can follow it"))
                    .note("only structs, enums, unions, modules and the type aliases of them have members")
                    .suggest(
                        format!("document `{before}` itself"),
                        vec![Edit { span: dot, replacement: String::new() }],
                        Applicability::MaybeIncorrect,
                    )
            }
            PathErrorKind::Private { symbol } => {
                let s = self.index.symbol(symbol);
                let scope = if s.owner.is_some() { "its type" } else { "its package" };
                let mut diag =
                    Diagnostic::error(codes::DOC_PRIVATE, format!("`{}` is private", self.index.path_of(symbol)))
                        .primary(span, "private declarations are left out of the documentation");
                if s.span != Span::default() {
                    diag = diag.secondary(s.span, format!("declared `private` here, which hides it outside {scope}"));
                }
                diag.note("`wid doc` documents what code outside can use; `-private` shows the rest too").suggest(
                    "document private declarations too",
                    vec![Edit { span: self.cmd.end(self.file), replacement: " -private".to_string() }],
                    Applicability::MachineApplicable,
                )
            }
        }
    }

    fn unknown_name(
        &self,
        span: Span,
        name: &str,
        package: PackageId,
        via: Option<String>,
        candidates: &[String],
    ) -> Diagnostic {
        let pkg = self.index.package(package);
        let shown = match &via {
            Some(alias) => format!("package `{}` (imported as `{alias}`)", pkg.name),
            None if pkg.path == "." => "this package".to_string(),
            None => format!("package `{}`", pkg.path),
        };
        let mut diag = Diagnostic::error(codes::DOC_UNKNOWN_SYMBOL, format!("no symbol `{name}` in {shown}"))
            .primary(span, format!("not declared in {shown}"));
        if self.read_as_symbol {
            diag = diag.note(format!(
                "`{}` is not a directory, a file or a collection path (like `core:fmt`), so it was read as a symbol of the package in `.`",
                self.symbol
            ));
        }
        let best = did_you_mean(name, candidates.iter().map(String::as_str));
        let mut suggested = best.is_some();
        if let Some(best) = best {
            diag = diag.suggest_replace(
                format!("a similar name exists: `{best}`"),
                span,
                best.to_string(),
                Applicability::MaybeIncorrect,
            );
        } else if via.is_none() {
            // A member of a type in the package, named without its type.
            let owners: Vec<String> = pkg
                .items
                .iter()
                .filter(|&&id| self.index.is_public(id))
                .flat_map(|&id| {
                    let s = self.index.symbol(id);
                    let mut found: Vec<String> = s
                        .members
                        .iter()
                        .filter(|&&m| self.index.is_public(m) && self.index.symbol(m).name == name)
                        .map(|&m| self.index.path_of(m))
                        .collect();
                    found.extend(s.fields.iter().filter(|f| f.name == name).map(|_| format!("{}.{name}", s.name)));
                    found.extend(
                        s.enum_members.iter().filter(|m| m.name == name).map(|m| format!("{}.{}", s.name, m.name)),
                    );
                    found
                })
                .collect();
            match owners.as_slice() {
                [only] => {
                    suggested = true;
                    diag = diag.suggest_replace(
                        format!("`{only}` has that name; name its type first"),
                        span,
                        only.clone(),
                        Applicability::MaybeIncorrect,
                    );
                }
                [] => {}
                many => diag = diag.note(format!("members with that name: {}", many.join(", "))),
            }
        }
        if !suggested {
            diag = diag.help(match &via {
                Some(alias) => format!("`wid doc {alias}` lists what package `{}` declares", pkg.name),
                None => format!("`{}` lists what the package declares", self.package_command()),
            });
        }
        diag
    }

    fn no_member(&self, span: Span, name: &str, owner: &Target, candidates: &[String]) -> Diagnostic {
        let (shown, decl) = match owner {
            Target::Symbol(id) => {
                let s = self.index.symbol(*id);
                (format!("{} `{}`", kind_words(s.kind), self.index.path_of(*id)), Some(s.span))
            }
            Target::Builtin(ty) => (format!("builtin type `{ty}`"), None),
            other => (self.describe(other), None),
        };
        let mut diag = Diagnostic::error(codes::DOC_NO_MEMBER, format!("{shown} has no member `{name}`"))
            .primary(span, format!("not a field, member or method of {shown}"));
        if let Some(decl) = decl.filter(|s| *s != Span::default()) {
            diag = diag.secondary(decl, format!("{shown} is declared here"));
        }
        if let Target::Builtin(ty) = owner {
            diag = diag.note(format!(
                "for a builtin type, `wid doc` lists the methods that extensions (`extend {ty}`) add to it; its builtin methods are described in SPEC.md"
            ));
        }
        match did_you_mean(name, candidates.iter().map(String::as_str)) {
            Some(best) => {
                diag = diag.suggest_replace(
                    format!("a similar member exists: `{best}`"),
                    span,
                    best.to_string(),
                    Applicability::MaybeIncorrect,
                )
            }
            None if candidates.is_empty() => {}
            None => diag = diag.note(format!("its members are {}", list_names(candidates))),
        }
        diag
    }

    /// What a target is, in prose.
    fn describe(&self, target: &Target) -> String {
        match target {
            Target::Symbol(id) => self.index.symbol(*id).kind.a_describe().to_string(),
            Target::Field { .. } => "a field".to_string(),
            Target::EnumMember { .. } => "an enum member".to_string(),
            Target::Package(_) => "a package".to_string(),
            Target::Builtin(_) => "a builtin type".to_string(),
        }
    }
}

/// Names for a message, at most 30 of them.
fn list_names(names: &[String]) -> String {
    let shown: Vec<String> = names.iter().take(30).map(|n| format!("`{n}`")).collect();
    let mut text = shown.join(", ");
    if names.len() > 30 {
        text.push_str(&format!(" and {} more", names.len() - 30));
    }
    text
}

/// A symbol kind as a page names it: `struct`, `type alias`.
fn kind_words(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Constant => "constant",
        SymbolKind::TypeAlias => "type alias",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Union => "union",
        SymbolKind::Module => "module",
        SymbolKind::Method => "method",
        SymbolKind::Macro => "macro",
        SymbolKind::Overload => "overload set",
        SymbolKind::Extension => "extension",
    }
}

/// Builds pages from the index.
struct PageBuilder<'i> {
    index: &'i Index,
    sources: &'i SourceMap,
    private: bool,
}

impl PageBuilder<'_> {
    fn shown(&self, id: SymbolId) -> bool {
        self.private || self.index.is_public(id)
    }

    fn package_info(&self, pkg: PackageId) -> PackageInfo {
        let p = self.index.package(pkg);
        PackageInfo { name: p.name.clone(), path: p.path.clone(), doc: p.doc.clone() }
    }

    fn location(&self, span: Span) -> Option<Location> {
        if span == Span::default() {
            return None;
        }
        let file = self.sources.file(span.file);
        let (line, column) = file.line_col(span.start);
        Some(Location { file: file.display.clone(), line, column })
    }

    /// The overview of a package: every declaration shown, with the
    /// members of its types.
    fn package_page(&self, pkg: PackageId) -> Page {
        let items = self
            .index
            .package(pkg)
            .items
            .iter()
            .filter(|&&id| self.shown(id))
            .map(|&id| self.overview_entry(id))
            .collect();
        Page { package: self.package_info(pkg), symbol: None, items }
    }

    /// The page for what a symbol path named in `pkg`.
    fn symbol_page(&self, pkg: PackageId, path: &str, target: &Target) -> Page {
        let item = match target {
            Target::Package(p) => return self.package_page(*p),
            Target::Symbol(id) => self.full_entry(*id),
            Target::Field { owner, index, promoted } => {
                let origin = promoted.as_ref().map(|(_, path)| OriginInfo {
                    kind: "using",
                    via: format!("using {path}: {}", self.index.symbol(*owner).name),
                    location: None,
                });
                let mut e = self.field_entry(*owner, *index, origin);
                if let Some((into, _)) = promoted {
                    e.promoted_into = Some(self.index.path_of(*into));
                }
                e
            }
            Target::EnumMember { owner, index } => self.member_entry(*owner, *index),
            Target::Builtin(name) => Entry {
                methods: self.group_entries(&self.index.builtin_groups(name)),
                ..blank_entry("builtin_type", name, name, name)
            },
        };
        Page { package: self.package_info(pkg), symbol: Some(path.to_string()), items: vec![item] }
    }

    /// The entry of a symbol, without what is listed under it.
    fn entry(&self, id: SymbolId) -> Entry {
        let s = self.index.symbol(id);
        let owner = s.owner.map(|o| {
            let os = self.index.symbol(o);
            let kind = match os.kind {
                SymbolKind::Extension => "extension",
                k => kind_words(k),
            };
            let shown = if os.kind == SymbolKind::Extension { os.signature.clone() } else { self.index.path_of(o) };
            (kind, shown)
        });
        Entry {
            kind: s.kind.as_str(),
            name: s.name.clone(),
            path: self.index.path_of(id),
            package: self.index.package(s.package).path.clone(),
            signature: s.signature.clone(),
            attributes: s.attributes.clone(),
            doc: s.doc.clone(),
            private: s.private,
            is_static: s.is_static,
            location: if s.c.is_some() { None } else { self.location(s.span) },
            owner,
            c: s.c.clone(),
            origin: None,
            promoted_into: None,
            fields: Vec::new(),
            members: Vec::new(),
            variants: Vec::new(),
            targets: Vec::new(),
            aliases: None,
            methods: Vec::new(),
        }
    }

    /// A symbol as the overview lists it: types with their own fields,
    /// members and methods.
    fn overview_entry(&self, id: SymbolId) -> Entry {
        let s = self.index.symbol(id);
        let mut e = self.entry(id);
        match s.kind {
            SymbolKind::Struct => {
                e.fields = (0..s.fields.len()).map(|i| self.field_entry(id, i, Some(own()))).collect();
            }
            SymbolKind::Enum => e.members = (0..s.enum_members.len()).map(|i| self.member_entry(id, i)).collect(),
            SymbolKind::Union => e.variants = s.links.iter().map(|l| l.text.clone()).collect(),
            SymbolKind::Extension => e.targets = s.links.iter().map(|l| l.text.clone()).collect(),
            SymbolKind::TypeAlias => e.aliases = s.links.first().map(|l| l.text.clone()),
            _ => {}
        }
        if matches!(s.kind, SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Module | SymbolKind::Extension) {
            let own = MemberGroup { origin: Origin::Own, members: s.members.clone() };
            e.methods = self.group_entries(&[own]);
        }
        e
    }

    /// A symbol with everything its page lists.
    fn full_entry(&self, id: SymbolId) -> Entry {
        let s = self.index.symbol(id);
        let mut e = self.entry(id);
        match s.kind {
            SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Union | SymbolKind::Module => {
                self.add_members(&mut e, id)
            }
            SymbolKind::TypeAlias => {
                e.aliases = s.links.first().map(|l| l.text.clone());
                let target = self.index.alias_target(id);
                if target != id && self.index.symbol(target).kind.has_members() {
                    self.add_members(&mut e, target);
                }
            }
            SymbolKind::Extension => {
                e.targets = s.links.iter().map(|l| l.text.clone()).collect();
                let own = MemberGroup { origin: Origin::Own, members: s.members.clone() };
                e.methods = self.group_entries(&[own]);
            }
            SymbolKind::Overload => {
                e.methods =
                    s.links.iter().filter_map(|l| l.symbol).filter(|&m| self.shown(m)).map(|m| self.entry(m)).collect();
            }
            SymbolKind::Constant | SymbolKind::Method | SymbolKind::Macro => {}
        }
        e
    }

    /// Adds a type's fields, members and methods from every origin.
    fn add_members(&self, e: &mut Entry, ty: SymbolId) {
        let s = self.index.symbol(ty);
        e.fields = self
            .index
            .fields_of(ty)
            .into_iter()
            .map(|f| {
                let origin = match &f.promoted_through {
                    Some(path) => {
                        let through = self.index.symbol(f.owner);
                        OriginInfo { kind: "using", via: format!("using {path}: {}", through.name), location: None }
                    }
                    None => own(),
                };
                self.field_entry(f.owner, f.index, Some(origin))
            })
            .collect();
        e.members = (0..s.enum_members.len()).map(|i| self.member_entry(ty, i)).collect();
        e.variants =
            if s.kind == SymbolKind::Union { s.links.iter().map(|l| l.text.clone()).collect() } else { Vec::new() };
        e.methods = self.group_entries(&self.index.member_groups(ty));
    }

    /// The members of groups, each with its origin, leaving out private
    /// ones unless asked.
    fn group_entries(&self, groups: &[MemberGroup]) -> Vec<Entry> {
        let mut out = Vec::new();
        for group in groups {
            let origin = self.origin(&group.origin);
            for &m in &group.members {
                if !self.shown(m) {
                    continue;
                }
                let mut e = self.entry(m);
                e.origin = Some(origin.clone());
                out.push(e);
            }
        }
        out
    }

    fn origin(&self, origin: &Origin) -> OriginInfo {
        match origin {
            Origin::Own => own(),
            Origin::Include { module, by } => {
                let m = self.index.symbol(*module);
                let b = self.index.symbol(*by);
                let mut via = format!("include {}", self.qualified(*module));
                match b.kind {
                    SymbolKind::Module => via.push_str(&format!(" (through module {})", self.index.path_of(*by))),
                    SymbolKind::Extension => via.push_str(&format!(" (through {})", b.signature)),
                    _ => {}
                }
                OriginInfo { kind: "include", via, location: self.location(m.span) }
            }
            Origin::Extend { extension } => {
                let x = self.index.symbol(*extension);
                OriginInfo { kind: "extend", via: x.signature.clone(), location: self.location(x.span) }
            }
            Origin::Using { path, ty } => {
                OriginInfo { kind: "using", via: format!("using {path}: {}", self.qualified(*ty)), location: None }
            }
        }
    }

    /// A symbol's name, with its package's name when that is not the
    /// root's.
    fn qualified(&self, id: SymbolId) -> String {
        let s = self.index.symbol(id);
        if s.package == PackageId(0) || self.index.package(s.package).path.starts_with("cimport:") {
            self.index.path_of(id)
        } else {
            format!("{}.{}", self.index.package(s.package).name, self.index.path_of(id))
        }
    }

    fn field_entry(&self, owner: SymbolId, index: usize, origin: Option<OriginInfo>) -> Entry {
        let o = self.index.symbol(owner);
        let f = &o.fields[index];
        let path = format!("{}.{}", self.index.path_of(owner), f.name);
        Entry {
            owner: Some(("struct", self.index.path_of(owner))),
            doc: f.doc.clone(),
            location: if o.c.is_some() { None } else { self.location(f.span) },
            origin,
            package: self.index.package(o.package).path.clone(),
            ..blank_entry("field", &f.name, &path, &f.declaration())
        }
    }

    fn member_entry(&self, owner: SymbolId, index: usize) -> Entry {
        let o = self.index.symbol(owner);
        let m = &o.enum_members[index];
        let path = format!("{}.{}", self.index.path_of(owner), m.name);
        Entry {
            owner: Some(("enum", self.index.path_of(owner))),
            doc: m.doc.clone(),
            location: self.location(m.span),
            package: self.index.package(o.package).path.clone(),
            ..blank_entry("enum_member", &m.name, &path, &m.declaration())
        }
    }
}

/// The origin of a type's own members.
fn own() -> OriginInfo {
    OriginInfo { kind: "own", via: String::new(), location: None }
}

fn blank_entry(kind: &'static str, name: &str, path: &str, signature: &str) -> Entry {
    Entry {
        kind,
        name: name.to_string(),
        path: path.to_string(),
        package: String::new(),
        signature: signature.to_string(),
        attributes: Vec::new(),
        doc: None,
        private: false,
        is_static: false,
        location: None,
        owner: None,
        c: None,
        origin: None,
        promoted_into: None,
        fields: Vec::new(),
        members: Vec::new(),
        variants: Vec::new(),
        targets: Vec::new(),
        aliases: None,
        methods: Vec::new(),
    }
}

/// The sections of an overview, in order, with the kinds each holds.
const SECTIONS: &[(&str, &[&str])] = &[
    ("CONSTANTS", &["constant"]),
    ("TYPES", &["struct", "enum", "union", "type_alias"]),
    ("MODULES", &["module"]),
    ("METHODS", &["method", "overload"]),
    ("MACROS", &["macro"]),
    ("EXTENSIONS", &["extension"]),
];

/// Renders a page as text: declarations at the margin, docs indented by
/// four spaces. An overview shows the first paragraph of each doc; a
/// symbol's page shows its whole doc and the first paragraph of what it
/// lists.
pub fn render_text(page: &Page) -> String {
    let mut out = String::new();
    out.push_str(&package_line(&page.package));
    out.push('\n');
    match &page.symbol {
        None => {
            if let Some(doc) = &page.package.doc {
                out.push('\n');
                push_doc(&mut out, doc, 0);
            }
            for (title, kinds) in SECTIONS {
                let entries: Vec<&Entry> = page.items.iter().filter(|e| kinds.contains(&e.kind)).collect();
                if entries.is_empty() {
                    continue;
                }
                out.push('\n');
                out.push_str(title);
                out.push('\n');
                for e in entries {
                    out.push('\n');
                    overview_text(&mut out, e);
                }
            }
        }
        Some(_) => {
            for e in &page.items {
                out.push('\n');
                symbol_text(&mut out, e);
            }
        }
    }
    out
}

/// `package fmt // import "core:fmt"`.
fn package_line(p: &PackageInfo) -> String {
    if let Some(header) = p.path.strip_prefix("cimport:") {
        format!("package {} // cimport \"{header}\"", p.name)
    } else if p.path == "." {
        format!("package {}", p.name)
    } else {
        format!("package {} // import \"{}\"", p.name, p.path)
    }
}

/// Appends doc text, every line indented by `indent` spaces.
fn push_doc(out: &mut String, doc: &str, indent: usize) {
    for line in doc.lines() {
        if line.trim().is_empty() {
            out.push('\n');
        } else {
            out.push_str(&" ".repeat(indent));
            out.push_str(line);
            out.push('\n');
        }
    }
}

fn line(out: &mut String, indent: usize, text: &str) {
    out.push_str(&" ".repeat(indent));
    out.push_str(text);
    out.push('\n');
}

/// One declaration of an overview.
fn overview_text(out: &mut String, e: &Entry) {
    line(out, 0, &private_prefix(e));
    let mut wrote = false;
    if let Some(doc) = &e.doc {
        push_doc(out, &first_paragraph(doc), 4);
        wrote = true;
    }
    for list in [&e.fields, &e.members, &e.methods] {
        if list.is_empty() {
            continue;
        }
        if wrote {
            out.push('\n');
        }
        wrote = true;
        for m in list {
            line(out, 4, &private_prefix(m));
            if let Some(doc) = &m.doc {
                push_doc(out, &first_paragraph(doc), 8);
            }
        }
    }
}

/// The signature, with `private` when the declaration is.
fn private_prefix(e: &Entry) -> String {
    if e.private { format!("private {}", e.signature) } else { e.signature.clone() }
}

/// A symbol's page.
fn symbol_text(out: &mut String, e: &Entry) {
    if !e.attributes.is_empty() {
        line(out, 0, &format!("@[{}]", e.attributes.join(", ")));
    }
    line(out, 0, &private_prefix(e));
    if let Some(doc) = &e.doc {
        push_doc(out, doc, 4);
        out.push('\n');
    }
    line(out, 4, &context_line(e));
    let lists: [(&str, &Vec<Entry>); 2] = [("FIELDS", &e.fields), ("MEMBERS", &e.members)];
    for (title, list) in lists {
        grouped(out, title, list);
    }
    if !e.variants.is_empty() {
        out.push_str("\nVARIANTS\n\n");
        for v in &e.variants {
            line(out, 0, v);
        }
    }
    let title = if e.kind == "overload" { "MEMBERS" } else { "METHODS" };
    grouped(out, title, &e.methods);
}

/// Lists entries under `title`, starting a new heading wherever their
/// origin changes: `METHODS FROM include Greeter`.
fn grouped(out: &mut String, title: &str, list: &[Entry]) {
    let mut current: Option<&str> = None;
    for m in list {
        let via = m.origin.as_ref().map_or("", |o| o.via.as_str());
        if current != Some(via) {
            out.push('\n');
            if via.is_empty() {
                line(out, 0, title);
            } else {
                let at = m.origin.as_ref().and_then(|o| o.location.as_ref());
                match at {
                    Some(at) => line(out, 0, &format!("{title} FROM {via} ({}:{})", at.file, at.line)),
                    None => line(out, 0, &format!("{title} FROM {via}")),
                }
            }
            out.push('\n');
            current = Some(via);
        }
        line(out, 0, &private_prefix(m));
        if let Some(doc) = &m.doc {
            push_doc(out, &first_paragraph(doc), 4);
        }
    }
}

/// What a declaration is and where it comes from: `method of struct Ball,
/// defined at ball.wid:12`.
fn context_line(e: &Entry) -> String {
    let what = match (e.kind, &e.owner) {
        ("builtin_type", _) => {
            return "builtin type; listed below are the methods extensions in this program add to it".to_string();
        }
        ("method", Some(("extension", ext))) => format!("method added by {ext}"),
        ("method", Some((kind, owner))) if e.is_static => format!("type-level method of {kind} {owner}"),
        ("method", Some((kind, owner))) => format!("method of {kind} {owner}"),
        ("constant", Some((kind, owner))) => format!("constant of {kind} {owner}"),
        ("overload", Some((kind, owner))) => format!("overload set of {kind} {owner}"),
        ("field", Some((_, owner))) => match (&e.promoted_into, &e.origin) {
            (Some(into), Some(origin)) => format!("field of struct {owner}, promoted into {into} by `{}`", origin.via),
            _ => format!("field of struct {owner}"),
        },
        ("enum_member", Some((_, owner))) => format!("member of enum {owner}"),
        ("overload", _) => "overload set".to_string(),
        (kind, _) => kind.replace('_', " "),
    };
    match (&e.c, &e.location) {
        (Some(c), _) => {
            let c_kind = match e.kind {
                "method" => "function",
                "constant" => "constant",
                _ => "type",
            };
            let at = c.declared_at.as_ref().map(|d| format!(", declared at {d}")).unwrap_or_default();
            format!("C {c_kind} `{}`{at} (cimport \"{}\")", c.name, c.header)
        }
        (None, Some(at)) => format!("{what}, defined at {}:{}", at.file, at.line),
        (None, None) => what,
    }
}

/// Renders a page as the JSON document `wid doc -json` prints: `package`
/// (`name`, `path`, `doc`), `symbol` (the path asked for, or null) and
/// `items`.
pub fn render_json(page: &Page) -> String {
    let doc = json!({
        "package": {
            "name": page.package.name,
            "path": page.package.path,
            "doc": page.package.doc,
        },
        "symbol": page.symbol,
        "items": page.items.iter().map(entry_json).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

fn location_json(at: &Option<Location>) -> Value {
    match at {
        Some(at) => json!({"file": at.file, "line": at.line, "column": at.column}),
        None => Value::Null,
    }
}

fn entry_json(e: &Entry) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("kind".into(), json!(e.kind));
    map.insert("name".into(), json!(e.name));
    map.insert("path".into(), json!(e.path));
    map.insert("package".into(), json!(e.package));
    map.insert("signature".into(), json!(e.signature));
    map.insert("attributes".into(), json!(e.attributes));
    map.insert("doc".into(), json!(e.doc));
    map.insert("private".into(), json!(e.private));
    map.insert("location".into(), location_json(&e.location));
    if e.kind == "method" || e.kind == "macro" {
        map.insert("static".into(), json!(e.is_static));
    }
    if let Some((kind, path)) = &e.owner {
        map.insert("owner".into(), json!({"kind": kind, "name": path}));
    }
    if let Some(c) = &e.c {
        map.insert("c".into(), json!({"name": c.name, "header": c.header, "declared_at": c.declared_at}));
    }
    if let Some(into) = &e.promoted_into {
        map.insert("promoted_into".into(), json!(into));
    }
    if let Some(o) = &e.origin {
        let via = if o.via.is_empty() { Value::Null } else { json!(o.via) };
        map.insert("origin".into(), json!({"kind": o.kind, "via": via, "location": location_json(&o.location)}));
    }
    match e.kind {
        "struct" => {
            map.insert("fields".into(), json!(e.fields.iter().map(entry_json).collect::<Vec<_>>()));
        }
        "enum" => {
            map.insert("members".into(), json!(e.members.iter().map(entry_json).collect::<Vec<_>>()));
        }
        "union" => {
            map.insert("variants".into(), json!(e.variants));
        }
        "extension" => {
            map.insert("targets".into(), json!(e.targets));
        }
        "type_alias" => {
            map.insert("aliases".into(), json!(e.aliases));
            if !e.fields.is_empty() {
                map.insert("fields".into(), json!(e.fields.iter().map(entry_json).collect::<Vec<_>>()));
            }
            if !e.members.is_empty() {
                map.insert("members".into(), json!(e.members.iter().map(entry_json).collect::<Vec<_>>()));
            }
        }
        _ => {}
    }
    if matches!(e.kind, "struct" | "enum" | "union" | "module" | "extension" | "overload" | "builtin_type")
        || !e.methods.is_empty()
    {
        map.insert("methods".into(), json!(e.methods.iter().map(entry_json).collect::<Vec<_>>()));
    }
    Value::Object(map)
}

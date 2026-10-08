//! `wid doc`: documentation for a package, a type, a method or a C symbol,
//! generated from doc comments.
//!
//! The command loads and checks the package with [`crate::analyze`], never
//! generating code, and reads the [`Index`](wid_sema::index::Index) the
//! checker builds of its declarations. It reads its arguments (see
//! [`DocRequest`]), resolves the symbol path with
//! [`Index::resolve`](wid_sema::index::Index::resolve) and builds a
//! [`Page`] of [`Item`]s, the model `wid query` answers with too, which
//! [`render_text`] and [`render_json`] print. Errors in the package are
//! reported as usual, and the page still documents every declaration the
//! checker collected.

use std::path::{Path, PathBuf};

use serde_json::json;
use wid_diagnostics::{Diagnostics, SourceMap};
use wid_query::item::{Item, ItemBuilder, PackageInfo, Style, item_json, package_json};
use wid_sema::PackageId;
use wid_sema::index::Target;
use wid_syntax::docs::first_paragraph;

use crate::Options;
use crate::cmdline::{CommandLine, ErrorContext, PackageArg, Tool, package_target};

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
    pub items: Vec<Item>,
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
    let args = &request.args;
    let mut cmd = CommandLine::new("wid doc");
    for arg in args {
        cmd.arg("", arg);
    }
    if opts.file_mode {
        cmd.flag("-file");
    }
    let dir = if request.dir.as_os_str().is_empty() { Path::new(".") } else { request.dir.as_path() };
    let reading = match args.len() {
        0 => Reading { package: None, symbol: None },
        1 if opts.file_mode || names_package(dir, &args[0]) => Reading { package: Some(0), symbol: None },
        1 => Reading { package: None, symbol: Some(0) },
        _ => Reading { package: Some(0), symbol: Some(1) },
    };
    let package = PackageArg {
        arg: reading.package.map(|i| (args[i].as_str(), i)),
        read_as_symbol: reading.symbol.filter(|_| reading.package.is_none()).map(|i| args[i].as_str()),
    };
    let target = match package_target(opts, dir, &cmd, Tool::Doc, package) {
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
    opts.testing = false;
    let mut analysis = crate::analyze(&opts);
    let file = cmd.add(&mut analysis.sources);
    let Some(root) = analysis.root() else {
        return DocOutput { sources: analysis.sources, diags: analysis.diags, page: None };
    };
    let builder = analysis.items(request.private);
    let page = match reading.symbol {
        None => Some(package_page(&builder, root)),
        Some(i) => {
            let symbol = &args[i];
            match wid_query::resolve(&analysis.index, root, symbol, request.private) {
                Ok(target) => Some(symbol_page(&builder, root, symbol, &target)),
                Err(failure) => {
                    let ctx = ErrorContext {
                        index: &analysis.index,
                        cmd: &cmd,
                        file,
                        tool: Tool::Doc,
                        arg: i,
                        query_arg: None,
                        symbol,
                        read_as_symbol: reading.package.is_none() && args.len() == 1,
                        package_arg: reading.package.map(|p| args[p].as_str()),
                    };
                    let diag = ctx.report(failure);
                    analysis.diags.push(diag);
                    None
                }
            }
        }
    };
    analysis.diags.sort();
    DocOutput { sources: analysis.sources, diags: analysis.diags, page }
}

/// What `wid doc` and `wid query` print.
pub struct Printed {
    /// The page or the answer: text, or JSON.
    pub stdout: String,
    /// The diagnostics: rendered, or JSON.
    pub stderr: String,
    /// Whether the request succeeded and the package had no errors.
    pub success: bool,
}

/// Renders a result the way `wid doc` prints it: the page on stdout (as
/// JSON with `json`) and the diagnostics on stderr (as JSON with
/// `json_errors`). The summary line after the diagnostics says whether a
/// page was written despite the errors.
pub fn print(out: &DocOutput, json: bool, json_errors: bool, color: bool) -> Printed {
    let stderr = if json_errors {
        wid_diagnostics::render_json(&out.diags, &out.sources) + "\n"
    } else if out.diags.is_empty() {
        String::new()
    } else {
        let failure = match out.page {
            Some(_) => "the documentation may be incomplete due to",
            None => "could not write the documentation due to",
        };
        wid_diagnostics::render_all_with(&out.diags, &out.sources, wid_diagnostics::RenderOptions { color }, failure)
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

/// The overview of a package: every declaration shown, with what it
/// declares itself.
fn package_page(builder: &ItemBuilder, pkg: PackageId) -> Page {
    let items = builder
        .index
        .package(pkg)
        .items
        .iter()
        .filter(|&&id| builder.shown(id))
        .map(|&id| builder.overview_item(id))
        .collect();
    Page { package: builder.package_info(pkg), symbol: None, items }
}

/// The page for what a symbol path named in `pkg`.
fn symbol_page(builder: &ItemBuilder, pkg: PackageId, path: &str, target: &Target) -> Page {
    if let Target::Package(p) = target {
        return package_page(builder, *p);
    }
    let item = builder.target_item(target, path);
    Page { package: builder.package_info(pkg), symbol: Some(path.to_string()), items: vec![item] }
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
                let entries: Vec<&Item> = page.items.iter().filter(|e| kinds.contains(&e.kind)).collect();
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
fn overview_text(out: &mut String, e: &Item) {
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
fn private_prefix(e: &Item) -> String {
    if e.private { format!("private {}", e.signature) } else { e.signature.clone() }
}

/// A symbol's page.
fn symbol_text(out: &mut String, e: &Item) {
    if !e.attributes.is_empty() {
        line(out, 0, &format!("@[{}]", e.attributes.join(", ")));
    }
    line(out, 0, &private_prefix(e));
    if let Some(doc) = &e.doc {
        push_doc(out, doc, 4);
        out.push('\n');
    }
    line(out, 4, &context_line(e));
    let lists: [(&str, &Vec<Item>); 2] = [("FIELDS", &e.fields), ("MEMBERS", &e.members)];
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
fn grouped(out: &mut String, title: &str, list: &[Item]) {
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
fn context_line(e: &Item) -> String {
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
        "package": package_json(&page.package),
        "symbol": page.symbol,
        "items": page.items.iter().map(|e| item_json(e, Style::Doc)).collect::<Vec<_>>(),
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

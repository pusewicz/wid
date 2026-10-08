//! Every `core` package has a package doc, and every public declaration in
//! it a doc comment: its types, their fields, enum members and methods,
//! constants, methods, overload sets and extensions. So `wid doc core:<pkg>`
//! has a summary for every entry.

use std::path::PathBuf;

use wid_diagnostics::FileId;
use wid_driver::Options;
use wid_query::Analysis;
use wid_sema::index::SymbolId;
use wid_syntax::docs::DocComments;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root exists")
}

/// Whether a doc comment says something.
fn documented(doc: Option<&str>) -> bool {
    doc.is_some_and(|d| !d.trim().is_empty())
}

/// Where a symbol is declared, as `core/pkg/file.wid:line`.
fn location(analysis: &Analysis, id: SymbolId) -> String {
    let span = analysis.index.symbol(id).span;
    let file = analysis.sources.file(span.file);
    let root = format!("{}/", repo_root().display());
    format!("{}:{}", file.display.trim_start_matches(&root), file.line_col(span.start).0)
}

/// Adds what `id` and its members leave undocumented to `missing`.
fn check(analysis: &Analysis, id: SymbolId, missing: &mut Vec<String>) {
    let index = &analysis.index;
    if !index.is_public(id) {
        return;
    }
    let s = index.symbol(id);
    let path = index.path_of(id);
    let at = location(analysis, id);
    if !documented(s.doc.as_deref()) {
        missing.push(format!("{at}: {} `{path}`", s.kind.as_str()));
    }
    for field in &s.fields {
        if !documented(field.doc.as_deref()) {
            missing.push(format!("{at}: field `{path}.{}`", field.name));
        }
    }
    for member in &s.enum_members {
        if !documented(member.doc.as_deref()) {
            missing.push(format!("{at}: enum member `{path}.{}`", member.name));
        }
    }
    for &member in &s.members {
        check(analysis, member, missing);
    }
}

#[test]
fn every_public_core_declaration_has_a_doc() {
    let root = repo_root();
    let mut packages: Vec<PathBuf> = std::fs::read_dir(root.join("core"))
        .expect("read core/")
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    packages.sort();
    assert!(packages.len() >= 8, "core/ holds its packages");
    let mut missing = Vec::new();
    for dir in packages {
        let name = format!("core:{}", dir.file_name().unwrap_or_default().to_string_lossy());
        let mut opts = Options::new(&dir);
        opts.wid_root = Some(root.clone());
        let analysis = wid_driver::analyze(&opts, &wid_driver::Overlay::new());
        assert!(!analysis.diags.has_errors(), "{name} has errors");
        let pkg = analysis.root().expect("the package loads");
        let package = analysis.index.package(pkg);
        assert_eq!(package.path, name);
        // The package doc opens the file named after the package, so it
        // describes the package rather than one file.
        let own = dir.join(format!("{}.wid", package.name));
        let text = std::fs::read_to_string(&own).unwrap_or_default();
        let (ast, _) = wid_syntax::parse_file(FileId(0), &text);
        let own_doc = DocComments::new(&ast, &text).package_doc();
        if !documented(own_doc.as_deref()) {
            missing.push(format!(
                "{name}: no package doc; open `core/{}/{}.wid` with a comment block and a blank line",
                package.name, package.name
            ));
        }
        assert_eq!(package.doc, own_doc, "{name}'s package doc comes from its own file");
        for &id in &package.items {
            check(&analysis, id, &mut missing);
        }
    }
    assert!(missing.is_empty(), "these public `core` declarations have no doc comment:\n{}", missing.join("\n"));
}

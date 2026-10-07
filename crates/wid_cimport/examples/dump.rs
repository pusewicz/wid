//! Prints what `wid_cimport` makes of a header.
//!
//! `cargo run --example dump -- <header|<name>> [clang args…]`. A header
//! written as `<name>` is looked up on the include path. Pass `-v` first to
//! print every item instead of a summary.

use std::collections::BTreeMap;
use std::path::PathBuf;

use wid_cimport::{Header, ImportRequest, ItemKind, MacroKind, import};

/// Parses the command line, imports the header and prints the result.
fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let verbose = args.first().is_some_and(|arg| arg == "-v");
    if verbose {
        args.remove(0);
    }
    if args.is_empty() {
        eprintln!("usage: dump [-v] <header|<name>> [clang args…]");
        std::process::exit(2);
    }
    let target = args.remove(0);
    let header = match target.strip_prefix('<').and_then(|name| name.strip_suffix('>')) {
        Some(name) => Header::Include(name.to_string()),
        None => Header::Path(PathBuf::from(target)),
    };
    let mut request = ImportRequest::new(header);
    request.clang_args = args;
    let started = std::time::Instant::now();
    let module = match import(&request) {
        Ok(module) => module,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    };
    let elapsed = started.elapsed();
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for item in &module.items {
        let kind = match &item.kind {
            ItemKind::Function(_) => "functions",
            ItemKind::Record(record) if record.is_opaque() => "records (opaque)",
            ItemKind::Record(_) => "records",
            ItemKind::Enum(_) => "enums",
            ItemKind::Typedef(_) => "typedefs",
            ItemKind::Global(_) => "globals",
            ItemKind::Macro(definition) => match &definition.kind {
                MacroKind::Expr { value: Some(_), .. } => "macros (constant)",
                MacroKind::Expr { value: None, .. } => "macros (typed, no value)",
                MacroKind::Other => "macros (not an expression)",
                MacroKind::FunctionLike { .. } => "macros (function-like)",
            },
        };
        *counts.entry(kind).or_default() += 1;
        if verbose {
            println!("{}:{} {:?}", item.location.file.display(), item.location.line, item.kind);
        }
    }
    println!("{} ({}, root {})", module.header.display(), module.target.triple, module.root.display());
    println!("{} items in {elapsed:.2?}", module.items.len());
    for (kind, count) in counts {
        println!("  {count:5} {kind}");
    }
}

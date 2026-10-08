//! Hover, go-to-definition and document symbols, read from what `wid query
//! type`, `def` and `outline` answer.

use lsp_types::{DocumentSymbol, Range, SymbolKind};
use wid_diagnostics::{FileId, SourceFile};
use wid_query::{Analysis, Item, Location, TypeItem};
use wid_syntax::docs::first_paragraph;

use crate::position::{Encoding, LineIndex};

/// The range of a location of `file`; `None` when it is in another file.
fn range_of(file: &SourceFile, lines: &LineIndex<'_>, at: &Location, encoding: Encoding) -> Option<Range> {
    if at.file != file.display {
        return None;
    }
    let start = file.offset_of(at.line, at.column)?;
    let end = file.offset_of(at.end_line, at.end_column)?;
    Some(lines.range(start as usize, end as usize, encoding))
}

/// Where the name a hover found is, in `file`.
pub(crate) fn name_range(file: &SourceFile, found: &TypeItem, encoding: Encoding) -> Option<Range> {
    range_of(file, &LineIndex::new(&file.text), &found.location, encoding)
}

/// A declaration kind in prose.
fn kind_words(kind: &str) -> &str {
    match kind {
        "type_alias" => "type alias",
        "overload" => "overload set",
        "enum_member" => "enum member",
        "builtin_type" => "builtin type",
        "local" => "local variable",
        other => other,
    }
}

/// What a hover shows: the declaration line of what the name refers to
/// (or the expression's type), what it is and where it is declared, its
/// type where the declaration line doesn't say it, and the first paragraph
/// of its doc. Markdown, or plain text for a client that can't show it.
pub(crate) fn hover_text(found: &TypeItem, markdown: bool) -> Option<String> {
    let code = |text: &str| if markdown { format!("```wid\n{text}\n```") } else { text.to_string() };
    let quote = |text: &str| if markdown { format!("`{text}`") } else { text.to_string() };
    // Code the checker couldn't type (it has errors) shows no type.
    let ty = found.ty.as_deref().filter(|t| !t.contains("{unknown}"));
    let mut blocks = Vec::new();
    let Some(item) = &found.refers_to else {
        blocks.push(code(ty?));
        if !found.instances.is_empty() {
            blocks.push(instances(found, &quote));
        }
        return Some(blocks.join("\n\n"));
    };
    blocks.push(code(&item.signature));
    let mut about = match item.kind {
        "local" | "parameter" => kind_words(item.kind).to_string(),
        "package" => format!("package {}", quote(&item.package)),
        kind => {
            let mut about = format!("{} {}", kind_words(kind), quote(&item.path));
            if item.private {
                about.insert_str(0, "private ");
            }
            if !item.package.is_empty() && item.package != "." {
                about.push_str(&format!(" in {}", quote(&item.package)));
            }
            if let Some(c) = &item.c {
                about.push_str(&format!(", C {} from {}", quote(&c.name), quote(&c.header)));
            }
            about
        }
    };
    if let Some(ty) = ty {
        match found.kind {
            "call" => about.push_str(&format!("\n\nReturns {}", quote(ty))),
            "expression" => about.push_str(&format!("\n\nType: {}", quote(ty))),
            "local" | "parameter" | "field" if !item.signature.contains(ty) => {
                about.push_str(&format!("\n\nType: {}", quote(ty)))
            }
            _ => {}
        }
    }
    blocks.push(about);
    if !found.instances.is_empty() {
        blocks.push(instances(found, &quote));
    }
    if let Some(doc) = &item.doc {
        blocks.push(first_paragraph(doc));
    }
    Some(blocks.join("\n\n"))
}

fn instances(found: &TypeItem, quote: &dyn Fn(&str) -> String) -> String {
    let list: Vec<String> = found.instances.iter().map(|t| quote(t)).collect();
    format!("Instances: {}", list.join(", "))
}

/// Where the declaration an item names is: its file and the byte range of
/// its name. A package is the file named after it (or its first file),
/// at its start. `None` for a builtin type and for a C declaration, which
/// have no Wid source.
pub(crate) fn declaration<'a>(analysis: &'a Analysis, item: &Item) -> Option<(&'a SourceFile, usize, usize)> {
    let sources = &analysis.sources;
    if let Some(at) = &item.location {
        let file = sources.file(sources.find_by_display(&at.file)?);
        let start = file.offset_of(at.line, at.column)?;
        let end = file.offset_of(at.end_line, at.end_column)?;
        return file.path.is_absolute().then_some((file, start as usize, end as usize));
    }
    if item.kind != "package" {
        return None;
    }
    let package = analysis.index.packages.iter().find(|p| p.path == item.package)?;
    let files: Vec<&SourceFile> = sources
        .files()
        .iter()
        .filter(|f| f.path.is_absolute() && f.path.parent() == Some(package.dir.as_path()))
        .collect();
    let named = files.iter().find(|f| f.path.file_stem().is_some_and(|s| s.to_string_lossy() == package.name));
    named.or(files.first()).map(|f| (*f, 0, 0))
}

/// The symbols of `file`, from the outline of the package being checked:
/// its declarations written there, with fields, enum members and methods
/// under their type.
pub(crate) fn document_symbols(analysis: &Analysis, file: FileId, encoding: Encoding) -> Vec<DocumentSymbol> {
    let Some(package) = analysis.root() else { return Vec::new() };
    let source = analysis.sources.file(file);
    let lines = LineIndex::new(&source.text);
    wid_query::outline(analysis, package).iter().filter_map(|item| symbol(item, source, &lines, encoding)).collect()
}

#[allow(deprecated)] // `DocumentSymbol::deprecated` must be written to build one.
fn symbol(item: &Item, file: &SourceFile, lines: &LineIndex<'_>, encoding: Encoding) -> Option<DocumentSymbol> {
    let range = range_of(file, lines, item.span.as_ref()?, encoding)?;
    let inside = |r: &Range| range.start <= r.start && r.end <= range.end;
    let selection_range =
        item.location.as_ref().and_then(|at| range_of(file, lines, at, encoding)).filter(inside).unwrap_or(range);
    let children: Vec<DocumentSymbol> = item
        .fields
        .iter()
        .chain(&item.members)
        .chain(&item.methods)
        .filter_map(|child| symbol(child, file, lines, encoding))
        .collect();
    Some(DocumentSymbol {
        name: if item.name.is_empty() { item.signature.clone() } else { item.name.clone() },
        detail: Some(item.signature.clone()),
        kind: symbol_kind(item),
        tags: None,
        deprecated: None,
        range,
        selection_range,
        children: (!children.is_empty()).then_some(children),
    })
}

/// The LSP kind of a declaration.
fn symbol_kind(item: &Item) -> SymbolKind {
    match item.kind {
        "constant" => SymbolKind::CONSTANT,
        "type_alias" => SymbolKind::TYPE_PARAMETER,
        "struct" => SymbolKind::STRUCT,
        "enum" | "union" => SymbolKind::ENUM,
        "module" => SymbolKind::MODULE,
        "method" if item.owner.is_some() => SymbolKind::METHOD,
        "extension" => SymbolKind::OBJECT,
        "field" => SymbolKind::FIELD,
        "enum_member" => SymbolKind::ENUM_MEMBER,
        _ => SymbolKind::FUNCTION,
    }
}

#[cfg(test)]
mod tests {
    use wid_query::{Item, Location, TypeItem};

    use super::hover_text;

    fn at(line: u32) -> Location {
        Location { file: "main.wid".into(), line, column: 1, end_line: line, end_column: 2 }
    }

    fn found(kind: &'static str, ty: Option<&str>, refers_to: Option<Item>) -> TypeItem {
        TypeItem { location: at(1), span: at(1), ty: ty.map(str::to_string), instances: Vec::new(), kind, refers_to }
    }

    #[test]
    fn hovers_show_the_declaration_and_its_doc() {
        let mut method = Item::blank("method", "move", "Ball.move", "def move(by: Int) -> Int");
        method.doc = Some("Moves the ball.\n\nMore.".into());
        method.package = ".".into();
        let text = hover_text(&found("call", Some("Int"), Some(method.clone())), true).expect("a hover");
        assert_eq!(
            text,
            "```wid\ndef move(by: Int) -> Int\n```\n\nmethod `Ball.move`\n\nReturns `Int`\n\nMoves the ball."
        );
        let plain = hover_text(&found("call", Some("Int"), Some(method)), false).expect("a hover");
        assert_eq!(plain, "def move(by: Int) -> Int\n\nmethod Ball.move\n\nReturns Int\n\nMoves the ball.");
        let local = Item::blank("local", "b", "b", "b: Ball");
        let text = hover_text(&found("local", Some("Ball"), Some(local)), true).expect("a hover");
        assert_eq!(text, "```wid\nb: Ball\n```\n\nlocal variable");
        let mut constant = Item::blank("constant", "MAX", "MAX", "MAX = 3");
        constant.package = "core:geo".into();
        let text = hover_text(&found("expression", Some("Int"), Some(constant)), true).expect("a hover");
        assert_eq!(text, "```wid\nMAX = 3\n```\n\nconstant `MAX` in `core:geo`\n\nType: `Int`");
        assert_eq!(hover_text(&found("expression", Some("F64"), None), true).as_deref(), Some("```wid\nF64\n```"));
        assert_eq!(hover_text(&found("expression", None, None), true), None);
        assert_eq!(hover_text(&found("expression", Some("{unknown}"), None), true), None);
    }
}

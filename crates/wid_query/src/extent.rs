//! Where whole declarations are. The symbol index keeps where a
//! declaration's name is; this table, read from the syntax trees, finds the
//! declaration around it: from its attributes (or `private`) to its last
//! token, `end` included.

use std::collections::HashMap;

use wid_diagnostics::{FileId, SourceMap, Span};
use wid_sema::ProgramInput;
use wid_syntax::ast::{File, Item, ItemKind};
use wid_syntax::visit::Visit;
use wid_syntax::visit::shared::walk_item;

/// The extents of the declarations written in a program's files: every
/// method, type, module, extension, constant, overload set and field, and
/// every enum member (its name and value), wherever it is written, in a
/// `quote` too. A macro call among declarations counts too: it is the
/// extent of a declaration it generates whose name comes from its
/// arguments (`counter :kills`).
#[derive(Clone, Debug, Default)]
pub struct Extents {
    /// By file, the declarations' spans, in the order they were met.
    by_file: HashMap<FileId, Vec<Span>>,
}

impl Extents {
    /// The extents of every file of a program.
    pub fn of_program(input: &ProgramInput) -> Extents {
        let mut extents = Extents::default();
        for package in &input.packages {
            for file in &package.files {
                extents.add_file(&file.ast);
            }
        }
        extents
    }

    /// Adds the declarations of one file.
    pub fn add_file(&mut self, file: &File) {
        let mut collector = Collector { spans: Vec::new() };
        collector.visit_items(&file.items);
        self.by_file.entry(file.file).or_default().extend(collector.spans);
    }

    /// The whole declaration whose name is at `name`: the smallest one that
    /// holds it. Code a macro generated is found in the macro's `quote`,
    /// and the result keeps the expansion's file id, like `name`.
    pub fn declaration(&self, sources: &SourceMap, name: Span) -> Option<Span> {
        let spans = self.by_file.get(&sources.real_file(name.file))?;
        spans
            .iter()
            .filter(|s| s.start <= name.start && name.end <= s.end)
            .min_by_key(|s| (s.end - s.start, s.start))
            .map(|s| Span::new(name.file, s.start, s.end))
    }
}

/// Collects the spans of declarations.
struct Collector {
    spans: Vec<Span>,
}

impl Visit for Collector {
    fn visit_item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Def(_)
            | ItemKind::Struct(_)
            | ItemKind::Union(_)
            | ItemKind::Module(_)
            | ItemKind::Extend(_)
            | ItemKind::Const(_)
            | ItemKind::Overload(_)
            | ItemKind::Field(_)
            | ItemKind::MacroCall(_) => self.spans.push(item.span),
            ItemKind::Enum(e) => {
                self.spans.push(item.span);
                for m in &e.members {
                    let end = m.value.as_ref().map_or(m.name.span.end, |v| v.span.end.max(m.name.span.end));
                    self.spans.push(Span::new(m.name.span.file, m.name.span.start, end));
                }
            }
            ItemKind::Import(_)
            | ItemKind::Cimport(_)
            | ItemKind::Include(_)
            | ItemKind::ComptimeIf(_)
            | ItemKind::Splice(_)
            | ItemKind::Error => {}
        }
        walk_item(self, item);
    }
}

#[cfg(test)]
mod tests {
    use wid_diagnostics::{FileId, SourceMap, Span};

    use super::Extents;

    #[test]
    fn the_smallest_declaration_around_a_name_wins() {
        let src = "struct Ball\n  pos: Int\n  counter :kills\n\n  @[inline]\n  def speed -> Int\n    1\n  end\nend\n\nenum Dir\n  north\n  east = 4\nend\n";
        let mut sources = SourceMap::new();
        let file = sources.add("main.wid".into(), "main.wid".into(), src);
        let (ast, diags) = wid_syntax::parse_file(file, src);
        assert!(diags.is_empty(), "{diags:?}");
        let mut extents = Extents::default();
        extents.add_file(&ast);
        let at = |needle: &str| {
            let start = src.find(needle).expect("needle") as u32;
            Span::new(file, start, start + needle.len() as u32)
        };
        let text = |name: &str| {
            let span = extents.declaration(&sources, at(name)).expect("a declaration");
            &src[span.start as usize..span.end as usize]
        };
        assert_eq!(text("speed"), "@[inline]\n  def speed -> Int\n    1\n  end");
        assert_eq!(text("Ball"), &src[..src.find("\n\nenum").expect("enum")]);
        assert_eq!(text("pos"), "pos: Int");
        assert_eq!(text("kills"), "counter :kills");
        assert_eq!(text("east"), "east = 4");
        assert_eq!(text("north"), "north");
        assert_eq!(extents.declaration(&sources, Span::new(FileId(9), 0, 1)), None);
    }
}

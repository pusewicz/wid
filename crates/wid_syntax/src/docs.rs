//! Doc comments: the `# ` comment lines directly above a declaration, and a
//! package's doc, the comment block that opens one of its files.
//!
//! The parser stores a declaration's doc in [`Item::doc`](crate::ast::Item);
//! these helpers find the docs that have no item of their own (enum members,
//! packages) from a file's comments.

use crate::ast::{File, ItemKind};

/// Finds doc comments in one parsed file.
pub struct DocComments<'f> {
    file: &'f File,
    text: &'f str,
    /// The byte offset where each line starts.
    line_starts: Vec<u32>,
}

impl<'f> DocComments<'f> {
    /// Prepares to read the doc comments of `file`, whose source is `text`.
    pub fn new(file: &'f File, text: &'f str) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(text.match_indices('\n').map(|(i, _)| i as u32 + 1));
        DocComments { file, text, line_starts }
    }

    fn line_of(&self, offset: u32) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i.saturating_sub(1),
        }
    }

    /// The text of the own-line comments on the lines directly above byte
    /// `offset`, with no blank line between them and it, joined with
    /// newlines. This is the rule the parser uses for
    /// [`Item::doc`](crate::ast::Item).
    pub fn before(&self, offset: u32) -> Option<String> {
        let mut wanted = self.line_of(offset).checked_sub(1)?;
        let mut lines = Vec::new();
        for comment in self.file.comments.iter().rev() {
            if !comment.own_line || comment.span.start >= offset {
                continue;
            }
            let cline = self.line_of(comment.span.start);
            if cline == wanted {
                lines.push(comment.text.clone());
                match wanted.checked_sub(1) {
                    Some(w) => wanted = w,
                    None => break,
                }
            } else if cline < wanted {
                break;
            }
        }
        if lines.is_empty() {
            return None;
        }
        lines.reverse();
        Some(lines.join("\n"))
    }

    /// The comment block the file opens with, when it documents the
    /// package rather than a declaration: the own-line comments from the
    /// first line on, followed by a blank line, the end of the file, or an
    /// `import` or `cimport` (which declare nothing to document).
    pub fn package_doc(&self) -> Option<String> {
        let mut lines = Vec::new();
        let mut next = 0;
        for comment in &self.file.comments {
            if !comment.own_line || self.line_of(comment.span.start) != next {
                break;
            }
            lines.push(comment.text.clone());
            next += 1;
        }
        if lines.is_empty() {
            return None;
        }
        let ends = match self.line_starts.get(next) {
            None => true,
            Some(&start) => {
                let end = self.line_starts.get(next + 1).map_or(self.text.len(), |&e| e as usize);
                self.text.get(start as usize..end).is_none_or(|line| line.trim().is_empty())
                    || self
                        .file
                        .items
                        .iter()
                        .find(|item| self.line_of(item.span.start) >= next)
                        .is_some_and(|item| matches!(item.kind, ItemKind::Import(_) | ItemKind::Cimport(_)))
            }
        };
        ends.then(|| lines.join("\n"))
    }
}

/// The first paragraph of a doc comment: its lines up to the first blank
/// one.
pub fn first_paragraph(doc: &str) -> String {
    doc.lines().take_while(|l| !l.trim().is_empty()).collect::<Vec<_>>().join("\n")
}

#[cfg(test)]
mod tests {
    use super::{DocComments, first_paragraph};
    use wid_diagnostics::FileId;

    fn package_doc(src: &str) -> Option<String> {
        let (file, _) = crate::parse_file(FileId(0), src);
        DocComments::new(&file, src).package_doc()
    }

    #[test]
    fn package_docs_are_the_opening_block() {
        let src = "# core:x: things.\n#\n# More.\n\nimport \"core:fmt\"\n\n# Not this.\ndef f\nend\n";
        assert_eq!(package_doc(src).as_deref(), Some("core:x: things.\n\nMore."));
        assert_eq!(package_doc("# The binding.\ncimport \"x.h\"\n").as_deref(), Some("The binding."));
        assert_eq!(package_doc("# Only a comment.\n").as_deref(), Some("Only a comment."));
        assert_eq!(package_doc("# Adds.\ndef add\nend\n"), None);
        assert_eq!(package_doc("\n# Late.\n\ndef f\nend\n"), None);
    }

    #[test]
    fn member_docs_sit_directly_above() {
        let src = "enum E\n  # The first.\n  # Really.\n  a\n\n  # Detached.\n\n  b\nend\n";
        let (file, _) = crate::parse_file(FileId(0), src);
        let docs = DocComments::new(&file, src);
        let a = src.find("  a").expect("a") as u32 + 2;
        let b = src.find("  b").expect("b") as u32 + 2;
        assert_eq!(docs.before(a).as_deref(), Some("The first.\nReally."));
        assert_eq!(docs.before(b), None);
        assert_eq!(first_paragraph("One\ntwo\n\nthree"), "One\ntwo");
    }
}

//! The compiler's diagnostics as LSP diagnostics and quick fixes.
//!
//! A diagnostic is published at its primary label. One in code a macro
//! generated is published at the macro call that generated it (the
//! outermost, which is in a file the user wrote), and its primary label in
//! the macro's `quote` becomes related information, like every secondary
//! label and every macro call that led to the code. The message is the
//! compiler's, then the primary label, the notes (`note: …`) and the helps
//! (`help: …`), one per line. A help with edits is also a quick fix.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use lsp_types::{
    CodeDescription, Diagnostic as LspDiagnostic, DiagnosticRelatedInformation, DiagnosticSeverity, Location,
    NumberOrString, Range, TextEdit, Uri,
};
use wid_diagnostics::{Applicability, Diagnostic, FileId, Severity, SourceMap, Span};

use crate::position::{Encoding, LineIndex};

/// A diagnostic as published, with the quick fixes its helps make.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Published {
    /// What the client shows.
    pub(crate) diagnostic: LspDiagnostic,
    /// The helps that carry edits.
    pub(crate) fixes: Vec<Fix>,
}

/// A help that carries edits.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Fix {
    /// The help's message.
    pub(crate) title: String,
    /// Whether the compiler knows the edits are right
    /// ([`Applicability::MachineApplicable`]).
    pub(crate) preferred: bool,
    /// The edits, by file.
    pub(crate) edits: Vec<(PathBuf, TextEdit)>,
}

/// Converts diagnostics of one analysis.
pub(crate) struct Converter<'a> {
    sources: &'a SourceMap,
    encoding: Encoding,
    /// `docs/errors` of the Wid root, when it is there: a code's page is
    /// its `codeDescription`.
    docs: Option<&'a Path>,
    /// The URI of a path: the one the client opened it with, or a new one.
    uri: &'a dyn Fn(&Path) -> Option<Uri>,
    lines: HashMap<FileId, LineIndex<'a>>,
}

impl<'a> Converter<'a> {
    /// A converter for diagnostics whose spans are in `sources`.
    pub(crate) fn new(
        sources: &'a SourceMap,
        encoding: Encoding,
        docs: Option<&'a Path>,
        uri: &'a dyn Fn(&Path) -> Option<Uri>,
    ) -> Self {
        Converter { sources, encoding, docs, uri, lines: HashMap::new() }
    }

    /// The file on disk a span is in, and its range there. Code a macro
    /// generated is in the macro's file, at its `quote`. `None` for what
    /// isn't in a file on disk: the command line, the Wid source of a
    /// `cimport`, the prelude's target constants.
    pub(crate) fn place(&mut self, span: Span) -> Option<(PathBuf, Range)> {
        let sources = self.sources;
        let file = sources.file(span.file);
        if !file.path.is_absolute() {
            return None;
        }
        let lines = self.lines.entry(file.id).or_insert_with(|| LineIndex::new(&file.text));
        Some((file.path.clone(), lines.range(span.start as usize, span.end as usize, self.encoding)))
    }

    fn location(&mut self, span: Span) -> Option<Location> {
        let (path, range) = self.place(span)?;
        Some(Location { uri: (self.uri)(&path)?, range })
    }

    /// A diagnostic, and the file it is published in: `None` when it has
    /// no place in a file on disk, for the caller to publish where it sees
    /// fit (with the range it is given here, the file's start).
    pub(crate) fn diagnostic(&mut self, diag: &Diagnostic) -> (Option<PathBuf>, Published) {
        let sources = self.sources;
        let primary = diag.labels.iter().position(|l| l.primary).or((!diag.labels.is_empty()).then_some(0));
        let primary_span = primary.map(|i| diag.labels[i].span);
        let generated = primary_span.is_some_and(|s| s.file.expansion_index().is_some());
        let shown = primary_span.map(|s| if generated { wid_query::written(sources, s).0 } else { s });
        let chain = diag.chain_span().map(|s| sources.expansion_chain(s)).unwrap_or_default();

        let mut message = diag.message.clone();
        let mut related = Vec::new();
        if let Some(i) = primary {
            let label = &diag.labels[i];
            if generated {
                let text = if label.message.is_empty() { diag.message.clone() } else { label.message.clone() };
                if let Some(location) = self.location(label.span) {
                    related.push(DiagnosticRelatedInformation { location, message: text });
                }
            } else if !label.message.is_empty() {
                message.push('\n');
                message.push_str(&label.message);
            }
        }
        if generated && let Some(outermost) = chain.last() {
            message.push_str(&format!("\nin the code `{}` generates", outermost.name));
        }
        for note in &diag.notes {
            message.push_str(&format!("\nnote: {note}"));
        }
        for help in &diag.helps {
            message.push_str(&format!("\nhelp: {}", help.message));
        }
        for (i, label) in diag.labels.iter().enumerate() {
            if Some(i) == primary {
                continue;
            }
            let text = if label.message.is_empty() { "related code".to_string() } else { label.message.clone() };
            if let Some(location) = self.location(label.span) {
                related.push(DiagnosticRelatedInformation { location, message: text });
            }
        }
        for expansion in &chain {
            if Some(expansion.call_site) == shown {
                continue;
            }
            if let Some(location) = self.location(expansion.call_site) {
                related.push(DiagnosticRelatedInformation {
                    location,
                    message: format!("`{}` expands here", expansion.name),
                });
            }
        }

        let fixes = diag
            .helps
            .iter()
            .filter(|h| !h.edits.is_empty())
            .filter_map(|help| {
                let edits = help
                    .edits
                    .iter()
                    .map(|e| {
                        self.place(e.span)
                            .map(|(path, range)| (path, TextEdit { range, new_text: e.replacement.clone() }))
                    })
                    .collect::<Option<Vec<_>>>()?;
                Some(Fix {
                    title: help.message.clone(),
                    preferred: help.applicability == Applicability::MachineApplicable,
                    edits,
                })
            })
            .collect();

        let place = shown.and_then(|s| self.place(s));
        let (path, range) = match place {
            Some((path, range)) => (Some(path), range),
            None => (None, Range::default()),
        };
        let code = diag.code.as_str();
        let code_description = self
            .docs
            .map(|dir| dir.join(format!("{code}.md")))
            .filter(|page| page.is_file())
            .and_then(|page| crate::uri::from_path(&page))
            .map(|href| CodeDescription { href });
        let diagnostic = LspDiagnostic {
            range,
            severity: Some(match diag.severity {
                Severity::Error => DiagnosticSeverity::ERROR,
                Severity::Warning => DiagnosticSeverity::WARNING,
            }),
            code: Some(NumberOrString::String(code.to_string())),
            code_description,
            source: Some("wid".into()),
            message,
            related_information: (!related.is_empty()).then_some(related),
            tags: None,
            data: None,
        };
        (path, Published { diagnostic, fixes })
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use lsp_types::{DiagnosticSeverity, NumberOrString, Position, Range, Uri};
    use wid_diagnostics::{Applicability, Diagnostic, Expansion, FileId, SourceMap, Span, codes};

    use super::Converter;
    use crate::position::Encoding;

    fn root() -> PathBuf {
        std::env::temp_dir().join("wid-lsp-convert")
    }

    fn uri(path: &Path) -> Option<Uri> {
        crate::uri::from_path(path)
    }

    fn range(a: (u32, u32), b: (u32, u32)) -> Range {
        Range { start: Position { line: a.0, character: a.1 }, end: Position { line: b.0, character: b.1 } }
    }

    #[test]
    fn labels_notes_helps_and_fixes() {
        let mut sources = SourceMap::new();
        let text = "def f\n  s = \"é😀\"; puts pos\nend\n";
        let main = sources.add(root().join("main.wid"), "main.wid".into(), text);
        let at = |needle: &str| {
            let start = text.find(needle).expect("in the text") as u32;
            Span::new(main, start, start + needle.len() as u32)
        };
        let diag = Diagnostic::error(codes::UNDEFINED_NAME, "undefined name `pos`")
            .primary(at("pos"), "not found in this scope")
            .secondary(at("def f"), "in this method")
            .note("`pos` is a field")
            .suggest_replace("read the field with `@pos`", at("pos"), "@pos", Applicability::MachineApplicable)
            .help("or rename it");
        let to_uri = uri;
        let mut converter = Converter::new(&sources, Encoding::Utf16, None, &to_uri);
        let (path, published) = converter.diagnostic(&diag);
        assert_eq!(path, Some(root().join("main.wid")));
        let d = &published.diagnostic;
        assert_eq!(d.range, range((1, 18), (1, 21)));
        assert_eq!(d.severity, Some(DiagnosticSeverity::ERROR));
        assert_eq!(d.code, Some(NumberOrString::String("E0201".into())));
        assert_eq!(
            d.message,
            "undefined name `pos`\nnot found in this scope\nnote: `pos` is a field\nhelp: read the field with `@pos`\nhelp: or rename it"
        );
        let related = d.related_information.as_ref().expect("the secondary label");
        assert_eq!((related[0].message.as_str(), related[0].location.range), ("in this method", range((0, 0), (0, 5))));
        let [fix] = published.fixes.as_slice() else { panic!("one fix: {:?}", published.fixes) };
        assert!(fix.preferred);
        assert_eq!(fix.edits[0].1.range, range((1, 18), (1, 21)));
        assert_eq!(fix.edits[0].1.new_text, "@pos");
        // In UTF-8, columns count bytes.
        let mut converter = Converter::new(&sources, Encoding::Utf8, None, &to_uri);
        assert_eq!(converter.diagnostic(&diag).1.diagnostic.range, range((1, 21), (1, 24)));
    }

    #[test]
    fn generated_code_is_shown_at_the_macro_call() {
        let mut sources = SourceMap::new();
        let lib = "macro def twice(body: Code)\n  quote do\n    #{body}\n    #{body}\n  end\nend\n";
        let main = "def main\n  twice x\nend\n";
        let lib_id = sources.add(root().join("lib.wid"), "lib.wid".into(), lib);
        let main_id = sources.add(root().join("main.wid"), "main.wid".into(), main);
        let call = Span::new(main_id, 11, 18);
        sources.set_expansions(vec![Expansion { template: lib_id, call_site: call, name: "twice".into() }]);
        let quote = lib.find("#{body}").expect("in the macro") as u32;
        let diag = Diagnostic::error(codes::UNDEFINED_NAME, "undefined name `x`")
            .primary(Span::new(FileId::expansion(0), quote, quote + 7), "not found");
        let to_uri = uri;
        let mut converter = Converter::new(&sources, Encoding::Utf16, None, &to_uri);
        let (path, published) = converter.diagnostic(&diag);
        assert_eq!(path, Some(root().join("main.wid")));
        let d = published.diagnostic;
        assert_eq!(d.range, range((1, 2), (1, 9)));
        assert_eq!(d.message, "undefined name `x`\nin the code `twice` generates");
        let related = d.related_information.expect("the label in the quote");
        assert_eq!(related.len(), 1, "the call is the diagnostic's own range");
        assert_eq!(related[0].message, "not found");
        assert_eq!(related[0].location.range, range((2, 4), (2, 11)));
        assert!(related[0].location.uri.as_str().ends_with("/lib.wid"));
    }

    #[test]
    fn a_diagnostic_off_disk_has_no_file() {
        let mut sources = SourceMap::new();
        let line = sources.add("<command line>".into(), "command line".into(), "wid check x");
        let diag = Diagnostic::error(codes::UNKNOWN_IMPORT, "no such package").primary(Span::new(line, 10, 11), "here");
        let to_uri = uri;
        let mut converter = Converter::new(&sources, Encoding::Utf16, None, &to_uri);
        let (path, published) = converter.diagnostic(&diag);
        assert_eq!(path, None);
        assert_eq!(published.diagnostic.range, Range::default());
        let bare = Diagnostic::warning(codes::UNKNOWN_IMPORT, "nothing to point at");
        let (path, published) = converter.diagnostic(&bare);
        assert_eq!((path, published.diagnostic.severity), (None, Some(DiagnosticSeverity::WARNING)));
    }
}

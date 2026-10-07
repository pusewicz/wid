//! Human-readable and JSON rendering of diagnostics.

use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde_json::{Value, json};

use crate::diagnostic::{Diagnostic, Diagnostics, Edit, Label, Severity};
use crate::source::{FileId, SourceFile, SourceMap, Span};

/// Options controlling human-readable output.
#[derive(Clone, Copy, Debug, Default)]
pub struct RenderOptions {
    /// Emit ANSI colors.
    pub color: bool,
}

struct Palette {
    color: bool,
}

impl Palette {
    fn paint(&self, code: &str, text: &str) -> String {
        if self.color { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() }
    }
    fn severity(&self, sev: Severity, text: &str) -> String {
        match sev {
            Severity::Error => self.paint("1;31", text),
            Severity::Warning => self.paint("1;33", text),
        }
    }
    fn gutter(&self, text: &str) -> String {
        self.paint("1;34", text)
    }
    fn bold(&self, text: &str) -> String {
        self.paint("1", text)
    }
    fn help(&self, text: &str) -> String {
        self.paint("1;36", text)
    }
}

/// Expands tabs to four spaces and returns the visual column for each byte.
fn expand_line(text: &str) -> (String, Vec<usize>) {
    let mut out = String::with_capacity(text.len());
    let mut cols = Vec::with_capacity(text.len() + 1);
    let mut col = 0usize;
    for ch in text.chars() {
        for _ in 0..ch.len_utf8() {
            cols.push(col);
        }
        if ch == '\t' {
            out.push_str("    ");
            col += 4;
        } else {
            out.push(ch);
            col += 1;
        }
    }
    cols.push(col);
    (out, cols)
}

/// Renders one diagnostic in the compiler's human format.
pub fn render(diag: &Diagnostic, sources: &SourceMap, opts: RenderOptions) -> String {
    let p = Palette { color: opts.color };
    let mut out = String::new();
    let sev = diag.severity.as_str();
    let _ = writeln!(
        out,
        "{}{}",
        p.severity(diag.severity, &format!("{sev}[{}]", diag.code)),
        p.bold(&format!(": {}", diag.message))
    );

    let mut max_line = 1u32;
    for label in &diag.labels {
        let file = sources.file(label.span.file);
        max_line = max_line.max(file.line_col(label.span.end).0);
    }
    for help in &diag.helps {
        for edit in &help.edits {
            let file = sources.file(edit.span.file);
            max_line = max_line.max(file.line_col(edit.span.end).0 + 1);
        }
    }
    let width = max_line.to_string().len();
    let pad = " ".repeat(width);

    let primary = diag.primary_span();
    let mut by_file: BTreeMap<(bool, FileId), Vec<&Label>> = BTreeMap::new();
    for label in &diag.labels {
        let is_other = Some(label.span.file) != primary.map(|s| s.file);
        by_file.entry((is_other, label.span.file)).or_default().push(label);
    }

    let mut first_file = true;
    for ((_, file_id), labels) in &by_file {
        let file = sources.file(*file_id);
        let anchor = labels.iter().find(|l| l.primary).unwrap_or(&labels[0]);
        let (line, col) = file.line_col(anchor.span.start);
        let arrow = if first_file { "-->" } else { ":::" };
        let _ = writeln!(out, "{pad}{} {}:{line}:{col}", p.gutter(arrow), file.display);
        let _ = writeln!(out, "{pad} {}", p.gutter("|"));
        render_labels(&mut out, &p, file, labels, &pad, width);
        first_file = false;
    }

    for note in &diag.notes {
        let indent = format!("{pad}   {}", " ".repeat("note: ".len()));
        let _ = writeln!(out, "{pad} {} {}", p.gutter("="), indent_continuation(&note_text(note), &indent));
    }
    for help in &diag.helps {
        let indent = " ".repeat("help: ".len());
        let _ = writeln!(out, "{}: {}", p.help("help"), indent_continuation(&help.message, &indent));
        if !help.edits.is_empty() {
            render_edits(&mut out, &p, sources, &help.edits, &pad);
        }
    }
    if diag.severity == Severity::Error {
        let _ = writeln!(out, "{pad} {} see `wid explain {}`", p.gutter("="), diag.code);
    }
    out
}

/// Indents every line after the first, so multi-line notes (like C compiler
/// output) stay inside the diagnostic.
fn indent_continuation(text: &str, indent: &str) -> String {
    let mut lines = text.lines();
    let mut out = lines.next().unwrap_or_default().to_string();
    for line in lines {
        out.push('\n');
        if !line.is_empty() {
            out.push_str(indent);
            out.push_str(line);
        }
    }
    out
}

fn note_text(note: &str) -> String {
    format!("note: {note}")
}

fn render_labels(out: &mut String, p: &Palette, file: &SourceFile, labels: &[&Label], pad: &str, width: usize) {
    let mut lines: BTreeMap<usize, Vec<&Label>> = BTreeMap::new();
    for label in labels {
        lines.entry(file.line_index(label.span.start)).or_default().push(label);
    }
    let mut prev: Option<usize> = None;
    for (line_idx, mut line_labels) in lines {
        if let Some(prev) = prev
            && line_idx > prev + 1
        {
            let _ = writeln!(out, "{}", p.gutter("..."));
        }
        prev = Some(line_idx);
        let raw = file.line_text_by_index(line_idx);
        let (text, cols) = expand_line(raw);
        let _ = writeln!(out, "{} {}", p.gutter(&format!("{:>width$} |", line_idx + 1)), text.trim_end());
        line_labels.sort_by_key(|l| (!l.primary, l.span.start));
        let line_start = file.line_start(line_idx);
        for label in line_labels {
            let start_byte = (label.span.start - line_start) as usize;
            let end_in_line = label.span.end.min(line_start + raw.len() as u32);
            let end_byte = (end_in_line.saturating_sub(line_start) as usize).max(start_byte);
            let start_col = cols.get(start_byte).copied().unwrap_or(text.len());
            let end_col = cols.get(end_byte).copied().unwrap_or(text.len());
            let len = (end_col.saturating_sub(start_col)).max(1);
            let mark = if label.primary { "^" } else { "-" }.repeat(len);
            let mark = if label.primary { p.paint("1;31", &mark) } else { p.paint("1;34", &mark) };
            let message = if label.message.is_empty() { String::new() } else { format!(" {}", label.message) };
            let _ = writeln!(out, "{pad} {} {}{mark}{message}", p.gutter("|"), " ".repeat(start_col));
        }
    }
}

fn render_edits(out: &mut String, p: &Palette, sources: &SourceMap, edits: &[Edit], pad: &str) {
    let mut by_file: BTreeMap<FileId, Vec<&Edit>> = BTreeMap::new();
    for edit in edits {
        by_file.entry(edit.span.file).or_default().push(edit);
    }
    for (file_id, mut edits) in by_file {
        let file = sources.file(file_id);
        edits.sort_by_key(|e| (e.span.start, e.span.end));
        let first_line = file.line_index(edits[0].span.start);
        let last_line = edits.iter().map(|e| file.line_index(e.span.end)).max().unwrap_or(first_line);
        let start = file.line_start(first_line) as usize;
        let end =
            if last_line + 1 < file.line_count() { file.line_start(last_line + 1) as usize } else { file.text.len() };
        let mut text = file.text[start..end].to_string();
        for edit in edits.iter().rev() {
            let s = edit.span.start as usize - start;
            let e = edit.span.end as usize - start;
            if s <= e && e <= text.len() {
                text.replace_range(s..e, &edit.replacement);
            }
        }
        for line in text.trim_end_matches('\n').lines() {
            let (expanded, _) = expand_line(line);
            let _ = writeln!(out, "{pad} {} {}", p.gutter("|"), expanded.trim_end());
        }
    }
}

/// Renders every diagnostic followed by a summary line.
pub fn render_all(diags: &Diagnostics, sources: &SourceMap, opts: RenderOptions) -> String {
    let mut out = String::new();
    for diag in diags.iter() {
        out.push_str(&render(diag, sources, opts));
        out.push('\n');
    }
    let errors = diags.error_count();
    let warnings = diags.warning_count();
    if errors > 0 || warnings > 0 {
        let p = Palette { color: opts.color };
        let plural = |n: usize, word: &str| {
            if n == 1 { format!("{n} {word}") } else { format!("{n} {word}s") }
        };
        let summary = match (errors, warnings) {
            (0, w) => format!("{} emitted", plural(w, "warning")),
            (e, 0) => format!("could not compile due to {}", plural(e, "error")),
            (e, w) => format!("could not compile due to {} and {}", plural(e, "error"), plural(w, "warning")),
        };
        let sev = if errors > 0 { Severity::Error } else { Severity::Warning };
        let _ = writeln!(out, "{}: {}", p.severity(sev, sev.as_str()), p.bold(&summary));
    }
    out
}

fn span_json(sources: &SourceMap, span: Span) -> serde_json::Map<String, Value> {
    let file = sources.file(span.file);
    let (line, column) = file.line_col(span.start);
    let (end_line, end_column) = file.line_col(span.end);
    let mut map = serde_json::Map::new();
    map.insert("file".into(), json!(file.display));
    map.insert("line".into(), json!(line));
    map.insert("column".into(), json!(column));
    map.insert("end_line".into(), json!(end_line));
    map.insert("end_column".into(), json!(end_column));
    map.insert("start".into(), json!(span.start));
    map.insert("end".into(), json!(span.end));
    map
}

/// Converts one diagnostic to a JSON value with resolved positions.
pub fn to_json(diag: &Diagnostic, sources: &SourceMap) -> Value {
    let labels: Vec<Value> = diag
        .labels
        .iter()
        .map(|l| {
            let mut m = span_json(sources, l.span);
            m.insert("message".into(), json!(l.message));
            m.insert("primary".into(), json!(l.primary));
            Value::Object(m)
        })
        .collect();
    let helps: Vec<Value> = diag
        .helps
        .iter()
        .map(|h| {
            let edits: Vec<Value> = h
                .edits
                .iter()
                .map(|e| {
                    let mut m = span_json(sources, e.span);
                    m.insert("replacement".into(), json!(e.replacement));
                    Value::Object(m)
                })
                .collect();
            json!({
                "message": h.message,
                "applicability": h.applicability.as_str(),
                "edits": edits,
            })
        })
        .collect();
    let mut obj = serde_json::Map::new();
    obj.insert("severity".into(), json!(diag.severity.as_str()));
    obj.insert("code".into(), json!(diag.code.as_str()));
    obj.insert("message".into(), json!(diag.message));
    if let Some(span) = diag.primary_span() {
        let pos = span_json(sources, span);
        for key in ["file", "line", "column"] {
            obj.insert(key.into(), pos[key].clone());
        }
    }
    obj.insert("labels".into(), Value::Array(labels));
    obj.insert("notes".into(), json!(diag.notes));
    obj.insert("helps".into(), Value::Array(helps));
    obj.insert("explain".into(), json!(format!("wid explain {}", diag.code)));
    Value::Object(obj)
}

/// Renders every diagnostic as one JSON document.
pub fn render_json(diags: &Diagnostics, sources: &SourceMap) -> String {
    let list: Vec<Value> = diags.iter().map(|d| to_json(d, sources)).collect();
    let doc = json!({
        "errors": diags.error_count(),
        "warnings": diags.warning_count(),
        "diagnostics": list,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

//! Source files, file ids and byte spans.

use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Identifies a file registered in a [`SourceMap`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord, Default)]
pub struct FileId(pub u32);

/// A half-open byte range `start..end` inside one file.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default, PartialOrd, Ord)]
pub struct Span {
    /// The file the range belongs to.
    pub file: FileId,
    /// Byte offset of the first character.
    pub start: u32,
    /// Byte offset one past the last character.
    pub end: u32,
}

impl Span {
    /// Creates a span covering `start..end` in `file`.
    pub fn new(file: FileId, start: u32, end: u32) -> Self {
        Span { file, start, end }
    }

    /// Returns the smallest span covering both `self` and `other`.
    pub fn to(self, other: Span) -> Span {
        if self.file != other.file {
            return self;
        }
        Span { file: self.file, start: self.start.min(other.start), end: self.end.max(other.end) }
    }

    /// Returns an empty span positioned at the start of `self`.
    pub fn shrink_to_start(self) -> Span {
        Span { file: self.file, start: self.start, end: self.start }
    }

    /// Returns an empty span positioned at the end of `self`.
    pub fn shrink_to_end(self) -> Span {
        Span { file: self.file, start: self.end, end: self.end }
    }

    /// Returns the length of the span in bytes.
    pub fn len(self) -> u32 {
        self.end.saturating_sub(self.start)
    }

    /// Returns true when the span covers no bytes.
    pub fn is_empty(self) -> bool {
        self.end <= self.start
    }

    /// Returns true when `offset` lies within the span (inclusive of the end).
    pub fn contains(self, offset: u32) -> bool {
        self.start <= offset && offset <= self.end
    }
}

/// A loaded source file with a precomputed line index.
#[derive(Debug)]
pub struct SourceFile {
    /// The id of this file.
    pub id: FileId,
    /// The path on disk (may be synthetic for in-memory sources).
    pub path: PathBuf,
    /// The path shown in diagnostics.
    pub display: String,
    /// The full file contents.
    pub text: Arc<str>,
    line_starts: Vec<u32>,
}

impl SourceFile {
    fn new(id: FileId, path: PathBuf, display: String, text: Arc<str>) -> Self {
        let mut line_starts = vec![0];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        SourceFile { id, path, display, text, line_starts }
    }

    /// Returns the 0-based line index containing byte `offset`.
    pub fn line_index(&self, offset: u32) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    /// Returns the 1-based line and 1-based character column of `offset`.
    pub fn line_col(&self, offset: u32) -> (u32, u32) {
        let line = self.line_index(offset);
        let start = self.line_starts[line] as usize;
        let end = (offset as usize).min(self.text.len());
        let col = self.text.get(start..end).map_or(0, |s| s.chars().count());
        (line as u32 + 1, col as u32 + 1)
    }

    /// Converts a 1-based line and 1-based character column, as `line_col`
    /// returns them, back into a byte offset.
    pub fn offset_of(&self, line: u32, col: u32) -> Option<u32> {
        let index = line.checked_sub(1)? as usize;
        let start = *self.line_starts.get(index)?;
        let text = self.line_text_by_index(index);
        let skip = col.saturating_sub(1) as usize;
        let within = text.char_indices().nth(skip).map_or(text.len(), |(i, _)| i);
        Some(start + within as u32)
    }

    /// Returns the 0-based line and UTF-16 column of `offset`, as used by LSP.
    pub fn line_col_utf16(&self, offset: u32) -> (u32, u32) {
        let line = self.line_index(offset);
        let start = self.line_starts[line] as usize;
        let end = (offset as usize).min(self.text.len());
        let col = self.text.get(start..end).map_or(0, |s| s.encode_utf16().count());
        (line as u32, col as u32)
    }

    /// Converts a 0-based line and UTF-16 column back into a byte offset.
    pub fn offset_of_utf16(&self, line: u32, col: u32) -> u32 {
        let Some(&start) = self.line_starts.get(line as usize) else {
            return self.text.len() as u32;
        };
        let text = self.line_text_by_index(line as usize);
        let mut units = 0u32;
        for (i, ch) in text.char_indices() {
            if units >= col {
                return start + i as u32;
            }
            units += ch.len_utf16() as u32;
        }
        start + text.len() as u32
    }

    /// Returns the byte offset where the 0-based line `index` starts.
    pub fn line_start(&self, index: usize) -> u32 {
        self.line_starts.get(index).copied().unwrap_or(self.text.len() as u32)
    }

    /// Returns the number of lines in the file.
    pub fn line_count(&self) -> usize {
        self.line_starts.len()
    }

    /// Returns the text of the 0-based line `index` without its newline.
    pub fn line_text_by_index(&self, index: usize) -> &str {
        let start = self.line_start(index) as usize;
        let end = self.line_starts.get(index + 1).map_or(self.text.len(), |&e| e as usize);
        self.text[start..end].trim_end_matches(['\n', '\r'])
    }

    /// Returns the source text covered by `span`.
    pub fn slice(&self, span: Span) -> &str {
        self.text.get(span.start as usize..span.end as usize).unwrap_or("")
    }
}

/// Owns every source file seen by one compilation.
#[derive(Debug, Default)]
pub struct SourceMap {
    files: Vec<SourceFile>,
}

impl SourceMap {
    /// Creates an empty source map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a file and returns its id.
    pub fn add(&mut self, path: PathBuf, display: String, text: impl Into<Arc<str>>) -> FileId {
        let id = FileId(self.files.len() as u32);
        self.files.push(SourceFile::new(id, path, display, text.into()));
        id
    }

    /// Returns the file with the given id.
    pub fn file(&self, id: FileId) -> &SourceFile {
        &self.files[id.0 as usize]
    }

    /// Returns every registered file.
    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    /// Finds a registered file by the name diagnostics show for it.
    pub fn find_by_display(&self, display: &str) -> Option<FileId> {
        self.files.iter().find(|f| f.display == display).map(|f| f.id)
    }

    /// Finds a registered file by its path on disk.
    pub fn find_by_path(&self, path: &Path) -> Option<FileId> {
        self.files.iter().find(|f| f.path == path).map(|f| f.id)
    }

    /// Returns the source text covered by `span`.
    pub fn slice(&self, span: Span) -> &str {
        self.file(span.file).slice(span)
    }
}

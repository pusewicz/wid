//! Which files belong to the import, the order the preprocessor saw them in,
//! and the comments next to declarations.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::ffi::Spot;

/// Facts about the files of one translation unit.
pub(crate) struct Sources {
    /// The canonical directory whose headers are imported.
    root: PathBuf,
    /// Whether only the headers directly in `root` are imported, not those in
    /// its subdirectories: `root` is shared by many libraries.
    flat: bool,
    /// The wrapper file that includes the requested header.
    main_file: String,
    /// For each file, the file that first included it and the offset of the
    /// `#include`.
    parents: HashMap<String, (String, u32)>,
    /// Memoised membership answers, keyed by libclang's file name.
    owned: HashMap<String, bool>,
    /// Memoised order prefixes, keyed by libclang's file name.
    prefixes: HashMap<String, Vec<u32>>,
    /// File contents read for comment extraction.
    texts: HashMap<String, Option<Vec<u8>>>,
}

impl Sources {
    /// Creates the view for a unit whose main file is `main_file`, whose
    /// imported directory is `root` (only its top level when `flat`) and whose
    /// inclusions are `(includer, offset, included)` triples in
    /// preprocessing order.
    pub fn new(root: PathBuf, flat: bool, main_file: String, inclusions: Vec<(String, u32, String)>) -> Sources {
        let mut parents = HashMap::new();
        for (includer, offset, included) in inclusions {
            if included != main_file {
                parents.entry(included).or_insert((includer, offset));
            }
        }
        Sources {
            root,
            flat,
            main_file,
            parents,
            owned: HashMap::new(),
            prefixes: HashMap::new(),
            texts: HashMap::new(),
        }
    }

    /// The canonical directory whose headers are imported.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Whether `file` belongs to the import.
    pub fn is_owned(&mut self, file: &str) -> bool {
        if file == self.main_file {
            return false;
        }
        if let Some(&owned) = self.owned.get(file) {
            return owned;
        }
        let owned = std::fs::canonicalize(file).is_ok_and(|path| {
            if self.flat { path.parent() == Some(self.root.as_path()) } else { path.starts_with(&self.root) }
        });
        self.owned.insert(file.to_string(), owned);
        owned
    }

    /// Whether the position is inside the imported directory tree.
    pub fn is_owned_spot(&mut self, spot: &Spot) -> bool {
        spot.file.as_deref().is_some_and(|file| self.is_owned(file))
    }

    /// A key that sorts positions in the order the preprocessor reaches them,
    /// across files: the offsets of the `#include` chain, then the offset.
    pub fn order_key(&mut self, spot: &Spot) -> Vec<u32> {
        let mut key = match &spot.file {
            Some(file) => self.prefix(file, 0),
            None => vec![u32::MAX],
        };
        key.push(spot.offset);
        key
    }

    /// The order key of the `#include` that first brought in `file`.
    fn prefix(&mut self, file: &str, depth: usize) -> Vec<u32> {
        if file == self.main_file {
            return Vec::new();
        }
        if let Some(prefix) = self.prefixes.get(file) {
            return prefix.clone();
        }
        let prefix = match self.parents.get(file).cloned() {
            Some((parent, offset)) if depth < 256 => {
                let mut prefix = self.prefix(&parent, depth + 1);
                prefix.push(offset);
                prefix
            }
            _ => vec![u32::MAX],
        };
        self.prefixes.insert(file.to_string(), prefix.clone());
        prefix
    }

    /// The contents of `file`, read from disk once.
    pub fn text(&mut self, file: &str) -> Option<&[u8]> {
        if !self.texts.contains_key(file) {
            self.texts.insert(file.to_string(), std::fs::read(file).ok());
        }
        self.texts.get(file)?.as_deref()
    }
}

/// Returns the comment that follows a declaration on the line where it ends,
/// skipping the `;` or `,` that terminates it.
pub(crate) fn trailing_comment(text: &[u8], end: usize) -> Option<String> {
    let mut at = end;
    while at < text.len() && matches!(text[at], b' ' | b'\t' | b';' | b',') {
        at += 1;
    }
    let rest = text.get(at..)?;
    if rest.starts_with(b"//") {
        let len = rest.iter().position(|&byte| byte == b'\n').unwrap_or(rest.len());
        return Some(String::from_utf8_lossy(&rest[..len]).trim_end().to_string());
    }
    if rest.starts_with(b"/*") {
        let close = find(rest, b"*/")?;
        return Some(String::from_utf8_lossy(&rest[..close + 2]).into_owned());
    }
    None
}

/// Returns the documentation comment (`/** … */`, `/*! … */`, or a run of
/// `///` or `//!` lines) that ends right before the line containing `start`.
pub(crate) fn preceding_doc_comment(text: &[u8], start: usize) -> Option<String> {
    let line_start = text[..start.min(text.len())].iter().rposition(|&byte| byte == b'\n').map_or(0, |at| at + 1);
    let mut end = line_start;
    while end > 0 && text[end - 1].is_ascii_whitespace() {
        end -= 1;
    }
    let before = &text[..end];
    if before.ends_with(b"*/") {
        let open = rfind(&before[..before.len() - 2], b"/*")?;
        let comment = &before[open..];
        let is_doc = (comment.starts_with(b"/**") && !comment.starts_with(b"/**/")) || comment.starts_with(b"/*!");
        return (is_doc && starts_line(text, open)).then(|| String::from_utf8_lossy(comment).into_owned());
    }
    let mut lines: Vec<&[u8]> = Vec::new();
    let mut cursor = end;
    while cursor > 0 {
        let begin = text[..cursor].iter().rposition(|&byte| byte == b'\n').map_or(0, |at| at + 1);
        let line = trim_start(&text[begin..cursor]);
        if !(line.starts_with(b"///") || line.starts_with(b"//!")) || line.starts_with(b"////") {
            break;
        }
        lines.push(line);
        cursor = begin.saturating_sub(1);
        if begin == 0 {
            break;
        }
    }
    if lines.is_empty() {
        return None;
    }
    lines.reverse();
    let joined: Vec<String> = lines.iter().map(|line| String::from_utf8_lossy(line).trim_end().to_string()).collect();
    Some(joined.join("\n"))
}

/// Whether only whitespace precedes `at` on its line.
fn starts_line(text: &[u8], at: usize) -> bool {
    text[..at].iter().rev().take_while(|&&byte| byte != b'\n').all(|byte| byte.is_ascii_whitespace())
}

/// `line` without leading spaces and tabs.
fn trim_start(line: &[u8]) -> &[u8] {
    let skip = line.iter().take_while(|byte| matches!(byte, b' ' | b'\t')).count();
    &line[skip..]
}

/// The first index of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|window| window == needle)
}

/// The last index of `needle` in `haystack`.
fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).rposition(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trailing_comments_skip_terminators() {
        let text = b"void f(void);  // Does f\nint x;";
        assert_eq!(trailing_comment(text, 12).as_deref(), Some("// Does f"));
        let text = b"A = 1, /* one */ B";
        assert_eq!(trailing_comment(text, 5).as_deref(), Some("/* one */"));
        assert_eq!(trailing_comment(b"int a, b; // b", 5), None);
    }

    #[test]
    fn preceding_doc_comments() {
        let text = b"/** The answer. */\n#define ANSWER 42\n";
        assert_eq!(preceding_doc_comment(text, 27).as_deref(), Some("/** The answer. */"));
        let text = b"/// One.\n/// Two.\n#define X 1\n";
        assert_eq!(preceding_doc_comment(text, 26).as_deref(), Some("/// One.\n/// Two."));
        let text = b"// Plain.\n#define X 1\n";
        assert_eq!(preceding_doc_comment(text, 18), None);
        let text = b"int y; /** trailing */\n#define X 1\n";
        assert_eq!(preceding_doc_comment(text, 31), None);
    }
}

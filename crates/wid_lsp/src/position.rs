//! Positions. The compiler counts byte offsets into a file; LSP counts lines
//! from 0 and columns in UTF-16 code units, or in UTF-8 bytes when the
//! client offers `positionEncoding: "utf-8"` (LSP 3.17).

use lsp_types::{Position, Range};

/// How LSP columns count.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Encoding {
    /// UTF-8 code units: bytes.
    Utf8,
    /// UTF-16 code units, LSP's default: a character outside the Basic
    /// Multilingual Plane (an emoji) counts two.
    Utf16,
}

/// Where each line of a text starts, to convert between byte offsets and
/// LSP positions. A line ends after `\n`, `\r\n` or a lone `\r`, as LSP
/// counts them.
pub(crate) struct LineIndex<'t> {
    text: &'t str,
    starts: Vec<usize>,
}

impl<'t> LineIndex<'t> {
    /// Indexes `text`.
    pub(crate) fn new(text: &'t str) -> Self {
        let bytes = text.as_bytes();
        let mut starts = vec![0];
        for (i, &b) in bytes.iter().enumerate() {
            if b == b'\n' || (b == b'\r' && bytes.get(i + 1) != Some(&b'\n')) {
                starts.push(i + 1);
            }
        }
        LineIndex { text, starts }
    }

    /// The text of the 0-based line `line`, without its line break.
    fn line(&self, line: usize) -> &'t str {
        let start = self.starts[line];
        let end = self.starts.get(line + 1).copied().unwrap_or(self.text.len());
        let text = &self.text[start..end];
        text.strip_suffix("\r\n").or_else(|| text.strip_suffix(['\n', '\r'])).unwrap_or(text)
    }

    /// The position of byte `offset`, clamped to the text and moved back
    /// to the start of the character it falls in.
    pub(crate) fn position(&self, offset: usize, encoding: Encoding) -> Position {
        let mut offset = offset.min(self.text.len());
        while !self.text.is_char_boundary(offset) {
            offset -= 1;
        }
        let line = match self.starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        };
        let before = &self.text[self.starts[line]..offset];
        let character = match encoding {
            Encoding::Utf8 => before.len(),
            Encoding::Utf16 => before.encode_utf16().count(),
        };
        Position { line: line as u32, character: character as u32 }
    }

    /// The range of bytes `start..end`.
    pub(crate) fn range(&self, start: usize, end: usize, encoding: Encoding) -> Range {
        Range { start: self.position(start, encoding), end: self.position(end, encoding) }
    }

    /// The byte offset of a position. A line past the end is the end of
    /// the text, a column past the end of its line is the line's end, and
    /// a column inside a character (a UTF-16 surrogate pair, or the bytes
    /// of one UTF-8 character) is that character's start.
    pub(crate) fn offset(&self, position: Position, encoding: Encoding) -> usize {
        let line = position.line as usize;
        let Some(&start) = self.starts.get(line) else { return self.text.len() };
        let text = self.line(line);
        let wanted = position.character as usize;
        match encoding {
            Encoding::Utf8 => {
                let mut at = wanted.min(text.len());
                while !text.is_char_boundary(at) {
                    at -= 1;
                }
                start + at
            }
            Encoding::Utf16 => {
                let mut units = 0;
                for (i, ch) in text.char_indices() {
                    units += ch.len_utf16();
                    if units > wanted {
                        return start + i;
                    }
                }
                start + text.len()
            }
        }
    }

    /// The position just past the last character.
    pub(crate) fn end(&self, encoding: Encoding) -> Position {
        self.position(self.text.len(), encoding)
    }
}

#[cfg(test)]
mod tests {
    use lsp_types::Position;

    use super::{Encoding, LineIndex};

    fn at(line: u32, character: u32) -> Position {
        Position { line, character }
    }

    /// `é` is two bytes and one UTF-16 unit, `😀` four bytes and two units.
    const TEXT: &str = "def main\n  puts \"é😀\", x # ok\nend\n";

    #[test]
    fn offsets_become_positions_in_either_encoding() {
        let index = LineIndex::new(TEXT);
        let x = TEXT.find(", x").map(|i| i + 2).expect("x is in the text");
        assert_eq!(index.position(x, Encoding::Utf16), at(1, 14));
        assert_eq!(index.position(x, Encoding::Utf8), at(1, 17));
        assert_eq!(index.position(0, Encoding::Utf16), at(0, 0));
        assert_eq!(index.position(TEXT.len(), Encoding::Utf16), at(3, 0));
        assert_eq!(index.end(Encoding::Utf8), at(3, 0));
        // An offset inside `😀` is its start.
        let emoji = TEXT.find('😀').expect("the emoji is in the text");
        assert_eq!(index.position(emoji + 2, Encoding::Utf16), at(1, 9));
        assert_eq!(index.position(emoji + 2, Encoding::Utf8), at(1, 10));
    }

    #[test]
    fn positions_become_offsets_in_either_encoding() {
        let index = LineIndex::new(TEXT);
        let x = TEXT.find(", x").map(|i| i + 2).expect("x is in the text");
        assert_eq!(index.offset(at(1, 14), Encoding::Utf16), x);
        assert_eq!(index.offset(at(1, 17), Encoding::Utf8), x);
        let emoji = TEXT.find('😀').expect("the emoji is in the text");
        // Between the two halves of the surrogate pair: the emoji's start.
        assert_eq!(index.offset(at(1, 10), Encoding::Utf16), emoji);
        assert_eq!(index.offset(at(1, 11), Encoding::Utf16), emoji + 4);
        // Inside the emoji's bytes: its start.
        assert_eq!(index.offset(at(1, 12), Encoding::Utf8), emoji);
        // Past the end of a line, of the text.
        assert_eq!(index.offset(at(0, 99), Encoding::Utf16), "def main".len());
        assert_eq!(index.offset(at(9, 0), Encoding::Utf8), TEXT.len());
    }

    #[test]
    fn every_offset_round_trips() {
        for text in [TEXT, "a\r\nb\rc\n\n", "😀😀\n", ""] {
            let index = LineIndex::new(text);
            for (offset, _) in text.char_indices().chain([(text.len(), ' ')]) {
                for encoding in [Encoding::Utf8, Encoding::Utf16] {
                    let position = index.position(offset, encoding);
                    let back = index.offset(position, encoding);
                    // A line break's `\n` after `\r` is part of the break.
                    let expected = if text[..offset].ends_with('\r') && text[offset..].starts_with('\n') {
                        offset - 1
                    } else {
                        offset
                    };
                    assert_eq!(back, expected, "{text:?} at {offset} ({encoding:?})");
                }
            }
        }
    }

    #[test]
    fn every_line_break_counts() {
        let index = LineIndex::new("a\r\nb\rc\nd");
        assert_eq!(index.position(3, Encoding::Utf16), at(1, 0));
        assert_eq!(index.position(5, Encoding::Utf16), at(2, 0));
        assert_eq!(index.position(7, Encoding::Utf16), at(3, 0));
        assert_eq!(index.offset(at(0, 5), Encoding::Utf8), 1);
    }
}

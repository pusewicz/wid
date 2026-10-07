//! Turns source text into tokens, cooked strings and comments.

use wid_diagnostics::{Applicability, Diagnostic, Diagnostics, Edit, FileId, Span, codes};

use crate::token::{Comment, Keyword, Token, TokenKind};

/// The output of lexing one file.
#[derive(Debug, Default)]
pub struct Lexed {
    /// Tokens, always ending with [`TokenKind::Eof`].
    pub tokens: Vec<Token>,
    /// Cooked string contents referenced by `Str` and `StrText` tokens.
    pub strings: Vec<String>,
    /// Every comment in source order.
    pub comments: Vec<Comment>,
    /// Lexical errors.
    pub diagnostics: Diagnostics,
}

/// An open `#{`: a string interpolation, or a splice outside a string.
/// Both end at the `}` that matches it, and newlines inside either are
/// ignored, so a splice is one expression even when it spans lines.
struct Interp {
    /// How many `{` are open inside it.
    depth: u32,
    /// The quote of the string being interpolated, or `None` for a splice.
    quote: Option<u8>,
    /// Where a missing `}` is assumed: for an interpolation that is never
    /// closed on its line (already reported), or a splice that is never
    /// closed at all (reported by the parser, which knows whether the
    /// splice is inside a `quote`).
    virtual_close: Option<usize>,
}

struct Lexer<'a> {
    src: &'a [u8],
    text: &'a str,
    file: FileId,
    pos: usize,
    out: Lexed,
    interps: Vec<Interp>,
    space_before: bool,
}

/// Lexes `text`, which belongs to `file`.
pub fn lex(file: FileId, text: &str) -> Lexed {
    let mut lexer = Lexer {
        src: text.as_bytes(),
        text,
        file,
        pos: 0,
        out: Lexed::default(),
        interps: Vec::new(),
        space_before: true,
    };
    lexer.run();
    lexer.out
}

fn is_ident_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

fn is_ident_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

impl<'a> Lexer<'a> {
    fn peek(&self) -> u8 {
        self.src.get(self.pos).copied().unwrap_or(0)
    }

    fn peek_at(&self, n: usize) -> u8 {
        self.src.get(self.pos + n).copied().unwrap_or(0)
    }

    fn span(&self, start: usize, end: usize) -> Span {
        Span::new(self.file, start as u32, end as u32)
    }

    fn push(&mut self, kind: TokenKind, start: usize, end: usize) {
        if kind == TokenKind::Newline {
            match self.out.tokens.last() {
                None => return,
                Some(t) if t.kind.continues_line() => return,
                _ => {}
            }
            if !self.interps.is_empty() {
                return;
            }
        }
        let space_before = self.space_before;
        self.out.tokens.push(Token { kind, span: self.span(start, end), space_before });
        self.space_before = false;
    }

    fn error(&mut self, diag: Diagnostic) {
        self.out.diagnostics.push(diag);
    }

    fn run(&mut self) {
        while self.pos < self.src.len() {
            self.lex_one();
        }
        let end = self.src.len();
        let mut reported = false;
        while let Some(open) = self.interps.pop() {
            if open.quote.is_none() {
                self.push(TokenKind::SpliceEnd, end, end);
            } else if !reported {
                reported = true;
                self.error(
                    Diagnostic::error(codes::UNTERMINATED_STRING, "unterminated string interpolation")
                        .primary(self.span(end, end), "expected `}` to close `#{`"),
                );
            }
        }
        self.push(TokenKind::Newline, end, end);
        self.out.tokens.push(Token { kind: TokenKind::Eof, span: self.span(end, end), space_before: true });
    }

    /// Returns true when the next non-blank line starts with `.method` or
    /// `&.`, which continues the previous expression.
    fn next_line_continues(&self) -> bool {
        let mut i = self.pos;
        loop {
            while i < self.src.len() && matches!(self.src[i], b' ' | b'\t' | b'\r' | b'\n') {
                i += 1;
            }
            if i < self.src.len() && self.src[i] == b'#' && self.src.get(i + 1) != Some(&b'{') {
                while i < self.src.len() && self.src[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            break;
        }
        let at = |n: usize| self.src.get(i + n).copied().unwrap_or(0);
        (at(0) == b'.' && at(1) != b'.') || (at(0) == b'&' && at(1) == b'.')
    }

    fn lex_one(&mut self) {
        let start = self.pos;
        if let Some(top) = self.interps.last()
            && (top.depth == 0 || top.quote.is_none())
            && top.virtual_close.is_some_and(|at| start >= at)
        {
            let (quote, at) = (top.quote, top.virtual_close.unwrap_or(start));
            self.interps.pop();
            match quote {
                Some(quote) => {
                    self.push(TokenKind::InterpEnd, start, start);
                    self.continue_string(quote, start, false);
                }
                // A zero-width `SpliceEnd` tells the parser the `}` is missing.
                None => self.push(TokenKind::SpliceEnd, at, at),
            }
            return;
        }
        let b = self.peek();
        match b {
            b' ' | b'\t' | b'\r' => {
                self.pos += 1;
                self.space_before = true;
            }
            b'\\' if self.peek_at(1) == b'\n' => {
                self.pos += 2;
                self.space_before = true;
            }
            b'\\' if self.peek_at(1) == b'\r' && self.peek_at(2) == b'\n' => {
                self.pos += 3;
                self.space_before = true;
            }
            b'\n' | b';' => {
                self.pos += 1;
                if b == b';' || !self.next_line_continues() {
                    self.push(TokenKind::Newline, start, start + 1);
                }
                self.space_before = true;
            }
            b'#' if self.peek_at(1) == b'{' => self.begin_splice(),
            b'#' => self.lex_comment(),
            b'"' | b'\'' => self.lex_string(b),
            b'0'..=b'9' => self.lex_number(),
            b'@' => {
                if self.peek_at(1) == b'[' {
                    self.pos += 2;
                    self.push(TokenKind::AtBracket, start, self.pos);
                } else if self.peek_at(1) == b'#' && self.peek_at(2) == b'{' {
                    self.pos += 1;
                    self.push(TokenKind::AtSplice, start, self.pos);
                } else if is_ident_start(self.peek_at(1)) {
                    self.pos += 1;
                    self.eat_ident_chars();
                    self.push(TokenKind::IVar, start, self.pos);
                } else {
                    self.pos += 1;
                    self.unexpected(start, "`@` must be followed by a field name or `[`");
                }
            }
            b'$' => {
                if is_ident_start(self.peek_at(1)) {
                    self.pos += 1;
                    self.eat_ident_chars();
                    self.push(TokenKind::TypeParam, start, self.pos);
                } else {
                    self.pos += 1;
                    self.unexpected(start, "`$` must be followed by a type parameter name like `$T`");
                }
            }
            b':' => self.lex_colon(),
            _ if is_ident_start(b) => self.lex_ident(),
            _ => self.lex_punct(),
        }
    }

    fn eat_ident_chars(&mut self) {
        while is_ident_char(self.peek()) {
            self.pos += 1;
        }
    }

    fn unexpected(&mut self, start: usize, label: &str) {
        let ch = self.text[start..].chars().next().unwrap_or('?');
        let end = start + ch.len_utf8();
        self.pos = self.pos.max(end);
        let message =
            if ch == '`' { "unexpected backtick".to_string() } else { format!("unexpected character `{ch}`") };
        self.error(
            Diagnostic::error(codes::UNEXPECTED_CHAR, message).primary(self.span(start, end), label).suggest_replace(
                "remove it",
                self.span(start, end),
                "",
                Applicability::MaybeIncorrect,
            ),
        );
    }

    /// Lexes the `#{` that starts a splice. The splice ends at the matching
    /// `}`; when there is none, a zero-width `SpliceEnd` is emitted where the
    /// `}` most likely belongs instead, so the code after it is still read.
    fn begin_splice(&mut self) {
        let start = self.pos;
        self.pos += 2;
        self.push(TokenKind::SpliceBegin, start, self.pos);
        let virtual_close = self.missing_splice_close();
        self.interps.push(Interp { depth: 0, quote: None, virtual_close });
        self.space_before = true;
    }

    /// Looks ahead from just after a splice's `#{` for its closing `}`,
    /// skipping nested braces, strings and comments (a splice may span
    /// lines). When there is none, returns where the `}` most likely
    /// belongs: after the last code on the line the splice starts on.
    fn missing_splice_close(&self) -> Option<usize> {
        let mut depth = 0u32;
        let mut i = self.pos;
        let mut code_end = self.pos;
        let mut first_line_end = None;
        while let Some(&b) = self.src.get(i) {
            match b {
                b'\n' => {
                    first_line_end.get_or_insert(code_end);
                }
                b' ' | b'\t' | b'\r' => {}
                b'#' if self.src.get(i + 1) != Some(&b'{') => {
                    while self.src.get(i + 1).is_some_and(|c| *c != b'\n') {
                        i += 1;
                    }
                }
                _ => {
                    match b {
                        b'{' => depth += 1,
                        b'}' if depth == 0 => return None,
                        b'}' => depth -= 1,
                        b'"' | b'\'' => {
                            let mut j = i + 1;
                            while let Some(&c) = self.src.get(j) {
                                if c == b {
                                    break;
                                }
                                j += if c == b'\\' { 2 } else { 1 };
                            }
                            i = j.min(self.src.len());
                        }
                        _ => {}
                    }
                    code_end = (i + 1).min(self.src.len());
                }
            }
            i += 1;
        }
        Some(first_line_end.unwrap_or(code_end))
    }

    fn lex_comment(&mut self) {
        let start = self.pos;
        while self.pos < self.src.len() && self.src[self.pos] != b'\n' {
            self.pos += 1;
        }
        let raw = &self.text[start + 1..self.pos];
        let text = raw.strip_prefix(' ').unwrap_or(raw).trim_end().to_string();
        let line_start = self.text[..start].rfind('\n').map_or(0, |i| i + 1);
        let own_line = self.text[line_start..start].trim().is_empty();
        self.out.comments.push(Comment { span: self.span(start, self.pos), text, own_line });
        self.space_before = true;
    }

    fn lex_ident(&mut self) {
        let start = self.pos;
        let upper = self.peek().is_ascii_uppercase();
        self.eat_ident_chars();
        if !upper && matches!(self.peek(), b'?' | b'!') && self.peek_at(1) != b'=' {
            // `x ? a : b` needs a space before `?`; `x?` is a predicate name.
            let next = self.peek_at(1);
            let is_ternary_like = self.peek() == b'?' && next == b':';
            if !is_ternary_like {
                self.pos += 1;
            }
        }
        let text = &self.text[start..self.pos];
        let kind = if let Some(kw) = Keyword::from_ident(text) {
            TokenKind::Kw(kw)
        } else if upper {
            TokenKind::Const
        } else {
            TokenKind::Ident
        };
        self.push(kind, start, self.pos);
    }

    fn lex_colon(&mut self) {
        let start = self.pos;
        let prev = if start > 0 { self.src[start - 1] } else { b' ' };
        let attached = is_ident_char(prev) || matches!(prev, b')' | b']' | b'}' | b'"' | b'\'' | b'?' | b'!');
        let next = self.peek_at(1);
        if !attached && next == b'#' && self.peek_at(2) == b'{' {
            self.pos += 1;
            self.push(TokenKind::ColonSplice, start, self.pos);
            return;
        }
        if !attached
            && matches!(next, b'"' | b'\'')
            && let Some(len) = self.src[start + 2..].iter().take_while(|b| **b != b'\n').position(|b| *b == next)
        {
            let end = start + 2 + len + 1;
            let content = self.text[start + 2..end - 1].to_string();
            let span = self.span(start, end);
            let mut diag = Diagnostic::error(codes::INVALID_SYMBOL, "symbols are written without quotes")
                .primary(span, "a quoted symbol");
            let is_name = content.bytes().next().is_some_and(is_ident_start) && content.bytes().all(is_ident_char);
            diag = if is_name {
                diag.suggest_replace("remove the quotes", span, format!(":{content}"), Applicability::MachineApplicable)
            } else {
                diag.note("a symbol is a name, like `:north`")
                    .help(format!("for arbitrary text, use a string: \"{content}\""))
            };
            self.error(diag);
            self.pos = end;
            self.push(TokenKind::Symbol, start, end);
            return;
        }
        if !attached && is_ident_start(next) {
            self.pos += 1;
            self.eat_ident_chars();
            if matches!(self.peek(), b'?' | b'!' | b'=') && self.peek_at(1) != b'=' {
                // `:empty?`, `:save!` and setter symbols like `:x=` are allowed,
                // but not when followed by `=` (`:a==b` is nonsense anyway).
                if self.peek() != b'=' || !matches!(self.peek_at(1), b'>' | b'~') {
                    self.pos += 1;
                }
            }
            self.push(TokenKind::Symbol, start, self.pos);
            return;
        }
        if !attached && next != b' ' && next != b':' {
            let ops: [&[u8]; 18] = [
                b"[]=", b"<=>", b"**", b"==", b"<=", b">=", b"<<", b">>", b"[]", b"!=", b"+", b"-", b"*", b"/", b"%",
                b"<", b">", b"!",
            ];
            for op in ops {
                if self.src[start + 1..].starts_with(op) {
                    self.pos = start + 1 + op.len();
                    self.push(TokenKind::Symbol, start, self.pos);
                    return;
                }
            }
        }
        self.pos += 1;
        self.push(TokenKind::Colon, start, self.pos);
    }

    fn lex_number(&mut self) {
        let start = self.pos;
        let mut is_float = false;
        if self.peek() == b'0' && matches!(self.peek_at(1), b'x' | b'b' | b'o' | b'X' | b'B' | b'O') {
            self.pos += 2;
            while self.peek().is_ascii_alphanumeric() || self.peek() == b'_' {
                self.pos += 1;
            }
        } else {
            while self.peek().is_ascii_digit() || self.peek() == b'_' {
                self.pos += 1;
            }
            if self.peek() == b'.' && self.peek_at(1).is_ascii_digit() {
                is_float = true;
                self.pos += 1;
                while self.peek().is_ascii_digit() || self.peek() == b'_' {
                    self.pos += 1;
                }
            }
            if matches!(self.peek(), b'e' | b'E')
                && (self.peek_at(1).is_ascii_digit()
                    || (matches!(self.peek_at(1), b'+' | b'-') && self.peek_at(2).is_ascii_digit()))
            {
                is_float = true;
                self.pos += 2;
                while self.peek().is_ascii_digit() {
                    self.pos += 1;
                }
            }
            if is_ident_start(self.peek()) {
                // The literal ends before the suffix, so the parser sees a
                // valid number and reports nothing more.
                let suffix_start = self.pos;
                self.eat_ident_chars();
                let suffix = String::from_utf8_lossy(&self.src[suffix_start..self.pos]).into_owned();
                let note = if suffix.starts_with(['e', 'E']) && !is_float {
                    "an exponent needs digits, like `1e3` or `2.5e-4`"
                } else {
                    "Wid number literals have no suffixes; the type comes from context, like `x: F32 = 1.5`"
                };
                let suffix_span = self.span(suffix_start, self.pos);
                self.error(
                    Diagnostic::error(codes::INVALID_NUMBER, format!("number literal with suffix `{suffix}`"))
                        .primary(suffix_span, "unexpected suffix")
                        .note(note)
                        .suggest_replace("remove the suffix", suffix_span, "", Applicability::MaybeIncorrect),
                );
                let kind = if is_float { TokenKind::Float } else { TokenKind::Int };
                self.push(kind, start, suffix_start);
                return;
            }
        }
        let kind = if is_float { TokenKind::Float } else { TokenKind::Int };
        self.push(kind, start, self.pos);
    }

    fn lex_string(&mut self, quote: u8) {
        let start = self.pos;
        self.pos += 1;
        self.continue_string(quote, start, true);
    }

    /// Looks ahead from just after `#{` for its closing `}` on the same line,
    /// skipping nested braces and string literals. When there is none,
    /// returns where the `}` most likely belongs: before the line's last
    /// `quote`, or at the end of the line.
    fn missing_interp_close(&self, quote: u8) -> Option<usize> {
        let mut depth = 0u32;
        let mut i = self.pos;
        let mut last_quote = None;
        while let Some(&b) = self.src.get(i) {
            match b {
                b'\n' => break,
                b'{' => depth += 1,
                b'}' if depth == 0 => return None,
                b'}' => depth -= 1,
                b'"' | b'\'' => {
                    if b == quote {
                        last_quote = Some(i);
                    }
                    let mut j = i + 1;
                    while let Some(&c) = self.src.get(j) {
                        match c {
                            b'\\' => j += 1,
                            b'\n' => break,
                            c if c == b => break,
                            _ => {}
                        }
                        j += 1;
                    }
                    if self.src.get(j) == Some(&b) {
                        if b == quote {
                            last_quote = Some(j);
                        }
                        i = j;
                    } else {
                        break;
                    }
                }
                _ => {}
            }
            i += 1;
        }
        Some(last_quote.unwrap_or(i))
    }

    /// Lexes string content until the closing quote or an interpolation.
    fn continue_string(&mut self, quote: u8, token_start: usize, first: bool) {
        let mut cooked = String::new();
        let seg_start = self.pos;
        loop {
            if self.pos >= self.src.len() {
                let span = self.span(token_start, self.pos);
                // End the string at the end of the line it starts on, so the
                // code after it (like a closing `end`) is still read.
                let line_end = self.src[seg_start..].iter().position(|b| *b == b'\n').map(|i| seg_start + i);
                let fix_at = line_end.unwrap_or(self.src.len());
                self.error(
                    Diagnostic::error(codes::UNTERMINATED_STRING, "unterminated string literal")
                        .primary(
                            span.shrink_to_start().to(self.span(token_start, token_start + 1)),
                            "string starts here",
                        )
                        .suggest(
                            format!("add a closing `{}`", quote as char),
                            vec![Edit { span: self.span(fix_at, fix_at), replacement: (quote as char).to_string() }],
                            Applicability::MaybeIncorrect,
                        ),
                );
                if let Some(end) = line_end {
                    self.pos = end;
                    cooked = String::from_utf8_lossy(&self.src[seg_start..end]).into_owned();
                }
                let idx = self.add_string(cooked);
                if first {
                    self.push(TokenKind::Str(idx), token_start, self.pos);
                } else {
                    self.push(TokenKind::StrText(idx), seg_start, self.pos);
                    self.push(TokenKind::StrEnd, self.pos, self.pos);
                }
                return;
            }
            let b = self.peek();
            if b == quote {
                self.pos += 1;
                let idx = self.add_string(cooked);
                if first {
                    self.push(TokenKind::Str(idx), token_start, self.pos);
                } else {
                    if !self.out.strings[idx as usize].is_empty() {
                        self.push(TokenKind::StrText(idx), seg_start, self.pos - 1);
                    }
                    self.push(TokenKind::StrEnd, self.pos - 1, self.pos);
                }
                return;
            }
            if quote == b'"' && b == b'#' && self.peek_at(1) == b'{' {
                if first {
                    self.push(TokenKind::StrBegin, token_start, token_start + 1);
                }
                if !cooked.is_empty() {
                    let idx = self.add_string(std::mem::take(&mut cooked));
                    self.push(TokenKind::StrText(idx), seg_start, self.pos);
                }
                let interp_start = self.pos;
                self.pos += 2;
                self.push(TokenKind::InterpBegin, interp_start, self.pos);
                let virtual_close = self.missing_interp_close(quote);
                if let Some(at) = virtual_close {
                    self.error(
                        Diagnostic::error(codes::UNTERMINATED_STRING, "unterminated string interpolation")
                            .primary(self.span(interp_start, self.pos), "this `#{` is never closed")
                            .suggest(
                                "close it with `}`",
                                vec![Edit { span: self.span(at, at), replacement: "}".into() }],
                                Applicability::MaybeIncorrect,
                            ),
                    );
                }
                self.interps.push(Interp { depth: 0, quote: Some(quote), virtual_close });
                self.space_before = true;
                return;
            }
            if b == b'\\' {
                let esc_start = self.pos;
                self.pos += 1;
                let e = self.peek();
                if quote == b'\'' {
                    match e {
                        b'\\' | b'\'' => {
                            cooked.push(e as char);
                            self.pos += 1;
                        }
                        _ => cooked.push('\\'),
                    }
                    continue;
                }
                self.pos += 1;
                match e {
                    b'n' => cooked.push('\n'),
                    b't' => cooked.push('\t'),
                    b'r' => cooked.push('\r'),
                    b'0' => cooked.push('\0'),
                    b'e' => cooked.push('\x1b'),
                    b'a' => cooked.push('\x07'),
                    b'b' => cooked.push('\x08'),
                    b'\\' => cooked.push('\\'),
                    b'"' => cooked.push('"'),
                    b'\'' => cooked.push('\''),
                    b'#' => cooked.push('#'),
                    b'\n' => {
                        while matches!(self.peek(), b' ' | b'\t') {
                            self.pos += 1;
                        }
                    }
                    b'x' => {
                        let hex = self.text.get(self.pos..self.pos + 2).unwrap_or("");
                        match u8::from_str_radix(hex, 16) {
                            Ok(v) if v < 0x80 => {
                                cooked.push(v as char);
                                self.pos += 2;
                            }
                            _ => self.bad_escape(esc_start, "`\\x` takes two hex digits below 80, like `\\x41`"),
                        }
                    }
                    b'u' => {
                        if self.peek() == b'{' {
                            let close = self.text[self.pos..].find('}').map(|i| self.pos + i);
                            let value = close
                                .and_then(|c| u32::from_str_radix(&self.text[self.pos + 1..c], 16).ok())
                                .and_then(char::from_u32);
                            match (close, value) {
                                (Some(c), Some(ch)) => {
                                    cooked.push(ch);
                                    self.pos = c + 1;
                                }
                                _ => self.bad_escape(esc_start, "`\\u{…}` takes a hex code point, like `\\u{1F600}`"),
                            }
                        } else {
                            self.bad_escape(esc_start, "write unicode escapes as `\\u{XXXX}`");
                        }
                    }
                    _ => {
                        let ch = self.text[self.pos - 1..].chars().next().unwrap_or('?');
                        self.pos += ch.len_utf8() - 1;
                        let span = self.span(esc_start, self.pos);
                        self.error(
                            Diagnostic::error(codes::INVALID_ESCAPE, format!("`\\{ch}` is not an escape sequence"))
                                .primary(span, "not a valid escape")
                                .note(
                                    "escapes: \\n \\t \\r \\0 \\e \\a \\b \\\\ \\\" \\' \\# \\xHH \\u{XXXX}, \
                                     and \\ at the end of a line joins it with the next",
                                )
                                .suggest_replace(
                                    "for a literal backslash, write `\\\\`",
                                    span,
                                    format!("\\\\{ch}"),
                                    Applicability::MaybeIncorrect,
                                ),
                        );
                    }
                }
                continue;
            }
            let ch = self.text[self.pos..].chars().next().unwrap_or('\0');
            cooked.push(ch);
            self.pos += ch.len_utf8();
        }
    }

    fn bad_escape(&mut self, start: usize, help: &str) {
        let span = self.span(start, self.pos.max(start + 2).min(self.src.len()));
        self.error(
            Diagnostic::error(codes::INVALID_ESCAPE, "invalid escape sequence")
                .primary(span, "not a valid escape")
                .help(help.to_string()),
        );
    }

    fn add_string(&mut self, s: String) -> u32 {
        self.out.strings.push(s);
        (self.out.strings.len() - 1) as u32
    }

    fn lex_punct(&mut self) {
        use TokenKind::*;
        let start = self.pos;
        let rest = &self.src[self.pos..];
        let table: &[(&[u8], TokenKind)] = &[
            (b"**=", StarStarEq),
            (b"<=>", Cmp),
            (b"<<=", ShlEq),
            (b">>=", ShrEq),
            (b"&&=", AndAndEq),
            (b"||=", OrOrEq),
            (b"---", TripleDash),
            (b"...", DotDotDot),
            (b"**", StarStar),
            (b"*=", StarEq),
            (b"<<", Shl),
            (b"<=", Le),
            (b">>", Shr),
            (b">=", Ge),
            (b"==", EqEq),
            (b"=>", FatArrow),
            (b"!=", NotEq),
            (b"&&", AndAnd),
            (b"&.", SafeNav),
            (b"&=", AmpEq),
            (b"||", OrOr),
            (b"|=", PipeEq),
            (b"+=", PlusEq),
            (b"-=", MinusEq),
            (b"->", Arrow),
            (b"/=", SlashEq),
            (b"%=", PercentEq),
            (b"~=", TildeEq),
            (b"..", DotDot),
            (b"*", Star),
            (b"<", Lt),
            (b">", Gt),
            (b"=", Eq),
            (b"!", Bang),
            (b"&", Amp),
            (b"|", Pipe),
            (b"+", Plus),
            (b"-", Minus),
            (b"/", Slash),
            (b"%", Percent),
            (b"~", Tilde),
            (b"^", Caret),
            (b".", Dot),
            (b"?", Question),
            (b",", Comma),
            (b"(", LParen),
            (b")", RParen),
            (b"[", LBracket),
            (b"]", RBracket),
            (b"{", LBrace),
            (b"}", RBrace),
        ];
        for (text, kind) in table {
            if rest.starts_with(text) {
                // `&.` followed by a digit is `&` then a float like `&.5`; not valid anyway.
                self.pos += text.len();
                match kind {
                    LBrace => {
                        if let Some(top) = self.interps.last_mut() {
                            top.depth += 1;
                        }
                    }
                    RBrace => {
                        if let Some(top) = self.interps.last_mut() {
                            if top.depth == 0 {
                                let quote = top.quote;
                                self.interps.pop();
                                match quote {
                                    Some(quote) => {
                                        self.push(InterpEnd, start, self.pos);
                                        self.continue_string(quote, start, false);
                                    }
                                    None => self.push(SpliceEnd, start, self.pos),
                                }
                                return;
                            }
                            top.depth -= 1;
                        }
                    }
                    _ => {}
                }
                self.push(*kind, start, self.pos);
                return;
            }
        }
        self.unexpected(start, "this character is not part of Wid's syntax");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        lex(FileId(0), src).tokens.iter().map(|t| t.kind).collect()
    }

    #[test]
    fn predicates_and_symbols() {
        use TokenKind::*;
        assert_eq!(kinds("x.nil?"), vec![Ident, Dot, Ident, Newline, Eof]);
        assert_eq!(
            kinds("f(:north, a: 1)"),
            vec![Ident, LParen, Symbol, Comma, Ident, Colon, Int, RParen, Newline, Eof]
        );
        assert_eq!(kinds("x != y"), vec![Ident, NotEq, Ident, Newline, Eof]);
        assert_eq!(kinds("overload :*, :a"), vec![Kw(Keyword::Overload), Symbol, Comma, Symbol, Newline, Eof]);
    }

    #[test]
    fn ranges_and_floats() {
        use TokenKind::*;
        assert_eq!(kinds("0..10"), vec![Int, DotDot, Int, Newline, Eof]);
        assert_eq!(kinds("1.5"), vec![Float, Newline, Eof]);
        assert_eq!(kinds("1.to_f"), vec![Int, Dot, Ident, Newline, Eof]);
    }

    #[test]
    fn interpolation() {
        use TokenKind::*;
        let lexed = lex(FileId(0), r#""a#{b + 1}c""#);
        let k: Vec<_> = lexed.tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            k,
            vec![StrBegin, StrText(0), InterpBegin, Ident, Plus, Int, InterpEnd, StrText(1), StrEnd, Newline, Eof]
        );
        assert_eq!(lexed.strings, vec!["a".to_string(), "c".to_string()]);
    }

    #[test]
    fn splices() {
        use TokenKind::*;
        assert_eq!(kinds("#{a}"), vec![SpliceBegin, Ident, SpliceEnd, Newline, Eof]);
        assert_eq!(
            kinds("def #{name} = 1"),
            vec![Kw(Keyword::Def), SpliceBegin, Ident, SpliceEnd, Eq, Int, Newline, Eof]
        );
        assert_eq!(kinds("@#{f} = 1"), vec![AtSplice, SpliceBegin, Ident, SpliceEnd, Eq, Int, Newline, Eof]);
        assert_eq!(kinds("x = :#{s}"), vec![Ident, Eq, ColonSplice, SpliceBegin, Ident, SpliceEnd, Newline, Eof]);
        assert_eq!(
            kinds("x.#{m}(1)"),
            vec![Ident, Dot, SpliceBegin, Ident, SpliceEnd, LParen, Int, RParen, Newline, Eof]
        );
        // An attached `:` is a type annotation, not a symbol splice.
        assert_eq!(kinds("x:#{t}"), vec![Ident, Colon, SpliceBegin, Ident, SpliceEnd, Newline, Eof]);
        assert_eq!(kinds("#{x}:Int"), vec![SpliceBegin, Ident, SpliceEnd, Colon, Const, Newline, Eof]);
    }

    #[test]
    fn splice_spans_lines_and_nests_braces() {
        use TokenKind::*;
        assert_eq!(
            kinds("#{f(\n  a,\n  b\n)}\nc"),
            vec![SpliceBegin, Ident, LParen, Ident, Comma, Ident, RParen, SpliceEnd, Newline, Ident, Newline, Eof]
        );
        assert_eq!(
            kinds("#{xs.map { |x| x }}"),
            vec![SpliceBegin, Ident, Dot, Ident, LBrace, Pipe, Ident, Pipe, Ident, RBrace, SpliceEnd, Newline, Eof]
        );
        assert_eq!(
            kinds("#{a #{b}}"),
            vec![SpliceBegin, Ident, SpliceBegin, Ident, SpliceEnd, SpliceEnd, Newline, Eof]
        );
        // A line that starts with a splice is code, not a comment, so the
        // `.c` line continues it rather than the line before.
        assert_eq!(kinds("a\n#{b}\n.c"), vec![Ident, Newline, SpliceBegin, Ident, SpliceEnd, Dot, Ident, Newline, Eof]);
    }

    #[test]
    fn splice_text_in_comments_and_strings() {
        use TokenKind::*;
        let lexed = lex(FileId(0), "x = 1 # see #{y}\n");
        assert_eq!(lexed.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(), vec![Ident, Eq, Int, Newline, Eof]);
        assert_eq!(lexed.comments[0].text, "see #{y}");
        // In a string, `#{` is interpolation, inside a `quote` too.
        assert_eq!(
            kinds("quote do\n  puts \"v #{x}\"\nend"),
            vec![
                Kw(Keyword::Quote),
                Kw(Keyword::Do),
                Newline,
                Ident,
                StrBegin,
                StrText(0),
                InterpBegin,
                Ident,
                InterpEnd,
                StrEnd,
                Newline,
                Kw(Keyword::End),
                Newline,
                Eof
            ]
        );
        // A splice may hold a string with its own interpolation.
        assert_eq!(
            kinds("#{\"a#{b}\"}"),
            vec![SpliceBegin, StrBegin, StrText(0), InterpBegin, Ident, InterpEnd, StrEnd, SpliceEnd, Newline, Eof]
        );
    }

    #[test]
    fn unclosed_splice_ends_at_its_line() {
        use TokenKind::*;
        let lexed = lex(FileId(0), "def #{a # note\n  b\nend");
        let k: Vec<_> = lexed.tokens.iter().map(|t| t.kind).collect();
        assert_eq!(
            k,
            vec![
                Kw(Keyword::Def),
                SpliceBegin,
                Ident,
                SpliceEnd,
                Newline,
                Ident,
                Newline,
                Kw(Keyword::End),
                Newline,
                Eof
            ]
        );
        // The missing `}` is a zero-width `SpliceEnd` right after `a`; the
        // parser reports it.
        let end = lexed.tokens[3].span;
        assert_eq!((end.start, end.end), (7, 7));
        assert!(lexed.diagnostics.is_empty());
        let at_eof = lex(FileId(0), "#{a");
        assert_eq!(
            at_eof.tokens.iter().map(|t| t.kind).collect::<Vec<_>>(),
            vec![SpliceBegin, Ident, SpliceEnd, Newline, Eof]
        );
    }

    #[test]
    fn line_continuation() {
        use TokenKind::*;
        assert_eq!(kinds("a +\n b"), vec![Ident, Plus, Ident, Newline, Eof]);
        assert_eq!(kinds("a\n  .b"), vec![Ident, Dot, Ident, Newline, Eof]);
    }
}

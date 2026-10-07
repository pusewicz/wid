//! A recursive-descent parser with Pratt expression parsing and error recovery.

use wid_diagnostics::{Applicability, Diagnostic, Diagnostics, Edit, FileId, Span, codes};

use crate::ast::*;
use crate::intern::Name;
use crate::lexer::{Lexed, lex};
use crate::token::{Comment, Keyword, Token, TokenKind};

use Keyword as K;
use TokenKind as T;

/// Lexes and parses one file.
pub fn parse_file(file: FileId, text: &str) -> (File, Diagnostics) {
    let lexed = lex(file, text);
    let mut parser = Parser::new(file, text, lexed);
    let items = parser.parse_items_until_eof();
    let mut diags = std::mem::take(&mut parser.lex_diags);
    for d in parser.diags {
        diags.push(d);
    }
    (File { file, items, comments: parser.comments }, diags)
}

/// Parses a standalone expression, used by tools and tests.
pub fn parse_expr_str(file: FileId, text: &str) -> (Expr, Diagnostics) {
    let lexed = lex(file, text);
    let mut parser = Parser::new(file, text, lexed);
    parser.skip_newlines();
    let expr = parser.parse_expr();
    let mut diags = std::mem::take(&mut parser.lex_diags);
    for d in parser.diags {
        diags.push(d);
    }
    (expr, diags)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ItemCtx {
    Package,
    Struct,
    Enum,
    Module,
    Extend,
}

/// What a parameter list belongs to, which decides whether it may end with
/// a `*` parameter.
#[derive(Clone, Copy, PartialEq, Eq)]
enum ParamsOf {
    /// A method, with its name for the advice.
    Def(Name),
    Macro,
    Proc,
}

struct Opener {
    keyword: &'static str,
    span: Span,
}

/// A `$T` written as a parameter of its own (`$T, xs: []T`, or Odin's
/// `$T: typeid`), skipped by the parameter list and reported after it.
struct LooseTypeParam {
    /// `T`, without the `$`.
    name: Name,
    /// The `$T` token.
    tok: Span,
    /// Its token index, and the index of the `,`, `)` or line end after it.
    first: usize,
    after: usize,
    /// Whether a `: type` followed, as in Odin.
    typed: bool,
}

/// A block that was closed by an `end`, kept to diagnose misplaced `end`s.
struct Closed {
    keyword: &'static str,
    span: Span,
    opener_indent: usize,
    end_span: Span,
    end_indent: usize,
}

struct Parser<'a> {
    file: FileId,
    text: &'a str,
    tokens: Vec<Token>,
    strings: Vec<String>,
    comments: Vec<Comment>,
    line_starts: Vec<u32>,
    pos: usize,
    diags: Vec<Diagnostic>,
    lex_diags: Diagnostics,
    last_error_at: Option<u32>,
    locals: Vec<Vec<Name>>,
    scope_floor: Vec<usize>,
    no_do: bool,
    openers: Vec<Opener>,
    closed: Vec<Closed>,
    /// Set when a `yield` is parsed, to mark the enclosing `def`.
    yield_seen: bool,
    /// A `?` the lexer glued to the last name of a type path (`C.int?`),
    /// applied by the enclosing type's suffix so `^C.int?` is `(^C.int)?`
    /// like `^Int?`.
    pending_optional: Option<Span>,
    /// The splice lists of the open `quote`s, innermost last. A splice
    /// expression is parsed with this stack emptied, so a `quote` inside it
    /// starts its own list and a bare splice inside it is reported.
    quotes: Vec<Vec<Expr>>,
    /// How many splice expressions enclose the position, counted from the
    /// innermost `quote` (for the message about a splice inside a splice).
    splice_depth: u32,
    /// Whether the position is inside a `macro def` (for the advice about a
    /// splice outside a `quote`).
    in_macro: bool,
    /// The names of the loose `$T` parameters (see [`LooseTypeParam`]) of
    /// the last parameter list whose fix found no type to introduce them
    /// in; `parse_def` drops their uses in the return type.
    uninferred: Vec<Name>,
}

/// Binding powers for infix operators.
fn infix_bp(kind: TokenKind) -> Option<(u8, u8)> {
    Some(match kind {
        T::Question => (2, 1),
        T::DotDot | T::DotDotDot => (3, 4),
        T::OrOr => (4, 5),
        T::AndAnd => (5, 6),
        T::EqEq | T::NotEq | T::Cmp => (7, 8),
        T::Lt | T::Le | T::Gt | T::Ge => (8, 9),
        T::Pipe | T::Tilde => (9, 10),
        T::Amp => (10, 11),
        T::Shl | T::Shr => (11, 12),
        T::Plus | T::Minus => (12, 13),
        T::Star | T::Slash | T::Percent => (13, 14),
        T::StarStar => (16, 15),
        _ => return None,
    })
}

/// Builtins whose arguments are types (`type_info` also takes a value), so
/// a type is expected there.
const TYPE_ARG_BUILTINS: &[&str] = &["size_of", "align_of", "type_info"];

const PREFIX_NEG_BP: u8 = 14;
const PREFIX_NOT_BP: u8 = 17;

fn binop_of(kind: TokenKind) -> Option<BinOp> {
    Some(match kind {
        T::Plus | T::PlusEq => BinOp::Add,
        T::Minus | T::MinusEq => BinOp::Sub,
        T::Star | T::StarEq => BinOp::Mul,
        T::Slash | T::SlashEq => BinOp::Div,
        T::Percent | T::PercentEq => BinOp::Rem,
        T::StarStar | T::StarStarEq => BinOp::Pow,
        T::Amp | T::AmpEq => BinOp::BitAnd,
        T::Pipe | T::PipeEq => BinOp::BitOr,
        T::Tilde | T::TildeEq => BinOp::BitXor,
        T::Shl | T::ShlEq => BinOp::Shl,
        T::Shr | T::ShrEq => BinOp::Shr,
        T::EqEq => BinOp::Eq,
        T::NotEq => BinOp::Ne,
        T::Lt => BinOp::Lt,
        T::Le => BinOp::Le,
        T::Gt => BinOp::Gt,
        T::Ge => BinOp::Ge,
        T::Cmp => BinOp::Cmp,
        T::AndAnd | T::AndAndEq => BinOp::And,
        T::OrOr | T::OrOrEq => BinOp::Or,
        _ => return None,
    })
}

fn is_assign_op(kind: TokenKind) -> bool {
    matches!(
        kind,
        T::PlusEq
            | T::MinusEq
            | T::StarEq
            | T::SlashEq
            | T::PercentEq
            | T::StarStarEq
            | T::AmpEq
            | T::PipeEq
            | T::TildeEq
            | T::ShlEq
            | T::ShrEq
            | T::AndAndEq
            | T::OrOrEq
    )
}

impl<'a> Parser<'a> {
    fn new(file: FileId, text: &'a str, lexed: Lexed) -> Self {
        let mut line_starts = vec![0u32];
        for (i, b) in text.bytes().enumerate() {
            if b == b'\n' {
                line_starts.push(i as u32 + 1);
            }
        }
        Parser {
            file,
            text,
            tokens: lexed.tokens,
            strings: lexed.strings,
            comments: lexed.comments,
            line_starts,
            pos: 0,
            diags: Vec::new(),
            lex_diags: lexed.diagnostics,
            last_error_at: None,
            locals: vec![Vec::new()],
            scope_floor: vec![0],
            no_do: false,
            openers: Vec::new(),
            closed: Vec::new(),
            yield_seen: false,
            pending_optional: None,
            quotes: Vec::new(),
            splice_depth: 0,
            in_macro: false,
            uninferred: Vec::new(),
        }
    }

    // ----- token helpers -------------------------------------------------

    fn peek(&self) -> Token {
        self.tokens[self.pos.min(self.tokens.len() - 1)]
    }

    fn kind(&self) -> TokenKind {
        self.peek().kind
    }

    fn nth(&self, n: usize) -> Token {
        self.tokens[(self.pos + n).min(self.tokens.len() - 1)]
    }

    fn at(&self, kind: TokenKind) -> bool {
        self.kind() == kind
    }

    fn at_kw(&self, kw: Keyword) -> bool {
        self.kind() == T::Kw(kw)
    }

    fn bump(&mut self) -> Token {
        let tok = self.peek();
        if self.pos < self.tokens.len() - 1 {
            self.pos += 1;
        }
        tok
    }

    fn eat(&mut self, kind: TokenKind) -> bool {
        if self.at(kind) {
            self.bump();
            true
        } else {
            false
        }
    }

    fn eat_kw(&mut self, kw: Keyword) -> bool {
        self.eat(T::Kw(kw))
    }

    fn prev_span(&self) -> Span {
        if self.pos == 0 { self.peek().span } else { self.tokens[self.pos - 1].span }
    }

    fn text_of(&self, span: Span) -> &'a str {
        &self.text[span.start as usize..span.end as usize]
    }

    /// Skips newlines, and splices outside a `quote` that read as comments
    /// (reported).
    fn skip_newlines(&mut self) {
        loop {
            if self.at(T::Newline) {
                self.bump();
            } else if self.at(T::SpliceBegin) && !self.splices_are_code() && self.stray_is_comment() {
                self.stray_splice();
            } else {
                return;
            }
        }
    }

    fn line_of(&self, offset: u32) -> usize {
        match self.line_starts.binary_search(&offset) {
            Ok(i) => i,
            Err(i) => i - 1,
        }
    }

    fn indent_of_line(&self, line: usize) -> usize {
        let start = self.line_starts[line] as usize;
        self.text[start..].bytes().take_while(|b| *b == b' ' || *b == b'\t').count()
    }

    // ----- diagnostics ---------------------------------------------------

    fn report(&mut self, diag: Diagnostic) {
        let at = diag.primary_span().map(|s| s.start);
        if at.is_some() && at == self.last_error_at {
            return;
        }
        // A stray character was already reported on this line; the tokens
        // around it rarely parse, and those errors would only repeat it.
        if diag.code == codes::UNEXPECTED_TOKEN
            && let Some(at) = at
        {
            let line = self.line_of(at);
            let stray = self.lex_diags.iter().any(|d| {
                d.code == codes::UNEXPECTED_CHAR && d.primary_span().is_some_and(|s| self.line_of(s.start) == line)
            });
            if stray {
                return;
            }
        }
        self.last_error_at = at;
        self.diags.push(diag);
    }

    fn found(&self) -> String {
        let tok = self.peek();
        match tok.kind {
            T::Ident | T::Const | T::IVar | T::Int | T::Float | T::Symbol | T::TypeParam => {
                format!("`{}`", self.text_of(tok.span))
            }
            // Where the lexer assumed a splice's missing `}`.
            T::SpliceEnd if tok.span.is_empty() => "end of line".to_string(),
            k => k.describe().to_string(),
        }
    }

    fn error_expected(&mut self, what: &str) {
        let tok = self.peek();
        let found = self.found();
        let mut diag = Diagnostic::error(codes::UNEXPECTED_TOKEN, format!("expected {what}, found {found}"))
            .primary(tok.span, format!("expected {what}"));
        if tok.kind == T::Eof
            && let Some(opener) = self.openers.last()
        {
            diag = diag.secondary(opener.span, format!("inside this `{}`", opener.keyword));
        }
        self.report(diag);
    }

    fn expect(&mut self, kind: TokenKind, what: &str) -> bool {
        if self.eat(kind) {
            true
        } else {
            self.error_expected(what);
            false
        }
    }

    fn expect_ident(&mut self, what: &str) -> Ident {
        let tok = self.peek();
        if tok.kind == T::Ident {
            self.bump();
            Ident { name: Name::new(self.text_of(tok.span)), span: tok.span }
        } else {
            self.error_expected(what);
            Ident { name: Name::new("<error>"), span: tok.span }
        }
    }

    fn expect_const(&mut self, what: &str) -> Ident {
        let tok = self.peek();
        if tok.kind == T::Const {
            self.bump();
            Ident { name: Name::new(self.text_of(tok.span)), span: tok.span }
        } else {
            let found = self.found();
            let mut diag = Diagnostic::error(codes::UNEXPECTED_TOKEN, format!("expected {what}, found {found}"))
                .primary(tok.span, format!("expected {what}"));
            if tok.kind == T::Ident {
                let text = self.text_of(tok.span);
                let mut fixed = text.to_string();
                if let Some(first) = fixed.get_mut(0..1) {
                    first.make_ascii_uppercase();
                }
                diag = diag.suggest_replace(
                    "type names start with an uppercase letter",
                    tok.span,
                    fixed,
                    Applicability::MachineApplicable,
                );
            }
            self.report(diag);
            if tok.kind == T::Ident {
                self.bump();
            }
            Ident { name: Name::new(self.text_of(tok.span)), span: tok.span }
        }
    }

    /// Skips to the end of the current line, or of the enclosing splice,
    /// after an error.
    fn recover_line(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.kind() {
                T::Eof => return,
                T::Newline | T::SpliceEnd if depth <= 0 => return,
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                T::RParen | T::RBracket | T::RBrace => depth -= 1,
                _ => {}
            }
            self.bump();
        }
    }

    /// Parses a constant's value as a type when the whole value is one that
    /// only reads as a type, like `RawPtr?`, `^Node?`, `C.int` or a
    /// parenthesized type (`(proc(Int) -> Int)?`, `(Int, Int)`). A
    /// parenthesized name (`(Vec2)`) or splice (`(#{v})`) reads as an
    /// expression the same way, and an expression like `(1 + 2) * 3`
    /// doesn't parse as a type.
    /// Restores the position and returns `None` otherwise.
    fn try_type_alias(&mut self) -> Option<TypeExpr> {
        let paren = self.at(T::LParen);
        if !paren && !matches!(self.peek().kind, T::Const | T::Ident | T::Caret | T::LBracket) {
            return None;
        }
        let save = self.pos;
        let splices = self.splice_mark();
        let texpr = self.try_parse_type()?;
        let only_type = match &texpr.kind {
            TypeKind::Path { .. } | TypeKind::Splice(_) if paren => false,
            _ if paren => true,
            TypeKind::Optional(_) | TypeKind::Pointer(_) | TypeKind::MultiPointer(_) => true,
            TypeKind::Path { segments, args } => {
                args.is_empty()
                    && segments.len() == 2
                    && segments[1].as_str().starts_with(|c: char| c.is_ascii_lowercase())
            }
            _ => false,
        };
        if only_type && self.at_stmt_end() {
            return Some(texpr);
        }
        self.pos = save;
        self.rewind_splices(splices);
        None
    }

    fn at_stmt_end(&self) -> bool {
        matches!(
            self.kind(),
            T::Newline
                | T::Eof
                | T::RBrace
                | T::RParen
                | T::InterpEnd
                | T::SpliceEnd
                | T::Kw(K::End)
                | T::Kw(K::Else)
                | T::Kw(K::Elsif)
                | T::Kw(K::When)
                | T::Kw(K::Then)
        )
    }

    fn expect_stmt_end(&mut self) {
        if self.at(T::SpliceBegin) && self.quotes.is_empty() {
            // Most likely a comment written without a space after `#`.
            self.stray_splice();
        }
        if self.at_stmt_end() {
            return;
        }
        let tok = self.peek();
        let prev = self.tokens[self.pos.saturating_sub(1)];
        if prev.kind == T::Ident && tok.space_before && self.can_start_command_arg(tok) {
            let name = self.text_of(prev.span).to_string();
            let line_end = self.line_end_offset(tok.span.start);
            let args = self.text[tok.span.start as usize..line_end as usize].trim_end().to_string();
            let args_span = Span::new(self.file, tok.span.start, tok.span.start + args.len() as u32);
            self.report(
                Diagnostic::error(
                    codes::NESTED_COMMAND_CALL,
                    format!("`{name}` is called without parentheses inside another expression"),
                )
                .primary(prev.span.to(args_span), "this nested call needs parentheses")
                .note("only the outermost call of a statement may omit parentheses")
                .suggest(
                    "add parentheses",
                    vec![Edit { span: prev.span.to(args_span), replacement: format!("{name}({args})") }],
                    Applicability::MaybeIncorrect,
                ),
            );
        } else if tok.kind == T::Comma {
            let tok_span = tok.span;
            self.report(
                Diagnostic::error(codes::UNEXPECTED_TOKEN, "expected end of line, found `,`")
                    .primary(tok_span, "a list of values is not a statement")
                    .help("to return several values, write `return a, b`; to assign them, write `a, b = …`"),
            );
        } else {
            self.error_expected("end of line");
        }
        self.recover_line();
    }

    fn line_end_offset(&self, offset: u32) -> u32 {
        let line = self.line_of(offset);
        self.line_starts.get(line + 1).map_or(self.text.len() as u32, |s| s - 1)
    }

    // ----- locals (for command-call disambiguation) ---------------------

    fn push_scope(&mut self) {
        self.locals.push(Vec::new());
    }

    fn pop_scope(&mut self) {
        self.locals.pop();
    }

    fn push_def_scope(&mut self) {
        self.scope_floor.push(self.locals.len());
        self.locals.push(Vec::new());
    }

    fn pop_def_scope(&mut self) {
        let floor = self.scope_floor.pop().unwrap_or(1);
        self.locals.truncate(floor.max(1));
    }

    fn declare(&mut self, name: Name) {
        if let Some(scope) = self.locals.last_mut() {
            scope.push(name);
        }
    }

    fn is_local(&self, name: Name) -> bool {
        let floor = self.scope_floor.last().copied().unwrap_or(0);
        self.locals[floor..].iter().any(|s| s.contains(&name))
    }

    // ----- quotes and splices ---------------------------------------------

    /// The number of tokens of the splice `#{…}` that starts `n` tokens
    /// ahead, through its `}`.
    fn splice_len(&self, n: usize) -> Option<usize> {
        if self.nth(n).kind != T::SpliceBegin {
            return None;
        }
        let mut depth = 0usize;
        let mut i = n;
        loop {
            match self.nth(i).kind {
                T::SpliceBegin => depth += 1,
                T::SpliceEnd if depth == 1 => return Some(i - n + 1),
                T::SpliceEnd => depth -= 1,
                T::Eof => return None,
                _ => {}
            }
            i += 1;
        }
    }

    /// Whether a splice here is code: inside a `quote`, or inside a splice
    /// (where another splice is reported as nested). Elsewhere a splice is
    /// a comment written without a space and is skipped at the end of the
    /// statement.
    fn splices_are_code(&self) -> bool {
        !self.quotes.is_empty() || self.splice_depth > 0
    }

    /// The number of tokens of the name `n` tokens ahead: an identifier,
    /// or inside a `quote` a splice standing for one.
    fn name_len(&self, n: usize) -> Option<usize> {
        match self.nth(n).kind {
            T::Ident => Some(1),
            T::SpliceBegin if !self.quotes.is_empty() => self.splice_len(n),
            _ => None,
        }
    }

    /// Parses a name: an identifier, or a splice, which becomes the
    /// placeholder [`splice_name`].
    fn parse_name(&mut self, what: &str) -> Ident {
        if !self.at(T::SpliceBegin) {
            return self.expect_ident(what);
        }
        let span = self.peek().span;
        self.parse_splice_name().unwrap_or(Ident { name: Name::new("<error>"), span })
    }

    /// Parses the name of a declared type: a constant, or a splice.
    fn parse_type_name(&mut self, what: &str) -> Ident {
        if self.at(T::SpliceBegin) { self.parse_name(what) } else { self.expect_const(what) }
    }

    /// Parses a splice in a name position. `None` means it was outside a
    /// `quote` (reported).
    fn parse_splice_name(&mut self) -> Option<Ident> {
        let (index, span) = self.parse_splice();
        index.map(|i| Ident { name: splice_name(i), span })
    }

    /// The length of the innermost quote's splice list, so a speculative
    /// parse can drop its splices with [`Parser::rewind_splices`].
    fn splice_mark(&self) -> usize {
        self.quotes.last().map_or(0, Vec::len)
    }

    fn rewind_splices(&mut self, mark: usize) {
        if let Some(list) = self.quotes.last_mut() {
            list.truncate(mark);
        }
    }

    /// Parses `#{expr}` at a `SpliceBegin`. Inside a `quote`, appends
    /// `expr` to the innermost quote's splices and returns its index;
    /// outside one, reports the splice, skips it and returns `None`. The
    /// span covers `#{` through `}`.
    fn parse_splice(&mut self) -> (Option<u32>, Span) {
        let begin = self.peek();
        if self.quotes.is_empty() {
            self.stray_splice();
            return (None, begin.span.to(self.prev_span()));
        }
        self.bump();
        let quotes = std::mem::take(&mut self.quotes);
        let no_do = std::mem::replace(&mut self.no_do, false);
        self.splice_depth += 1;
        // The expression runs in the macro, so the macro's locals are in
        // scope there.
        self.scope_floor.push(0);
        let expr = if self.at(T::SpliceEnd) && !self.peek().span.is_empty() {
            let end = self.peek().span;
            self.report(
                Diagnostic::error(codes::UNEXPECTED_TOKEN, "empty splice")
                    .primary(begin.span.to(end), "there is nothing to insert here")
                    .help("put the macro value to insert between the braces, like `#{name}`"),
            );
            Expr { kind: ExprKind::Error, span: end }
        } else {
            self.parse_expr()
        };
        self.scope_floor.pop();
        self.splice_depth -= 1;
        self.no_do = no_do;
        self.quotes = quotes;
        self.close_splice(begin.span);
        let span = begin.span.to(self.prev_span());
        match self.quotes.last_mut() {
            Some(list) => {
                list.push(expr);
                (Some(list.len() as u32 - 1), span)
            }
            None => (None, span),
        }
    }

    /// Consumes the `}` of the splice whose `#{` is at `begin`, skipping
    /// what its expression left over.
    fn close_splice(&mut self, begin: Span) {
        let mut depth = 0u32;
        let mut i = 0;
        let end = loop {
            match self.nth(i).kind {
                T::Eof => break None,
                T::SpliceBegin => depth += 1,
                T::SpliceEnd if depth == 0 => break Some(self.nth(i)),
                T::SpliceEnd => depth -= 1,
                _ => {}
            }
            i += 1;
        };
        match end {
            // The lexer found no `}`: report that, not the tokens before
            // the place it assumed one.
            Some(end) if end.span.is_empty() => self.report(
                Diagnostic::error(codes::UNTERMINATED_STRING, "unterminated splice")
                    .primary(begin, "this `#{` is never closed")
                    .suggest(
                        "close it with `}`",
                        vec![Edit { span: end.span, replacement: "}".into() }],
                        Applicability::MaybeIncorrect,
                    ),
            ),
            _ if i > 0 => self.error_expected("`}` to close the splice"),
            _ => {}
        }
        for _ in 0..i {
            self.bump();
        }
        self.eat(T::SpliceEnd);
    }

    /// Whether the splice at the current `SpliceBegin`, outside any `quote`,
    /// reads as a comment written without a space after `#`: it starts a
    /// line, follows a complete expression, or ends a line after a `,`.
    /// After `=`, `.` or `def` it is meant as code.
    fn stray_is_comment(&self) -> bool {
        let ends_line = self.splice_len(0).is_some_and(|n| matches!(self.nth(n).kind, T::Newline | T::Eof));
        match self.pos.checked_sub(1).map(|i| self.tokens[i].kind) {
            None | Some(T::Newline) => true,
            Some(T::Comma) => ends_line,
            Some(kind) => matches!(
                kind,
                T::Int
                    | T::Float
                    | T::Str(_)
                    | T::StrEnd
                    | T::Symbol
                    | T::Ident
                    | T::Const
                    | T::IVar
                    | T::TypeParam
                    | T::RParen
                    | T::RBracket
                    | T::RBrace
                    | T::SpliceEnd
                    | T::Kw(K::End | K::Nil | K::True | K::False | K::SelfKw | K::Then | K::Else | K::Do)
            ),
        }
    }

    /// Reports the splice at the current `SpliceBegin`, which is outside any
    /// `quote`, and skips it. When it reads as a comment, the rest of its
    /// line is skipped too, as the comment would be.
    fn stray_splice(&mut self) {
        let comment = self.stray_is_comment();
        let begin = self.bump();
        let mut depth = 1u32;
        while depth > 0 && !self.at(T::Eof) {
            match self.bump().kind {
                T::SpliceBegin => depth += 1,
                T::SpliceEnd => depth -= 1,
                _ => {}
            }
        }
        let end = self.prev_span();
        let span = begin.span.to(end);
        if self.splice_depth > 0 {
            let mut edits = vec![Edit { span: begin.span, replacement: String::new() }];
            if !end.is_empty() && end != begin.span {
                edits.push(Edit { span: end, replacement: String::new() });
            }
            self.report(
                Diagnostic::error(codes::SPLICE_OUTSIDE_QUOTE, "a splice inside a splice")
                    .primary(span, "this is already macro code")
                    .note("the expression inside `#{…}` runs in the macro, so it uses values directly")
                    .suggest("remove the inner `#{` and `}`", edits, Applicability::MachineApplicable),
            );
            return;
        }
        if comment {
            while !matches!(self.kind(), T::Newline | T::Eof) {
                self.bump();
            }
        }
        let after_hash = Span::new(self.file, begin.span.start + 1, begin.span.start + 1);
        let mut diag = Diagnostic::error(codes::SPLICE_OUTSIDE_QUOTE, "`#{` starts a splice outside a `quote`")
            .primary(span, "a splice only works inside `quote do … end`")
            .note("outside a string, `#{` always starts a splice; a comment starts with `# `");
        diag = if comment || end.is_empty() {
            let applicability = if comment { Applicability::MachineApplicable } else { Applicability::MaybeIncorrect };
            diag.suggest(
                "if this is a comment, add a space after `#`",
                vec![Edit { span: after_hash, replacement: " ".into() }],
                applicability,
            )
        } else {
            diag.suggest(
                "to write the code itself, remove `#{` and `}`",
                vec![
                    Edit { span: begin.span, replacement: String::new() },
                    Edit { span: end, replacement: String::new() },
                ],
                Applicability::MaybeIncorrect,
            )
        };
        if self.in_macro {
            diag = diag.help("to build code in a macro, splice values into a `quote do … end` and return it");
        }
        self.report(diag);
    }

    /// Parses the body of a `quote do … end` (see [`QuoteExpr`]).
    fn parse_quote_body(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if matches!(
                self.kind(),
                T::Eof | T::RBrace | T::SpliceEnd | T::Kw(K::End) | T::Kw(K::Else) | T::Kw(K::Elsif) | T::Kw(K::When)
            ) {
                break;
            }
            let before = self.pos;
            stmts.push(self.parse_quote_line());
            self.expect_stmt_end();
            if self.pos == before {
                self.bump();
            }
        }
        stmts
    }

    /// One line of a `quote` body: a declaration when the line can only
    /// start one, otherwise a statement.
    fn parse_quote_line(&mut self) -> Stmt {
        let start = self.peek().span;
        if self.quote_item_ahead() {
            let kind = match self.parse_item(ItemCtx::Package) {
                Some(item) => StmtKind::Item(Box::new(item)),
                None => StmtKind::Error,
            };
            return Stmt { kind, span: start.to(self.prev_span()), attrs: Vec::new() };
        }
        if self.at_kw(K::Comptime) && self.nth(1).kind == T::Kw(K::If) {
            return self.parse_quote_comptime_if();
        }
        self.parse_stmt()
    }

    /// Whether the line ahead, after any attributes and `private`, starts a
    /// declaration that can't be a statement.
    fn quote_item_ahead(&self) -> bool {
        let mut i = 0;
        while self.nth(i).kind == T::AtBracket {
            let mut depth = 0u32;
            loop {
                match self.nth(i).kind {
                    T::AtBracket | T::LBracket => depth += 1,
                    T::RBracket if depth <= 1 => break,
                    T::RBracket => depth -= 1,
                    T::Eof => return false,
                    _ => {}
                }
                i += 1;
            }
            i += 1;
            while self.nth(i).kind == T::Newline {
                i += 1;
            }
        }
        if self.nth(i).kind == T::Kw(K::Private) {
            i += 1;
        }
        match self.nth(i).kind {
            T::Kw(
                K::Def
                | K::Macro
                | K::Struct
                | K::Enum
                | K::Union
                | K::Module
                | K::Extend
                | K::Overload
                | K::Include
                | K::Import
                | K::Cimport,
            ) => true,
            T::Const => matches!(self.nth(i + 1).kind, T::Eq | T::Colon),
            _ => false,
        }
    }

    /// `comptime if` at the top of a `quote` body. Its branches follow the
    /// body's rules, so they may hold declarations as well as statements.
    fn parse_quote_comptime_if(&mut self) -> Stmt {
        let start = self.bump().span;
        let kw = self.bump();
        self.openers.push(Opener { keyword: "comptime if", span: start.to(kw.span) });
        let branch = |p: &mut Self| {
            let no_do = std::mem::replace(&mut p.no_do, true);
            let cond = p.parse_cond();
            p.no_do = no_do;
            p.eat_kw(K::Then);
            p.push_scope();
            p.declare_cond(&cond);
            let body = p.parse_quote_body();
            p.pop_scope();
            (cond, body)
        };
        let (cond, then) = branch(self);
        let mut elifs = Vec::new();
        while self.eat_kw(K::Elsif) {
            elifs.push(branch(self));
        }
        let else_ = if self.eat_kw(K::Else) {
            self.push_scope();
            let body = self.parse_quote_body();
            self.pop_scope();
            Some(body)
        } else {
            None
        };
        self.expect_end();
        let span = start.to(self.prev_span());
        let if_expr = IfExpr { cond, then, elifs, else_, unless: false };
        Stmt {
            kind: StmtKind::Expr(Expr { kind: ExprKind::ComptimeIf(Box::new(if_expr)), span }),
            span,
            attrs: Vec::new(),
        }
    }

    /// A declaration that starts with a splice, inside a `quote`: a field
    /// (`#{name}: T` in a struct), a constant (`#{name} = value`), or a
    /// splice standing alone ([`ItemKind::Splice`]).
    fn parse_splice_item(&mut self, ctx: ItemCtx) -> ItemKind {
        let after = self.splice_len(0).map(|n| self.nth(n));
        match after.map(|t| (t.kind, t.space_before)) {
            Some((T::Colon, false)) => {
                let name = self.parse_name("a name");
                self.bump();
                let ty = self.parse_type();
                let default = match (self.eat(T::Eq), ctx) {
                    (false, _) => None,
                    (true, ItemCtx::Package) => Some(self.parse_const_value()),
                    (true, _) => Some(self.parse_expr()),
                };
                match (ctx, default) {
                    (ItemCtx::Package, Some(value)) => {
                        ItemKind::Const(Box::new(ConstDecl { name, ty: Some(ty), value }))
                    }
                    (ItemCtx::Package, None) => {
                        self.error_expected("`=` and the constant's value");
                        ItemKind::Error
                    }
                    (_, default) => ItemKind::Field(Box::new(FieldDecl { name, ty, default, using: false })),
                }
            }
            Some((T::Eq, _)) => {
                let name = self.parse_name("a name");
                self.bump();
                let value = self.parse_const_value();
                ItemKind::Const(Box::new(ConstDecl { name, ty: None, value }))
            }
            _ => match self.parse_splice().0 {
                Some(index) => ItemKind::Splice(index),
                None => ItemKind::Error,
            },
        }
    }

    // ----- items ---------------------------------------------------------

    fn parse_items_until_eof(&mut self) -> Vec<Item> {
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::Eof) {
                break;
            }
            if self.at_kw(K::End) {
                let tok = self.bump();
                self.report(
                    Diagnostic::error(codes::UNEXPECTED_TOKEN, "unexpected `end`")
                        .primary(tok.span, "there is nothing here to close")
                        .help("remove this `end`, or check that the block above it was opened with `def`, `do`, `if` or similar"),
                );
                continue;
            }
            let before = self.pos;
            if let Some(item) = self.parse_item(ItemCtx::Package) {
                items.push(item);
            }
            self.expect_stmt_end();
            if self.pos == before {
                self.bump();
            }
        }
        items
    }

    fn parse_item_body(&mut self, ctx: ItemCtx) -> Vec<Item> {
        let mut items = Vec::new();
        loop {
            self.skip_newlines();
            if matches!(self.kind(), T::Eof | T::SpliceEnd | T::Kw(K::End | K::Else | K::Elsif)) {
                break;
            }
            let before = self.pos;
            if let Some(item) = self.parse_item(ctx) {
                items.push(item);
            }
            self.expect_stmt_end();
            if self.pos == before {
                self.bump();
            }
        }
        items
    }

    fn doc_before(&self, offset: u32) -> Option<String> {
        let line = self.line_of(offset);
        if line == 0 {
            return None;
        }
        let mut wanted = line - 1;
        let mut lines = Vec::new();
        for comment in self.comments.iter().rev() {
            if !comment.own_line || comment.span.start >= offset {
                continue;
            }
            let cline = self.line_of(comment.span.start);
            if cline == wanted {
                lines.push(comment.text.clone());
                if wanted == 0 {
                    break;
                }
                wanted -= 1;
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

    fn parse_attrs(&mut self) -> Vec<Attribute> {
        let mut attrs = Vec::new();
        while self.at(T::AtBracket) {
            self.bump();
            loop {
                self.skip_newlines();
                if self.at(T::RBracket) {
                    break;
                }
                let name = self.expect_ident("an attribute name");
                let mut args = Vec::new();
                if self.at(T::LParen) && !self.peek().space_before {
                    self.bump();
                    while !self.at(T::RParen) && !self.at(T::Eof) {
                        self.skip_newlines();
                        args.push(self.parse_expr());
                        self.skip_newlines();
                        if !self.eat(T::Comma) {
                            break;
                        }
                    }
                    self.expect(T::RParen, "`)`");
                } else if self.at(T::Colon) {
                    self.bump();
                    args.push(self.parse_expr());
                }
                attrs.push(Attribute { name, args, span: name.span.to(self.prev_span()) });
                if !self.eat(T::Comma) {
                    break;
                }
            }
            self.expect(T::RBracket, "`]` to close the attribute list");
            self.skip_newlines();
        }
        attrs
    }

    fn parse_item(&mut self, ctx: ItemCtx) -> Option<Item> {
        let start = self.peek().span;
        let doc = self.doc_before(start.start);
        let attrs = self.parse_attrs();
        let private = self.eat_kw(K::Private);
        let kind = match self.kind() {
            T::Kw(K::Import) => self.parse_import(),
            T::Kw(K::Cimport) => self.parse_cimport(),
            T::Kw(K::Def) => ItemKind::Def(Box::new(self.parse_def(false))),
            T::Kw(K::Macro) => {
                self.bump();
                if !self.at_kw(K::Def) {
                    self.error_expected("`def` after `macro`");
                    return None;
                }
                ItemKind::Def(Box::new(self.parse_def(true)))
            }
            T::Kw(K::Struct) => self.parse_struct(),
            T::Kw(K::Enum) => self.parse_enum(),
            T::Kw(K::Union) => self.parse_union(),
            T::Kw(K::Module) => self.parse_module(),
            T::Kw(K::Extend) => self.parse_extend(),
            T::Kw(K::Overload) => self.parse_overload(),
            T::Kw(K::Include) => {
                self.bump();
                ItemKind::Include(self.parse_type())
            }
            T::Kw(K::Using) if ctx != ItemCtx::Package => {
                self.bump();
                let name = self.parse_name("a field name");
                self.expect(T::Colon, "`:` and the field type");
                let ty = self.parse_type();
                ItemKind::Field(Box::new(FieldDecl { name, ty, default: None, using: true }))
            }
            T::Kw(K::Comptime) if self.nth(1).kind == T::Kw(K::If) => {
                self.bump();
                self.parse_comptime_if_item(ctx)
            }
            T::Const if matches!(self.nth(1).kind, T::Eq | T::Colon) => {
                let name = self.expect_const("a constant name");
                let ty = if self.eat(T::Colon) { Some(self.parse_type()) } else { None };
                self.expect(T::Eq, "`=` and the constant's value");
                let value = self.parse_const_value();
                ItemKind::Const(Box::new(ConstDecl { name, ty, value }))
            }
            T::Ident | T::Kw(_)
                if (ctx != ItemCtx::Package || self.at(T::Ident))
                    && self.nth(1).kind == T::Colon
                    && !self.nth(1).space_before =>
            {
                let tok = self.bump();
                let name = Ident { name: Name::new(self.text_of(tok.span)), span: tok.span };
                self.bump();
                let ty = self.parse_type();
                let default = if self.eat(T::Eq) { Some(self.parse_expr()) } else { None };
                ItemKind::Field(Box::new(FieldDecl { name, ty, default, using: false }))
            }
            T::SpliceBegin if self.quotes.is_empty() => {
                // Most likely a comment written without a space after `#`.
                self.stray_splice();
                return None;
            }
            T::SpliceBegin => self.parse_splice_item(ctx),
            T::Ident => {
                let save = self.pos;
                let splices = self.splice_mark();
                let expr = self.parse_expr_cmd();
                let is_statement =
                    self.at(T::Eq) || is_assign_op(self.kind()) || self.at(T::Comma) || self.at_modifier();
                // A call, a name, or a package member (`lib.make`) may be a
                // macro call.
                let qualified = matches!(
                    &expr.kind,
                    ExprKind::Member { recv, safe: false, .. } if matches!(recv.kind, ExprKind::Ident(_))
                );
                match &expr.kind {
                    ExprKind::Call(_) | ExprKind::Ident(_) if !is_statement => ItemKind::MacroCall(Box::new(expr)),
                    _ if qualified && !is_statement => ItemKind::MacroCall(Box::new(expr)),
                    _ => {
                        self.pos = save;
                        self.rewind_splices(splices);
                        let stmt = self.parse_stmt();
                        self.top_level_statement(stmt.span, ctx);
                        ItemKind::Error
                    }
                }
            }
            _ => {
                let tok = self.peek();
                if matches!(
                    tok.kind,
                    T::Kw(
                        K::If
                            | K::Unless
                            | K::While
                            | K::Until
                            | K::For
                            | K::Loop
                            | K::Case
                            | K::Return
                            | K::Defer
                            | K::Guard
                    )
                ) || matches!(tok.kind, T::IVar | T::AtSplice)
                {
                    let stmt = self.parse_stmt();
                    self.top_level_statement(stmt.span, ctx);
                } else {
                    let what = match ctx {
                        ItemCtx::Package => "a declaration (`def`, `struct`, `enum`, `import`, …)",
                        ItemCtx::Struct => "a field or method",
                        ItemCtx::Enum => "an enum member or method",
                        ItemCtx::Module | ItemCtx::Extend => "a method",
                    };
                    self.error_expected(what);
                    self.recover_line();
                }
                ItemKind::Error
            }
        };
        let span = start.to(self.prev_span());
        Some(Item { kind, span, attrs, private, doc })
    }

    /// The value of a constant after its `=`: an expression, or a type that
    /// only reads as one (`distinct F64`, `proc(Int)`, `^Node?`, `C.int`,
    /// `(proc(Int) -> Int)?`).
    fn parse_const_value(&mut self) -> Expr {
        let type_start = self.at(T::AtBracket)
            || (self.at(T::Ident) && matches!(self.text_of(self.peek().span), "distinct" | "proc"));
        if type_start {
            let texpr = self.parse_type();
            Expr { span: texpr.span, kind: ExprKind::Type(Box::new(texpr)) }
        } else if let Some(texpr) = self.try_type_alias() {
            Expr { span: texpr.span, kind: ExprKind::Type(Box::new(texpr)) }
        } else {
            self.parse_expr()
        }
    }

    fn top_level_statement(&mut self, span: Span, ctx: ItemCtx) {
        let mut diag = Diagnostic::error(codes::TOP_LEVEL_STATEMENT, "statements must be inside a method")
            .primary(span, "this statement is outside any `def`");
        if ctx == ItemCtx::Package {
            diag = diag.help("move it into `def main … end`, which runs when the program starts");
        }
        self.report(diag);
    }

    fn parse_string_literal(&mut self, what: &str) -> Option<(String, Span)> {
        let tok = self.peek();
        if let T::Str(idx) = tok.kind {
            self.bump();
            Some((self.strings[idx as usize].clone(), tok.span))
        } else {
            self.error_expected(what);
            None
        }
    }

    fn parse_import(&mut self) -> ItemKind {
        self.bump();
        let Some((path, path_span)) = self.parse_string_literal("an import path like \"core:fmt\"") else {
            self.recover_line();
            return ItemKind::Error;
        };
        let mut alias = None;
        while self.eat(T::Comma) {
            let key = self.expect_ident("`as:`");
            self.expect(T::Colon, "`:`");
            if key.as_str() == "as" {
                let tok = self.peek();
                if tok.kind == T::Symbol {
                    self.bump();
                    let text = &self.text_of(tok.span)[1..];
                    alias = Some(Ident { name: Name::new(text), span: tok.span });
                } else {
                    self.error_expected("a symbol like `:rl`");
                }
            } else {
                self.report(
                    Diagnostic::error(codes::BAD_NAMED_ARG, format!("unknown import option `{}`", key.as_str()))
                        .primary(key.span, "imports only accept `as:`"),
                );
                self.parse_expr();
            }
        }
        ItemKind::Import(Import { path, path_span, alias })
    }

    fn parse_cimport(&mut self) -> ItemKind {
        self.bump();
        let Some((header, header_span)) = self.parse_string_literal("a header path like \"stb_image.h\"") else {
            self.recover_line();
            return ItemKind::Error;
        };
        let mut options = Vec::new();
        while self.eat(T::Comma) {
            self.skip_newlines();
            let name = self.expect_ident("an option name");
            self.expect(T::Colon, "`:`");
            let value = if self.at(T::LBrace) {
                self.parse_cimport_hash(name.as_str() == "types")
            } else {
                CimportValue::Expr(self.parse_expr())
            };
            options.push(CimportOption { name, value, span: name.span.to(self.prev_span()) });
        }
        ItemKind::Cimport(Cimport { header, header_span, options })
    }

    /// Parses `{Key: value, "Key": value}` after a `cimport` option. The
    /// values are types when `types` is set.
    fn parse_cimport_hash(&mut self, types: bool) -> CimportValue {
        let open = self.bump();
        let mut entries = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::RBrace) || self.at(T::Eof) {
                break;
            }
            let tok = self.peek();
            let key = match tok.kind {
                T::Ident | T::Const => self.text_of(tok.span).to_string(),
                T::Str(_) => match self.parse_string_literal("a key") {
                    Some((text, _)) => {
                        self.expect(T::Colon, "`:` after the key");
                        self.skip_newlines();
                        let value = self.cimport_hash_value(types);
                        entries.push(CimportEntry { key: text, key_span: tok.span, value });
                        if !self.eat(T::Comma) {
                            break;
                        }
                        continue;
                    }
                    None => String::new(),
                },
                _ => {
                    self.report(
                        Diagnostic::error(
                            codes::UNEXPECTED_TOKEN,
                            format!("expected a key, found {}", tok.kind.describe()),
                        )
                        .primary(tok.span, "write `Key: value`"),
                    );
                    break;
                }
            };
            self.bump();
            self.expect(T::Colon, "`:` after the key");
            self.skip_newlines();
            let value = self.cimport_hash_value(types);
            entries.push(CimportEntry { key, key_span: tok.span, value });
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.skip_newlines();
        self.expect(T::RBrace, "`}` to close the hash");
        CimportValue::Hash { entries, span: open.span.to(self.prev_span()) }
    }

    /// A value in a `cimport` option hash: a type for `types:`, otherwise an
    /// expression.
    fn cimport_hash_value(&mut self, types: bool) -> Expr {
        if types {
            let t = self.parse_type();
            Expr { span: t.span, kind: ExprKind::Type(Box::new(t)) }
        } else {
            self.parse_expr()
        }
    }

    fn parse_def_name(&mut self) -> Ident {
        let tok = self.peek();
        let text: Option<String> = match tok.kind {
            T::Ident => Some(self.text_of(tok.span).to_string()),
            T::SpliceBegin => return self.parse_name("a method name"),
            T::Plus
            | T::Minus
            | T::Star
            | T::Slash
            | T::Percent
            | T::StarStar
            | T::EqEq
            | T::NotEq
            | T::Lt
            | T::Le
            | T::Gt
            | T::Ge
            | T::Cmp
            | T::Shl
            | T::Shr
            | T::Amp
            | T::Pipe
            | T::Tilde
            | T::Bang => Some(self.text_of(tok.span).to_string()),
            T::LBracket => {
                self.bump();
                if !self.at(T::RBracket) {
                    self.error_expected("`]` for the `[]` operator");
                    return Ident { name: Name::new("<error>"), span: tok.span };
                }
                let close = self.peek();
                let mut name = "[]".to_string();
                let mut span = tok.span.to(close.span);
                if self.nth(1).kind == T::Eq && !self.nth(1).space_before {
                    self.bump();
                    span = span.to(self.peek().span);
                    name.push('=');
                }
                self.bump();
                return Ident { name: Name::new(&name), span };
            }
            T::Const => {
                self.report(
                    Diagnostic::error(codes::UNEXPECTED_TOKEN, "method names start with a lowercase letter")
                        .primary(tok.span, "this looks like a type name")
                        .help("types are declared with `struct`, `enum` or `union`; methods use snake_case names"),
                );
                Some(self.text_of(tok.span).to_string())
            }
            T::Kw(k) => Some(k.as_str().to_string()),
            _ => None,
        };
        match text {
            Some(text) => {
                self.bump();
                Ident { name: Name::new(&text), span: tok.span }
            }
            None => {
                self.error_expected("a method name");
                Ident { name: Name::new("<error>"), span: tok.span }
            }
        }
    }

    fn parse_def(&mut self, is_macro: bool) -> FnDecl {
        let def_tok = self.bump();
        let mut is_static = false;
        if self.at_kw(K::SelfKw) && self.nth(1).kind == T::Dot {
            self.bump();
            self.bump();
            is_static = true;
        }
        let name = self.parse_def_name();
        self.push_def_scope();
        let outer_yield = std::mem::replace(&mut self.yield_seen, false);
        let outer_macro = std::mem::replace(&mut self.in_macro, is_macro);
        let owner = if is_macro { ParamsOf::Macro } else { ParamsOf::Def(name.name) };
        let (params, block, c_variadic) =
            if self.at(T::LParen) { self.parse_params(owner) } else { (Vec::new(), None, None) };
        let uninferred = std::mem::take(&mut self.uninferred);
        let mut ret = if self.eat(T::Arrow) { Some(self.parse_type()) } else { None };
        // A loose `$T` that no parameter could introduce was reported; its
        // uses in the return type would only repeat that as unknown types.
        if let Some(ret) = &mut ret {
            for name in uninferred {
                while let Some(ty) = find_type(ret, &|k| is_plain_name(k, name)) {
                    ty.kind = TypeKind::Error;
                }
            }
        }
        let sig_span = def_tok.span.to(self.prev_span());
        let body = if self.eat(T::Eq) {
            self.skip_newlines();
            FnBody::Expr(Box::new(self.parse_expr_cmd()))
        } else {
            self.openers.push(Opener { keyword: "def", span: def_tok.span.to(name.span) });
            let body = self.parse_block_body();
            self.expect_end();
            FnBody::Block(body)
        };
        self.pop_def_scope();
        self.in_macro = outer_macro;
        let yields = std::mem::replace(&mut self.yield_seen, outer_yield);
        FnDecl { name, is_static, is_macro, params, block, ret, body, sig_span, yields, c_variadic }
    }

    fn parse_params(&mut self, owner: ParamsOf) -> (Vec<Param>, Option<BlockParamDecl>, Option<Span>) {
        self.bump();
        let mut params = Vec::new();
        let mut stars = Vec::new();
        let mut block = None;
        let mut variadic = None;
        let mut loose = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::RParen) || self.at(T::Eof) {
                break;
            }
            if self.at(T::TypeParam) {
                loose.push(self.skip_loose_type_param());
                self.skip_newlines();
                if !self.eat(T::Comma) {
                    break;
                }
                continue;
            }
            if self.at(T::DotDotDot) {
                let dots = self.bump();
                variadic = Some(dots.span);
                self.skip_newlines();
                if !self.at(T::RParen) {
                    self.report(
                        Diagnostic::error(codes::UNEXPECTED_TOKEN, "`...` must be the last parameter")
                            .primary(dots.span, "C variadic arguments come after every named parameter"),
                    );
                }
                break;
            }
            let start = self.peek().span;
            if self.eat(T::Amp) {
                let name = self.parse_name("a block parameter name");
                if !self.eat(T::Colon) {
                    self.report(
                        Diagnostic::error(codes::UNEXPECTED_TOKEN, "block parameters need a type")
                            .primary(name.span, "write the block's shape here")
                            .suggest(
                                "declare what the block receives and returns",
                                vec![Edit { span: name.span.shrink_to_end(), replacement: ": block(T)".into() }],
                                Applicability::HasPlaceholders,
                            ),
                    );
                }
                let ty = if self.at(T::Comma) || self.at(T::RParen) {
                    TypeExpr { kind: TypeKind::Error, span: name.span }
                } else {
                    self.parse_type()
                };
                block = Some(BlockParamDecl { name, ty, span: start.to(self.prev_span()) });
                self.declare(name.name);
            } else {
                let splat = self.at(T::Star);
                if splat {
                    stars.push((params.len(), self.bump().span));
                }
                let name = self.parse_name("a parameter name");
                self.declare(name.name);
                let ty = if self.eat(T::Colon) {
                    self.parse_type()
                } else {
                    self.report(
                        Diagnostic::error(
                            codes::UNEXPECTED_TOKEN,
                            format!("parameter `{}` needs a type", self.text_of(name.span)),
                        )
                        .primary(name.span, "every parameter has a declared type")
                        .suggest(
                            "add its type (`Int` is only an example)",
                            vec![Edit { span: name.span.shrink_to_end(), replacement: ": Int".into() }],
                            Applicability::HasPlaceholders,
                        ),
                    );
                    TypeExpr { kind: TypeKind::Error, span: name.span }
                };
                let default = if self.eat(T::Eq) { Some(self.parse_expr()) } else { None };
                params.push(Param { name, ty, default, splat, span: start.to(self.prev_span()) });
            }
            self.skip_newlines();
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.skip_newlines();
        self.expect(T::RParen, "`)` to close the parameter list");
        for (index, star) in stars {
            self.check_variadic_param(owner, &mut params, index, star);
        }
        for tp in &loose {
            self.report_loose_type_param(owner, &mut params, tp);
        }
        (params, block, variadic)
    }

    /// Skips a `$T` written as a parameter of its own, with any `: type`
    /// after it, up to the `,`, `)` or line end that ends it.
    fn skip_loose_type_param(&mut self) -> LooseTypeParam {
        let first = self.pos;
        let tok = self.bump();
        let typed = self.at(T::Colon);
        let mut depth = 0i32;
        loop {
            match self.kind() {
                T::Eof => break,
                T::Comma | T::RParen | T::Newline if depth <= 0 => break,
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                T::RParen | T::RBracket | T::RBrace => depth -= 1,
                _ => {}
            }
            self.bump();
        }
        let name = Name::new(&self.text_of(tok.span)[1..]);
        LooseTypeParam { name, tok: tok.span, first, after: self.pos, typed }
    }

    /// Reports a `$T` written as a parameter of its own (E0105). In a
    /// method, the fix removes it and introduces `$T` where a parameter's
    /// type first uses `T`, and the parameters recover as that fix.
    fn report_loose_type_param(&mut self, owner: ParamsOf, params: &mut [Param], tp: &LooseTypeParam) {
        let name = tp.name;
        let label = match owner {
            ParamsOf::Def(_) => "a type parameter is introduced inside a parameter's type",
            ParamsOf::Macro => "a macro takes no type parameters",
            ParamsOf::Proc => "a proc takes no type parameters",
        };
        let mut diag = Diagnostic::error(codes::UNEXPECTED_TOKEN, format!("`${name}` can't be a parameter on its own"))
            .primary(tp.tok, label);
        if tp.typed && owner != ParamsOf::Macro {
            diag = diag.note(format!(
                "unlike Odin's `${name}: typeid`, a type parameter is never passed: it is inferred from the arguments"
            ));
        }
        let example = format!("like `def first(xs: []${name}) -> {name}`");
        match owner {
            ParamsOf::Macro => {
                diag = diag.help("a macro receives a type through a `Type` parameter, like `t: Type`");
            }
            ParamsOf::Proc => {
                diag = diag.help(format!("give its parameters concrete types; a method can be generic, {example}"));
            }
            ParamsOf::Def(_) => {
                let removal = Edit { span: self.loose_param_removal(tp), replacement: String::new() };
                let introduced = |k: &TypeKind| matches!(k, TypeKind::Param(id) if id.name == name);
                if let Some(param) = params.iter_mut().find_map(|p| find_type(&mut p.ty, &introduced).map(|_| p.name)) {
                    diag = diag.suggest(
                        format!("remove it: the type of `{}` already introduces `${name}`", param.as_str()),
                        vec![removal],
                        Applicability::MachineApplicable,
                    );
                } else if let Some((param, ty)) = params
                    .iter_mut()
                    .find_map(|p| find_type(&mut p.ty, &|k| is_plain_name(k, name)).map(|ty| (p.name, ty)))
                {
                    let span = ty.span;
                    ty.kind = TypeKind::Param(Ident { name, span });
                    diag = diag.suggest(
                        format!(
                            "introduce `${name}` in the type of `{}`, which `{name}` is inferred from",
                            param.as_str()
                        ),
                        vec![removal, Edit { span, replacement: format!("${name}") }],
                        Applicability::MachineApplicable,
                    );
                } else {
                    diag = diag.help(format!(
                        "write `${name}` inside the type of the parameter `{name}` is inferred from, {example}"
                    ));
                    self.uninferred.push(name);
                }
            }
        }
        self.report(diag);
    }

    /// The text to delete for a loose `$T` parameter: `$T, ` up to the
    /// next parameter, or `, $T` after the previous one when it is last.
    fn loose_param_removal(&self, tp: &LooseTypeParam) -> Span {
        let tokens = &self.tokens;
        let end = tokens[tp.after.saturating_sub(1)].span.end;
        if tokens[tp.after].kind == T::Comma
            && let Some(next) = tokens[tp.after + 1..].iter().find(|t| t.kind != T::Newline)
            && next.kind != T::RParen
        {
            return Span::new(self.file, tp.tok.start, next.span.start);
        }
        let before = tokens[..tp.first].iter().rposition(|t| t.kind != T::Newline);
        if let Some(comma) = before.filter(|&i| tokens[i].kind == T::Comma)
            && let Some(prev) = tokens[..comma].iter().rev().find(|t| t.kind != T::Newline)
        {
            return Span::new(self.file, prev.span.end, end);
        }
        Span::new(self.file, tp.tok.start, end)
    }

    /// Reports a `*` parameter anywhere but last in a `macro def`, or with
    /// a default (E0112). Outside a macro, recovers as if the suggested
    /// `[]T` parameter had been written.
    fn check_variadic_param(&mut self, owner: ParamsOf, params: &mut [Param], index: usize, star: Span) {
        let count = params.len();
        let param = &mut params[index];
        let text = self.text_of(param.span);
        if owner != ParamsOf::Macro {
            let (what, call) = match owner {
                ParamsOf::Def(name) if splice_index(name).is_none() => ("a method", format!("{name}([a, b, c])")),
                ParamsOf::Def(_) | ParamsOf::Macro => ("a method", "f([a, b, c])".to_string()),
                ParamsOf::Proc => ("a proc", "f.call([a, b, c])".to_string()),
            };
            let ty = self.text_of(param.ty.span);
            // Without a type (already reported) there is no slice to suggest.
            let typed = !matches!(param.ty.kind, TypeKind::Error);
            if typed {
                self.report(
                    Diagnostic::error(codes::VARIADIC_PARAM, "only a `macro def` can take a `*` parameter")
                        .primary(param.span, "this would collect the remaining arguments")
                        .note(format!(
                            "{what} takes a fixed number of arguments; a `*` parameter is for macros, \
                         like `flags :READ, :WRITE`"
                        ))
                        .suggest(
                            format!("take a slice instead, and pass an array literal: `{call}`"),
                            vec![
                                Edit { span: star, replacement: String::new() },
                                Edit { span: param.ty.span, replacement: format!("[]{ty}") },
                            ],
                            Applicability::MaybeIncorrect,
                        ),
                );
            }
            // Recover as the suggested slice parameter. It keeps `splat`, so
            // sema lets it take a call's remaining arguments without more
            // errors.
            let elem = std::mem::replace(&mut param.ty, TypeExpr { kind: TypeKind::Error, span: star });
            param.ty = TypeExpr { span: elem.span, kind: TypeKind::Slice(Box::new(elem)) };
            param.default = None;
            return;
        }
        if index + 1 < count {
            let last = params[count - 1].span;
            let next = params[index + 1].span;
            let param = &params[index];
            self.report(
                Diagnostic::error(codes::VARIADIC_PARAM, "a `*` parameter must be the last parameter")
                    .primary(param.span, "this collects every remaining argument")
                    .secondary(next, "so no parameter can come after it")
                    .suggest(
                        "move it to the end",
                        vec![
                            Edit {
                                span: Span::new(self.file, param.span.start, next.start),
                                replacement: String::new(),
                            },
                            Edit { span: last.shrink_to_end(), replacement: format!(", {text}") },
                        ],
                        Applicability::MaybeIncorrect,
                    ),
            );
        }
        let param = &params[index];
        if let Some(default) = &param.default {
            self.report(
                Diagnostic::error(codes::VARIADIC_PARAM, "a `*` parameter can't have a default")
                    .primary(default.span, "a default for a `*` parameter")
                    .note("it collects zero or more arguments, so without any it is an empty slice")
                    .suggest_replace(
                        "remove the default",
                        Span::new(self.file, param.ty.span.end, default.span.end),
                        "",
                        Applicability::MachineApplicable,
                    ),
            );
        }
    }

    fn parse_generic_params(&mut self) -> Vec<GenericParam> {
        let mut out = Vec::new();
        if !self.at(T::LParen) || self.peek().space_before {
            return out;
        }
        self.bump();
        loop {
            self.skip_newlines();
            if self.at(T::RParen) {
                break;
            }
            let tok = self.peek();
            if tok.kind == T::TypeParam {
                self.bump();
                let name = Ident { name: Name::new(&self.text_of(tok.span)[1..]), span: tok.span };
                let ty = if self.eat(T::Colon) { Some(self.parse_type()) } else { None };
                out.push(GenericParam { name, ty, span: tok.span.to(self.prev_span()) });
            } else {
                self.error_expected("a generic parameter like `$T`");
                self.bump();
            }
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.expect(T::RParen, "`)`");
        out
    }

    fn parse_struct(&mut self) -> ItemKind {
        let kw = self.bump();
        let name = self.parse_type_name("a struct name");
        let generics = self.parse_generic_params();
        self.openers.push(Opener { keyword: "struct", span: kw.span.to(name.span) });
        let body = self.parse_item_body(ItemCtx::Struct);
        self.expect_end();
        ItemKind::Struct(Box::new(StructDecl { name, generics, body }))
    }

    fn parse_enum(&mut self) -> ItemKind {
        let kw = self.bump();
        let name = self.parse_type_name("an enum name");
        let backing = if self.eat(T::Colon) { Some(self.parse_type()) } else { None };
        self.openers.push(Opener { keyword: "enum", span: kw.span.to(name.span) });
        let mut members = Vec::new();
        let mut body = Vec::new();
        loop {
            self.skip_newlines();
            if matches!(self.kind(), T::Eof | T::SpliceEnd | T::Kw(K::End)) {
                break;
            }
            // A splice is a member when a value or another member follows;
            // standing alone it is an `ItemKind::Splice`. `struct`, `enum`
            // and `union` standing alone are members (`TypeKind.struct`).
            let member = match self.name_len(0) {
                Some(n) if self.at(T::Ident) => {
                    matches!(self.nth(n).kind, T::Newline | T::Eq | T::Comma | T::Kw(K::End))
                }
                Some(n) => matches!(self.nth(n).kind, T::Eq | T::Comma),
                None => {
                    is_keyword_member(self.kind())
                        && matches!(self.nth(1).kind, T::Newline | T::Eq | T::Comma | T::Kw(K::End))
                }
            };
            if member {
                loop {
                    let member = self.parse_enum_member();
                    let value = if self.eat(T::Eq) { Some(self.parse_expr()) } else { None };
                    members.push(EnumMember { name: member, value });
                    if !self.eat(T::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
            } else if self.at(T::Const) && matches!(self.nth(1).kind, T::Newline | T::Comma | T::Kw(K::End)) {
                let tok = self.bump();
                let text = self.text_of(tok.span);
                let lower = to_snake_case(text);
                self.report(
                    Diagnostic::error(codes::UNEXPECTED_TOKEN, "enum members are lowercase")
                        .primary(tok.span, "members are written like symbols")
                        .note("members are selected with symbols such as `:north`, so they use snake_case")
                        .suggest_replace(
                            "use a lowercase name",
                            tok.span,
                            lower.clone(),
                            Applicability::MachineApplicable,
                        ),
                );
                members.push(EnumMember { name: Ident { name: Name::new(&lower), span: tok.span }, value: None });
            } else if let Some(item) = self.parse_item(ItemCtx::Enum) {
                body.push(item);
            }
            self.expect_stmt_end();
        }
        self.expect_end();
        ItemKind::Enum(Box::new(EnumDecl { name, backing, members, body }))
    }

    /// An enum member's name: an identifier, a splice, or `struct`, `enum`
    /// or `union` standing alone, so an enum can name the kinds of types
    /// (`TypeKind.struct`).
    fn parse_enum_member(&mut self) -> Ident {
        let tok = self.peek();
        if is_keyword_member(tok.kind) && matches!(self.nth(1).kind, T::Newline | T::Eq | T::Comma | T::Kw(K::End)) {
            self.bump();
            return Ident { name: Name::new(self.text_of(tok.span)), span: tok.span };
        }
        self.parse_name("an enum member")
    }

    fn parse_union(&mut self) -> ItemKind {
        self.bump();
        let name = self.parse_type_name("a union name");
        let generics = self.parse_generic_params();
        self.expect(T::Eq, "`=` followed by the variants, like `union Shape = Circle | Rect`");
        let mut variants = vec![self.parse_type()];
        while self.eat(T::Pipe) {
            variants.push(self.parse_type());
        }
        ItemKind::Union(Box::new(UnionDecl { name, generics, variants }))
    }

    fn parse_module(&mut self) -> ItemKind {
        let kw = self.bump();
        let name = self.parse_type_name("a module name");
        self.openers.push(Opener { keyword: "module", span: kw.span.to(name.span) });
        let body = self.parse_item_body(ItemCtx::Module);
        self.expect_end();
        ItemKind::Module(Box::new(ModuleDecl { name, body }))
    }

    fn parse_extend(&mut self) -> ItemKind {
        let kw = self.bump();
        let mut targets = vec![self.parse_type()];
        while self.eat(T::Comma) {
            self.skip_newlines();
            targets.push(self.parse_type());
        }
        self.openers.push(Opener { keyword: "extend", span: kw.span.to(self.prev_span()) });
        let body = self.parse_item_body(ItemCtx::Extend);
        self.expect_end();
        ItemKind::Extend(Box::new(ExtendDecl { targets, body }))
    }

    fn parse_symbol_ident(&mut self, what: &str) -> Option<Ident> {
        let tok = self.peek();
        if tok.kind == T::ColonSplice {
            self.bump();
            return self.parse_splice_name().map(|name| Ident { span: tok.span.to(name.span), ..name });
        }
        if tok.kind == T::Symbol {
            self.bump();
            Some(Ident { name: Name::new(&self.text_of(tok.span)[1..]), span: tok.span })
        } else {
            self.error_expected(what);
            None
        }
    }

    fn parse_overload(&mut self) -> ItemKind {
        self.bump();
        let Some(name) = self.parse_symbol_ident("the overloaded name as a symbol, like `:*`") else {
            self.recover_line();
            return ItemKind::Error;
        };
        let mut members = Vec::new();
        while self.eat(T::Comma) {
            if let Some(m) = self.parse_symbol_ident("a member name as a symbol") {
                members.push(m);
            }
        }
        ItemKind::Overload(OverloadDecl { name, members })
    }

    fn parse_comptime_if_item(&mut self, ctx: ItemCtx) -> ItemKind {
        let kw = self.bump();
        self.openers.push(Opener { keyword: "comptime if", span: kw.span });
        let item = self.parse_comptime_if_chain(ctx);
        self.expect_end();
        item
    }

    /// The condition and branches of a declaration-level `comptime if`, with
    /// `elsif` branches nested in the `else` branch. The caller consumes the
    /// closing `end`.
    fn parse_comptime_if_chain(&mut self, ctx: ItemCtx) -> ItemKind {
        let cond = self.parse_expr();
        self.eat_kw(K::Then);
        let then = self.parse_item_body(ctx);
        let else_ = if self.at_kw(K::Elsif) {
            let start = self.bump().span;
            let nested = self.parse_comptime_if_chain(ctx);
            vec![Item { kind: nested, span: start.to(self.prev_span()), attrs: Vec::new(), private: false, doc: None }]
        } else if self.eat_kw(K::Else) {
            self.parse_item_body(ctx)
        } else {
            Vec::new()
        };
        ItemKind::ComptimeIf(Box::new(ComptimeIfItem { cond, then, else_ }))
    }

    fn expect_end(&mut self) {
        let opener = self.openers.pop();
        if self.at_kw(K::End) {
            let end = self.bump();
            if let Some(o) = &opener {
                let opener_indent = self.indent_of_line(self.line_of(o.span.start));
                let end_indent = self.indent_of_line(self.line_of(end.span.start));
                self.closed.push(Closed {
                    keyword: o.keyword,
                    span: o.span,
                    opener_indent,
                    end_span: end.span,
                    end_indent,
                });
            }
            return;
        }
        let tok = self.peek();
        let Some(mut opener) = opener else {
            self.error_expected("`end`");
            return;
        };
        // An inner block whose `end` is dedented past its opener probably
        // stole the `end` that belongs to this block; blame the inner one.
        let mut stolen = None;
        if tok.kind == T::Eof
            && let Some(c) =
                self.closed.iter().find(|c| c.span.start > opener.span.start && c.end_indent < c.opener_indent)
        {
            stolen = Some(c.end_span);
            opener = Opener { keyword: c.keyword, span: c.span };
        }
        let mut diag = Diagnostic::error(codes::MISSING_END, format!("this `{}` is never closed", opener.keyword))
            .primary(opener.span, format!("`{}` opened here needs a matching `end`", opener.keyword));
        if let Some(end_span) = stolen {
            diag = diag
                .secondary(end_span, "this `end` is indented like an outer block, so it probably closes that instead");
        } else if tok.kind != T::Eof {
            diag = diag.secondary(tok.span, format!("expected `end` before {}", self.found()));
        }
        let opener_line = self.line_of(opener.span.start);
        let indent = self.indent_of_line(opener_line);
        let mut insert_at = None;
        for line in opener_line + 1..self.line_starts.len() {
            let start = self.line_starts[line] as usize;
            let end = self.line_starts.get(line + 1).map_or(self.text.len(), |e| *e as usize);
            let content = self.text[start..end].trim();
            if content.is_empty() || (content.starts_with('#') && !content.starts_with("#{")) {
                continue;
            }
            if self.indent_of_line(line) <= indent
                && !["else", "elsif", "when", "}"].iter().any(|k| content.starts_with(k))
                && !(content.starts_with("end") && self.indent_of_line(line) == indent)
            {
                insert_at = Some(start as u32);
                break;
            }
        }
        let pad = " ".repeat(indent);
        match insert_at {
            Some(at) => {
                diag = diag.suggest(
                    "the indentation suggests the `end` belongs here",
                    vec![Edit { span: Span::new(self.file, at, at), replacement: format!("{pad}end\n") }],
                    Applicability::MaybeIncorrect,
                );
            }
            None => {
                let at = self.text.len() as u32;
                let nl = if self.text.ends_with('\n') { "" } else { "\n" };
                diag = diag.suggest(
                    "add `end` at the end of the file",
                    vec![Edit { span: Span::new(self.file, at, at), replacement: format!("{nl}{pad}end\n") }],
                    Applicability::MaybeIncorrect,
                );
            }
        }
        self.report(diag);
    }

    // ----- types ---------------------------------------------------------

    /// Parses a type, reporting errors.
    fn parse_type(&mut self) -> TypeExpr {
        let ty = self.parse_type_base();
        self.parse_type_suffix(ty)
    }

    fn parse_type_suffix(&mut self, mut ty: TypeExpr) -> TypeExpr {
        if let Some(q) = self.pending_optional.take() {
            ty = TypeExpr { span: ty.span.to(q), kind: TypeKind::Optional(Box::new(ty)) };
        }
        while self.at(T::Question) && !self.peek().space_before {
            let q = self.bump();
            ty = TypeExpr { span: ty.span.to(q.span), kind: TypeKind::Optional(Box::new(ty)) };
        }
        ty
    }

    fn parse_type_base(&mut self) -> TypeExpr {
        let tok = self.peek();
        let start = tok.span;
        let kind = match tok.kind {
            T::Caret => {
                self.bump();
                TypeKind::Pointer(Box::new(self.parse_type_base()))
            }
            T::LBracket => {
                self.bump();
                if self.eat(T::Caret) {
                    self.expect(T::RBracket, "`]`");
                    TypeKind::MultiPointer(Box::new(self.parse_type_base()))
                } else if self.eat(T::RBracket) {
                    TypeKind::Slice(Box::new(self.parse_type()))
                } else if self.at(T::Ident)
                    && self.text_of(self.peek().span) == "dynamic"
                    && self.nth(1).kind == T::RBracket
                {
                    self.bump();
                    self.bump();
                    TypeKind::Dynamic(Box::new(self.parse_type()))
                } else {
                    let len = self.parse_expr();
                    self.expect(T::RBracket, "`]`");
                    TypeKind::Array(Box::new(len), Box::new(self.parse_type()))
                }
            }
            T::LParen => {
                self.bump();
                let mut elems = Vec::new();
                loop {
                    self.skip_newlines();
                    if self.at(T::RParen) {
                        break;
                    }
                    elems.push(self.parse_type());
                    if !self.eat(T::Comma) {
                        break;
                    }
                }
                self.skip_newlines();
                self.expect(T::RParen, "`)`");
                if elems.len() == 1 {
                    let inner = elems.pop().expect("one element");
                    return TypeExpr { span: start.to(self.prev_span()), kind: inner.kind };
                }
                TypeKind::Tuple(elems)
            }
            T::TypeParam => {
                self.bump();
                TypeKind::Param(Ident { name: Name::new(&self.text_of(tok.span)[1..]), span: tok.span })
            }
            T::Ident if self.text_of(tok.span) == "map" && self.nth(1).kind == T::LBracket => {
                self.bump();
                self.bump();
                let key = self.parse_type();
                self.expect(T::RBracket, "`]`");
                let value = self.parse_type();
                TypeKind::Map(Box::new(key), Box::new(value))
            }
            T::AtBracket => {
                let attrs = self.parse_attrs();
                let mut c_abi = false;
                for attr in &attrs {
                    if attr.name.as_str() == "c" && attr.args.is_empty() {
                        c_abi = true;
                    } else {
                        self.report(
                            Diagnostic::error(
                                codes::UNEXPECTED_TOKEN,
                                format!("`{}` is not a proc type attribute", attr.name.as_str()),
                            )
                            .primary(attr.span, "proc types only take `c`, as in `@[c] proc(C.int)`"),
                        );
                    }
                }
                let next = self.peek();
                if !(next.kind == T::Ident && self.text_of(next.span) == "proc") {
                    self.report(
                        Diagnostic::error(
                            codes::UNEXPECTED_TOKEN,
                            format!("expected `proc`, found {}", next.kind.describe()),
                        )
                        .primary(next.span, "only a `proc(…)` type can have attributes")
                        .help("write a C callback type like `@[c] proc(C.int) -> C.int`"),
                    );
                    return TypeExpr { kind: TypeKind::Error, span: start.to(self.prev_span()) };
                }
                let mut inner = self.parse_type_base();
                if let TypeKind::Proc { c_abi: abi, .. } = &mut inner.kind {
                    *abi = c_abi;
                }
                inner.span = start.to(inner.span);
                return inner;
            }
            T::Ident if matches!(self.text_of(tok.span), "proc" | "block") => {
                let is_block = self.text_of(tok.span) == "block";
                self.bump();
                let has_params = self.at(T::LParen) && !self.peek().space_before;
                if has_params {
                    self.bump();
                }
                let mut params = Vec::new();
                let mut variadic = false;
                if has_params {
                    loop {
                        self.skip_newlines();
                        if self.at(T::RParen) || self.at(T::Eof) {
                            break;
                        }
                        if self.at(T::DotDotDot) {
                            self.bump();
                            variadic = true;
                            self.skip_newlines();
                            break;
                        }
                        if self.at(T::Ident) && self.nth(1).kind == T::Colon {
                            self.bump();
                            self.bump();
                        }
                        params.push(self.parse_type());
                        if !self.eat(T::Comma) {
                            break;
                        }
                    }
                    self.expect(T::RParen, "`)`");
                }
                let ret = if self.eat(T::Arrow) { Some(Box::new(self.parse_type())) } else { None };
                if is_block {
                    TypeKind::Block { params, ret }
                } else {
                    TypeKind::Proc { params, ret, c_abi: false, variadic }
                }
            }
            T::Ident if self.text_of(tok.span) == "distinct" => {
                self.bump();
                TypeKind::Distinct(Box::new(self.parse_type()))
            }
            T::Ident if self.text_of(tok.span) == "matrix" && self.nth(1).kind == T::LBracket => {
                self.bump();
                self.bump();
                let rows = self.parse_expr();
                self.expect(T::Comma, "`,`");
                let cols = self.parse_expr();
                self.expect(T::RBracket, "`]`");
                let elem = self.parse_type();
                TypeKind::Matrix { rows: Box::new(rows), cols: Box::new(cols), elem: Box::new(elem) }
            }
            T::Const | T::Ident => return self.parse_type_path(),
            T::SpliceBegin => {
                let (index, span) = self.parse_splice();
                return TypeExpr { kind: index.map_or(TypeKind::Error, TypeKind::Splice), span };
            }
            _ => {
                self.error_expected("a type");
                return TypeExpr { kind: TypeKind::Error, span: tok.span };
            }
        };
        TypeExpr { kind, span: start.to(self.prev_span()) }
    }

    fn parse_type_path(&mut self) -> TypeExpr {
        let start = self.peek().span;
        let mut segments = Vec::new();
        let mut optional_suffix = None;
        loop {
            let tok = self.peek();
            match tok.kind {
                T::Const | T::Ident => {
                    self.bump();
                    let mut text = self.text_of(tok.span);
                    let mut span = tok.span;
                    if tok.kind == T::Ident && text.ends_with('?') {
                        text = &text[..text.len() - 1];
                        span = Span::new(span.file, span.start, span.end - 1);
                        optional_suffix = Some(Span::new(span.file, span.end, span.end + 1));
                    }
                    segments.push(Ident { name: Name::new(text), span });
                }
                _ => {
                    self.error_expected("a type name");
                    break;
                }
            }
            let current = segments.last().map(|s: &Ident| s.as_str()).unwrap_or("");
            let continues =
                self.nth(1).kind == T::Const || current.starts_with(|c: char| c.is_ascii_lowercase()) || current == "C";
            if optional_suffix.is_none()
                && self.at(T::Dot)
                && matches!(self.nth(1).kind, T::Const | T::Ident)
                && continues
            {
                self.bump();
                continue;
            }
            break;
        }
        let mut args = Vec::new();
        if optional_suffix.is_none() && self.at(T::LParen) && !self.peek().space_before {
            self.bump();
            loop {
                self.skip_newlines();
                if self.at(T::RParen) {
                    break;
                }
                args.push(self.parse_generic_arg());
                if !self.eat(T::Comma) {
                    break;
                }
            }
            self.skip_newlines();
            self.expect(T::RParen, "`)`");
        }
        if optional_suffix.is_some() {
            self.pending_optional = optional_suffix;
        }
        TypeExpr { kind: TypeKind::Path { segments, args }, span: start.to(self.prev_span()) }
    }

    fn parse_generic_arg(&mut self) -> GenericArg {
        match self.kind() {
            T::Const | T::Caret | T::LBracket | T::TypeParam | T::LParen => GenericArg::Type(self.parse_type()),
            T::Ident if matches!(self.text_of(self.peek().span), "map" | "proc" | "distinct") => {
                GenericArg::Type(self.parse_type())
            }
            _ => GenericArg::Expr(self.parse_expr()),
        }
    }

    /// Tries to parse a type at the current position without reporting
    /// errors; restores the position on failure.
    fn try_parse_type(&mut self) -> Option<TypeExpr> {
        let saved_pos = self.pos;
        let saved_diags = self.diags.len();
        let saved_last = self.last_error_at;
        let saved_splices = self.splice_mark();
        let ty = self.parse_type();
        if self.diags.len() > saved_diags || matches!(ty.kind, TypeKind::Error) {
            self.pos = saved_pos;
            self.diags.truncate(saved_diags);
            self.last_error_at = saved_last;
            self.rewind_splices(saved_splices);
            return None;
        }
        Some(ty)
    }

    // ----- statements ----------------------------------------------------

    fn parse_block_body(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        let (start, errors) = (self.peek().span, self.diags.len());
        loop {
            self.skip_newlines();
            if matches!(
                self.kind(),
                T::Eof | T::RBrace | T::SpliceEnd | T::Kw(K::End) | T::Kw(K::Else) | T::Kw(K::Elsif) | T::Kw(K::When)
            ) {
                break;
            }
            let before = self.pos;
            stmts.push(self.parse_stmt());
            self.expect_stmt_end();
            if self.pos == before {
                self.bump();
            }
        }
        // A body whose only lines were reported and skipped (like a stray
        // splice) keeps a placeholder, so it doesn't also read as empty.
        if stmts.is_empty() && self.diags.len() > errors {
            stmts.push(Stmt { kind: StmtKind::Error, span: start, attrs: Vec::new() });
        }
        stmts
    }

    fn is_decl_start(&self) -> bool {
        let mut i = 0;
        loop {
            let Some(len) = self.name_len(i) else { return false };
            match self.nth(i + len).kind {
                T::Colon => return true,
                T::Comma => i += len + 1,
                _ => return false,
            }
        }
    }

    fn parse_stmt(&mut self) -> Stmt {
        let attrs = self.parse_attrs();
        let start = self.peek().span;
        let kind = match self.kind() {
            T::Kw(K::Return) => {
                self.bump();
                let values =
                    if self.at_stmt_end() || self.at_modifier() { Vec::new() } else { self.parse_expr_list(true) };
                StmtKind::Return(values)
            }
            T::Kw(K::Break) | T::Kw(K::Next) => {
                let tok = self.bump();
                let value = if self.at_stmt_end() || self.at_modifier() { None } else { Some(self.parse_expr_cmd()) };
                if tok.kind == T::Kw(K::Break) { StmtKind::Break(value) } else { StmtKind::Next(value) }
            }
            T::Kw(K::Defer) => {
                let kw = self.bump();
                if self.at_kw(K::Do) {
                    self.bump();
                    self.openers.push(Opener { keyword: "defer do", span: kw.span });
                    self.push_scope();
                    let body = self.parse_block_body();
                    self.pop_scope();
                    self.expect_end();
                    StmtKind::Defer(body)
                } else {
                    StmtKind::Defer(vec![self.parse_stmt()])
                }
            }
            T::Kw(K::Guard) => self.parse_guard(),
            T::Kw(K::Def | K::Struct | K::Enum | K::Union | K::Module | K::Extend | K::Overload | K::Macro) => {
                let item = self.parse_item(ItemCtx::Struct);
                match item {
                    Some(item) => StmtKind::Item(Box::new(item)),
                    None => StmtKind::Error,
                }
            }
            T::Ident | T::SpliceBegin if self.is_decl_start() => self.parse_decl(),
            _ => self.parse_expr_or_assign(),
        };
        let mut stmt = Stmt { kind, span: start.to(self.prev_span()), attrs };
        while self.at_modifier() {
            let kw = self.bump();
            let unless = kw.kind == T::Kw(K::Unless);
            let cond = self.parse_expr();
            let span = stmt.span.to(self.prev_span());
            let attrs = std::mem::take(&mut stmt.attrs);
            stmt = Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::If(Box::new(IfExpr {
                        cond: Cond::Expr(cond),
                        then: vec![stmt],
                        elifs: Vec::new(),
                        else_: None,
                        unless,
                    })),
                    span,
                }),
                span,
                attrs,
            };
        }
        stmt
    }

    fn at_modifier(&self) -> bool {
        matches!(self.kind(), T::Kw(K::If) | T::Kw(K::Unless))
    }

    fn parse_decl(&mut self) -> StmtKind {
        let mut names = vec![self.parse_name("a variable name")];
        while self.eat(T::Comma) {
            names.push(self.parse_name("a variable name"));
        }
        self.expect(T::Colon, "`:`");
        let ty = self.parse_type();
        let mut value = None;
        let mut uninit = false;
        if self.eat(T::Eq) {
            if self.eat(T::TripleDash) {
                uninit = true;
            } else {
                value = Some(self.parse_expr_cmd());
            }
        }
        for n in &names {
            self.declare(n.name);
        }
        StmtKind::Decl { names, ty, value, uninit }
    }

    fn parse_guard(&mut self) -> StmtKind {
        let kw = self.bump();
        let mut names = Vec::new();
        let save = self.pos;
        let mut is_bind = false;
        let mut i = 0;
        while let Some(len) = self.name_len(i) {
            if self.nth(i + len).kind == T::Comma {
                i += len + 1;
                continue;
            }
            is_bind = self.nth(i + len).kind == T::Eq;
            break;
        }
        if is_bind {
            names.push(self.parse_name("a name"));
            while self.eat(T::Comma) {
                names.push(self.parse_name("a name"));
            }
            self.expect(T::Eq, "`=`");
        } else {
            self.pos = save;
        }
        let value = self.parse_expr_cmd();
        if !self.at_kw(K::Else) {
            let tok = self.peek();
            self.report(
                Diagnostic::error(
                    codes::UNEXPECTED_TOKEN,
                    format!("expected `else` after the guarded value, found {}", self.found()),
                )
                .primary(tok.span, "expected `else`")
                .note("a guard reads: `guard x = maybe() else … end`; the else-branch must leave the scope")
                .suggest(
                    "add an else-branch",
                    vec![Edit {
                        span: self.prev_span().shrink_to_end(),
                        replacement: {
                            let indent = " ".repeat(self.indent_of_line(self.line_of(kw.span.start)));
                            format!(" else\n{indent}  return\n{indent}end")
                        },
                    }],
                    Applicability::HasPlaceholders,
                ),
            );
            self.recover_line();
            for n in &names {
                self.declare(n.name);
            }
            return StmtKind::Guard { names, value, err: None, else_body: Vec::new() };
        }
        self.bump();
        let mut err = None;
        self.push_scope();
        if self.eat(T::Pipe) {
            let e = self.parse_name("the error binding name");
            self.declare(e.name);
            err = Some(e);
            self.expect(T::Pipe, "`|`");
        }
        self.openers.push(Opener { keyword: "guard", span: kw.span });
        let else_body = self.parse_block_body();
        self.pop_scope();
        self.expect_end();
        for n in &names {
            self.declare(n.name);
        }
        StmtKind::Guard { names, value, err, else_body }
    }

    fn parse_expr_or_assign(&mut self) -> StmtKind {
        let first = self.parse_expr_cmd();
        if self.at(T::Comma) && is_assignable(&first) {
            let mut targets = vec![first];
            let save = self.pos;
            let splices = self.splice_mark();
            let mut ok = true;
            while self.eat(T::Comma) {
                let t = self.parse_expr_bp(PREFIX_NEG_BP, false);
                if !is_assignable(&t) {
                    ok = false;
                    break;
                }
                targets.push(t);
            }
            if ok && self.at(T::Eq) {
                self.bump();
                self.skip_newlines();
                let values = self.parse_expr_list(true);
                self.declare_targets(&targets);
                return StmtKind::Assign { targets, op: None, values };
            }
            if ok && is_assign_op(self.kind()) {
                let tok = self.bump();
                self.skip_newlines();
                let value = self.parse_expr_cmd();
                return StmtKind::Assign { targets, op: binop_of(tok.kind), values: vec![value] };
            }
            self.pos = save;
            self.rewind_splices(splices);
            let first = targets.swap_remove(0);
            return self.recover_value_list(first);
        }
        if self.at(T::Comma) {
            return self.recover_value_list(first);
        }
        if self.at(T::Eq) {
            let eq = self.bump();
            if !is_assignable(&first) {
                let diag = self.invalid_target(&first).secondary(eq.span, "assignment here");
                self.report(diag);
            }
            self.skip_newlines();
            let values = self.parse_expr_list(true);
            let targets = vec![first];
            self.declare_targets(&targets);
            return StmtKind::Assign { targets, op: None, values };
        }
        if is_assign_op(self.kind()) {
            let tok = self.bump();
            if !is_assignable(&first) {
                let diag = self.invalid_target(&first);
                self.report(diag);
            }
            self.skip_newlines();
            let value = self.parse_expr_cmd();
            return StmtKind::Assign { targets: vec![first], op: binop_of(tok.kind), values: vec![value] };
        }
        StmtKind::Expr(first)
    }

    /// Explains why `target` cannot be assigned to, with advice that fits
    /// what was written.
    fn invalid_target(&self, target: &Expr) -> Diagnostic {
        let base = Diagnostic::error(codes::INVALID_ASSIGN_TARGET, "cannot assign to this expression");
        match &target.kind {
            ExprKind::Const(name) => {
                let lower = to_snake_case(name.as_str());
                base.primary(target.span, "this is a constant")
                    .note("constants get their value once, where they are declared")
                    .suggest_replace(
                        format!("use a variable instead, like `{lower}`"),
                        target.span,
                        lower,
                        Applicability::MaybeIncorrect,
                    )
            }
            ExprKind::Member { safe: true, .. } => base
                .primary(target.span, "`&.` reads a field only when the value is not nil")
                .help("unwrap the optional first, like `if v = opt` … `v.field = value` … `end`"),
            ExprKind::Call(_) => base
                .primary(target.span, "this is a method call")
                .note("a call returns a temporary value, so there is nothing to store into"),
            _ => base
                .primary(target.span, "not a variable, field or index")
                .note("only variables, fields (`a.b`), indexes (`a[i]`) and dereferences (`p^`) can be assigned"),
        }
    }

    /// Recovers from `a, b` used as a statement, which is only valid after
    /// `return` or before `=`, by treating it as a `return`.
    fn recover_value_list(&mut self, first: Expr) -> StmtKind {
        let comma = self.peek().span;
        self.report(
            Diagnostic::error(codes::UNEXPECTED_TOKEN, "expected end of line, found `,`")
                .primary(comma, "a list of values is not a statement")
                .suggest(
                    "to return several values, write `return`",
                    vec![Edit { span: first.span.shrink_to_start(), replacement: "return ".into() }],
                    Applicability::MaybeIncorrect,
                )
                .help("to assign several values, write `a, b = …`"),
        );
        let mut values = vec![first];
        while self.eat(T::Comma) {
            self.skip_newlines();
            values.push(self.parse_expr());
        }
        StmtKind::Return(values)
    }

    fn declare_targets(&mut self, targets: &[Expr]) {
        for t in targets {
            if let ExprKind::Ident(name) = t.kind {
                self.declare(name);
            }
        }
    }

    fn parse_expr_list(&mut self, cmd: bool) -> Vec<Expr> {
        let mut out = vec![if cmd { self.parse_expr_cmd() } else { self.parse_expr() }];
        while self.eat(T::Comma) {
            self.skip_newlines();
            out.push(self.parse_expr());
        }
        out
    }

    // ----- expressions ---------------------------------------------------

    /// Parses an expression where an outermost command call is allowed.
    fn parse_expr_cmd(&mut self) -> Expr {
        self.parse_expr_bp(0, true)
    }

    fn parse_expr(&mut self) -> Expr {
        self.parse_expr_bp(0, false)
    }

    fn parse_expr_bp(&mut self, min_bp: u8, cmd: bool) -> Expr {
        let mut lhs = self.parse_prefix(cmd);
        loop {
            let tok = self.peek();
            if tok.kind == T::Caret && tok.space_before {
                self.report(
                    Diagnostic::error(codes::UNEXPECTED_TOKEN, "`^` is not an operator in Wid")
                        .primary(tok.span, "`^` only builds pointer types and dereferences (`p^`)")
                        .suggest_replace("bitwise xor is written `~`", tok.span, "~", Applicability::MaybeIncorrect),
                );
                self.bump();
                let rhs = self.parse_expr_bp(10, false);
                let span = lhs.span.to(rhs.span);
                lhs =
                    Expr { kind: ExprKind::Binary { op: BinOp::BitXor, lhs: Box::new(lhs), rhs: Box::new(rhs) }, span };
                continue;
            }
            let Some((lbp, rbp)) = infix_bp(tok.kind) else { break };
            if lbp < min_bp {
                break;
            }
            self.bump();
            match tok.kind {
                T::Question => {
                    if self.spaced_optional(&lhs, tok) {
                        lhs = Expr { kind: ExprKind::Error, span: lhs.span.to(tok.span) };
                        continue;
                    }
                    self.skip_newlines();
                    let then = self.parse_expr_bp(0, false);
                    self.skip_newlines();
                    if !self.eat(T::Colon) {
                        self.error_expected("`:` in the conditional expression");
                    }
                    self.skip_newlines();
                    let else_ = self.parse_expr_bp(rbp, false);
                    let span = lhs.span.to(else_.span);
                    lhs = Expr {
                        kind: ExprKind::Ternary { cond: Box::new(lhs), then: Box::new(then), else_: Box::new(else_) },
                        span,
                    };
                }
                T::DotDot | T::DotDotDot => {
                    let inclusive = tok.kind == T::DotDot;
                    let hi = if self.can_start_expr(self.peek()) && !self.at_stmt_end() {
                        Some(Box::new(self.parse_expr_bp(rbp, false)))
                    } else {
                        None
                    };
                    let span = lhs.span.to(self.prev_span());
                    lhs = Expr { kind: ExprKind::Range { lo: Some(Box::new(lhs)), hi, inclusive }, span };
                }
                _ => {
                    let op = binop_of(tok.kind).expect("infix token has a binop");
                    self.skip_newlines();
                    let rhs = self.parse_expr_bp(rbp, false);
                    let span = lhs.span.to(rhs.span);
                    lhs = Expr { kind: ExprKind::Binary { op, lhs: Box::new(lhs), rhs: Box::new(rhs) }, span };
                }
            }
        }
        lhs
    }

    /// Reports `Int ?` before `)` or `,`, where the `?` (just consumed) reads
    /// as an unfinished `x ? a : b` but a type's `?` was likely meant: one
    /// after a space is a conditional's.
    fn spaced_optional(&mut self, lhs: &Expr, q: Token) -> bool {
        let upper = |name: &Ident| name.as_str().starts_with(|c: char| c.is_ascii_uppercase());
        let package = |recv: &Expr| matches!(recv.kind, ExprKind::Ident(_) | ExprKind::Const(_));
        // `Int`, `rl.Color`, `Pool(Ball, 64)`, `geo.Pool(Ball, 64)`.
        let names_type = match &lhs.kind {
            ExprKind::Const(_) => true,
            ExprKind::Member { recv, name, safe: false } => package(recv) && upper(name),
            ExprKind::Call(call) if call.block.is_none() => match &call.callee {
                Callee::Name(name) => upper(name),
                Callee::Method { recv, name, safe: false } => package(recv) && upper(name),
                Callee::Method { .. } => false,
            },
            _ => false,
        };
        if !names_type || !q.space_before || !matches!(self.kind(), T::RParen | T::Comma) {
            return false;
        }
        let gap = Span::new(self.file, lhs.span.end, q.span.start);
        self.report(
            Diagnostic::error(codes::UNEXPECTED_TOKEN, "a `?` after a space starts a conditional")
                .primary(q.span, "this reads as `cond ? a : b`, which needs both values")
                .suggest(
                    "for an optional type, write the `?` right after the type",
                    vec![Edit { span: gap, replacement: String::new() }],
                    Applicability::MaybeIncorrect,
                ),
        );
        true
    }

    fn parse_prefix(&mut self, cmd: bool) -> Expr {
        let tok = self.peek();
        let unary = |op| Some(op);
        let op = match tok.kind {
            T::Minus => unary(UnOp::Neg),
            T::Bang => unary(UnOp::Not),
            T::Tilde => unary(UnOp::BitNot),
            _ => None,
        };
        if op == Some(UnOp::Neg)
            && matches!(self.nth(1).kind, T::Int | T::Float)
            && !self.nth(1).space_before
            && self.nth(2).kind != T::StarStar
        {
            // `-7.abs` is `(-7).abs`, as in Ruby: the sign belongs to the literal.
            self.bump();
            let lit = self.parse_primary(false);
            let span = tok.span.to(lit.span);
            let neg = Expr { kind: ExprKind::Unary { op: UnOp::Neg, expr: Box::new(lit) }, span };
            return self.parse_postfix(neg, cmd);
        }
        if let Some(op) = op {
            self.bump();
            let bp = if op == UnOp::Neg { PREFIX_NEG_BP } else { PREFIX_NOT_BP };
            let expr = self.parse_expr_bp(bp, false);
            let span = tok.span.to(expr.span);
            if op == UnOp::Neg {
                match expr.kind {
                    ExprKind::Int(_) | ExprKind::Float(_) => {
                        return Expr { kind: ExprKind::Unary { op, expr: Box::new(expr) }, span };
                    }
                    _ => {}
                }
            }
            return Expr { kind: ExprKind::Unary { op, expr: Box::new(expr) }, span };
        }
        if tok.kind == T::Amp {
            self.bump();
            let expr = self.parse_expr_bp(PREFIX_NOT_BP, false);
            let span = tok.span.to(expr.span);
            return Expr { kind: ExprKind::AddrOf(Box::new(expr)), span };
        }
        if tok.kind == T::DotDot || tok.kind == T::DotDotDot {
            self.bump();
            let hi = self.parse_expr_bp(4, false);
            let span = tok.span.to(hi.span);
            return Expr {
                kind: ExprKind::Range { lo: None, hi: Some(Box::new(hi)), inclusive: tok.kind == T::DotDot },
                span,
            };
        }
        let primary = self.parse_primary(cmd);
        self.parse_postfix(primary, cmd)
    }

    fn can_start_expr(&self, tok: Token) -> bool {
        if matches!(tok.kind, T::SpliceBegin | T::AtSplice | T::ColonSplice) {
            return self.splices_are_code();
        }
        matches!(
            tok.kind,
            T::Int
                | T::Float
                | T::Str(_)
                | T::StrBegin
                | T::Symbol
                | T::Ident
                | T::Const
                | T::IVar
                | T::LParen
                | T::LBracket
                | T::LBrace
                | T::Minus
                | T::Bang
                | T::Tilde
                | T::Amp
                | T::Arrow
                | T::Caret
                | T::TripleDash
                | T::Kw(K::Nil)
                | T::Kw(K::True)
                | T::Kw(K::False)
                | T::Kw(K::SelfKw)
                | T::Kw(K::If)
                | T::Kw(K::Unless)
                | T::Kw(K::Case)
                | T::Kw(K::While)
                | T::Kw(K::Until)
                | T::Kw(K::For)
                | T::Kw(K::Loop)
                | T::Kw(K::Yield)
                | T::Kw(K::Comptime)
                | T::Kw(K::Quote)
        )
    }

    /// Returns true when `tok` can begin the first argument of a call written
    /// without parentheses, like `puts "hi"`.
    fn can_start_command_arg(&self, tok: Token) -> bool {
        if !tok.space_before {
            return false;
        }
        let idx = self.tokens.partition_point(|t| t.span.start <= tok.span.start);
        let tight_next = self.tokens.get(idx).is_some_and(|n| !n.space_before);
        match tok.kind {
            T::Int
            | T::Float
            | T::Str(_)
            | T::StrBegin
            | T::Symbol
            | T::Ident
            | T::Const
            | T::IVar
            | T::Arrow
            | T::Kw(K::Nil)
            | T::Kw(K::True)
            | T::Kw(K::False)
            | T::Kw(K::SelfKw)
            | T::LParen
            | T::Bang
            | T::Kw(K::Comptime) => true,
            // Outside a `quote` a splice is a mistyped comment, not an argument.
            T::SpliceBegin | T::AtSplice | T::ColonSplice => self.splices_are_code(),
            T::LBracket | T::Minus | T::Star | T::Amp | T::Tilde | T::Caret => tight_next,
            _ => false,
        }
    }

    fn parse_int(&mut self, tok: Token) -> Expr {
        let text: String = self.text_of(tok.span).chars().filter(|c| *c != '_').collect();
        let (digits, radix) = if let Some(rest) = text.strip_prefix("0x").or(text.strip_prefix("0X")) {
            (rest, 16)
        } else if let Some(rest) = text.strip_prefix("0b").or(text.strip_prefix("0B")) {
            (rest, 2)
        } else if let Some(rest) = text.strip_prefix("0o").or(text.strip_prefix("0O")) {
            (rest, 8)
        } else {
            (text.as_str(), 10)
        };
        match u128::from_str_radix(digits, radix) {
            Ok(v) => Expr { kind: ExprKind::Int(v), span: tok.span },
            Err(_) => {
                let (kind, allowed) = match radix {
                    16 => ("hexadecimal", "0-9 and a-f"),
                    8 => ("octal", "0-7"),
                    2 => ("binary", "0 and 1"),
                    _ => ("decimal", "0-9"),
                };
                let diag = if digits.is_empty() {
                    Diagnostic::error(codes::INVALID_NUMBER, format!("{kind} literal without digits"))
                        .primary(tok.span, "digits must follow the prefix")
                        .help(format!("{kind} literals use the digits {allowed}"))
                } else if let Some(bad) = digits.chars().find(|c| !c.is_digit(radix)) {
                    Diagnostic::error(codes::INVALID_NUMBER, format!("`{bad}` is not a {kind} digit"))
                        .primary(tok.span, "invalid digit in this literal")
                        .help(format!("{kind} literals use the digits {allowed}"))
                } else {
                    Diagnostic::error(codes::INVALID_NUMBER, "integer literal is too large")
                        .primary(tok.span, "does not fit in 128 bits")
                        .help("use a float literal, or split the value")
                };
                self.report(diag);
                Expr { kind: ExprKind::Error, span: tok.span }
            }
        }
    }

    fn parse_string_parts(&mut self) -> Expr {
        let begin = self.bump();
        let mut parts = Vec::new();
        loop {
            let tok = self.peek();
            match tok.kind {
                T::StrText(idx) => {
                    self.bump();
                    parts.push(StrPart::Text(self.strings[idx as usize].clone()));
                }
                T::InterpBegin => {
                    self.bump();
                    let saved = self.no_do;
                    self.no_do = false;
                    let expr = self.parse_expr();
                    self.no_do = saved;
                    if !self.eat(T::InterpEnd) {
                        self.error_expected("`}` to close the interpolation");
                        while !matches!(self.kind(), T::InterpEnd | T::StrEnd | T::Eof | T::Newline) {
                            self.bump();
                        }
                        self.eat(T::InterpEnd);
                    }
                    parts.push(StrPart::Interp(expr));
                }
                T::StrEnd => {
                    self.bump();
                    break;
                }
                _ => {
                    self.error_expected("the rest of the string");
                    break;
                }
            }
        }
        Expr { kind: ExprKind::Str(parts), span: begin.span.to(self.prev_span()) }
    }

    fn parse_primary(&mut self, cmd: bool) -> Expr {
        let tok = self.peek();
        let span = tok.span;
        let simple = |kind| Expr { kind, span };
        match tok.kind {
            T::Int => {
                self.bump();
                self.parse_int(tok)
            }
            T::Float => {
                self.bump();
                let text: String = self.text_of(span).chars().filter(|c| *c != '_').collect();
                match text.parse::<f64>() {
                    Ok(v) => simple(ExprKind::Float(v)),
                    Err(_) => {
                        self.report(
                            Diagnostic::error(codes::INVALID_NUMBER, "invalid float literal")
                                .primary(span, "not a valid number"),
                        );
                        simple(ExprKind::Error)
                    }
                }
            }
            T::Str(idx) => {
                self.bump();
                simple(ExprKind::Str(vec![StrPart::Text(self.strings[idx as usize].clone())]))
            }
            T::StrBegin => self.parse_string_parts(),
            T::Symbol => {
                self.bump();
                // A quoted symbol was reported by the lexer (E0110); one
                // that is not a plain name becomes an error placeholder.
                let text = &self.text_of(span)[1..];
                let name = text.trim_matches(['"', '\'']);
                let quoted = name.len() != text.len();
                let plain = name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
                    && name.chars().all(|c| c.is_alphanumeric() || c == '_');
                if quoted && !plain { simple(ExprKind::Error) } else { simple(ExprKind::Symbol(Name::new(name))) }
            }
            T::Kw(K::Nil) => {
                self.bump();
                simple(ExprKind::Nil)
            }
            T::Kw(K::True) => {
                self.bump();
                simple(ExprKind::True)
            }
            T::Kw(K::False) => {
                self.bump();
                simple(ExprKind::False)
            }
            T::Kw(K::SelfKw) => {
                self.bump();
                simple(ExprKind::SelfRef)
            }
            T::TripleDash => {
                self.bump();
                simple(ExprKind::Uninit)
            }
            T::Kw(K::If) | T::Kw(K::Unless) => self.parse_if(),
            T::Kw(K::While) | T::Kw(K::Until) => self.parse_while(),
            T::Kw(K::For) => self.parse_for(),
            T::Kw(K::Loop) => {
                self.bump();
                if !self.at_kw(K::Do) {
                    self.error_expected("`do` after `loop`");
                }
                self.eat_kw(K::Do);
                self.openers.push(Opener { keyword: "loop", span });
                self.push_scope();
                let body = self.parse_block_body();
                self.pop_scope();
                self.expect_end();
                Expr { kind: ExprKind::Loop(body), span: span.to(self.prev_span()) }
            }
            T::Kw(K::Case) => self.parse_case(),
            T::Kw(K::Yield) => {
                self.bump();
                self.yield_seen = true;
                let args = if self.at(T::LParen) && !self.peek().space_before {
                    self.bump();
                    let mut args = Vec::new();
                    loop {
                        self.skip_newlines();
                        if self.at(T::RParen) {
                            break;
                        }
                        args.push(self.parse_expr());
                        if !self.eat(T::Comma) {
                            break;
                        }
                    }
                    self.skip_newlines();
                    self.expect(T::RParen, "`)`");
                    args
                } else if !self.at_stmt_end() && !self.at_modifier() && self.can_start_expr(self.peek()) {
                    self.parse_expr_list(false)
                } else {
                    Vec::new()
                };
                Expr { kind: ExprKind::Yield(args), span: span.to(self.prev_span()) }
            }
            T::Kw(K::Comptime) => {
                self.bump();
                if self.at_kw(K::Do) {
                    self.bump();
                    self.openers.push(Opener { keyword: "comptime do", span });
                    let body = self.parse_block_body();
                    self.expect_end();
                    Expr { kind: ExprKind::Comptime(body), span: span.to(self.prev_span()) }
                } else {
                    let stmt = self.parse_stmt();
                    let span = span.to(self.prev_span());
                    match stmt.kind {
                        StmtKind::Expr(Expr { kind: ExprKind::If(if_expr), .. }) if stmt.attrs.is_empty() => {
                            Expr { kind: ExprKind::ComptimeIf(if_expr), span }
                        }
                        kind => Expr { kind: ExprKind::Comptime(vec![Stmt { kind, ..stmt }]), span },
                    }
                }
            }
            T::Kw(K::Quote) => {
                self.bump();
                if !self.eat_kw(K::Do) {
                    self.error_expected("`do` after `quote`");
                }
                self.openers.push(Opener { keyword: "quote", span });
                // The quote's code runs where the macro is called: it has
                // its own locals, splices and `yield`s.
                self.quotes.push(Vec::new());
                let splice_depth = std::mem::replace(&mut self.splice_depth, 0);
                let outer_yield = std::mem::replace(&mut self.yield_seen, false);
                self.push_def_scope();
                let body = self.parse_quote_body();
                self.pop_def_scope();
                self.yield_seen = outer_yield;
                self.splice_depth = splice_depth;
                let splices = self.quotes.pop().unwrap_or_default();
                self.expect_end();
                Expr { kind: ExprKind::Quote(Box::new(QuoteExpr { body, splices })), span: span.to(self.prev_span()) }
            }
            T::SpliceBegin => {
                let (index, span) = self.parse_splice();
                let Some(index) = index else { return Expr { kind: ExprKind::Error, span } };
                if self.at(T::LParen) && !self.peek().space_before {
                    // `#{name}(args)` calls the method the splice names.
                    let name = Ident { name: splice_name(index), span };
                    return self.parse_call_with_parens(Callee::Name(name), span);
                }
                Expr { kind: ExprKind::Splice(index), span }
            }
            T::AtSplice | T::ColonSplice => {
                self.bump();
                let Some(name) = self.parse_splice_name() else {
                    return Expr { kind: ExprKind::Error, span: span.to(self.prev_span()) };
                };
                let kind =
                    if tok.kind == T::AtSplice { ExprKind::IVar(name.name) } else { ExprKind::Symbol(name.name) };
                Expr { kind, span: span.to(name.span) }
            }
            T::LBrace => {
                self.bump();
                if !self.eat(T::RBrace) {
                    self.report(
                        Diagnostic::error(codes::UNEXPECTED_TOKEN, "`{…}` literals must be empty")
                            .primary(span, "`{}` is the zero value of the expected type")
                            .help("build a struct value with `Type.new(field: value)`"),
                    );
                    let mut depth = 1;
                    while depth > 0 && !self.at(T::Eof) {
                        match self.bump().kind {
                            T::LBrace => depth += 1,
                            T::RBrace => depth -= 1,
                            _ => {}
                        }
                    }
                }
                Expr { kind: ExprKind::Zero, span: span.to(self.prev_span()) }
            }
            T::LBracket => {
                if let Some(ty) = self.try_parse_type() {
                    return Expr { span: ty.span, kind: ExprKind::Type(Box::new(ty)) };
                }
                self.bump();
                let saved = self.no_do;
                self.no_do = false;
                let mut elems = Vec::new();
                loop {
                    self.skip_newlines();
                    if self.at(T::RBracket) || self.at(T::Eof) {
                        break;
                    }
                    elems.push(self.parse_expr());
                    self.skip_newlines();
                    if !self.eat(T::Comma) {
                        break;
                    }
                }
                self.skip_newlines();
                self.no_do = saved;
                self.expect(T::RBracket, "`]` to close the array");
                Expr { kind: ExprKind::Array(elems), span: span.to(self.prev_span()) }
            }
            T::Caret => {
                if let Some(ty) = self.try_parse_type() {
                    return Expr { span: ty.span, kind: ExprKind::Type(Box::new(ty)) };
                }
                self.error_expected("an expression");
                self.bump();
                simple(ExprKind::Error)
            }
            T::LParen => {
                self.bump();
                let saved = self.no_do;
                self.no_do = false;
                self.skip_newlines();
                let inner = self.parse_expr_cmd();
                self.skip_newlines();
                self.no_do = saved;
                self.expect(T::RParen, "`)`");
                Expr { kind: ExprKind::Paren(Box::new(inner)), span: span.to(self.prev_span()) }
            }
            T::Arrow => self.parse_lambda(),
            T::IVar => {
                self.bump();
                simple(ExprKind::IVar(Name::new(&self.text_of(span)[1..])))
            }
            T::Const => {
                self.bump();
                let name = Ident { name: Name::new(self.text_of(span)), span };
                if self.at(T::LParen) && !self.peek().space_before {
                    return self.parse_call_with_parens(Callee::Name(name), span);
                }
                simple(ExprKind::Const(name.name))
            }
            T::Ident => {
                let text = self.text_of(span);
                if matches!(text, "map" | "matrix")
                    && self.nth(1).kind == T::LBracket
                    && !self.nth(1).space_before
                    && !self.is_local(Name::new(text))
                    && let Some(ty) = self.try_parse_type()
                {
                    return Expr { span: ty.span, kind: ExprKind::Type(Box::new(ty)) };
                }
                self.bump();
                let name = Ident { name: Name::new(text), span };
                if self.at(T::LParen) && !self.peek().space_before {
                    return self.parse_call_with_parens(Callee::Name(name), span);
                }
                if !self.is_local(name.name) && self.can_start_command_arg(self.peek()) {
                    if cmd {
                        return self.parse_command_call(Callee::Name(name), span);
                    }
                    return self.parse_nested_command_call(Callee::Name(name), span, name.span);
                }
                if !self.is_local(name.name)
                    && let Some(block) = self.parse_block_arg()
                {
                    let full = span.to(block.span);
                    return Expr {
                        kind: ExprKind::Call(Box::new(Call {
                            callee: Callee::Name(name),
                            args: Vec::new(),
                            block: Some(block),
                            parens: false,
                        })),
                        span: full,
                    };
                }
                simple(ExprKind::Ident(name.name))
            }
            _ => {
                self.error_expected("an expression");
                if !self.at_stmt_end() {
                    self.bump();
                }
                simple(ExprKind::Error)
            }
        }
    }

    /// Parses `(args)`. `types` is true for the builtins whose arguments are
    /// types (see [`Parser::parse_type_arg`]).
    fn parse_args_in_parens(&mut self, types: bool) -> Vec<Arg> {
        self.bump();
        let saved = self.no_do;
        self.no_do = false;
        let mut args = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::RParen) || self.at(T::Eof) {
                break;
            }
            args.push(self.parse_arg(types, true));
            self.skip_newlines();
            if !self.eat(T::Comma) {
                break;
            }
        }
        self.skip_newlines();
        self.no_do = saved;
        self.expect(T::RParen, "`)` to close the argument list");
        args
    }

    /// Parses one call argument. `parens` is false for a call without
    /// parentheses, whose last argument ends with the statement.
    fn parse_arg(&mut self, types: bool, parens: bool) -> Arg {
        if let Some(len) = self.name_len(0)
            && self.nth(len).kind == T::Colon
        {
            let name = self.parse_name("an argument name");
            self.bump();
            self.skip_newlines();
            let value = self.parse_arg_value(types, parens);
            return Arg { name: Some(name), value, splat: false };
        }
        if self.at(T::Star) {
            self.bump();
            return Arg { name: None, value: self.parse_expr(), splat: true };
        }
        Arg { name: None, value: self.parse_arg_value(types, parens), splat: false }
    }

    /// An argument's value: an expression, or a type written in place.
    fn parse_arg_value(&mut self, types: bool, parens: bool) -> Expr {
        match self.parse_type_arg(types, parens) {
            Some(ty) => Expr { span: ty.span, kind: ExprKind::Type(Box::new(ty)) },
            None => self.parse_expr(),
        }
    }

    /// Parses a call argument as a type when it is a type that no
    /// expression spells, followed by `,` or `)` (or, without `parens`, the
    /// end of the statement or an `if`/`unless` modifier):
    ///
    /// - in any call, a type ending in a `?` of its own (`Int?`,
    ///   `rl.Color?`, `Pool(Ball, 64)?`; in `empty?` the `?` belongs to the
    ///   name), a `proc(…) -> R` type, or one with attributes
    ///   (`@[c] proc(I32)`). No expression ends in `?` or goes on with `->`.
    /// - where a type is expected (`types`: the arguments of `size_of`,
    ///   `align_of` and `type_info`), also any type that starts with `proc`,
    ///   `block` or `distinct` (keywords there, unless a local has the
    ///   name), `$T`, and a tuple type `(A, B)`.
    ///
    /// Types that also read as expressions (`Vec2`, `C.int`,
    /// `Pool(Ball, 64)`) stay expressions, which the checker resolves as
    /// types, and `[`, `^`, `map[` and `matrix[` start types in any
    /// expression. Returns `None`, with nothing consumed, for anything else.
    fn parse_type_arg(&mut self, types: bool, parens: bool) -> Option<TypeExpr> {
        let tok = self.peek();
        let text = self.text_of(tok.span);
        let local = tok.kind == T::Ident && self.is_local(Name::new(text));
        let keyword = tok.kind == T::Ident && !local && matches!(text, "proc" | "block" | "distinct");
        // These can't start an expression, so they are types even when
        // malformed, and the type parser reports what is wrong.
        if tok.kind == T::AtBracket || (types && (keyword || tok.kind == T::TypeParam)) {
            return Some(self.parse_type());
        }
        let candidate = match tok.kind {
            T::Const | T::Ident | T::SpliceBegin if !local => {
                let (question, arrow) = self.scan_arg();
                question || (arrow && text == "proc")
            }
            T::LParen => types,
            _ => false,
        };
        if !candidate {
            return None;
        }
        let saved_pos = self.pos;
        let saved_splices = self.splice_mark();
        let ty = self.try_parse_type()?;
        let tuple = types && matches!(ty.kind, TypeKind::Tuple(_));
        if self.at_arg_end(parens) && (tuple || self.only_type(&ty)) {
            return Some(ty);
        }
        self.pos = saved_pos;
        self.rewind_splices(saved_splices);
        None
    }

    /// Reads the tokens of the argument that starts here, up to the `,`,
    /// `)`, line end or `if`/`unless` modifier that closes it (outside
    /// brackets): whether its last token is a `?` written right after the
    /// token before, and whether a `->` appears outside brackets. Consumes
    /// nothing.
    fn scan_arg(&self) -> (bool, bool) {
        let mut depth = 0usize;
        let mut arrow = false;
        let mut last = None;
        for tok in &self.tokens[self.pos..] {
            match tok.kind {
                T::LParen | T::LBracket | T::LBrace | T::AtBracket | T::StrBegin | T::SpliceBegin => depth += 1,
                T::Comma | T::RParen | T::RBracket | T::RBrace | T::Newline | T::Eof if depth == 0 => break,
                T::Kw(K::If | K::Unless) if depth == 0 && last.is_some() => break,
                T::RParen | T::RBracket | T::RBrace | T::StrEnd | T::SpliceEnd => {
                    depth = depth.saturating_sub(1);
                }
                T::Arrow if depth == 0 => arrow = true,
                _ => {}
            }
            last = Some(*tok);
        }
        (last.is_some_and(|t| t.kind == T::Question && !t.space_before), arrow)
    }

    /// Whether the position is the `,` or `)` that ends an argument, after
    /// any newlines. Without `parens`, the arguments of a call without
    /// parentheses also end where the statement does, or at an `if` or
    /// `unless` modifier.
    fn at_arg_end(&self, parens: bool) -> bool {
        if !parens {
            return self.at(T::Comma) || self.at_stmt_end() || self.at_modifier();
        }
        let next = self.tokens[self.pos..].iter().find(|t| t.kind != T::Newline);
        next.is_some_and(|t| matches!(t.kind, T::Comma | T::RParen))
    }

    /// Whether a type parsed as an argument can't be read as an expression:
    /// an optional whose `?` is a token of its own, or a proc type with a
    /// return type or attributes.
    fn only_type(&self, ty: &TypeExpr) -> bool {
        match &ty.kind {
            TypeKind::Optional(inner) => {
                let at = self.tokens.partition_point(|t| t.span.start < inner.span.end);
                self.tokens.get(at).is_some_and(|t| t.kind == T::Question && t.span.start == inner.span.end)
                    || self.only_type(inner)
            }
            TypeKind::Proc { ret, c_abi, .. } => ret.is_some() || *c_abi,
            _ => false,
        }
    }

    /// Whether `callee` is `size_of`, `align_of` or `type_info`, whose
    /// arguments are types (`type_info` also takes a value).
    fn takes_type_args(&self, callee: &Callee) -> bool {
        matches!(callee, Callee::Name(n) if TYPE_ARG_BUILTINS.contains(&n.as_str()) && !self.is_local(n.name))
    }

    fn parse_call_with_parens(&mut self, callee: Callee, start: Span) -> Expr {
        let types = self.takes_type_args(&callee);
        let args = self.parse_args_in_parens(types);
        let block = self.parse_block_arg();
        Expr {
            kind: ExprKind::Call(Box::new(Call { callee, args, block, parens: true })),
            span: start.to(self.prev_span()),
        }
    }

    fn parse_command_call(&mut self, callee: Callee, start: Span) -> Expr {
        let types = self.takes_type_args(&callee);
        let mut args = vec![self.parse_arg(types, false)];
        while self.eat(T::Comma) {
            self.skip_newlines();
            args.push(self.parse_arg(types, false));
        }
        let block = if self.at_kw(K::Do) && !self.no_do { self.parse_block_arg() } else { None };
        Expr {
            kind: ExprKind::Call(Box::new(Call { callee, args, block, parens: false })),
            span: start.to(self.prev_span()),
        }
    }

    /// Parses a call without parentheses where only parenthesized calls are
    /// allowed, reporting it with a fix and keeping the call for recovery.
    fn parse_nested_command_call(&mut self, callee: Callee, start: Span, name_span: Span) -> Expr {
        let first = self.peek().span;
        let types = self.takes_type_args(&callee);
        let errors = self.diags.len();
        let mut args = vec![self.parse_arg(types, false)];
        while self.at(T::Comma) && self.can_start_expr(self.nth(1)) {
            self.bump();
            args.push(self.parse_arg(types, false));
        }
        // An argument that failed to parse may have gone past the line end;
        // the `)` goes after its last token, before the newline.
        let last = self.tokens[..self.pos].iter().rev().find(|t| t.kind != T::Newline).map_or(first, |t| t.span);
        let applicability =
            if self.diags.len() > errors { Applicability::MaybeIncorrect } else { Applicability::MachineApplicable };
        let name = self.text_of(name_span).to_string();
        let gap = Span::new(self.file, name_span.end, first.start);
        self.report(
            Diagnostic::error(
                codes::NESTED_COMMAND_CALL,
                format!("`{name}` is called without parentheses inside another expression"),
            )
            .primary(name_span.to(last), "this nested call needs parentheses")
            .note("only the outermost call of a statement may omit parentheses")
            .suggest(
                "add parentheses",
                vec![
                    Edit { span: gap, replacement: "(".into() },
                    Edit { span: last.shrink_to_end(), replacement: ")".into() },
                ],
                applicability,
            ),
        );
        Expr { kind: ExprKind::Call(Box::new(Call { callee, args, block: None, parens: false })), span: start.to(last) }
    }

    fn parse_block_arg(&mut self) -> Option<BlockArg> {
        let tok = self.peek();
        let is_brace = tok.kind == T::LBrace;
        let is_do = tok.kind == T::Kw(K::Do) && !self.no_do;
        if !is_brace && !is_do {
            return None;
        }
        self.bump();
        let saved = self.no_do;
        self.no_do = false;
        self.push_scope();
        let mut params = Vec::new();
        if self.eat(T::OrOr) {
        } else if self.eat(T::Pipe) {
            loop {
                if self.at(T::Pipe) {
                    break;
                }
                let by_ref = self.eat(T::Amp);
                let name = self.parse_name("a block parameter name");
                self.declare(name.name);
                params.push(BlockParam { name, by_ref });
                if !self.eat(T::Comma) {
                    break;
                }
            }
            self.expect(T::Pipe, "`|` to close the block parameters");
        }
        let body;
        if is_brace {
            body = self.parse_block_body();
            self.skip_newlines();
            if !self.eat(T::RBrace) {
                self.error_expected("`}` to close the block");
            }
        } else {
            self.openers.push(Opener { keyword: "do", span: tok.span });
            body = self.parse_block_body();
            self.expect_end();
        }
        self.pop_scope();
        self.no_do = saved;
        Some(BlockArg { params, body, span: tok.span.to(self.prev_span()) })
    }

    fn parse_postfix(&mut self, mut expr: Expr, cmd: bool) -> Expr {
        loop {
            let tok = self.peek();
            match tok.kind {
                T::Dot | T::SafeNav => {
                    let safe = tok.kind == T::SafeNav;
                    self.bump();
                    self.skip_newlines();
                    let name_tok = self.peek();
                    let name = match name_tok.kind {
                        T::Ident | T::Const => {
                            self.bump();
                            Ident { name: Name::new(self.text_of(name_tok.span)), span: name_tok.span }
                        }
                        T::Kw(k) => {
                            self.bump();
                            Ident { name: Name::new(k.as_str()), span: name_tok.span }
                        }
                        T::SpliceBegin => match self.parse_splice_name() {
                            Some(name) => name,
                            None => return Expr { kind: ExprKind::Error, span: expr.span.to(self.prev_span()) },
                        },
                        _ => {
                            self.error_expected("a method or field name after `.`");
                            return Expr { kind: ExprKind::Error, span: expr.span.to(name_tok.span) };
                        }
                    };
                    let start = expr.span;
                    if self.at(T::LParen) && !self.peek().space_before {
                        expr = self.parse_call_with_parens(Callee::Method { recv: expr, name, safe }, start);
                    } else if self.can_start_command_arg(self.peek()) {
                        if cmd {
                            return self.parse_command_call(Callee::Method { recv: expr, name, safe }, start);
                        }
                        let name_span = name.span;
                        expr =
                            self.parse_nested_command_call(Callee::Method { recv: expr, name, safe }, start, name_span);
                    } else if let Some(block) = self.parse_block_arg() {
                        let span = start.to(block.span);
                        expr = Expr {
                            kind: ExprKind::Call(Box::new(Call {
                                callee: Callee::Method { recv: expr, name, safe },
                                args: Vec::new(),
                                block: Some(block),
                                parens: false,
                            })),
                            span,
                        };
                    } else {
                        let span = start.to(name.span);
                        expr = Expr { kind: ExprKind::Member { recv: Box::new(expr), name, safe }, span };
                    }
                }
                T::LBracket if !tok.space_before => {
                    self.bump();
                    let saved = self.no_do;
                    self.no_do = false;
                    let mut args = Vec::new();
                    loop {
                        self.skip_newlines();
                        if self.at(T::RBracket) {
                            break;
                        }
                        args.push(self.parse_expr());
                        if !self.eat(T::Comma) {
                            break;
                        }
                    }
                    self.skip_newlines();
                    self.no_do = saved;
                    self.expect(T::RBracket, "`]`");
                    let span = expr.span.to(self.prev_span());
                    expr = Expr { kind: ExprKind::Index { recv: Box::new(expr), args }, span };
                }
                T::Caret if !tok.space_before => {
                    self.bump();
                    let span = expr.span.to(tok.span);
                    expr = Expr { kind: ExprKind::Deref(Box::new(expr)), span };
                }
                _ => break,
            }
        }
        expr
    }

    fn parse_cond(&mut self) -> Cond {
        if let Some(len) = self.name_len(0)
            && self.nth(len).kind == T::Eq
        {
            let name = self.parse_name("a name");
            self.bump();
            let value = self.parse_expr();
            return Cond::Bind { name, value };
        }
        let lhs = self.parse_expr();
        if self.at(T::Eq) {
            let eq = self.bump();
            self.report(
                Diagnostic::error(codes::INVALID_ASSIGN_TARGET, "`=` assigns; a condition compares with `==`")
                    .primary(eq.span, "this is an assignment")
                    .note("`if name = value` binds a name to an optional's value, but the left side here is not a name")
                    .suggest_replace("compare with `==`", eq.span, "==", Applicability::MachineApplicable),
            );
            let rhs = self.parse_expr();
            let span = lhs.span.to(rhs.span);
            return Cond::Expr(Expr {
                kind: ExprKind::Binary { op: BinOp::Eq, lhs: Box::new(lhs), rhs: Box::new(rhs) },
                span,
            });
        }
        Cond::Expr(lhs)
    }

    fn declare_cond(&mut self, cond: &Cond) {
        if let Cond::Bind { name, .. } = cond {
            self.declare(name.name);
        }
    }

    fn parse_if(&mut self) -> Expr {
        let kw = self.bump();
        let unless = kw.kind == T::Kw(K::Unless);
        self.openers.push(Opener { keyword: if unless { "unless" } else { "if" }, span: kw.span });
        let saved = self.no_do;
        self.no_do = true;
        let cond = self.parse_cond();
        self.no_do = saved;
        self.eat_kw(K::Then);
        self.push_scope();
        self.declare_cond(&cond);
        let then = self.parse_block_body();
        self.pop_scope();
        let mut elifs = Vec::new();
        while self.at_kw(K::Elsif) {
            self.bump();
            let cond = self.parse_cond();
            self.eat_kw(K::Then);
            self.push_scope();
            self.declare_cond(&cond);
            let body = self.parse_block_body();
            self.pop_scope();
            elifs.push((cond, body));
        }
        let else_ = if self.eat_kw(K::Else) {
            self.push_scope();
            let body = self.parse_block_body();
            self.pop_scope();
            Some(body)
        } else {
            None
        };
        self.expect_end();
        Expr {
            kind: ExprKind::If(Box::new(IfExpr { cond, then, elifs, else_, unless })),
            span: kw.span.to(self.prev_span()),
        }
    }

    fn parse_while(&mut self) -> Expr {
        let kw = self.bump();
        let until = kw.kind == T::Kw(K::Until);
        self.openers.push(Opener { keyword: if until { "until" } else { "while" }, span: kw.span });
        let saved = self.no_do;
        self.no_do = true;
        let cond = self.parse_cond();
        self.no_do = saved;
        if let (true, Cond::Bind { name, value }) = (until, &cond) {
            self.report(
                Diagnostic::error(codes::UNEXPECTED_TOKEN, "`until` cannot bind a value")
                    .primary(name.span.to(value.span), "this binding would only exist when the value is nil")
                    .help("loop with `while v = maybe … end` instead"),
            );
        }
        self.eat_kw(K::Do);
        self.push_scope();
        self.declare_cond(&cond);
        let body = self.parse_block_body();
        self.pop_scope();
        self.expect_end();
        Expr { kind: ExprKind::While { cond: Box::new(cond), body, until }, span: kw.span.to(self.prev_span()) }
    }

    fn parse_for(&mut self) -> Expr {
        let kw = self.bump();
        self.openers.push(Opener { keyword: "for", span: kw.span });
        let mut bindings = Vec::new();
        loop {
            let by_ref = self.eat(T::Amp);
            let name = self.parse_name("a loop variable");
            bindings.push(BlockParam { name, by_ref });
            if !self.eat(T::Comma) {
                break;
            }
        }
        if !self.eat_kw(K::In) {
            self.error_expected("`in`");
        }
        let saved = self.no_do;
        self.no_do = true;
        let iter = self.parse_expr();
        self.no_do = saved;
        self.eat_kw(K::Do);
        self.push_scope();
        for b in &bindings {
            self.declare(b.name.name);
        }
        let body = self.parse_block_body();
        self.pop_scope();
        self.expect_end();
        Expr { kind: ExprKind::For(Box::new(ForExpr { bindings, iter, body })), span: kw.span.to(self.prev_span()) }
    }

    fn parse_case(&mut self) -> Expr {
        let kw = self.bump();
        self.openers.push(Opener { keyword: "case", span: kw.span });
        let subject = if self.at(T::Newline) { None } else { Some(self.parse_expr()) };
        self.skip_newlines();
        let mut whens = Vec::new();
        while self.at_kw(K::When) {
            let when_tok = self.bump();
            let mut patterns = vec![self.parse_when_pattern()];
            while self.eat(T::Comma) {
                self.skip_newlines();
                patterns.push(self.parse_when_pattern());
            }
            let when_span = when_tok.span.to(self.prev_span());
            self.eat_kw(K::Then);
            self.push_scope();
            let body = self.parse_block_body();
            self.pop_scope();
            whens.push(When { patterns, body, span: when_span });
        }
        let else_ = if self.eat_kw(K::Else) {
            self.push_scope();
            let body = self.parse_block_body();
            self.pop_scope();
            Some(body)
        } else {
            None
        };
        if whens.is_empty() {
            self.report(
                Diagnostic::error(codes::UNEXPECTED_TOKEN, "`case` needs at least one `when`")
                    .primary(kw.span, "this `case` has no branches"),
            );
        }
        self.expect_end();
        Expr { kind: ExprKind::Case(Box::new(CaseExpr { subject, whens, else_ })), span: kw.span.to(self.prev_span()) }
    }

    fn parse_when_pattern(&mut self) -> Expr {
        if matches!(self.kind(), T::LBracket | T::Caret)
            && let Some(ty) = self.try_parse_type()
        {
            return Expr { span: ty.span, kind: ExprKind::Type(Box::new(ty)) };
        }
        self.parse_expr()
    }

    fn parse_lambda(&mut self) -> Expr {
        let arrow = self.bump();
        self.push_def_scope();
        let (params, block, variadic) =
            if self.at(T::LParen) { self.parse_params(ParamsOf::Proc) } else { (Vec::new(), None, None) };
        if let Some(dots) = variadic {
            self.report(
                Diagnostic::error(codes::UNEXPECTED_TOKEN, "procs cannot take C variadic arguments")
                    .primary(dots, "only `@[extern]` methods take `...`")
                    .help("take the extra values as a slice parameter, like `rest: []Int`"),
            );
        }
        if let Some(b) = block {
            self.report(
                Diagnostic::error(codes::BLOCK_MISMATCH, "procs cannot take blocks")
                    .primary(b.span, "only methods can take a `&block` parameter")
                    .help("pass a proc parameter instead, like `f: proc(Int) -> Int`"),
            );
        }
        let ret = if self.eat(T::Arrow) { Some(self.parse_type()) } else { None };
        let body = if self.eat(T::LBrace) {
            let body = self.parse_block_body();
            self.skip_newlines();
            self.expect(T::RBrace, "`}` to close the proc");
            body
        } else if self.eat_kw(K::Do) {
            self.openers.push(Opener { keyword: "do", span: arrow.span });
            let body = self.parse_block_body();
            self.expect_end();
            body
        } else {
            self.error_expected("`{` or `do` to start the proc body");
            Vec::new()
        };
        self.pop_def_scope();
        Expr { kind: ExprKind::Lambda(Box::new(Lambda { params, ret, body })), span: arrow.span.to(self.prev_span()) }
    }
}

/// The first type, in source order, within `ty` (itself included) whose
/// kind satisfies `pred`. Array lengths and other expressions inside it
/// are not searched.
fn find_type<'t>(ty: &'t mut TypeExpr, pred: &dyn Fn(&TypeKind) -> bool) -> Option<&'t mut TypeExpr> {
    if pred(&ty.kind) {
        return Some(ty);
    }
    let in_list = |list: &'t mut Vec<TypeExpr>| list.iter_mut().find_map(|t| find_type(t, pred));
    match &mut ty.kind {
        TypeKind::Path { args, .. } => args.iter_mut().find_map(|arg| match arg {
            GenericArg::Type(t) => find_type(t, pred),
            GenericArg::Expr(_) => None,
        }),
        TypeKind::Pointer(t)
        | TypeKind::MultiPointer(t)
        | TypeKind::Array(_, t)
        | TypeKind::Slice(t)
        | TypeKind::Dynamic(t)
        | TypeKind::Optional(t)
        | TypeKind::Distinct(t)
        | TypeKind::Matrix { elem: t, .. } => find_type(t, pred),
        TypeKind::Map(key, value) => find_type(key, pred).or_else(|| find_type(value, pred)),
        TypeKind::Proc { params, ret, .. } | TypeKind::Block { params, ret } => {
            in_list(params).or_else(|| ret.as_deref_mut().and_then(|t| find_type(t, pred)))
        }
        TypeKind::Tuple(elems) => in_list(elems),
        TypeKind::Param(_) | TypeKind::Splice(_) | TypeKind::Spliced(_) | TypeKind::Error => None,
    }
}

/// Whether a type is the bare name `name`, like the `T` of `[]T`.
fn is_plain_name(kind: &TypeKind, name: Name) -> bool {
    matches!(kind, TypeKind::Path { segments, args } if args.is_empty() && segments.len() == 1 && segments[0].name == name)
}

fn is_assignable(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Ident(_) | ExprKind::IVar(_) | ExprKind::Index { .. } | ExprKind::Deref(_) | ExprKind::Splice(_) => {
            true
        }
        ExprKind::Member { safe, .. } => !safe,
        ExprKind::Paren(inner) => is_assignable(inner),
        _ => false,
    }
}

/// The reserved words that may name an enum member when they stand alone.
fn is_keyword_member(kind: TokenKind) -> bool {
    matches!(kind, T::Kw(Keyword::Struct | Keyword::Enum | Keyword::Union))
}

fn to_snake_case(text: &str) -> String {
    let mut out = String::new();
    let mut prev: Option<char> = None;
    for ch in text.chars() {
        if ch.is_ascii_uppercase() {
            if prev.is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit()) {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else {
            out.push(ch);
        }
        prev = Some(ch);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_ok(src: &str) -> File {
        let (file, diags) = parse_file(FileId(0), src);
        let msgs: Vec<_> = diags.iter().map(|d| d.message.clone()).collect();
        assert!(msgs.is_empty(), "unexpected diagnostics: {msgs:?}");
        file
    }

    #[test]
    fn taste_sample_parses() {
        parse_ok(
            r#"
import "vendor:raylib", as: :rl

Vec2 = [2]F32

struct Ball
  pos: Vec2
  vel: Vec2 = [120.0, 80.0]
  radius: F32 = 8.0

  def update(dt: F32)
    @pos += @vel * dt
    @vel.y = -@vel.y unless @pos.y.between?(0.0, 450.0)
  end
end

def main
  rl.init_window(800, 450, "bounce")
  defer rl.close_window

  balls = [dynamic]Ball.new
  defer free(balls)
  balls << Ball.new(pos: [400.0, 225.0])

  until rl.window_should_close
    balls.each { |&b| b.update(rl.get_frame_time) }
    rl.begin_drawing
    rl.clear_background(rl.BLACK)
    for b in balls
      rl.draw_circle_v(b.pos, b.radius, rl.RED)
    end
    rl.end_drawing
    free_all(context.temp_allocator)
  end
end
"#,
        );
    }

    #[test]
    fn errors_sample_parses() {
        parse_ok(
            r#"
def load_level(path: String) -> (Level, Error)
  guard data = os.read_file(path) else |err|
    return {}, err
  end

  guard spawn = data.find_spawn else   # Vec2?
    return {}, :no_spawn
  end

  lives = data.parse_int("lives") || 3
  return Level.new(data, spawn, lives), nil
end

cimport "vendor/stb_image.h", as: :stbi, strip_prefix: "stbi_",
  implement: "STB_IMAGE_IMPLEMENTATION"

def load_rgba(path: CString) -> ([^]U8, Int, Int)?
  w, h, n: C.int
  px = stbi.load(path, &w, &h, &n, 4)
  return nil if px.nil?
  return px, w.to_i, h.to_i
end

struct Transform
  origin: Vec2
  basis: matrix[2, 2]F32

  def apply(p: Vec2) -> Vec2 = @basis * p + @origin
  def compose(t: Transform) -> Transform =
    Transform.new(apply(t.origin), @basis * t.basis)
  overload :*, :apply, :compose
end

def max(a: $T, b: T) -> T
  a > b ? a : b
end

def nudge(p: ^Vec2, by: []F32)
  p.x += by[0]
end
"#,
        );
    }

    #[test]
    fn command_calls() {
        let file = parse_ok("def main\n  puts \"hi\"\n  x = 1\n  puts x - 1\nend\n");
        let ItemKind::Def(def) = &file.items[0].kind else { panic!() };
        let FnBody::Block(body) = &def.body else { panic!() };
        assert_eq!(body.len(), 3);
    }

    /// The first `quote` in the body of the file's first def.
    fn first_quote(file: &File) -> QuoteExpr {
        let ItemKind::Def(def) = &file.items[0].kind else { panic!("not a def") };
        let FnBody::Block(body) = &def.body else { panic!("endless def") };
        body.iter()
            .find_map(|s| match &s.kind {
                StmtKind::Expr(Expr { kind: ExprKind::Quote(q), .. }) => Some((**q).clone()),
                StmtKind::Assign { values, .. } => match &values[0].kind {
                    ExprKind::Quote(q) => Some((**q).clone()),
                    _ => None,
                },
                _ => None,
            })
            .expect("a quote")
    }

    fn splice_of(ident: &Ident) -> u32 {
        ident.splice_index().unwrap_or_else(|| panic!("`{}` is not a splice", ident.as_str()))
    }

    fn codes_of(src: &str) -> Vec<&'static str> {
        parse_file(FileId(0), src).1.iter().map(|d| d.code.as_str()).collect()
    }

    #[test]
    fn splices_in_expression_and_type_positions() {
        let file = parse_ok(
            "macro def m(ty: Type, value: Code) -> Code\n\
             \x20 quote do\n\
             \x20   x: #{ty} = #{value}\n\
             \x20   y = [#{value}, 2]\n\
             \x20   #{value}.foo(#{value})\n\
             \x20   z: [#{value}]#{ty}? = {}\n\
             \x20   #{value}\n\
             \x20 end\n\
             end\n",
        );
        let q = first_quote(&file);
        assert_eq!(q.splices.len(), 8);
        assert!(matches!(&q.splices[0].kind, ExprKind::Ident(n) if n.as_str() == "ty"));
        let StmtKind::Decl { ty, value: Some(value), .. } = &q.body[0].kind else { panic!("{:?}", q.body[0]) };
        assert!(matches!(ty.kind, TypeKind::Splice(0)));
        assert!(matches!(value.kind, ExprKind::Splice(1)));
        let StmtKind::Assign { values, .. } = &q.body[1].kind else { panic!() };
        let ExprKind::Array(elems) = &values[0].kind else { panic!("{:?}", values[0]) };
        assert!(matches!(elems[0].kind, ExprKind::Splice(2)));
        let StmtKind::Expr(Expr { kind: ExprKind::Call(call), .. }) = &q.body[2].kind else { panic!() };
        let Callee::Method { recv, .. } = &call.callee else { panic!() };
        assert!(matches!(recv.kind, ExprKind::Splice(3)));
        assert!(matches!(call.args[0].value.kind, ExprKind::Splice(4)));
        let StmtKind::Decl { ty, .. } = &q.body[3].kind else { panic!() };
        let TypeKind::Array(len, elem) = &ty.kind else { panic!("{ty:?}") };
        assert!(matches!(len.kind, ExprKind::Splice(5)));
        let TypeKind::Optional(elem) = &elem.kind else { panic!("{elem:?}") };
        assert!(matches!(elem.kind, TypeKind::Splice(6)));
        // Standing alone, a splice is a statement (or, expanded among
        // declarations, the declarations it holds).
        assert!(matches!(q.body[4].kind, StmtKind::Expr(Expr { kind: ExprKind::Splice(7), .. })));
    }

    #[test]
    fn splices_in_name_positions() {
        let file = parse_ok(
            "macro def m(name: Symbol) -> Code\n\
             \x20 quote do\n\
             \x20   def #{name}(#{name}: Int) = @#{name}\n\
             \x20   def self.#{name} = x.#{name}\n\
             \x20   struct #{name}\n\
             \x20     #{name}: Int\n\
             \x20   end\n\
             \x20   enum #{name}\n\
             \x20     #{name}, b\n\
             \x20   end\n\
             \x20   y = :#{name}\n\
             \x20   #{name}(1, #{name}: 2)\n\
             \x20   for #{name} in xs\n\
             \x20   end\n\
             \x20   xs.each { |#{name}| }\n\
             \x20   if #{name} = maybe\n\
             \x20   end\n\
             \x20   #{name}: Int = 1\n\
             \x20   overload :#{name}, :#{name}\n\
             \x20   guard #{name} = maybe else |#{name}|\n\
             \x20     return\n\
             \x20   end\n\
             \x20 end\n\
             end\n",
        );
        let q = first_quote(&file);
        assert_eq!(q.splices.len(), 20);
        let item = |i: usize| match &q.body[i].kind {
            StmtKind::Item(item) => item.kind.clone(),
            other => panic!("statement {i} is not an item: {other:?}"),
        };
        let ItemKind::Def(def) = item(0) else { panic!() };
        assert_eq!(splice_of(&def.name), 0);
        assert_eq!(splice_of(&def.params[0].name), 1);
        let FnBody::Expr(body) = &def.body else { panic!() };
        assert!(matches!(&body.kind, ExprKind::IVar(n) if splice_index(*n) == Some(2)));
        let ItemKind::Def(def) = item(1) else { panic!() };
        assert!(def.is_static);
        assert_eq!(splice_of(&def.name), 3);
        let FnBody::Expr(body) = &def.body else { panic!() };
        let ExprKind::Member { name, .. } = &body.kind else { panic!("{body:?}") };
        assert_eq!(splice_of(name), 4);
        let ItemKind::Struct(s) = item(2) else { panic!() };
        assert_eq!(splice_of(&s.name), 5);
        let ItemKind::Field(field) = &s.body[0].kind else { panic!("{:?}", s.body[0]) };
        assert_eq!(splice_of(&field.name), 6);
        let ItemKind::Enum(e) = item(3) else { panic!() };
        assert_eq!(splice_of(&e.name), 7);
        assert_eq!(splice_of(&e.members[0].name), 8);
        assert_eq!(e.members[1].name.as_str(), "b");
        let StmtKind::Assign { values, .. } = &q.body[4].kind else { panic!() };
        assert!(matches!(&values[0].kind, ExprKind::Symbol(n) if splice_index(*n) == Some(9)));
        let StmtKind::Expr(Expr { kind: ExprKind::Call(call), .. }) = &q.body[5].kind else { panic!() };
        let Callee::Name(callee) = &call.callee else { panic!() };
        assert_eq!(splice_of(callee), 10);
        assert_eq!(splice_of(call.args[1].name.as_ref().expect("a named argument")), 11);
        let StmtKind::Expr(Expr { kind: ExprKind::For(f), .. }) = &q.body[6].kind else { panic!() };
        assert_eq!(splice_of(&f.bindings[0].name), 12);
        let StmtKind::Expr(Expr { kind: ExprKind::Call(call), .. }) = &q.body[7].kind else { panic!() };
        assert_eq!(splice_of(&call.block.as_ref().expect("a block").params[0].name), 13);
        let StmtKind::Expr(Expr { kind: ExprKind::If(if_expr), .. }) = &q.body[8].kind else { panic!() };
        let Cond::Bind { name, .. } = &if_expr.cond else { panic!() };
        assert_eq!(splice_of(name), 14);
        let StmtKind::Decl { names, .. } = &q.body[9].kind else { panic!("{:?}", q.body[9]) };
        assert_eq!(splice_of(&names[0]), 15);
        let ItemKind::Overload(o) = item(10) else { panic!() };
        assert_eq!((splice_of(&o.name), splice_of(&o.members[0])), (16, 17));
        let StmtKind::Guard { names, err: Some(err), .. } = &q.body[11].kind else { panic!() };
        assert_eq!((splice_of(&names[0]), splice_of(err)), (18, 19));
    }

    #[test]
    fn quote_in_a_splice_has_its_own_splices() {
        let file = parse_ok(
            "macro def m(a: Code, c: Bool) -> Code\n\
             \x20 quote do\n\
             \x20   x = #{c ? quote do foo(#{a}) end : a}\n\
             \x20   y = #{a}\n\
             \x20 end\n\
             end\n",
        );
        let q = first_quote(&file);
        assert_eq!(q.splices.len(), 2);
        let ExprKind::Ternary { then, .. } = &q.splices[0].kind else { panic!("{:?}", q.splices[0]) };
        let ExprKind::Quote(inner) = &then.kind else { panic!() };
        assert_eq!(inner.splices.len(), 1);
        let StmtKind::Expr(Expr { kind: ExprKind::Call(call), .. }) = &inner.body[0].kind else { panic!() };
        assert!(matches!(call.args[0].value.kind, ExprKind::Splice(0)));
        let StmtKind::Assign { values, .. } = &q.body[1].kind else { panic!() };
        assert!(matches!(values[0].kind, ExprKind::Splice(1)));
    }

    #[test]
    fn quote_bodies_hold_declarations() {
        let file = parse_ok(
            "macro def m(methods: []Code) -> Code\n\
             \x20 quote do\n\
             \x20   @[c] def a = 1\n\
             \x20   private def b = 2\n\
             \x20   MAX = 3\n\
             \x20   struct S\n\
             \x20     x: Int\n\
             \x20     #{methods}\n\
             \x20   end\n\
             \x20   include Comparable\n\
             \x20   attr_reader :hp\n\
             \x20   lib.attr_reader :hp, :mana\n\
             \x20   comptime if OS == :linux\n\
             \x20     def c = 1\n\
             \x20     LIMIT: Int = 2\n\
             \x20   else\n\
             \x20     puts 1\n\
             \x20   end\n\
             \x20   #{methods}\n\
             \x20   @[no_bounds_check] x = xs[0]\n\
             \x20 end\n\
             end\n",
        );
        let q = first_quote(&file);
        let item = |stmt: &Stmt| match &stmt.kind {
            StmtKind::Item(item) => (**item).clone(),
            other => panic!("not an item: {other:?}"),
        };
        let a = item(&q.body[0]);
        assert!(a.has_attr("c") && matches!(a.kind, ItemKind::Def(_)));
        assert!(item(&q.body[1]).private);
        assert!(matches!(item(&q.body[2]).kind, ItemKind::Const(_)));
        let ItemKind::Struct(s) = item(&q.body[3]).kind else { panic!() };
        assert!(matches!(s.body[0].kind, ItemKind::Field(_)));
        assert!(matches!(s.body[1].kind, ItemKind::Splice(0)));
        assert!(matches!(item(&q.body[4]).kind, ItemKind::Include(_)));
        assert!(matches!(q.body[5].kind, StmtKind::Expr(Expr { kind: ExprKind::Call(_), .. })));
        assert!(matches!(q.body[6].kind, StmtKind::Expr(Expr { kind: ExprKind::Call(_), .. })));
        let StmtKind::Expr(Expr { kind: ExprKind::ComptimeIf(if_expr), .. }) = &q.body[7].kind else { panic!() };
        assert!(matches!(item(&if_expr.then[0]).kind, ItemKind::Def(_)));
        assert!(matches!(item(&if_expr.then[1]).kind, ItemKind::Const(_)));
        assert!(matches!(if_expr.else_.as_ref().expect("else")[0].kind, StmtKind::Expr(_)));
        assert!(matches!(q.body[8].kind, StmtKind::Expr(Expr { kind: ExprKind::Splice(1), .. })));
        assert!(matches!(q.body[9].kind, StmtKind::Assign { .. }));
        assert_eq!(q.body[9].attrs[0].name.as_str(), "no_bounds_check");
    }

    #[test]
    fn variadic_macro_parameter() {
        let file = parse_ok("macro def attr_reader(*names: Symbol) -> Code\n  quote do\n  end\nend\n");
        let ItemKind::Def(def) = &file.items[0].kind else { panic!() };
        assert!(def.params[0].splat);
        assert!(matches!(&def.params[0].ty.kind, TypeKind::Path { segments, .. } if segments[0].as_str() == "Symbol"));
    }

    #[test]
    fn misused_variadic_parameters() {
        let (file, diags) = parse_file(FileId(0), "def sum(*xs: Int) -> Int = 0\n");
        assert_eq!(diags.iter().map(|d| d.code.as_str()).collect::<Vec<_>>(), ["E0112"]);
        // Recovery reads it as the suggested slice parameter.
        let ItemKind::Def(def) = &file.items[0].kind else { panic!() };
        assert!(def.params[0].splat && matches!(def.params[0].ty.kind, TypeKind::Slice(_)));
        assert_eq!(codes_of("def main\n  g = ->(*xs: Int) { 1 }\nend\n"), ["E0112"]);
        assert_eq!(codes_of("macro def m(*a: Symbol, b: Int) -> Code = quote do end\n"), ["E0112"]);
        assert_eq!(codes_of("macro def m(*a: Symbol = [:x]) -> Code = quote do end\n"), ["E0112"]);
    }

    #[test]
    fn loose_type_parameters() {
        let src = "def first($T, xs: []T) -> T = xs[0]\n\ndef main\n  p first([1])\nend\n";
        let (file, diags) = parse_file(FileId(0), src);
        let diags: Vec<_> = diags.iter().collect();
        assert_eq!(diags.iter().map(|d| d.code.as_str()).collect::<Vec<_>>(), ["E0105"]);
        assert_eq!(diags[0].primary_span().map(|s| (s.start, s.end)), Some((10, 12)));
        // The fix removes `$T, ` and introduces `$T` in `[]T`, and the
        // parameters recover as that fix.
        let edits: Vec<_> = diags[0].helps[0].edits.iter().map(|e| (e.span.start, e.replacement.as_str())).collect();
        assert_eq!(edits, [(10, ""), (20, "$T")]);
        let ItemKind::Def(def) = &file.items[0].kind else { panic!() };
        assert_eq!(def.params.len(), 1);
        let TypeKind::Slice(elem) = &def.params[0].ty.kind else { panic!("{:?}", def.params[0].ty) };
        assert!(matches!(&elem.kind, TypeKind::Param(id) if id.as_str() == "T"));
        assert!(matches!(&file.items[1].kind, ItemKind::Def(def) if def.name.as_str() == "main"));
        // Odin's `$T: typeid`, a last parameter, and lists of methods,
        // macros and procs report one error each.
        assert_eq!(codes_of("def pick(xs: []T, $T: typeid) -> T = xs[0]\n"), ["E0105"]);
        assert_eq!(codes_of("def zero($T: Type) -> T\n  {}\nend\n"), ["E0105"]);
        assert_eq!(codes_of("def both($K, $V, m: map[K]V) -> Int = 0\n"), ["E0105", "E0105"]);
        assert_eq!(codes_of("macro def m($T) -> Code = quote do end\n"), ["E0105"]);
        assert_eq!(codes_of("def main\n  f = ->($T, x: Int) -> Int { x }\nend\n"), ["E0105"]);
    }

    #[test]
    fn qualified_macro_calls_are_declarations() {
        let file = parse_ok(
            "lib.setup :a, :b\n\
             lib.make(:a)\n\
             lib.init\n\
             struct S\n\
             \x20 hp: Int\n\
             \x20 lib.attr_reader :hp, :mana\n\
             \x20 lib.make\n\
             end\n\
             enum E\n\
             \x20 a\n\
             \x20 lib.attr(:x)\n\
             end\n\
             module M\n\
             \x20 lib.helpers\n\
             end\n\
             extend S\n\
             \x20 lib.more :x\n\
             end\n",
        );
        let is_call = |item: &Item| matches!(item.kind, ItemKind::MacroCall(_));
        assert!(file.items[..3].iter().all(is_call));
        let ItemKind::Struct(s) = &file.items[3].kind else { panic!() };
        assert!(s.body[1..].iter().all(is_call));
        let ItemKind::Enum(e) = &file.items[4].kind else { panic!() };
        assert!(is_call(&e.body[0]));
        let ItemKind::Module(m) = &file.items[5].kind else { panic!() };
        assert!(is_call(&m.body[0]));
        let ItemKind::Extend(x) = &file.items[6].kind else { panic!() };
        assert!(is_call(&x.body[0]));
    }

    #[test]
    fn stray_splices_are_reported_and_skipped() {
        let (file, diags) = parse_file(
            FileId(0),
            "def main\n  #{TODO} fix this\n  x = 1 #{note}\n  y = #{x}\n  xs = [\n    1, #{first}\n    2,\n  ]\n  puts x\nend\n#{top}\n",
        );
        let codes: Vec<_> = diags.iter().map(|d| (d.code.as_str(), d.helps[0].applicability)).collect();
        use Applicability::*;
        assert_eq!(
            codes,
            [
                ("E0111", MachineApplicable),
                ("E0111", MachineApplicable),
                ("E0111", MaybeIncorrect),
                ("E0111", MachineApplicable),
                ("E0111", MachineApplicable)
            ]
        );
        let ItemKind::Def(def) = &file.items[0].kind else { panic!() };
        let FnBody::Block(body) = &def.body else { panic!() };
        assert_eq!(body.len(), 4);
        let StmtKind::Assign { values, .. } = &body[2].kind else { panic!() };
        assert!(matches!(&values[0].kind, ExprKind::Array(elems) if elems.len() == 2));
        // Unclosed outside a `quote`, it is still the one error.
        assert_eq!(codes_of("def main\n  #{ oops\n  puts 1\nend\n"), ["E0111"]);
    }

    #[test]
    fn splice_errors_inside_quotes() {
        assert_eq!(
            codes_of("macro def m(a: Symbol) -> Code\n  quote do\n    def #{a\n    end\n  end\nend\n"),
            ["E0102"]
        );
        assert_eq!(
            codes_of("macro def m(a: Symbol) -> Code\n  quote do\n    def #{f(#{a})} = 1\n  end\nend\n"),
            ["E0111"]
        );
        let (_, diags) = parse_file(FileId(0), "macro def m(a: Symbol) -> Code\n  #{a}\nend\n");
        assert_eq!(diags.iter().map(|d| d.code.as_str()).collect::<Vec<_>>(), ["E0111"]);
        assert_eq!(diags.iter().next().expect("one").helps.len(), 2, "advice about `quote` in a macro");
    }

    #[test]
    fn parenthesized_types_as_constant_values() {
        let file = parse_ok(
            "MaybeCb = (proc(Int) -> Int)?\n\
             Cb = (proc(Int) -> Int)\n\
             Pair = (Int, String)\n\
             Same = (Vec2)\n\
             NINE = (1 + 2) * 3\n\
             SEVEN = (NINE - 2)\n",
        );
        let values: Vec<_> = file
            .items
            .iter()
            .map(|item| match &item.kind {
                ItemKind::Const(c) => &c.value.kind,
                other => panic!("{other:?}"),
            })
            .collect();
        let ExprKind::Type(ty) = values[0] else { panic!("{:?}", values[0]) };
        let TypeKind::Optional(inner) = &ty.kind else { panic!("{ty:?}") };
        assert!(matches!(inner.kind, TypeKind::Proc { ret: Some(_), .. }));
        assert!(matches!(values[1], ExprKind::Type(t) if matches!(t.kind, TypeKind::Proc { .. })));
        assert!(matches!(values[2], ExprKind::Type(t) if matches!(t.kind, TypeKind::Tuple(_))));
        // These read as expressions.
        assert!(matches!(values[3], ExprKind::Paren(_)));
        assert!(matches!(values[4], ExprKind::Binary { .. }));
        assert!(matches!(values[5], ExprKind::Paren(_)));
        // In a `quote`, a parenthesized splice may be any code.
        let file = parse_ok(
            "macro def m(v: Code, t: Type) -> Code\n\
             \x20 quote do\n\
             \x20   A = (#{v})\n\
             \x20   B = (#{v}) * 2\n\
             \x20   C = (#{t})?\n\
             \x20 end\n\
             end\n",
        );
        let q = first_quote(&file);
        let value = |stmt: &Stmt| match &stmt.kind {
            StmtKind::Item(item) => match &item.kind {
                ItemKind::Const(c) => c.value.kind.clone(),
                other => panic!("{other:?}"),
            },
            other => panic!("{other:?}"),
        };
        assert!(matches!(value(&q.body[0]), ExprKind::Paren(_)));
        assert!(matches!(value(&q.body[1]), ExprKind::Binary { .. }));
        assert!(matches!(value(&q.body[2]), ExprKind::Type(t) if matches!(t.kind, TypeKind::Optional(_))));
        assert_eq!(q.splices.len(), 3);
    }

    #[test]
    fn keywords_name_enum_members_when_alone() {
        let file =
            parse_ok("enum Kind\n  int\n  struct\n  enum = 7\n  union, map\n\n  def plain? -> Bool = true\nend\n");
        let ItemKind::Enum(e) = &file.items[0].kind else { panic!("expected an enum") };
        let names: Vec<&str> = e.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["int", "struct", "enum", "union", "map"]);
        assert!(e.members[2].value.is_some());
        assert_eq!(e.body.len(), 1);

        // Followed by a name, `struct` still starts a declaration, not a member.
        let (file, _) = parse_file(FileId(0), "enum Kind\n  a\n  struct Inner\n  end\nend\n");
        let ItemKind::Enum(e) = &file.items[0].kind else { panic!("expected an enum") };
        assert_eq!(e.members.len(), 1);
    }

    /// The first argument of the call on each line of `main`'s body.
    fn first_args(body: &str) -> Vec<ExprKind> {
        let file = parse_ok(&format!("def main\n{body}end\n"));
        let ItemKind::Def(def) = &file.items[0].kind else { panic!("not a def") };
        let FnBody::Block(stmts) = &def.body else { panic!("endless def") };
        stmts
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Expr(Expr { kind: ExprKind::Call(call), .. }) => Some(call.args[0].value.kind.clone()),
                _ => None,
            })
            .collect()
    }

    fn written_type(kind: &ExprKind) -> &TypeKind {
        match kind {
            ExprKind::Type(t) => &t.kind,
            other => panic!("not a type: {other:?}"),
        }
    }

    #[test]
    fn types_written_in_place_as_arguments() {
        let args = first_args(
            "  size_of(Int?)\n\
             \x20 align_of(proc(Int) -> Int)\n\
             \x20 type_info(@[c] proc(I32))\n\
             \x20 size_of((Int, Bool))\n\
             \x20 size_of(proc(Int))\n\
             \x20 type_info(proc)\n\
             \x20 size_of(distinct F64)\n\
             \x20 align_of(block(Int))\n\
             \x20 size_of($T)\n\
             \x20 size_of(Pool(Ball, 64)?)\n\
             \x20 type_info(rl.Color?)\n\
             \x20 size_of((Int?))\n\
             \x20 f(Int?, proc(Int) -> Int?)\n\
             \x20 f(@[c] proc())\n\
             \x20 f(\n    Int??\n  )\n\
             \x20 f(t: Int?)\n",
        );
        assert_eq!(args.len(), 16);
        let optional_of = |k: &TypeKind| match k {
            TypeKind::Optional(inner) => inner.kind.clone(),
            other => panic!("not optional: {other:?}"),
        };
        let path = |k: &TypeKind| match k {
            TypeKind::Path { segments, args } => {
                (segments.iter().map(|s| s.as_str()).collect::<Vec<_>>().join("."), args.len())
            }
            other => panic!("not a path: {other:?}"),
        };
        assert_eq!(path(&optional_of(written_type(&args[0]))), ("Int".into(), 0));
        assert!(matches!(written_type(&args[1]), TypeKind::Proc { ret: Some(_), c_abi: false, .. }));
        assert!(matches!(written_type(&args[2]), TypeKind::Proc { ret: None, c_abi: true, .. }));
        assert!(matches!(written_type(&args[3]), TypeKind::Tuple(elems) if elems.len() == 2));
        assert!(matches!(written_type(&args[4]), TypeKind::Proc { params, ret: None, .. } if params.len() == 1));
        assert!(matches!(written_type(&args[5]), TypeKind::Proc { params, .. } if params.is_empty()));
        assert!(matches!(written_type(&args[6]), TypeKind::Distinct(_)));
        assert!(matches!(written_type(&args[7]), TypeKind::Block { .. }));
        assert!(matches!(written_type(&args[8]), TypeKind::Param(n) if n.as_str() == "T"));
        assert_eq!(path(&optional_of(written_type(&args[9]))), ("Pool".into(), 2));
        assert_eq!(path(&optional_of(written_type(&args[10]))), ("rl.Color".into(), 0));
        assert_eq!(path(&optional_of(written_type(&args[11]))), ("Int".into(), 0));
        assert_eq!(path(&optional_of(written_type(&args[12]))), ("Int".into(), 0));
        assert!(matches!(written_type(&args[13]), TypeKind::Proc { c_abi: true, .. }));
        assert!(matches!(optional_of(written_type(&args[14])), TypeKind::Optional(_)));
        assert_eq!(path(&optional_of(written_type(&args[15]))), ("Int".into(), 0));

        // The second argument of `f(Int?, proc(Int) -> Int?)` too.
        let file = parse_ok("def main\n  f(Int?, proc(Int) -> Int?)\nend\n");
        let ItemKind::Def(def) = &file.items[0].kind else { panic!() };
        let FnBody::Block(stmts) = &def.body else { panic!() };
        let StmtKind::Expr(Expr { kind: ExprKind::Call(call), .. }) = &stmts[0].kind else { panic!() };
        let TypeKind::Proc { ret: Some(ret), .. } = written_type(&call.args[1].value.kind) else { panic!() };
        assert!(matches!(ret.kind, TypeKind::Optional(_)));

        // A splice too; a type tried and given up doesn't keep its splices.
        let file = parse_ok(
            "macro def m(t: Type, a: Code) -> Code\n  quote do\n    size_of(#{t}?)\n    f(#{a}?\n      1 : 2)\n  end\nend\n",
        );
        let q = first_quote(&file);
        assert_eq!(q.splices.len(), 2);
        let StmtKind::Expr(Expr { kind: ExprKind::Call(call), .. }) = &q.body[0].kind else { panic!() };
        let TypeKind::Optional(inner) = written_type(&call.args[0].value.kind) else { panic!() };
        assert!(matches!(inner.kind, TypeKind::Splice(0)));
        let StmtKind::Expr(Expr { kind: ExprKind::Call(call), .. }) = &q.body[1].kind else { panic!() };
        let ExprKind::Ternary { cond, .. } = &call.args[0].value.kind else { panic!("{:?}", call.args[0]) };
        assert!(matches!(cond.kind, ExprKind::Splice(1)));
    }

    #[test]
    fn arguments_that_read_as_expressions_stay_expressions() {
        let args = first_args(
            "  f(FLAG? 1 : 2)\n\
             \x20 f(FLAG ? 1 : 2)\n\
             \x20 f(FLAG?\n    1 : 2)\n\
             \x20 f(FLAG ?\n    1 : 2)\n\
             \x20 f(xs.empty?)\n\
             \x20 f(empty?)\n\
             \x20 f(Foo.ready?)\n\
             \x20 type_info(x)\n\
             \x20 type_info((x))\n\
             \x20 size_of(Vec2)\n\
             \x20 size_of(C.int)\n\
             \x20 size_of(C.int?)\n\
             \x20 size_of(Pool(Ball, 64))\n\
             \x20 f(proc(x))\n\
             \x20 f(Int)\n",
        );
        let shapes: Vec<&str> = args
            .iter()
            .map(|k| match k {
                ExprKind::Ternary { .. } => "ternary",
                ExprKind::Member { .. } => "member",
                ExprKind::Ident(_) => "ident",
                ExprKind::Paren(_) => "paren",
                ExprKind::Const(_) => "const",
                ExprKind::Call(_) => "call",
                other => panic!("unexpected {other:?}"),
            })
            .collect();
        assert_eq!(
            shapes,
            [
                "ternary", "ternary", "ternary", "ternary", "member", "ident", "member", "ident", "paren", "const",
                "member", "member", "call", "call", "const"
            ]
        );
        // A local named `proc` is called, not a type.
        let args = first_args("  proc = ->(x: Int) -> Int { x }\n  type_info(proc(1))\n  size_of(proc)\n");
        assert!(matches!(&args[0], ExprKind::Call(_)));
        assert!(matches!(&args[1], ExprKind::Ident(_)));
    }

    #[test]
    fn a_spaced_question_mark_before_the_argument_end() {
        let (_, diags) = parse_file(FileId(0), "def main\n  size_of(Int ?)\n  f(rl.Color ?, Pool(Ball, 4) ?)\nend\n");
        let found: Vec<_> = diags.iter().map(|d| (d.code.as_str(), d.helps[0].edits[0].replacement.as_str())).collect();
        assert_eq!(found, [("E0105", ""), ("E0105", ""), ("E0105", "")]);
        // Anything else stays an unfinished conditional.
        assert_eq!(codes_of("def main\n  f(x ?)\nend\n"), ["E0105"]);
        assert!(parse_file(FileId(0), "def main\n  f(x ?)\nend\n").1.iter().all(|d| d.helps.is_empty()));
    }

    #[test]
    fn types_written_in_place_in_calls_without_parentheses() {
        // The arguments of a call without parentheses end with the line or
        // at a modifier.
        let args = first_args("  size_of Int?\n  g proc(Int) -> Int\n");
        assert!(matches!(written_type(&args[0]), TypeKind::Optional(_)));
        assert!(matches!(written_type(&args[1]), TypeKind::Proc { .. }));
        assert!(codes_of("def main\n  n = size_of Int?\n  p n\n  f Int? if x\nend\n").is_empty());
        // Nested, it is one E0109 whose `)` goes before the line end.
        let src = "def main\n  puts size_of Int?\nend\n";
        let (_, diags) = parse_file(FileId(0), src);
        let diags: Vec<_> = diags.iter().collect();
        assert_eq!(diags.iter().map(|d| d.code.as_str()).collect::<Vec<_>>(), ["E0109"]);
        let help = &diags[0].helps[0];
        let mut fixed = src.to_string();
        for edit in help.edits.iter().rev() {
            fixed.replace_range(edit.span.start as usize..edit.span.end as usize, &edit.replacement);
        }
        assert_eq!(fixed, "def main\n  puts size_of(Int?)\nend\n");
        assert_eq!(help.applicability, Applicability::MachineApplicable);
        assert!(codes_of(&fixed).is_empty());
        // After an argument that failed to parse, the `)` still goes before
        // the line end, and the fix may be wrong.
        let (_, diags) = parse_file(FileId(0), "def main\n  puts double 4 +\nend\n");
        let fix = diags.iter().find(|d| d.code.as_str() == "E0109").map(|d| &d.helps[0]).expect("E0109");
        assert_eq!(fix.edits[1].span.start, 26);
        assert_eq!(fix.applicability, Applicability::MaybeIncorrect);
    }

    #[test]
    fn missing_end_reports_opener() {
        let (_, diags) = parse_file(FileId(0), "def main\n  if x\n    puts 1\n  \nend\n");
        assert!(diags.iter().any(|d| d.code == codes::MISSING_END));
    }
}

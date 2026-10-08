//! A recursive-descent parser with Pratt expression parsing and error recovery.

use wid_diagnostics::{Applicability, Diagnostic, Diagnostics, Edit, FileId, Span, codes};

use crate::ast::*;
use crate::intern::Name;
use crate::lexer::{Lexed, lex};
use crate::token::{Comment, Keyword, Token, TokenKind};
use crate::visit::{VisitMut, walk_expr, walk_type};

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

/// A parsed parameter list (see [`Parser::parse_params`]).
#[derive(Default)]
struct ParamList {
    params: Vec<Param>,
    /// The `&block` parameter.
    block: Option<BlockParamDecl>,
    /// The `...` of C variadic arguments.
    c_variadic: Option<Span>,
    /// Whether the list was left open at the end of its line (reported).
    left_open: bool,
}

/// An array length written `[$N]` in a method's signature, as if a method
/// could take a value parameter (only generic structs can). The type reads
/// as the slice `[]T` it should be, and `parse_def` reports it.
struct ArrayValueParam {
    /// `N`, without the `$`.
    name: Name,
    /// The `$N` token.
    tok: Span,
    /// `[$N]`, which the fix makes `[]`.
    brackets: Span,
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

/// A bracketed list of expressions being parsed: a call's arguments, an
/// array literal or an index.
struct List {
    /// The `(` or `[`.
    open: Span,
    /// The token that closes it, and its text.
    close: TokenKind,
    closer: &'static str,
    /// What the parser expects at its end, for messages ("`)` to close
    /// the argument list").
    what: &'static str,
    /// What one item and several are called ("argument", "arguments").
    item: &'static str,
    items: &'static str,
    /// What its items are.
    kind: Items,
}

/// What the items of a [`List`] are, which tells a line that doesn't go
/// on with the list apart from its next item.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Items {
    /// Call arguments, or a proc type's parameters: values or types, which
    /// may be named (`name: value`).
    Args,
    /// Values or types that are never named: array elements, indices, a
    /// generic type's arguments or a tuple's types. A line that starts
    /// with `name:` declares something (a field, a local).
    Unnamed,
    /// A method's or proc's parameters, which start with a name and its
    /// `:` (see [`Parser::param_ahead`]).
    Params,
}

/// What follows an item of a [`List`].
enum ListStep {
    /// Another item (after its `,`, eaten).
    Item,
    /// The closer, or what isn't one on the item's line: the caller
    /// expects the closer.
    Close,
    /// Nothing more on the item's line: the list was left open, and that
    /// is reported.
    Open,
}

/// The parser's state before a speculative parse (see [`Parser::mark`]).
struct Mark {
    pos: usize,
    diags: usize,
    last_error_at: Option<u32>,
    splices: usize,
    inserted: usize,
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
    /// The `quote` keyword of each open `quote`, innermost last, for the
    /// fix that builds a glued name on a line before it (see
    /// [`Parser::glued_name`]); `None` for a `quote` inside a splice, where
    /// no line goes before it.
    quote_keywords: Vec<Option<Span>>,
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
    /// While a method's parameters and return type are parsed, the `[$N]`
    /// array lengths in them (see [`ArrayValueParam`]).
    array_params: Option<Vec<ArrayValueParam>>,
    /// The token indices of the line ends put back after an unfinished
    /// line (see [`Parser::insert_line_end`]), in order, so a speculative
    /// parse that is rewound takes them back.
    inserted: Vec<usize>,
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
            quote_keywords: Vec::new(),
            splice_depth: 0,
            in_macro: false,
            uninferred: Vec::new(),
            array_params: None,
            inserted: Vec::new(),
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
    /// after an error. A bracket opened on the line goes on over the
    /// lines up to its closer, but a declaration, or an `end` that no
    /// `do` skipped here opened, starting a later line was never part of
    /// the line: it was left open (`X = Foo{` followed by `def main`), so
    /// the skipping stops there, and the declaration is parsed on its own.
    fn recover_line(&mut self) {
        let mut depth = 0i32;
        let mut blocks = 0u32;
        loop {
            let tok = self.peek();
            match tok.kind {
                T::Eof => return,
                T::Newline | T::SpliceEnd if depth <= 0 => return,
                T::Kw(K::End) if blocks > 0 => blocks -= 1,
                kind if starts_declaration(kind) && self.pos > 0 => {
                    let last = self.tokens[self.pos - 1];
                    if self.line_of(tok.span.start) > self.line_of(last.span.start) {
                        if last.kind != T::Newline {
                            self.restore_line_end(last.span);
                        }
                        return;
                    }
                }
                T::Kw(K::Do) => blocks += 1,
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
        // `(proc…`, `(^…`, `([]…`: no expression starts that way, so it is a
        // type even when malformed, and the type parser reports what is wrong.
        if paren && self.type_only_after_parens() {
            return Some(self.parse_type());
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

    /// Reports a `(` whose line ended before its `)`, with a fix that
    /// closes it after `last`, the line's last token.
    fn unclosed_paren(&mut self, open: Span, last: Span) {
        self.unclosed(open, last, ")", "`)`");
    }

    /// Reports the bracket at `open` whose line ended before its `closer`,
    /// with a fix that closes it after `last`, the line's last token.
    /// `what` is what the parser expected, as in "expected `]` to close
    /// the array".
    fn unclosed(&mut self, open: Span, last: Span, closer: &str, what: &str) {
        let at = last.shrink_to_end();
        let bracket = self.text_of(open);
        self.report(
            Diagnostic::error(codes::UNEXPECTED_TOKEN, format!("expected {what}, found end of line"))
                .primary(at, format!("expected `{closer}`"))
                .secondary(open, format!("this `{bracket}` is never closed"))
                .suggest(
                    "close it",
                    vec![Edit { span: at, replacement: closer.into() }],
                    Applicability::MachineApplicable,
                ),
        );
    }

    /// Puts back the line end the lexer dropped after `last` (an operator,
    /// `,`, `(` or `[` that would have continued the line), unless one is
    /// already here, so the next line is parsed on its own.
    fn restore_line_end(&mut self, last: Span) {
        if !self.at(T::Newline) {
            self.insert_line_end(last.shrink_to_end());
        }
    }

    /// Inserts a line end at the position, recorded so that
    /// [`Parser::try_parse_type`] can take it back.
    fn insert_line_end(&mut self, at: Span) {
        self.tokens.insert(self.pos, Token { kind: T::Newline, span: at, space_before: false });
        self.inserted.push(self.pos);
    }

    /// Whether the line ended right before the token here, after the one
    /// before it, which continued the line (an operator, `=`, `,`, `(`,
    /// `[`), and this one starts a declaration or the `end` of a block:
    /// `X = 1 +` followed by `def main` never finished its line. Returns
    /// the token that ended the line.
    fn line_cut_before_declaration(&self) -> Option<Token> {
        let last = *self.tokens[..self.pos].last()?;
        let next = self.peek();
        (last.kind != T::Newline
            && starts_declaration(next.kind)
            && self.line_of(next.span.start) > self.line_of(last.span.start))
        .then_some(last)
    }

    /// At the start of an item of `list` (after its opener or a `,`): when
    /// the line ended there and the next one starts a declaration or the
    /// `end` of a block (`X = max(1,` followed by `def main`), or what no
    /// item of the list starts with (a method's body after a parameter
    /// list, as in `def foo(a: Int,` followed by `x = a`, or a field after
    /// a type's arguments), the list was left open at the end of the line.
    /// Reports that, with a fix that closes it, and puts back the line
    /// end, so the next line is parsed on its own.
    fn list_left_open(&mut self, list: &List) -> bool {
        let cut = self.line_cut_before_declaration().or_else(|| {
            let last = *self.tokens[..self.pos].last()?;
            let next = self.peek();
            if last.kind == T::Newline || self.line_of(next.span.start) == self.line_of(last.span.start) {
                return None;
            }
            let after_name = self.name_len(0).map(|len| self.nth(len));
            let left = match list.kind {
                Items::Args => false,
                Items::Unnamed => after_name.is_some_and(|t| t.kind == T::Colon && !t.space_before),
                // A parameter, maybe without its type (reported as that).
                Items::Params => {
                    !matches!(next.kind, T::RParen | T::Eof)
                        && !self.param_ahead()
                        && !after_name.is_some_and(|t| matches!(t.kind, T::Comma | T::RParen | T::Newline | T::Eof))
                }
            };
            left.then_some(last)
        });
        let Some(last) = cut else { return false };
        self.unclosed(list.open, last.span, list.closer, list.what);
        self.restore_line_end(last.span);
        true
    }

    /// After an item of `list`: eats the `,` before the next item, and
    /// otherwise decides whether the list ends here. The item is complete,
    /// so a line end may only lead to a `,` or the list's closer: anything
    /// else on the next line is not part of the list, which was left open
    /// at the end of this line (reported with a fix that closes it), unless
    /// that line is indented under the list's first line and starts an
    /// expression (or a parameter, in a parameter list), which is the next
    /// item with its `,` missing.
    fn list_step(&mut self, list: &List) -> ListStep {
        let (last, before) = (self.prev_span(), self.pos);
        self.skip_newlines();
        if self.eat(T::Comma) {
            return ListStep::Item;
        }
        if self.at(list.close) || (self.pos == before && !self.at(T::Eof)) {
            return ListStep::Close;
        }
        let next = self.peek();
        let indent = |p: &Self, span: Span| p.indent_of_line(p.line_of(span.start));
        let item = if list.kind == Items::Params { self.param_ahead() } else { self.can_start_expr(next) };
        if next.kind != T::Eof
            && !starts_declaration(next.kind)
            && item
            && indent(self, next.span) > indent(self, list.open)
        {
            let at = last.shrink_to_end();
            self.report(
                Diagnostic::error(
                    codes::UNEXPECTED_TOKEN,
                    format!("expected `,` or `{}`, found end of line", list.closer),
                )
                .primary(at, "expected `,`")
                .secondary(next.span, format!("the next {} starts here", list.item))
                .suggest(
                    format!("separate the {} with `,`", list.items),
                    vec![Edit { span: at, replacement: ",".into() }],
                    Applicability::MaybeIncorrect,
                ),
            );
            return ListStep::Item;
        }
        self.pos = before;
        self.unclosed(list.open, last, list.closer, list.what);
        ListStep::Open
    }

    /// Whether the next line, after any blank ones, starts a declaration
    /// other than an `end`.
    fn declaration_ahead(&self) -> bool {
        let next = self.tokens[self.pos..].iter().find(|t| t.kind != T::Newline);
        next.is_some_and(|t| t.kind != T::Kw(K::End) && starts_declaration(t.kind))
    }

    /// Whether a parameter starts here: a name and its `:`, `&name`,
    /// `*name`, `$T` or `...`. A method's body, on the lines after an
    /// unclosed parameter list, doesn't start that way.
    fn param_ahead(&self) -> bool {
        match self.kind() {
            T::Amp | T::Star | T::TypeParam | T::DotDotDot => true,
            _ => self.name_len(0).is_some_and(|len| self.nth(len).kind == T::Colon),
        }
    }

    /// Expects the `]` of the bracket at `open` in a type (`[N]T`,
    /// `map[K]V`). When the line ends before it, the bracket was left open
    /// at the end of the line: that is reported with a fix that closes it,
    /// the next line is parsed on its own, and this returns false, as the
    /// type can't go on.
    fn close_type_bracket(&mut self, open: Span) -> bool {
        if self.eat(T::RBracket) {
            return true;
        }
        let last = self.prev_span();
        let next = self.peek();
        if matches!(next.kind, T::Newline | T::Eof) || self.line_of(next.span.start) > self.line_of(last.start) {
            self.unclosed(open, last, "]", "`]`");
            self.restore_line_end(last);
            return false;
        }
        self.error_expected("`]`");
        true
    }

    /// Reports a line that ends with the `(` at `open` (just consumed) when
    /// the next line starts a declaration or the `end` of a block, as in
    /// `X = (` followed by `def main`: the expression is missing. The lexer
    /// joined the lines, because a newline after `(` continues the line;
    /// the line end is restored, so the next line is parsed on its own.
    fn empty_paren_line(&mut self, open: Span) -> bool {
        let restored = self.at(T::Newline);
        let next = self.tokens[self.pos..].iter().find(|t| t.kind != T::Newline).copied();
        let Some(next) = next else { return false };
        if !starts_declaration(next.kind) || self.line_of(next.span.start) == self.line_of(open.start) {
            return false;
        }
        let at = open.shrink_to_end();
        self.report(
            Diagnostic::error(codes::UNEXPECTED_TOKEN, "expected an expression, found end of line")
                .primary(at, "expected an expression")
                .secondary(open, "this `(` is never closed")
                .help("write the value after the `(`, and close it on the same line"),
        );
        if !restored {
            self.insert_line_end(at);
        }
        true
    }

    /// Whether what follows the `(`s here can only start a type: `proc`,
    /// `block`, `distinct`, `map[`, `matrix[`, `^`, `@[`, `$T`, `[]T`,
    /// `[^]` or `[dynamic]`. Used where no local can have those names.
    fn type_only_after_parens(&self) -> bool {
        let mut n = 0;
        while self.nth(n).kind == T::LParen {
            n += 1;
        }
        let tok = self.nth(n);
        match tok.kind {
            T::Caret | T::AtBracket | T::TypeParam => true,
            T::Ident => match self.text_of(tok.span) {
                "proc" | "block" | "distinct" => true,
                "map" | "matrix" => self.nth(n + 1).kind == T::LBracket && !self.nth(n + 1).space_before,
                _ => false,
            },
            T::LBracket => match self.nth(n + 1).kind {
                T::Caret => true,
                // `[]` alone is an empty array literal.
                T::RBracket => !matches!(self.nth(n + 2).kind, T::RParen | T::Comma | T::Newline | T::Eof),
                T::Ident => self.text_of(self.nth(n + 1).span) == "dynamic" && self.nth(n + 2).kind == T::RBracket,
                _ => false,
            },
            _ => false,
        }
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
    /// or inside a `quote` a splice standing for one (or a name glued from
    /// both, which is reported, see [`Parser::glued_len`]).
    fn name_len(&self, n: usize) -> Option<usize> {
        let kind = self.nth(n).kind;
        if matches!(kind, T::Ident | T::SpliceBegin)
            && let Some(len) = self.glued_len(n)
        {
            return Some(len);
        }
        match kind {
            T::Ident => Some(1),
            T::SpliceBegin if !self.quotes.is_empty() => self.splice_len(n),
            _ => None,
        }
    }

    /// The number of tokens of the name `n` tokens ahead when it is glued
    /// together from text and splices, with no space between them:
    /// `bump_#{name}`, `#{name}_count`, `@hp_#{n}` or `:#{a}_b`. Ruby
    /// builds a name from a string that way, but a splice inserts a whole
    /// name, so [`Parser::glued_name`] reports it. Outside a `quote` too,
    /// where it is never a comment, but not inside a splice's expression,
    /// where a splice is reported as nested.
    fn glued_len(&self, n: usize) -> Option<usize> {
        if self.quotes.is_empty() && self.splice_depth > 0 {
            return None;
        }
        let mut i = n;
        // The `@` or `:` of `@#{…}` or `:#{…}`.
        if matches!(self.nth(i).kind, T::AtSplice | T::ColonSplice) {
            i += 1;
        }
        let (mut splices, mut texts, mut after_splice) = (0, 0, false);
        loop {
            let tok = self.nth(i);
            if i > n && tok.space_before {
                break;
            }
            match tok.kind {
                T::SpliceBegin => {
                    i += self.splice_len(i)?;
                    splices += 1;
                    after_splice = true;
                    continue;
                }
                T::Ident | T::Const => {}
                T::IVar | T::Symbol if i == n => {}
                T::Int if after_splice => {}
                _ => break,
            }
            i += 1;
            texts += 1;
            after_splice = false;
        }
        (splices > 0 && texts > 0).then_some(i - n)
    }

    /// Parses a name glued from text and splices (see
    /// [`Parser::glued_len`]) and reports it. For the rest of the check it
    /// is the name it was meant to build, a splice of `"text#{…}".to_sym`,
    /// so the code around it still parses and what uses the name still
    /// finds it. Returns the name (after its `@` or `:`) and the span of
    /// all of it, or `None`, with nothing consumed, when no glued name
    /// starts here.
    fn glued_name(&mut self) -> Option<(Ident, Span)> {
        let len = self.glued_len(0)?;
        let first = self.peek();
        let span = first.span.to(self.nth(len - 1).span);
        let sigil = matches!(first.kind, T::AtSplice | T::ColonSplice | T::IVar | T::Symbol);
        let name_span = Span::new(self.file, span.start + u32::from(sigil), span.end);
        let (help, edits) = self.glued_fix(len, name_span);
        if self.quotes.is_empty() {
            return Some(self.glued_name_outside_quote(span, name_span, help));
        }
        let mut parts = Vec::new();
        while self.peek().span.start < span.end && !self.at(T::Eof) {
            let tok = self.peek();
            match tok.kind {
                T::SpliceBegin => {
                    self.parse_splice();
                    // The name's own splice holds the expression.
                    if let Some(expr) = self.quotes.last_mut().and_then(Vec::pop) {
                        parts.push(StrPart::Interp(expr));
                    }
                }
                T::AtSplice | T::ColonSplice => {
                    self.bump();
                }
                T::IVar | T::Symbol => {
                    self.bump();
                    parts.push(StrPart::Text(self.text_of(tok.span)[1..].to_string()));
                }
                _ => {
                    self.bump();
                    parts.push(StrPart::Text(self.text_of(tok.span).to_string()));
                }
            }
        }
        let text = Expr { kind: ExprKind::Str(parts), span: name_span };
        let to_sym =
            Callee::Method { recv: text, name: Ident { name: Name::new("to_sym"), span: name_span }, safe: false };
        let call = Call { callee: to_sym, args: Vec::new(), block: None, parens: false };
        let index = self.quotes.last_mut().map(|list| {
            list.push(Expr { kind: ExprKind::Call(Box::new(call)), span: name_span });
            list.len() as u32 - 1
        });
        self.report(
            Diagnostic::error(codes::MISPLACED_SPLICE, "a splice can't be part of a name")
                .primary(span, "a splice inserts a whole name, not part of one")
                .note("the text next to the splice is read as a separate name")
                .suggest(help, edits, Applicability::MaybeIncorrect),
        );
        let name = index.map_or_else(|| Name::new("<error>"), splice_name);
        Some((Ident { name, span: name_span }, span))
    }

    /// [`Parser::glued_name`] outside a `quote`, where nothing is spliced:
    /// skips the name at `span` (`name_span` without its `@` or `:`),
    /// reports it as one E0111 and returns the name as written
    /// (`bump_#{name}`), which no code can refer to, so a declaration of it
    /// still parses and clashes with nothing. `help` builds the name in a
    /// macro.
    fn glued_name_outside_quote(&mut self, span: Span, name_span: Span, help: String) -> (Ident, Span) {
        while self.peek().span.start < span.end && !self.at(T::Eof) {
            self.bump();
        }
        let mut diag = Diagnostic::error(codes::MISPLACED_SPLICE, "`#{` starts a splice outside a `quote`")
            .primary(span, "a splice only works inside `quote do … end`")
            .note("inside one, a splice can't be part of a name either: it inserts a whole name")
            .help(help);
        if self.in_macro {
            diag = diag.help("to build code in a macro, splice values into a `quote do … end` and return it");
        }
        self.report(diag);
        (Ident { name: Name::new(self.text_of(name_span)), span: name_span }, span)
    }

    /// The help for the glued name of `len` tokens here, written
    /// `name_span` without its `@` or `:`: build it in the macro with
    /// `to_sym` and splice that. Its edits put that on a line before the
    /// `quote` (once for each name in it) and splice the variable in place
    /// of the glued name.
    fn glued_fix(&self, len: usize, name_span: Span) -> (String, Vec<Edit>) {
        // A variable for the name, from its text and the splices that are
        // plain names (`bump_#{name}` is `bump_name`).
        let (mut var, mut spliced) = (String::new(), Vec::new());
        let mut i = 0;
        while i < len {
            let tok = self.nth(i);
            let part = match tok.kind {
                T::SpliceBegin => {
                    let n = self.splice_len(i).unwrap_or(1);
                    let inner = Span::new(self.file, tok.span.end, self.nth(i + n - 1).span.start);
                    let last = self.text_of(inner).trim().rsplit('.').next().unwrap_or_default();
                    let plain = last.starts_with(|c: char| c.is_ascii_lowercase() || c == '_')
                        && last.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
                    let part = if plain { last } else { "name" };
                    spliced.push(part);
                    i += n;
                    part
                }
                T::IVar | T::Symbol => {
                    i += 1;
                    &self.text_of(tok.span)[1..]
                }
                T::AtSplice | T::ColonSplice => {
                    i += 1;
                    continue;
                }
                _ => {
                    i += 1;
                    self.text_of(tok.span)
                }
            };
            if !var.is_empty() && !var.ends_with('_') && !part.starts_with('_') {
                var.push('_');
            }
            var.push_str(part);
        }
        let mut var = to_snake_case(&var).split('_').filter(|s| !s.is_empty()).collect::<Vec<_>>().join("_");
        if var.is_empty() || var.starts_with(|c: char| c.is_ascii_digit()) || spliced.contains(&var.as_str()) {
            var = format!("{var}_sym").trim_start_matches('_').to_string();
        }
        let written = self.text_of(name_span);
        let built = format!("{var} = \"{written}\".to_sym");
        let help = "build the name as a `Symbol` in the macro, before the `quote`, and splice it whole";
        let keyword = self.quote_keywords.last().copied().flatten();
        let Some(keyword) = keyword.filter(|_| !written.contains(['"', '\\', '\n'])) else {
            return (format!("{help}: `{built}`, then `#{{{var}}}`"), Vec::new());
        };
        let line = self.line_of(keyword.start);
        let start = self.line_starts[line];
        let indent = &self.text[start as usize..start as usize + self.indent_of_line(line)];
        let insert = Edit { span: Span::new(self.file, start, start), replacement: format!("{indent}{built}\n") };
        let mut edits = vec![Edit { span: name_span, replacement: format!("#{{{var}}}") }];
        // Another glued name with the same text already builds it.
        if !self.diags.iter().flat_map(|d| &d.helps).flat_map(|h| &h.edits).any(|e| *e == insert) {
            edits.insert(0, insert);
        }
        (help.to_string(), edits)
    }

    /// Parses a name: an identifier, or a splice, which becomes the
    /// placeholder [`splice_name`].
    fn parse_name(&mut self, what: &str) -> Ident {
        if let Some((name, _)) = self.glued_name() {
            return name;
        }
        if !self.at(T::SpliceBegin) {
            return self.expect_ident(what);
        }
        let span = self.peek().span;
        self.parse_splice_name().unwrap_or(Ident { name: Name::new("<error>"), span })
    }

    /// Parses the name of a declared type: a constant, or a splice.
    fn parse_type_name(&mut self, what: &str) -> Ident {
        if self.at(T::SpliceBegin) || self.glued_len(0).is_some() {
            self.parse_name(what)
        } else {
            self.expect_const(what)
        }
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
    /// After `=`, `.` or `def` it is meant as code, and so is a splice
    /// glued to a name (`bump_#{name}`, `#{name}_count`), which is part of
    /// the name (see [`Parser::glued_len`]).
    fn stray_is_comment(&self) -> bool {
        let prev = self.pos.checked_sub(1).map(|i| self.tokens[i]);
        let after_name = prev.is_some_and(|t| matches!(t.kind, T::Ident | T::Const | T::IVar | T::Symbol));
        if self.glued_len(0).is_some() || (after_name && !self.peek().space_before) {
            return false;
        }
        let ends_line = self.splice_len(0).is_some_and(|n| matches!(self.nth(n).kind, T::Newline | T::Eof));
        match prev.map(|t| t.kind) {
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
                Diagnostic::error(codes::MISPLACED_SPLICE, "a splice inside a splice")
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
        let mut diag = Diagnostic::error(codes::MISPLACED_SPLICE, "`#{` starts a splice outside a `quote`")
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
        if let Some(stmt) = self.quote_private_call() {
            return stmt;
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
    /// splice standing alone ([`ItemKind::Splice`]). A name glued from text
    /// and a splice (see [`Parser::glued_len`]) reads as the splice.
    fn parse_splice_item(&mut self, ctx: ItemCtx) -> ItemKind {
        let glued = self.glued_len(0);
        let after = glued.or_else(|| self.splice_len(0)).map(|n| self.nth(n));
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
            // Alone, or a macro call (`make_#{name}(1)`).
            _ if glued.is_some() => {
                let expr = self.parse_expr_cmd();
                match expr.kind {
                    ExprKind::Splice(index) => ItemKind::Splice(index),
                    _ if is_macro_call(&expr) => ItemKind::MacroCall(Box::new(expr)),
                    _ => ItemKind::Error,
                }
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
        // `private` and the space after it, for the fix on a field.
        let private_kw = self.at_kw(K::Private).then(|| self.bump_private());
        let mut private = private_kw.is_some();
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
                let mut value = self.parse_const_value();
                // The line goes on after the value (`X = Foo{`), which is
                // reported at the end of the item: the value isn't the
                // whole of it, so it isn't checked on its own.
                if !self.at_stmt_end() && !(self.at(T::SpliceBegin) && self.quotes.is_empty()) {
                    value.kind = ExprKind::Error;
                }
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
            T::SpliceBegin if self.quotes.is_empty() && self.glued_len(0).is_none() => {
                // Most likely a comment written without a space after `#`.
                self.stray_splice();
                return None;
            }
            T::SpliceBegin => self.parse_splice_item(ctx),
            // A name glued from text and a splice (`bump_#{name}: Int`),
            // reported and read as the splice.
            T::Ident | T::Const if self.glued_len(0).is_some() => self.parse_splice_item(ctx),
            T::Ident => {
                let save = self.pos;
                let splices = self.splice_mark();
                let expr = self.parse_expr_cmd();
                if is_macro_call(&expr) && !self.at_statement_rest() {
                    ItemKind::MacroCall(Box::new(expr))
                } else {
                    self.pos = save;
                    self.rewind_splices(splices);
                    let stmt = self.parse_stmt();
                    self.top_level_statement(stmt.span, ctx);
                    ItemKind::Error
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
        // Fields are always public. Elsewhere a field is already an error.
        if let (Some((kw, removal)), ItemKind::Field(f), ItemCtx::Struct) = (private_kw, &kind, ctx) {
            self.private_field(kw, removal, f);
            private = false;
        }
        // `private` hides a declaration by its name; these have none.
        if let Some((kw, removal)) = private_kw
            && let Some((what, why)) = nameless_item(&kind)
        {
            self.report(
                Diagnostic::error(codes::UNEXPECTED_TOKEN, format!("{what} can't be `private`"))
                    .primary(kw, "`private` has nothing to hide here")
                    .note(why)
                    .suggest(
                        "remove `private`",
                        vec![Edit { span: removal, replacement: String::new() }],
                        Applicability::MachineApplicable,
                    ),
            );
            private = false;
        }
        let span = start.to(self.prev_span());
        Some(Item { kind, span, attrs, private, doc })
    }

    /// Whether what follows a line's first expression makes the line a
    /// statement: an assignment (`=`, `+=`), a list of values or targets
    /// (`,`) or a modifier (`if`, `unless`).
    fn at_statement_rest(&self) -> bool {
        self.at(T::Eq) || is_assign_op(self.kind()) || self.at(T::Comma) || self.at_modifier()
    }

    /// `private` before a macro call on a line of a `quote` (`private
    /// helpers :hp`), as at package level: a declaration-level call whose
    /// declarations are all private. Anything else after `private` (a
    /// local, an assignment) is left to the statement parser, which
    /// reports `private` there.
    fn quote_private_call(&mut self) -> Option<Stmt> {
        if !self.at_kw(K::Private)
            || self.nth(1).kind != T::Ident
            || (self.nth(2).kind == T::Colon && !self.nth(2).space_before)
        {
            return None;
        }
        let start = self.peek().span;
        let mark = self.mark();
        self.bump();
        let expr = self.parse_expr_cmd();
        if !is_macro_call(&expr) || self.at_statement_rest() || !self.clean_since(&mark) {
            self.rewind(mark);
            return None;
        }
        let span = start.to(self.prev_span());
        let item =
            Item { kind: ItemKind::MacroCall(Box::new(expr)), span, attrs: Vec::new(), private: true, doc: None };
        Some(Stmt { kind: StmtKind::Item(Box::new(item)), span, attrs: Vec::new() })
    }

    /// `private` and the space after it, consumed, for a fix that removes
    /// both.
    fn bump_private(&mut self) -> (Span, Span) {
        let kw = self.bump().span;
        (kw, kw.to(self.peek().span.shrink_to_start()))
    }

    /// Reports `private` written before a statement (E0105), at `kw` (with
    /// the space after it, `removal`). The statement after it is parsed as
    /// if it weren't there.
    fn private_statement(&mut self, kw: Span, removal: Span) {
        let (label, note) = if self.quotes.is_empty() {
            (
                "this line is a statement",
                "`private` hides a method outside its type, or a type, constant or macro outside its package; \
                 a local variable is visible only in its method anyway",
            )
        } else {
            (
                "this line of the `quote` reads as a statement",
                "`private` hides a method outside its type, or a type, constant or macro outside its package; \
                 fields are always public, and a local variable is visible only in its method",
            )
        };
        self.report(
            Diagnostic::error(codes::UNEXPECTED_TOKEN, "`private` applies only to declarations")
                .primary(kw, label)
                .note(note)
                .suggest(
                    "remove `private`",
                    vec![Edit { span: removal, replacement: String::new() }],
                    Applicability::MachineApplicable,
                ),
        );
    }

    /// Reports `private` written on a struct field (E0105): fields are
    /// always public. The field is kept, so nothing cascades.
    fn private_field(&mut self, kw: Span, removal: Span, field: &FieldDecl) {
        let name = self.text_of(field.name.span);
        self.report(
            Diagnostic::error(codes::UNEXPECTED_TOKEN, "a field can't be `private`")
                .primary(kw, "fields are always public")
                .note(format!(
                    "any code can read and write `{name}` directly; `private` applies to methods and other declarations"
                ))
                .suggest(
                    "remove `private`",
                    vec![Edit { span: removal, replacement: String::new() }],
                    Applicability::MachineApplicable,
                ),
        );
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
        if let Some((name, _)) = self.glued_name() {
            return name;
        }
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
        let outer_array_params = self.array_params.replace(Vec::new());
        let ParamList { mut params, block, c_variadic, left_open } =
            if self.at(T::LParen) { self.parse_params(owner) } else { ParamList::default() };
        let uninferred = std::mem::take(&mut self.uninferred);
        let mut ret = if self.eat(T::Arrow) { Some(self.parse_type()) } else { None };
        let array_params = std::mem::replace(&mut self.array_params, outer_array_params).unwrap_or_default();
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
        let mut body = if self.eat(T::Eq) {
            self.skip_newlines();
            FnBody::Expr(Box::new(self.parse_expr_cmd()))
        } else if left_open && self.declaration_ahead() {
            // `def foo(a: Int,` followed by `def main`: the line, which
            // was left open, ended the method too, and the next
            // declaration is parsed on its own.
            FnBody::Block(Vec::new())
        } else {
            self.openers.push(Opener { keyword: "def", span: def_tok.span.to(name.span) });
            let body = self.parse_block_body();
            self.expect_end();
            FnBody::Block(body)
        };
        for found in &array_params {
            self.report_array_value_param(is_macro, found, &mut params, &mut ret, &mut body);
        }
        self.pop_def_scope();
        self.in_macro = outer_macro;
        let yields = std::mem::replace(&mut self.yield_seen, outer_yield);
        FnDecl { name, is_static, is_macro, params, block, ret, body, sig_span, yields, c_variadic }
    }

    fn parse_params(&mut self, owner: ParamsOf) -> ParamList {
        let open = self.bump().span;
        let list = List {
            open,
            close: T::RParen,
            closer: ")",
            what: "`)` to close the parameter list",
            item: "parameter",
            items: "parameters",
            kind: Items::Params,
        };
        let mut params = Vec::new();
        let mut stars = Vec::new();
        let mut block = None;
        let mut variadic = None;
        let mut loose = Vec::new();
        let closed = loop {
            if self.list_left_open(&list) {
                break false;
            }
            self.skip_newlines();
            if self.at(T::RParen) || self.at(T::Eof) {
                break true;
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
                break true;
            }
            let start = self.peek().span;
            if self.at(T::TypeParam) {
                loose.push(self.skip_loose_type_param());
            } else if self.eat(T::Amp) {
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
            match self.list_step(&list) {
                ListStep::Item => {}
                ListStep::Close => break true,
                ListStep::Open => break false,
            }
        };
        if closed {
            self.expect(T::RParen, list.what);
        }
        for (index, star) in stars {
            self.check_variadic_param(owner, &mut params, index, star);
        }
        for tp in &loose {
            self.report_loose_type_param(owner, &mut params, tp);
        }
        ParamList { params, block, c_variadic: variadic, left_open: !closed }
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
    /// Parses `[$N]T` after the `[`, as if `$N` introduced a value
    /// parameter. In a method's signature it reads as the slice `[]T` and is
    /// reported by `parse_def` (see [`ArrayValueParam`]); anywhere else it is
    /// reported here.
    fn array_value_param(&mut self, open: Span) -> TypeKind {
        let tok = self.bump();
        self.bump();
        let name = Name::new(&self.text_of(tok.span)[1..]);
        let brackets = open.to(self.prev_span());
        let elem = self.parse_type();
        if let Some(found) = &mut self.array_params {
            found.push(ArrayValueParam { name, tok: tok.span, brackets });
            return TypeKind::Slice(Box::new(elem));
        }
        let elem_text = self.text_of(elem.span).to_string();
        self.report(
            Diagnostic::error(codes::UNEXPECTED_TOKEN, format!("`${name}` can't be an array length here"))
                .primary(tok.span, "only a generic struct takes value parameters")
                .help(format!(
                    "write a constant length, like `[4]{elem_text}`; a generic struct declares a value parameter, like `struct Pool($T, ${name}: Int)`, and writes `[{name}]T`"
                )),
        );
        TypeKind::Array(Box::new(Expr { kind: ExprKind::Error, span: tok.span }), Box::new(elem))
    }

    /// Reports a `[$N]` array length in a method's signature (E0105): a
    /// method can't take a value parameter. The parameter already reads as
    /// a slice, which an array argument converts to, and the uses of `N` in
    /// the method are dropped, so they aren't reported again. The fix makes
    /// the parameter a slice and reads `N` as its `.size`.
    fn report_array_value_param(
        &mut self,
        is_macro: bool,
        found: &ArrayValueParam,
        params: &mut [Param],
        ret: &mut Option<TypeExpr>,
        body: &mut FnBody,
    ) {
        let name = found.name;
        let mut drop = DropValueParam { name, uses: Vec::new(), in_type: 0 };
        for p in params.iter_mut() {
            if let Some(default) = &mut p.default {
                drop.visit_expr(default);
            }
        }
        if let Some(ret) = ret {
            drop.visit_type(ret);
        }
        match body {
            FnBody::Expr(e) => drop.visit_expr(e),
            FnBody::Block(stmts) => drop.visit_stmts(stmts),
        }
        let what = if is_macro { "a macro" } else { "a method" };
        let holder = params.iter().find(|p| p.ty.span.start <= found.tok.start && found.tok.end <= p.ty.span.end);
        let label = match holder {
            Some(_) => format!("`${name}` would be the length of the array passed in"),
            None => format!("`${name}` would be the length of the array returned"),
        };
        let mut diag =
            Diagnostic::error(codes::UNEXPECTED_TOKEN, format!("{what} can't take a value parameter like `${name}`"))
                .primary(found.tok, label)
                .note(format!("only generic structs take value parameters, like `struct Pool($T, ${name}: Int)`"));
        match holder {
            // The array is the parameter itself, so `N` is its size.
            Some(param) if param.ty.span.start == found.brackets.start => {
                let param = param.name.as_str().to_string();
                let in_type = drop.uses.iter().any(|(_, t)| *t);
                let mut edits = vec![Edit { span: found.brackets, replacement: "[]".into() }];
                if !in_type {
                    edits.extend(
                        drop.uses.iter().map(|(span, _)| Edit { span: *span, replacement: format!("{param}.size") }),
                    );
                }
                let applicability =
                    if in_type { Applicability::MaybeIncorrect } else { Applicability::MachineApplicable };
                diag = diag.suggest(
                    format!("take a slice: an array argument converts to one, and `{param}.size` is its length"),
                    edits,
                    applicability,
                );
            }
            Some(_) => {
                diag = diag.suggest(
                    "take a slice: an array converts to one",
                    vec![Edit { span: found.brackets, replacement: "[]".into() }],
                    Applicability::MaybeIncorrect,
                );
            }
            None => {
                diag = diag.help("return an array of a constant length, like `[4]Int`, or a slice, like `[]Int`");
            }
        }
        self.report(diag);
    }

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
            // Members are always public, like fields.
            if self.at_kw(K::Private) && (self.enum_member_at(1) || self.upper_member_at(1)) {
                let (kw, removal) = self.bump_private();
                self.report(
                    Diagnostic::error(codes::UNEXPECTED_TOKEN, "an enum member can't be `private`")
                        .primary(kw, "enum members are always public")
                        .note(
                            "any code that can name the enum can select its members; `private` applies to methods and other declarations",
                        )
                        .suggest(
                            "remove `private`",
                            vec![Edit { span: removal, replacement: String::new() }],
                            Applicability::MachineApplicable,
                        ),
                );
            }
            if self.enum_member_at(0) {
                loop {
                    let member = self.parse_enum_member();
                    let value = if self.eat(T::Eq) { Some(self.parse_expr()) } else { None };
                    members.push(EnumMember { name: member, value });
                    if !self.eat(T::Comma) {
                        break;
                    }
                    self.skip_newlines();
                }
            } else if self.upper_member_at(0) {
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

    /// Whether the token `n` ahead starts an enum member. A splice is a
    /// member when a value or another member follows; standing alone it is
    /// an `ItemKind::Splice`. `struct`, `enum` and `union` standing alone
    /// are members (`TypeKind.struct`).
    fn enum_member_at(&self, n: usize) -> bool {
        match self.name_len(n) {
            Some(len) if self.nth(n).kind == T::Ident => {
                matches!(self.nth(n + len).kind, T::Newline | T::Eq | T::Comma | T::Kw(K::End))
            }
            Some(len) => matches!(self.nth(n + len).kind, T::Eq | T::Comma),
            None => {
                is_keyword_member(self.nth(n).kind)
                    && matches!(self.nth(n + 1).kind, T::Newline | T::Eq | T::Comma | T::Kw(K::End))
            }
        }
    }

    /// Whether the token `n` ahead is an uppercase name standing as an enum
    /// member, which is reported.
    fn upper_member_at(&self, n: usize) -> bool {
        self.nth(n).kind == T::Const && matches!(self.nth(n + 1).kind, T::Newline | T::Comma | T::Kw(K::End))
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
        if let Some((name, span)) = self.glued_name() {
            return Some(Ident { span, ..name });
        }
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
        if let Some((name, span)) = self.glued_name() {
            return TypeExpr { kind: name.splice_index().map_or(TypeKind::Error, TypeKind::Splice), span };
        }
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
                    if !self.close_type_bracket(start) {
                        return TypeExpr { kind: TypeKind::Error, span: start.to(self.prev_span()) };
                    }
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
                } else if self.at(T::TypeParam) && self.nth(1).kind == T::RBracket {
                    self.array_value_param(start)
                } else {
                    let len = self.parse_expr();
                    let elem = if self.close_type_bracket(start) {
                        self.parse_type()
                    } else {
                        TypeExpr { kind: TypeKind::Error, span: self.prev_span() }
                    };
                    TypeKind::Array(Box::new(len), Box::new(elem))
                }
            }
            T::LParen => {
                self.bump();
                // A line end that the list doesn't continue leaves it open:
                // the `)` goes at the end of the line, and the next line is
                // parsed on its own.
                let list = List {
                    open: start,
                    close: T::RParen,
                    closer: ")",
                    what: "`)`",
                    item: "type",
                    items: "types",
                    kind: Items::Unnamed,
                };
                let mut elems = self.parse_type_list(&list, |p| Some(p.parse_type()));
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
                let open = self.bump().span;
                let key = self.parse_type();
                let value = if self.close_type_bracket(open) {
                    self.parse_type()
                } else {
                    TypeExpr { kind: TypeKind::Error, span: self.prev_span() }
                };
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
                let mut params = Vec::new();
                let mut variadic = false;
                if self.at(T::LParen) && !self.peek().space_before {
                    let list = List {
                        open: self.bump().span,
                        close: T::RParen,
                        closer: ")",
                        what: "`)`",
                        item: "parameter",
                        items: "parameters",
                        kind: Items::Args,
                    };
                    params = self.parse_type_list(&list, |p| {
                        if p.eat(T::DotDotDot) {
                            variadic = true;
                            p.skip_newlines();
                            return None;
                        }
                        if p.at(T::Ident) && p.nth(1).kind == T::Colon {
                            p.bump();
                            p.bump();
                        }
                        Some(p.parse_type())
                    });
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
                let open = self.bump().span;
                let rows = self.parse_expr();
                self.expect(T::Comma, "`,`");
                let cols = self.parse_expr();
                let elem = if self.close_type_bracket(open) {
                    self.parse_type()
                } else {
                    TypeExpr { kind: TypeKind::Error, span: self.prev_span() }
                };
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

    /// Parses the items of a list in a type (a tuple's types, or a generic
    /// type's or proc type's arguments) after its opener, through its
    /// closer, as [`Parser::list_step`] reads lists: a list left open at
    /// the end of a line is one error there, with a fix that closes it.
    /// `item` parses one item, or returns `None` where the list must end.
    fn parse_type_list<I>(&mut self, list: &List, mut item: impl FnMut(&mut Self) -> Option<I>) -> Vec<I> {
        let mut items = Vec::new();
        let closed = loop {
            if self.list_left_open(list) {
                break false;
            }
            self.skip_newlines();
            if self.at(list.close) || self.at(T::Eof) {
                break true;
            }
            let Some(parsed) = item(self) else { break true };
            items.push(parsed);
            match self.list_step(list) {
                ListStep::Item => {}
                ListStep::Close => break true,
                ListStep::Open => break false,
            }
        };
        if closed {
            self.expect(list.close, list.what);
        }
        items
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
            let list = List {
                open: self.bump().span,
                close: T::RParen,
                closer: ")",
                what: "`)`",
                item: "argument",
                items: "arguments",
                kind: Items::Unnamed,
            };
            args = self.parse_type_list(&list, |p| Some(p.parse_generic_arg()));
        }
        if optional_suffix.is_some() {
            self.pending_optional = optional_suffix;
        }
        TypeExpr { kind: TypeKind::Path { segments, args }, span: start.to(self.prev_span()) }
    }

    fn parse_generic_arg(&mut self) -> GenericArg {
        match self.kind() {
            // A name may be a type or a constant, which the checker tells
            // apart; one followed by more (`SIZE * 2`, `(N + 1)`) is a
            // value for a value parameter.
            T::Const | T::LParen => {
                let ends = |p: &Self| {
                    let next = p.tokens[p.pos..].iter().find(|t| t.kind != T::Newline);
                    next.is_some_and(|t| matches!(t.kind, T::Comma | T::RParen))
                };
                let mark = self.mark();
                if let Some(ty) = self.try_parse_type()
                    && ends(self)
                {
                    return GenericArg::Type(ty);
                }
                self.rewind(mark);
                let mark = self.mark();
                let value = self.parse_expr();
                if self.clean_since(&mark) && ends(self) {
                    return GenericArg::Expr(value);
                }
                // Neither: report it as the type it most likely is, unless
                // no type starts that way (`(2 + )`).
                let first = self.tokens[mark.pos..].iter().find(|t| t.kind != T::LParen).map(|t| t.kind);
                if !first.is_some_and(|k| {
                    matches!(
                        k,
                        T::Const | T::Ident | T::Caret | T::LBracket | T::TypeParam | T::AtBracket | T::SpliceBegin
                    )
                }) {
                    return GenericArg::Expr(value);
                }
                self.rewind(mark);
                GenericArg::Type(self.parse_type())
            }
            T::Caret | T::LBracket | T::TypeParam => GenericArg::Type(self.parse_type()),
            T::Ident if matches!(self.text_of(self.peek().span), "map" | "proc" | "distinct") => {
                GenericArg::Type(self.parse_type())
            }
            _ => GenericArg::Expr(self.parse_expr()),
        }
    }

    /// Tries to parse a type at the current position without reporting
    /// errors; restores the position on failure.
    fn try_parse_type(&mut self) -> Option<TypeExpr> {
        let mark = self.mark();
        let ty = self.parse_type();
        if !self.clean_since(&mark) || matches!(ty.kind, TypeKind::Error) {
            self.rewind(mark);
            return None;
        }
        Some(ty)
    }

    /// The parser's state here, for a speculative parse to go back to.
    fn mark(&self) -> Mark {
        Mark {
            pos: self.pos,
            diags: self.diags.len(),
            last_error_at: self.last_error_at,
            splices: self.splice_mark(),
            inserted: self.inserted.len(),
        }
    }

    /// Whether nothing was reported, and no line end put back, since `mark`.
    fn clean_since(&self, mark: &Mark) -> bool {
        self.diags.len() == mark.diags && self.inserted.len() == mark.inserted
    }

    /// Goes back to `mark`, dropping what was reported and the line ends
    /// put back since.
    fn rewind(&mut self, mark: Mark) {
        self.pos = mark.pos;
        self.diags.truncate(mark.diags);
        self.last_error_at = mark.last_error_at;
        self.rewind_splices(mark.splices);
        while self.inserted.len() > mark.inserted {
            let at = self.inserted.pop().expect("invariant: more line ends than marked");
            self.tokens.remove(at);
        }
    }

    // ----- statements ----------------------------------------------------

    fn parse_block_body(&mut self) -> Vec<Stmt> {
        self.parse_body(false)
    }

    /// Parses statements up to the end of a block. In a `{ … }` block
    /// (`brace`), a declaration starting a line ends the body too: the
    /// block was left open before it, as in `X = xs.map { |x| x * 2`
    /// followed by `def main`.
    fn parse_body(&mut self, brace: bool) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        let (start, errors) = (self.peek().span, self.diags.len());
        loop {
            self.skip_newlines();
            if matches!(
                self.kind(),
                T::Eof | T::RBrace | T::SpliceEnd | T::Kw(K::End) | T::Kw(K::Else) | T::Kw(K::Elsif) | T::Kw(K::When)
            ) || (brace
                && starts_declaration(self.kind())
                && (!self.at_kw(K::Private) || starts_declaration(self.nth(1).kind)))
            {
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
        let decl_kw = |kind| {
            matches!(
                kind,
                T::Kw(K::Def | K::Struct | K::Enum | K::Union | K::Module | K::Extend | K::Overload | K::Macro)
            )
        };
        if self.at_kw(K::Private) && !decl_kw(self.nth(1).kind) {
            let (kw, removal) = self.bump_private();
            self.private_statement(kw, removal);
        }
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
            kind if decl_kw(kind) || kind == T::Kw(K::Private) => {
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
        let (start, splices, errors) = (self.pos, self.splice_mark(), self.diags.len());
        let mut lhs = match self.paren_optional_type() {
            Some(ty) => Expr { span: ty.span, kind: ExprKind::Type(Box::new(ty)) },
            None => self.parse_prefix(cmd),
        };
        loop {
            let tok = self.peek();
            // `t = Int?`: a conditional needs a space before its `?`, so one
            // right after a type's name (`Int`, `rl.Color`, `Pool(Ball, 64)`)
            // ends the type, unless a conditional's `:` follows, as in
            // `N? a : b`. The type is written in place, like a call
            // argument's (`parse_type_arg`).
            if tok.kind == T::Question
                && !tok.space_before
                && names_type(&lhs)
                && self.diags.len() == errors
                && !self.conditional_colon_ahead()
            {
                if let Some(ty) = self.reparse_as_type(start, splices) {
                    lhs = Expr { span: ty.span, kind: ExprKind::Type(Box::new(ty)) };
                    continue;
                }
                lhs = self.parse_prefix(cmd);
            }
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

    /// Whether the `?` here is followed by a conditional's `:` outside
    /// brackets: on the rest of its line, or, when the `?` ends its line, on
    /// the next line with a space before it (`f(FLAG?` then `1 : 2)`; a
    /// declaration like `hp: Int` has none).
    fn conditional_colon_ahead(&self) -> bool {
        let next_line = self.nth(1).kind == T::Newline;
        let mut depth = 0usize;
        for tok in &self.tokens[self.pos + usize::from(next_line) + 1..] {
            match tok.kind {
                T::LParen | T::LBracket | T::LBrace | T::AtBracket | T::StrBegin | T::SpliceBegin => depth += 1,
                T::RParen | T::RBracket | T::RBrace | T::StrEnd | T::SpliceEnd if depth > 0 => depth -= 1,
                T::Colon if depth == 0 => return !next_line || tok.space_before,
                T::Newline | T::Eof | T::RParen | T::RBracket | T::RBrace | T::SpliceEnd => return false,
                _ => {}
            }
        }
        false
    }

    /// Parses a parenthesized type made optional, like
    /// `(proc(Int) -> Int)?`, written in place: a `(` whose `)` is followed
    /// directly by `?`, with no conditional's `:` after it, that parses as
    /// a type ending in that `?`. Returns `None`, with nothing consumed,
    /// otherwise, as for `(a > b)? x : y`.
    fn paren_optional_type(&mut self) -> Option<TypeExpr> {
        if !self.at(T::LParen) {
            return None;
        }
        let mut depth = 0usize;
        let mut close = None;
        for (i, tok) in self.tokens[self.pos..].iter().enumerate() {
            match tok.kind {
                T::LParen => depth += 1,
                T::RParen if depth == 1 => {
                    close = Some(self.pos + i);
                    break;
                }
                T::RParen => depth -= 1,
                T::Eof => return None,
                _ => {}
            }
        }
        let question = close? + 1;
        let after = *self.tokens.get(question)?;
        if after.kind != T::Question || after.space_before {
            return None;
        }
        let save = (self.pos, self.splice_mark());
        self.pos = question;
        let colon = self.conditional_colon_ahead();
        self.pos = save.0;
        if colon {
            return None;
        }
        let ty = self.try_parse_type()?;
        if matches!(ty.kind, TypeKind::Optional(_)) && self.only_type(&ty) {
            return Some(ty);
        }
        self.pos = save.0;
        self.rewind_splices(save.1);
        None
    }

    /// Parses again, as a type, the expression that starts at token `start`
    /// and ends before the `?` here (with the splice list at `splices`
    /// before it), when the type goes on through that `?`. Otherwise
    /// returns `None` with the position back at `start`.
    fn reparse_as_type(&mut self, start: usize, splices: usize) -> Option<TypeExpr> {
        let question = self.pos;
        self.pos = start;
        self.rewind_splices(splices);
        let ty = self.try_parse_type();
        if ty.is_some() && self.pos > question {
            return ty;
        }
        self.pos = start;
        self.rewind_splices(splices);
        None
    }

    /// Reports `Int ?` before `)` or `,`, where the `?` (just consumed) reads
    /// as an unfinished `x ? a : b` but a type's `?` was likely meant: one
    /// after a space is a conditional's.
    fn spaced_optional(&mut self, lhs: &Expr, q: Token) -> bool {
        if !names_type(lhs) || !q.space_before || !matches!(self.kind(), T::RParen | T::Comma) {
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
        if let Some((name, span)) = self.glued_name() {
            // It reads as the splice of the name it builds: a name, field,
            // symbol or call.
            let kind = match tok.kind {
                T::AtSplice | T::IVar => ExprKind::IVar(name.name),
                T::ColonSplice | T::Symbol => ExprKind::Symbol(name.name),
                _ if self.at(T::LParen) && !self.peek().space_before => {
                    return self.parse_call_with_parens(Callee::Name(name), span);
                }
                _ if cmd && self.can_start_command_arg(self.peek()) => {
                    return self.parse_command_call(Callee::Name(name), span);
                }
                _ => name.splice_index().map_or(ExprKind::Error, ExprKind::Splice),
            };
            return Expr { kind, span };
        }
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
                self.quote_keywords.push((self.splice_depth == 0).then_some(span));
                let splice_depth = std::mem::replace(&mut self.splice_depth, 0);
                let outer_yield = std::mem::replace(&mut self.yield_seen, false);
                self.push_def_scope();
                let body = self.parse_quote_body();
                self.pop_def_scope();
                self.yield_seen = outer_yield;
                self.splice_depth = splice_depth;
                let splices = self.quotes.pop().unwrap_or_default();
                self.quote_keywords.pop();
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
                // `X = {` followed by a declaration: the `}` is missing.
                if self.line_cut_before_declaration().is_some() {
                    self.unclosed(span, span, "}", "`}`");
                    self.restore_line_end(span);
                    return simple(ExprKind::Zero);
                }
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
                let list = List {
                    open: span,
                    close: T::RBracket,
                    closer: "]",
                    what: "`]` to close the array",
                    item: "element",
                    items: "elements",
                    kind: Items::Unnamed,
                };
                let mut elems = Vec::new();
                let closed = loop {
                    if self.list_left_open(&list) {
                        break false;
                    }
                    self.skip_newlines();
                    if self.at(T::RBracket) || self.at(T::Eof) {
                        break true;
                    }
                    elems.push(self.parse_expr());
                    match self.list_step(&list) {
                        ListStep::Item => {}
                        ListStep::Close => break true,
                        ListStep::Open => break false,
                    }
                };
                self.no_do = saved;
                if closed {
                    self.expect(T::RBracket, list.what);
                }
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
                if self.empty_paren_line(span) {
                    return Expr { kind: ExprKind::Error, span };
                }
                let saved = self.no_do;
                self.no_do = false;
                self.skip_newlines();
                let inner = self.parse_expr_cmd();
                self.no_do = saved;
                // The expression is complete: newlines may only lead to its
                // `)`. Anything else on the next line (a `def`, another
                // statement) is not part of it, so the `(` was left open at
                // the end of this line.
                let (last, before) = (self.prev_span(), self.pos);
                self.skip_newlines();
                if !self.eat(T::RParen) {
                    if self.pos > before || self.at(T::Eof) {
                        self.pos = before;
                        self.unclosed_paren(span, last);
                    } else {
                        self.error_expected("`)`");
                    }
                }
                Expr { kind: ExprKind::Paren(Box::new(inner)), span: span.to(self.prev_span()) }
            }
            T::Arrow => self.parse_lambda(),
            T::IVar => {
                self.bump();
                let name = Ident { name: Name::new(&self.text_of(span)[1..]), span };
                // `@name(args)` calls the proc the field holds, like
                // `self.name(args)`.
                if self.at(T::LParen) && !self.peek().space_before {
                    return self.parse_call_with_parens(Callee::IVar(name), span);
                }
                simple(ExprKind::IVar(name.name))
            }
            T::Const => {
                self.bump();
                let name = Ident { name: Name::new(self.text_of(span)), span };
                if self.at(T::LParen) && !self.peek().space_before {
                    return self.parse_call_with_parens(Callee::Name(name), span);
                }
                if self.struct_literal_ahead() {
                    return self.parse_struct_literal(simple(ExprKind::Const(name.name)));
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
                if let Some(last) = self.line_cut_before_declaration() {
                    // `X = 1 +` followed by `def main`: the line ended with
                    // an operator (or `=`, `,`), and a declaration can't
                    // continue it.
                    let at = last.span.shrink_to_end();
                    let op = self.text_of(last.span);
                    self.report(
                        Diagnostic::error(codes::UNEXPECTED_TOKEN, "expected an expression, found end of line")
                            .primary(at, "expected an expression")
                            .secondary(last.span, format!("the line goes on after this `{op}`"))
                            .help(format!(
                                "write the rest of the expression after `{op}` on this line; \
                                 a declaration on the next line can't continue it"
                            )),
                    );
                    self.restore_line_end(last.span);
                    return Expr { kind: ExprKind::Error, span: at };
                }
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
        let open = self.bump().span;
        let saved = self.no_do;
        self.no_do = false;
        let list = List {
            open,
            close: T::RParen,
            closer: ")",
            what: "`)` to close the argument list",
            item: "argument",
            items: "arguments",
            kind: Items::Args,
        };
        let mut args = Vec::new();
        let closed = loop {
            if self.list_left_open(&list) {
                break false;
            }
            self.skip_newlines();
            if self.at(T::RParen) || self.at(T::Eof) {
                break true;
            }
            args.push(self.parse_arg(types, Some(list.open)));
            match self.list_step(&list) {
                ListStep::Item => {}
                ListStep::Close => break true,
                ListStep::Open => break false,
            }
        };
        self.no_do = saved;
        if closed {
            self.expect(T::RParen, list.what);
        }
        args
    }

    /// Parses one call argument. `open` is the `(` of the argument list,
    /// and `None` for a call without parentheses, whose last argument ends
    /// with the statement.
    fn parse_arg(&mut self, types: bool, open: Option<Span>) -> Arg {
        let parens = open.is_some();
        if let Some(len) = self.name_len(0)
            && self.nth(len).kind == T::Colon
        {
            let name = self.parse_name("an argument name");
            let colon = self.bump().span;
            // The value may go on the next line, indented deeper than the
            // call's own line. `add(a:` followed by a declaration, an
            // `end` or a line indented no deeper is missing it, and the
            // line is left for the list to end.
            let call_line = self.line_of(open.unwrap_or(colon).start);
            if self.at(T::Newline)
                && self.tokens[self.pos..].iter().find(|t| t.kind != T::Newline).is_some_and(|t| {
                    starts_declaration(t.kind)
                        || t.kind == T::Eof
                        || self.indent_of_line(self.line_of(t.span.start)) <= self.indent_of_line(call_line)
                })
            {
                let at = colon.shrink_to_end();
                let text = self.text_of(name.span);
                self.report(
                    Diagnostic::error(codes::UNEXPECTED_TOKEN, "expected an expression, found end of line")
                        .primary(at, format!("expected the value of `{text}`"))
                        .help(format!("write the value after `{text}:` on this line")),
                );
                return Arg { name: Some(name), value: Expr { kind: ExprKind::Error, span: at }, splat: false };
            }
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
            // `(proc(Int) -> Int)?`: no expression ends in a `?` of its own.
            T::LParen => types || self.scan_arg().0,
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
        // `Pool(Int, 4){a: 1}`: a generic struct's instance, like any
        // constant, never takes a block.
        let name = match &callee {
            Callee::Name(name) | Callee::IVar(name) | Callee::Method { name, .. } => *name,
        };
        if is_constant_name(name.as_str()) && self.struct_literal_ahead() {
            let call = Call { callee, args, block: None, parens: true };
            let ty = Expr { kind: ExprKind::Call(Box::new(call)), span: start.to(self.prev_span()) };
            return self.parse_struct_literal(ty);
        }
        let block = self.parse_block_arg();
        Expr {
            kind: ExprKind::Call(Box::new(Call { callee, args, block, parens: true })),
            span: start.to(self.prev_span()),
        }
    }

    /// Whether a `{` written right after the token before (a constant, as
    /// in `Foo{`) starts a struct literal: anything but a block's `|x|`.
    fn struct_literal_ahead(&self) -> bool {
        self.at(T::LBrace) && !self.peek().space_before && !matches!(self.nth(1).kind, T::Pipe | T::OrOr)
    }

    /// Parses `{…}` after the type `ty` (`Foo`, `geo.Vec2`,
    /// `Pool(Int, 4)`): a struct literal, as Odin, Go, Rust and Zig write
    /// it. Wid builds a struct with `new`, so this is one E0113, with a fix
    /// that writes `Foo.new(a: 1)` for `Foo{a: 1}` (and `a: 1` for Odin's
    /// `a = 1`). The braces are read as the arguments of that `new` call,
    /// so the value has the struct's type and adds no more errors. Left
    /// open at the end of its line, it is this error alone, and the fix
    /// also closes the call.
    fn parse_struct_literal(&mut self, ty: Expr) -> Expr {
        let open = self.bump().span;
        let saved = self.no_do;
        self.no_do = false;
        let list = List {
            open,
            close: T::RBrace,
            closer: "}",
            what: "`}` to close the struct literal",
            item: "argument",
            items: "arguments",
            kind: Items::Args,
        };
        let errors = self.diags.len();
        let mut edits = vec![Edit { span: open, replacement: ".new(".into() }];
        let mut args = Vec::new();
        // The list's own report that it was left open, which this error
        // replaces: the diagnostics and last error position before it.
        let mut left_open = None;
        loop {
            let before = (self.diags.len(), self.last_error_at);
            if self.list_left_open(&list) {
                left_open = Some(before);
                break;
            }
            self.skip_newlines();
            if self.at(T::RBrace) || self.at(T::Eof) {
                break;
            }
            // Odin's `Foo{a = 1}` names a field with `=`.
            if let Some(len) = self.name_len(0)
                && self.nth(len).kind == T::Eq
            {
                let name = self.parse_name("a field name");
                let eq = self.bump().span;
                edits.push(Edit { span: name.span.shrink_to_end().to(eq), replacement: ":".into() });
                args.push(Arg { name: Some(name), value: self.parse_expr(), splat: false });
            } else {
                args.push(self.parse_arg(false, Some(open)));
            }
            let before = (self.diags.len(), self.last_error_at);
            match self.list_step(&list) {
                ListStep::Item => {}
                ListStep::Close => break,
                ListStep::Open => {
                    left_open = Some(before);
                    break;
                }
            }
        }
        self.no_do = saved;
        let fixable = match left_open {
            Some((count, last_error_at)) => {
                self.diags.truncate(count);
                self.last_error_at = last_error_at;
                // The line's last token, before the line end put back.
                let last = self.tokens[..self.pos].iter().rev().find(|t| t.kind != T::Newline).map_or(open, |t| t.span);
                edits.push(Edit { span: last.shrink_to_end(), replacement: ")".into() });
                true
            }
            None if self.at(T::RBrace) => {
                let close = self.bump().span;
                edits.push(Edit { span: close, replacement: ")".into() });
                true
            }
            None => {
                self.expect(T::RBrace, list.what);
                false
            }
        };
        let text = self.text_of(ty.span).to_string();
        let applicability =
            if self.diags.len() > errors { Applicability::MaybeIncorrect } else { Applicability::MachineApplicable };
        let mut diag = Diagnostic::error(codes::STRUCT_LITERAL, "Wid has no struct literal syntax")
            .primary(ty.span.to(open), format!("structs are built with `new`: `{text}.new(…)`"))
            .note("`new` takes the fields by name or in order; the ones not given get their default or zero");
        if fixable {
            diag = diag.suggest(format!("call `{text}.new` with the fields as arguments"), edits, applicability);
        }
        self.report(diag);
        let new = Ident { name: Name::new("new"), span: open };
        let span = ty.span.to(self.prev_span());
        let callee = Callee::Method { recv: ty, name: new, safe: false };
        Expr { kind: ExprKind::Call(Box::new(Call { callee, args, block: None, parens: true })), span }
    }

    fn parse_command_call(&mut self, callee: Callee, start: Span) -> Expr {
        let types = self.takes_type_args(&callee);
        let mut args = vec![self.parse_arg(types, None)];
        while self.eat(T::Comma) {
            self.skip_newlines();
            args.push(self.parse_arg(types, None));
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
        let mut args = vec![self.parse_arg(types, None)];
        while self.at(T::Comma) && self.can_start_expr(self.nth(1)) {
            self.bump();
            args.push(self.parse_arg(types, None));
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
        } else if let Some(pipe) = self.eat(T::Pipe).then(|| self.prev_span()) {
            let what = "`|` to close the block parameters";
            let closed = loop {
                if self.at(T::Pipe) {
                    break true;
                }
                // `{ |x,` followed by a declaration or an `end`.
                if let Some(last) = self.line_cut_before_declaration() {
                    self.unclosed(pipe, last.span, "|", what);
                    self.restore_line_end(last.span);
                    break false;
                }
                let by_ref = self.eat(T::Amp);
                let name = self.parse_name("a block parameter name");
                self.declare(name.name);
                params.push(BlockParam { name, by_ref });
                if !self.eat(T::Comma) {
                    break true;
                }
            };
            if closed {
                self.expect(T::Pipe, what);
            }
        }
        let body;
        if is_brace {
            body = self.parse_body(true);
            // The body's last token, and the line end after it.
            let before = self.tokens[..self.pos].iter().rposition(|t| t.kind != T::Newline).map_or(self.pos, |i| i + 1);
            let last = self.tokens[before.saturating_sub(1)].span;
            self.skip_newlines();
            if !self.eat(T::RBrace) {
                let next = self.peek();
                if next.kind == T::Eof || self.line_of(next.span.start) > self.line_of(last.start) {
                    // The body ended on a later line with something that
                    // isn't its `}` (an `end`, a declaration): the block
                    // was left open after its last line, which ends there.
                    self.pos = before;
                    self.restore_line_end(last);
                    let same_line = self.line_of(last.start) == self.line_of(tok.span.start);
                    let at = last.shrink_to_end();
                    self.report(
                        Diagnostic::error(
                            codes::UNEXPECTED_TOKEN,
                            "expected `}` to close the block, found end of line",
                        )
                        .primary(at, "expected `}`")
                        .secondary(tok.span, "this `{` is never closed")
                        .suggest(
                            "close it",
                            vec![Edit { span: at, replacement: " }".into() }],
                            if same_line { Applicability::MachineApplicable } else { Applicability::MaybeIncorrect },
                        ),
                    );
                } else {
                    self.error_expected("`}` to close the block");
                }
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
                    let glued = self.glued_name().map(|(name, _)| name);
                    let name = match (glued, name_tok.kind) {
                        (Some(name), _) => name,
                        (None, T::Ident | T::Const) => {
                            self.bump();
                            Ident { name: Name::new(self.text_of(name_tok.span)), span: name_tok.span }
                        }
                        (None, T::Kw(k)) => {
                            self.bump();
                            Ident { name: Name::new(k.as_str()), span: name_tok.span }
                        }
                        (None, T::SpliceBegin) => match self.parse_splice_name() {
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
                    } else if is_constant_name(name.as_str()) && self.struct_literal_ahead() {
                        // `geo.Vec2{x: 1}`: a constant never takes a block.
                        let span = start.to(name.span);
                        let ty = Expr { kind: ExprKind::Member { recv: Box::new(expr), name, safe }, span };
                        expr = self.parse_struct_literal(ty);
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
                    let list = List {
                        open: tok.span,
                        close: T::RBracket,
                        closer: "]",
                        what: "`]`",
                        item: "index",
                        items: "indices",
                        kind: Items::Unnamed,
                    };
                    let mut args = Vec::new();
                    let closed = loop {
                        if self.list_left_open(&list) {
                            break false;
                        }
                        self.skip_newlines();
                        if self.at(T::RBracket) {
                            break true;
                        }
                        args.push(self.parse_expr());
                        match self.list_step(&list) {
                            ListStep::Item => {}
                            ListStep::Close => break true,
                            ListStep::Open => break false,
                        }
                    };
                    self.no_do = saved;
                    if closed {
                        self.expect(T::RBracket, list.what);
                    }
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
        let ParamList { params, block, c_variadic: variadic, left_open } =
            if self.at(T::LParen) { self.parse_params(ParamsOf::Proc) } else { ParamList::default() };
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
        let body = if left_open {
            // The line ended with the parameter list (reported), and the
            // body with it.
            Vec::new()
        } else if self.eat(T::LBrace) {
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

/// Drops the uses of a value parameter's name that `[$N]` tried to
/// introduce (see [`Parser::report_array_value_param`]), remembering where
/// each was and whether it was inside a type.
struct DropValueParam {
    name: Name,
    uses: Vec<(Span, bool)>,
    in_type: u32,
}

impl VisitMut for DropValueParam {
    fn visit_expr(&mut self, expr: &mut Expr) {
        if matches!(expr.kind, ExprKind::Const(n) if n == self.name) {
            self.uses.push((expr.span, self.in_type > 0));
            expr.kind = ExprKind::Error;
            return;
        }
        walk_expr(self, expr);
    }

    fn visit_type(&mut self, ty: &mut TypeExpr) {
        if is_plain_name(&ty.kind, self.name) {
            self.uses.push((ty.span, true));
            ty.kind = TypeKind::Error;
            return;
        }
        self.in_type += 1;
        walk_type(self, ty);
        self.in_type -= 1;
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

/// Whether a name is a constant's, which starts with an uppercase letter.
fn is_constant_name(name: &str) -> bool {
    name.starts_with(|c: char| c.is_ascii_uppercase())
}

/// Whether an expression reads as the name of a type that a `?` could make
/// optional: `Int`, `rl.Color`, `Pool(Ball, 64)`, `geo.Pool(Ball, 64)`, one
/// of those in parentheses, or a type parsed in place (`[]Int`).
fn names_type(e: &Expr) -> bool {
    let upper = |name: &Ident| name.as_str().starts_with(|c: char| c.is_ascii_uppercase());
    let package = |recv: &Expr| matches!(recv.kind, ExprKind::Ident(_) | ExprKind::Const(_));
    match &e.kind {
        ExprKind::Const(_) | ExprKind::Type(_) => true,
        ExprKind::Paren(inner) => names_type(inner),
        ExprKind::Member { recv, name, safe: false } => package(recv) && upper(name),
        ExprKind::Call(call) if call.block.is_none() => match &call.callee {
            Callee::Name(name) => upper(name),
            Callee::Method { recv, name, safe: false } => package(recv) && upper(name),
            Callee::Method { .. } | Callee::IVar(_) => false,
        },
        _ => false,
    }
}

/// Whether a line starting with this token can't continue an expression
/// from the line before: a declaration, or the `end` of a block.
fn starts_declaration(kind: TokenKind) -> bool {
    matches!(
        kind,
        T::Kw(
            K::Def
                | K::Macro
                | K::Struct
                | K::Enum
                | K::Union
                | K::Module
                | K::Extend
                | K::Overload
                | K::Import
                | K::Cimport
                | K::Private
                | K::End
        )
    )
}

/// Whether a line among declarations that parsed as `expr` (with no `=`,
/// `,` or modifier after it) is a macro call: a call, a name, or a
/// package member (`lib.make`).
fn is_macro_call(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Call(_) | ExprKind::Ident(_) => true,
        ExprKind::Member { recv, safe: false, .. } => matches!(recv.kind, ExprKind::Ident(_)),
        _ => false,
    }
}

/// For a declaration-level line that declares no name `private` could
/// hide, what it is and why `private` means nothing there.
fn nameless_item(kind: &ItemKind) -> Option<(&'static str, &'static str)> {
    Some(match kind {
        ItemKind::Include(_) => (
            "an `include`",
            "`include` mixes a module's methods into this type; the ones the module declares with `private def` stay private",
        ),
        ItemKind::Import(_) => {
            ("an `import`", "an import is visible only in the file that writes it, never in other packages")
        }
        ItemKind::Cimport(_) => (
            "a `cimport`",
            "with `as:`, the C declarations are visible only in this file; without it, they join this package's own declarations, which other packages see",
        ),
        ItemKind::Extend(_) => (
            "an `extend`",
            "`extend` adds methods to a type declared elsewhere; write `private def` on the ones to hide",
        ),
        ItemKind::ComptimeIf(_) => {
            ("a `comptime if`", "write `private` on the declarations in its branches that should be hidden")
        }
        ItemKind::Splice(_) => ("a splice", "write `private` on the declarations in the code that is spliced in"),
        _ => return None,
    })
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
    fn private_fields() {
        let src = "struct Hero\n  private hp: Int\n  private using base: Base\n  private def heal = 1\nend\n";
        let (file, diags) = parse_file(FileId(0), src);
        let diags: Vec<_> = diags.iter().collect();
        assert_eq!(diags.iter().map(|d| d.code.as_str()).collect::<Vec<_>>(), ["E0105", "E0105"]);
        // The fix removes `private ` and nothing else.
        let edits: Vec<_> = diags[0].helps[0].edits.iter().map(|e| (e.span.start, e.span.end)).collect();
        assert_eq!(edits, [(14, 22)]);
        // The fields are kept, as if written without `private`; the method
        // stays private.
        let ItemKind::Struct(s) = &file.items[0].kind else { panic!() };
        assert!(matches!(&s.body[0].kind, ItemKind::Field(f) if f.name.as_str() == "hp") && !s.body[0].private);
        assert!(matches!(&s.body[1].kind, ItemKind::Field(f) if f.using) && !s.body[1].private);
        assert!(matches!(s.body[2].kind, ItemKind::Def(_)) && s.body[2].private);
        // In a struct inside a `quote` too; elsewhere a field is a checker
        // error already.
        assert_eq!(
            codes_of("macro def m -> Code\n  quote do\n    struct S\n      private x: Int\n    end\n  end\nend\n"),
            ["E0105"]
        );
        assert!(codes_of("module M\n  private hp: Int\nend\n").is_empty());
    }

    /// The messages of the diagnostics for `src`, and `src` with the edits
    /// of every first fix applied.
    fn fix_all(src: &str) -> (Vec<String>, String) {
        apply_fixes(src, Applicability::MachineApplicable)
    }

    /// The messages, and the source with the first fix of each diagnostic
    /// that has the given applicability applied.
    fn apply_fixes(src: &str, applicability: Applicability) -> (Vec<String>, String) {
        let (_, diags) = parse_file(FileId(0), src);
        let mut edits: Vec<_> = diags
            .iter()
            .filter_map(|d| d.helps.first())
            .filter(|h| h.applicability == applicability)
            .flat_map(|h| h.edits.iter())
            .map(|e| (e.span.start as usize, e.span.end as usize, e.replacement.clone()))
            .collect();
        edits.sort_by_key(|e| std::cmp::Reverse(e.0));
        let mut fixed = src.to_string();
        for (start, end, replacement) in edits {
            fixed.replace_range(start..end, &replacement);
        }
        (diags.iter().map(|d| d.message.clone()).collect(), fixed)
    }

    #[test]
    fn private_where_it_does_not_apply() {
        // Before a declaration without a name of its own: one error each,
        // whose fix removes `private `.
        let src = "private import \"core:strings\"\nprivate cimport \"a.h\", as: :a\n\
                   struct Hero\n  private include M\nend\n\
                   private extend Hero\n  def heal -> Int = 1\nend\n\
                   private comptime if true\n  def f -> Int = 1\nend\n";
        let (messages, fixed) = fix_all(src);
        assert_eq!(
            messages,
            [
                "an `import` can't be `private`",
                "a `cimport` can't be `private`",
                "an `include` can't be `private`",
                "an `extend` can't be `private`",
                "a `comptime if` can't be `private`",
            ]
        );
        assert_eq!(fixed, src.replace("private ", ""));
        let (file, _) = parse_file(FileId(0), src);
        assert!(file.items.iter().all(|item| !item.private));
        // A splice standing alone among declarations, in a `quote`.
        let (messages, _) =
            fix_all("macro def m -> Code\n  quote do\n    struct S\n      private #{x}\n    end\n  end\nend\n");
        assert_eq!(messages, ["a splice can't be `private`"]);
        // Declarations and macro calls keep it, without an error.
        assert!(
            codes_of("private def f -> Int = 1\nprivate X = 1\nprivate struct S\nend\nprivate make :x\n").is_empty()
        );

        // An enum member is parsed as one, after the error.
        let src = "enum E\n  private a\n  private b = 2, c\n  private struct\n  private helpers()\nend\n";
        let (messages, fixed) = fix_all(src);
        assert_eq!(messages, ["an enum member can't be `private`"; 3]);
        assert_eq!(fixed, src.replacen("private ", "", 3));
        let (file, _) = parse_file(FileId(0), src);
        let ItemKind::Enum(e) = &file.items[0].kind else { panic!() };
        let names: Vec<_> = e.members.iter().map(|m| m.name.as_str()).collect();
        assert_eq!(names, ["a", "b", "c", "struct"]);
        // `private` before a macro call in an enum body is the call's.
        assert!(matches!(e.body[0].kind, ItemKind::MacroCall(_)) && e.body[0].private);

        // A statement is parsed as if `private` weren't there, so `x` is
        // declared and nothing cascades.
        let src = "def main\n  private x = 1\n  p x\nend\n";
        let (messages, fixed) = fix_all(src);
        assert_eq!(messages, ["`private` applies only to declarations"]);
        assert_eq!(fixed, "def main\n  x = 1\n  p x\nend\n");
        let (messages, fixed) = fix_all("macro def m -> Code\n  quote do\n    private hp: Int\n  end\nend\n");
        assert_eq!(messages, ["`private` applies only to declarations"]);
        assert_eq!(fixed, "macro def m -> Code\n  quote do\n    hp: Int\n  end\nend\n");
        // A declaration in a method keeps it; nesting is the checker's error.
        assert!(codes_of("def main\n  private def f -> Int = 1\nend\n").is_empty());
    }

    #[test]
    fn private_macro_calls_in_a_quote() {
        // `private` before a call or a name in a `quote` is a private
        // declaration-level macro call, as at package level; before an
        // assignment it is still reported, once.
        let src = "macro def m -> Code\n  quote do\n    private helpers :foo\n    private lib.make\n    \
                   private setup\n    private x = 1\n    private y, z = 1, 2\n    helpers :bar\n  end\nend\n";
        let (file, diags) = parse_file(FileId(0), src);
        let messages: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
        assert_eq!(messages, ["`private` applies only to declarations"; 2]);
        let ItemKind::Def(def) = &file.items[0].kind else { panic!() };
        let FnBody::Block(body) = &def.body else { panic!() };
        let Some(Stmt { kind: StmtKind::Expr(Expr { kind: ExprKind::Quote(quote), .. }), .. }) = body.first() else {
            panic!()
        };
        let private_calls: Vec<bool> = quote
            .body
            .iter()
            .map(|s| matches!(&s.kind, StmtKind::Item(i) if i.private && matches!(i.kind, ItemKind::MacroCall(_))))
            .collect();
        assert_eq!(private_calls, [true, true, true, false, false, false]);
    }

    #[test]
    fn unclosed_paren_ends_at_the_line_end() {
        // The `)` goes at the end of the line, and `main` is still parsed.
        for value in ["(1 + 2", "([]"] {
            let src = format!("X = {value}\ndef main\n  p X\nend\n");
            let (messages, fixed) = fix_all(&src);
            assert_eq!(messages, ["expected `)`, found end of line"], "{value}");
            assert_eq!(fixed, src.replacen('\n', ")\n", 1));
            assert_eq!(parse_file(FileId(0), &src).0.items.len(), 2);
        }
        let src = "def main\n  x = (1 + 2\n  p x\nend\n";
        let (messages, fixed) = fix_all(src);
        assert_eq!(messages, ["expected `)`, found end of line"]);
        assert_eq!(fixed, "def main\n  x = (1 + 2)\n  p x\nend\n");
        // At the end of the file.
        assert_eq!(fix_all("X = (1 + 2").1, "X = (1 + 2)");
        // A `(` with nothing after it, before a declaration or an `end`.
        for src in ["X = (\ndef main\nend\n", "def main\n  x = (\nend\n"] {
            let (file, diags) = parse_file(FileId(0), src);
            let messages: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
            assert_eq!(messages, ["expected an expression, found end of line"], "{src:?}");
            assert!(matches!(file.items.last().map(|i| &i.kind), Some(ItemKind::Def(_))));
        }
        // Newlines inside parentheses still work where the expression goes
        // on, and before the `)`.
        parse_ok(
            "def main\n  a = (1 +\n    2)\n  b = (\n    3 * 4\n  )\n  c = (a > 1 ?\n    5 : 6)\n\
             \x20 d = ([1, 2]\n    .size)\n  e = foo(a,\n    b)\n  f = (xs.map do |x|\n    x\n  end)\n\
             \x20 g = (if a > 1\n    1\n  else\n    2\n  end)\n  h = ((a +\n    b))\nend\n",
        );
        // A token that doesn't continue it on the same line is reported there.
        let (messages, _) = fix_all("def main\n  x = (1 + 2 3)\nend\n");
        assert_eq!(messages, ["expected `)`, found `3`"]);
    }

    #[test]
    fn optional_type_written_in_place() {
        // The value of each `x = …` in `main`, by shape.
        let shapes = |body: &str| -> Vec<String> {
            let file = parse_ok(&format!("def main\n{body}end\n"));
            let ItemKind::Def(f) = &file.items[0].kind else { panic!() };
            let FnBody::Block(body) = &f.body else { panic!() };
            body.iter()
                .filter_map(|s| match &s.kind {
                    StmtKind::Assign { values, .. } => Some(match &values[0].kind {
                        ExprKind::Type(t) if matches!(t.kind, TypeKind::Optional(_)) => "optional".to_string(),
                        ExprKind::Ternary { .. } => "ternary".to_string(),
                        ExprKind::Binary { rhs, .. } if matches!(rhs.kind, ExprKind::Type(_)) => {
                            "binary with type".to_string()
                        }
                        other => format!("{other:?}"),
                    }),
                    _ => None,
                })
                .collect()
        };
        // A `?` right after a type's name ends the type when no
        // conditional's `:` follows.
        assert_eq!(
            shapes(
                "  a = Int?\n  b = rl.Color?\n  c = Pool(Ball, 64)?\n  d = geo.Pool(Ball, 64)?\n\
                 \x20 e = (Int)?\n  f = (proc(Int) -> Int)?\n  g = Int??\n  h = 1 + Int?\n  i = Int?\n  hp: Int\n"
            ),
            [&["optional"; 7][..], &["binary with type", "optional"]].concat()
        );
        // Before a modifier too.
        parse_ok("def main\n  t = Int? if ready\nend\n");
        // Conditionals parse as before: with spaces, after a predicate
        // name, or with the `:` after it (on the line, or on the next one).
        assert_eq!(
            shapes(
                "  a = c ? 1 : 2\n  b = xs.empty? ? 1 : 2\n  c = empty? ? 1 : 2\n  d = N? 1 : 2\n\
                 \x20 e = (a > b)? 1 : 2\n  f = N?\n    1 : 2\n  g = (Int)? 1 : 2\n"
            ),
            ["ternary"; 7]
        );
        // A predicate call stays one, and a type in a call argument too.
        let file = parse_ok("def main\n  p foo?, xs.empty?\n  p f(Int?), size_of(Int?)\nend\n");
        let ItemKind::Def(f) = &file.items[0].kind else { panic!() };
        let FnBody::Block(body) = &f.body else { panic!() };
        let StmtKind::Expr(e) = &body[1].kind else { panic!() };
        let ExprKind::Call(call) = &e.kind else { panic!() };
        assert!(call.args.iter().all(|a| matches!(&a.value.kind, ExprKind::Call(c) if matches!(
            c.args[0].value.kind, ExprKind::Type(_)
        ))));
        // `t = Int?` is one expression: nothing runs past the line end.
        assert!(codes_of("def main\n  t = Int?\nend\n").is_empty());
    }

    #[test]
    fn unclosed_lists_end_at_the_line_end() {
        // One error at the end of the first line, with a fix that closes
        // the list, and `main` is still parsed.
        for (value, message, closer) in [
            ("max(1, 2", "expected `)` to close the argument list, found end of line", ")"),
            ("max(1,", "expected `)` to close the argument list, found end of line", ")"),
            ("max(", "expected `)` to close the argument list, found end of line", ")"),
            ("[1, 2", "expected `]` to close the array, found end of line", "]"),
            ("[", "expected `]` to close the array, found end of line", "]"),
            ("xs[1", "expected `]`, found end of line", "]"),
            ("{", "expected `}`, found end of line", "}"),
            ("xs.map { |x| x * 2", "expected `}` to close the block, found end of line", " }"),
            ("xs.map { |x|", "expected `}` to close the block, found end of line", " }"),
            ("xs.map {", "expected `}` to close the block, found end of line", " }"),
            ("xs.map { |x,", "expected `|` to close the block parameters, found end of line", "|"),
        ] {
            for next in ["def main\n  p X\nend\n", "struct S\nend\n", "private def f = 1\n"] {
                let src = format!("X = {value}\n{next}");
                let (messages, fixed) = fix_all(&src);
                assert_eq!(messages, [message], "{src:?}");
                assert_eq!(fixed, src.replacen('\n', &format!("{closer}\n"), 1), "{src:?}");
                assert_eq!(parse_file(FileId(0), &src).0.items.len(), 2, "{src:?}");
            }
        }
        // A line that ends with an operator, `=`, `,` or a named
        // argument's `:`.
        for value in ["1 +", "1 &&", "-", "", "add(a:", "add(1, b:"] {
            for next in ["def main\nend\n", "enum E\n  a\nend\n"] {
                let src = format!("X = {value}\n{next}");
                let (file, diags) = parse_file(FileId(0), &src);
                let messages: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
                assert_eq!(messages, ["expected an expression, found end of line"], "{src:?}");
                assert_eq!(file.items.len(), 2, "{src:?}");
            }
        }
        // In a method, before a statement or its `end`.
        for (line, fixed) in [
            ("x = foo(1, 2", "x = foo(1, 2)"),
            ("x = [1, 2", "x = [1, 2]"),
            ("x = xs[1", "x = xs[1]"),
            ("x = foo(a, [1, 2", "x = foo(a, [1, 2]"),
        ] {
            let src = format!("def main\n  {line}\n  p x\nend\n");
            assert_eq!(fix_all(&src).1, format!("def main\n  {fixed}\n  p x\nend\n"), "{src:?}");
        }
        let (messages, fixed) = fix_all("def main\n  xs.each { |x| p x\nend\n");
        assert_eq!(messages, ["expected `}` to close the block, found end of line"]);
        assert_eq!(fixed, "def main\n  xs.each { |x| p x }\nend\n");
        for line in ["x = 1 +", "p 1,", "x ="] {
            assert_eq!(codes_of(&format!("def main\n  {line}\nend\n")), ["E0105"], "{line}");
        }
        // A line indented under the list is its next item, with its `,`
        // missing (a fix to review).
        for src in ["X = foo(\n  1\n  2\n)\n", "X = [\n  1,\n  2\n  3,\n]\n"] {
            let (file, diags) = parse_file(FileId(0), src);
            let diags: Vec<_> = diags.iter().collect();
            assert_eq!(diags.len(), 1, "{src:?}");
            assert!(diags[0].message.starts_with("expected `,` or"), "{src:?}");
            assert_eq!(diags[0].helps[0].applicability, Applicability::MaybeIncorrect);
            assert_eq!(file.items.len(), 1, "{src:?}");
        }
        // Lists, blocks and operators still go on over lines.
        parse_ok(
            "def main\n  a = foo(1,\n    2)\n  b = foo(\n    1,\n    2\n  )\n  c = [\n    1, 2,\n    3,\n  ]\n\
             \x20 d = [[1, 2],\n    [3, 4]]\n  e = foo(xs.map do |x|\n    x\n  end)\n  f = xs[\n    1\n  ]\n\
             \x20 g = 1 +\n    2 *\n    3\n  h = foo(1, 2)\n    .bar\n  xs.each { |x|\n    p x\n  }\n\
             \x20 i = xs.map { |x,\n    y| x }\n  j = {}\n  k = foo(a:\n    1)\n  l = foo(\n    a:\n    1)\n\
             \x20 m = ->(a: Int,\n    b: Int) { a }\n  p a, b, c, d, e, f, g, h, i, j, k, l, m\nend\n",
        );
        // Parameter lists: one error at the end of the line, and the
        // method ends there when a declaration follows; otherwise the
        // lines after it are its body.
        for (head, closed) in [
            ("def foo(a: Int,", "def foo(a: Int,)"),
            ("def foo(a: Int, b: Int", "def foo(a: Int, b: Int)"),
            ("def foo(", "def foo()"),
            ("macro def foo(a: Code,", "macro def foo(a: Code,)"),
        ] {
            for (rest, items) in [("def main\nend\n", 2), ("  p 1\nend\ndef main\nend\n", 2), ("end\n", 1)] {
                let src = format!("{head}\n{rest}");
                let (messages, fixed) = fix_all(&src);
                assert_eq!(messages.len(), 1, "{src:?}: {messages:?}");
                assert!(messages[0].ends_with("found end of line"), "{src:?}");
                assert_eq!(fixed, format!("{closed}\n{rest}"), "{src:?}");
                assert_eq!(parse_file(FileId(0), &src).0.items.len(), items, "{src:?}");
            }
        }
        assert_eq!(codes_of("def main\n  f = ->(a: Int,\n  p 1\nend\n"), ["E0105"]);
        // In a parameter's type, the one error closes the type's list.
        let (messages, fixed) = fix_all("def foo(a: Pool(Int,\ndef main\nend\n");
        assert_eq!(messages, ["expected `)`, found end of line"]);
        assert_eq!(fixed, "def foo(a: Pool(Int,)\ndef main\nend\n");
        // A parameter indented under the list is the next one, with its
        // `,` missing; a parameter without its type is still that error.
        let (messages, _) = fix_all("def foo(a: Int\n        b: Int) = a\n");
        assert_eq!(messages, ["expected `,` or `)`, found end of line"]);
        let (messages, _) = fix_all("def foo(a: Int,\n        b) = a\n");
        assert_eq!(messages, ["parameter `b` needs a type"]);
        // Types: a generic type's arguments, a tuple, a proc type, an
        // array's length and a map's key, in a struct whose next field is
        // still declared.
        for (ty, closer) in [
            ("Pool(Int, 4", ")"),
            ("Pool(Int,", ")"),
            ("(Int, Int", ")"),
            ("proc(Int, Int", ")"),
            ("[4", "]"),
            ("[^", "]"),
            ("map[String", "]"),
            ("matrix[2, 2", "]"),
        ] {
            let src = format!("struct S\n  pool: {ty}\n  hp: Int\nend\n");
            let (file, diags) = parse_file(FileId(0), &src);
            let messages: Vec<_> = diags.iter().map(|d| d.message.as_str()).collect();
            assert_eq!(messages, [format!("expected `{closer}`, found end of line")], "{src:?}");
            let ItemKind::Struct(s) = &file.items[0].kind else { panic!() };
            assert_eq!(s.body.len(), 2, "{src:?}");
            assert_eq!(fix_all(&src).1, src.replacen(ty, &format!("{ty}{closer}"), 1));
        }
        parse_ok(
            "struct S\n  a: Pool(Int,\n    4)\n  b: proc(Int,\n    Int) -> Int\n  c: (Int,\n    Int)\n  \
             d: Pool(\n    Int,\n    4\n  )\nend\n",
        );
        // A named argument's value on a line indented no deeper than the
        // call's is missing.
        for src in ["def main\n  x = add(a:\n  p x\nend\n", "def main\n  p a:\n  p 1\nend\n"] {
            let (messages, _) = fix_all(src);
            assert_eq!(messages, ["expected an expression, found end of line"], "{src:?}");
        }
        // Recovery after an error stops at a declaration (or an `end`) on
        // a later line, where a bracket was left open.
        for src in ["X = Foo{\ndef main\nend\n", "X = 1 2 +\ndef main\nend\n", "X = foo(1 2\ndef main\nend\n"] {
            let (file, diags) = parse_file(FileId(0), src);
            assert_eq!(diags.iter().count(), 1, "{src:?}");
            assert!(matches!(file.items.last().map(|i| &i.kind), Some(ItemKind::Def(_))), "{src:?}");
        }
        // It skips a `do … end` block on the way.
        let src = "def main\n  y = 1 2 + f(xs.map do |x|\n    x\n  end)\nend\ndef other\nend\n";
        let (file, diags) = parse_file(FileId(0), src);
        assert_eq!(diags.iter().count(), 1);
        assert_eq!(file.items.len(), 2);
    }

    #[test]
    fn keywords_are_quoted_in_messages() {
        let (messages, _) = fix_all("def main\n  x = def\nend\n");
        assert_eq!(messages, ["expected an expression, found `def`"]);
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
    fn names_glued_to_splices() {
        // One error at the glued name, and the rest of the file parses.
        let src = "macro def m(name: Symbol) -> Code\n  quote do\n    def bump_#{name} -> Int = 1\n  end\nend\n\
                   def main\nend\n";
        let (file, diags) = parse_file(FileId(0), src);
        let diags: Vec<_> = diags.iter().collect();
        assert_eq!(diags.len(), 1);
        assert_eq!((diags[0].code.as_str(), diags[0].message.as_str()), ("E0111", "a splice can't be part of a name"));
        assert_eq!(file.items.len(), 2);
        let q = first_quote(&file);
        let StmtKind::Item(item) = &q.body[0].kind else { panic!() };
        let ItemKind::Def(def) = &item.kind else { panic!() };
        assert_eq!(splice_of(&def.name), 0);
        // The fix builds the name before the `quote` and splices it.
        let (_, fixed) = apply_fixes(src, Applicability::MaybeIncorrect);
        assert_eq!(
            fixed,
            "macro def m(name: Symbol) -> Code\n  bump_name = \"bump_#{name}\".to_sym\n  quote do\n    \
             def #{bump_name} -> Int = 1\n  end\nend\ndef main\nend\n"
        );
        parse_ok(&fixed);
        // In every name position, with the text before or after the splice
        // (or both), one error each.
        let body = "def get_#{a}(x_#{a}: Int) -> Int = @#{a}_x + x.#{a}_y\n\
                    struct Box#{a}\n  v_#{a}: Int\nend\n\
                    enum E#{a}\n  m_#{a}\n  b\nend\n\
                    MAX_#{a} = 3\n\
                    def go\n  \
                      y_#{a}: Int = 1\n  z_#{a} = :#{a}_s + :s_#{a}\n  call_#{a}(1, k_#{a}: 2)\n  \
                      for i_#{a} in xs\n  end\n  xs.each { |e_#{a}| }\n  v: T#{a} = 1\n  \
                      #{a}_#{a}_#{a} = 2\n  p_#{a} 1, 2\n  w = #{a}2\n\
                    end\n\
                    overload :o_#{a}, :#{a}\n";
        let src = format!("macro def m(a: Symbol) -> Code\n  quote do\n{body}  end\nend\n");
        let (file, diags) = parse_file(FileId(0), &src);
        let lines: Vec<_> = diags
            .iter()
            .map(|d| {
                assert_eq!(d.message, "a splice can't be part of a name");
                src[..d.primary_span().expect("a span").start as usize].matches('\n').count() + 1
            })
            .collect();
        assert_eq!(lines, [3, 3, 3, 3, 4, 5, 7, 8, 11, 13, 14, 14, 14, 15, 15, 16, 18, 19, 20, 21, 22, 24]);
        assert_eq!(first_quote(&file).body.len(), 6);
        // The same name glued twice is built once.
        let src = "macro def m(a: Symbol) -> Code\n  quote do\n    def get_#{a} = 1\n    def two = get_#{a} + 1\n  \
                   end\nend\n";
        let (messages, fixed) = apply_fixes(src, Applicability::MaybeIncorrect);
        assert_eq!(messages.len(), 2);
        assert_eq!(fixed.matches("get_a = \"get_#{a}\".to_sym").count(), 1);
        parse_ok(&fixed);
        // In a `quote` inside a splice, no line can go before it: the help
        // says what to write.
        let (_, diags) = parse_file(
            FileId(0),
            "macro def m(a: Symbol) -> Code\n  quote do\n    #{quote do\n      x_#{a} = 1\n    end}\n  end\nend\n",
        );
        let diags: Vec<_> = diags.iter().collect();
        assert_eq!(diags.len(), 1);
        assert!(diags[0].helps[0].edits.is_empty());
        assert!(diags[0].helps[0].message.ends_with("`x_a = \"x_#{a}\".to_sym`, then `#{x_a}`"));
        // Spaced, a splice is a separate argument.
        parse_ok("macro def m(a: Code) -> Code\n  quote do\n    p #{a}\n    x = [#{a}, a]\n  end\nend\n");
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

    #[test]
    fn names_glued_to_splices_outside_a_quote() {
        // One E0111, never a comment, and the line is still the `def`.
        let (file, diags) = parse_file(FileId(0), "def bump_#{name} = 1\ndef main\nend\n");
        assert_eq!(diags.iter().map(|d| d.code.as_str()).collect::<Vec<_>>(), ["E0111"]);
        assert!(diags.iter().flat_map(|d| &d.helps).all(|h| h.edits.is_empty() && !h.message.contains("comment")));
        assert_eq!(file.items.len(), 2);
        let ItemKind::Def(def) = &file.items[0].kind else { panic!("a def") };
        assert_eq!(def.name.as_str(), "bump_#{name}");
        assert!(matches!(def.body, FnBody::Expr(_)));
        // A splice spaced from the text before it is still a comment.
        let (messages, fixed) = fix_all("def main\n  x = 1 #{note}\n  #{TODO} later\n  p x\nend\n");
        assert_eq!(messages.len(), 2);
        assert_eq!(fixed, "def main\n  x = 1 # {note}\n  # {TODO} later\n  p x\nend\n");
    }

    #[test]
    fn struct_literal_syntax_is_a_new_call() {
        // One E0113 each, whose fix writes the `new` call the braces are
        // read as.
        let src = "W = Foo{a: 1}\ndef main\n  w = geo.Vec2{1, 2}\n  o = Foo{a = 1, b = 2}\n  \
                   g = Pool(Int, 4){}\n  m = Foo{\n    a: 1,\n  }.a\nend\n";
        let (messages, fixed) = fix_all(src);
        assert_eq!(messages, ["Wid has no struct literal syntax"; 5]);
        assert_eq!(
            fixed,
            "W = Foo.new(a: 1)\ndef main\n  w = geo.Vec2.new(1, 2)\n  o = Foo.new(a: 1, b: 2)\n  \
             g = Pool(Int, 4).new()\n  m = Foo.new(\n    a: 1,\n  ).a\nend\n"
        );
        parse_ok(&fixed);
        // `Foo{a: 1}` reads as `Foo.new(a: 1)`.
        let (file, _) = parse_file(FileId(0), src);
        let ItemKind::Const(c) = &file.items[0].kind else { panic!("a constant") };
        let ExprKind::Call(call) = &c.value.kind else { panic!("a call") };
        let Callee::Method { recv, name, .. } = &call.callee else { panic!("a method call") };
        assert!(matches!(recv.kind, ExprKind::Const(_)) && name.as_str() == "new" && call.parens);
        assert!(matches!(call.args.as_slice(), [Arg { name: Some(a), .. }] if a.as_str() == "a"));
        // Left open before a declaration: the one error's fix also closes it.
        let (messages, fixed) = fix_all("X = Foo{a: 1\ndef main\nend\n");
        assert_eq!(messages, ["Wid has no struct literal syntax"]);
        assert_eq!(fixed, "X = Foo.new(a: 1)\ndef main\nend\n");
        // A block after a call stays one.
        assert!(codes_of("def main\n  xs.each{ |x| p x }\n  Foo.bar{ p 1 }\nend\n").is_empty());
    }
}

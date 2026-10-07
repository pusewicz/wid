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

struct Opener {
    keyword: &'static str,
    span: Span,
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

    fn skip_newlines(&mut self) {
        while self.at(T::Newline) {
            self.bump();
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

    /// Skips to the end of the current line after an error.
    fn recover_line(&mut self) {
        let mut depth = 0i32;
        loop {
            match self.kind() {
                T::Eof => return,
                T::Newline if depth <= 0 => return,
                T::LParen | T::LBracket | T::LBrace => depth += 1,
                T::RParen | T::RBracket | T::RBrace => depth -= 1,
                _ => {}
            }
            self.bump();
        }
    }

    /// Parses a constant's value as a type when the whole value is one that
    /// only reads as a type, like `RawPtr?`, `^Node?` or `C.int`. Restores the
    /// position and returns `None` otherwise.
    fn try_type_alias(&mut self) -> Option<TypeExpr> {
        if !matches!(self.peek().kind, T::Const | T::Ident | T::Caret | T::LBracket) {
            return None;
        }
        let save = self.pos;
        let diags = self.diags.len();
        let texpr = self.parse_type();
        let only_type = match &texpr.kind {
            TypeKind::Optional(_) | TypeKind::Pointer(_) | TypeKind::MultiPointer(_) => true,
            TypeKind::Path { segments, args } => {
                args.is_empty()
                    && segments.len() == 2
                    && segments[1].as_str().starts_with(|c: char| c.is_ascii_lowercase())
            }
            _ => false,
        };
        if only_type && self.diags.len() == diags && self.at_stmt_end() {
            return Some(texpr);
        }
        self.pos = save;
        self.diags.truncate(diags);
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
                | T::Kw(K::End)
                | T::Kw(K::Else)
                | T::Kw(K::Elsif)
                | T::Kw(K::When)
                | T::Kw(K::Then)
        )
    }

    fn expect_stmt_end(&mut self) {
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
            if self.at(T::Eof) || self.at_kw(K::End) || self.at_kw(K::Else) || self.at_kw(K::Elsif) {
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
                let name = self.expect_ident("a field name");
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
                let type_start = self.at(T::AtBracket)
                    || (self.at(T::Ident) && matches!(self.text_of(self.peek().span), "distinct" | "proc"));
                let value = if type_start {
                    let texpr = self.parse_type();
                    Expr { span: texpr.span, kind: ExprKind::Type(Box::new(texpr)) }
                } else if let Some(texpr) = self.try_type_alias() {
                    Expr { span: texpr.span, kind: ExprKind::Type(Box::new(texpr)) }
                } else {
                    self.parse_expr()
                };
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
            T::Ident => {
                let save = self.pos;
                let expr = self.parse_expr_cmd();
                let is_statement =
                    self.at(T::Eq) || is_assign_op(self.kind()) || self.at(T::Comma) || self.at_modifier();
                match &expr.kind {
                    ExprKind::Call(_) | ExprKind::Ident(_) if !is_statement => ItemKind::MacroCall(Box::new(expr)),
                    _ => {
                        self.pos = save;
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
                ) || tok.kind == T::IVar
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
        let (params, block, c_variadic) =
            if self.at(T::LParen) { self.parse_params() } else { (Vec::new(), None, None) };
        let ret = if self.eat(T::Arrow) { Some(self.parse_type()) } else { None };
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
        let yields = std::mem::replace(&mut self.yield_seen, outer_yield);
        FnDecl { name, is_static, is_macro, params, block, ret, body, sig_span, yields, c_variadic }
    }

    fn parse_params(&mut self) -> (Vec<Param>, Option<BlockParamDecl>, Option<Span>) {
        self.bump();
        let mut params = Vec::new();
        let mut block = None;
        let mut variadic = None;
        loop {
            self.skip_newlines();
            if self.at(T::RParen) || self.at(T::Eof) {
                break;
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
                let name = self.expect_ident("a block parameter name");
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
                let splat = self.eat(T::Star);
                let name = self.expect_ident("a parameter name");
                self.declare(name.name);
                let ty = if self.eat(T::Colon) {
                    self.parse_type()
                } else {
                    self.report(
                        Diagnostic::error(
                            codes::UNEXPECTED_TOKEN,
                            format!("parameter `{}` needs a type", name.as_str()),
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
        (params, block, variadic)
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
        let name = self.expect_const("a struct name");
        let generics = self.parse_generic_params();
        self.openers.push(Opener { keyword: "struct", span: kw.span.to(name.span) });
        let body = self.parse_item_body(ItemCtx::Struct);
        self.expect_end();
        ItemKind::Struct(Box::new(StructDecl { name, generics, body }))
    }

    fn parse_enum(&mut self) -> ItemKind {
        let kw = self.bump();
        let name = self.expect_const("an enum name");
        let backing = if self.eat(T::Colon) { Some(self.parse_type()) } else { None };
        self.openers.push(Opener { keyword: "enum", span: kw.span.to(name.span) });
        let mut members = Vec::new();
        let mut body = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::Eof) || self.at_kw(K::End) {
                break;
            }
            let member_start = self.at(T::Ident) || is_keyword_member(self.kind());
            if member_start && matches!(self.nth(1).kind, T::Newline | T::Eq | T::Comma | T::Kw(K::End)) {
                loop {
                    let member = self.expect_enum_member();
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

    /// An enum member's name: an identifier, or `struct`, `enum` or `union`
    /// standing alone, so an enum can name the kinds of types
    /// (`TypeKind.struct`).
    fn expect_enum_member(&mut self) -> Ident {
        let tok = self.peek();
        if is_keyword_member(tok.kind) && matches!(self.nth(1).kind, T::Newline | T::Eq | T::Comma | T::Kw(K::End)) {
            self.bump();
            return Ident { name: Name::new(self.text_of(tok.span)), span: tok.span };
        }
        self.expect_ident("an enum member")
    }

    fn parse_union(&mut self) -> ItemKind {
        self.bump();
        let name = self.expect_const("a union name");
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
        let name = self.expect_const("a module name");
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
            if content.is_empty() || content.starts_with('#') {
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
        let ty = self.parse_type();
        if self.diags.len() > saved_diags || matches!(ty.kind, TypeKind::Error) {
            self.pos = saved_pos;
            self.diags.truncate(saved_diags);
            self.last_error_at = saved_last;
            return None;
        }
        Some(ty)
    }

    // ----- statements ----------------------------------------------------

    fn parse_block_body(&mut self) -> Vec<Stmt> {
        let mut stmts = Vec::new();
        loop {
            self.skip_newlines();
            if matches!(
                self.kind(),
                T::Eof | T::RBrace | T::Kw(K::End) | T::Kw(K::Else) | T::Kw(K::Elsif) | T::Kw(K::When)
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
        stmts
    }

    fn is_decl_start(&self) -> bool {
        let mut i = 0;
        loop {
            if self.nth(i).kind != T::Ident {
                return false;
            }
            match self.nth(i + 1).kind {
                T::Colon => return true,
                T::Comma => i += 2,
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
            T::Ident if self.is_decl_start() => self.parse_decl(),
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
        let mut names = vec![self.expect_ident("a variable name")];
        while self.eat(T::Comma) {
            names.push(self.expect_ident("a variable name"));
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
        if self.at(T::Ident) {
            let mut i = 0;
            while self.nth(i).kind == T::Ident {
                if self.nth(i + 1).kind == T::Comma {
                    i += 2;
                    continue;
                }
                is_bind = self.nth(i + 1).kind == T::Eq;
                break;
            }
        }
        if is_bind {
            names.push(self.expect_ident("a name"));
            while self.eat(T::Comma) {
                names.push(self.expect_ident("a name"));
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
            let e = self.expect_ident("the error binding name");
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
                let body = self.parse_block_body();
                self.expect_end();
                Expr { kind: ExprKind::Quote(body), span: span.to(self.prev_span()) }
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

    fn parse_args_in_parens(&mut self) -> Vec<Arg> {
        self.bump();
        let saved = self.no_do;
        self.no_do = false;
        let mut args = Vec::new();
        loop {
            self.skip_newlines();
            if self.at(T::RParen) || self.at(T::Eof) {
                break;
            }
            args.push(self.parse_arg());
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

    fn parse_arg(&mut self) -> Arg {
        if self.at(T::Ident) && self.nth(1).kind == T::Colon {
            let tok = self.bump();
            self.bump();
            self.skip_newlines();
            let name = Ident { name: Name::new(self.text_of(tok.span)), span: tok.span };
            let value = self.parse_expr();
            return Arg { name: Some(name), value, splat: false };
        }
        if self.at(T::Star) {
            self.bump();
            return Arg { name: None, value: self.parse_expr(), splat: true };
        }
        Arg { name: None, value: self.parse_expr(), splat: false }
    }

    fn parse_call_with_parens(&mut self, callee: Callee, start: Span) -> Expr {
        let args = self.parse_args_in_parens();
        let block = self.parse_block_arg();
        Expr {
            kind: ExprKind::Call(Box::new(Call { callee, args, block, parens: true })),
            span: start.to(self.prev_span()),
        }
    }

    fn parse_command_call(&mut self, callee: Callee, start: Span) -> Expr {
        let mut args = vec![self.parse_arg()];
        while self.eat(T::Comma) {
            self.skip_newlines();
            args.push(self.parse_arg());
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
        let mut args = vec![self.parse_arg()];
        while self.at(T::Comma) && self.can_start_expr(self.nth(1)) {
            self.bump();
            args.push(self.parse_arg());
        }
        let last = self.prev_span();
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
                Applicability::MachineApplicable,
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
                let name = self.expect_ident("a block parameter name");
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
                    let name_text = match name_tok.kind {
                        T::Ident | T::Const => self.text_of(name_tok.span).to_string(),
                        T::Kw(k) => k.as_str().to_string(),
                        _ => {
                            self.error_expected("a method or field name after `.`");
                            return Expr { kind: ExprKind::Error, span: expr.span.to(name_tok.span) };
                        }
                    };
                    self.bump();
                    let name = Ident { name: Name::new(&name_text), span: name_tok.span };
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
        if self.at(T::Ident) && self.nth(1).kind == T::Eq {
            let name = self.expect_ident("a name");
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
            let name = self.expect_ident("a loop variable");
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
        let (params, block, variadic) = if self.at(T::LParen) { self.parse_params() } else { (Vec::new(), None, None) };
        if let Some(dots) = variadic {
            self.report(
                Diagnostic::error(codes::UNEXPECTED_TOKEN, "procs cannot take C variadic arguments")
                    .primary(dots, "only `@[extern]` methods take `...`")
                    .help("collect extra arguments into a slice with a `*rest: T` parameter"),
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

fn is_assignable(expr: &Expr) -> bool {
    match &expr.kind {
        ExprKind::Ident(_) | ExprKind::IVar(_) | ExprKind::Index { .. } | ExprKind::Deref(_) => true,
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

    #[test]
    fn missing_end_reports_opener() {
        let (_, diags) = parse_file(FileId(0), "def main\n  if x\n    puts 1\n  \nend\n");
        assert!(diags.iter().any(|d| d.code == codes::MISSING_END));
    }
}

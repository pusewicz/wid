//! The canonical formatter behind `wid fmt` (SPEC "Toolchain and CLI").
//!
//! [`format`] reprints a file that parsed without errors. It works on the
//! token stream, not on a printed tree: every token keeps its exact text
//! (strings, interpolations, numbers and splices are never touched), every
//! comment is kept, and only the whitespace between tokens changes, plus a
//! trailing `,` added to or removed from a multi-line list. The syntax tree
//! tells it what each token is (a binary or a unary `-`, a block's `{`, a
//! ternary's `:`) and where blocks open and close, so the result parses to
//! the same tree: [`same_tree`] checks that, and `wid fmt` refuses to write
//! a file when it doesn't hold.
//!
//! The style, which has no options:
//! - Indentation is two spaces per block. The author's line breaks are kept;
//!   a line that continues a statement is indented one more step, and the
//!   lines of a bracketed list one step past the line that opens it. In a
//!   block's header, a continued line and a list that closes on its last
//!   item's line go two steps, so they stand apart from the body.
//! - One space around binary operators, `=`, `->` and a ternary's `?` and
//!   `:`, after `,` and a name's `:`, and inside a `{ }` block; none inside
//!   `( )` and `[ ]`, around `.` and ranges, or after a unary operator.
//!   Where the space decides how the line parses (`foo -1`, `foo [1]`,
//!   `foo (x)`), it stays as written.
//! - A list whose closer sits on its own line ends with `,`; one that closes
//!   on its last item's line doesn't.
//! - At most one blank line in a row, none at the start or end of a block or
//!   the file, and one around every declaration that spans several lines.
//! - Comments start with `# `. Trailing comments on consecutive lines of the
//!   same indentation line up one space after the longest code; an own-line
//!   comment takes the indentation of the line after it.

use std::collections::HashMap;

use wid_diagnostics::{FileId, Span};

use crate::ast::*;
use crate::lexer::lex;
use crate::token::{Keyword as K, Token, TokenKind as T};
use crate::visit::{Visit, VisitMut, shared};

/// Formats `source`, whose parse is `file`, canonically. The file must have
/// parsed without errors; see the module documentation.
pub fn format(source: &str, file: &File) -> String {
    let lexed = lex(file.file, source);
    let toks = Tokens::new(source, lexed.tokens);
    let mut facts = Collector::new(&toks);
    facts.visit_items(&file.items);
    Layout::new(source, &toks, &file.comments, facts).render()
}

/// Whether two parses have the same syntax tree, spans aside, and the same
/// comments in the same order. Formatting must keep both.
pub fn same_tree(a: &File, b: &File) -> bool {
    tree_key(a) == tree_key(b) && comment_key(a) == comment_key(b)
}

/// The comments of a file as the formatter must keep them: their text and
/// whether each stands on a line of its own.
fn comment_key(file: &File) -> Vec<(&str, bool)> {
    file.comments.iter().map(|c| (c.text.as_str(), c.own_line)).collect()
}

fn tree_key(file: &File) -> String {
    struct Erase;
    impl VisitMut for Erase {
        fn visit_span(&mut self, span: &mut Span) {
            *span = Span::new(FileId(0), 0, 0);
        }
    }
    let mut items = file.items.clone();
    Erase.visit_items(&mut items);
    format!("{items:?}")
}

// ----- tokens -------------------------------------------------------------

/// The lexed tokens with lookups by offset and the bracket that matches
/// each bracket.
struct Tokens<'s> {
    src: &'s str,
    toks: Vec<Token>,
    /// For each opening bracket (`(`, `[`, `@[`, `{`, a string's opening
    /// quote, `#{`), the index of its closer, and the other way round.
    partner: Vec<Option<usize>>,
}

impl<'s> Tokens<'s> {
    fn new(src: &'s str, toks: Vec<Token>) -> Self {
        let mut partner = vec![None; toks.len()];
        let mut stack: Vec<usize> = Vec::new();
        for (i, t) in toks.iter().enumerate() {
            match t.kind {
                T::LParen | T::LBracket | T::AtBracket | T::LBrace | T::StrBegin | T::InterpBegin | T::SpliceBegin => {
                    stack.push(i)
                }
                T::RParen | T::RBracket | T::RBrace | T::StrEnd | T::InterpEnd | T::SpliceEnd => {
                    let wanted: &[T] = match t.kind {
                        T::RParen => &[T::LParen],
                        T::RBracket => &[T::LBracket, T::AtBracket],
                        T::RBrace => &[T::LBrace],
                        T::StrEnd => &[T::StrBegin],
                        T::InterpEnd => &[T::InterpBegin],
                        _ => &[T::SpliceBegin],
                    };
                    if let Some(at) = stack.iter().rposition(|&o| wanted.contains(&toks[o].kind)) {
                        let open = stack[at];
                        stack.truncate(at);
                        partner[open] = Some(i);
                        partner[i] = Some(open);
                    }
                }
                _ => {}
            }
        }
        Tokens { src, toks, partner }
    }

    fn kind(&self, i: usize) -> T {
        self.toks[i].kind
    }

    fn text(&self, i: usize) -> &'s str {
        let s = self.toks[i].span;
        self.src.get(s.start as usize..s.end as usize).unwrap_or("")
    }

    /// Whether token `i` is a real token: not a line end or the end of file.
    fn real(&self, i: usize) -> bool {
        !matches!(self.kind(i), T::Newline | T::Eof)
    }

    /// The first real token that starts at or after `offset`.
    fn after(&self, offset: u32) -> Option<usize> {
        let mut i = self.toks.partition_point(|t| t.span.start < offset);
        while i < self.toks.len() && !self.real(i) {
            i += 1;
        }
        (i < self.toks.len()).then_some(i)
    }

    /// The real token that starts exactly at `offset`.
    fn at(&self, offset: u32) -> Option<usize> {
        self.after(offset).filter(|&i| self.toks[i].span.start == offset)
    }

    /// The last real token that ends at or before `offset`.
    fn before(&self, offset: u32) -> Option<usize> {
        let mut i = self.toks.partition_point(|t| t.span.start < offset);
        while i > 0 {
            i -= 1;
            if self.real(i) && self.toks[i].span.end <= offset {
                return Some(i);
            }
        }
        None
    }

    /// The last real token of a span that ends at `offset`.
    fn ending_at(&self, offset: u32) -> Option<usize> {
        self.before(offset).filter(|&i| self.toks[i].span.end == offset)
    }

    /// The previous real token before token `i`.
    fn prev_real(&self, i: usize) -> Option<usize> {
        (0..i).rev().find(|&j| self.real(j))
    }
}

// ----- what the tree says about tokens -------------------------------------

/// What a token is, where the token kind alone doesn't say how to space it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Role {
    Plain,
    /// A binary operator, an assignment, a default's or a constant's `=`,
    /// a return type's `->`, a union's `|`.
    BinOp,
    /// A prefix operator: unary `-`, `!`, `~`, `&x`, `*xs`, `&blk`, `^T`.
    Prefix,
    /// The `^` of a dereference, `p^`.
    Postfix,
    TernaryQ,
    TernaryColon,
    /// The `:` before an enum's backing type.
    EnumColon,
    /// The `?` that makes a type optional, `Int?`.
    OptionalQ,
    /// The braces of a block or proc body.
    BlockOpen,
    BlockClose,
    /// The bars around block parameters, and around a `guard`'s error.
    PipeOpen,
    PipeClose,
    /// The `->` that starts a proc literal.
    LambdaArrow,
    /// A `(` that is always written right after what comes before it: a
    /// call's arguments, a method's or proc's parameters.
    TightParen,
    /// A token of a method's name, like `[`, `]` and `=` in `def []=`.
    DefName,
    Range,
    /// The `:` after a name the tree says it labels: a parameter, a field,
    /// a local's or a constant's type, a named argument or option.
    Label,
    /// The `]` before a type's element type, as in `[]Int` and
    /// `map[String]Int`.
    TypeClose,
    /// A `[` written right after a type keyword: `map[`, `matrix[`.
    TightBracket,
    /// The `]` that closes attributes, before what they apply to.
    AttrClose,
}

/// How lines inside a frame are indented.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum FrameKind {
    /// A body that ends with `end` or `}`.
    Block,
    /// An `if`, `unless`, `comptime if` or `guard`: `else` and `elsif`
    /// line up with the line that opens it.
    Branch,
    /// A `case`: `when` and `else` line up with `case`.
    Case,
    /// `( )`, `[ ]`, `@[ ]` or a `{ }` that holds no statements.
    Bracket,
}

/// A block the tree says opens at a token.
#[derive(Clone, Copy, Debug)]
struct Reg {
    closer: usize,
    kind: FrameKind,
    /// The token after which the body starts, when it isn't the opener:
    /// the closing `|` of block parameters.
    header_end: Option<usize>,
}

/// A list that may take a trailing comma: its closer and the last token of
/// its last item.
#[derive(Clone, Copy, Debug)]
struct ListEnd {
    closer: usize,
    last: usize,
}

/// Walks the tree and records what the formatter needs about tokens.
struct Collector<'t, 's> {
    toks: &'t Tokens<'s>,
    roles: Vec<Role>,
    frames: HashMap<usize, Reg>,
    closers: HashMap<usize, usize>,
    lists: Vec<ListEnd>,
    /// Offsets of declarations that get a blank line before their doc
    /// comment: those next to a declaration spanning several lines.
    spaced_items: Vec<u32>,
    /// Tokens that start a statement, a declaration or an enum member: a
    /// line that starts with one doesn't continue the line before.
    starts: Vec<bool>,
    line_starts: Vec<u32>,
}

fn is_binary_op(kind: T) -> bool {
    matches!(
        kind,
        T::Plus
            | T::Minus
            | T::Star
            | T::Slash
            | T::Percent
            | T::StarStar
            | T::Amp
            | T::Pipe
            | T::Tilde
            | T::Shl
            | T::Shr
            | T::EqEq
            | T::NotEq
            | T::Lt
            | T::Le
            | T::Gt
            | T::Ge
            | T::Cmp
            | T::AndAnd
            | T::OrOr
    )
}

fn is_assign_op(kind: T) -> bool {
    matches!(
        kind,
        T::Eq
            | T::PlusEq
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

impl<'t, 's> Collector<'t, 's> {
    fn new(toks: &'t Tokens<'s>) -> Self {
        let mut line_starts = vec![0];
        line_starts.extend(toks.src.match_indices('\n').map(|(i, _)| i as u32 + 1));
        Collector {
            toks,
            roles: vec![Role::Plain; toks.toks.len()],
            frames: HashMap::new(),
            closers: HashMap::new(),
            lists: Vec::new(),
            spaced_items: Vec::new(),
            starts: vec![false; toks.toks.len()],
            line_starts,
        }
    }

    fn start(&mut self, offset: u32) {
        if let Some(i) = self.toks.at(offset) {
            self.starts[i] = true;
        }
    }

    fn line_of(&self, offset: u32) -> usize {
        self.line_starts.partition_point(|&s| s <= offset).saturating_sub(1)
    }

    fn set(&mut self, tok: Option<usize>, kinds: &[T], role: Role) {
        if let Some(i) = tok
            && i < self.toks.toks.len()
            && kinds.contains(&self.toks.kind(i))
            && self.roles[i] == Role::Plain
        {
            self.roles[i] = role;
        }
    }

    /// Records a block from `opener` to the `end` or `}` that ends `span`.
    /// A block that ends with `}` opens at its `{`.
    fn register(&mut self, opener: Option<usize>, span: Span, kind: FrameKind, header_end: Option<usize>) {
        let (Some(mut opener), Some(closer)) = (opener, self.toks.ending_at(span.end)) else { return };
        match self.toks.kind(closer) {
            T::Kw(K::End) => {}
            T::RBrace => {
                let Some(open) = self.toks.partner[closer] else { return };
                opener = open;
                self.roles[open] = Role::BlockOpen;
                self.roles[closer] = Role::BlockClose;
            }
            _ => return,
        }
        if opener >= closer || self.closers.contains_key(&closer) || self.frames.contains_key(&opener) {
            return;
        }
        self.closers.insert(closer, opener);
        self.frames.insert(opener, Reg { closer, kind, header_end });
    }

    /// The first token of a declaration or statement that starts at
    /// `start`, after its attributes and `private`.
    fn head(&self, start: u32) -> Option<usize> {
        let mut i = self.toks.after(start)?;
        loop {
            match self.toks.kind(i) {
                T::AtBracket => {
                    let close = self.toks.partner[i]?;
                    i = (close + 1..self.toks.toks.len()).find(|&j| self.toks.real(j))?;
                }
                T::Kw(K::Private) => i += 1,
                _ => return Some(i),
            }
        }
    }

    /// Marks the `]` that closes each attribute list.
    /// Marks the `]` that closes each attribute list, and its `@[` as where
    /// the declaration or statement starts.
    fn attrs(&mut self, attrs: &[Attribute]) {
        for attr in attrs {
            self.set(self.toks.after(attr.span.end), &[T::RBracket], Role::AttrClose);
            if let Some(open) = self.tok_before(attr.name.span.start)
                && self.toks.kind(open) == T::AtBracket
            {
                self.starts[open] = true;
            }
        }
    }

    /// The token before the expression or type that starts at `offset`.
    fn tok_before(&self, offset: u32) -> Option<usize> {
        self.toks.before(offset)
    }

    fn list(&mut self, open: Option<usize>, last_end: Option<u32>) {
        let (Some(open), Some(end)) = (open, last_end) else { return };
        let (Some(closer), Some(last)) = (self.toks.partner[open], self.toks.ending_at(end)) else { return };
        if last < closer {
            self.lists.push(ListEnd { closer, last });
        }
    }

    /// Marks the `:` right after a name.
    fn label(&mut self, name_end: u32) {
        self.set(self.toks.after(name_end), &[T::Colon], Role::Label);
    }

    fn generics(&mut self, generics: &[GenericParam]) {
        for g in generics.iter().filter(|g| g.ty.is_some()) {
            self.label(g.name.span.end);
        }
    }

    fn params(&mut self, open: Option<usize>, params: &[Param], block: Option<&BlockParamDecl>, variadic: bool) {
        for p in params {
            self.label(p.name.span.end);
            if p.splat {
                self.set(self.tok_before(p.name.span.start), &[T::Star], Role::Prefix);
            }
            if let Some(d) = &p.default {
                self.set(self.tok_before(d.span.start), &[T::Eq], Role::BinOp);
            }
        }
        if let Some(b) = block {
            self.set(self.tok_before(b.name.span.start), &[T::Amp], Role::Prefix);
            self.label(b.name.span.end);
        }
        // The `(` must follow on the same line: `def foo` followed by a line
        // that starts with `(` takes no parameters.
        let open = open.filter(|&i| self.toks.kind(i) == T::LParen && self.toks.prev_real(i) == Some(i - 1));
        self.set(open, &[T::LParen], Role::TightParen);
        // `...` can't take a trailing comma.
        if !variadic {
            let last = params.iter().map(|p| p.span.end).chain(block.map(|b| b.span.end)).max();
            self.list(open, last);
        }
    }

    fn cond(&mut self, cond: &Cond) {
        if let Cond::Bind { name, .. } = cond {
            self.set(self.toks.after(name.span.end), &[T::Eq], Role::BinOp);
        }
    }

    fn block_params(&mut self, params: &[BlockParam]) {
        for p in params {
            if p.by_ref {
                self.set(self.tok_before(p.name.span.start), &[T::Amp], Role::Prefix);
            }
        }
    }

    /// Marks the bars around `|a, b|` that start right after token `open`,
    /// and returns the closing one.
    fn pipes(&mut self, open: usize) -> Option<usize> {
        let first = (open + 1..self.toks.toks.len()).find(|&j| self.toks.real(j))?;
        if self.toks.kind(first) != T::Pipe {
            return None;
        }
        let close = (first + 1..self.toks.toks.len()).find(|&j| self.toks.kind(j) == T::Pipe)?;
        self.roles[first] = Role::PipeOpen;
        self.roles[close] = Role::PipeClose;
        Some(close)
    }
}

impl Visit for Collector<'_, '_> {
    fn visit_items(&mut self, items: &Vec<Item>) {
        let real: Vec<&Item> = items.iter().filter(|i| !matches!(i.kind, ItemKind::Error)).collect();
        for pair in real.windows(2) {
            let multi = |item: &Item| self.line_of(item.span.start) != self.line_of(item.span.end.saturating_sub(1));
            if multi(pair[0]) || multi(pair[1]) {
                self.spaced_items.push(pair[1].span.start);
            }
        }
        shared::walk_items(self, items);
    }

    fn visit_item(&mut self, item: &Item) {
        let head = self.head(item.span.start);
        self.start(item.span.start);
        if let Some(h) = head {
            self.starts[h] = true;
        }
        self.attrs(&item.attrs);
        match &item.kind {
            ItemKind::Import(import) => {
                // `as:`, which the tree keeps no span for.
                let end = item.span.end;
                let mut i = self.toks.after(import.path_span.end);
                while let Some(j) = i.filter(|&j| self.toks.toks[j].span.end <= end) {
                    if self.toks.kind(j) == T::Ident && self.toks.text(j) == "as" {
                        self.set(Some(j + 1), &[T::Colon], Role::Label);
                    }
                    i = Some(j + 1);
                }
            }
            ItemKind::Def(f) => {
                if matches!(f.body, FnBody::Block(_)) {
                    // The body starts after the signature, even one that
                    // ends with an operator, like `def -`.
                    let header_end = self.toks.ending_at(f.sig_span.end);
                    self.register(self.toks.at(f.sig_span.start), item.span, FrameKind::Block, header_end);
                }
                if let (Some(first), Some(last)) =
                    (self.toks.at(f.name.span.start), self.toks.ending_at(f.name.span.end))
                {
                    for i in first..=last {
                        self.roles[i] = Role::DefName;
                    }
                }
                let open = self.toks.after(f.name.span.end);
                self.params(open, &f.params, f.block.as_ref(), f.c_variadic.is_some());
                if let Some(ret) = &f.ret {
                    self.set(self.tok_before(ret.span.start), &[T::Arrow], Role::BinOp);
                }
                if let FnBody::Expr(e) = &f.body {
                    self.set(self.tok_before(e.span.start), &[T::Eq], Role::BinOp);
                }
            }
            ItemKind::Struct(s) => {
                self.register(head, item.span, FrameKind::Block, None);
                self.generics(&s.generics);
            }
            ItemKind::Module(_) | ItemKind::Extend(_) => {
                self.register(head, item.span, FrameKind::Block, None);
            }
            ItemKind::Cimport(c) => {
                for option in &c.options {
                    self.label(option.name.span.end);
                    if let CimportValue::Hash { entries, .. } = &option.value {
                        for entry in entries {
                            self.label(entry.key_span.end);
                        }
                    }
                }
            }
            ItemKind::Enum(e) => {
                self.register(head, item.span, FrameKind::Block, None);
                if let Some(b) = &e.backing {
                    self.set(self.tok_before(b.span.start), &[T::Colon], Role::EnumColon);
                }
                for m in &e.members {
                    self.start(m.name.span.start);
                    if let Some(v) = &m.value {
                        self.set(self.tok_before(v.span.start), &[T::Eq], Role::BinOp);
                    }
                }
            }
            ItemKind::ComptimeIf(_) => self.register(head, item.span, FrameKind::Branch, None),
            ItemKind::Union(u) => {
                self.generics(&u.generics);
                for (i, v) in u.variants.iter().enumerate() {
                    let kinds: &[T] = if i == 0 { &[T::Eq] } else { &[T::Pipe] };
                    self.set(self.tok_before(v.span.start), kinds, Role::BinOp);
                }
            }
            ItemKind::Const(c) => {
                if c.ty.is_some() {
                    self.label(c.name.span.end);
                }
                self.set(self.tok_before(c.value.span.start), &[T::Eq], Role::BinOp);
            }
            ItemKind::Field(f) => {
                self.label(f.name.span.end);
                if let Some(d) = &f.default {
                    self.set(self.tok_before(d.span.start), &[T::Eq], Role::BinOp);
                }
            }
            _ => {}
        }
        shared::walk_item(self, item);
    }

    fn visit_stmt(&mut self, stmt: &Stmt) {
        self.start(stmt.span.start);
        if let Some(h) = self.head(stmt.span.start) {
            self.starts[h] = true;
        }
        self.attrs(&stmt.attrs);
        match &stmt.kind {
            StmtKind::Decl { names, value, uninit, .. } => {
                if let Some(last) = names.last() {
                    self.label(last.span.end);
                }
                if let Some(v) = value {
                    self.set(self.tok_before(v.span.start), &[T::Eq], Role::BinOp);
                } else if *uninit && let Some(dash) = self.toks.ending_at(stmt.span.end) {
                    self.set(self.toks.prev_real(dash), &[T::Eq], Role::BinOp);
                }
            }
            StmtKind::Assign { values, .. } => {
                if let Some(v) = values.first()
                    && let Some(op) = self.tok_before(v.span.start)
                    && is_assign_op(self.toks.kind(op))
                {
                    self.set(Some(op), &[self.toks.kind(op)], Role::BinOp);
                }
            }
            StmtKind::Defer(_) => {
                let head = self.head(stmt.span.start);
                if let Some(h) = head
                    && self.toks.kind(h) == T::Kw(K::Defer)
                    && self.toks.toks.get(h + 1).is_some_and(|t| t.kind == T::Kw(K::Do))
                {
                    self.register(head, stmt.span, FrameKind::Block, None);
                }
            }
            StmtKind::Guard { names, err, .. } => {
                let head = self.head(stmt.span.start).filter(|&h| self.toks.kind(h) == T::Kw(K::Guard));
                let mut header_end = None;
                if let Some(err) = err {
                    let open = self.tok_before(err.span.start);
                    let close = self.toks.after(err.span.end);
                    if open.is_some_and(|i| self.toks.kind(i) == T::Pipe)
                        && close.is_some_and(|i| self.toks.kind(i) == T::Pipe)
                    {
                        self.set(open, &[T::Pipe], Role::PipeOpen);
                        self.set(close, &[T::Pipe], Role::PipeClose);
                        header_end = close;
                    }
                }
                self.register(head, stmt.span, FrameKind::Branch, header_end);
                if let Some(last) = names.last() {
                    self.set(self.toks.after(last.span.end), &[T::Eq], Role::BinOp);
                }
            }
            _ => {}
        }
        shared::walk_stmt(self, stmt);
    }

    fn visit_expr(&mut self, e: &Expr) {
        let first = self.toks.at(e.span.start);
        let first_kind = first.map(|i| self.toks.kind(i));
        match &e.kind {
            ExprKind::Binary { lhs, .. } => {
                if let Some(op) = self.toks.after(lhs.span.end)
                    && is_binary_op(self.toks.kind(op))
                {
                    self.set(Some(op), &[self.toks.kind(op)], Role::BinOp);
                }
            }
            ExprKind::Unary { .. } => self.set(first, &[T::Minus, T::Bang, T::Tilde], Role::Prefix),
            ExprKind::AddrOf(_) => self.set(first, &[T::Amp], Role::Prefix),
            ExprKind::Deref(_) => self.set(self.toks.ending_at(e.span.end), &[T::Caret], Role::Postfix),
            ExprKind::Ternary { cond, else_, .. } => {
                self.set(self.toks.after(cond.span.end), &[T::Question], Role::TernaryQ);
                self.set(self.tok_before(else_.span.start), &[T::Colon], Role::TernaryColon);
            }
            ExprKind::Range { lo, .. } => {
                let op = match lo {
                    Some(lo) => self.toks.after(lo.span.end),
                    None => first,
                };
                self.set(op, &[T::DotDot, T::DotDotDot], Role::Range);
            }
            ExprKind::If(f) => {
                if matches!(first_kind, Some(T::Kw(K::If | K::Unless))) {
                    self.register(first, e.span, FrameKind::Branch, None);
                }
                self.cond(&f.cond);
                for (c, _) in &f.elifs {
                    self.cond(c);
                }
            }
            ExprKind::ComptimeIf(f) => {
                if first_kind == Some(T::Kw(K::Comptime)) {
                    self.register(first, e.span, FrameKind::Branch, None);
                }
                self.cond(&f.cond);
                for (c, _) in &f.elifs {
                    self.cond(c);
                }
            }
            ExprKind::While { cond, .. } => {
                if matches!(first_kind, Some(T::Kw(K::While | K::Until))) {
                    self.register(first, e.span, FrameKind::Block, None);
                }
                self.cond(cond);
            }
            ExprKind::For(f) => {
                if first_kind == Some(T::Kw(K::For)) {
                    self.register(first, e.span, FrameKind::Block, None);
                }
                self.block_params(&f.bindings);
            }
            ExprKind::Loop(_) => {
                if first_kind == Some(T::Kw(K::Loop)) {
                    self.register(first, e.span, FrameKind::Block, None);
                }
            }
            ExprKind::Case(_) => {
                if first_kind == Some(T::Kw(K::Case)) {
                    self.register(first, e.span, FrameKind::Case, None);
                }
            }
            ExprKind::Comptime(_) => {
                let next_is_do = first.and_then(|i| self.toks.toks.get(i + 1)).is_some_and(|t| t.kind == T::Kw(K::Do));
                if first_kind == Some(T::Kw(K::Comptime)) && next_is_do {
                    self.register(first, e.span, FrameKind::Block, None);
                }
            }
            ExprKind::Quote(_) => {
                if first_kind == Some(T::Kw(K::Quote)) {
                    self.register(first, e.span, FrameKind::Block, None);
                }
            }
            ExprKind::Lambda(l) => {
                if first_kind == Some(T::Arrow) {
                    self.set(first, &[T::Arrow], Role::LambdaArrow);
                    let open = first.and_then(|i| self.toks.after(self.toks.toks[i].span.end));
                    self.params(open, &l.params, None, false);
                    if let Some(ret) = &l.ret {
                        self.set(self.tok_before(ret.span.start), &[T::Arrow], Role::BinOp);
                    }
                    self.register(first, e.span, FrameKind::Block, None);
                }
            }
            ExprKind::Call(call) => {
                let name_end = match &call.callee {
                    Callee::Name(n) | Callee::IVar(n) => n.span.end,
                    Callee::Method { name, .. } => name.span.end,
                };
                let open = self
                    .toks
                    .after(name_end)
                    .filter(|&i| call.parens && self.toks.kind(i) == T::LParen && !self.toks.toks[i].space_before);
                if open.is_some() {
                    self.set(open, &[T::LParen], Role::TightParen);
                    self.list(open, call.args.last().map(|a| a.value.span.end));
                }
                for arg in &call.args {
                    if let Some(name) = &arg.name {
                        self.label(name.span.end);
                    }
                    if arg.splat {
                        self.set(self.tok_before(arg.value.span.start), &[T::Star], Role::Prefix);
                    }
                }
                if let Some(block) = &call.block
                    && let Some(open) = self.toks.at(block.span.start)
                {
                    let header_end = self.pipes(open);
                    self.block_params(&block.params);
                    self.register(Some(open), block.span, FrameKind::Block, header_end);
                }
            }
            ExprKind::Array(elems) if first_kind == Some(T::LBracket) => {
                self.list(first, elems.last().map(|x| x.span.end));
            }
            _ => {}
        }
        shared::walk_expr(self, e);
    }

    fn visit_type(&mut self, ty: &TypeExpr) {
        match &ty.kind {
            TypeKind::Pointer(_) => self.set(self.toks.at(ty.span.start), &[T::Caret], Role::Prefix),
            TypeKind::Optional(_) => self.set(self.toks.ending_at(ty.span.end), &[T::Question], Role::OptionalQ),
            TypeKind::Array(_, elem)
            | TypeKind::Slice(elem)
            | TypeKind::Dynamic(elem)
            | TypeKind::MultiPointer(elem)
            | TypeKind::Matrix { elem, .. } => {
                self.set(self.tok_before(elem.span.start), &[T::RBracket], Role::TypeClose);
                if let TypeKind::Matrix { rows, .. } = &ty.kind {
                    self.set(self.tok_before(rows.span.start), &[T::LBracket], Role::TightBracket);
                }
            }
            TypeKind::Map(key, value) => {
                self.set(self.tok_before(key.span.start), &[T::LBracket], Role::TightBracket);
                self.set(self.tok_before(value.span.start), &[T::RBracket], Role::TypeClose);
            }
            TypeKind::Path { segments, args } if !args.is_empty() => {
                if let Some(last) = segments.last() {
                    self.set(self.toks.after(last.span.end), &[T::LParen], Role::TightParen);
                }
            }
            TypeKind::Proc { params, ret, .. } | TypeKind::Block { params, ret } => {
                // `@[c] proc(…)`: the attribute, then the `(` after `proc`.
                if let Some(first) = self.toks.at(ty.span.start) {
                    let mut name = first;
                    if self.toks.kind(first) == T::AtBracket
                        && let Some(close) = self.toks.partner[first]
                    {
                        self.roles[close] = Role::AttrClose;
                        name = close + 1;
                    }
                    self.set(Some(name + 1), &[T::LParen], Role::TightParen);
                }
                // A parameter's name, which the tree drops: `proc(n: Int)`.
                for p in params {
                    self.set(self.tok_before(p.span.start), &[T::Colon], Role::Label);
                }
                if let Some(ret) = ret {
                    self.set(self.tok_before(ret.span.start), &[T::Arrow], Role::BinOp);
                }
            }
            _ => {}
        }
        shared::walk_type(self, ty);
    }
}

// ----- layout --------------------------------------------------------------

/// One unit of output: a token, a string literal or splice kept whole, a
/// `;`, or a comment.
#[derive(Clone, Copy, Debug)]
struct Elem {
    start: u32,
    end: u32,
    /// The first and last token, for tokens and kept-whole groups.
    first: usize,
    last: usize,
    kind: ElemKind,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum ElemKind {
    /// One token.
    Token,
    /// Tokens printed exactly as written: a string with interpolation, or
    /// a splice and the name parts glued to it.
    Verbatim,
    /// A `;` between statements.
    Semi,
    Comment,
}

/// A source line as the formatter sees it: elements with no line break
/// between them.
#[derive(Clone, Debug)]
struct Line {
    elems: std::ops::Range<usize>,
    /// Whether a blank line was written before it.
    blank_before: bool,
    /// Whether it continues the line before with a `\`.
    backslash: bool,
    level: i32,
    /// Whether a block or bracket opened on it is still open after it.
    opens: bool,
    /// Whether it starts with a closer, or with `else`, `elsif` or `when`.
    closes: bool,
    mid: bool,
    force_blank: bool,
}

#[derive(Clone, Copy, Debug)]
struct Frame {
    kind: FrameKind,
    opener: usize,
    closer: Option<usize>,
    header_end: Option<usize>,
    open_line: usize,
    base: i32,
    inner: i32,
    mid: i32,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Space {
    None,
    One,
    Keep,
}

struct Layout<'t, 's> {
    src: &'s str,
    toks: &'t Tokens<'s>,
    facts: Collector<'t, 's>,
    elems: Vec<Elem>,
    lines: Vec<Line>,
    /// Commas to drop (trailing commas before a closer on the same line).
    drop: Vec<bool>,
    /// Elements after which a `,` is added.
    add_comma: Vec<bool>,
    /// For each token, whether it starts a line.
    starts_line: Vec<bool>,
    /// The element each token is the first or last token of.
    elem_of_tok: HashMap<usize, usize>,
}

impl<'t, 's> Layout<'t, 's> {
    fn new(src: &'s str, toks: &'t Tokens<'s>, comments: &[crate::token::Comment], facts: Collector<'t, 's>) -> Self {
        let mut layout = Layout {
            src,
            toks,
            facts,
            elems: Vec::new(),
            lines: Vec::new(),
            drop: Vec::new(),
            add_comma: Vec::new(),
            starts_line: vec![false; toks.toks.len()],
            elem_of_tok: HashMap::new(),
        };
        layout.build_elems(comments);
        layout.build_lines();
        for (e, elem) in layout.elems.iter().enumerate() {
            if elem.kind != ElemKind::Comment {
                layout.elem_of_tok.insert(elem.last, e);
                layout.elem_of_tok.insert(elem.first, e);
            }
        }
        for line in &layout.lines {
            let elem = layout.elems[line.elems.start];
            if elem.kind != ElemKind::Comment {
                layout.starts_line[elem.first] = true;
            }
        }
        layout
    }

    fn build_elems(&mut self, comments: &[crate::token::Comment]) {
        let toks = &self.toks.toks;
        let mut elems: Vec<Elem> = Vec::new();
        let mut i = 0;
        while i < toks.len() {
            let t = toks[i];
            match t.kind {
                T::Eof => break,
                T::Newline => {
                    if self.toks.text(i) == ";" {
                        elems.push(Elem {
                            start: t.span.start,
                            end: t.span.end,
                            first: i,
                            last: i,
                            kind: ElemKind::Semi,
                        });
                    }
                    i += 1;
                }
                T::StrBegin | T::SpliceBegin | T::AtSplice | T::ColonSplice => {
                    let begin = if t.kind == T::StrBegin || t.kind == T::SpliceBegin { i } else { i + 1 };
                    let mut end = self.toks.partner[begin].unwrap_or(begin);
                    if t.kind != T::StrBegin {
                        // Name parts glued after the splice, and more splices.
                        while let Some(n) = toks.get(end + 1)
                            && !n.space_before
                            && n.span.start == toks[end].span.end
                            && matches!(n.kind, T::Ident | T::Const | T::SpliceBegin)
                        {
                            end = if n.kind == T::SpliceBegin {
                                self.toks.partner[end + 1].unwrap_or(end + 1)
                            } else {
                                end + 1
                            };
                        }
                    }
                    let mut start_tok = i;
                    // A name glued before the splice: `bump_#{name}`, `@hp_#{n}`.
                    if t.kind == T::SpliceBegin
                        && !t.space_before
                        && let Some(prev) = elems.last()
                        && prev.kind == ElemKind::Token
                        && prev.end == t.span.start
                        && matches!(toks[prev.first].kind, T::Ident | T::Const | T::IVar)
                    {
                        start_tok = prev.first;
                        elems.pop();
                    }
                    let start = toks[start_tok].span.start;
                    let last_end = toks[end].span.end;
                    elems.push(Elem { start, end: last_end, first: start_tok, last: end, kind: ElemKind::Verbatim });
                    i = end + 1;
                }
                _ => {
                    if t.span.start == t.span.end {
                        i += 1;
                        continue;
                    }
                    elems.push(Elem { start: t.span.start, end: t.span.end, first: i, last: i, kind: ElemKind::Token });
                    i += 1;
                }
            }
        }
        // A comment inside a splice that spans lines is part of its text.
        let verbatim: Vec<(u32, u32)> =
            elems.iter().filter(|e| e.kind == ElemKind::Verbatim).map(|e| (e.start, e.end)).collect();
        for c in comments {
            if verbatim.iter().any(|&(start, end)| start <= c.span.start && c.span.start < end) {
                continue;
            }
            elems.push(Elem {
                start: c.span.start,
                end: c.span.end,
                first: usize::MAX,
                last: usize::MAX,
                kind: ElemKind::Comment,
            });
        }
        elems.sort_by_key(|e| (e.start, e.kind == ElemKind::Comment));
        self.drop = vec![false; elems.len()];
        self.add_comma = vec![false; elems.len()];
        self.elems = elems;
    }

    fn gap(&self, i: usize) -> &'s str {
        let start = if i == 0 { 0 } else { self.elems[i - 1].end as usize };
        self.src.get(start..self.elems[i].start as usize).unwrap_or("")
    }

    fn build_lines(&mut self) {
        let mut lines: Vec<Line> = Vec::new();
        for i in 0..self.elems.len() {
            let gap = self.gap(i);
            let breaks = gap.matches('\n').count();
            if i == 0 || breaks > 0 {
                if let Some(last) = lines.last_mut() {
                    last.elems.end = i;
                }
                lines.push(Line {
                    elems: i..i,
                    blank_before: breaks >= 2,
                    backslash: i > 0 && breaks == 1 && gap.contains('\\'),
                    level: 0,
                    opens: false,
                    closes: false,
                    mid: false,
                    force_blank: false,
                });
            }
        }
        if let Some(last) = lines.last_mut() {
            last.elems.end = self.elems.len();
        }
        self.lines = lines;
    }

    fn is_comment_line(&self, l: usize) -> bool {
        self.elems[self.lines[l].elems.start].kind == ElemKind::Comment
    }

    fn line_of_elem(&self, e: usize) -> usize {
        self.lines.partition_point(|l| l.elems.end <= e)
    }

    /// The column where an element starts in the source.
    fn column(&self, e: usize) -> usize {
        let start = self.elems[e].start;
        let line = self.facts.line_of(start);
        (start - self.facts.line_starts[line]) as usize
    }

    fn is_mid(&self, tok: usize) -> bool {
        matches!(self.toks.kind(tok), T::Kw(K::Else | K::Elsif | K::When))
    }

    /// Computes each code line's indentation from the blocks and brackets
    /// open before it.
    fn indent(&mut self) {
        let mut frames: Vec<Frame> = Vec::new();
        for l in 0..self.lines.len() {
            if self.is_comment_line(l) {
                continue;
            }
            let first_elem = self.lines[l].elems.start;
            let first = self.elems[first_elem].first;
            let closing = self.closing_frame(&frames, first);
            let (level, closes, mid) = if let Some(at) = closing {
                (frames[at].base, true, false)
            } else if let Some(top) = frames.last()
                && matches!(top.kind, FrameKind::Branch | FrameKind::Case)
                && self.is_mid(first)
                && (top.kind == FrameKind::Case || self.toks.kind(first) != T::Kw(K::When))
            {
                (top.mid, false, true)
            } else {
                let inner = frames.last().map_or(0, |f| f.inner);
                (inner + i32::from(self.continues(&frames, first)), false, false)
            };
            let line = &mut self.lines[l];
            line.level = level;
            line.closes = closes;
            line.mid = mid;
            for e in self.lines[l].elems.clone() {
                let elem = self.elems[e];
                if elem.kind != ElemKind::Token {
                    continue;
                }
                let tok = elem.first;
                if let Some(at) = self.closing_frame(&frames, tok) {
                    frames.truncate(at);
                }
                if let Some(frame) = self.opening_frame(tok, l, level, first, &frames) {
                    frames.push(frame);
                }
            }
            self.lines[l].opens = frames.last().is_some_and(|f| f.open_line == l);
        }
        // An own-line comment takes the indentation of the line after it,
        // or the body's, before a closer it was written deeper than.
        let mut next: Option<usize> = None;
        for l in (0..self.lines.len()).rev() {
            if !self.is_comment_line(l) {
                next = Some(l);
                continue;
            }
            self.lines[l].level = match next {
                None => 0,
                Some(n) => {
                    let deeper = self.column(self.lines[l].elems.start) > self.column(self.lines[n].elems.start);
                    let line = &self.lines[n];
                    line.level + i32::from((line.closes || line.mid) && deeper)
                }
            };
        }
    }

    /// The index of the frame that token `tok` closes, if it closes one.
    fn closing_frame(&self, frames: &[Frame], tok: usize) -> Option<usize> {
        if let Some(at) = frames.iter().rposition(|f| f.closer == Some(tok)) {
            return Some(at);
        }
        // An `end` the tree didn't explain closes the innermost block.
        if self.toks.kind(tok) == T::Kw(K::End)
            && !self.facts.closers.contains_key(&tok)
            && !self.toks.prev_real(tok).is_some_and(|p| matches!(self.toks.kind(p), T::Dot | T::SafeNav))
        {
            return frames.iter().rposition(|f| f.kind != FrameKind::Bracket);
        }
        None
    }

    fn opening_frame(&self, tok: usize, line: usize, level: i32, first: usize, frames: &[Frame]) -> Option<Frame> {
        let (kind, closer, header_end) = if let Some(reg) = self.facts.frames.get(&tok) {
            (reg.kind, Some(reg.closer), reg.header_end)
        } else if matches!(self.toks.kind(tok), T::LParen | T::LBracket | T::AtBracket | T::LBrace) {
            (FrameKind::Bracket, Some(self.toks.partner[tok]?), None)
        } else {
            return None;
        };
        let collapsed = frames.last().filter(|f| f.open_line == line);
        let inner = collapsed.map_or(level + 1, |f| f.inner);
        let (mid, inner) = match kind {
            FrameKind::Case => {
                let mid = if tok == first { level } else { level + 1 };
                (mid, mid + 1)
            }
            // A list in a block's header that closes at the end of its last
            // item, like `def f(a: Int,` and `b: Int)`, is indented one
            // step deeper than the body that follows.
            FrameKind::Bracket
                if collapsed.is_some_and(|f| f.kind != FrameKind::Bracket)
                    && closer.is_some_and(|c| !self.starts_line[c]) =>
            {
                (level, inner + 1)
            }
            _ => (level, inner),
        };
        Some(Frame { kind, opener: tok, closer, header_end, open_line: line, base: level, inner, mid })
    }

    /// Whether the line that starts with token `first` continues the
    /// statement or list item of the line before. In a list, a line after a
    /// `,` or the opener starts an item; elsewhere, a line starts a
    /// statement, a declaration or an enum member, or goes on with one.
    fn continues(&self, frames: &[Frame], first: usize) -> bool {
        let Some(prev) = self.toks.prev_real(first) else { return false };
        match frames.last() {
            Some(f) if f.kind == FrameKind::Bracket => !(self.toks.kind(prev) == T::Comma || prev == f.opener),
            top => {
                let after_header = top.is_some_and(|f| prev == f.opener || Some(prev) == f.header_end);
                !self.facts.starts[first] && !after_header && self.facts.roles[prev] != Role::AttrClose
            }
        }
    }

    /// Decides the trailing commas: added after the last item of a list
    /// whose closer starts a line, dropped before a closer on the same line.
    fn trailing_commas(&mut self) {
        let elem_of_tok = &self.elem_of_tok;
        for list in &self.facts.lists {
            let (Some(&closer_e), Some(&last_e)) = (elem_of_tok.get(&list.closer), elem_of_tok.get(&list.last)) else {
                continue;
            };
            // The token after the last item: a `,` or the closer.
            let after = (list.last + 1..list.closer).find(|&i| self.toks.real(i));
            let comma = after.filter(|&i| self.toks.kind(i) == T::Comma);
            if after.is_some() && comma.is_none() {
                continue;
            }
            let closer_line = self.line_of_elem(closer_e);
            let closer_first = self.lines[closer_line].elems.start == closer_e;
            match comma {
                None if closer_first && self.elems[last_e].last == list.last => self.add_comma[last_e] = true,
                Some(c) if !closer_first => {
                    if let Some(&ce) = elem_of_tok.get(&c)
                        && self.line_of_elem(ce) == closer_line
                    {
                        self.drop[ce] = true;
                    }
                }
                _ => {}
            }
        }
    }

    /// Marks the blank lines to keep, add and drop.
    fn blank_lines(&mut self) {
        for &offset in &self.facts.spaced_items {
            let e = self.elems.partition_point(|x| x.start < offset);
            if self.elems.get(e).is_none_or(|x| x.start != offset || x.kind == ElemKind::Comment) {
                continue;
            }
            let mut l = self.line_of_elem(e);
            if self.lines[l].elems.start != e {
                continue;
            }
            while l > 0 && self.is_comment_line(l - 1) && !self.lines[l].blank_before {
                l -= 1;
            }
            self.lines[l].force_blank = true;
        }
    }

    fn render(mut self) -> String {
        if self.elems.is_empty() {
            return String::new();
        }
        self.indent();
        self.trailing_commas();
        self.blank_lines();
        let mut out: Vec<OutLine> = Vec::new();
        for l in 0..self.lines.len() {
            let line = &self.lines[l];
            let prev = l.checked_sub(1).map(|p| &self.lines[p]);
            let blank = l > 0
                && (line.blank_before || line.force_blank)
                && !line.closes
                && !line.mid
                && !prev.is_some_and(|p| p.opens || p.mid);
            if blank {
                out.push(OutLine { indent: 0, code: String::new(), comment: None });
            }
            if line.backslash
                && let Some(last) = out.last_mut()
            {
                last.code.push_str(" \\");
            }
            out.push(self.render_line(l));
        }
        align_comments(&mut out);
        let mut text = String::new();
        for line in &out {
            if !line.code.is_empty() || line.comment.is_some() {
                text.push_str(&" ".repeat(line.indent));
            }
            text.push_str(&line.code);
            if let Some((col, comment)) = &line.comment {
                if !line.code.is_empty() {
                    text.push_str(&" ".repeat(col.saturating_sub(line.end_column()).max(1)));
                }
                text.push_str(comment);
            }
            text.push('\n');
        }
        text
    }

    fn render_line(&self, l: usize) -> OutLine {
        let line = &self.lines[l];
        let mut code = String::new();
        let mut comment = None;
        let mut prev: Option<usize> = None;
        for e in line.elems.clone() {
            if self.drop[e] {
                continue;
            }
            let elem = self.elems[e];
            if elem.kind == ElemKind::Comment {
                comment = Some(self.comment_text(e));
                continue;
            }
            if let Some(p) = prev
                && self.space(p, e)
            {
                code.push(' ');
            }
            code.push_str(&self.src[elem.start as usize..elem.end as usize]);
            if self.add_comma[e] {
                code.push(',');
            }
            prev = Some(e);
        }
        let indent = usize::try_from(line.level).unwrap_or(0) * 2;
        OutLine { indent, code, comment: comment.map(|c| (0, c)) }
    }

    /// A comment with a space after its `#`; a `#!` line opening the file
    /// is kept as written.
    fn comment_text(&self, e: usize) -> String {
        let elem = self.elems[e];
        let raw = self.src[elem.start as usize..elem.end as usize].trim_end();
        let body = raw.strip_prefix('#').unwrap_or(raw);
        if body.is_empty() || body.starts_with([' ', '\t']) || (elem.start == 0 && body.starts_with('!')) {
            raw.to_string()
        } else {
            format!("# {body}")
        }
    }

    fn role(&self, e: usize) -> Role {
        let elem = self.elems[e];
        if elem.kind == ElemKind::Token { self.facts.roles[elem.first] } else { Role::Plain }
    }

    fn tok_kind(&self, e: usize) -> Option<T> {
        let elem = self.elems[e];
        (elem.kind == ElemKind::Token).then(|| self.toks.kind(elem.first))
    }

    /// Whether to put a space between two elements on one line.
    fn space(&self, a: usize, b: usize) -> bool {
        let written = !self.src[self.elems[a].end as usize..self.elems[b].start as usize].is_empty();
        match self.spacing(a, b) {
            Space::One => true,
            Space::Keep => written,
            Space::None => written && !self.glues(a, b),
        }
    }

    /// Whether two elements written with a space between them read the same
    /// without it.
    fn glues(&self, a: usize, b: usize) -> bool {
        let (ea, eb) = (self.elems[a], self.elems[b]);
        if ea.kind != ElemKind::Token || eb.kind != ElemKind::Token {
            return true;
        }
        // A string's kind carries its index in the lexer's table.
        fn bare(kind: T) -> T {
            match kind {
                T::Str(_) => T::Str(0),
                kind => kind,
            }
        }
        let joined = format!("{}{}", self.toks.text(ea.first), self.toks.text(eb.first));
        let lexed = lex(FileId(0), &joined);
        let kinds: Vec<T> =
            lexed.tokens.iter().map(|t| bare(t.kind)).filter(|k| !matches!(k, T::Newline | T::Eof)).collect();
        lexed.diagnostics.is_empty() && kinds == [bare(self.toks.kind(ea.first)), bare(self.toks.kind(eb.first))]
    }

    fn spacing(&self, a: usize, b: usize) -> Space {
        let (ra, rb) = (self.role(a), self.role(b));
        let (ka, kb) = (self.tok_kind(a), self.tok_kind(b));
        let class_a = self.class(a);
        let class_b = self.class(b);
        use Class as C;
        if matches!(class_b, C::Comma | C::Semi) {
            return Space::None;
        }
        if class_a == C::Open || class_b == C::Close {
            return Space::None;
        }
        if matches!(class_a, C::Comma | C::Semi) || ra == Role::AttrClose {
            return Space::One;
        }
        if ra == Role::TypeClose || rb == Role::TightBracket {
            return Space::None;
        }
        if ra == Role::BlockOpen {
            return if rb == Role::BlockClose { Space::None } else { Space::One };
        }
        if rb == Role::BlockClose {
            return Space::One;
        }
        if matches!(ka, Some(T::Dot | T::SafeNav)) || matches!(kb, Some(T::Dot | T::SafeNav)) {
            return Space::None;
        }
        if ra == Role::PipeOpen || rb == Role::PipeClose {
            return Space::None;
        }
        if rb == Role::PipeOpen || ra == Role::PipeClose {
            return Space::One;
        }
        if ra == Role::Prefix {
            return Space::None;
        }
        if ra == Role::DefName && matches!(rb, Role::DefName | Role::TightParen) {
            return Space::None;
        }
        if rb == Role::DefName {
            return Space::One;
        }
        if rb == Role::TightParen {
            return Space::None;
        }
        if ra == Role::LambdaArrow {
            return Space::One;
        }
        if ra == Role::Range || rb == Role::Range {
            return Space::None;
        }
        let spaced = [Role::BinOp, Role::TernaryQ, Role::TernaryColon, Role::EnumColon];
        if spaced.contains(&ra) || spaced.contains(&rb) {
            return Space::One;
        }
        if rb == Role::Label {
            return Space::None;
        }
        if ra == Role::Label {
            return Space::One;
        }
        if kb == Some(T::Colon) {
            return if self.attached(b) { Space::None } else { Space::Keep };
        }
        if ka == Some(T::Colon) {
            return if self.attached(a) { Space::One } else { Space::Keep };
        }
        if matches!(rb, Role::OptionalQ | Role::Postfix) {
            return Space::None;
        }
        if rb == Role::Prefix {
            return if class_a == C::Keyword { Space::One } else { Space::Keep };
        }
        if class_a == C::Keyword {
            // `yield(x)` and `yield [x]` differ, and so might `end[0]`.
            let keep =
                matches!(kb, Some(T::LParen | T::LBracket | T::LBrace)) && matches!(ka, Some(T::Kw(K::Yield | K::End)));
            return if keep { Space::Keep } else { Space::One };
        }
        if class_b == C::Keyword || (class_a == C::Atom && class_b == C::Atom) || rb == Role::BlockOpen {
            return Space::One;
        }
        Space::Keep
    }

    /// Whether the `:` at element `e` was written right after the name
    /// before it, as in `x: Int` and `name: value`.
    fn attached(&self, e: usize) -> bool {
        !self.toks.toks[self.elems[e].first].space_before
    }

    fn class(&self, e: usize) -> Class {
        let elem = self.elems[e];
        match elem.kind {
            ElemKind::Comment => return Class::Other,
            ElemKind::Semi => return Class::Semi,
            ElemKind::Verbatim => return Class::Atom,
            ElemKind::Token => {}
        }
        let role = self.facts.roles[elem.first];
        match self.toks.kind(elem.first) {
            T::Int
            | T::Float
            | T::Str(_)
            | T::Symbol
            | T::Ident
            | T::Const
            | T::IVar
            | T::TypeParam
            | T::TripleDash
            | T::Kw(K::Nil | K::True | K::False | K::SelfKw) => Class::Atom,
            // A keyword used as a name, like `TypeKind.struct`.
            T::Kw(_)
                if self
                    .toks
                    .prev_real(elem.first)
                    .is_some_and(|p| matches!(self.toks.kind(p), T::Dot | T::SafeNav)) =>
            {
                Class::Atom
            }
            T::Kw(_) => Class::Keyword,
            T::LParen | T::LBracket | T::AtBracket => Class::Open,
            T::LBrace if role != Role::BlockOpen => Class::Open,
            T::RParen | T::RBracket => Class::Close,
            T::RBrace if role != Role::BlockClose => Class::Close,
            T::Comma => Class::Comma,
            _ => Class::Other,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Class {
    Atom,
    Keyword,
    Open,
    Close,
    Comma,
    Semi,
    Other,
}

/// A line of output: its indentation, its code, and a trailing or
/// own-line comment with the column it is aligned to.
struct OutLine {
    indent: usize,
    code: String,
    comment: Option<(usize, String)>,
}

impl OutLine {
    /// The column after the code: on its last line when a string literal
    /// spans several.
    fn end_column(&self) -> usize {
        match self.code.rsplit_once('\n') {
            Some((_, last)) => last.chars().count(),
            None => self.indent + self.code.chars().count(),
        }
    }
}

/// Lines up the trailing comments of consecutive lines with the same
/// indentation, one space after the longest code among them.
fn align_comments(out: &mut [OutLine]) {
    let trailing = |l: &OutLine| l.comment.is_some() && !l.code.is_empty();
    let mut i = 0;
    while i < out.len() {
        if !trailing(&out[i]) {
            i += 1;
            continue;
        }
        let mut j = i + 1;
        while j < out.len() && trailing(&out[j]) && out[j].indent == out[i].indent && !out[j].code.contains('\n') {
            j += 1;
        }
        let col = out[i..j].iter().map(|l| l.end_column() + 1).max().unwrap_or(0);
        for line in &mut out[i..j] {
            if let Some((c, _)) = &mut line.comment {
                *c = col;
            }
        }
        i = j;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse_file;

    fn fmt(src: &str) -> String {
        let (file, diags) = parse_file(FileId(0), src);
        assert!(diags.is_empty(), "{src}: {diags:?}");
        let out = format(src, &file);
        let (again, diags) = parse_file(FileId(0), &out);
        assert!(diags.is_empty(), "formatted:\n{out}\n{diags:?}");
        assert!(same_tree(&file, &again), "the tree changed:\n{out}");
        assert_eq!(format(&out, &again), out, "not idempotent");
        out
    }

    #[test]
    fn spaces_operators_and_lists() {
        assert_eq!(fmt("def main\n  x = 1+2*3\n  p(x,x)\nend\n"), "def main\n  x = 1 + 2 * 3\n  p(x, x)\nend\n");
        assert_eq!(fmt("def f( a:Int ,b : Int)->Int\n  a-b\nend\n"), "def f(a: Int, b: Int) -> Int\n  a - b\nend\n");
    }

    #[test]
    fn keeps_spaces_that_change_the_parse() {
        let src = "def main\n  foo -1\n  puts [1, 2]\n  x = [3]\n  p x[0]\nend\n";
        assert_eq!(fmt(src), src);
    }

    #[test]
    fn indents_blocks() {
        assert_eq!(
            fmt("def main\nx = [1, 2].map do |v|\nv * 2\nend\nif x.size > 1\nputs x\nelse\nputs 0\nend\nend\n"),
            "def main\n  x = [1, 2].map do |v|\n    v * 2\n  end\n  if x.size > 1\n    puts x\n  else\n    puts 0\n  end\nend\n"
        );
    }

    #[test]
    fn empty_and_comment_only_files() {
        assert_eq!(fmt(""), "");
        assert_eq!(fmt("\n\n  \n"), "");
        assert_eq!(fmt("\n\n#only\n\n\n#  comments  \n\n"), "# only\n\n#  comments\n");
    }

    #[test]
    fn line_ends_tabs_and_semicolons() {
        assert_eq!(
            fmt("def main\r\n\tx = 1 ;y = 2\r\n\tp x,\ty\r\nend\r\n"),
            "def main\n  x = 1; y = 2\n  p x, y\nend\n"
        );
    }

    #[test]
    fn operator_methods() {
        let src = "struct V\n  x: Int\n\n  def -\n    V.new(x: -@x)\n  end\n\n  def []=(i: Int, v: Int)\n    @x = v\n  end\n\n  def ==(o: V) -> Bool = @x == o.x\nend\n";
        assert_eq!(fmt(src), src);
        assert_eq!(
            fmt("struct V\n  x: Int\n\n  def -\n      V.new(x: -@x)\n  end\nend\n"),
            "struct V\n  x: Int\n\n  def -\n    V.new(x: -@x)\n  end\nend\n"
        );
    }

    #[test]
    fn splices_keep_their_text_and_comments() {
        let src =
            "macro def m(xs: []Code) -> Code\n  quote do\n    p #{xs.map { |x| x } # each\n       .size}\n  end\nend\n";
        assert_eq!(fmt(src), src);
    }
}

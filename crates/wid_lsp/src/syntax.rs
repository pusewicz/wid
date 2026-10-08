//! What the text around the cursor says, read from the buffer as it is
//! now, even mid-edit: what completion is asked for (`value.`, `@`, a
//! name), the receiver before a `.`, the locals in scope, and the call
//! whose arguments the cursor is in. Everything here is a pure function of
//! the text (and its syntax tree), so it works when the buffer doesn't
//! parse and no check has seen it yet.

use std::ops::Range;

use wid_diagnostics::{FileId, Span};
use wid_syntax::ast::{self, Callee, Cond, ExprKind as E, FnBody, ItemKind, Stmt, StmtKind};
use wid_syntax::lexer::lex;
use wid_syntax::token::{Keyword, TokenKind};

/// What completion is asked for at a position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Context {
    /// A member after `receiver.` or `receiver&.`: the receiver's bytes,
    /// and where the name being typed starts.
    Member {
        /// The receiver, as written before the `.`.
        receiver: Range<usize>,
        /// The start of the name being typed (the cursor, when none is).
        start: usize,
    },
    /// A field of `self` after `@`.
    Ivar {
        /// The start of the name being typed, just after the `@`.
        start: usize,
    },
    /// A name without a receiver.
    Name {
        /// The start of the name being typed.
        start: usize,
    },
    /// Nothing a name could complete: a comment, a string, a number, a
    /// symbol, or the name a declaration introduces.
    Nothing,
}

/// Bytes of a name: ASCII letters, digits and `_`, and every byte of a
/// non-ASCII character.
fn is_name_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

/// Whether `offset` is inside a comment or a string literal (outside its
/// interpolations).
fn in_comment_or_string(text: &str, offset: usize) -> bool {
    let lexed = lex(FileId(0), text);
    let offset = offset as u32;
    let comment = lexed.comments.iter().any(|c| c.span.start < offset && offset <= c.span.end);
    let string = lexed.tokens.iter().any(|t| {
        matches!(t.kind, TokenKind::Str(_) | TokenKind::StrBegin | TokenKind::StrText(_) | TokenKind::StrEnd)
            && t.span.start < offset
            && offset < t.span.end
    });
    comment || string
}

/// What completion is asked for at byte `offset` of `text`.
pub(crate) fn context(text: &str, offset: usize) -> Context {
    let offset = offset.min(text.len());
    if !text.is_char_boundary(offset) || in_comment_or_string(text, offset) {
        return Context::Nothing;
    }
    let bytes = text.as_bytes();
    let mut start = offset;
    while start > 0 && is_name_byte(bytes[start - 1]) {
        start -= 1;
    }
    // A number (`1.5`), not a name.
    if start < offset && bytes[start].is_ascii_digit() {
        return Context::Nothing;
    }
    match start.checked_sub(1).map(|i| bytes[i]) {
        Some(b'@') => Context::Ivar { start },
        Some(b'.') => {
            let dot = start - 1;
            // `0..n`: a range, so a name.
            if dot > 0 && bytes[dot - 1] == b'.' {
                return Context::Name { start };
            }
            let mut end = if dot > 0 && bytes[dot - 1] == b'&' { dot - 1 } else { dot };
            // `.method` starting a line continues the line above.
            let line_start = text[..end].rfind('\n').map_or(0, |i| i + 1);
            if text[line_start..end].trim().is_empty() {
                end = text[..line_start].trim_end().len();
            }
            let receiver = receiver_start(bytes, end)..end;
            if receiver.is_empty() {
                return Context::Nothing;
            }
            Context::Member { receiver, start }
        }
        // `:name`, a symbol; `$T`, a type parameter.
        Some(b':' | b'$') => Context::Nothing,
        _ if declares_name(&text[..start]) => Context::Nothing,
        _ => Context::Name { start },
    }
}

/// Whether the text before a name ends with a keyword that declares a new
/// name there (`def `, `struct `), where no existing name is wanted.
fn declares_name(before: &str) -> bool {
    let trimmed = before.trim_end();
    if trimmed.len() == before.len() {
        return false;
    }
    let word_start = trimmed.rfind(|c: char| !(c.is_alphanumeric() || c == '_')).map_or(0, |i| i + 1);
    matches!(&trimmed[word_start..], "def" | "struct" | "enum" | "union" | "module" | "overload")
        || trimmed.ends_with("def self.")
}

/// Where the receiver that ends at byte `end` starts: a chain of names,
/// calls, indexes, `@field`s, strings and dereferences joined by `.` or
/// `&.` (`a.b(c)[0]^`). It equals `end` when there is none.
pub(crate) fn receiver_start(bytes: &[u8], end: usize) -> usize {
    let mut i = end;
    loop {
        let piece = i;
        // Postfix groups and dereferences, read backwards.
        while i > 0 {
            match bytes[i - 1] {
                b')' | b']' => match opener(bytes, i - 1) {
                    Some(open) => i = open,
                    None => return piece.max(i),
                },
                b'^' => i -= 1,
                _ => break,
            }
        }
        // The atom: a name (maybe `@name` or `name?`), or a string.
        let before_atom = i;
        if i > 1 && matches!(bytes[i - 1], b'?' | b'!') && is_name_byte(bytes[i - 2]) {
            i -= 1;
        }
        let name_end = i;
        while i > 0 && is_name_byte(bytes[i - 1]) {
            i -= 1;
        }
        if i < name_end {
            if i > 0 && bytes[i - 1] == b'@' {
                i -= 1;
            }
        } else {
            i = before_atom;
            if i > 0 && matches!(bytes[i - 1], b'"' | b'\'') {
                let quote = bytes[i - 1];
                match bytes[..i - 1].iter().rposition(|&b| b == quote) {
                    Some(open) => i = open,
                    None => return i,
                }
            }
        }
        if i == piece {
            return i;
        }
        // A chain goes on through `.` or `&.`, not `..`.
        if i > 0 && bytes[i - 1] == b'.' && !(i > 1 && bytes[i - 2] == b'.') {
            i -= 1;
            if i > 0 && bytes[i - 1] == b'&' {
                i -= 1;
            }
            continue;
        }
        return i;
    }
}

/// The `(` or `[` that the `)` or `]` at `close` closes.
fn opener(bytes: &[u8], close: usize) -> Option<usize> {
    let mut depth = 0usize;
    let mut i = close + 1;
    while i > 0 {
        i -= 1;
        match bytes[i] {
            b')' | b']' => depth += 1,
            b'(' | b'[' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            b'\n' if depth > 8 => return None,
            _ => {}
        }
    }
    None
}

/// A local variable or parameter visible at a position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Local {
    /// Its name.
    pub(crate) name: String,
    /// Where it is declared: the name, or for a method's or proc's
    /// parameter, the whole parameter, as the checker records bindings.
    pub(crate) binding: Span,
    /// Whether it is a parameter (of a method, proc or block).
    pub(crate) parameter: bool,
    /// The type written where it is declared (`x: Ball = …`,
    /// `by: Int`), as written.
    pub(crate) written: Option<String>,
}

/// The locals visible at byte `offset` of a parsed file: the parameters
/// of the method (or proc) around it, the block and loop variables of the
/// blocks and loops around it, and the variables declared before it in
/// the bodies around it. A variable comes once, where it is first
/// declared.
pub(crate) fn locals_at(file: &ast::File, text: &str, offset: usize) -> Vec<Local> {
    let mut finder = Locals { text, offset: offset as u32, out: Vec::new() };
    finder.items(&file.items);
    finder.out
}

struct Locals<'t> {
    text: &'t str,
    offset: u32,
    out: Vec<Local>,
}

impl Locals<'_> {
    /// Whether the cursor is inside `span`, its end included.
    fn inside(&self, span: Span) -> bool {
        span.start < self.offset && self.offset <= span.end
    }

    fn written(&self, span: Span) -> String {
        self.text.get(span.start as usize..span.end as usize).unwrap_or_default().to_string()
    }

    fn add(&mut self, name: &ast::Ident, binding: Span, parameter: bool, written: Option<String>) {
        let text = name.as_str();
        if text.is_empty() || text.starts_with("#{") || self.out.iter().any(|l| l.name == text) {
            return;
        }
        self.out.push(Local { name: text.to_string(), binding, parameter, written });
    }

    fn items(&mut self, items: &[ast::Item]) {
        for item in items {
            if !self.inside(item.span) {
                continue;
            }
            match &item.kind {
                ItemKind::Def(f) => self.def(f),
                ItemKind::Struct(s) => self.items(&s.body),
                ItemKind::Enum(e) => self.items(&e.body),
                ItemKind::Module(m) => self.items(&m.body),
                ItemKind::Extend(x) => self.items(&x.body),
                ItemKind::ComptimeIf(c) => {
                    self.items(&c.then);
                    self.items(&c.else_);
                }
                _ => {}
            }
        }
    }

    fn params(&mut self, params: &[ast::Param]) {
        for p in params {
            let written = self.written(p.ty.span);
            self.add(&p.name, p.span, true, Some(written));
        }
    }

    fn def(&mut self, f: &ast::FnDecl) {
        self.params(&f.params);
        if let Some(b) = &f.block {
            let written = self.written(b.ty.span);
            self.add(&b.name, b.span, true, Some(written));
        }
        match &f.body {
            FnBody::Block(stmts) => self.stmts(stmts),
            FnBody::Expr(e) => self.expr(e),
        }
    }

    /// The variables a body declares before the cursor, and those of the
    /// statement the cursor is in.
    fn stmts(&mut self, stmts: &[Stmt]) {
        for s in stmts {
            if s.span.start >= self.offset {
                break;
            }
            if self.inside(s.span) {
                self.inner(s);
                break;
            }
            self.declared(s);
        }
    }

    /// The variables a statement declares in the body it is in.
    fn declared(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Decl { names, ty, .. } => {
                let written = self.written(ty.span);
                for name in names {
                    self.add(name, name.span, false, Some(written.clone()));
                }
            }
            StmtKind::Assign { targets, op: None, .. } => {
                for t in targets {
                    if let E::Ident(name) = &t.kind {
                        self.add(&ast::Ident { name: *name, span: t.span }, t.span, false, None);
                    }
                }
            }
            StmtKind::Guard { names, .. } => {
                for name in names {
                    self.add(name, name.span, false, None);
                }
            }
            _ => {}
        }
    }

    /// The variables of the blocks inside the statement the cursor is in.
    fn inner(&mut self, s: &Stmt) {
        match &s.kind {
            StmtKind::Expr(e) => self.expr(e),
            StmtKind::Decl { value: Some(v), .. } => self.expr(v),
            StmtKind::Assign { targets, values, .. } => {
                for e in targets.iter().chain(values) {
                    self.expr(e);
                }
            }
            StmtKind::Return(values) => values.iter().for_each(|e| self.expr(e)),
            StmtKind::Break(Some(e)) | StmtKind::Next(Some(e)) => self.expr(e),
            StmtKind::Defer(body) => self.stmts(body),
            StmtKind::Guard { value, err, else_body, .. } => {
                if self.inside(value.span) {
                    self.expr(value);
                } else if self.offset > value.span.end {
                    if let Some(err) = err {
                        self.add(err, err.span, false, None);
                    }
                    self.stmts(else_body);
                }
            }
            _ => {}
        }
    }

    /// The branch of `branches` (each starting where its condition or
    /// first line does) that the cursor is in.
    fn branch(&self, branches: &[(u32, &[Stmt])]) -> Option<usize> {
        branches.iter().rposition(|(start, _)| *start < self.offset)
    }

    fn cond(&mut self, cond: &Cond) -> Option<ast::Ident> {
        match cond {
            Cond::Expr(e) => {
                self.expr(e);
                None
            }
            Cond::Bind { name, value } => {
                self.expr(value);
                Some(*name)
            }
        }
    }

    fn expr(&mut self, e: &ast::Expr) {
        if !self.inside(e.span) {
            return;
        }
        match &e.kind {
            E::If(i) | E::ComptimeIf(i) => {
                let bound = self.cond(&i.cond);
                let cond_end = cond_span(&i.cond).end;
                let mut branches: Vec<(u32, &[Stmt])> = vec![(cond_end, &i.then)];
                for (c, body) in &i.elifs {
                    branches.push((cond_span(c).start, body));
                }
                if let Some(body) = &i.else_ {
                    let start = body.first().map_or(e.span.end, |s| s.span.start);
                    branches.push((start, body));
                }
                if let Some(k) = self.branch(&branches) {
                    if k == 0
                        && let Some(name) = bound
                    {
                        self.add(&name, name.span, false, None);
                    }
                    if k > 0
                        && let Some((c, _)) = i.elifs.get(k - 1)
                        && let Some(name) = self.cond(c)
                        && cond_span(c).end < self.offset
                    {
                        self.add(&name, name.span, false, None);
                    }
                    self.stmts(branches[k].1);
                }
            }
            E::While { cond, body, .. } => {
                let bound = self.cond(cond);
                if cond_span(cond).end < self.offset {
                    if let Some(name) = bound {
                        self.add(&name, name.span, false, None);
                    }
                    self.stmts(body);
                }
            }
            E::For(f) => {
                self.expr(&f.iter);
                if f.iter.span.end < self.offset {
                    for b in &f.bindings {
                        self.add(&b.name, b.name.span, false, None);
                    }
                    self.stmts(&f.body);
                }
            }
            E::Loop(body) | E::Comptime(body) => self.stmts(body),
            E::Case(c) => {
                if let Some(subject) = &c.subject {
                    self.expr(subject);
                }
                let mut branches: Vec<(u32, &[Stmt])> = c.whens.iter().map(|w| (w.span.start, &w.body[..])).collect();
                if let Some(body) = &c.else_ {
                    let start = body.first().map_or(e.span.end, |s| s.span.start);
                    branches.push((start, body));
                }
                for w in &c.whens {
                    w.patterns.iter().for_each(|p| self.expr(p));
                }
                if let Some(k) = self.branch(&branches) {
                    self.stmts(branches[k].1);
                }
            }
            E::Lambda(l) => {
                self.params(&l.params);
                self.stmts(&l.body);
            }
            E::Call(call) => {
                match &call.callee {
                    Callee::Method { recv, .. } => self.expr(recv),
                    Callee::Name(_) | Callee::IVar(_) => {}
                }
                for a in &call.args {
                    self.expr(&a.value);
                }
                if let Some(block) = &call.block
                    && self.inside(block.span)
                {
                    for p in &block.params {
                        self.add(&p.name, p.name.span, true, None);
                    }
                    self.stmts(&block.body);
                }
            }
            E::Member { recv, .. } => self.expr(recv),
            E::Index { recv, args } => {
                self.expr(recv);
                args.iter().for_each(|a| self.expr(a));
            }
            E::Unary { expr, .. } | E::Deref(expr) | E::AddrOf(expr) | E::Paren(expr) => self.expr(expr),
            E::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            E::Ternary { cond, then, else_ } => {
                self.expr(cond);
                self.expr(then);
                self.expr(else_);
            }
            E::Range { lo, hi, .. } => {
                lo.iter().chain(hi).for_each(|e| self.expr(e));
            }
            E::Array(items) | E::Yield(items) => items.iter().for_each(|e| self.expr(e)),
            E::Str(parts) => {
                for part in parts {
                    if let ast::StrPart::Interp(e) = part {
                        self.expr(e);
                    }
                }
            }
            _ => {}
        }
    }
}

fn cond_span(cond: &Cond) -> Span {
    match cond {
        Cond::Expr(e) => e.span,
        Cond::Bind { name, value } => Span::new(name.span.file, name.span.start, value.span.end),
    }
}

/// The call whose argument list the cursor is in: where its callee (with
/// its receiver) is, and which argument the cursor is at, counting from 0,
/// or the name of a named argument there (`move(by: |)`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CallSite {
    /// The callee: `move` in `b.move(`, with the receiver before it.
    pub(crate) callee: Range<usize>,
    /// The receiver, when the callee has one.
    pub(crate) receiver: Option<Range<usize>>,
    /// The argument the cursor is at.
    pub(crate) argument: usize,
    /// The argument's name, when it is passed by name.
    pub(crate) named: Option<String>,
}

/// The call whose parentheses hold byte `offset` of `text`, read
/// backwards from the cursor: the innermost `(` that isn't closed before
/// it and follows a name.
pub(crate) fn call_at(text: &str, offset: usize) -> Option<CallSite> {
    let offset = offset.min(text.len());
    if !text.is_char_boundary(offset) || in_comment_or_string(text, offset) {
        return None;
    }
    let bytes = text.as_bytes();
    let mut depth = 0usize;
    let mut commas = 0usize;
    let mut arg_start = offset;
    let mut i = offset;
    let mut quote: Option<u8> = None;
    while i > 0 {
        i -= 1;
        let b = bytes[i];
        if let Some(q) = quote {
            if b == q {
                quote = None;
            }
            continue;
        }
        match b {
            b'"' | b'\'' => quote = Some(b),
            b')' | b']' | b'}' => depth += 1,
            b'[' | b'{' if depth > 0 => depth -= 1,
            b'(' if depth > 0 => depth -= 1,
            // A `[` or `{` the cursor is in isn't a call's.
            b'[' | b'{' => return None,
            b',' if depth == 0 => {
                if commas == 0 {
                    arg_start = i + 1;
                }
                commas += 1;
            }
            b'(' => {
                if commas == 0 {
                    arg_start = i + 1;
                }
                let name_end = i;
                let mut name_start = name_end;
                if name_start > 1 && matches!(bytes[name_start - 1], b'?' | b'!') {
                    name_start -= 1;
                }
                while name_start > 0 && is_name_byte(bytes[name_start - 1]) {
                    name_start -= 1;
                }
                if name_start == name_end || bytes[name_start].is_ascii_digit() {
                    return None;
                }
                let word = &text[name_start..name_end];
                if Keyword::from_ident(word).is_some() {
                    return None;
                }
                let receiver = (name_start > 0 && bytes[name_start - 1] == b'.').then(|| {
                    let mut end = name_start - 1;
                    if end > 0 && bytes[end - 1] == b'&' {
                        end -= 1;
                    }
                    receiver_start(bytes, end)..end
                });
                let receiver = receiver.filter(|r| !r.is_empty());
                let arg = text[arg_start..offset].trim_start();
                let named = arg.split_once(':').and_then(|(name, _)| {
                    let name = name.trim();
                    (!name.is_empty() && name.bytes().all(is_name_byte)).then(|| name.to_string())
                });
                return Some(CallSite { callee: name_start..name_end, receiver, argument: commas, named });
            }
            // The argument list goes on over a line that ends with `(` or
            // `,`; any other line ends the search.
            b'\n' if depth == 0 && !text[..i].trim_end().ends_with(['(', ',']) => return None,
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use wid_diagnostics::FileId;

    use super::{CallSite, Context, call_at, context, locals_at};

    /// The context at `|` in `text`, with the `|` removed.
    fn at(text: &str) -> (String, Context) {
        let offset = text.find('|').expect("a cursor");
        let text = text.replacen('|', "", 1);
        let found = context(&text, offset);
        (text, found)
    }

    fn receiver(text: &str) -> String {
        match at(text) {
            (text, Context::Member { receiver, .. }) => text[receiver].to_string(),
            (_, other) => panic!("{other:?}"),
        }
    }

    #[test]
    fn the_context_says_what_is_asked_for() {
        assert_eq!(receiver("  b.|\n"), "b");
        assert_eq!(receiver("  b.mo|\n"), "b");
        assert_eq!(receiver("  x = ball.pos.|"), "ball.pos");
        assert_eq!(receiver("  foo(a, b).bar[0].|"), "foo(a, b).bar[0]");
        assert_eq!(receiver("  @pos&.|"), "@pos");
        assert_eq!(receiver("  geo.|"), "geo");
        assert_eq!(receiver("  \"text\".si|"), "\"text\"");
        assert_eq!(receiver("  p^.|"), "p^");
        assert_eq!(receiver("  xs.empty?.|"), "xs.empty?");
        assert_eq!(receiver("  ball\n    .|"), "ball");
        assert_eq!(receiver("  (a + b).|"), "(a + b)");
        assert_eq!(at("  @|").1, Context::Ivar { start: 3 });
        assert_eq!(at("  @po|").1, Context::Ivar { start: 3 });
        assert_eq!(at("  pu|").1, Context::Name { start: 2 });
        assert_eq!(at("  x = |").1, Context::Name { start: 6 });
        assert_eq!(at("  for i in 0..|").1, Context::Name { start: 15 });
        assert_eq!(at("  x = 1.|").1, Context::Member { receiver: 6..7, start: 8 });
        assert_eq!(at("  x = 1.5|").1, Context::Nothing);
        assert_eq!(at("  # a comment b.|").1, Context::Nothing);
        assert_eq!(at("  puts \"a b.|\"").1, Context::Nothing);
        assert_eq!(at("  puts \"#{b.|}\"").1, Context::Member { receiver: 11..12, start: 13 });
        assert_eq!(at("  case d when :no|").1, Context::Nothing);
        assert_eq!(at("def mo|").1, Context::Nothing);
        assert_eq!(at("  .|").1, Context::Nothing);
    }

    #[test]
    fn locals_are_those_in_scope() {
        let src = "\
def main(count: Int, &blk: block(Int))
  a = 1
  if a > 0
    inner = 2
  end
  xs.each do |x|
    y = x
    CURSOR_BLOCK
  end
  for i in 0..count
    z = i
  end
  guard v = find(a) else
    return
  end
  w: Int = 3
  CURSOR_END
end

def other
  q = 1
end
";
        let names = |marker: &str| {
            let offset = src.find(marker).expect("a marker");
            let (file, _) = wid_syntax::parse_file(FileId(0), src);
            locals_at(&file, src, offset).into_iter().map(|l| l.name).collect::<Vec<_>>()
        };
        assert_eq!(names("CURSOR_BLOCK"), ["count", "blk", "a", "x", "y"]);
        assert_eq!(names("CURSOR_END"), ["count", "blk", "a", "v", "w"]);
        assert_eq!(names("inner = 2"), ["count", "blk", "a"]);
        assert_eq!(names("z = i"), ["count", "blk", "a", "i"]);
        assert_eq!(names("q = 1"), Vec::<String>::new());
        let (file, _) = wid_syntax::parse_file(FileId(0), src);
        let offset = src.find("CURSOR_END").expect("a marker");
        let locals = locals_at(&file, src, offset);
        let count = &locals[0];
        assert!(count.parameter);
        assert_eq!(count.written.as_deref(), Some("Int"));
        assert_eq!(&src[count.binding.start as usize..count.binding.end as usize], "count: Int");
    }

    #[test]
    fn the_call_around_the_cursor() {
        let call = |text: &str| {
            let offset = text.find('|').expect("a cursor");
            let text = text.replacen('|', "", 1);
            call_at(&text, offset).map(|c| {
                let callee = text[c.callee.clone()].to_string();
                let receiver = c.receiver.clone().map(|r| text[r].to_string());
                (callee, receiver, c.argument, c.named)
            })
        };
        assert_eq!(call("  b.move(|"), Some(("move".into(), Some("b".into()), 0, None)));
        assert_eq!(call("  clamp(1, f(2), |)"), Some(("clamp".into(), None, 2, None)));
        assert_eq!(call("  clamp(1, f(2|"), Some(("f".into(), None, 0, None)));
        assert_eq!(call("  Ball.new(pos: |"), Some(("new".into(), Some("Ball".into()), 0, Some("pos".into()))));
        assert_eq!(call("  xs[f(1), |"), None);
        assert_eq!(call("  if (a|"), None);
        assert_eq!(call("  puts x|"), None);
        let _ = CallSite { callee: 0..0, receiver: None, argument: 0, named: None };
    }
}

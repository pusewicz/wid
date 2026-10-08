//! Renders syntax back to source text: types, expressions and the
//! declaration line of an item (`def clamp(x: Int, lo: Int = 0) -> Int`,
//! `struct Pool($T, $N: Int)`), without bodies.
//!
//! The output reads like the source and depends only on the tree, plus the
//! source text of literals when a [`Context`] gives it (so `0xFF` and `'a'`
//! stay as written), which makes it stable enough for tools: `wid doc`
//! shows signatures with it, and `wid fmt` is meant to build on it.
//! Statement bodies are not printed: a construct that holds statements
//! (`if`, `case`, a block, `comptime do`) prints as its keywords around `…`,
//! unless its body is one expression that fits on the line.

use wid_diagnostics::Span;

use crate::ast::{
    Arg, Attribute, BlockParamDecl, Callee, Cond, ConstDecl, EnumDecl, Expr, ExprKind, FieldDecl, FnDecl, GenericArg,
    GenericParam, Item, ItemKind, Param, Stmt, StmtKind, StrPart, TypeExpr, TypeKind, UnionDecl,
};

/// What the printer can't read from the tree alone.
pub trait Context {
    /// The source text of `span`, used to print number and string literals
    /// as they were written.
    fn source(&self, _span: Span) -> Option<&str> {
        None
    }

    /// How to show a type a macro expansion spliced in
    /// ([`TypeKind::Spliced`]), which only the checker can name.
    fn spliced_type(&self, _id: u32) -> String {
        "<spliced type>".to_string()
    }
}

/// A context that knows nothing beyond the tree.
pub struct Plain;

impl Context for Plain {}

/// Renders syntax as source text. See the module docs.
pub struct Printer<'c> {
    cx: &'c dyn Context,
}

/// Renders a type with no source text at hand.
pub fn type_expr(ty: &TypeExpr) -> String {
    Printer::new(&Plain).ty(ty)
}

/// Renders an expression with no source text at hand.
pub fn expr(e: &Expr) -> String {
    Printer::new(&Plain).expr(e)
}

impl<'c> Printer<'c> {
    /// A printer that reads literals and spliced types through `cx`.
    pub fn new(cx: &'c dyn Context) -> Self {
        Printer { cx }
    }

    /// The declaration line of an item, without attributes, `private` or
    /// body: `def name(a: Int) -> Int`, `struct Name($T)`,
    /// `enum Dir : U8`, `union Shape = Circle | Rect`, `module Name`,
    /// `extend A, B`, `NAME: T = value`, `overload :name, :a, :b`,
    /// `include Name`, `name: T = default`. `None` for items that declare
    /// nothing (imports, `comptime if`, macro calls, splices, errors).
    pub fn item_header(&self, item: &Item) -> Option<String> {
        Some(match &item.kind {
            ItemKind::Def(f) => self.fn_signature(f),
            ItemKind::Struct(s) => format!("struct {}{}", s.name.as_str(), self.generics(&s.generics)),
            ItemKind::Enum(e) => self.enum_header(e),
            ItemKind::Union(u) => self.union_header(u),
            ItemKind::Module(m) => format!("module {}", m.name.as_str()),
            ItemKind::Extend(e) => {
                let targets: Vec<String> = e.targets.iter().map(|t| self.ty(t)).collect();
                format!("extend {}", targets.join(", "))
            }
            ItemKind::Const(c) => self.constant(c),
            ItemKind::Overload(o) => {
                let mut out = format!("overload :{}", o.name.as_str());
                for m in &o.members {
                    out.push_str(&format!(", :{}", m.as_str()));
                }
                out
            }
            ItemKind::Include(t) => format!("include {}", self.ty(t)),
            ItemKind::Field(f) => self.field(f),
            ItemKind::Import(_)
            | ItemKind::Cimport(_)
            | ItemKind::MacroCall(_)
            | ItemKind::ComptimeIf(_)
            | ItemKind::Splice(_)
            | ItemKind::Error => return None,
        })
    }

    /// `def name(params) -> R`, `def self.name`, `macro def name(…) -> Code`.
    pub fn fn_signature(&self, f: &FnDecl) -> String {
        let mut out = String::new();
        if f.is_macro {
            out.push_str("macro ");
        }
        out.push_str("def ");
        if f.is_static {
            out.push_str("self.");
        }
        out.push_str(f.name.as_str());
        let mut params: Vec<String> = f.params.iter().map(|p| self.param(p)).collect();
        if let Some(b) = &f.block {
            params.push(self.block_param(b));
        }
        if f.c_variadic.is_some() {
            params.push("...".to_string());
        }
        if !params.is_empty() {
            out.push('(');
            out.push_str(&params.join(", "));
            out.push(')');
        }
        if let Some(ret) = &f.ret {
            out.push_str(" -> ");
            out.push_str(&self.ty(ret));
        }
        out
    }

    /// `name: T = default`, `*names: T`.
    pub fn param(&self, p: &Param) -> String {
        let mut out = String::new();
        if p.splat {
            out.push('*');
        }
        out.push_str(p.name.as_str());
        out.push_str(": ");
        out.push_str(&self.ty(&p.ty));
        if let Some(d) = &p.default {
            out.push_str(" = ");
            out.push_str(&self.expr(d));
        }
        out
    }

    /// `&blk: block(T) -> R`.
    pub fn block_param(&self, b: &BlockParamDecl) -> String {
        format!("&{}: {}", b.name.as_str(), self.ty(&b.ty))
    }

    /// `($T, $N: Int)`, or nothing for a declaration without generics.
    pub fn generics(&self, generics: &[GenericParam]) -> String {
        if generics.is_empty() {
            return String::new();
        }
        let list: Vec<String> = generics
            .iter()
            .map(|g| match &g.ty {
                Some(t) => format!("${}: {}", g.name.as_str(), self.ty(t)),
                None => format!("${}", g.name.as_str()),
            })
            .collect();
        format!("({})", list.join(", "))
    }

    /// `enum Name : Backing`.
    pub fn enum_header(&self, e: &EnumDecl) -> String {
        match &e.backing {
            Some(b) => format!("enum {} : {}", e.name.as_str(), self.ty(b)),
            None => format!("enum {}", e.name.as_str()),
        }
    }

    /// `union Name($T) = A | B`.
    pub fn union_header(&self, u: &UnionDecl) -> String {
        let variants: Vec<String> = u.variants.iter().map(|v| self.ty(v)).collect();
        format!("union {}{} = {}", u.name.as_str(), self.generics(&u.generics), variants.join(" | "))
    }

    /// `NAME = value` or `NAME: T = value`; a value C provides (`---`) is
    /// left out: `RED: Color`.
    pub fn constant(&self, c: &ConstDecl) -> String {
        let mut out = c.name.as_str().to_string();
        if let Some(t) = &c.ty {
            out.push_str(": ");
            out.push_str(&self.ty(t));
        }
        if !matches!(c.value.kind, ExprKind::Uninit) {
            out.push_str(" = ");
            out.push_str(&self.expr(&c.value));
        }
        out
    }

    /// `name: T = default`, `using name: T`.
    pub fn field(&self, f: &FieldDecl) -> String {
        let mut out = String::new();
        if f.using {
            out.push_str("using ");
        }
        out.push_str(f.name.as_str());
        out.push_str(": ");
        out.push_str(&self.ty(&f.ty));
        if let Some(d) = &f.default {
            out.push_str(" = ");
            out.push_str(&self.expr(d));
        }
        out
    }

    /// One attribute: `extern("InitWindow")`, `c`, `size(8)`.
    pub fn attribute(&self, a: &Attribute) -> String {
        if a.args.is_empty() {
            return a.name.as_str().to_string();
        }
        let args: Vec<String> = a.args.iter().map(|e| self.expr(e)).collect();
        format!("{}({})", a.name.as_str(), args.join(", "))
    }

    /// An attribute list as written before a declaration: `@[extern("X"), c]`,
    /// or nothing.
    pub fn attributes(&self, attrs: &[Attribute]) -> String {
        if attrs.is_empty() {
            return String::new();
        }
        let list: Vec<String> = attrs.iter().map(|a| self.attribute(a)).collect();
        format!("@[{}]", list.join(", "))
    }

    /// Renders a type.
    pub fn ty(&self, t: &TypeExpr) -> String {
        match &t.kind {
            TypeKind::Path { segments, args } => {
                let mut out: Vec<&str> = Vec::new();
                for s in segments {
                    out.push(s.as_str());
                }
                let mut text = out.join(".");
                if !args.is_empty() {
                    let list: Vec<String> = args
                        .iter()
                        .map(|a| match a {
                            GenericArg::Type(t) => self.ty(t),
                            GenericArg::Expr(e) => self.expr(e),
                        })
                        .collect();
                    text.push('(');
                    text.push_str(&list.join(", "));
                    text.push(')');
                }
                text
            }
            TypeKind::Param(name) => format!("${}", name.as_str()),
            TypeKind::Pointer(inner) => format!("^{}", self.pointee(inner)),
            TypeKind::MultiPointer(inner) => format!("[^]{}", self.pointee(inner)),
            TypeKind::Array(len, elem) => format!("[{}]{}", self.expr(len), self.ty(elem)),
            TypeKind::Slice(elem) => format!("[]{}", self.ty(elem)),
            TypeKind::Dynamic(elem) => format!("[dynamic]{}", self.ty(elem)),
            TypeKind::Map(k, v) => format!("map[{}]{}", self.ty(k), self.ty(v)),
            TypeKind::Proc { params, ret, c_abi, variadic } => {
                let mut list: Vec<String> = params.iter().map(|p| self.ty(p)).collect();
                if *variadic {
                    list.push("...".to_string());
                }
                let mut out = if *c_abi { "@[c] proc".to_string() } else { "proc".to_string() };
                out.push('(');
                out.push_str(&list.join(", "));
                out.push(')');
                if let Some(r) = ret {
                    out.push_str(" -> ");
                    out.push_str(&self.ty(r));
                }
                out
            }
            TypeKind::Block { params, ret } => {
                let mut out = "block".to_string();
                if !params.is_empty() {
                    let list: Vec<String> = params.iter().map(|p| self.ty(p)).collect();
                    out.push('(');
                    out.push_str(&list.join(", "));
                    out.push(')');
                }
                if let Some(r) = ret {
                    out.push_str(" -> ");
                    out.push_str(&self.ty(r));
                }
                out
            }
            TypeKind::Optional(inner) => {
                let text = self.ty(inner);
                // `?` ends a name, a pointer or a closing parenthesis;
                // anything else is grouped first: `([]Int)?`.
                let bare = matches!(
                    inner.kind,
                    TypeKind::Path { .. }
                        | TypeKind::Param(_)
                        | TypeKind::Pointer(_)
                        | TypeKind::MultiPointer(_)
                        | TypeKind::Tuple(_)
                        | TypeKind::Spliced(_)
                        | TypeKind::Splice(_)
                        | TypeKind::Error
                );
                if bare { format!("{text}?") } else { format!("({text})?") }
            }
            TypeKind::Tuple(items) => {
                let list: Vec<String> = items.iter().map(|i| self.ty(i)).collect();
                format!("({})", list.join(", "))
            }
            TypeKind::Distinct(inner) => format!("distinct {}", self.ty(inner)),
            TypeKind::Matrix { rows, cols, elem } => {
                format!("matrix[{}, {}]{}", self.expr(rows), self.expr(cols), self.ty(elem))
            }
            TypeKind::Splice(_) => "#{…}".to_string(),
            TypeKind::Spliced(id) => self.cx.spliced_type(*id),
            TypeKind::Error => "<error>".to_string(),
        }
    }

    /// The type after `^` or `[^]`: an optional one is grouped, `^(T?)`,
    /// since `^T?` is an optional pointer.
    fn pointee(&self, inner: &TypeExpr) -> String {
        let text = self.ty(inner);
        if matches!(inner.kind, TypeKind::Optional(_)) { format!("({text})") } else { text }
    }

    /// Renders an expression on one line.
    pub fn expr(&self, e: &Expr) -> String {
        match &e.kind {
            ExprKind::Int(v) => self.literal(e.span).unwrap_or_else(|| v.to_string()),
            ExprKind::Float(v) => self.literal(e.span).unwrap_or_else(|| float_text(*v)),
            ExprKind::Str(parts) => self.literal(e.span).unwrap_or_else(|| self.string(parts)),
            ExprKind::Symbol(name) => format!(":{}", name.as_str()),
            ExprKind::Nil => "nil".to_string(),
            ExprKind::True => "true".to_string(),
            ExprKind::False => "false".to_string(),
            ExprKind::SelfRef => "self".to_string(),
            ExprKind::Zero => "{}".to_string(),
            ExprKind::Uninit => "---".to_string(),
            ExprKind::Array(items) => format!("[{}]", self.list(items)),
            ExprKind::Ident(name) | ExprKind::Const(name) => name.as_str().to_string(),
            ExprKind::IVar(name) => format!("@{}", name.as_str()),
            ExprKind::Type(t) => self.ty(t),
            ExprKind::Member { recv, name, safe } => {
                format!("{}{}{}", self.expr(recv), if *safe { "&." } else { "." }, name.as_str())
            }
            ExprKind::Call(call) => {
                let mut out = match &call.callee {
                    Callee::Name(name) => name.as_str().to_string(),
                    Callee::Method { recv, name, safe } => {
                        format!("{}{}{}", self.expr(recv), if *safe { "&." } else { "." }, name.as_str())
                    }
                };
                let args: Vec<String> = call.args.iter().map(|a| self.arg(a)).collect();
                if call.parens {
                    out.push('(');
                    out.push_str(&args.join(", "));
                    out.push(')');
                } else if !args.is_empty() {
                    out.push(' ');
                    out.push_str(&args.join(", "));
                }
                if let Some(block) = &call.block {
                    let params: Vec<String> = block
                        .params
                        .iter()
                        .map(|p| format!("{}{}", if p.by_ref { "&" } else { "" }, p.name.as_str()))
                        .collect();
                    let params = if params.is_empty() { String::new() } else { format!("|{}| ", params.join(", ")) };
                    out.push_str(&format!(" {{ {params}{} }}", self.body(&block.body)));
                }
                out
            }
            ExprKind::Index { recv, args } => format!("{}[{}]", self.expr(recv), self.list(args)),
            ExprKind::Unary { op, expr } => format!("{}{}", op.as_str(), self.expr(expr)),
            ExprKind::Binary { op, lhs, rhs } => {
                format!("{} {} {}", self.expr(lhs), op.as_str(), self.expr(rhs))
            }
            ExprKind::Ternary { cond, then, else_ } => {
                format!("{} ? {} : {}", self.expr(cond), self.expr(then), self.expr(else_))
            }
            ExprKind::Range { lo, hi, inclusive } => {
                let lo = lo.as_ref().map(|e| self.expr(e)).unwrap_or_default();
                let hi = hi.as_ref().map(|e| self.expr(e)).unwrap_or_default();
                format!("{lo}{}{hi}", if *inclusive { ".." } else { "..." })
            }
            ExprKind::If(i) => {
                let kw = if i.unless { "unless" } else { "if" };
                format!("{kw} {} … end", self.cond(&i.cond))
            }
            ExprKind::While { cond, until, .. } => {
                format!("{} {} … end", if *until { "until" } else { "while" }, self.cond(cond))
            }
            ExprKind::For(f) => {
                let names: Vec<String> = f
                    .bindings
                    .iter()
                    .map(|b| format!("{}{}", if b.by_ref { "&" } else { "" }, b.name.as_str()))
                    .collect();
                format!("for {} in {} … end", names.join(", "), self.expr(&f.iter))
            }
            ExprKind::Loop(_) => "loop do … end".to_string(),
            ExprKind::Case(c) => match &c.subject {
                Some(s) => format!("case {} … end", self.expr(s)),
                None => "case … end".to_string(),
            },
            ExprKind::Lambda(l) => {
                let params: Vec<String> = l.params.iter().map(|p| self.param(p)).collect();
                let ret = l.ret.as_ref().map(|r| format!(" -> {}", self.ty(r))).unwrap_or_default();
                format!("->({}){ret} {{ {} }}", params.join(", "), self.body(&l.body))
            }
            ExprKind::Yield(args) => {
                if args.is_empty() {
                    "yield".to_string()
                } else {
                    format!("yield {}", self.list(args))
                }
            }
            ExprKind::Deref(inner) => format!("{}^", self.expr(inner)),
            ExprKind::AddrOf(inner) => format!("&{}", self.expr(inner)),
            ExprKind::Comptime(stmts) => match single_expr(stmts) {
                Some(e) => format!("comptime {}", self.expr(e)),
                None => "comptime do … end".to_string(),
            },
            ExprKind::ComptimeIf(i) => format!("comptime if {} … end", self.cond(&i.cond)),
            ExprKind::Quote(_) => "quote do … end".to_string(),
            ExprKind::Splice(_) => "#{…}".to_string(),
            ExprKind::Paren(inner) => format!("({})", self.expr(inner)),
            ExprKind::Error => "<error>".to_string(),
        }
    }

    /// A literal's source text, when the context has it.
    fn literal(&self, span: Span) -> Option<String> {
        let text = self.cx.source(span)?;
        (!text.is_empty() && !text.contains('\n')).then(|| text.to_string())
    }

    /// A string literal rebuilt from its parts.
    fn string(&self, parts: &[StrPart]) -> String {
        let mut out = String::from("\"");
        for part in parts {
            match part {
                StrPart::Text(text) => {
                    let mut chars = text.chars().peekable();
                    while let Some(c) = chars.next() {
                        match c {
                            '"' => out.push_str("\\\""),
                            '\\' => out.push_str("\\\\"),
                            '\n' => out.push_str("\\n"),
                            '\t' => out.push_str("\\t"),
                            '\r' => out.push_str("\\r"),
                            '\0' => out.push_str("\\0"),
                            '#' if chars.peek() == Some(&'{') => out.push_str("\\#"),
                            c => out.push(c),
                        }
                    }
                }
                StrPart::Interp(e) => {
                    out.push_str("#{");
                    out.push_str(&self.expr(e));
                    out.push('}');
                }
            }
        }
        out.push('"');
        out
    }

    fn arg(&self, a: &Arg) -> String {
        let value = self.expr(&a.value);
        let value = if a.splat { format!("*{value}") } else { value };
        match &a.name {
            Some(name) => format!("{}: {value}", name.as_str()),
            None => value,
        }
    }

    fn list(&self, items: &[Expr]) -> String {
        items.iter().map(|e| self.expr(e)).collect::<Vec<_>>().join(", ")
    }

    fn cond(&self, c: &Cond) -> String {
        match c {
            Cond::Expr(e) => self.expr(e),
            Cond::Bind { name, value } => format!("{} = {}", name.as_str(), self.expr(value)),
        }
    }

    /// A body as one line: its expression when it is one, else `…`.
    fn body(&self, stmts: &[Stmt]) -> String {
        match single_expr(stmts) {
            Some(e) => self.expr(e),
            None if stmts.is_empty() => String::new(),
            None => "…".to_string(),
        }
    }
}

/// The expression a body consists of, when it is one expression statement.
fn single_expr(stmts: &[Stmt]) -> Option<&Expr> {
    match stmts {
        [Stmt { kind: StmtKind::Expr(e), .. }] => Some(e),
        _ => None,
    }
}

/// A float the way Wid writes it: always with a point or an exponent.
fn float_text(v: f64) -> String {
    let text = format!("{v:?}");
    if text.contains(['.', 'e', 'E']) || !v.is_finite() { text } else { format!("{text}.0") }
}

#[cfg(test)]
mod tests {
    use super::{Context, Printer};
    use crate::ast::ItemKind;
    use wid_diagnostics::{FileId, Span};

    struct Text<'a>(&'a str);

    impl Context for Text<'_> {
        fn source(&self, span: Span) -> Option<&str> {
            self.0.get(span.start as usize..span.end as usize)
        }
    }

    /// The declaration lines of every item in `src`, and of the members of
    /// the first type.
    fn headers(src: &str) -> Vec<String> {
        let (file, diags) = crate::parse_file(FileId(0), src);
        assert!(diags.is_empty(), "{diags:?}");
        let cx = Text(src);
        let p = Printer::new(&cx);
        let mut out = Vec::new();
        for item in &file.items {
            out.extend(p.item_header(item));
            let body = match &item.kind {
                ItemKind::Struct(s) => &s.body,
                ItemKind::Enum(e) => &e.body,
                ItemKind::Module(m) => &m.body,
                ItemKind::Extend(e) => &e.body,
                _ => continue,
            };
            for member in body {
                out.extend(p.item_header(member).map(|h| format!("  {h}")));
            }
        }
        out
    }

    #[test]
    fn declaration_lines_read_like_the_source() {
        let src = "\
struct Pool($T, $N: Int)
  items: [N]T
  using base: ^Entity
  speed: F32 = 120.0
  def push(x: T) -> Bool = true
  def self.create(allocator: Allocator = context.allocator) -> Pool(T, N)
  end
  def each(&blk: block(^T))
  end
  def -
  end
  def []=(i: Int, v: T)
  end
end
enum Dir : U8
  north
end
union Shape = Circle | Rect
Vec2 = [2]F32
MASK = 0xFF
SPACE: Rune = ' '
Callback = @[c] proc(C.int, ...) -> C.int?
Maybe = (proc(Int) -> Int)?
P = ^(Int?)
Q = ^Int?
S = []Int?
macro def counter(*names: Symbol) -> Code
  quote do
  end
end
def log(fmt: CString, ...) end
overload :clamp, :clamp_int, :clamp_f32
extend []$T, String
end
";
        assert_eq!(
            headers(src),
            [
                "struct Pool($T, $N: Int)",
                "  items: [N]T",
                "  using base: ^Entity",
                "  speed: F32 = 120.0",
                "  def push(x: T) -> Bool",
                "  def self.create(allocator: Allocator = context.allocator) -> Pool(T, N)",
                "  def each(&blk: block(^T))",
                "  def -",
                "  def []=(i: Int, v: T)",
                "enum Dir : U8",
                "union Shape = Circle | Rect",
                "Vec2 = [2]F32",
                "MASK = 0xFF",
                "SPACE: Rune = ' '",
                "Callback = @[c] proc(C.int, ...) -> C.int?",
                "Maybe = (proc(Int) -> Int)?",
                "P = ^(Int?)",
                "Q = ^Int?",
                "S = []Int?",
                "macro def counter(*names: Symbol) -> Code",
                "def log(fmt: CString, ...)",
                "overload :clamp, :clamp_int, :clamp_f32",
                "extend []$T, String",
            ]
        );
    }

    #[test]
    fn expressions_keep_their_shape() {
        let src = "\
A = (1 + 2) * -3
B = Vec2.new(x: 0.0, y: f(1, *xs))
C = \"tab\\there #{name}\"
D = comptime build_table(256)
E = [dynamic]Int.new
F = xs[1..2]
G = a ? b&.c : d^
";
        assert_eq!(
            headers(src),
            [
                "A = (1 + 2) * -3",
                "B = Vec2.new(x: 0.0, y: f(1, *xs))",
                "C = \"tab\\there #{name}\"",
                "D = comptime build_table(256)",
                "E = [dynamic]Int.new",
                "F = xs[1..2]",
                "G = a ? b&.c : d^",
            ]
        );
        let (file, _) = crate::parse_file(FileId(0), "X = \"a\\\"b\\n\"\nY = 2.5e3\n");
        let plain: Vec<String> =
            file.items.iter().filter_map(|i| super::Printer::new(&super::Plain).item_header(i)).collect();
        assert_eq!(plain, ["X = \"a\\\"b\\n\"", "Y = 2500.0"]);
    }
}

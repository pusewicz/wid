//! The abstract syntax tree produced by the parser.

use wid_diagnostics::{FileId, Span};

use crate::intern::Name;
use crate::token::Comment;

/// An identifier with its location.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Ident {
    /// The interned text.
    pub name: Name,
    /// Where it appears.
    pub span: Span,
}

impl Ident {
    /// Returns the identifier text.
    pub fn as_str(&self) -> &'static str {
        self.name.as_str()
    }

    /// For a name-position splice, the index of its expression in the
    /// innermost enclosing [`QuoteExpr::splices`]. See [`splice_name`].
    pub fn splice_index(&self) -> Option<u32> {
        splice_index(self.name)
    }
}

/// The placeholder name of the splice at `index` in a name position:
/// `#{index}`. Source text can't spell it, so it never collides with a real
/// name.
///
/// A splice where the grammar expects a name (`def #{name}`, `x.#{name}`,
/// `@#{name}`, `:#{name}`, `struct #{name}`, parameter, field, local,
/// block-parameter and loop-variable names, enum members, named arguments
/// and the callee of `#{name}(args)`) becomes this name in the [`Ident`],
/// [`ExprKind::IVar`] or [`ExprKind::Symbol`] that would hold the written
/// one; the node's span covers the whole `#{…}`. Expansion replaces it with
/// the spliced `Symbol`.
pub fn splice_name(index: u32) -> Name {
    Name::new(&format!("#{{{index}}}"))
}

/// The splice index of a placeholder made by [`splice_name`], or `None` for
/// any other name.
pub fn splice_index(name: Name) -> Option<u32> {
    name.as_str().strip_prefix("#{")?.strip_suffix('}')?.parse().ok()
}

/// One parsed source file.
#[derive(Clone, Debug)]
pub struct File {
    /// The file id in the source map.
    pub file: FileId,
    /// Top-level declarations.
    pub items: Vec<Item>,
    /// All comments, for the formatter.
    pub comments: Vec<Comment>,
}

impl File {
    /// The item that starts at byte `start`, searching `comptime if`
    /// branches too.
    pub fn item_at(&self, start: u32) -> Option<&Item> {
        fn find(items: &[Item], start: u32) -> Option<&Item> {
            items.iter().find_map(|item| {
                if item.span.start == start && !matches!(item.kind, ItemKind::ComptimeIf(_)) {
                    return Some(item);
                }
                match &item.kind {
                    ItemKind::ComptimeIf(c) => find(&c.then, start).or_else(|| find(&c.else_, start)),
                    _ => None,
                }
            })
        }
        find(&self.items, start)
    }

    /// Every `import` and `cimport` item, including those inside `comptime
    /// if` branches, with whether it sits in such a branch.
    pub fn import_items(&self) -> Vec<(&Item, bool)> {
        fn walk<'a>(items: &'a [Item], conditional: bool, out: &mut Vec<(&'a Item, bool)>) {
            for item in items {
                match &item.kind {
                    ItemKind::Import(_) | ItemKind::Cimport(_) => out.push((item, conditional)),
                    ItemKind::ComptimeIf(c) => {
                        walk(&c.then, true, out);
                        walk(&c.else_, true, out);
                    }
                    _ => {}
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.items, false, &mut out);
        out
    }
}

/// `@[name]`, `@[name(args)]` or `@[name: value]`.
#[derive(Clone, Debug)]
pub struct Attribute {
    /// The attribute name.
    pub name: Ident,
    /// Positional arguments.
    pub args: Vec<Expr>,
    /// The whole attribute.
    pub span: Span,
}

/// A declaration at package, struct, enum, module or extend level.
#[derive(Clone, Debug)]
pub struct Item {
    /// What is declared.
    pub kind: ItemKind,
    /// The whole declaration.
    pub span: Span,
    /// Attributes written before the declaration.
    pub attrs: Vec<Attribute>,
    /// Whether `private` was written.
    pub private: bool,
    /// The doc comment directly above the declaration.
    pub doc: Option<String>,
}

impl Item {
    /// Returns true when the item carries the attribute `name`.
    pub fn has_attr(&self, name: &str) -> bool {
        self.attrs.iter().any(|a| a.name.as_str() == name)
    }

    /// Returns the attribute `name`, if present.
    pub fn attr(&self, name: &str) -> Option<&Attribute> {
        self.attrs.iter().find(|a| a.name.as_str() == name)
    }
}

/// The kinds of declarations.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub enum ItemKind {
    Import(Import),
    Cimport(Cimport),
    Def(Box<FnDecl>),
    Struct(Box<StructDecl>),
    Enum(Box<EnumDecl>),
    Union(Box<UnionDecl>),
    Module(Box<ModuleDecl>),
    Extend(Box<ExtendDecl>),
    Const(Box<ConstDecl>),
    Overload(OverloadDecl),
    /// `include Module` inside a struct or extend.
    Include(TypeExpr),
    /// A field inside a struct.
    Field(Box<FieldDecl>),
    /// A macro invocation used as a declaration: `attr_reader :x`,
    /// `attr_reader(:x)`, or qualified by a package, `lib.attr_reader :x`
    /// and `lib.make`. The expression is an [`ExprKind::Call`], an
    /// [`ExprKind::Ident`] or an [`ExprKind::Member`].
    MacroCall(Box<Expr>),
    /// `comptime if cond … else … end` choosing declarations.
    ComptimeIf(Box<ComptimeIfItem>),
    /// A splice standing alone on a line among declarations, like
    /// `#{methods}` in a `struct` body inside a `quote`; the index is into
    /// the innermost [`QuoteExpr::splices`]. Expansion replaces it with the
    /// spliced declarations (`Code` or `[]Code`), or, in an enum body, with
    /// members (`Symbol` or `[]Symbol`).
    Splice(u32),
    /// Placeholder left after a parse error.
    Error,
}

/// `import "core:fmt"` or `import "vendor:raylib", as: :rl`.
#[derive(Clone, Debug)]
pub struct Import {
    /// The import path text.
    pub path: String,
    /// Where the path string is.
    pub path_span: Span,
    /// The `as:` alias.
    pub alias: Option<Ident>,
}

/// `cimport "header.h", as: :name, …`.
#[derive(Clone, Debug)]
pub struct Cimport {
    /// The header path.
    pub header: String,
    /// Where the header string is.
    pub header_span: Span,
    /// Named options such as `as:`, `strip_prefix:`, `define:`.
    pub options: Vec<CimportOption>,
}

/// One `name: value` option of a `cimport`.
#[derive(Clone, Debug)]
pub struct CimportOption {
    /// The option name.
    pub name: Ident,
    /// The value.
    pub value: CimportValue,
    /// The whole option.
    pub span: Span,
}

/// The value of a `cimport` option.
#[derive(Clone, Debug)]
pub enum CimportValue {
    /// A plain expression: a string, a symbol or an array of them.
    Expr(Expr),
    /// `{Key: value, …}`. The values of `types:` are types
    /// ([`ExprKind::Type`]); the others are expressions.
    Hash {
        /// The entries, in source order.
        entries: Vec<CimportEntry>,
        /// The braces and everything between them.
        span: Span,
    },
}

/// One `Key: value` entry of a `cimport` option hash.
#[derive(Clone, Debug)]
pub struct CimportEntry {
    /// The key, as written (an identifier or a string's contents).
    pub key: String,
    /// Where the key is.
    pub key_span: Span,
    /// The value.
    pub value: Expr,
}

/// A function or method definition.
#[derive(Clone, Debug)]
pub struct FnDecl {
    /// The name; operators are interned as their symbol text.
    pub name: Ident,
    /// `def self.name` declares a type-level function.
    pub is_static: bool,
    /// `macro def` runs at compile time.
    pub is_macro: bool,
    /// Ordinary parameters.
    pub params: Vec<Param>,
    /// The `&blk: block(…)` parameter.
    pub block: Option<BlockParamDecl>,
    /// The declared return type.
    pub ret: Option<TypeExpr>,
    /// The body.
    pub body: FnBody,
    /// Span of the signature line, for diagnostics.
    pub sig_span: Span,
    /// Whether the body uses `yield`.
    pub yields: bool,
    /// Where `...` ends the parameter list: the method takes C variadic
    /// arguments. Only `@[extern]` methods may.
    pub c_variadic: Option<Span>,
}

/// A function body.
#[derive(Clone, Debug)]
pub enum FnBody {
    /// `def … end`.
    Block(Vec<Stmt>),
    /// Endless `def f = expr`.
    Expr(Box<Expr>),
}

/// One function parameter.
#[derive(Clone, Debug)]
pub struct Param {
    /// The parameter name.
    pub name: Ident,
    /// The declared type.
    pub ty: TypeExpr,
    /// The default value.
    pub default: Option<Expr>,
    /// `*names: T`, a variadic parameter: it collects the remaining
    /// positional arguments into a `[]T`. Only the last parameter of a
    /// `macro def` may be one, without a default; the parser reports any
    /// other (E0112). Outside a macro it recovers as the `names: []T` its
    /// fix suggests, with no default, keeping the flag so calls passing
    /// several arguments add no errors.
    pub splat: bool,
    /// The whole parameter.
    pub span: Span,
}

/// `&blk: block(T) -> R`.
#[derive(Clone, Debug)]
pub struct BlockParamDecl {
    /// The block name.
    pub name: Ident,
    /// The block type.
    pub ty: TypeExpr,
    /// The whole parameter.
    pub span: Span,
}

/// `struct Name($T) … end`.
#[derive(Clone, Debug)]
pub struct StructDecl {
    /// The struct name.
    pub name: Ident,
    /// Generic parameters.
    pub generics: Vec<GenericParam>,
    /// Fields, methods and other members.
    pub body: Vec<Item>,
}

/// `$T` or `$N: Int` in a generic declaration.
#[derive(Clone, Debug)]
pub struct GenericParam {
    /// The parameter name without `$`.
    pub name: Ident,
    /// For value parameters, their type.
    pub ty: Option<TypeExpr>,
    /// The whole parameter.
    pub span: Span,
}

/// A struct field.
#[derive(Clone, Debug)]
pub struct FieldDecl {
    /// The field name.
    pub name: Ident,
    /// The field type.
    pub ty: TypeExpr,
    /// The default used by `T.new`.
    pub default: Option<Expr>,
    /// `using name: T` promotes the field's members.
    pub using: bool,
}

/// `enum Name : Backing … end`.
#[derive(Clone, Debug)]
pub struct EnumDecl {
    /// The enum name.
    pub name: Ident,
    /// The backing integer type.
    pub backing: Option<TypeExpr>,
    /// The members in declaration order.
    pub members: Vec<EnumMember>,
    /// Methods and constants.
    pub body: Vec<Item>,
}

/// One enum member.
#[derive(Clone, Debug)]
pub struct EnumMember {
    /// The member name.
    pub name: Ident,
    /// An explicit value.
    pub value: Option<Expr>,
}

/// `union Name = A | B`.
#[derive(Clone, Debug)]
pub struct UnionDecl {
    /// The union name.
    pub name: Ident,
    /// Generic parameters.
    pub generics: Vec<GenericParam>,
    /// The variant types.
    pub variants: Vec<TypeExpr>,
}

/// `module Name … end`.
#[derive(Clone, Debug)]
pub struct ModuleDecl {
    /// The module name.
    pub name: Ident,
    /// Methods and constants to mix in.
    pub body: Vec<Item>,
}

/// `extend T1, T2 … end`.
#[derive(Clone, Debug)]
pub struct ExtendDecl {
    /// The extended types; may contain `$T` patterns.
    pub targets: Vec<TypeExpr>,
    /// Methods to add.
    pub body: Vec<Item>,
}

/// `NAME = value` or `NAME: T = value`.
#[derive(Clone, Debug)]
pub struct ConstDecl {
    /// The constant name.
    pub name: Ident,
    /// The declared type.
    pub ty: Option<TypeExpr>,
    /// The value.
    pub value: Expr,
}

/// `overload :name, :a, :b`.
#[derive(Clone, Debug)]
pub struct OverloadDecl {
    /// The overloaded name.
    pub name: Ident,
    /// The member functions.
    pub members: Vec<Ident>,
}

/// `comptime if … end` at declaration level.
#[derive(Clone, Debug)]
pub struct ComptimeIfItem {
    /// The compile-time condition.
    pub cond: Expr,
    /// Declarations used when the condition holds.
    pub then: Vec<Item>,
    /// Declarations used otherwise (`elsif` chains nest here).
    pub else_: Vec<Item>,
}

/// A type expression.
#[derive(Clone, Debug)]
pub struct TypeExpr {
    /// The shape of the type.
    pub kind: TypeKind,
    /// Where it appears.
    pub span: Span,
}

/// The shapes of type expressions.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub enum TypeKind {
    /// `Name`, `pkg.Name`, `Name(args)`.
    Path {
        segments: Vec<Ident>,
        args: Vec<GenericArg>,
    },
    /// `$T`.
    Param(Ident),
    Pointer(Box<TypeExpr>),
    MultiPointer(Box<TypeExpr>),
    Array(Box<Expr>, Box<TypeExpr>),
    Slice(Box<TypeExpr>),
    Dynamic(Box<TypeExpr>),
    Map(Box<TypeExpr>, Box<TypeExpr>),
    /// `proc(A) -> R`; `@[c] proc(A, ...)` uses the C calling convention
    /// and may end with C variadic arguments.
    Proc {
        params: Vec<TypeExpr>,
        ret: Option<Box<TypeExpr>>,
        c_abi: bool,
        variadic: bool,
    },
    Block {
        params: Vec<TypeExpr>,
        ret: Option<Box<TypeExpr>>,
    },
    Optional(Box<TypeExpr>),
    Tuple(Vec<TypeExpr>),
    Distinct(Box<TypeExpr>),
    Matrix {
        rows: Box<Expr>,
        cols: Box<Expr>,
        elem: Box<TypeExpr>,
    },
    /// `#{t}` where a type is expected, inside a `quote`; the index is into
    /// the innermost [`QuoteExpr::splices`].
    Splice(u32),
    /// A type a macro expansion spliced in, already resolved: the number is
    /// the checker's id for the type. The parser never produces it.
    Spliced(u32),
    Error,
}

/// A generic argument, which may be a type or a value.
#[derive(Clone, Debug)]
pub enum GenericArg {
    /// A type (or a constant name that sema resolves).
    Type(TypeExpr),
    /// A value expression.
    Expr(Expr),
}

/// A statement.
#[derive(Clone, Debug)]
pub struct Stmt {
    /// What the statement does.
    pub kind: StmtKind,
    /// Where it appears.
    pub span: Span,
    /// Attributes such as `@[no_bounds_check]`.
    pub attrs: Vec<Attribute>,
}

/// The kinds of statements.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub enum StmtKind {
    Expr(Expr),
    /// `x: T`, `x: T = v`, `x: T = ---`.
    Decl {
        names: Vec<Ident>,
        ty: TypeExpr,
        value: Option<Expr>,
        uninit: bool,
    },
    /// `a = b`, `a, b = f()`, `x += 1`.
    Assign {
        targets: Vec<Expr>,
        op: Option<BinOp>,
        values: Vec<Expr>,
    },
    Return(Vec<Expr>),
    Break(Option<Expr>),
    Next(Option<Expr>),
    Defer(Vec<Stmt>),
    /// `guard a, b = f() else |err| … end`.
    Guard {
        names: Vec<Ident>,
        value: Expr,
        err: Option<Ident>,
        else_body: Vec<Stmt>,
    },
    /// A declaration among statements. In a method it is an error; in a
    /// `quote` body it is part of the generated code (see [`QuoteExpr`]).
    Item(Box<Item>),
    Error,
}

/// An expression.
#[derive(Clone, Debug)]
pub struct Expr {
    /// The shape of the expression.
    pub kind: ExprKind,
    /// Where it appears.
    pub span: Span,
}

/// One segment of a string literal.
#[derive(Clone, Debug)]
pub enum StrPart {
    /// Literal text.
    Text(String),
    /// `#{expr}`.
    Interp(Expr),
}

/// The kinds of expressions.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub enum ExprKind {
    Int(u128),
    Float(f64),
    Str(Vec<StrPart>),
    Symbol(Name),
    Nil,
    True,
    False,
    SelfRef,
    /// `{}`, the zero value of the expected type.
    Zero,
    /// `---`, an explicitly uninitialized value.
    Uninit,
    Array(Vec<Expr>),
    /// A lowercase name: a local, a parameter or a zero-argument call.
    Ident(Name),
    /// An uppercase name: a constant or a type.
    Const(Name),
    /// `@name`.
    IVar(Name),
    /// A type written in expression position, like `[dynamic]Int`.
    Type(Box<TypeExpr>),
    /// `recv.name` or `recv&.name` without arguments.
    Member {
        recv: Box<Expr>,
        name: Ident,
        safe: bool,
    },
    Call(Box<Call>),
    Index {
        recv: Box<Expr>,
        args: Vec<Expr>,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
    },
    Ternary {
        cond: Box<Expr>,
        then: Box<Expr>,
        else_: Box<Expr>,
    },
    Range {
        lo: Option<Box<Expr>>,
        hi: Option<Box<Expr>>,
        inclusive: bool,
    },
    If(Box<IfExpr>),
    /// `while cond … end`; `while v = maybe` loops while the value is not nil.
    While {
        cond: Box<Cond>,
        body: Vec<Stmt>,
        until: bool,
    },
    For(Box<ForExpr>),
    Loop(Vec<Stmt>),
    Case(Box<CaseExpr>),
    Lambda(Box<Lambda>),
    Yield(Vec<Expr>),
    /// `p^`.
    Deref(Box<Expr>),
    /// `&x`.
    AddrOf(Box<Expr>),
    /// `comptime expr` or `comptime do … end`.
    Comptime(Vec<Stmt>),
    /// `comptime if … end`: only the branch whose condition holds at
    /// compile time is compiled.
    ComptimeIf(Box<IfExpr>),
    /// `quote do … end`.
    Quote(Box<QuoteExpr>),
    /// `#{expr}` in an expression or statement position, inside a `quote`;
    /// the index is into the innermost [`QuoteExpr::splices`]. Standing
    /// alone as a statement, a `[]Code` value inserts several statements.
    Splice(u32),
    Paren(Box<Expr>),
    Error,
}

/// `quote do … end`: code that a macro builds and returns.
///
/// The body is kept as a [`Stmt`] sequence because the context it expands
/// in is only known at the macro call. Each line is parsed as a statement,
/// except lines that can only start a declaration, which become
/// [`StmtKind::Item`]: `def`, `macro def`, `struct`, `enum`, `union`,
/// `module`, `extend`, `overload`, `include`, `import`, `cimport` and
/// `NAME = value` (with any attributes and `private`). A `comptime if` at
/// the top of the body keeps the same rule in its branches. Expanded as
/// statements, the body is used as is; expanded as declarations, each
/// [`StmtKind::Item`] gives its item, and a call or name statement
/// (`attr_reader :hp`) gives an [`ItemKind::MacroCall`].
///
/// Splices are numbered in source order per `quote`: each `#{expr}`
/// directly inside this quote (not inside a nested `quote`) appends `expr`
/// to [`QuoteExpr::splices`] and is replaced in the body by
/// [`ExprKind::Splice`], [`TypeKind::Splice`], [`ItemKind::Splice`] or, in
/// a name position, an [`Ident`] named by [`splice_name`]. A `quote`
/// written inside a splice expression has its own list.
#[derive(Clone, Debug)]
pub struct QuoteExpr {
    /// The generated code.
    pub body: Vec<Stmt>,
    /// The spliced expressions, which run in the macro.
    pub splices: Vec<Expr>,
}

/// A call with arguments and an optional block.
#[derive(Clone, Debug)]
pub struct Call {
    /// What is being called.
    pub callee: Callee,
    /// Positional and named arguments.
    pub args: Vec<Arg>,
    /// A literal block.
    pub block: Option<BlockArg>,
    /// Whether the arguments were parenthesized.
    pub parens: bool,
}

/// The target of a call.
#[derive(Clone, Debug)]
pub enum Callee {
    /// `name(…)` or `Name(…)`.
    Name(Ident),
    /// `recv.name(…)`.
    Method {
        /// The receiver.
        recv: Expr,
        /// The method name.
        name: Ident,
        /// `&.` safe navigation.
        safe: bool,
    },
}

/// One call argument.
#[derive(Clone, Debug)]
pub struct Arg {
    /// The parameter name for named arguments.
    pub name: Option<Ident>,
    /// The value.
    pub value: Expr,
    /// `*xs` spreads a slice into a splat parameter.
    pub splat: bool,
}

/// A literal block passed to a call.
#[derive(Clone, Debug)]
pub struct BlockArg {
    /// The block parameters.
    pub params: Vec<BlockParam>,
    /// The body.
    pub body: Vec<Stmt>,
    /// The whole block.
    pub span: Span,
}

/// A block or `for` binding.
#[derive(Clone, Copy, Debug)]
pub struct BlockParam {
    /// The bound name.
    pub name: Ident,
    /// `&x` binds by reference.
    pub by_ref: bool,
}

/// `if`/`unless` with optional `elsif` and `else`.
#[derive(Clone, Debug)]
pub struct IfExpr {
    /// The first condition.
    pub cond: Cond,
    /// The first branch.
    pub then: Vec<Stmt>,
    /// `elsif` branches.
    pub elifs: Vec<(Cond, Vec<Stmt>)>,
    /// The `else` branch.
    pub else_: Option<Vec<Stmt>>,
    /// True for `unless`.
    pub unless: bool,
}

/// A condition, optionally binding an unwrapped optional.
#[derive(Clone, Debug)]
pub enum Cond {
    /// A boolean or nil-testable expression.
    Expr(Expr),
    /// `if v = maybe`.
    Bind {
        /// The bound name.
        name: Ident,
        /// The optional value.
        value: Expr,
    },
}

/// `for x in xs … end`.
#[derive(Clone, Debug)]
pub struct ForExpr {
    /// One or two bindings.
    pub bindings: Vec<BlockParam>,
    /// The iterated value.
    pub iter: Expr,
    /// The body.
    pub body: Vec<Stmt>,
}

/// `case subject when … end`.
#[derive(Clone, Debug)]
pub struct CaseExpr {
    /// The matched value; absent for `case when cond`.
    pub subject: Option<Expr>,
    /// The branches.
    pub whens: Vec<When>,
    /// The fallback branch.
    pub else_: Option<Vec<Stmt>>,
}

/// One `when` branch.
#[derive(Clone, Debug)]
pub struct When {
    /// Values, ranges, symbols or types to match.
    pub patterns: Vec<Expr>,
    /// The body.
    pub body: Vec<Stmt>,
    /// The `when` line.
    pub span: Span,
}

/// `->(x: Int) -> Int { … }`.
#[derive(Clone, Debug)]
pub struct Lambda {
    /// The parameters.
    pub params: Vec<Param>,
    /// The return type.
    pub ret: Option<TypeExpr>,
    /// The body.
    pub body: Vec<Stmt>,
}

/// Binary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Pow,
    BitAnd,
    BitOr,
    BitXor,
    Shl,
    Shr,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Cmp,
    And,
    Or,
}

impl BinOp {
    /// Returns the operator as written in source.
    pub fn as_str(self) -> &'static str {
        match self {
            BinOp::Add => "+",
            BinOp::Sub => "-",
            BinOp::Mul => "*",
            BinOp::Div => "/",
            BinOp::Rem => "%",
            BinOp::Pow => "**",
            BinOp::BitAnd => "&",
            BinOp::BitOr => "|",
            BinOp::BitXor => "~",
            BinOp::Shl => "<<",
            BinOp::Shr => ">>",
            BinOp::Eq => "==",
            BinOp::Ne => "!=",
            BinOp::Lt => "<",
            BinOp::Le => "<=",
            BinOp::Gt => ">",
            BinOp::Ge => ">=",
            BinOp::Cmp => "<=>",
            BinOp::And => "&&",
            BinOp::Or => "||",
        }
    }

    /// Returns true for comparison operators that produce `Bool`.
    pub fn is_comparison(self) -> bool {
        matches!(self, BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge)
    }
}

/// Unary operators.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[allow(missing_docs)]
pub enum UnOp {
    Neg,
    Not,
    BitNot,
}

impl UnOp {
    /// Returns the operator as written in source.
    pub fn as_str(self) -> &'static str {
        match self {
            UnOp::Neg => "-",
            UnOp::Not => "!",
            UnOp::BitNot => "~",
        }
    }
}

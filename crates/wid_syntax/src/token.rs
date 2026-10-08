//! Token kinds produced by the lexer.

use wid_diagnostics::Span;

/// Reserved words.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[allow(missing_docs)]
pub enum Keyword {
    Def,
    End,
    If,
    Unless,
    Elsif,
    Else,
    Then,
    While,
    Until,
    For,
    In,
    Do,
    Loop,
    Return,
    Break,
    Next,
    Yield,
    Struct,
    Enum,
    Union,
    Module,
    Include,
    Extend,
    Using,
    Import,
    Cimport,
    Guard,
    Case,
    When,
    Defer,
    Nil,
    True,
    False,
    SelfKw,
    Comptime,
    Macro,
    Quote,
    Overload,
    Private,
}

impl Keyword {
    /// Every keyword, in declaration order, for tools that list them (the
    /// LSP's completion). `crates/wid_syntax/tests/editor_syntax.rs` checks
    /// that it holds every word [`Keyword::from_ident`] maps.
    pub const ALL: [Keyword; 39] = [
        Keyword::Def,
        Keyword::End,
        Keyword::If,
        Keyword::Unless,
        Keyword::Elsif,
        Keyword::Else,
        Keyword::Then,
        Keyword::While,
        Keyword::Until,
        Keyword::For,
        Keyword::In,
        Keyword::Do,
        Keyword::Loop,
        Keyword::Return,
        Keyword::Break,
        Keyword::Next,
        Keyword::Yield,
        Keyword::Struct,
        Keyword::Enum,
        Keyword::Union,
        Keyword::Module,
        Keyword::Include,
        Keyword::Extend,
        Keyword::Using,
        Keyword::Import,
        Keyword::Cimport,
        Keyword::Guard,
        Keyword::Case,
        Keyword::When,
        Keyword::Defer,
        Keyword::Nil,
        Keyword::True,
        Keyword::False,
        Keyword::SelfKw,
        Keyword::Comptime,
        Keyword::Macro,
        Keyword::Quote,
        Keyword::Overload,
        Keyword::Private,
    ];

    /// Maps identifier text to a keyword, if it is one.
    pub fn from_ident(text: &str) -> Option<Keyword> {
        Some(match text {
            "def" => Keyword::Def,
            "end" => Keyword::End,
            "if" => Keyword::If,
            "unless" => Keyword::Unless,
            "elsif" => Keyword::Elsif,
            "else" => Keyword::Else,
            "then" => Keyword::Then,
            "while" => Keyword::While,
            "until" => Keyword::Until,
            "for" => Keyword::For,
            "in" => Keyword::In,
            "do" => Keyword::Do,
            "loop" => Keyword::Loop,
            "return" => Keyword::Return,
            "break" => Keyword::Break,
            "next" => Keyword::Next,
            "yield" => Keyword::Yield,
            "struct" => Keyword::Struct,
            "enum" => Keyword::Enum,
            "union" => Keyword::Union,
            "module" => Keyword::Module,
            "include" => Keyword::Include,
            "extend" => Keyword::Extend,
            "using" => Keyword::Using,
            "import" => Keyword::Import,
            "cimport" => Keyword::Cimport,
            "guard" => Keyword::Guard,
            "case" => Keyword::Case,
            "when" => Keyword::When,
            "defer" => Keyword::Defer,
            "nil" => Keyword::Nil,
            "true" => Keyword::True,
            "false" => Keyword::False,
            "self" => Keyword::SelfKw,
            "comptime" => Keyword::Comptime,
            "macro" => Keyword::Macro,
            "quote" => Keyword::Quote,
            "overload" => Keyword::Overload,
            "private" => Keyword::Private,
            _ => return None,
        })
    }

    /// Returns the source text of the keyword.
    pub fn as_str(self) -> &'static str {
        match self {
            Keyword::Def => "def",
            Keyword::End => "end",
            Keyword::If => "if",
            Keyword::Unless => "unless",
            Keyword::Elsif => "elsif",
            Keyword::Else => "else",
            Keyword::Then => "then",
            Keyword::While => "while",
            Keyword::Until => "until",
            Keyword::For => "for",
            Keyword::In => "in",
            Keyword::Do => "do",
            Keyword::Loop => "loop",
            Keyword::Return => "return",
            Keyword::Break => "break",
            Keyword::Next => "next",
            Keyword::Yield => "yield",
            Keyword::Struct => "struct",
            Keyword::Enum => "enum",
            Keyword::Union => "union",
            Keyword::Module => "module",
            Keyword::Include => "include",
            Keyword::Extend => "extend",
            Keyword::Using => "using",
            Keyword::Import => "import",
            Keyword::Cimport => "cimport",
            Keyword::Guard => "guard",
            Keyword::Case => "case",
            Keyword::When => "when",
            Keyword::Defer => "defer",
            Keyword::Nil => "nil",
            Keyword::True => "true",
            Keyword::False => "false",
            Keyword::SelfKw => "self",
            Keyword::Comptime => "comptime",
            Keyword::Macro => "macro",
            Keyword::Quote => "quote",
            Keyword::Overload => "overload",
            Keyword::Private => "private",
        }
    }

    /// Returns the keyword in backticks, as diagnostics quote code.
    pub fn quoted(self) -> &'static str {
        match self {
            Keyword::Def => "`def`",
            Keyword::End => "`end`",
            Keyword::If => "`if`",
            Keyword::Unless => "`unless`",
            Keyword::Elsif => "`elsif`",
            Keyword::Else => "`else`",
            Keyword::Then => "`then`",
            Keyword::While => "`while`",
            Keyword::Until => "`until`",
            Keyword::For => "`for`",
            Keyword::In => "`in`",
            Keyword::Do => "`do`",
            Keyword::Loop => "`loop`",
            Keyword::Return => "`return`",
            Keyword::Break => "`break`",
            Keyword::Next => "`next`",
            Keyword::Yield => "`yield`",
            Keyword::Struct => "`struct`",
            Keyword::Enum => "`enum`",
            Keyword::Union => "`union`",
            Keyword::Module => "`module`",
            Keyword::Include => "`include`",
            Keyword::Extend => "`extend`",
            Keyword::Using => "`using`",
            Keyword::Import => "`import`",
            Keyword::Cimport => "`cimport`",
            Keyword::Guard => "`guard`",
            Keyword::Case => "`case`",
            Keyword::When => "`when`",
            Keyword::Defer => "`defer`",
            Keyword::Nil => "`nil`",
            Keyword::True => "`true`",
            Keyword::False => "`false`",
            Keyword::SelfKw => "`self`",
            Keyword::Comptime => "`comptime`",
            Keyword::Macro => "`macro`",
            Keyword::Quote => "`quote`",
            Keyword::Overload => "`overload`",
            Keyword::Private => "`private`",
        }
    }
}

/// The kind of a token. Literal payloads live in side tables or the source.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
#[allow(missing_docs)]
pub enum TokenKind {
    Int,
    Float,
    /// A complete string without interpolation; the payload indexes the
    /// lexer's cooked string table.
    Str(u32),
    StrBegin,
    /// A cooked text segment of an interpolated string.
    StrText(u32),
    InterpBegin,
    InterpEnd,
    StrEnd,
    /// `#{` outside a string literal: a splice, valid inside `quote`.
    SpliceBegin,
    /// The `}` that closes a splice. A zero-width one stands for a `}` that
    /// is missing (the lexer could not find one).
    SpliceEnd,
    /// The `@` of `@#{name}`, a field name splice; a [`TokenKind::SpliceBegin`]
    /// follows directly.
    AtSplice,
    /// The `:` of `:#{name}`, a symbol literal splice; a
    /// [`TokenKind::SpliceBegin`] follows directly.
    ColonSplice,
    /// `:name` or `:+`; the text after the colon is the symbol.
    Symbol,
    Ident,
    Const,
    /// `@name`.
    IVar,
    /// `$T`.
    TypeParam,
    Kw(Keyword),
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Dot,
    SafeNav,
    Colon,
    Arrow,
    FatArrow,
    Question,
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    StarStar,
    Amp,
    Pipe,
    Tilde,
    Caret,
    Shl,
    Shr,
    Bang,
    Eq,
    EqEq,
    NotEq,
    Lt,
    Le,
    Gt,
    Ge,
    Cmp,
    AndAnd,
    OrOr,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    PercentEq,
    StarStarEq,
    AmpEq,
    PipeEq,
    TildeEq,
    ShlEq,
    ShrEq,
    AndAndEq,
    OrOrEq,
    DotDot,
    DotDotDot,
    TripleDash,
    AtBracket,
    Newline,
    Eof,
}

impl TokenKind {
    /// Returns a short human description used in "expected X" messages.
    pub fn describe(self) -> &'static str {
        use TokenKind::*;
        match self {
            Int => "integer literal",
            Float => "float literal",
            Str(_) | StrBegin => "string literal",
            StrText(_) => "string text",
            InterpBegin => "`#{`",
            InterpEnd => "`}`",
            StrEnd => "end of string",
            SpliceBegin => "`#{`",
            SpliceEnd => "`}`",
            AtSplice => "`@#{`",
            ColonSplice => "`:#{`",
            Symbol => "symbol",
            Ident => "identifier",
            Const => "constant",
            IVar => "field reference",
            TypeParam => "type parameter",
            Kw(k) => k.quoted(),
            LParen => "`(`",
            RParen => "`)`",
            LBracket => "`[`",
            RBracket => "`]`",
            LBrace => "`{`",
            RBrace => "`}`",
            Comma => "`,`",
            Dot => "`.`",
            SafeNav => "`&.`",
            Colon => "`:`",
            Arrow => "`->`",
            FatArrow => "`=>`",
            Question => "`?`",
            Plus => "`+`",
            Minus => "`-`",
            Star => "`*`",
            Slash => "`/`",
            Percent => "`%`",
            StarStar => "`**`",
            Amp => "`&`",
            Pipe => "`|`",
            Tilde => "`~`",
            Caret => "`^`",
            Shl => "`<<`",
            Shr => "`>>`",
            Bang => "`!`",
            Eq => "`=`",
            EqEq => "`==`",
            NotEq => "`!=`",
            Lt => "`<`",
            Le => "`<=`",
            Gt => "`>`",
            Ge => "`>=`",
            Cmp => "`<=>`",
            AndAnd => "`&&`",
            OrOr => "`||`",
            PlusEq => "`+=`",
            MinusEq => "`-=`",
            StarEq => "`*=`",
            SlashEq => "`/=`",
            PercentEq => "`%=`",
            StarStarEq => "`**=`",
            AmpEq => "`&=`",
            PipeEq => "`|=`",
            TildeEq => "`~=`",
            ShlEq => "`<<=`",
            ShrEq => "`>>=`",
            AndAndEq => "`&&=`",
            OrOrEq => "`||=`",
            DotDot => "`..`",
            DotDotDot => "`...`",
            TripleDash => "`---`",
            AtBracket => "`@[`",
            Newline => "end of line",
            Eof => "end of file",
        }
    }

    /// Returns true when a newline directly after this token continues the
    /// expression instead of ending the statement.
    pub fn continues_line(self) -> bool {
        use TokenKind::*;
        matches!(
            self,
            Comma
                | Dot
                | SafeNav
                | Plus
                | Minus
                | Star
                | Slash
                | Percent
                | StarStar
                | Amp
                | Pipe
                | Tilde
                | Shl
                | Shr
                | Eq
                | EqEq
                | NotEq
                | Lt
                | Le
                | Gt
                | Ge
                | Cmp
                | AndAnd
                | OrOr
                | PlusEq
                | MinusEq
                | StarEq
                | SlashEq
                | PercentEq
                | StarStarEq
                | AmpEq
                | PipeEq
                | TildeEq
                | ShlEq
                | ShrEq
                | AndAndEq
                | OrOrEq
                | Arrow
                | FatArrow
                | LParen
                | LBracket
                | LBrace
                | AtBracket
                | Newline
        )
    }
}

/// One lexed token.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Token {
    /// What kind of token this is.
    pub kind: TokenKind,
    /// Where the token is in the source.
    pub span: Span,
    /// Whether whitespace directly precedes the token.
    pub space_before: bool,
}

/// A comment, kept for the formatter and documentation generator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    /// The span of the comment including the `#`.
    pub span: Span,
    /// The comment text without the leading `#` and one space.
    pub text: String,
    /// True when the comment is the only thing on its line.
    pub own_line: bool,
}

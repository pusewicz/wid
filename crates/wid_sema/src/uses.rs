//! What names and expressions resolve to, by source span: the declaration,
//! field, enum member, package, builtin type or local each name refers to,
//! and the type of every expression, binding and written type. `wid query
//! refs`, `calls` and `type` read it, and so do `wid lsp`'s hover and
//! go-to-definition.
//!
//! [`check_program_indexed`](crate::check_program_indexed) records it as it
//! checks, next to the [`Index`](crate::index::Index); `check_program`
//! records nothing. Code is recorded once however often it is lowered: a
//! generic method for each instance, a block method at each call, a field
//! default at each `new`. Only code the checker lowers is recorded: the
//! root package's methods, and the code of other packages that it reaches.
//!
//! Spans in code a macro generated are in the expansion's virtual file
//! ([`FileId::expansion`](wid_diagnostics::FileId::expansion)); the source
//! map that knows the expansions leads them back to the macro call.

use wid_diagnostics::Span;

use crate::index::SymbolId;
use crate::input::PackageId;

/// Every reference and every typed span of a checked program.
#[derive(Clone, Debug, Default)]
pub struct Uses {
    /// Every use of a name, sorted by span, then target and kind, without
    /// duplicates. A declaration's own name is a [`RefKind::Declaration`].
    pub refs: Vec<Ref>,
    /// The type of every expression, binding, parameter, written type and
    /// declaration name, sorted by span; one entry per span.
    pub types: Vec<Typed>,
    /// What the members of the types in [`Uses::types`] are, each once:
    /// [`Typed::members`] indexes this list.
    pub members: Vec<Members>,
}

/// A use of a name.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Ref {
    /// The name as written: for `p.heal(1)`, `heal`; for a qualified
    /// constant (`geo.MAX`), its last name; for an operator method, the
    /// whole operation.
    pub span: Span,
    /// What it names.
    pub target: RefTarget,
    /// How it is used.
    pub kind: RefKind,
}

/// What a name refers to.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefTarget {
    /// A declaration: a method, type, constant, module, macro or overload
    /// set.
    Symbol(SymbolId),
    /// Field `index` of a struct, in its [`Symbol::fields`](crate::index::Symbol::fields).
    /// A field promoted by `using` is its declaring struct's.
    Field {
        /// The struct that declares the field.
        owner: SymbolId,
        /// The field's position.
        index: usize,
    },
    /// Member `index` of an enum, in its
    /// [`Symbol::enum_members`](crate::index::Symbol::enum_members).
    EnumMember {
        /// The enum.
        owner: SymbolId,
        /// The member's position.
        index: usize,
    },
    /// An imported package, named by an import name.
    Package(PackageId),
    /// A builtin type written as a type: `Int`, `String`.
    Builtin(String),
    /// A local variable or parameter, by where it is declared: the name,
    /// or for a parameter the whole parameter (`amount: Int`).
    Local {
        /// Where it is declared.
        binding: Span,
        /// Whether it is a parameter of a method or proc.
        parameter: bool,
    },
}

/// How a name is used.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum RefKind {
    /// Where it is declared: its name in its own declaration, or for an
    /// import name, the `import` or `cimport` that binds it.
    Declaration,
    /// Its value is read: a variable, field, constant or enum member, a
    /// method named by `method(:f)` or in an `overload` list.
    Read,
    /// It is assigned: the target of `=`, `+=`, `||=` or a named argument
    /// of `T.new`.
    Write,
    /// It is called: a method, an overload set (and the member the call
    /// chose), a macro or an operator method.
    Call,
    /// It is used as a type: written in a type, `T.new`, `include`,
    /// `extend`.
    Type,
    /// A package is used through its import name: `geo` in `geo.Vec`.
    Import,
}

impl RefKind {
    /// The kind as JSON names it.
    pub fn as_str(self) -> &'static str {
        match self {
            RefKind::Declaration => "declaration",
            RefKind::Read => "read",
            RefKind::Write => "write",
            RefKind::Call => "call",
            RefKind::Type => "type",
            RefKind::Import => "import",
        }
    }
}

/// The type of a span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Typed {
    /// The expression, binding, written type or declaration name.
    pub span: Span,
    /// The type as Wid displays it: `^Ball?`, `proc(Int) -> Int`.
    pub ty: String,
    /// For code lowered once per generic instance with different types,
    /// each instance's type, in the order they were checked; empty when
    /// there is one.
    pub instances: Vec<String>,
    /// What the span is.
    pub kind: TypedKind,
    /// What a member access on a value of the type reaches, as an index
    /// into [`Uses::members`] (for the first instance's type); `None` for a
    /// type with nothing to reach, like `Void` or a type the checker
    /// couldn't work out.
    pub members: Option<usize>,
}

/// What `value.name` can reach on a value of a type, for completion: the
/// type is read through optionals and a pointer, as a member access reads
/// it (`^Ball?` reaches what `Ball` does).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Members {
    /// The struct, enum or union that declares the type (a generic
    /// struct's for one of its instances), whose fields, enum members and
    /// methods the index lists.
    pub decl: Option<SymbolId>,
    /// The kind of the type, which decides its builtin methods.
    pub shape: Shape,
    /// The `extend` blocks that add methods to the type, other than those
    /// that name `decl` (which the index lists): the extensions of a
    /// builtin type or of a pattern (`extend []$T`), in the order lookup
    /// tries them. An array or dynamic array also has those of its slice.
    pub extensions: Vec<SymbolId>,
}

/// The kinds of types that differ in their builtin methods.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Shape {
    /// `Bool`.
    Bool,
    /// An integer or float type.
    Number,
    /// `Rune`.
    Rune,
    /// `String`.
    String,
    /// An enum.
    Enum,
    /// A struct.
    Struct,
    /// A fixed array; `numeric` when its elements are numbers, so that up
    /// to four of them have swizzle names (`v.x`, `c.rgb`).
    Array {
        /// The number of elements.
        len: u64,
        /// Whether the elements are numbers.
        numeric: bool,
    },
    /// A slice, `[]T`.
    Slice,
    /// A dynamic array, `[dynamic]T`.
    Dynamic,
    /// A map, `map[K]V`.
    Map,
    /// A matrix.
    Matrix,
    /// A proc, which `.call` calls.
    Proc,
    /// Any other type: a union, a pointer to pointer, a C type, ….
    Other,
}

/// What a typed span is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum TypedKind {
    /// An expression.
    Expression,
    /// A call, written with arguments or a block, or a method called by
    /// name alone.
    Call,
    /// Where a local variable is declared.
    Local,
    /// A parameter of a method or proc (the whole parameter).
    Parameter,
    /// A field: its name in a struct, or in `T.new(name: …)`.
    Field,
    /// A written type.
    Type,
    /// The name in a method's, constant's or type's own declaration: its
    /// type is the method's proc type, the constant's type, or the type.
    Declaration,
}

impl TypedKind {
    /// The kind as JSON names it.
    pub fn as_str(self) -> &'static str {
        match self {
            TypedKind::Expression => "expression",
            TypedKind::Call => "call",
            TypedKind::Local => "local",
            TypedKind::Parameter => "parameter",
            TypedKind::Field => "field",
            TypedKind::Type => "type",
            TypedKind::Declaration => "declaration",
        }
    }
}

impl Uses {
    /// The uses of `target`, in span order.
    pub fn refs_to<'a>(&'a self, target: &'a RefTarget) -> impl Iterator<Item = &'a Ref> + 'a {
        self.refs.iter().filter(move |r| r.target == *target)
    }

    /// The typed entry at exactly `span`.
    pub fn typed(&self, span: Span) -> Option<&Typed> {
        self.types.binary_search_by(|t| t.span.cmp(&span)).ok().map(|i| &self.types[i])
    }

    /// What a member access reaches on a value of the type of `typed`.
    pub fn members_of(&self, typed: &Typed) -> Option<&Members> {
        self.members.get(typed.members?)
    }
}

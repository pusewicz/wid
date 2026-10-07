//! The typed, statement-oriented intermediate representation.
//!
//! Expressions here have no hidden control flow: anything that needs
//! sequencing (value-producing `if`/`case`, short-circuiting with statements on
//! the right, `defer`, block inlining) has already been lowered to statements
//! and temporaries. Code generation is a direct translation.

use wid_diagnostics::Span;
use wid_syntax::Name;

use crate::types::{Abi, CConv, TyId, TypeTable};

/// Identifies a function in [`Program::functions`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct FnId(pub u32);

/// Identifies a local variable within one function.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct LocalId(pub u32);

/// Identifies a global in [`Program::globals`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct GlobalId(pub u32);

/// Identifies a jump target within one function.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct LabelId(pub u32);

/// A whole checked program.
#[derive(Debug)]
pub struct Program {
    /// Every type used by the program.
    pub types: TypeTable,
    /// Every function, including instances of generic functions.
    pub functions: Vec<Function>,
    /// Package-level variables and materialized constants.
    pub globals: Vec<Global>,
    /// The entry point.
    pub main: Option<FnId>,
    /// The `@[test]` methods of a test build, in source order.
    pub tests: Vec<TestCase>,
    /// `core:testing`'s `run_test`, which the test `main` calls for each test.
    pub test_runner: Option<FnId>,
    /// Members of the builtin `Error` enum; value `i + 1` names `errors[i]`.
    pub errors: Vec<Name>,
    /// C headers to include, with the macros to define before each.
    pub c_includes: Vec<CInclude>,
    /// Extra C or C++ source files to compile and link.
    pub c_sources: Vec<std::path::PathBuf>,
    /// Libraries to link.
    pub link_libs: Vec<String>,
    /// Extra flags for the C compiler (from `pkg_config:` and similar).
    pub c_flags: Vec<String>,
    /// Extra flags for the linker.
    pub link_flags: Vec<String>,
    /// Files read by `embed`, which the build depends on.
    pub embedded_files: Vec<std::path::PathBuf>,
    /// Whether runtime checks are compiled in.
    pub checks: Checks,
    /// Whether this is a debug build.
    pub debug: bool,
}

/// One `@[test]` method.
#[derive(Clone, Debug)]
pub struct TestCase {
    /// The method name, which `wid test -filter:` matches.
    pub name: String,
    /// The test function.
    pub func: FnId,
    /// The declaration.
    pub span: wid_diagnostics::Span,
}

/// Which runtime checks the program was compiled with.
#[derive(Clone, Copy, Debug, Default)]
pub struct Checks {
    /// Bounds checks on indexing and slicing.
    pub bounds: bool,
    /// Integer overflow traps.
    pub overflow: bool,
}

/// A C header included verbatim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CInclude {
    /// The header as written in `#include`, with quotes or angle brackets.
    pub header: String,
    /// `NAME` or `NAME=value` macros defined before the include.
    pub defines: Vec<String>,
    /// A macro defined only once, before a dedicated implementation include.
    pub implement: Option<String>,
}

/// A package-level variable.
#[derive(Debug)]
pub struct Global {
    /// The C identifier.
    pub c_name: String,
    /// The type.
    pub ty: TyId,
    /// A constant initializer.
    pub init: Option<Expr>,
    /// Whether the global is read-only.
    pub constant: bool,
    /// Whether C defines it (a macro or an `extern` variable), so it is read
    /// by name and never defined here.
    pub foreign: bool,
    /// How C's value converts to the global's Wid type.
    pub c_conv: Option<CConv>,
    /// For `embed("file")`: the file whose bytes are the value, through
    /// C23 `#embed`.
    pub embed: Option<Embedded>,
    /// Holds compile-time-only data (like reflection results), so it is
    /// never emitted.
    pub comptime_only: bool,
}

/// A file whose bytes a global holds.
#[derive(Debug, Clone)]
pub struct Embedded {
    /// The absolute path, as `#embed` names it.
    pub path: std::path::PathBuf,
    /// The bytes, read when the program was checked.
    pub bytes: std::sync::Arc<[u8]>,
}

/// A function body ready for code generation.
#[derive(Debug)]
pub struct Function {
    /// The name shown in diagnostics and stack traces.
    pub display: String,
    /// The C identifier.
    pub c_name: String,
    /// Parameter locals, in order.
    pub params: Vec<LocalId>,
    /// The return type; `Void` for none.
    pub ret: TyId,
    /// Every local, indexed by [`LocalId`].
    pub locals: Vec<Local>,
    /// The body; `None` for functions implemented in C.
    pub body: Option<Block>,
    /// The calling convention.
    pub abi: Abi,
    /// The symbol name to export, from `@[export]`.
    pub export: Option<String>,
    /// For imported C functions, the C name to call.
    pub foreign: Option<String>,
    /// For a C function an included header declares: no prototype is
    /// emitted, and arguments and the result convert where Wid's C types
    /// differ from the header's.
    pub c_call: Option<CCall>,
    /// Takes C variadic arguments after its parameters.
    pub c_variadic: bool,
    /// Where the function is declared.
    pub span: Span,
    /// Uses values that exist only at compile time (like `Type`), so it can
    /// run only inside `comptime` and is never emitted.
    pub comptime_only: bool,
}

/// How a call to a header-declared C function converts its values.
#[derive(Clone, Debug, Default)]
pub struct CCall {
    /// One entry per parameter.
    pub params: Vec<Option<CConv>>,
    /// The result.
    pub ret: Option<CConv>,
}

/// A local variable.
#[derive(Clone, Debug)]
pub struct Local {
    /// The source name, if any.
    pub name: Option<Name>,
    /// The type.
    pub ty: TyId,
}

/// A sequence of statements forming a C scope.
#[derive(Clone, Debug, Default)]
pub struct Block {
    /// The statements in order.
    pub stmts: Vec<Stmt>,
}

/// A statement.
#[derive(Clone, Debug)]
pub enum Stmt {
    /// Declares a local; `None` zero-initializes it.
    Let {
        /// The local.
        local: LocalId,
        /// The initial value.
        init: Option<Expr>,
    },
    /// Declares a local without initializing it.
    LetUninit(LocalId),
    /// Stores `value` into the place `target`.
    Assign {
        /// A place expression.
        target: Expr,
        /// The value.
        value: Expr,
    },
    /// Evaluates an expression for its effects.
    Expr(Expr),
    /// A two-way branch.
    If {
        /// A `Bool` condition.
        cond: Expr,
        /// Taken when true.
        then: Block,
        /// Taken when false.
        else_: Block,
    },
    /// Loops until a `Goto` leaves it.
    Loop {
        /// The body.
        body: Block,
        /// Jumping here continues with the next iteration.
        continue_label: LabelId,
        /// Jumping here leaves the loop.
        break_label: LabelId,
    },
    /// A nested scope with a label placed right after it.
    Labeled {
        /// The body.
        body: Block,
        /// Jumping here leaves the block.
        end_label: LabelId,
    },
    /// A nested scope.
    Scope(Block),
    /// Jumps to a label.
    Goto(LabelId),
    /// Returns from the function.
    Return(Option<Expr>),
    /// Marks a point the program never reaches.
    Unreachable,
    /// Source position for debuggers and runtime errors.
    Line(Span),
    /// Runs `body` with a private copy of the context, so assignments to
    /// `context` fields last only until the end of the block.
    WithContext(Block),
}

/// An expression with its type.
#[derive(Clone, Debug)]
pub struct Expr {
    /// The shape.
    pub kind: ExprKind,
    /// The type.
    pub ty: TyId,
}

/// Numeric and pointer conversions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CastKind {
    /// Between numeric types, or to and from enums and distinct types.
    Numeric,
    /// Reinterprets a pointer.
    Pointer,
    /// Converts `^T` or `[^]T` to `T?`-style nullable form (no-op in C).
    NoOp,
}

/// Builtin operations implemented by the runtime or the code generator.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Builtin {
    /// Writes a value of any type, using RTTI for non-primitive values.
    Print { newline: bool, inspect: bool },
    /// Writes a string to stderr and aborts.
    Panic,
    /// Aborts when the condition is false.
    Assert,
    /// `size_of(T)`.
    SizeOf,
    /// Number of elements in a string, slice, dynamic array or map.
    Len,
    /// Pointer to the RTTI record of a type.
    TypeInfo,
    /// Starts a string builder: `(allocator)`.
    BuilderNew,
    /// Writes a value to a writer: `(^writer, value)`.
    Write { inspect: bool },
    /// The text a builder holds: `(^writer)`.
    BuilderString,
    /// `Context.default`.
    DefaultContext,
    /// `alloc(T)`: `(allocator)` returning `^T`.
    Alloc,
    /// `alloc([]T, n)`: `(n, allocator)` returning `[]T`.
    AllocSlice,
    /// `free(x)`: `(value, allocator)` for pointers, slices and strings, or
    /// `(^container)` for dynamic arrays and maps.
    Free,
    /// `free_all(allocator)`.
    FreeAll,
    /// Copies a string into a NUL-terminated C string: `(string, allocator)`.
    ToCString,
    /// Byte-wise string comparison: `(a, b)` returning a negative, zero or
    /// positive `I32`.
    StringCmp,
    /// Appends to a dynamic array: `(^dyn, value)`.
    DynPush,
    /// Inserts into a dynamic array: `(^dyn, index, value)`.
    DynInsert,
    /// Removes from a dynamic array: `(^dyn, index)`.
    DynRemove,
    /// Reserves capacity: `(^dyn, count)`.
    DynReserve,
    /// Appends every element of a slice: `(^dyn, slice)`.
    DynAppend,
    /// Sets the length, zero-filling growth: `(^dyn, count)`.
    DynResize,
    /// Stores into a map: `(^map, key, value)`.
    MapPut,
    /// Looks up a key: `(^map, key)` returning `^V?`.
    MapFind,
    /// Removes a key: `(^map, key)` returning `Bool`.
    MapRemove,
    /// The next occupied slot at or after an index: `(^map, i)` returning
    /// `Int` (-1 at the end).
    MapNext,
    /// The key in a slot: `(^map, i)` returning `^K`.
    MapKeyAt,
    /// The value in a slot: `(^map, i)` returning `^V`.
    MapValueAt,
    /// Decodes the rune at a byte offset: `(string, i, ^rune)` returning its
    /// width in bytes.
    Utf8Decode,
    /// Byte offset of a substring: `(string, needle)` returning `Int` (-1 if
    /// absent).
    StringFind,
    /// Views a `String`'s bytes as `[]U8`: `(string)`.
    StringBytes,
    /// Views a `[]U8` as a `String`: `(slice)`.
    BytesString,
    /// The pointer to a slice's first element, as a `[^]T`: `(slice)`.
    SliceData,
    /// Wraps a NUL-terminated `CString` as a `String`: `(cstring)`.
    CStringString,
    /// The `Location` of the builtin's span, in the function being emitted.
    CallerLocation,
    /// Checks an index: `(i, len)` returning `i`, or panicking.
    Bounds,
    /// The name of a `Type`: `(type)` returning `String`. Compile time only.
    TypeName,
    /// The size of a `Type` in bytes: `(type)` returning `Int`. Compile time only.
    TypeSize,
    /// The alignment of a `Type`: `(type)` returning `Int`. Compile time only.
    TypeAlign,
    /// The fields of a struct `Type`: `(type)` returning `[]FieldInfo`.
    /// Compile time only.
    TypeFields,
}

/// The shapes of expressions.
#[derive(Clone, Debug)]
#[allow(missing_docs)]
pub enum ExprKind {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    /// The empty value of an optional, pointer-like or union type.
    Nil,
    /// The zero value of the type.
    Zero,
    Local(LocalId),
    Global(GlobalId),
    /// A read-only global holding a value computed at compile time. Unlike
    /// `Global` it is not a place.
    ConstGlobal(GlobalId),
    /// A function used as a value.
    FnRef(FnId),
    /// `base.field`; `base` is a struct value or place.
    Field {
        base: Box<Expr>,
        index: u32,
    },
    Deref(Box<Expr>),
    AddrOf(Box<Expr>),
    /// Direct call of a Wid or C function.
    Call {
        func: FnId,
        args: Vec<Expr>,
    },
    /// Call through a proc value.
    CallIndirect {
        callee: Box<Expr>,
        args: Vec<Expr>,
        span: Span,
    },
    Builtin {
        op: Builtin,
        args: Vec<Expr>,
        span: Span,
    },
    Unary {
        op: UnaryOp,
        expr: Box<Expr>,
    },
    Binary {
        op: BinaryOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        span: Span,
    },
    Cast {
        kind: CastKind,
        expr: Box<Expr>,
    },
    /// A conditional between two pure expressions.
    Select {
        cond: Box<Expr>,
        then: Box<Expr>,
        else_: Box<Expr>,
    },
    /// The implicit context pointer.
    Context,
    /// A struct, tuple or fixed array built from its elements in order.
    Aggregate(Vec<Expr>),
    /// A member of the builtin `Error` enum.
    ErrorTag(Name),
    /// Wraps a value into its optional type.
    OptSome(Box<Expr>),
    /// True when an optional holds a value.
    OptIsSome(Box<Expr>),
    /// The value inside an optional known to hold one.
    OptGet(Box<Expr>),
    /// Builds a union value holding `variant` (0-based).
    UnionWrap {
        variant: u32,
        value: Box<Expr>,
    },
    /// The tag of a union: 0 for nil, `variant + 1` otherwise.
    UnionTag(Box<Expr>),
    /// The payload of a union known to hold `variant`.
    UnionGet {
        value: Box<Expr>,
        variant: u32,
    },
    /// Element `index` of a fixed array, slice, dynamic array, multi-pointer
    /// or string (a byte). `base` is evaluated more than once and must be pure.
    Index {
        base: Box<Expr>,
        index: Box<Expr>,
        checked: bool,
        span: Span,
    },
    /// A view of elements `lo` to `hi` (exclusive) of an array, slice,
    /// dynamic array or string. Operands must be pure.
    SliceOf {
        base: Box<Expr>,
        lo: Box<Expr>,
        hi: Box<Expr>,
        checked: bool,
        span: Span,
    },
}

/// Unary operators on primitives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum UnaryOp {
    Neg,
    Not,
    BitNot,
}

/// Binary operators on primitives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum BinaryOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
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
    /// Short-circuit `&&` with a pure right side.
    And,
    /// Short-circuit `||` with a pure right side.
    Or,
    /// Exponentiation.
    Pow,
    /// Three-way comparison returning -1, 0 or 1 as an `Int`; both operands
    /// must be pure.
    Cmp,
}

impl Expr {
    /// Builds an expression.
    pub fn new(kind: ExprKind, ty: TyId) -> Self {
        Expr { kind, ty }
    }

    /// Returns true for values that cannot change: literals and function
    /// references. Unlike pure expressions they may be read after other code
    /// (such as deferred blocks) has run.
    pub fn is_constant(&self) -> bool {
        matches!(
            self.kind,
            ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::Bool(_)
                | ExprKind::Str(_)
                | ExprKind::Nil
                | ExprKind::Zero
                | ExprKind::FnRef(_)
                | ExprKind::ErrorTag(_)
                | ExprKind::ConstGlobal(_)
        )
    }

    /// Returns true when evaluating the expression has no side effects, so it
    /// may be duplicated or reordered.
    pub fn is_pure(&self) -> bool {
        match &self.kind {
            ExprKind::Int(_)
            | ExprKind::Float(_)
            | ExprKind::Bool(_)
            | ExprKind::Str(_)
            | ExprKind::Nil
            | ExprKind::Zero
            | ExprKind::Local(_)
            | ExprKind::Global(_)
            | ExprKind::ConstGlobal(_)
            | ExprKind::FnRef(_)
            | ExprKind::ErrorTag(_)
            | ExprKind::Context => true,
            ExprKind::Aggregate(elems) => elems.iter().all(Expr::is_pure),
            ExprKind::Field { base, .. } => base.is_pure(),
            ExprKind::Deref(e)
            | ExprKind::AddrOf(e)
            | ExprKind::Unary { expr: e, .. }
            | ExprKind::Cast { expr: e, .. }
            | ExprKind::OptSome(e)
            | ExprKind::OptIsSome(e)
            | ExprKind::OptGet(e)
            | ExprKind::UnionTag(e)
            | ExprKind::UnionWrap { value: e, .. }
            | ExprKind::UnionGet { value: e, .. } => e.is_pure(),
            ExprKind::Index { base, index, checked, .. } => !checked && base.is_pure() && index.is_pure(),
            ExprKind::SliceOf { base, lo, hi, checked, .. } => {
                !checked && base.is_pure() && lo.is_pure() && hi.is_pure()
            }
            ExprKind::Binary { op, lhs, rhs, .. } => {
                !matches!(op, BinaryOp::Div | BinaryOp::Rem | BinaryOp::Pow) && lhs.is_pure() && rhs.is_pure()
            }
            ExprKind::Select { cond, then, else_ } => cond.is_pure() && then.is_pure() && else_.is_pure(),
            ExprKind::Builtin {
                op: Builtin::SizeOf | Builtin::TypeInfo | Builtin::TypeName | Builtin::TypeSize | Builtin::TypeAlign,
                ..
            } => true,
            ExprKind::Builtin { op: Builtin::Len, args, .. } => args.iter().all(Expr::is_pure),
            ExprKind::Call { .. } | ExprKind::CallIndirect { .. } | ExprKind::Builtin { .. } => false,
        }
    }
}

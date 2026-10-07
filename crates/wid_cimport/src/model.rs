//! The imported view of a C header set: plain data, no libclang handles.

use std::path::PathBuf;

/// The declarations of an imported header set.
#[derive(Clone, Debug, PartialEq)]
pub struct CModule {
    /// The main header, as libclang resolved it.
    pub header: PathBuf,
    /// The directory whose headers belong to the import: the canonical
    /// parent directory of the main header. Declarations from headers outside
    /// it (the C library, other packages) are not items, and types they
    /// declare appear as [`CType::Opaque`] or as plain scalars. When this is
    /// a directory shared by many libraries (`/usr/include`, the SDK, an
    /// `-isystem` directory), only headers at its top level are imported.
    pub root: PathBuf,
    /// The target the headers were parsed for.
    pub target: Target,
    /// Every declaration, in the order the preprocessor first saw it.
    pub items: Vec<Item>,
    /// The functions [`ImportRequest::probe_functions`](crate::ImportRequest)
    /// asked for that are declared outside the imported headers.
    pub probed: Vec<Function>,
}

impl CModule {
    /// Finds the record, enum or typedef that a [`CType::Named`] refers to.
    ///
    /// A typedef name may belong to a [`Record`] or [`Enum`] rather than to a
    /// [`Typedef`] item: `typedef struct { … } Color;` and
    /// `typedef struct Color Color;` produce one record whose `typedef_name`
    /// is `Color`.
    pub fn resolve(&self, named: &Named) -> Option<&Item> {
        self.items.iter().find(|item| match (&item.kind, named.kind) {
            (ItemKind::Typedef(typedef), NamedKind::Typedef) => typedef.name == named.name,
            (ItemKind::Record(record), NamedKind::Typedef) => record.typedef_name.as_deref() == Some(&named.name),
            (ItemKind::Enum(enumeration), NamedKind::Typedef) => {
                enumeration.typedef_name.as_deref() == Some(&named.name)
            }
            (ItemKind::Record(record), NamedKind::Struct) => {
                record.kind == RecordKind::Struct && record.tag.as_deref() == Some(&named.name)
            }
            (ItemKind::Record(record), NamedKind::Union) => {
                record.kind == RecordKind::Union && record.tag.as_deref() == Some(&named.name)
            }
            (ItemKind::Enum(enumeration), NamedKind::Enum) => enumeration.tag.as_deref() == Some(&named.name),
            _ => false,
        })
    }

    /// Finds the first item whose primary name is `name`.
    pub fn find(&self, name: &str) -> Option<&Item> {
        self.items.iter().find(|item| item.name() == Some(name))
    }
}

/// The target a translation unit was parsed for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    /// The normalised target triple, such as `arm64-apple-macosx15.0.0`.
    pub triple: String,
    /// The width of a data pointer in bits.
    pub pointer_bits: u32,
}

/// A position in a header.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Location {
    /// The header, as libclang named it (symbolic links are not resolved).
    pub file: PathBuf,
    /// The 1-based line.
    pub line: u32,
    /// The 1-based column, in bytes.
    pub column: u32,
}

/// One top-level declaration.
#[derive(Clone, Debug, PartialEq)]
pub struct Item {
    /// Where the declaration's name is written; for a record or enum that is
    /// declared before it is defined, the definition.
    pub location: Location,
    /// The comment documenting the declaration, verbatim with its comment
    /// markers: a `/** … */` or `///` comment before it, or a comment on the
    /// same line after it.
    pub doc: Option<String>,
    /// What the declaration is.
    pub kind: ItemKind,
}

impl Item {
    /// The name the declaration is referred to by: the typedef name of a
    /// record or enum if it has one, otherwise its tag. `None` only for an
    /// anonymous enum.
    pub fn name(&self) -> Option<&str> {
        match &self.kind {
            ItemKind::Function(function) => Some(&function.name),
            ItemKind::Record(record) => record.typedef_name.as_deref().or(record.tag.as_deref()),
            ItemKind::Enum(enumeration) => enumeration.typedef_name.as_deref().or(enumeration.tag.as_deref()),
            ItemKind::Typedef(typedef) => Some(&typedef.name),
            ItemKind::Global(global) => Some(&global.name),
            ItemKind::Macro(definition) => Some(&definition.name),
        }
    }
}

/// The kinds of top-level declaration.
#[derive(Clone, Debug, PartialEq)]
pub enum ItemKind {
    /// A function declaration or definition.
    Function(Function),
    /// A struct or union.
    Record(Record),
    /// An enum and its constants.
    Enum(Enum),
    /// A typedef that is not merged into a record or enum.
    Typedef(Typedef),
    /// A variable declared at file scope.
    Global(Global),
    /// A preprocessor macro.
    Macro(Macro),
}

/// A function.
#[derive(Clone, Debug, PartialEq)]
pub struct Function {
    /// The C name.
    pub name: String,
    /// The signature, with parameter names.
    pub sig: FnSig,
    /// Declared `inline`.
    pub is_inline: bool,
    /// Declared `static`, so each translation unit gets its own copy and
    /// there is no symbol to link against.
    pub is_static: bool,
}

/// A function type.
#[derive(Clone, Debug, PartialEq)]
pub struct FnSig {
    /// The parameters, after C's adjustment of array and function parameters
    /// to pointers.
    pub params: Vec<Param>,
    /// The return type.
    pub ret: CType,
    /// Takes `...` after the named parameters.
    pub variadic: bool,
    /// Has a prototype. Only false for old-style `f()` declarations, which
    /// exist when parsing with an `-std=` older than C23.
    pub prototyped: bool,
}

/// A function parameter.
#[derive(Clone, Debug, PartialEq)]
pub struct Param {
    /// The name, when the declaration gives one.
    pub name: Option<String>,
    /// The type.
    pub ty: CType,
    /// Qualifiers on the parameter itself, such as the `restrict` in
    /// `char *restrict s`.
    pub quals: Quals,
}

/// Whether a record is a struct or a union.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum RecordKind {
    /// A `struct`.
    Struct,
    /// A `union`.
    Union,
}

/// A struct or union.
#[derive(Clone, Debug, PartialEq)]
pub struct Record {
    /// Struct or union.
    pub kind: RecordKind,
    /// The tag, as in `struct Tag`. `None` for an anonymous record.
    pub tag: Option<String>,
    /// The typedef that names this record, for `typedef struct { … } Name;`
    /// and `typedef struct Name Name;`.
    pub typedef_name: Option<String>,
    /// The fields and layout, or `None` for an opaque (incomplete) record.
    pub body: Option<RecordBody>,
}

impl Record {
    /// Whether the record is only declared, never defined.
    pub fn is_opaque(&self) -> bool {
        self.body.is_none()
    }
}

/// The definition of a record.
#[derive(Clone, Debug, PartialEq)]
pub struct RecordBody {
    /// The fields, in declaration order, including unnamed bit-fields and
    /// anonymous members.
    pub fields: Vec<Field>,
    /// `sizeof` in bytes.
    pub size: u64,
    /// `alignof` in bytes.
    pub align: u64,
}

impl RecordBody {
    /// Whether the last field is a flexible array member (`T name[];`).
    pub fn has_flexible_array_member(&self) -> bool {
        self.fields.last().is_some_and(|field| matches!(&field.ty, CType::Array(array) if array.len.is_none()))
    }
}

/// A field of a record.
#[derive(Clone, Debug, PartialEq)]
pub struct Field {
    /// The name. `None` for an anonymous struct or union member (whose fields
    /// are accessed as if they were the parent's) and for unnamed bit-fields.
    pub name: Option<String>,
    /// The type. An anonymous member's type is [`CType::Record`].
    pub ty: CType,
    /// Qualifiers on the field itself.
    pub quals: Quals,
    /// The width of a bit-field.
    pub bit_width: Option<u32>,
    /// The offset in bits from the start of the record that declares it.
    pub offset_bits: u64,
    /// The comment documenting the field.
    pub doc: Option<String>,
}

/// An enum.
#[derive(Clone, Debug, PartialEq)]
pub struct Enum {
    /// The tag, as in `enum Tag`.
    pub tag: Option<String>,
    /// The typedef that names this enum, for `typedef enum { … } Name;` and
    /// `typedef enum Name Name;`.
    pub typedef_name: Option<String>,
    /// The integer type the compiler chose, or the one written after `:`.
    pub underlying: CType,
    /// The constants, in declaration order.
    pub constants: Vec<EnumConstant>,
}

/// An enumeration constant.
#[derive(Clone, Debug, PartialEq)]
pub struct EnumConstant {
    /// The C name.
    pub name: String,
    /// The value.
    pub value: i128,
    /// Where the constant is declared.
    pub location: Location,
    /// The comment documenting the constant.
    pub doc: Option<String>,
}

/// A typedef.
#[derive(Clone, Debug, PartialEq)]
pub struct Typedef {
    /// The C name.
    pub name: String,
    /// The aliased type. A callback typedef such as
    /// `typedef void (*Callback)(int code)` is a [`CType::FnPtr`] whose
    /// parameters carry their names.
    pub ty: CType,
    /// Qualifiers written on the aliased type, as in `typedef const int C;`.
    pub quals: Quals,
}

/// A variable at file scope.
#[derive(Clone, Debug, PartialEq)]
pub struct Global {
    /// The C name.
    pub name: String,
    /// The type.
    pub ty: CType,
    /// Qualifiers on the variable itself.
    pub quals: Quals,
    /// Declared `static`, so each translation unit gets its own copy.
    pub is_static: bool,
    /// Declared `thread_local` or `_Thread_local`.
    pub is_thread_local: bool,
}

/// A preprocessor macro.
#[derive(Clone, Debug, PartialEq)]
pub struct Macro {
    /// The C name.
    pub name: String,
    /// The replacement list, with whitespace between tokens collapsed.
    pub body: String,
    /// What the macro expands to.
    pub kind: MacroKind,
}

/// The shape of a macro.
#[derive(Clone, Debug, PartialEq)]
pub enum MacroKind {
    /// An object-like macro that expands to a C expression.
    Expr {
        /// The type of the expression in an initializer: arrays and
        /// functions decay to pointers, so a string macro is `char *`.
        ty: CType,
        /// The value, when the expression is a constant libclang can
        /// evaluate.
        value: Option<MacroValue>,
    },
    /// An object-like macro that does not expand to an expression: a
    /// keyword, attribute, type name, statement or fragment.
    Other,
    /// A macro that takes arguments. These are listed so the checker can name
    /// them in errors; they are never imported.
    FunctionLike {
        /// The parameter names, without the `...`.
        params: Vec<String>,
        /// Takes `...` (or a GNU named variadic parameter).
        variadic: bool,
    },
}

/// The value of a constant macro.
#[derive(Clone, Debug, PartialEq)]
pub enum MacroValue {
    /// An integer constant.
    Int(i128),
    /// A `bool` constant (`true`, `false` or a `bool`-typed expression).
    Bool(bool),
    /// A character constant such as `'a'`; the value is its `int` value.
    Char(i128),
    /// A floating-point constant.
    Float(f64),
    /// A narrow string literal's bytes, without the terminating NUL.
    Str(Vec<u8>),
}

/// The qualifiers on one level of a type.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Quals {
    /// `const`.
    pub is_const: bool,
    /// `volatile`.
    pub is_volatile: bool,
    /// `restrict`.
    pub is_restrict: bool,
}

/// A C type.
///
/// Qualifiers are not part of the type; each place a type appears (a field,
/// a parameter, a pointee, an array element) carries its own [`Quals`].
#[derive(Clone, Debug, PartialEq)]
pub enum CType {
    /// `void`.
    Void,
    /// `bool` or `_Bool`.
    Bool,
    /// Plain `char`, whose signedness depends on the target.
    Char {
        /// Whether `char` is signed on the target.
        signed: bool,
    },
    /// An integer type other than plain `char`.
    Int(IntType),
    /// A floating-point type.
    Float(FloatType),
    /// A pointer to data.
    Pointer(Box<PointerType>),
    /// An array.
    Array(Box<ArrayType>),
    /// A pointer to a function.
    FnPtr(Box<FnSig>),
    /// A function type that is not behind a pointer, as in
    /// `typedef void Handler(int);`.
    Function(Box<FnSig>),
    /// A record, enum or typedef declared in the imported headers.
    Named(Named),
    /// An anonymous struct or union defined in place, as a field's type.
    Record(Box<Record>),
    /// `_Atomic(T)`.
    Atomic(Box<CType>),
    /// A type that is declared outside the imported headers and is not a
    /// scalar (`FILE`, `va_list`, `struct tm`), or one with no Wid
    /// counterpart (vectors, `_Complex`, `_BitInt`). Carries the C spelling.
    Opaque(String),
}

/// An integer type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct IntType {
    /// The width in bits.
    pub bits: u32,
    /// Whether the type is signed.
    pub signed: bool,
    /// How C spells it: `int`, `unsigned long`, or the typedef name for
    /// types declared outside the imported headers, such as `size_t` and
    /// `uint8_t`.
    pub spelling: String,
}

/// A floating-point type.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FloatType {
    /// The storage width in bits (`long double` is 128 on x86-64 even though
    /// it holds 80 bits of precision).
    pub bits: u32,
    /// How C spells it: `float`, `double`, `long double`, `_Float16`, or a
    /// typedef name declared outside the imported headers.
    pub spelling: String,
}

/// A pointer to data.
#[derive(Clone, Debug, PartialEq)]
pub struct PointerType {
    /// The type pointed to.
    pub pointee: CType,
    /// Qualifiers on the pointee: `const char *` has `is_const`.
    pub pointee_quals: Quals,
}

/// An array.
#[derive(Clone, Debug, PartialEq)]
pub struct ArrayType {
    /// The element type.
    pub element: CType,
    /// Qualifiers on the elements.
    pub element_quals: Quals,
    /// The length, or `None` for `T[]` (incomplete arrays and flexible array
    /// members).
    pub len: Option<u64>,
}

/// A reference to a record, enum or typedef by name.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Named {
    /// Which C namespace the name lives in.
    pub kind: NamedKind,
    /// The name.
    pub name: String,
}

/// The namespace of a [`Named`] type.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum NamedKind {
    /// `struct Name`.
    Struct,
    /// `union Name`.
    Union,
    /// `enum Name`.
    Enum,
    /// A typedef name. See [`CModule::resolve`].
    Typedef,
}

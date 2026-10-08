//! The type table: interned types, aggregate type metadata and layout.

use std::collections::HashMap;

use wid_diagnostics::Span;
use wid_syntax::Name;

/// An interned type.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct TyId(pub u32);

/// Identifies a struct instance in the type table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct StructId(pub u32);

/// Identifies an enum in the type table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct EnumId(pub u32);

/// Identifies a union instance in the type table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct UnionId(pub u32);

/// Identifies a distinct type in the type table.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct DistinctId(pub u32);

/// Integer types.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
#[allow(missing_docs)]
pub enum IntTy {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Int,
    UInt,
}

impl IntTy {
    /// Every integer type.
    pub const ALL: [IntTy; 10] = [
        IntTy::I8,
        IntTy::I16,
        IntTy::I32,
        IntTy::I64,
        IntTy::U8,
        IntTy::U16,
        IntTy::U32,
        IntTy::U64,
        IntTy::Int,
        IntTy::UInt,
    ];

    /// Returns the size in bytes.
    pub fn size(self) -> u64 {
        match self {
            IntTy::I8 | IntTy::U8 => 1,
            IntTy::I16 | IntTy::U16 => 2,
            IntTy::I32 | IntTy::U32 => 4,
            IntTy::I64 | IntTy::U64 | IntTy::Int | IntTy::UInt => 8,
        }
    }

    /// Returns true for signed types.
    pub fn signed(self) -> bool {
        matches!(self, IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::Int)
    }

    /// Returns the Wid name.
    pub fn name(self) -> &'static str {
        match self {
            IntTy::I8 => "I8",
            IntTy::I16 => "I16",
            IntTy::I32 => "I32",
            IntTy::I64 => "I64",
            IntTy::U8 => "U8",
            IntTy::U16 => "U16",
            IntTy::U32 => "U32",
            IntTy::U64 => "U64",
            IntTy::Int => "Int",
            IntTy::UInt => "UInt",
        }
    }

    /// Returns the smallest and largest representable values.
    pub fn range(self) -> (i128, i128) {
        let bits = self.size() * 8;
        if self.signed() { (-(1i128 << (bits - 1)), (1i128 << (bits - 1)) - 1) } else { (0, (1i128 << bits) - 1) }
    }
}

/// Floating-point types.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
#[allow(missing_docs)]
pub enum FloatTy {
    F32,
    F64,
}

impl FloatTy {
    /// Returns the size in bytes.
    pub fn size(self) -> u64 {
        match self {
            FloatTy::F32 => 4,
            FloatTy::F64 => 8,
        }
    }

    /// Returns the Wid name.
    pub fn name(self) -> &'static str {
        match self {
            FloatTy::F32 => "F32",
            FloatTy::F64 => "F64",
        }
    }
}

/// The calling convention of a procedure.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum Abi {
    /// Wid functions receive the implicit context pointer.
    Wid,
    /// Plain C functions.
    C,
}

/// A procedure signature.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct ProcSig {
    /// Parameter types.
    pub params: Vec<TyId>,
    /// The return type (`Void` when nothing is returned).
    pub ret: TyId,
    /// The calling convention.
    pub abi: Abi,
    /// Whether extra C variadic arguments are accepted.
    pub variadic: bool,
}

/// The structure of a type.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
#[allow(missing_docs)]
pub enum TyKind {
    /// The poison type produced after an error; compatible with everything.
    Unknown,
    /// No value.
    Void,
    /// The type of expressions that never finish, like `panic`.
    Never,
    Bool,
    Int(IntTy),
    Float(FloatTy),
    Rune,
    String,
    CString,
    RawPtr,
    TypeId,
    /// A type as a compile-time value (`T` in `comptime T.fields`). It
    /// exists only while the compiler runs code.
    Type,
    /// Code a macro receives or builds with `quote`, as a compile-time
    /// value: the number of a fragment the interpreter recorded, plus one
    /// (zero is no code). It exists only while the compiler runs code.
    Code,
    Any,
    /// The builtin error set.
    Error,
    /// A name like `:hp` that is not an enum member: the interner's number
    /// for it ([`wid_syntax::Name::index`]). Macros take and splice names
    /// as `Symbol` values, which exist only while the compiler runs code.
    Symbol,
    /// The type of `nil` before it meets an expected type.
    Nil,
    /// The type of a type used as a value (`F32` in `x.to(F32)`).
    TypeValue(TyId),
    Pointer(TyId),
    MultiPointer(TyId),
    Array(TyId, u64),
    Slice(TyId),
    Dynamic(TyId),
    Map(TyId, TyId),
    Proc(ProcSig),
    Optional(TyId),
    Tuple(Vec<TyId>),
    Struct(StructId),
    Enum(EnumId),
    Union(UnionId),
    Distinct(DistinctId),
    Matrix(TyId, u32, u32),
    /// A C type imported verbatim, like `struct Foo` or `FILE`.
    Foreign(Name),
    /// A generic parameter in a signature that has not been instantiated.
    Param(Name),
    /// A value used as a generic argument, like the `64` in `Pool(Ball, 64)`.
    ConstValue(i128),
}

/// A field of a struct.
#[derive(Clone, Debug)]
pub struct FieldInfo {
    /// The field name.
    pub name: Name,
    /// The field type.
    pub ty: TyId,
    /// Byte offset from the start of the struct.
    pub offset: u64,
    /// Whether members are promoted with `using`.
    pub using: bool,
    /// Where the field is declared.
    pub span: Span,
    /// For a field of an imported C struct whose C type differs from its
    /// Wid type, how to convert between them.
    pub c_conv: Option<CConv>,
    /// The C name of a field of an `@[extern]` struct, when it differs from
    /// the Wid name (`@[extern("vertexCount")] vertex_count: C.int`).
    pub c_name: Option<String>,
}

/// How a value crosses between Wid and C when the two spell its type
/// differently.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum CConv {
    /// A data or function pointer: cast it to this C type.
    Pointer(String),
    /// A C array, viewed as Wid's array struct.
    Array,
    /// A record that `types:` maps to a Wid type: copy it to or from this
    /// C type with `memcpy`.
    Mapped(String),
}

/// Metadata for a struct instance.
#[derive(Clone, Debug)]
pub struct StructInfo {
    /// The display name, including generic arguments.
    pub name: String,
    /// The C identifier.
    pub c_name: String,
    /// The fields, once resolved.
    pub fields: Vec<FieldInfo>,
    /// Size in bytes.
    pub size: u64,
    /// Alignment in bytes.
    pub align: u64,
    /// True once fields and layout are known.
    pub complete: bool,
    /// True when the struct comes from a C header and is emitted by it.
    pub foreign: bool,
    /// True for an `@[opaque]` C struct, which Wid only uses through
    /// pointers.
    pub opaque: bool,
    /// Where the struct is declared.
    pub span: Span,
}

/// Metadata for an enum.
#[derive(Clone, Debug)]
pub struct EnumInfo {
    /// The display name.
    pub name: String,
    /// The C identifier.
    pub c_name: String,
    /// The backing integer type.
    pub backing: IntTy,
    /// Members and their values, in declaration order.
    pub members: Vec<(Name, i128)>,
    /// True when the enum comes from a C header.
    pub foreign: bool,
    /// Where the enum is declared.
    pub span: Span,
}

/// Metadata for a tagged union.
#[derive(Clone, Debug)]
pub struct UnionInfo {
    /// The display name.
    pub name: String,
    /// The C identifier.
    pub c_name: String,
    /// The variant types; tag `i + 1` selects variant `i`, tag 0 is nil.
    pub variants: Vec<TyId>,
    /// Size in bytes.
    pub size: u64,
    /// Alignment in bytes.
    pub align: u64,
    /// True once variants and layout are known.
    pub complete: bool,
    /// Where the union is declared.
    pub span: Span,
}

/// Metadata for a distinct type.
#[derive(Clone, Debug)]
pub struct DistinctInfo {
    /// The display name.
    pub name: String,
    /// The C identifier.
    pub c_name: String,
    /// The underlying type.
    pub base: TyId,
}

/// Interns types and stores aggregate metadata.
#[derive(Debug)]
pub struct TypeTable {
    kinds: Vec<TyKind>,
    map: HashMap<TyKind, TyId>,
    /// Struct metadata.
    pub structs: Vec<StructInfo>,
    /// Enum metadata.
    pub enums: Vec<EnumInfo>,
    /// Union metadata.
    pub unions: Vec<UnionInfo>,
    /// Distinct type metadata.
    pub distincts: Vec<DistinctInfo>,
    /// Names of C types imported verbatim, with their size and alignment.
    pub foreign_layout: HashMap<Name, (u64, u64)>,
    /// The builtin `Allocator` struct.
    pub allocator_ty: TyId,
    /// The builtin `Location` struct: a source position.
    pub location_ty: TyId,
    /// The builtin `AllocMode` enum an allocator procedure receives.
    pub alloc_mode_ty: TyId,
    /// The builtin `Logger` struct.
    pub logger_ty: TyId,
    /// The builtin `Context` struct.
    pub context_ty: TyId,
    /// The runtime's text writer, used for printing and interpolation.
    pub writer_ty: TyId,
}

/// The type of expressions that never finish, interned at a fixed id so
/// lowered statements can be checked for divergence without the table.
pub const NEVER: TyId = TyId(1);

macro_rules! well_known {
    ($($name:ident => $kind:expr;)*) => {
        impl TypeTable {
            $(
                #[doc = concat!("The `", stringify!($name), "` type.")]
                pub fn $name(&mut self) -> TyId { self.intern($kind) }
            )*
        }
    };
}

well_known! {
    unknown => TyKind::Unknown;
    void => TyKind::Void;
    never => TyKind::Never;
    bool => TyKind::Bool;
    int => TyKind::Int(IntTy::Int);
    uint => TyKind::Int(IntTy::UInt);
    u8 => TyKind::Int(IntTy::U8);
    i32 => TyKind::Int(IntTy::I32);
    f32 => TyKind::Float(FloatTy::F32);
    f64 => TyKind::Float(FloatTy::F64);
    rune => TyKind::Rune;
    string => TyKind::String;
    cstring => TyKind::CString;
    rawptr => TyKind::RawPtr;
    typeid => TyKind::TypeId;
    type_ty => TyKind::Type;
    code => TyKind::Code;
    any => TyKind::Any;
    error => TyKind::Error;
    symbol => TyKind::Symbol;
    nil => TyKind::Nil;
}

impl Default for TypeTable {
    fn default() -> Self {
        Self::new()
    }
}

impl TypeTable {
    /// Creates a table with the primitive types interned.
    pub fn new() -> Self {
        let mut table = TypeTable {
            kinds: Vec::new(),
            map: HashMap::new(),
            structs: Vec::new(),
            enums: Vec::new(),
            unions: Vec::new(),
            distincts: Vec::new(),
            foreign_layout: HashMap::new(),
            allocator_ty: TyId(0),
            location_ty: TyId(0),
            alloc_mode_ty: TyId(0),
            logger_ty: TyId(0),
            context_ty: TyId(0),
            writer_ty: TyId(0),
        };
        table.intern(TyKind::Unknown);
        let never = table.intern(TyKind::Never);
        debug_assert_eq!(never, NEVER);
        table.add_builtin_structs();
        table
    }

    /// Registers the runtime structs Wid code can name: `Allocator`,
    /// `Logger`, `Context` and the writer used by printing.
    fn add_builtin_structs(&mut self) {
        let raw = self.rawptr();
        let int = self.int();
        let foreign = |table: &mut Self, name: &str, c_name: &str, fields: &[(&str, TyId)]| {
            let parts: Vec<(u64, u64)> = fields.iter().map(|(_, t)| table.layout(*t)).collect();
            let offs = offsets(&parts);
            let (size, align) = aggregate(&parts);
            let fields = fields
                .iter()
                .zip(offs)
                .map(|((n, t), offset)| FieldInfo {
                    c_conv: None,
                    c_name: None,
                    name: Name::new(n),
                    ty: *t,
                    offset,
                    using: false,
                    span: Span::default(),
                })
                .collect();
            table.new_struct(StructInfo {
                opaque: false,
                name: name.into(),
                c_name: c_name.into(),
                fields,
                size,
                align,
                complete: true,
                foreign: true,
                span: Span::default(),
            })
        };
        let cstring = self.cstring();
        let i32 = self.i32();
        let string = self.string();
        let void = self.void();
        let location = foreign(
            self,
            "Location",
            "wid_Location",
            &[("file", cstring), ("line", i32), ("column", i32), ("proc", cstring)],
        );
        let alloc_mode = self.new_enum(EnumInfo {
            name: "AllocMode".into(),
            c_name: "wid_AllocMode".into(),
            backing: IntTy::U8,
            members: ["alloc", "free", "free_all", "resize"]
                .iter()
                .enumerate()
                .map(|(i, n)| (Name::new(n), i as i128))
                .collect(),
            foreign: true,
            span: Span::default(),
        });
        let alloc_proc = self.intern(TyKind::Proc(ProcSig {
            params: vec![raw, alloc_mode, int, int, raw, int, location],
            ret: raw,
            abi: Abi::C,
            variadic: false,
        }));
        let log_proc = self.intern(TyKind::Proc(ProcSig {
            params: vec![raw, i32, string, location],
            ret: void,
            abi: Abi::C,
            variadic: false,
        }));
        let allocator = foreign(self, "Allocator", "wid_Allocator", &[("proc", alloc_proc), ("data", raw)]);
        let logger = foreign(self, "Logger", "wid_Logger", &[("proc", log_proc), ("data", raw)]);
        let context = foreign(
            self,
            "Context",
            "wid_Context",
            &[
                ("allocator", allocator),
                ("temp_allocator", allocator),
                ("logger", logger),
                ("user_data", raw),
                ("user_index", int),
            ],
        );
        let writer_name = Name::new("wid_Writer");
        self.foreign_layout.insert(writer_name, (48, 8));
        self.allocator_ty = allocator;
        self.location_ty = location;
        self.alloc_mode_ty = alloc_mode;
        self.logger_ty = logger;
        self.context_ty = context;
        self.writer_ty = self.intern(TyKind::Foreign(writer_name));
    }

    /// Interns a type.
    pub fn intern(&mut self, kind: TyKind) -> TyId {
        if let Some(&id) = self.map.get(&kind) {
            return id;
        }
        let id = TyId(self.kinds.len() as u32);
        self.kinds.push(kind.clone());
        self.map.insert(kind, id);
        id
    }

    /// Returns the type with this structure if it was interned, without
    /// interning it.
    pub fn lookup(&self, kind: &TyKind) -> Option<TyId> {
        self.map.get(kind).copied()
    }

    /// Returns the structure of a type.
    pub fn kind(&self, ty: TyId) -> &TyKind {
        &self.kinds[ty.0 as usize]
    }

    /// Returns the number of interned types.
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Returns true when no types are interned.
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Interns `T?`. Optionals of optionals collapse.
    pub fn optional(&mut self, inner: TyId) -> TyId {
        if matches!(self.kind(inner), TyKind::Optional(_)) {
            return inner;
        }
        self.intern(TyKind::Optional(inner))
    }

    /// Interns `^T`.
    pub fn pointer(&mut self, inner: TyId) -> TyId {
        self.intern(TyKind::Pointer(inner))
    }

    /// Interns `[]T`.
    pub fn slice(&mut self, inner: TyId) -> TyId {
        self.intern(TyKind::Slice(inner))
    }

    /// Interns a tuple; single-element tuples collapse to their element.
    pub fn tuple(&mut self, elems: Vec<TyId>) -> TyId {
        match elems.len() {
            0 => self.void(),
            1 => elems[0],
            _ => self.intern(TyKind::Tuple(elems)),
        }
    }

    /// Registers a new struct and returns its type.
    pub fn new_struct(&mut self, info: StructInfo) -> TyId {
        let id = StructId(self.structs.len() as u32);
        self.structs.push(info);
        self.intern(TyKind::Struct(id))
    }

    /// Registers a new enum and returns its type.
    pub fn new_enum(&mut self, info: EnumInfo) -> TyId {
        let id = EnumId(self.enums.len() as u32);
        self.enums.push(info);
        self.intern(TyKind::Enum(id))
    }

    /// Registers a new union and returns its type.
    pub fn new_union(&mut self, info: UnionInfo) -> TyId {
        let id = UnionId(self.unions.len() as u32);
        self.unions.push(info);
        self.intern(TyKind::Union(id))
    }

    /// Registers a new distinct type and returns it.
    pub fn new_distinct(&mut self, info: DistinctInfo) -> TyId {
        let id = DistinctId(self.distincts.len() as u32);
        self.distincts.push(info);
        self.intern(TyKind::Distinct(id))
    }

    /// Returns the struct metadata for a struct type.
    pub fn struct_info(&self, id: StructId) -> &StructInfo {
        &self.structs[id.0 as usize]
    }

    /// Returns the enum metadata for an enum type.
    pub fn enum_info(&self, id: EnumId) -> &EnumInfo {
        &self.enums[id.0 as usize]
    }

    /// Returns the union metadata for a union type.
    pub fn union_info(&self, id: UnionId) -> &UnionInfo {
        &self.unions[id.0 as usize]
    }

    /// Strips `distinct` wrappers.
    pub fn base(&self, mut ty: TyId) -> TyId {
        while let TyKind::Distinct(id) = self.kind(ty) {
            ty = self.distincts[id.0 as usize].base;
        }
        ty
    }

    /// Returns true for integer types (looking through `distinct`).
    pub fn is_int(&self, ty: TyId) -> bool {
        matches!(self.kind(self.base(ty)), TyKind::Int(_))
    }

    /// Returns true for float types (looking through `distinct`).
    pub fn is_float(&self, ty: TyId) -> bool {
        matches!(self.kind(self.base(ty)), TyKind::Float(_))
    }

    /// Returns true for integer and float types.
    pub fn is_numeric(&self, ty: TyId) -> bool {
        self.is_int(ty) || self.is_float(ty)
    }

    /// Returns true when `nil` is a valid value of the type.
    pub fn is_nilable(&self, ty: TyId) -> bool {
        matches!(
            self.kind(self.base(ty)),
            TyKind::Optional(_) | TyKind::Error | TyKind::Union(_) | TyKind::RawPtr | TyKind::CString | TyKind::Proc(_)
        )
    }

    /// Returns the size in bytes.
    pub fn size_of(&self, ty: TyId) -> u64 {
        self.layout(ty).0
    }

    /// Returns the alignment in bytes.
    pub fn align_of(&self, ty: TyId) -> u64 {
        self.layout(ty).1
    }

    /// Returns `(size, align)` for 64-bit targets. A size that doesn't fit
    /// in a `u64` is `u64::MAX`; it belongs to a type over
    /// [`MAX_TYPE_SIZE`], which the checker reports where the type is
    /// written, so code generation never sees one.
    pub fn layout(&self, ty: TyId) -> (u64, u64) {
        let (size, align) = self.wide_layout(ty);
        (narrow(size), align)
    }

    /// Returns `(size, align)` like [`TypeTable::layout`], with the size
    /// computed without overflow, so the checker can say how large a type
    /// over [`MAX_TYPE_SIZE`] would be.
    pub fn wide_layout(&self, ty: TyId) -> (u128, u64) {
        let fixed = |size: u64, align: u64| (u128::from(size), align);
        match self.kind(ty) {
            TyKind::Unknown
            | TyKind::Void
            | TyKind::Never
            | TyKind::Nil
            | TyKind::TypeValue(_)
            | TyKind::Param(_)
            | TyKind::ConstValue(_) => fixed(0, 1),
            TyKind::Bool => fixed(1, 1),
            TyKind::Int(i) => fixed(i.size(), i.size()),
            TyKind::Float(f) => fixed(f.size(), f.size()),
            TyKind::Rune | TyKind::Error => fixed(4, 4),
            TyKind::String | TyKind::Slice(_) => fixed(16, 8),
            TyKind::CString | TyKind::RawPtr | TyKind::Pointer(_) | TyKind::MultiPointer(_) | TyKind::Proc(_) => {
                fixed(8, 8)
            }
            TyKind::TypeId | TyKind::Type | TyKind::Code | TyKind::Symbol => fixed(8, 8),
            TyKind::Any => fixed(16, 8),
            TyKind::Array(elem, n) => {
                let (s, a) = self.wide_layout(*elem);
                (s.saturating_mul(u128::from(*n)), a)
            }
            TyKind::Matrix(elem, r, c) => {
                let (s, a) = self.wide_layout(*elem);
                (s.saturating_mul(u128::from(*r)).saturating_mul(u128::from(*c)), a)
            }
            TyKind::Dynamic(_) => fixed(40, 8),
            TyKind::Map(_, _) => fixed(40, 8),
            TyKind::Optional(inner) => {
                if self.optional_is_pointer(ty) {
                    fixed(8, 8)
                } else {
                    aggregate_wide([self.wide_layout(*inner), (1, 1)])
                }
            }
            TyKind::Tuple(elems) => aggregate_wide(elems.iter().map(|e| self.wide_layout(*e))),
            TyKind::Struct(id) => {
                let info = self.struct_info(*id);
                fixed(info.size, info.align)
            }
            TyKind::Enum(id) => {
                let b = self.enum_info(*id).backing;
                fixed(b.size(), b.size())
            }
            TyKind::Union(id) => {
                let info = self.union_info(*id);
                fixed(info.size, info.align)
            }
            TyKind::Distinct(id) => self.wide_layout(self.distincts[id.0 as usize].base),
            TyKind::Foreign(name) => {
                let (s, a) = self.foreign_layout.get(name).copied().unwrap_or((8, 8));
                fixed(s, a)
            }
        }
    }

    /// How many bytes a type takes in the generated C, without overflow,
    /// when that is over [`MAX_TYPE_SIZE`]; `None` when the type fits.
    pub fn oversize(&self, ty: TyId) -> Option<u128> {
        let size = self.c_layout(ty).0;
        (size > u128::from(MAX_TYPE_SIZE)).then_some(size)
    }

    /// `(size, align)` of a type as C lays out the generated code: those of
    /// [`TypeTable::wide_layout`], except that the C code gives a struct or
    /// union without members one byte and an `[0]T` array one element, which
    /// Wid counts as empty.
    pub fn c_layout(&self, ty: TyId) -> (u128, u64) {
        match self.kind(ty) {
            TyKind::Array(elem, n) => {
                let (s, a) = self.c_layout(*elem);
                (s.saturating_mul(u128::from((*n).max(1))), a)
            }
            TyKind::Matrix(elem, r, c) => {
                let (s, a) = self.c_layout(*elem);
                (s.saturating_mul(u128::from(*r)).saturating_mul(u128::from(*c)), a)
            }
            TyKind::Optional(inner) if !self.optional_is_pointer(ty) => aggregate_wide([self.c_layout(*inner), (1, 1)]),
            TyKind::Tuple(elems) => aggregate_wide(elems.iter().map(|e| self.c_layout(*e))),
            TyKind::Struct(id) => {
                let info = self.struct_info(*id);
                let (size, align) = match info.fields.is_empty() {
                    true => (1, 1),
                    false => aggregate_wide(info.fields.iter().map(|f| self.c_layout(f.ty))),
                };
                (size.max(u128::from(info.size)), align.max(info.align))
            }
            TyKind::Union(id) => {
                let info = self.union_info(*id);
                let payload = info
                    .variants
                    .iter()
                    .map(|v| self.c_layout(*v))
                    .fold((1, 1), |(s, a), (vs, va)| (s.max(vs), a.max(va)));
                let (size, align) = aggregate_wide([(4, 4), payload]);
                (size.max(u128::from(info.size)), align.max(info.align))
            }
            TyKind::Distinct(id) => self.c_layout(self.distincts[id.0 as usize].base),
            _ => self.wide_layout(ty),
        }
    }

    /// Returns true when `T?` is represented as a nullable pointer.
    pub fn optional_is_pointer(&self, ty: TyId) -> bool {
        match self.kind(ty) {
            TyKind::Optional(inner) => matches!(
                self.kind(self.base(*inner)),
                TyKind::Pointer(_) | TyKind::MultiPointer(_) | TyKind::Proc(_) | TyKind::CString | TyKind::RawPtr
            ),
            _ => false,
        }
    }

    /// Formats a type the way it is written in Wid source.
    pub fn display(&self, ty: TyId) -> String {
        match self.kind(ty) {
            TyKind::Unknown => "{unknown}".into(),
            TyKind::Void => "Void".into(),
            TyKind::Never => "Never".into(),
            TyKind::Bool => "Bool".into(),
            TyKind::Int(i) => i.name().into(),
            TyKind::Float(f) => f.name().into(),
            TyKind::Rune => "Rune".into(),
            TyKind::String => "String".into(),
            TyKind::CString => "CString".into(),
            TyKind::RawPtr => "RawPtr".into(),
            TyKind::TypeId => "TypeId".into(),
            TyKind::Type => "Type".into(),
            TyKind::Any => "Any".into(),
            TyKind::Error => "Error".into(),
            TyKind::Symbol => "Symbol".into(),
            TyKind::Code => "Code".into(),
            TyKind::Nil => "nil".into(),
            TyKind::TypeValue(t) => format!("type {}", self.display(*t)),
            TyKind::Pointer(t) | TyKind::MultiPointer(t) => {
                let prefix = if matches!(self.kind(ty), TyKind::Pointer(_)) { "^" } else { "[^]" };
                match self.kind(*t) {
                    TyKind::Optional(_) => format!("{prefix}({})", self.display(*t)),
                    _ => format!("{prefix}{}", self.display(*t)),
                }
            }
            TyKind::Array(t, n) => format!("[{n}]{}", self.display(*t)),
            TyKind::Slice(t) => format!("[]{}", self.display(*t)),
            TyKind::Dynamic(t) => format!("[dynamic]{}", self.display(*t)),
            TyKind::Map(k, v) => format!("map[{}]{}", self.display(*k), self.display(*v)),
            TyKind::Proc(sig) => {
                let params: Vec<String> = sig.params.iter().map(|p| self.display(*p)).collect();
                let ret = if matches!(self.kind(sig.ret), TyKind::Void) {
                    String::new()
                } else {
                    format!(" -> {}", self.display(sig.ret))
                };
                format!("proc({}){ret}", params.join(", "))
            }
            TyKind::Optional(t) => {
                // `proc(Int) -> Int?` returns an optional, so an optional
                // proc that returns a value is `(proc(Int) -> Int)?`.
                let shown = self.display(*t);
                match self.kind(*t) {
                    TyKind::Proc(sig) if !matches!(self.kind(sig.ret), TyKind::Void) => format!("({shown})?"),
                    _ => format!("{shown}?"),
                }
            }
            TyKind::Tuple(elems) => {
                let parts: Vec<String> = elems.iter().map(|e| self.display(*e)).collect();
                format!("({})", parts.join(", "))
            }
            TyKind::Struct(id) => self.struct_info(*id).name.clone(),
            TyKind::Enum(id) => self.enum_info(*id).name.clone(),
            TyKind::Union(id) => self.union_info(*id).name.clone(),
            TyKind::Distinct(id) => self.distincts[id.0 as usize].name.clone(),
            TyKind::Matrix(t, r, c) => format!("matrix[{r}, {c}]{}", self.display(*t)),
            TyKind::Foreign(name) => name.as_str().to_string(),
            TyKind::Param(name) => name.as_str().to_string(),
            TyKind::ConstValue(v) => v.to_string(),
        }
    }
}

/// The largest number of bytes a type may take, and of elements an array
/// may have: `2^61 - 1`. Clang rejects an array of `2^61` bytes or more (it
/// counts sizes in bits, in 64 bits) and gcc any type over `PTRDIFF_MAX`, so
/// every type within the limit compiles with both. The checker reports
/// larger types (E0329).
pub const MAX_TYPE_SIZE: u64 = (1 << 61) - 1;

/// A size from [`TypeTable::wide_layout`] as a `u64`, `u64::MAX` when it
/// doesn't fit.
fn narrow(size: u128) -> u64 {
    u64::try_from(size).unwrap_or(u64::MAX)
}

/// Rounds `offset` up to a multiple of `align`, saturating.
fn align_up(offset: u128, align: u64) -> u128 {
    offset.checked_next_multiple_of(u128::from(align.max(1))).unwrap_or(u128::MAX)
}

/// Lays out fields in order with natural alignment, returning `(size, align)`.
/// Sizes saturate instead of overflowing, like [`TypeTable::wide_layout`].
pub fn aggregate_wide(parts: impl IntoIterator<Item = (u128, u64)>) -> (u128, u64) {
    let mut offset = 0u128;
    let mut align = 1u64;
    for (s, a) in parts {
        let a = a.max(1);
        offset = align_up(offset, a).saturating_add(s);
        align = align.max(a);
    }
    (align_up(offset, align), align)
}

/// Lays out fields in order with natural alignment, returning `(size, align)`.
/// A size that doesn't fit in a `u64` is `u64::MAX`.
pub fn aggregate(parts: &[(u64, u64)]) -> (u64, u64) {
    let (size, align) = aggregate_wide(parts.iter().map(|&(s, a)| (u128::from(s), a)));
    (narrow(size), align)
}

/// Returns the offset of each part when laid out in order, saturating at
/// `u64::MAX`.
pub fn offsets(parts: &[(u64, u64)]) -> Vec<u64> {
    let mut offset = 0u128;
    let mut out = Vec::with_capacity(parts.len());
    for &(s, a) in parts {
        offset = align_up(offset, a);
        out.push(narrow(offset));
        offset = offset.saturating_add(u128::from(s));
    }
    out
}

//! Turns libclang declarations into [`Item`]s and libclang types into
//! [`CType`]s.
// libclang constants keep their C names.
#![allow(non_upper_case_globals)]

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use clang_sys::*;

use crate::ffi::{Cursor, Type};
use crate::model::*;
use crate::source::{Sources, trailing_comment};

/// The C namespace a declaration's name occupies, used to merge
/// redeclarations.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
enum Key {
    /// Struct, union and enum tags.
    Tag(String),
    /// Functions, variables, typedefs and enum constants.
    Ordinary(String),
    /// An anonymous enum, by USR.
    Anonymous(String),
    /// Macros.
    Macro(String),
}

/// An item with the key that orders it.
struct Entry {
    order: Vec<u32>,
    item: Item,
}

/// Accumulates the items of one import.
pub(crate) struct Lowerer {
    /// File membership, ordering and contents.
    pub sources: Sources,
    entries: Vec<Entry>,
    names: HashMap<Key, usize>,
    /// Functions to describe wherever they are declared.
    probes: HashSet<String>,
    /// The probed functions found so far.
    pub probed: Vec<Function>,
}

impl Lowerer {
    /// Creates an empty lowerer over `sources`.
    pub fn new(sources: Sources, probes: &[String]) -> Lowerer {
        Lowerer {
            sources,
            entries: Vec::new(),
            names: HashMap::new(),
            probes: probes.iter().cloned().collect(),
            probed: Vec::new(),
        }
    }

    /// The items, sorted into preprocessing order.
    pub fn finish(mut self) -> Vec<Item> {
        self.entries.sort_by(|a, b| a.order.cmp(&b.order));
        self.entries.into_iter().map(|entry| entry.item).collect()
    }

    /// Lowers the top-level declarations among `cursors` that belong to the
    /// imported headers.
    pub fn declarations(&mut self, cursors: &[Cursor<'_>]) {
        for &cursor in cursors {
            if !self.is_owned(cursor) {
                if cursor.kind() == CXCursor_FunctionDecl {
                    let name = cursor.spelling();
                    if self.probes.remove(&name) {
                        let function = self.lower_function(cursor, name);
                        self.probed.push(function);
                    }
                }
                continue;
            }
            match cursor.kind() {
                CXCursor_FunctionDecl => self.function(cursor),
                CXCursor_StructDecl | CXCursor_UnionDecl => {
                    self.record(cursor);
                }
                CXCursor_EnumDecl => {
                    self.enumeration(cursor);
                }
                CXCursor_TypedefDecl => self.typedef(cursor),
                CXCursor_VarDecl => self.global(cursor),
                _ => {}
            }
        }
    }

    /// Adds a macro item unless one with the same name exists, in which case
    /// the later definition replaces it.
    pub fn add_macro(&mut self, order: Vec<u32>, item: Item, name: &str) {
        let key = Key::Macro(name.to_string());
        match self.names.get(&key) {
            Some(&index) => self.entries[index] = Entry { order, item },
            None => {
                self.push(key, order, item);
            }
        }
    }

    /// Whether the cursor is declared in the imported headers.
    pub fn is_owned(&mut self, cursor: Cursor<'_>) -> bool {
        let spot = cursor.location().expansion();
        self.sources.is_owned_spot(&spot)
    }

    /// The location of the cursor's name.
    pub fn location(cursor: Cursor<'_>) -> Location {
        let spot = cursor.location().expansion();
        Location { file: PathBuf::from(spot.file.unwrap_or_default()), line: spot.line, column: spot.column }
    }

    /// The order key of the cursor.
    fn order(&mut self, cursor: Cursor<'_>) -> Vec<u32> {
        let spot = cursor.location().expansion();
        self.sources.order_key(&spot)
    }

    /// The documentation for a declaration: libclang's attached doc comment,
    /// or a comment trailing it on the same line.
    pub fn doc(&mut self, cursor: Cursor<'_>) -> Option<String> {
        if let Some(comment) = cursor.raw_comment() {
            return Some(comment);
        }
        let end = cursor.extent().end().expansion();
        let offset = end.offset as usize;
        let text = self.sources.text(end.file.as_deref()?)?;
        trailing_comment(text, offset)
    }

    /// Records a new entry under `key`.
    fn push(&mut self, key: Key, order: Vec<u32>, item: Item) -> usize {
        let index = self.entries.len();
        self.entries.push(Entry { order, item });
        self.names.insert(key, index);
        index
    }

    /// Lowers a function declaration.
    fn function(&mut self, cursor: Cursor<'_>) {
        let name = cursor.spelling();
        let key = Key::Ordinary(name.clone());
        if let Some(&index) = self.names.get(&key) {
            if self.entries[index].item.doc.is_none() {
                self.entries[index].item.doc = self.doc(cursor);
            }
            return;
        }
        let function = self.lower_function(cursor, name);
        let item = Item { location: Self::location(cursor), doc: self.doc(cursor), kind: ItemKind::Function(function) };
        let order = self.order(cursor);
        self.push(key, order, item);
    }

    /// Lowers the signature and storage of a function declaration.
    fn lower_function(&mut self, cursor: Cursor<'_>, name: String) -> Function {
        let fn_type = cursor.ty().canonical();
        let params = cursor
            .arguments()
            .into_iter()
            .map(|param| {
                let ty = param.ty();
                let mut lowered = self.param_type(ty);
                name_params(&mut lowered, param);
                Param { name: non_empty(param.spelling()), ty: lowered, quals: quals(ty) }
            })
            .collect();
        let sig = FnSig {
            params,
            ret: self.ty(cursor.result_type()),
            variadic: fn_type.is_variadic(),
            prototyped: fn_type.kind() == CXType_FunctionProto,
        };
        Function { name, sig, is_inline: cursor.is_inlined(), is_static: cursor.storage_class() == CX_SC_Static }
    }

    /// Lowers a variable declared at file scope.
    fn global(&mut self, cursor: Cursor<'_>) {
        let name = cursor.spelling();
        let key = Key::Ordinary(name.clone());
        if self.names.contains_key(&key) {
            return;
        }
        let ty = cursor.ty();
        let mut lowered = self.ty(ty);
        name_params(&mut lowered, cursor);
        let item = Item {
            location: Self::location(cursor),
            doc: self.doc(cursor),
            kind: ItemKind::Global(Global {
                name,
                ty: lowered,
                quals: quals(ty),
                is_static: cursor.storage_class() == CX_SC_Static,
                is_thread_local: cursor.is_thread_local(),
            }),
        };
        let order = self.order(cursor);
        self.push(key, order, item);
    }

    /// Lowers a typedef, merging it into the record or enum it names when it
    /// aliases one exactly and that one has no typedef name yet.
    fn typedef(&mut self, cursor: Cursor<'_>) {
        let name = cursor.spelling();
        let key = Key::Ordinary(name.clone());
        if self.names.contains_key(&key) {
            return;
        }
        let underlying = cursor.typedef_underlying();
        let target = strip_elaboration(underlying);
        let unqualified = quals(underlying) == Quals::default() && quals(target) == Quals::default();
        if unqualified && matches!(target.kind(), CXType_Record | CXType_Enum) {
            let decl = target.declaration();
            if self.is_owned(decl) {
                let index = if target.kind() == CXType_Record { self.record(decl) } else { self.enumeration(decl) };
                if let Some(index) = index {
                    let slot = match &mut self.entries[index].item.kind {
                        ItemKind::Record(record) => &mut record.typedef_name,
                        ItemKind::Enum(enumeration) => &mut enumeration.typedef_name,
                        _ => return,
                    };
                    if slot.is_none() {
                        *slot = Some(name);
                        self.names.insert(key, index);
                        return;
                    }
                }
            }
        }
        let mut ty = self.ty(underlying);
        name_params(&mut ty, cursor);
        let item = Item {
            location: Self::location(cursor),
            doc: self.doc(cursor),
            kind: ItemKind::Typedef(Typedef { name, ty, quals: quals(underlying) }),
        };
        let order = self.order(cursor);
        self.push(key, order, item);
    }

    /// Lowers a struct or union declaration and returns its entry. Returns
    /// `None` for an anonymous record, which is lowered in place wherever it
    /// is used instead.
    fn record(&mut self, cursor: Cursor<'_>) -> Option<usize> {
        let kind = if cursor.kind() == CXCursor_UnionDecl { RecordKind::Union } else { RecordKind::Struct };
        let tag = tag_name(cursor);
        let (key, typedef_name) = match &tag {
            Some(tag) => (Key::Tag(tag.clone()), None),
            None if cursor.is_anonymous() => return None,
            None => (Key::Ordinary(cursor.spelling()), Some(cursor.spelling())),
        };
        let index = match self.names.get(&key) {
            Some(&index) => index,
            None => {
                let item = Item {
                    location: Self::location(cursor),
                    doc: self.doc(cursor),
                    kind: ItemKind::Record(Record { kind, tag, typedef_name, body: None }),
                };
                let order = self.order(cursor);
                self.push(key, order, item)
            }
        };
        let has_body = matches!(&self.entries[index].item.kind, ItemKind::Record(record) if record.body.is_some());
        if cursor.is_definition() && !has_body {
            let body = self.record_body(cursor);
            let doc = self.doc(cursor);
            let entry = &mut self.entries[index].item;
            entry.location = Self::location(cursor);
            if doc.is_some() {
                entry.doc = doc;
            }
            if let ItemKind::Record(record) = &mut entry.kind {
                record.body = body;
            }
        }
        Some(index)
    }

    /// Lowers the fields and layout of a record definition, hoisting tagged
    /// records and enums declared inside it to file scope as C does.
    fn record_body(&mut self, cursor: Cursor<'_>) -> Option<RecordBody> {
        for child in cursor.children() {
            match child.kind() {
                CXCursor_StructDecl | CXCursor_UnionDecl if tag_name(child).is_some() => {
                    self.record(child);
                }
                CXCursor_EnumDecl => {
                    self.enumeration(child);
                }
                _ => {}
            }
        }
        let ty = cursor.ty();
        let (size, align) = (ty.size_of()?, ty.align_of()?);
        let fields = ty
            .fields()
            .into_iter()
            .map(|field| {
                let field_ty = field.ty();
                let mut lowered = self.ty(field_ty);
                name_params(&mut lowered, field);
                // libclang spells the implicit field of an anonymous member
                // `union Outer::(anonymous at …)`.
                let name = Some(field.spelling()).filter(|name| is_identifier(name));
                Field {
                    name,
                    ty: lowered,
                    quals: quals(field_ty),
                    bit_width: field.bit_width(),
                    offset_bits: field.field_offset_bits().unwrap_or(0),
                    doc: self.doc(field),
                }
            })
            .collect();
        Some(RecordBody { fields, size, align })
    }

    /// Lowers an enum declaration and returns its entry.
    fn enumeration(&mut self, cursor: Cursor<'_>) -> Option<usize> {
        let tag = tag_name(cursor);
        let (key, typedef_name) = match &tag {
            Some(tag) => (Key::Tag(tag.clone()), None),
            None if cursor.is_anonymous() => (Key::Anonymous(cursor.usr()), None),
            None => (Key::Ordinary(cursor.spelling()), Some(cursor.spelling())),
        };
        let integer = cursor.enum_integer_type();
        let signed = is_signed(integer.canonical().kind());
        let constants: Vec<EnumConstant> = cursor
            .children()
            .into_iter()
            .filter(|child| child.kind() == CXCursor_EnumConstantDecl)
            .map(|constant| EnumConstant {
                name: constant.spelling(),
                value: constant.enum_value(signed),
                location: Self::location(constant),
                doc: self.doc(constant),
            })
            .collect();
        if let Some(&index) = self.names.get(&key) {
            let entry = &mut self.entries[index].item;
            if let ItemKind::Enum(enumeration) = &mut entry.kind
                && enumeration.constants.is_empty()
                && !constants.is_empty()
            {
                enumeration.constants = constants;
                entry.location = Self::location(cursor);
            }
            return Some(index);
        }
        let underlying = self.ty(integer);
        let item = Item {
            location: Self::location(cursor),
            doc: self.doc(cursor),
            kind: ItemKind::Enum(Enum { tag, typedef_name, underlying, constants }),
        };
        let order = self.order(cursor);
        Some(self.push(key, order, item))
    }

    /// Lowers a parameter type, applying C's adjustment of arrays and
    /// functions to pointers.
    fn param_type(&mut self, ty: Type<'_>) -> CType {
        let target = strip_elaboration(ty);
        match target.kind() {
            CXType_ConstantArray | CXType_IncompleteArray | CXType_VariableArray | CXType_DependentSizedArray => {
                let element = target.array_element();
                CType::Pointer(Box::new(PointerType { pointee: self.ty(element), pointee_quals: quals(element) }))
            }
            CXType_FunctionProto | CXType_FunctionNoProto => CType::FnPtr(Box::new(self.sig(target))),
            _ => self.ty(ty),
        }
    }

    /// Lowers a function type without parameter names.
    fn sig(&mut self, fn_type: Type<'_>) -> FnSig {
        let params = fn_type
            .arg_types()
            .unwrap_or_default()
            .into_iter()
            .map(|param| Param { name: None, ty: self.param_type(param), quals: quals(param) })
            .collect();
        FnSig {
            params,
            ret: self.ty(fn_type.result()),
            variadic: fn_type.is_variadic(),
            prototyped: fn_type.kind() == CXType_FunctionProto,
        }
    }

    /// Lowers a type. Top-level qualifiers are not part of the result; read
    /// them with [`quals`].
    pub fn ty(&mut self, ty: Type<'_>) -> CType {
        let kind = ty.kind();
        if let Some((signed, spelling)) = builtin_int(kind) {
            return CType::Int(IntType { bits: bits(ty), signed, spelling: spelling.to_string() });
        }
        if let Some(spelling) = builtin_float(kind) {
            return CType::Float(FloatType { bits: bits(ty), spelling: spelling.to_string() });
        }
        match kind {
            CXType_Elaborated => self.ty(ty.named()),
            CXType_Void => CType::Void,
            CXType_Bool => CType::Bool,
            CXType_Char_S => CType::Char { signed: true },
            CXType_Char_U => CType::Char { signed: false },
            CXType_Pointer => {
                let pointee = ty.pointee();
                let target = strip_elaboration(pointee);
                if matches!(target.kind(), CXType_FunctionProto | CXType_FunctionNoProto) {
                    CType::FnPtr(Box::new(self.sig(target)))
                } else {
                    CType::Pointer(Box::new(PointerType { pointee: self.ty(pointee), pointee_quals: quals(pointee) }))
                }
            }
            CXType_FunctionProto | CXType_FunctionNoProto => CType::Function(Box::new(self.sig(ty))),
            CXType_ConstantArray | CXType_IncompleteArray => {
                let element = ty.array_element();
                let len = if kind == CXType_ConstantArray { ty.array_size() } else { None };
                CType::Array(Box::new(ArrayType { element: self.ty(element), element_quals: quals(element), len }))
            }
            CXType_Typedef => self.typedef_ref(ty.declaration(), ty),
            CXType_Record => self.record_ref(ty.declaration()),
            CXType_Enum => self.enum_ref(ty.declaration()),
            CXType_Atomic => CType::Atomic(Box::new(self.ty(ty.atomic_value()))),
            CXType_Auto => {
                // `__auto_type` keeps the deduced type's typedef sugar only
                // through its declaration.
                let decl = ty.declaration();
                match decl.kind() {
                    CXCursor_TypedefDecl => self.typedef_ref(decl, ty),
                    CXCursor_StructDecl | CXCursor_UnionDecl => self.record_ref(decl),
                    CXCursor_EnumDecl => self.enum_ref(decl),
                    _ => self.desugared(ty),
                }
            }
            _ => self.desugared(ty),
        }
    }

    /// Lowers a type libclang does not expose through its canonical form,
    /// or keeps its spelling when that is unexposed too.
    fn desugared(&mut self, ty: Type<'_>) -> CType {
        let canonical = ty.canonical();
        let kind = canonical.kind();
        if kind != ty.kind() && !matches!(kind, CXType_Unexposed | CXType_Invalid | CXType_Auto) {
            return self.ty(canonical);
        }
        CType::Opaque(unqualified_spelling(ty))
    }

    /// Lowers a use of a typedef. Typedefs from outside the imported headers
    /// become the scalar they stand for, spelled with their name, or opaque.
    fn typedef_ref(&mut self, decl: Cursor<'_>, ty: Type<'_>) -> CType {
        let name = decl.spelling();
        if self.is_owned(decl) {
            return CType::Named(Named { kind: NamedKind::Typedef, name });
        }
        let canonical = ty.canonical();
        let kind = canonical.kind();
        if let Some((signed, _)) = builtin_int(kind) {
            return CType::Int(IntType { bits: bits(canonical), signed, spelling: name });
        }
        if let Some(_spelling) = builtin_float(kind) {
            return CType::Float(FloatType { bits: bits(canonical), spelling: name });
        }
        match kind {
            CXType_Bool => CType::Bool,
            CXType_Void => CType::Void,
            CXType_Char_S => CType::Char { signed: true },
            CXType_Char_U => CType::Char { signed: false },
            _ => CType::Opaque(name),
        }
    }

    /// Lowers a use of a struct or union type.
    fn record_ref(&mut self, decl: Cursor<'_>) -> CType {
        let tag = tag_name(decl);
        let union = decl.kind() == CXCursor_UnionDecl;
        if !self.is_owned(decl) {
            return CType::Opaque(unqualified_spelling(decl.ty()));
        }
        match tag {
            Some(name) => CType::Named(Named { kind: if union { NamedKind::Union } else { NamedKind::Struct }, name }),
            None if !decl.is_anonymous() => CType::Named(Named { kind: NamedKind::Typedef, name: decl.spelling() }),
            None => {
                let kind = if union { RecordKind::Union } else { RecordKind::Struct };
                let body = self.record_body(decl);
                CType::Record(Box::new(Record { kind, tag: None, typedef_name: None, body }))
            }
        }
    }

    /// Lowers a use of an enum type. An anonymous enum used as a type is its
    /// integer type; its constants are an item of their own.
    fn enum_ref(&mut self, decl: Cursor<'_>) -> CType {
        if !self.is_owned(decl) {
            return CType::Opaque(unqualified_spelling(decl.ty()));
        }
        match tag_name(decl) {
            Some(name) => CType::Named(Named { kind: NamedKind::Enum, name }),
            None if !decl.is_anonymous() => CType::Named(Named { kind: NamedKind::Typedef, name: decl.spelling() }),
            None => self.ty(decl.enum_integer_type()),
        }
    }
}

/// The qualifiers written on `ty` itself.
pub(crate) fn quals(ty: Type<'_>) -> Quals {
    Quals { is_const: ty.is_const(), is_volatile: ty.is_volatile(), is_restrict: ty.is_restrict() }
}

/// Looks through `struct Foo`-style elaboration to the type it names.
fn strip_elaboration(ty: Type<'_>) -> Type<'_> {
    if ty.kind() == CXType_Elaborated { strip_elaboration(ty.named()) } else { ty }
}

/// The width of a scalar type in bits.
fn bits(ty: Type<'_>) -> u32 {
    ty.size_of().and_then(|size| u32::try_from(size * 8).ok()).unwrap_or(0)
}

/// Signedness and spelling of the builtin integer types other than plain `char`.
fn builtin_int(kind: CXTypeKind) -> Option<(bool, &'static str)> {
    Some(match kind {
        CXType_SChar => (true, "signed char"),
        CXType_UChar => (false, "unsigned char"),
        CXType_Short => (true, "short"),
        CXType_UShort => (false, "unsigned short"),
        CXType_Int => (true, "int"),
        CXType_UInt => (false, "unsigned int"),
        CXType_Long => (true, "long"),
        CXType_ULong => (false, "unsigned long"),
        CXType_LongLong => (true, "long long"),
        CXType_ULongLong => (false, "unsigned long long"),
        CXType_Int128 => (true, "__int128"),
        CXType_UInt128 => (false, "unsigned __int128"),
        CXType_WChar => (true, "wchar_t"),
        CXType_Char16 => (false, "char16_t"),
        CXType_Char32 => (false, "char32_t"),
        _ => return None,
    })
}

/// Spelling of the builtin floating-point types.
fn builtin_float(kind: CXTypeKind) -> Option<&'static str> {
    Some(match kind {
        CXType_Float => "float",
        CXType_Double => "double",
        CXType_LongDouble => "long double",
        CXType_Float16 => "_Float16",
        CXType_Half => "__fp16",
        CXType_BFloat16 => "__bf16",
        CXType_Float128 => "__float128",
        CXType_Ibm128 => "__ibm128",
        _ => return None,
    })
}

/// Whether a canonical integer type kind is signed.
fn is_signed(kind: CXTypeKind) -> bool {
    match kind {
        CXType_Char_S => true,
        _ => builtin_int(kind).is_some_and(|(signed, _)| signed),
    }
}

/// The tag of a struct, union or enum declaration, or `None` when it has
/// none. libclang spells a tagless record by its typedef name or as
/// `struct (unnamed at …)`, so the tag is only trusted when the type is
/// spelled `struct <tag>`.
pub(crate) fn tag_name(decl: Cursor<'_>) -> Option<String> {
    let spelling = decl.spelling();
    if !is_identifier(&spelling) {
        return None;
    }
    let keyword = match decl.kind() {
        CXCursor_StructDecl => "struct",
        CXCursor_UnionDecl => "union",
        CXCursor_EnumDecl => "enum",
        _ => return None,
    };
    (unqualified_spelling(decl.ty()) == format!("{keyword} {spelling}")).then_some(spelling)
}

/// Whether `text` is a C identifier.
pub(crate) fn is_identifier(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some_and(|first| first == '_' || first.is_ascii_alphabetic())
        && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
}

/// A type's spelling without leading qualifiers.
fn unqualified_spelling(ty: Type<'_>) -> String {
    let spelling = ty.spelling();
    let mut rest = spelling.as_str();
    loop {
        let trimmed = ["const ", "volatile ", "restrict "].iter().find_map(|qualifier| rest.strip_prefix(qualifier));
        match trimmed {
            Some(tail) => rest = tail,
            None => return rest.to_string(),
        }
    }
}

/// `Some(text)` unless it is empty.
fn non_empty(text: String) -> Option<String> {
    if text.is_empty() { None } else { Some(text) }
}

/// Names the parameters of a function-pointer type from the parameter
/// declarations nested under the declaration that spells it.
fn name_params(ty: &mut CType, cursor: Cursor<'_>) {
    let sig = match ty {
        CType::FnPtr(sig) | CType::Function(sig) => sig,
        _ => return,
    };
    let names: Vec<Cursor<'_>> =
        cursor.children().into_iter().filter(|child| child.kind() == CXCursor_ParmDecl).collect();
    if names.len() != sig.params.len() {
        return;
    }
    for (param, decl) in sig.params.iter_mut().zip(names) {
        param.name = non_empty(decl.spelling());
        name_params(&mut param.ty, decl);
    }
}

//! C spellings of Wid types and the typedefs that back them.

use std::fmt::Write as _;

use wid_sema::types::{FloatTy, IntTy, TyId, TyKind};

use crate::Gen;

const C_KEYWORDS: &[&str] = &[
    "auto",
    "break",
    "case",
    "char",
    "const",
    "continue",
    "default",
    "do",
    "double",
    "else",
    "enum",
    "extern",
    "float",
    "for",
    "goto",
    "if",
    "inline",
    "int",
    "long",
    "register",
    "restrict",
    "return",
    "short",
    "signed",
    "sizeof",
    "static",
    "struct",
    "switch",
    "typedef",
    "union",
    "unsigned",
    "void",
    "volatile",
    "while",
    "bool",
    "true",
    "false",
    "nullptr",
    "alignas",
    "alignof",
    "constexpr",
    "static_assert",
    "thread_local",
    "typeof",
    "typeof_unqual",
    "_Atomic",
    "_BitInt",
    "_Complex",
    "_Generic",
    "_Imaginary",
    "_Noreturn",
    "_Decimal32",
    "_Decimal64",
    "_Decimal128",
];

/// Returns a C-safe spelling of a Wid field name.
pub(crate) fn field_ident(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if C_KEYWORDS.contains(&s.as_str()) || s.starts_with("__") {
        s.push('_');
    }
    s
}

impl Gen<'_> {
    /// Returns the C type for a value of type `ty`, emitting its full
    /// definition (and those of its by-value members) if needed.
    pub(crate) fn c_type(&mut self, ty: TyId) -> String {
        let kind = self.p.types.kind(ty).clone();
        match kind {
            TyKind::Unknown | TyKind::Void | TyKind::Never | TyKind::Nil | TyKind::Symbol | TyKind::TypeValue(_) => {
                "void".into()
            }
            TyKind::Bool => "bool".into(),
            TyKind::Int(i) => int_c_type(i).into(),
            TyKind::Float(FloatTy::F32) => "float".into(),
            TyKind::Float(FloatTy::F64) => "double".into(),
            TyKind::Rune => "wid_Rune".into(),
            TyKind::String => "wid_String".into(),
            TyKind::CString => "char *".into(),
            TyKind::RawPtr => "void *".into(),
            TyKind::TypeId | TyKind::Type => "wid_TypeId".into(),
            TyKind::Error => "wid_Error".into(),
            TyKind::Foreign(name) => name.as_str().to_string(),
            TyKind::Pointer(inner) | TyKind::MultiPointer(inner) => {
                let inner = self.pointee_type(inner);
                format!("{inner} *")
            }
            TyKind::Optional(inner) if self.p.types.optional_is_pointer(ty) => self.c_type(inner),
            TyKind::Distinct(id) => {
                let base = self.p.types.distincts[id.0 as usize].base;
                self.c_type(base)
            }
            TyKind::Enum(id) => {
                let info = self.p.types.enum_info(id);
                if info.foreign {
                    return info.c_name.clone();
                }
                self.ensure_named(ty);
                self.type_names[&ty].clone()
            }
            _ => {
                self.ensure_defined(ty);
                self.type_names[&ty].clone()
            }
        }
    }

    /// Returns the C type used behind a pointer; aggregates only need a
    /// forward declaration there.
    fn pointee_type(&mut self, ty: TyId) -> String {
        match self.p.types.kind(ty) {
            TyKind::Struct(_)
            | TyKind::Union(_)
            | TyKind::Tuple(_)
            | TyKind::Slice(_)
            | TyKind::Dynamic(_)
            | TyKind::Array(..)
            | TyKind::Map(..)
            | TyKind::Optional(_)
            | TyKind::Any
            | TyKind::Matrix(..)
                if !self.p.types.optional_is_pointer(ty) =>
            {
                self.ensure_named(ty);
                self.type_names[&ty].clone()
            }
            TyKind::Void | TyKind::Unknown => "void".into(),
            _ => self.c_type(ty),
        }
    }

    /// Assigns a C name to an aggregate type and forward-declares it.
    fn ensure_named(&mut self, ty: TyId) {
        if self.type_names.contains_key(&ty) {
            return;
        }
        if let TyKind::Struct(id) = self.p.types.kind(ty) {
            let info = self.p.types.struct_info(*id).clone();
            if info.foreign {
                self.type_names.insert(ty, info.c_name.clone());
                if !info.opaque && !info.c_name.starts_with("wid_") {
                    let _ = writeln!(
                        self.typedefs,
                        "static_assert(sizeof({0}) == {1} && alignof({0}) == {2}, \"Wid's layout of `{3}` differs from C's\");",
                        info.c_name, info.size, info.align, info.name
                    );
                }
                return;
            }
        }
        let base = self.name_for(ty);
        let mut name = base.clone();
        let mut n = 2;
        while self.used_names.contains(&name) {
            name = format!("{base}_{n}");
            n += 1;
        }
        self.used_names.insert(name.clone());
        self.type_names.insert(ty, name.clone());
        match self.p.types.kind(ty) {
            TyKind::Enum(id) => {
                let info = self.p.types.enum_info(*id).clone();
                let backing = int_c_type(info.backing);
                let _ = writeln!(self.typedefs, "typedef {backing} {name};");
            }
            TyKind::Proc(_) => {}
            _ => {
                let _ = writeln!(self.typedefs, "typedef struct {name} {name};");
            }
        }
    }

    fn name_for(&self, ty: TyId) -> String {
        let t = &self.p.types;
        match t.kind(ty) {
            TyKind::Struct(id) => t.struct_info(*id).c_name.clone(),
            TyKind::Enum(id) => t.enum_info(*id).c_name.clone(),
            TyKind::Union(id) => t.union_info(*id).c_name.clone(),
            _ => {
                let display = t.display(ty);
                let mut s = String::from("wid_");
                let mut last_us = false;
                for c in display.chars() {
                    let mapped = match c {
                        'a'..='z' | 'A'..='Z' | '0'..='9' => Some(c),
                        '[' => {
                            s.push_str(match t.kind(ty) {
                                TyKind::Slice(_) => "Slice_",
                                _ => "",
                            });
                            None
                        }
                        '?' => {
                            s.push_str("Opt");
                            None
                        }
                        '^' => {
                            s.push_str("Ptr");
                            None
                        }
                        _ => None,
                    };
                    match mapped {
                        Some(c) => {
                            s.push(c);
                            last_us = false;
                        }
                        None if !last_us => {
                            s.push('_');
                            last_us = true;
                        }
                        None => {}
                    }
                }
                s.trim_end_matches('_').to_string()
            }
        }
    }

    /// Emits the full definition of an aggregate type after its by-value
    /// dependencies.
    fn ensure_defined(&mut self, ty: TyId) {
        if self.defined.contains(&ty) {
            return;
        }
        self.ensure_named(ty);
        self.defined.insert(ty);
        let name = self.type_names[&ty].clone();
        let kind = self.p.types.kind(ty).clone();
        let mut def = String::new();
        match kind {
            TyKind::Struct(id) => {
                let info = self.p.types.struct_info(id).clone();
                if info.foreign {
                    return;
                }
                let _ = writeln!(def, "struct {name} {{");
                for f in &info.fields {
                    let fty = self.c_type(f.ty);
                    let _ = writeln!(def, "    {fty} {};", field_ident(f.name.as_str()));
                }
                if info.fields.is_empty() {
                    def.push_str("    char unused_;\n");
                }
                def.push_str("};\n");
            }
            TyKind::Tuple(elems) => {
                let _ = writeln!(def, "struct {name} {{");
                for (i, e) in elems.iter().enumerate() {
                    let ety = self.c_type(*e);
                    let _ = writeln!(def, "    {ety} f{i};");
                }
                def.push_str("};\n");
            }
            TyKind::Optional(inner) => {
                let ity = self.c_type(inner);
                let _ = writeln!(def, "struct {name} {{\n    {ity} value;\n    bool has;\n}};");
            }
            TyKind::Slice(inner) => {
                let ity = self.pointee_type(inner);
                let _ = writeln!(def, "struct {name} {{\n    {ity} *data;\n    wid_Int len;\n}};");
            }
            TyKind::Dynamic(inner) => {
                let ity = self.pointee_type(inner);
                let _ = writeln!(
                    def,
                    "struct {name} {{\n    {ity} *data;\n    wid_Int len;\n    wid_Int cap;\n    wid_Allocator allocator;\n}};"
                );
            }
            TyKind::Array(inner, n) => {
                let ity = self.c_type(inner);
                let n = n.max(1);
                let _ = writeln!(def, "struct {name} {{\n    {ity} data[{n}];\n}};");
            }
            TyKind::Matrix(inner, r, c) => {
                let ity = self.c_type(inner);
                let _ = writeln!(def, "struct {name} {{\n    {ity} data[{}];\n}};", u64::from(r) * u64::from(c));
            }
            TyKind::Union(id) => {
                let info = self.p.types.union_info(id).clone();
                let _ = writeln!(def, "struct {name} {{\n    uint32_t tag;\n    union {{");
                for (i, v) in info.variants.iter().enumerate() {
                    let vty = self.c_type(*v);
                    let _ = writeln!(def, "        {vty} v{i};");
                }
                if info.variants.is_empty() {
                    def.push_str("        char unused_;\n");
                }
                def.push_str("    } as;\n};\n");
            }
            TyKind::Map(_, _) => {
                let _ = writeln!(def, "struct {name} {{\n    wid_RawMap raw;\n}};");
            }
            TyKind::Any => {
                let _ = writeln!(def, "struct {name} {{\n    void *data;\n    wid_TypeId id;\n}};");
            }
            TyKind::Proc(sig) => {
                let ret = self.c_type(sig.ret);
                let mut params = Vec::new();
                if sig.abi == wid_sema::types::Abi::Wid {
                    params.push("wid_Context *".to_string());
                }
                for p in &sig.params {
                    params.push(self.c_type(*p));
                }
                if sig.variadic {
                    params.push("...".into());
                }
                if params.is_empty() {
                    params.push("void".into());
                }
                let _ = writeln!(def, "typedef {ret} (*{name})({});", params.join(", "));
            }
            _ => {}
        }
        self.typedefs.push_str(&def);
    }
}

/// Returns the C spelling of an integer type.
pub(crate) fn int_c_type(i: IntTy) -> &'static str {
    match i {
        IntTy::I8 => "int8_t",
        IntTy::I16 => "int16_t",
        IntTy::I32 => "int32_t",
        IntTy::I64 => "int64_t",
        IntTy::U8 => "uint8_t",
        IntTy::U16 => "uint16_t",
        IntTy::U32 => "uint32_t",
        IntTy::U64 => "uint64_t",
        IntTy::Int => "wid_Int",
        IntTy::UInt => "wid_UInt",
    }
}

/// Returns the runtime suffix (`i64`, `u8`, …) of an integer type.
pub(crate) fn int_suffix_of(types: &wid_sema::types::TypeTable, ty: TyId) -> &'static str {
    match types.kind(types.base(ty)) {
        TyKind::Int(IntTy::I8) => "i8",
        TyKind::Int(IntTy::I16) => "i16",
        TyKind::Int(IntTy::I32) => "i32",
        TyKind::Int(IntTy::U8) => "u8",
        TyKind::Int(IntTy::U16) => "u16",
        TyKind::Int(IntTy::U32) => "u32",
        TyKind::Int(IntTy::U64 | IntTy::UInt) => "u64",
        _ => "i64",
    }
}

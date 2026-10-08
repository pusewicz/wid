//! What `type_info` reports about a type. The checker, the compile-time
//! interpreter and the C generator all build their tables from
//! [`describe`], so the three agree.

use std::collections::{HashMap, VecDeque};

use wid_syntax::Name;

use crate::types::{IntTy, TyId, TyKind, TypeTable};

/// A type as the prelude's `TypeInfo` describes it.
#[derive(Clone, Debug)]
pub struct Description {
    /// The name, as Wid displays the type.
    pub name: String,
    /// The `TypeKind` member, like `"struct"`.
    pub kind: &'static str,
    /// The type whose layout gives `size`, `align` and field offsets: the
    /// type itself, or the base of a `distinct` type. `None` when only C
    /// knows the layout (an opaque struct), which reports size and
    /// alignment 0.
    pub layout: Option<TyId>,
    /// The pointee, element, map value, optional payload, enum backing type
    /// or proc return type.
    pub elem: Option<TyId>,
    /// The key type of a map.
    pub key: Option<TyId>,
    /// The length of an array, or the rows of a matrix.
    pub count: u64,
    /// The columns of a matrix.
    pub columns: u64,
    /// Struct fields in declaration order, or proc parameters.
    pub fields: Vec<Field>,
    /// Enum members and their values, in declaration order.
    pub members: Vec<(String, i64)>,
    /// Union variants, in declaration order.
    pub variants: Vec<TyId>,
}

/// A struct field, tuple element or proc parameter.
#[derive(Clone, Debug)]
pub struct Field {
    /// The name; empty for a proc parameter, since a proc type doesn't
    /// keep parameter names.
    pub name: String,
    /// The type.
    pub ty: TyId,
    /// The index of the field in the layout type (a struct field or tuple
    /// element), whose offset C reports; `None` for a proc parameter.
    pub index: Option<u32>,
    /// The offset in Wid's own layout.
    pub offset: u64,
}

impl Description {
    /// The types this description points at, in table order.
    pub fn referenced(&self) -> Vec<TyId> {
        let mut out: Vec<TyId> = self.elem.into_iter().chain(self.key).collect();
        out.extend(self.fields.iter().map(|f| f.ty));
        out.extend(self.variants.iter().copied());
        out
    }
}

/// Describes a type. A `distinct` type reports its base type's kind and
/// details under its own name; `Error` is an enum of the program's error
/// symbols, backed by `U32`.
pub fn describe(types: &TypeTable, errors: &[Name], ty: TyId) -> Description {
    let base = types.base(ty);
    let int = |i: IntTy| types.lookup(&TyKind::Int(i));
    let mut d = Description {
        name: types.display(ty),
        kind: "struct",
        layout: Some(base),
        elem: None,
        key: None,
        count: 0,
        columns: 0,
        fields: Vec::new(),
        members: Vec::new(),
        variants: Vec::new(),
    };
    match types.kind(base) {
        TyKind::Int(i) => d.kind = if i.signed() { "int" } else { "uint" },
        TyKind::Float(_) => d.kind = "float",
        TyKind::Bool => d.kind = "bool",
        TyKind::Rune => d.kind = "rune",
        TyKind::String => d.kind = "string",
        TyKind::CString => d.kind = "cstring",
        TyKind::RawPtr => d.kind = "rawptr",
        TyKind::TypeId => d.kind = "typeid",
        TyKind::Any => d.kind = "any",
        TyKind::Pointer(t) => (d.kind, d.elem) = ("pointer", Some(*t)),
        TyKind::MultiPointer(t) => (d.kind, d.elem) = ("multi_pointer", Some(*t)),
        TyKind::Array(t, n) => (d.kind, d.elem, d.count) = ("array", Some(*t), *n),
        TyKind::Slice(t) => (d.kind, d.elem) = ("slice", Some(*t)),
        TyKind::Dynamic(t) => (d.kind, d.elem) = ("dynamic_array", Some(*t)),
        TyKind::Map(k, v) => (d.kind, d.key, d.elem) = ("map", Some(*k), Some(*v)),
        TyKind::Matrix(t, r, c) => {
            (d.kind, d.elem, d.count, d.columns) = ("matrix", Some(*t), u64::from(*r), u64::from(*c));
        }
        TyKind::Optional(t) => (d.kind, d.elem) = ("optional", Some(*t)),
        TyKind::Proc(sig) => {
            d.kind = "proc";
            d.fields =
                sig.params.iter().map(|p| Field { name: String::new(), ty: *p, index: None, offset: 0 }).collect();
            d.elem = (!matches!(types.kind(sig.ret), TyKind::Void | TyKind::Never)).then_some(sig.ret);
        }
        TyKind::Struct(id) => {
            let info = types.struct_info(*id);
            if info.opaque {
                d.layout = None;
            } else {
                d.fields = info
                    .fields
                    .iter()
                    .enumerate()
                    .map(|(i, f)| Field {
                        name: f.name.as_str().to_string(),
                        ty: f.ty,
                        index: Some(i as u32),
                        offset: f.offset,
                    })
                    .collect();
            }
        }
        TyKind::Tuple(elems) => {
            let parts: Vec<(u64, u64)> = elems.iter().map(|e| types.layout(*e)).collect();
            d.fields = elems
                .iter()
                .zip(crate::types::offsets(&parts))
                .enumerate()
                .map(|(i, (t, offset))| Field { name: i.to_string(), ty: *t, index: Some(i as u32), offset })
                .collect();
        }
        TyKind::Enum(id) => {
            let info = types.enum_info(*id);
            d.kind = "enum";
            d.elem = int(info.backing);
            d.members = info.members.iter().map(|(n, v)| (n.as_str().to_string(), *v as i64)).collect();
        }
        TyKind::Error => {
            d.kind = "enum";
            d.elem = int(IntTy::U32);
            d.members = errors.iter().enumerate().map(|(i, n)| (n.as_str().to_string(), i as i64 + 1)).collect();
        }
        TyKind::Union(id) => {
            d.kind = "union";
            d.variants = types.union_info(*id).variants.clone();
        }
        // Only C knows these; the checker rejects the rest before a table
        // is built.
        _ => d.layout = None,
    }
    d
}

/// Why a type can't be described at run time.
#[derive(Clone, Debug)]
pub enum Undescribable {
    /// No value has the type (`Never`, or nothing at all).
    NoValues,
    /// The type, or a type its table would point at, exists only while
    /// compiling (`Type`). `path` says how it is reached from the
    /// described type, like "has a field `type: Type`".
    CompileTimeOnly {
        /// The compile-time-only type.
        culprit: TyId,
        /// Each step from the described type to the culprit.
        path: Vec<String>,
    },
    /// The type holds an error type (already reported) or a generic
    /// parameter that was never instantiated.
    Poisoned,
}

/// Checks that `ty`, and every type its table points at, can be described
/// at run time.
pub fn check(types: &TypeTable, errors: &[Name], ty: TyId) -> Result<(), Undescribable> {
    if matches!(types.kind(types.base(ty)), TyKind::Never | TyKind::Void) {
        return Err(Undescribable::NoValues);
    }
    let mut parent: HashMap<TyId, Option<(TyId, String)>> = HashMap::new();
    parent.insert(ty, None);
    let mut queue = VecDeque::from([ty]);
    while let Some(t) = queue.pop_front() {
        match types.kind(types.base(t)) {
            // An uninstantiated generic parameter only appears in code that
            // is never lowered for real; there is nothing useful to report.
            TyKind::Unknown | TyKind::Param(_) | TyKind::ConstValue(_) => return Err(Undescribable::Poisoned),
            TyKind::Type | TyKind::Code | TyKind::Symbol | TyKind::Nil | TyKind::TypeValue(_) => {
                let mut path = Vec::new();
                let mut at = t;
                while let Some(Some((from, step))) = parent.get(&at) {
                    path.push(step.clone());
                    at = *from;
                }
                path.reverse();
                return Err(Undescribable::CompileTimeOnly { culprit: t, path });
            }
            _ => {}
        }
        let d = describe(types, errors, t);
        let mut edges: Vec<(TyId, String)> = Vec::new();
        let shown = |x: TyId| types.display(x);
        match d.kind {
            "pointer" | "multi_pointer" => edges.extend(d.elem.map(|e| (e, format!("points at `{}`", shown(e))))),
            "proc" => {
                edges.extend(d.fields.iter().map(|f| (f.ty, format!("takes `{}`", shown(f.ty)))));
                edges.extend(d.elem.map(|e| (e, format!("returns `{}`", shown(e)))));
            }
            "map" => {
                edges.extend(d.key.map(|k| (k, format!("has `{}` keys", shown(k)))));
                edges.extend(d.elem.map(|e| (e, format!("holds `{}` values", shown(e)))));
            }
            "enum" => {}
            _ => {
                edges.extend(d.elem.map(|e| (e, format!("holds `{}`", shown(e)))));
                edges.extend(d.fields.iter().map(|f| (f.ty, format!("has a field `{}: {}`", f.name, shown(f.ty)))));
                edges.extend(d.variants.iter().map(|v| (*v, format!("can hold `{}`", shown(*v)))));
            }
        }
        for (next, step) in edges {
            if let std::collections::hash_map::Entry::Vacant(slot) = parent.entry(next) {
                slot.insert(Some((t, step)));
                queue.push_back(next);
            }
        }
    }
    Ok(())
}

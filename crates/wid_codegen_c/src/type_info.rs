//! The tables behind `type_info`: one read-only static object,
//! `wid_typeinfo`, holding a `TypeInfo` for every type the program describes
//! and for every type those point at, plus the field, member and variant
//! arrays they view. Everything lives in the one object so entries can
//! point at each other (recursive types) without forward declarations.
//! Sizes, alignments and field offsets come from the C compiler (`sizeof`,
//! `alignof`, `offsetof`), so they are exact for layouts C defines.

use std::fmt::Write as _;

use wid_sema::type_info::{Description, describe};
use wid_sema::types::{TyId, TyKind};

use crate::{Gen, c_string_literal};

/// The C name of the table object.
const TABLE: &str = "wid_typeinfo";

/// The arrays of the table object.
#[derive(Default)]
struct Arrays {
    types: Vec<String>,
    fields: Vec<String>,
    members: Vec<String>,
    variants: Vec<String>,
}

/// The prelude records the table is made of.
struct Records {
    info: TyId,
    info_c: String,
    field: Option<TyId>,
    member: Option<TyId>,
    variant: Option<TyId>,
    /// `TypeKind`'s members and their values.
    kinds: Vec<(String, i128)>,
}

impl Gen<'_> {
    /// A C expression for `type_info(described)`: a pointer of type
    /// `ptr_ty` (`^TypeInfo`) to its table entry.
    pub(crate) fn type_info_ref(&mut self, described: TyId, ptr_ty: TyId) -> String {
        if let TyKind::Pointer(info) = *self.p.types.kind(ptr_ty) {
            self.type_info_struct.get_or_insert(info);
        }
        let i = self.type_info_slot(described);
        let cty = self.c_type(ptr_ty);
        format!("(({cty})&{TABLE}.types[{i}])")
    }

    /// The index of a type's entry, giving it the next one the first time.
    fn type_info_slot(&mut self, ty: TyId) -> usize {
        if let Some(&i) = self.type_info_index.get(&ty) {
            return i;
        }
        let i = self.type_infos.len();
        self.type_infos.push(ty);
        self.type_info_index.insert(ty, i);
        i
    }

    /// Defines the table, once every function that uses `type_info` has
    /// been emitted. Entries are in the order the program first describes
    /// them, followed by the types they point at.
    pub(crate) fn type_info_table(&mut self) -> String {
        let Some(info) = self.type_info_struct else { return String::new() };
        let Some(records) = self.records(info) else { return String::new() };
        let mut descs: Vec<Description> = Vec::new();
        while descs.len() < self.type_infos.len() {
            let d = describe(&self.p.types, &self.p.errors, self.type_infos[descs.len()]);
            for t in d.referenced() {
                self.type_info_slot(t);
            }
            descs.push(d);
        }
        let mut arrays = Arrays::default();
        for (i, d) in descs.iter().enumerate() {
            let entry = self.entry(d, &records, &mut arrays);
            arrays.types.push(format!("/* {i} */ {entry}"));
        }

        let mut out = String::from("/* The tables `type_info` points into, one entry per described type. */\n");
        out.push_str("static const struct {\n");
        let _ = writeln!(out, "    {} types[{}];", records.info_c, arrays.types.len());
        let parts = [
            (records.field, "fields", &arrays.fields),
            (records.member, "members", &arrays.members),
            (records.variant, "variants", &arrays.variants),
        ];
        for (ty, name, items) in parts {
            if let (Some(t), false) = (ty, items.is_empty()) {
                let c = self.c_type(t);
                let sep = if c.ends_with('*') { "" } else { " " };
                let _ = writeln!(out, "    {c}{sep}{name}[{}];", items.len());
            }
        }
        let _ = writeln!(out, "}} {TABLE} = {{");
        let all = [
            ("types", &arrays.types),
            ("fields", &arrays.fields),
            ("members", &arrays.members),
            ("variants", &arrays.variants),
        ];
        for (name, items) in all {
            if items.is_empty() {
                continue;
            }
            let _ = writeln!(out, "    .{name} = {{");
            for item in items {
                let _ = writeln!(out, "        {item},");
            }
            out.push_str("    },\n");
        }
        out.push_str("};\n\n");
        out
    }

    /// The prelude's `TypeInfo` and the records its slices hold.
    fn records(&mut self, info: TyId) -> Option<Records> {
        let TyKind::Struct(sid) = *self.p.types.kind(info) else { return None };
        let fields = &self.p.types.struct_info(sid).fields;
        let slot = |name: &str| fields.iter().find(|f| f.name.as_str() == name).map(|f| f.ty);
        let elem = |t: Option<TyId>| match t.map(|t| self.p.types.kind(t)) {
            Some(TyKind::Slice(e)) => Some(*e),
            _ => None,
        };
        let kinds = match slot("kind").map(|t| self.p.types.kind(t)) {
            Some(TyKind::Enum(id)) => {
                self.p.types.enum_info(*id).members.iter().map(|(n, v)| (n.as_str().to_string(), *v)).collect()
            }
            _ => Vec::new(),
        };
        let (field, member, variant) = (elem(slot("fields")), elem(slot("members")), elem(slot("variants")));
        let info_c = self.c_type(info);
        Some(Records { info, info_c, field, member, variant, kinds })
    }

    /// The initializer of one `TypeInfo`, adding its fields, members and
    /// variants to the table's arrays.
    fn entry(&mut self, d: &Description, r: &Records, arrays: &mut Arrays) -> String {
        let (size, align) = match d.layout {
            Some(l) => {
                let c = self.c_type(l);
                (format!("(wid_Int)sizeof({c})"), format!("(wid_Int)alignof({c})"))
            }
            None => ("0".to_string(), "0".to_string()),
        };
        let fields = self.field_inits(d, r);
        let members: Vec<String> = match r.member {
            Some(m) => d
                .members
                .iter()
                .map(|(name, value)| {
                    let v = if *value == i64::MIN { "INT64_MIN".to_string() } else { format!("INT64_C({value})") };
                    self.record_init(m, |slot| match slot {
                        "name" => Some(string_init(name)),
                        "value" => Some(v.clone()),
                        _ => None,
                    })
                })
                .collect(),
            None => Vec::new(),
        };
        let variants: Vec<String> = d.variants.iter().map(|v| self.entry_ptr(*v, r)).collect();
        let TyKind::Struct(sid) = *self.p.types.kind(r.info) else { return "{}".into() };
        let names: Vec<String> =
            self.p.types.struct_info(sid).fields.iter().map(|f| f.name.as_str().to_string()).collect();
        let mut parts = Vec::with_capacity(names.len());
        for (i, name) in names.iter().enumerate() {
            let value = match name.as_str() {
                "name" => string_init(&d.name),
                "kind" => r.kinds.iter().find(|(n, _)| n == d.kind).map_or_else(|| "0".into(), |(_, v)| v.to_string()),
                "size" => size.clone(),
                "align" => align.clone(),
                "elem" => self.optional_ptr(d.elem, r),
                "key" => self.optional_ptr(d.key, r),
                "count" => d.count.to_string(),
                "columns" => d.columns.to_string(),
                "fields" => self.append(r.field, "fields", &mut arrays.fields, &fields),
                "members" => self.append(r.member, "members", &mut arrays.members, &members),
                "variants" => self.append(r.variant, "variants", &mut arrays.variants, &variants),
                _ => "{}".to_string(),
            };
            parts.push(format!(".{} = {value}", self.field_name(r.info, i as u32)));
        }
        format!("{{{}}}", parts.join(", "))
    }

    /// The `TypeInfoField` initializers of a description's fields.
    fn field_inits(&mut self, d: &Description, r: &Records) -> Vec<String> {
        let Some(record) = r.field else { return Vec::new() };
        let mut out = Vec::with_capacity(d.fields.len());
        for field in &d.fields {
            let offset = match (d.layout, field.index) {
                (Some(l), Some(index)) => {
                    let c = self.c_type(l);
                    let member = self.field_name(l, index);
                    format!("(wid_Int)offsetof({c}, {member})")
                }
                _ => "0".to_string(),
            };
            let target = self.entry_ptr(field.ty, r);
            out.push(self.record_init(record, |slot| match slot {
                "name" => Some(string_init(&field.name)),
                "type" => Some(target.clone()),
                "offset" => Some(offset.clone()),
                _ => None,
            }));
        }
        out
    }

    /// Adds `items` to one of the table's arrays and returns the slice
    /// that views them.
    fn append(&mut self, elem: Option<TyId>, array: &str, into: &mut Vec<String>, items: &[String]) -> String {
        let Some(elem) = elem else { return "{}".into() };
        if items.is_empty() {
            return "{}".into();
        }
        let start = into.len();
        into.extend_from_slice(items);
        let c = self.c_type(elem);
        format!("{{.data = ({})&{TABLE}.{array}[{start}], .len = {}}}", pointer_to(&c), items.len())
    }

    /// The address of a type's entry, as the non-`const` pointer Wid reads
    /// it through.
    fn entry_ptr(&mut self, ty: TyId, r: &Records) -> String {
        let i = self.type_info_slot(ty);
        format!("({})&{TABLE}.types[{i}]", pointer_to(&r.info_c))
    }

    fn optional_ptr(&mut self, ty: Option<TyId>, r: &Records) -> String {
        match ty {
            Some(t) => self.entry_ptr(t, r),
            None => "nullptr".into(),
        }
    }

    /// A designated initializer for a prelude record (`TypeInfoField`,
    /// `TypeInfoMember`), with each field's value by name.
    fn record_init(&self, ty: TyId, value: impl Fn(&str) -> Option<String>) -> String {
        let TyKind::Struct(id) = *self.p.types.kind(ty) else { return "{}".into() };
        let parts: Vec<String> = self
            .p
            .types
            .struct_info(id)
            .fields
            .iter()
            .enumerate()
            .map(|(i, f)| {
                let v = value(f.name.as_str()).unwrap_or_else(|| "{}".into());
                format!(".{} = {v}", self.field_name(ty, i as u32))
            })
            .collect();
        format!("{{{}}}", parts.join(", "))
    }
}

/// The C spelling of a pointer to the C type `c`.
fn pointer_to(c: &str) -> String {
    if c.ends_with('*') { format!("{c}*") } else { format!("{c} *") }
}

/// A static initializer for a `String`.
fn string_init(s: &str) -> String {
    format!("{{(const uint8_t *){}, {}}}", c_string_literal(s.as_bytes()), s.len())
}

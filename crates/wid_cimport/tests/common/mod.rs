//! Helpers shared by the integration tests.
#![allow(dead_code)]

use std::path::{Path, PathBuf};

use wid_cimport::*;

/// The path of a fixture header.
pub fn fixture(relative: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures").join(relative)
}

/// Imports a fixture header with default settings.
pub fn import_fixture(relative: &str) -> CModule {
    match import(&ImportRequest::new(Header::Path(fixture(relative)))) {
        Ok(module) => module,
        Err(error) => panic!("importing {relative} failed: {error}"),
    }
}

/// The item named `name` of the given kind, or a panic naming what is there.
pub fn item<'m>(module: &'m CModule, name: &str) -> &'m Item {
    module.find(name).unwrap_or_else(|| panic!("no item named {name}"))
}

/// The function named `name`.
pub fn function<'m>(module: &'m CModule, name: &str) -> &'m Function {
    match &item(module, name).kind {
        ItemKind::Function(function) => function,
        other => panic!("{name} is not a function: {other:?}"),
    }
}

/// The record named `name` (by typedef name or tag).
pub fn record<'m>(module: &'m CModule, name: &str) -> &'m Record {
    match &item(module, name).kind {
        ItemKind::Record(record) => record,
        other => panic!("{name} is not a record: {other:?}"),
    }
}

/// The enum named `name` (by typedef name or tag).
pub fn enumeration<'m>(module: &'m CModule, name: &str) -> &'m Enum {
    match &item(module, name).kind {
        ItemKind::Enum(enumeration) => enumeration,
        other => panic!("{name} is not an enum: {other:?}"),
    }
}

/// The typedef named `name`.
pub fn typedef<'m>(module: &'m CModule, name: &str) -> &'m Typedef {
    match &item(module, name).kind {
        ItemKind::Typedef(typedef) => typedef,
        other => panic!("{name} is not a typedef: {other:?}"),
    }
}

/// The global named `name`.
pub fn global<'m>(module: &'m CModule, name: &str) -> &'m Global {
    match &item(module, name).kind {
        ItemKind::Global(global) => global,
        other => panic!("{name} is not a global: {other:?}"),
    }
}

/// The macro named `name`.
pub fn macro_def<'m>(module: &'m CModule, name: &str) -> &'m Macro {
    match &item(module, name).kind {
        ItemKind::Macro(definition) => definition,
        other => panic!("{name} is not a macro: {other:?}"),
    }
}

/// The type and value of an expression macro.
pub fn macro_expr<'m>(module: &'m CModule, name: &str) -> (&'m CType, Option<&'m MacroValue>) {
    match &macro_def(module, name).kind {
        MacroKind::Expr { ty, value } => (ty, value.as_ref()),
        other => panic!("{name} is not an expression macro: {other:?}"),
    }
}

/// The field of a record body named `name`.
pub fn field<'r>(record: &'r Record, name: &str) -> &'r Field {
    let body = record.body.as_ref().expect("record has a body");
    body.fields.iter().find(|field| field.name.as_deref() == Some(name)).unwrap_or_else(|| panic!("no field {name}"))
}

/// A builtin integer type.
pub fn int(bits: u32, signed: bool, spelling: &str) -> CType {
    CType::Int(IntType { bits, signed, spelling: spelling.to_string() })
}

/// A reference to a named type.
pub fn named(kind: NamedKind, name: &str) -> CType {
    CType::Named(Named { kind, name: name.to_string() })
}

/// A pointer type with the given pointee qualifiers.
pub fn pointer(pointee: CType, pointee_quals: Quals) -> CType {
    CType::Pointer(Box::new(PointerType { pointee, pointee_quals }))
}

/// Qualifiers with only `const` set.
pub const CONST: Quals = Quals { is_const: true, is_volatile: false, is_restrict: false };

/// No qualifiers.
pub const NONE: Quals = Quals { is_const: false, is_volatile: false, is_restrict: false };

/// Calls `visit` on every type in the module, recursively.
pub fn each_type(module: &CModule, visit: &mut dyn FnMut(&CType)) {
    for item in &module.items {
        match &item.kind {
            ItemKind::Function(function) => each_sig_type(&function.sig, visit),
            ItemKind::Record(record) => each_record_type(record, visit),
            ItemKind::Enum(enumeration) => each_nested_type(&enumeration.underlying, visit),
            ItemKind::Typedef(typedef) => each_nested_type(&typedef.ty, visit),
            ItemKind::Global(global) => each_nested_type(&global.ty, visit),
            ItemKind::Macro(definition) => {
                if let MacroKind::Expr { ty, .. } = &definition.kind {
                    each_nested_type(ty, visit);
                }
            }
        }
    }
}

/// Visits the types in a signature.
fn each_sig_type(sig: &FnSig, visit: &mut dyn FnMut(&CType)) {
    each_nested_type(&sig.ret, visit);
    for param in &sig.params {
        each_nested_type(&param.ty, visit);
    }
}

/// Visits the field types of a record.
fn each_record_type(record: &Record, visit: &mut dyn FnMut(&CType)) {
    for field in record.body.iter().flat_map(|body| &body.fields) {
        each_nested_type(&field.ty, visit);
    }
}

/// Visits a type and everything inside it.
fn each_nested_type(ty: &CType, visit: &mut dyn FnMut(&CType)) {
    visit(ty);
    match ty {
        CType::Pointer(pointer) => each_nested_type(&pointer.pointee, visit),
        CType::Array(array) => each_nested_type(&array.element, visit),
        CType::FnPtr(sig) | CType::Function(sig) => each_sig_type(sig, visit),
        CType::Record(record) => each_record_type(record, visit),
        CType::Atomic(inner) => each_nested_type(inner, visit),
        _ => {}
    }
}

/// Asserts the structural invariants every import must satisfy: named types
/// resolve, items come from inside the root, and nothing is imported twice.
pub fn assert_consistent(module: &CModule) {
    let mut unresolved = Vec::new();
    each_type(module, &mut |ty| {
        if let CType::Named(named) = ty
            && module.resolve(named).is_none()
        {
            unresolved.push(named.clone());
        }
    });
    unresolved.dedup();
    assert!(unresolved.is_empty(), "unresolved named types: {unresolved:?}");
    for item in &module.items {
        let real = std::fs::canonicalize(&item.location.file).expect("item file exists");
        assert!(real.starts_with(&module.root), "{:?} is outside {}", item.name(), module.root.display());
    }
    let mut seen = std::collections::HashSet::new();
    for item in &module.items {
        let key = match &item.kind {
            ItemKind::Record(record) => format!("tag {:?} {:?}", record.tag, record.typedef_name),
            ItemKind::Enum(enumeration) if enumeration.tag.is_some() => format!("tag {:?}", enumeration.tag),
            ItemKind::Enum(_) => continue,
            ItemKind::Macro(definition) => format!("macro {}", definition.name),
            _ => format!("ordinary {:?}", item.name()),
        };
        assert!(seen.insert(key.clone()), "duplicate item {key}");
    }
}

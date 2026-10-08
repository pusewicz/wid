//! The JSON document `wid query` prints (SPEC "Toolchain and CLI"):
//! `query` (its name), `symbol` (the path asked about, or `null`),
//! `package` (`name`, `path`, `doc`, or `null` when it couldn't be
//! loaded) and `results`.
//!
//! - `outline`: one item per declaration with `kind`, `name`, `path`,
//!   `package`, `signature`, `attributes`, `private`, `summary` (the first
//!   paragraph of its doc), `location`, `span`, and where they apply
//!   `static`, `c` and `children` (a struct's fields, an enum's members,
//!   then the methods, constants and overload sets written in it).
//! - `def`: items in `wid doc -json`'s shape (see [`item_json`]) with
//!   `span`.
//! - `methods`: groups, each with an `origin` and its `methods`.
//! - `refs` and `calls`: uses, each with `location`, `kind` (`read`,
//!   `write`, `call`, `type`, `import` or `declaration`), `context` (the
//!   path of the declaration it is in, or `null`) and, for a use in code a
//!   macro generated, `via_macro`.
//! - `type`: one result with `location` (the name or code at the
//!   position), `span` (what the type is of), `type` (or `null`), `kind`
//!   and, where they apply, `instances` and `refers_to` (a `def` item).
//!
//! Every location has `file`, `line`, `column`, `end_line` and
//! `end_column`. Object keys are sorted and lists keep the engine's
//! order, so the same program always gives the same document.

use serde_json::{Value, json};
use wid_syntax::docs::first_paragraph;

use crate::item::{Item, PackageInfo, Style, c_json, item_json, location_json, origin_json, package_json};
use crate::{Answer, MethodGroup, Query, RefItem, TypeItem};

/// The document for a query: its answer, or empty `results` when it
/// failed (the diagnostics say why).
pub fn document(query: &Query, package: Option<&PackageInfo>, answer: Option<&Answer>) -> Value {
    let results: Vec<Value> = match answer {
        None => Vec::new(),
        Some(Answer::Outline(items)) => items.iter().map(outline_json).collect(),
        Some(Answer::Def(items)) => items.iter().map(|i| item_json(i, Style::Query)).collect(),
        Some(Answer::Methods(groups)) => groups.iter().map(group_json).collect(),
        Some(Answer::Refs(refs)) => refs.iter().map(ref_json).collect(),
        Some(Answer::Type(found)) => vec![type_json(found)],
    };
    json!({
        "query": query.name(),
        "symbol": query.symbol(),
        "package": package.map(package_json),
        "results": results,
    })
}

/// A document as text: pretty-printed, keys sorted.
pub fn render(document: &Value) -> String {
    serde_json::to_string_pretty(document).unwrap_or_default()
}

/// A declaration in an outline.
pub fn outline_json(e: &Item) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("kind".into(), json!(e.kind));
    map.insert("name".into(), json!(e.name));
    map.insert("path".into(), json!(e.path));
    map.insert("package".into(), json!(e.package));
    map.insert("signature".into(), json!(e.signature));
    map.insert("attributes".into(), json!(e.attributes));
    map.insert("private".into(), json!(e.private));
    map.insert("summary".into(), json!(e.doc.as_deref().map(first_paragraph)));
    map.insert("location".into(), location_json(&e.location, Style::Query));
    map.insert("span".into(), location_json(&e.span, Style::Query));
    if e.kind == "method" || e.kind == "macro" {
        map.insert("static".into(), json!(e.is_static));
    }
    if let Some(c) = &e.c {
        map.insert("c".into(), c_json(c));
    }
    if matches!(e.kind, "struct" | "enum" | "union" | "module" | "extension") {
        let children: Vec<Value> = e.fields.iter().chain(&e.members).chain(&e.methods).map(outline_json).collect();
        map.insert("children".into(), json!(children));
    }
    Value::Object(map)
}

/// A group of methods: its `origin` and its `methods`.
pub fn group_json(group: &MethodGroup) -> Value {
    json!({
        "origin": origin_json(&group.origin, Style::Query),
        "methods": group.methods.iter().map(|m| item_json(m, Style::Query)).collect::<Vec<_>>(),
    })
}

/// A use: `location`, `kind`, `context` and, from a macro, `via_macro`.
pub fn ref_json(r: &RefItem) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("location".into(), location_json(&Some(r.location.clone()), Style::Query));
    map.insert("kind".into(), json!(r.kind.as_str()));
    map.insert("context".into(), json!(r.context));
    if let Some(m) = &r.via_macro {
        map.insert("via_macro".into(), json!(m));
    }
    Value::Object(map)
}

/// What is at a position: `location`, `span`, `type`, `kind`, and where
/// they apply `instances` and `refers_to`.
pub fn type_json(t: &TypeItem) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("location".into(), location_json(&Some(t.location.clone()), Style::Query));
    map.insert("span".into(), location_json(&Some(t.span.clone()), Style::Query));
    map.insert("type".into(), json!(t.ty));
    map.insert("kind".into(), json!(t.kind));
    if !t.instances.is_empty() {
        map.insert("instances".into(), json!(t.instances));
    }
    if let Some(item) = &t.refers_to {
        map.insert("refers_to".into(), item_json(item, Style::Query));
    }
    Value::Object(map)
}

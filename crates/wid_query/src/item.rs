//! Declarations as `wid doc` and `wid query` show them: an [`ItemBuilder`]
//! turns what the symbol index holds into [`Item`]s, plain data that
//! [`item_json`] renders in the JSON shape both commands share.

use serde_json::{Value, json};
use wid_diagnostics::{SourceMap, Span};
use wid_sema::PackageId;
use wid_sema::index::{CDoc, Index, MemberGroup, Origin, SymbolId, SymbolKind, Target};

use crate::extent::Extents;

/// A range of a source file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    /// The file, as diagnostics show it.
    pub file: String,
    /// The 1-based line where it starts.
    pub line: u32,
    /// The 1-based column where it starts.
    pub column: u32,
    /// The 1-based line where it ends.
    pub end_line: u32,
    /// The 1-based column just past its last character.
    pub end_column: u32,
}

/// A package, as a page or an answer names it.
#[derive(Clone, Debug)]
pub struct PackageInfo {
    /// The name: `fmt`.
    pub name: String,
    /// The import path: `core:fmt`, `.`, `cimport:raylib.h`.
    pub path: String,
    /// The package doc.
    pub doc: Option<String>,
}

/// Where a member listed on a type comes from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginInfo {
    /// `own`, `include`, `extend` or `using`.
    pub kind: &'static str,
    /// What brings the member in, as written: `include Greeter`,
    /// `extend Ball`, `using base: Entity`; empty for the type's own.
    pub via: String,
    /// Where that is declared.
    pub location: Option<Location>,
}

impl OriginInfo {
    /// The origin of a type's own members.
    pub fn own() -> OriginInfo {
        OriginInfo { kind: "own", via: String::new(), location: None }
    }
}

/// One declaration, and what is listed under it.
#[derive(Clone, Debug)]
pub struct Item {
    /// `constant`, `type_alias`, `struct`, `enum`, `union`, `module`,
    /// `method`, `macro`, `overload`, `extension`, `field`, `enum_member`,
    /// `builtin_type` or `package`.
    pub kind: &'static str,
    /// The name.
    pub name: String,
    /// The symbol path that names it: `Ball.update`.
    pub path: String,
    /// The import path of the package that declares it.
    pub package: String,
    /// The declaration line: `def update(dt: F32)`, `vel: Vec2 = [1.0, 0.0]`.
    pub signature: String,
    /// The attributes written before it: `extern("InitWindow")`.
    pub attributes: Vec<String>,
    /// The doc comment.
    pub doc: Option<String>,
    /// Whether it was declared `private`.
    pub private: bool,
    /// `def self.name`.
    pub is_static: bool,
    /// Where its name is; `None` for C declarations, builtin types and
    /// packages.
    pub location: Option<Location>,
    /// The whole declaration, from its attributes (or `private`) to its
    /// last token; `None` where `location` is.
    pub span: Option<Location>,
    /// The type, module or extension that declares it: its kind (`struct`)
    /// and path (`Ball`) or declaration line (`extend String`).
    pub owner: Option<(&'static str, String)>,
    /// For a C declaration, its C name and header.
    pub c: Option<CDoc>,
    /// For a member listed on a type, where it comes from.
    pub origin: Option<OriginInfo>,
    /// For a field named through a struct that `using` promotes it into,
    /// that struct (`Player` for `Player.hp`).
    pub promoted_into: Option<String>,
    /// A struct's fields.
    pub fields: Vec<Item>,
    /// An enum's members.
    pub members: Vec<Item>,
    /// A union's variant types.
    pub variants: Vec<String>,
    /// An extension's target types.
    pub targets: Vec<String>,
    /// The type a type alias stands for, as written.
    pub aliases: Option<String>,
    /// Methods, type-level constants and overload sets, each with its
    /// origin; for an overload set, its members.
    pub methods: Vec<Item>,
}

impl Item {
    /// An item with nothing but its kind, names and declaration line.
    pub fn blank(kind: &'static str, name: &str, path: &str, signature: &str) -> Item {
        Item {
            kind,
            name: name.to_string(),
            path: path.to_string(),
            package: String::new(),
            signature: signature.to_string(),
            attributes: Vec::new(),
            doc: None,
            private: false,
            is_static: false,
            location: None,
            span: None,
            owner: None,
            c: None,
            origin: None,
            promoted_into: None,
            fields: Vec::new(),
            members: Vec::new(),
            variants: Vec::new(),
            targets: Vec::new(),
            aliases: None,
            methods: Vec::new(),
        }
    }
}

/// A symbol kind in prose: `struct`, `type alias`, `overload set`.
pub fn kind_words(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Constant => "constant",
        SymbolKind::TypeAlias => "type alias",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Union => "union",
        SymbolKind::Module => "module",
        SymbolKind::Method => "method",
        SymbolKind::Macro => "macro",
        SymbolKind::Overload => "overload set",
        SymbolKind::Extension => "extension",
    }
}

/// Builds [`Item`]s from the index.
#[derive(Clone, Copy)]
pub struct ItemBuilder<'a> {
    /// The program's declarations.
    pub index: &'a Index,
    /// Its files, for locations.
    pub sources: &'a SourceMap,
    /// Where its declarations end.
    pub extents: &'a Extents,
    /// List private members under types too.
    pub private: bool,
}

impl ItemBuilder<'_> {
    /// Whether a symbol is listed: it is public, or private ones are.
    pub fn shown(&self, id: SymbolId) -> bool {
        self.private || self.index.is_public(id)
    }

    /// The package `pkg`.
    pub fn package_info(&self, pkg: PackageId) -> PackageInfo {
        let p = self.index.package(pkg);
        PackageInfo { name: p.name.clone(), path: p.path.clone(), doc: p.doc.clone() }
    }

    /// Where a span is; `None` for the default span of a declaration that
    /// has no place in a file.
    pub fn location(&self, span: Span) -> Option<Location> {
        if span == Span::default() {
            return None;
        }
        let file = self.sources.file(span.file);
        let (line, column) = file.line_col(span.start);
        let (end_line, end_column) = file.line_col(span.end);
        Some(Location { file: file.display.clone(), line, column, end_line, end_column })
    }

    /// Where the whole declaration whose name is at `name` is.
    fn extent(&self, name: Span) -> Option<Location> {
        if name == Span::default() {
            return None;
        }
        self.location(self.extents.declaration(self.sources, name).unwrap_or(name))
    }

    /// The item of a symbol, without what is listed under it.
    pub fn entry(&self, id: SymbolId) -> Item {
        let s = self.index.symbol(id);
        let owner = s.owner.map(|o| {
            let os = self.index.symbol(o);
            let kind = match os.kind {
                SymbolKind::Extension => "extension",
                k => kind_words(k),
            };
            let shown = if os.kind == SymbolKind::Extension { os.signature.clone() } else { self.index.path_of(o) };
            (kind, shown)
        });
        let in_c = s.c.is_some();
        Item {
            package: self.index.package(s.package).path.clone(),
            attributes: s.attributes.clone(),
            doc: s.doc.clone(),
            private: s.private,
            is_static: s.is_static,
            location: if in_c { None } else { self.location(s.span) },
            span: if in_c { None } else { self.extent(s.span) },
            owner,
            c: s.c.clone(),
            ..Item::blank(s.kind.as_str(), &s.name, &self.index.path_of(id), &s.signature)
        }
    }

    /// A symbol with what it declares itself: a struct's fields, an enum's
    /// members, a union's variants, an extension's targets, an alias's
    /// type, and the methods written in a type, module or extension.
    pub fn overview_item(&self, id: SymbolId) -> Item {
        let s = self.index.symbol(id);
        let mut e = self.entry(id);
        match s.kind {
            SymbolKind::Struct => {
                e.fields = (0..s.fields.len()).map(|i| self.field_item(id, i, Some(OriginInfo::own()))).collect();
            }
            SymbolKind::Enum => e.members = (0..s.enum_members.len()).map(|i| self.member_item(id, i)).collect(),
            SymbolKind::Union => e.variants = s.links.iter().map(|l| l.text.clone()).collect(),
            SymbolKind::Extension => e.targets = s.links.iter().map(|l| l.text.clone()).collect(),
            SymbolKind::TypeAlias => e.aliases = s.links.first().map(|l| l.text.clone()),
            _ => {}
        }
        if matches!(s.kind, SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Module | SymbolKind::Extension) {
            let own = MemberGroup { origin: Origin::Own, members: s.members.clone() };
            e.methods = self.group_items(&[own]);
        }
        e
    }

    /// A symbol with everything its documentation lists: for a type, its
    /// fields, members and methods from every origin.
    pub fn full_item(&self, id: SymbolId) -> Item {
        let s = self.index.symbol(id);
        let mut e = self.entry(id);
        match s.kind {
            SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Union | SymbolKind::Module => {
                self.add_members(&mut e, id)
            }
            SymbolKind::TypeAlias => {
                e.aliases = s.links.first().map(|l| l.text.clone());
                let target = self.index.alias_target(id);
                if target != id && self.index.symbol(target).kind.has_members() {
                    self.add_members(&mut e, target);
                }
            }
            SymbolKind::Extension => {
                e.targets = s.links.iter().map(|l| l.text.clone()).collect();
                let own = MemberGroup { origin: Origin::Own, members: s.members.clone() };
                e.methods = self.group_items(&[own]);
            }
            SymbolKind::Overload => {
                e.methods =
                    s.links.iter().filter_map(|l| l.symbol).filter(|&m| self.shown(m)).map(|m| self.entry(m)).collect();
            }
            SymbolKind::Constant | SymbolKind::Method | SymbolKind::Macro => {}
        }
        e
    }

    /// The item of what a symbol path named, in full. A package (an
    /// import name) becomes a `package` item named `path`.
    pub fn target_item(&self, target: &Target, path: &str) -> Item {
        match target {
            Target::Package(p) => self.package_item(*p, path),
            Target::Symbol(id) => self.full_item(*id),
            Target::Field { owner, index, promoted } => {
                let origin = promoted.as_ref().map(|(_, path)| OriginInfo {
                    kind: "using",
                    via: format!("using {path}: {}", self.index.symbol(*owner).name),
                    location: None,
                });
                let mut e = self.field_item(*owner, *index, origin);
                if let Some((into, _)) = promoted {
                    e.promoted_into = Some(self.index.path_of(*into));
                }
                e
            }
            Target::EnumMember { owner, index } => self.member_item(*owner, *index),
            Target::Builtin(name) => Item {
                methods: self.group_items(&self.index.builtin_groups(name)),
                ..Item::blank("builtin_type", name, name, name)
            },
        }
    }

    /// An imported package, named by the import name `alias`: its
    /// declaration line is the `import` or `cimport` that binds the name.
    pub fn package_item(&self, pkg: PackageId, alias: &str) -> Item {
        let p = self.index.package(pkg);
        let signature = match &p.header {
            Some(header) => format!("cimport \"{header}\", as: :{alias}"),
            None if p.name == alias => format!("import \"{}\"", p.path),
            None => format!("import \"{}\", as: :{alias}", p.path),
        };
        Item { package: p.path.clone(), doc: p.doc.clone(), ..Item::blank("package", alias, alias, &signature) }
    }

    /// Adds a type's fields, members and methods from every origin.
    fn add_members(&self, e: &mut Item, ty: SymbolId) {
        let s = self.index.symbol(ty);
        e.fields = self
            .index
            .fields_of(ty)
            .into_iter()
            .map(|f| {
                let origin = match &f.promoted_through {
                    Some(path) => {
                        let through = self.index.symbol(f.owner);
                        OriginInfo { kind: "using", via: format!("using {path}: {}", through.name), location: None }
                    }
                    None => OriginInfo::own(),
                };
                self.field_item(f.owner, f.index, Some(origin))
            })
            .collect();
        e.members = (0..s.enum_members.len()).map(|i| self.member_item(ty, i)).collect();
        e.variants =
            if s.kind == SymbolKind::Union { s.links.iter().map(|l| l.text.clone()).collect() } else { Vec::new() };
        e.methods = self.group_items(&self.index.member_groups(ty));
    }

    /// The members of groups, each with its origin, leaving out private
    /// ones unless they are listed.
    pub fn group_items(&self, groups: &[MemberGroup]) -> Vec<Item> {
        let mut out = Vec::new();
        for group in groups {
            let origin = self.origin(&group.origin);
            for &m in &group.members {
                if !self.shown(m) {
                    continue;
                }
                let mut e = self.entry(m);
                e.origin = Some(origin.clone());
                out.push(e);
            }
        }
        out
    }

    /// Where a group of members comes from, as written.
    pub fn origin(&self, origin: &Origin) -> OriginInfo {
        match origin {
            Origin::Own => OriginInfo::own(),
            Origin::Include { module, by } => {
                let m = self.index.symbol(*module);
                let b = self.index.symbol(*by);
                let mut via = format!("include {}", self.qualified(*module));
                match b.kind {
                    SymbolKind::Module => via.push_str(&format!(" (through module {})", self.index.path_of(*by))),
                    SymbolKind::Extension => via.push_str(&format!(" (through {})", b.signature)),
                    _ => {}
                }
                OriginInfo { kind: "include", via, location: self.location(m.span) }
            }
            Origin::Extend { extension } => {
                let x = self.index.symbol(*extension);
                OriginInfo { kind: "extend", via: x.signature.clone(), location: self.location(x.span) }
            }
            Origin::Using { path, ty } => {
                OriginInfo { kind: "using", via: format!("using {path}: {}", self.qualified(*ty)), location: None }
            }
        }
    }

    /// A symbol's path, with its package's name when that is not the
    /// root's.
    pub fn qualified(&self, id: SymbolId) -> String {
        let s = self.index.symbol(id);
        if s.package == PackageId(0) || self.index.package(s.package).path.starts_with("cimport:") {
            self.index.path_of(id)
        } else {
            format!("{}.{}", self.index.package(s.package).name, self.index.path_of(id))
        }
    }

    /// Field `index` of the struct `owner`.
    pub fn field_item(&self, owner: SymbolId, index: usize, origin: Option<OriginInfo>) -> Item {
        let o = self.index.symbol(owner);
        let f = &o.fields[index];
        let path = format!("{}.{}", self.index.path_of(owner), f.name);
        let in_c = o.c.is_some();
        Item {
            owner: Some(("struct", self.index.path_of(owner))),
            doc: f.doc.clone(),
            location: if in_c { None } else { self.location(f.span) },
            span: if in_c { None } else { self.extent(f.span) },
            origin,
            package: self.index.package(o.package).path.clone(),
            ..Item::blank("field", &f.name, &path, &f.declaration())
        }
    }

    /// Member `index` of the enum `owner`.
    pub fn member_item(&self, owner: SymbolId, index: usize) -> Item {
        let o = self.index.symbol(owner);
        let m = &o.enum_members[index];
        let path = format!("{}.{}", self.index.path_of(owner), m.name);
        Item {
            owner: Some(("enum", self.index.path_of(owner))),
            doc: m.doc.clone(),
            location: self.location(m.span),
            span: self.extent(m.span),
            package: self.index.package(o.package).path.clone(),
            ..Item::blank("enum_member", &m.name, &path, &m.declaration())
        }
    }
}

/// How much an item's JSON says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    /// `wid doc -json`: a location is where a name starts.
    Doc,
    /// `wid query`: a location also says where the name ends, and an item
    /// has the `span` of its whole declaration.
    Query,
}

/// A location as JSON: `file`, `line` and `column`, and with
/// [`Style::Query`] `end_line` and `end_column`; `null` when there is none.
pub fn location_json(at: &Option<Location>, style: Style) -> Value {
    match (at, style) {
        (Some(at), Style::Doc) => json!({"file": at.file, "line": at.line, "column": at.column}),
        (Some(at), Style::Query) => json!({
            "file": at.file,
            "line": at.line,
            "column": at.column,
            "end_line": at.end_line,
            "end_column": at.end_column,
        }),
        (None, _) => Value::Null,
    }
}

/// An item as JSON (SPEC "Toolchain and CLI"): `kind`, `name`, `path`,
/// `package`, `signature`, `attributes`, `doc`, `private` and `location`,
/// and where they apply `static`, `owner`, `c`, `promoted_into`, `origin`,
/// `fields`, `members`, `variants`, `targets`, `aliases` and `methods`.
/// [`Style::Query`] adds `span`.
pub fn item_json(e: &Item, style: Style) -> Value {
    let list = |items: &[Item]| json!(items.iter().map(|i| item_json(i, style)).collect::<Vec<_>>());
    let mut map = serde_json::Map::new();
    map.insert("kind".into(), json!(e.kind));
    map.insert("name".into(), json!(e.name));
    map.insert("path".into(), json!(e.path));
    map.insert("package".into(), json!(e.package));
    map.insert("signature".into(), json!(e.signature));
    map.insert("attributes".into(), json!(e.attributes));
    map.insert("doc".into(), json!(e.doc));
    map.insert("private".into(), json!(e.private));
    map.insert("location".into(), location_json(&e.location, style));
    if style == Style::Query {
        map.insert("span".into(), location_json(&e.span, style));
    }
    if e.kind == "method" || e.kind == "macro" {
        map.insert("static".into(), json!(e.is_static));
    }
    if let Some((kind, path)) = &e.owner {
        map.insert("owner".into(), json!({"kind": kind, "name": path}));
    }
    if let Some(c) = &e.c {
        map.insert("c".into(), c_json(c));
    }
    if let Some(into) = &e.promoted_into {
        map.insert("promoted_into".into(), json!(into));
    }
    if let Some(o) = &e.origin {
        map.insert("origin".into(), origin_json(o, style));
    }
    match e.kind {
        "struct" => {
            map.insert("fields".into(), list(&e.fields));
        }
        "enum" => {
            map.insert("members".into(), list(&e.members));
        }
        "union" => {
            map.insert("variants".into(), json!(e.variants));
        }
        "extension" => {
            map.insert("targets".into(), json!(e.targets));
        }
        "type_alias" => {
            map.insert("aliases".into(), json!(e.aliases));
            if !e.fields.is_empty() {
                map.insert("fields".into(), list(&e.fields));
            }
            if !e.members.is_empty() {
                map.insert("members".into(), list(&e.members));
            }
        }
        _ => {}
    }
    if matches!(e.kind, "struct" | "enum" | "union" | "module" | "extension" | "overload" | "builtin_type")
        || !e.methods.is_empty()
    {
        map.insert("methods".into(), list(&e.methods));
    }
    Value::Object(map)
}

/// A C origin as JSON: `name`, `header` and `declared_at`.
pub fn c_json(c: &CDoc) -> Value {
    json!({"name": c.name, "header": c.header, "declared_at": c.declared_at})
}

/// An origin as JSON: `kind`, `via` (`null` for a type's own) and
/// `location`.
pub fn origin_json(o: &OriginInfo, style: Style) -> Value {
    let via = if o.via.is_empty() { Value::Null } else { json!(o.via) };
    json!({"kind": o.kind, "via": via, "location": location_json(&o.location, style)})
}

/// A package as JSON: `name`, `path` and `doc`.
pub fn package_json(p: &PackageInfo) -> Value {
    json!({"name": p.name, "path": p.path, "doc": p.doc})
}

//! The names completion offers (`wid lsp`'s `textDocument/completion`),
//! read from the symbol index and what the checker recorded: a value's
//! fields and methods, a type's members, a package's public declarations,
//! the fields of `self`, and the names a package sees.
//!
//! Members are listed in lookup order: a struct's fields (its own, then
//! those `using` promotes), then its methods by origin as `methods` lists
//! them (its own, `include`d modules, `extend` blocks, `using` promotion),
//! then the extensions of builtin types and patterns, then the builtin
//! methods. A name comes once, from the first place lookup finds it.
//! Private declarations are offered only where they can be used: a type's
//! private methods inside the type, a package's private declarations in
//! the package.

use wid_sema::PackageId;
use wid_sema::index::{MemberGroup, Origin, SymbolId, SymbolKind};
use wid_sema::uses::{Members, Shape};
use wid_syntax::docs::first_paragraph;
use wid_syntax::lexer::{NameShape, name_shape};
use wid_syntax::token::Keyword;

use crate::Analysis;

/// What a completion is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    /// A field of a struct.
    Field,
    /// A method of a type, module or extension, or a builtin method.
    Method,
    /// A package-level method.
    Function,
    /// A constant.
    Constant,
    /// A struct, or a builtin type.
    Struct,
    /// An enum.
    Enum,
    /// A union.
    Union,
    /// A module.
    Module,
    /// A type alias.
    TypeAlias,
    /// A macro.
    Macro,
    /// An overload set.
    Overload,
    /// A member of an enum.
    EnumMember,
    /// A local variable or parameter.
    Variable,
    /// An import name.
    Package,
    /// A keyword.
    Keyword,
}

/// A name completion offers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The name, as it is written.
    pub label: String,
    /// What it is.
    pub kind: CandidateKind,
    /// Its declaration line: `def move(by: Int) -> Int`, `pos: Int`.
    pub detail: String,
    /// The first paragraph of its doc.
    pub doc: Option<String>,
    /// For a member of a type, where it comes from when that isn't the
    /// type itself: `include Greeter`, `extend Ball`, `using base: Entity`,
    /// `builtin`.
    pub origin: Option<String>,
}

/// Who completes: which private declarations they may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Viewer {
    /// The package whose file is being edited.
    pub package: PackageId,
    /// The type whose method is being edited (for a method of an `extend`,
    /// the type it extends), whose private methods are usable.
    pub inside: Option<SymbolId>,
}

/// The candidates for `value.` on a value whose type reaches `members`.
pub fn value_members(analysis: &Analysis, members: &Members, viewer: Viewer) -> Vec<Candidate> {
    let index = &analysis.index;
    let mut list = List::default();
    if let Some(decl) = members.decl {
        if index.symbol(decl).kind == SymbolKind::Struct {
            list.extend(fields(analysis, decl));
        }
        list.groups(analysis, &index.member_groups(decl), viewer, decl, |s| {
            matches!(s.kind, SymbolKind::Method | SymbolKind::Overload | SymbolKind::Macro) && !s.is_static
        });
    }
    let groups = index.extension_groups(&members.extensions);
    list.groups(analysis, &groups, viewer, SymbolId(u32::MAX), |s| {
        matches!(s.kind, SymbolKind::Method | SymbolKind::Overload | SymbolKind::Macro) && !s.is_static
    });
    list.extend(builtin_methods(members.shape));
    list.0
}

/// The candidates for `Type.`: `new` for a struct, an enum's members, and
/// the type-level methods (`def self.…`) and constants it answers to.
pub fn type_members(analysis: &Analysis, ty: SymbolId, viewer: Viewer) -> Vec<Candidate> {
    let index = &analysis.index;
    let ty = index.alias_target(ty);
    let s = index.symbol(ty);
    let mut list = List::default();
    if s.kind == SymbolKind::Struct {
        let fields: Vec<String> = s.fields.iter().map(|f| f.declaration()).collect();
        list.push(Candidate {
            label: "new".into(),
            kind: CandidateKind::Method,
            detail: format!("{}.new({})", s.name, fields.join(", ")),
            doc: Some(format!(
                "Builds a `{}` from its fields, by position or by name; a field left out takes its default or zero.",
                s.name
            )),
            origin: Some("builtin".into()),
        });
    }
    for m in &s.enum_members {
        list.push(Candidate {
            label: m.name.clone(),
            kind: CandidateKind::EnumMember,
            detail: m.declaration(),
            doc: m.doc.as_deref().map(first_paragraph),
            origin: None,
        });
    }
    let type_level = |s: &wid_sema::index::Symbol| match s.kind {
        SymbolKind::Method | SymbolKind::Macro => s.is_static,
        SymbolKind::Overload => {
            s.links.iter().filter_map(|l| l.symbol).next().is_some_and(|m| index.symbol(m).is_static)
        }
        SymbolKind::Constant | SymbolKind::TypeAlias => true,
        _ => false,
    };
    list.groups(analysis, &index.member_groups(ty), viewer, ty, type_level);
    list.0
}

/// The candidates for `alias.`: the public declarations of the package an
/// import name names, private ones too in the package itself.
pub fn package_members(analysis: &Analysis, pkg: PackageId, viewer: Viewer) -> Vec<Candidate> {
    let index = &analysis.index;
    let mut list = List::default();
    for &id in index.package(pkg).scope.values() {
        let s = index.symbol(id);
        if s.private && viewer.package != pkg {
            continue;
        }
        if let Some(c) = symbol(analysis, id, None) {
            list.push(c);
        }
    }
    list.0
}

/// The fields of a struct, its own and then those `using` promotes: what
/// `@` reaches in its methods, and the first candidates for `value.`.
pub fn fields(analysis: &Analysis, ty: SymbolId) -> Vec<Candidate> {
    let index = &analysis.index;
    let ty = index.alias_target(ty);
    let mut list = List::default();
    for f in index.fields_of(ty) {
        let owner = index.symbol(f.owner);
        let field = &owner.fields[f.index];
        list.push(Candidate {
            label: field.name.clone(),
            kind: CandidateKind::Field,
            detail: field.declaration(),
            doc: field.doc.as_deref().map(first_paragraph),
            origin: f.promoted_through.as_ref().map(|path| format!("using {path}: {}", owner.name)),
        });
    }
    list.0
}

/// The methods a method of `ty` can call without a receiver, on `self`
/// (type-level ones too).
pub fn self_methods(analysis: &Analysis, ty: SymbolId, viewer: Viewer) -> Vec<Candidate> {
    let index = &analysis.index;
    let ty = index.alias_target(ty);
    let mut list = List::default();
    list.groups(analysis, &index.member_groups(ty), viewer, ty, |s| {
        matches!(s.kind, SymbolKind::Method | SymbolKind::Overload | SymbolKind::Macro | SymbolKind::Constant)
    });
    list.0
}

/// The names code in a file of `pkg` sees without a receiver, besides its
/// locals: the package's declarations (private ones too), its import
/// names, the prelude's public names, the builtin types and the keywords.
pub fn scope_names(analysis: &Analysis, pkg: PackageId) -> Vec<Candidate> {
    let index = &analysis.index;
    let items = analysis.items(true);
    let mut list = List::default();
    let p = index.package(pkg);
    for &id in p.scope.values() {
        if let Some(c) = symbol(analysis, id, None) {
            list.push(c);
        }
    }
    for (alias, &imported) in &p.imports {
        let item = items.package_item(imported, alias);
        list.push(Candidate {
            label: alias.clone(),
            kind: CandidateKind::Package,
            detail: item.signature,
            doc: item.doc.as_deref().map(first_paragraph),
            origin: None,
        });
    }
    if let Some(prelude) = index.prelude.filter(|&p| p != pkg) {
        for &id in index.package(prelude).scope.values() {
            if !index.symbol(id).private
                && let Some(c) = symbol(analysis, id, None)
            {
                list.push(c);
            }
        }
    }
    for name in &index.builtin_types {
        let mut doc = None;
        let extensions = index.builtin_groups(name);
        if !extensions.is_empty() {
            let count: usize = extensions.iter().map(|g| g.members.len()).sum();
            doc = Some(format!("A builtin type, with {count} methods from extensions."));
        }
        list.push(Candidate {
            label: name.to_string(),
            kind: CandidateKind::Struct,
            detail: name.to_string(),
            doc: doc.or_else(|| Some("A builtin type.".into())),
            origin: Some("builtin".into()),
        });
    }
    for keyword in Keyword::ALL {
        list.push(Candidate {
            label: keyword.as_str().into(),
            kind: CandidateKind::Keyword,
            detail: format!("keyword {}", keyword.as_str()),
            doc: None,
            origin: None,
        });
    }
    list.0
}

/// A declaration as a candidate; `None` for one completion never offers:
/// an extension, an operator method, a name no code can write.
pub fn symbol(analysis: &Analysis, id: SymbolId, origin: Option<String>) -> Option<Candidate> {
    let s = analysis.index.symbol(id);
    let shape = name_shape(&s.name);
    if !matches!(shape, NameShape::Ident { .. } | NameShape::Const) {
        return None;
    }
    let kind = match s.kind {
        SymbolKind::Constant => CandidateKind::Constant,
        SymbolKind::TypeAlias => CandidateKind::TypeAlias,
        SymbolKind::Struct => CandidateKind::Struct,
        SymbolKind::Enum => CandidateKind::Enum,
        SymbolKind::Union => CandidateKind::Union,
        SymbolKind::Module => CandidateKind::Module,
        SymbolKind::Method if s.owner.is_some() => CandidateKind::Method,
        SymbolKind::Method => CandidateKind::Function,
        SymbolKind::Macro => CandidateKind::Macro,
        SymbolKind::Overload => CandidateKind::Overload,
        SymbolKind::Extension => return None,
    };
    let mut doc = s.doc.as_deref().map(first_paragraph);
    if let Some(c) = &s.c {
        let from = format!("C `{}` from `{}`.", c.name, c.header);
        doc = Some(match doc {
            Some(d) => format!("{d}\n\n{from}"),
            None => from,
        });
    }
    Some(Candidate { label: s.name.clone(), kind, detail: s.signature.clone(), doc, origin })
}

/// The builtin methods of a kind of type (SPEC "Data and behavior"), with
/// the ones every value has (`to_s`, `inspect`) last.
pub fn builtin_methods(shape: Shape) -> Vec<Candidate> {
    const CONVERT: &[(&str, &str, &str)] = &[
        ("to_i", "def to_i -> Int", "The value as an `Int`."),
        ("to_f", "def to_f -> F64", "The value as an `F64`."),
        ("to", "def to(T) -> T", "Converts the value to the number type `T`: `x.to(F32)`."),
    ];
    const SIZED: &[(&str, &str, &str)] = &[
        ("size", "def size -> Int", "The number of elements."),
        ("empty?", "def empty? -> Bool", "Whether there are no elements."),
    ];
    const ENDS: &[(&str, &str, &str)] = &[
        ("first", "def first -> T?", "The first element, or `nil` when there is none."),
        ("last", "def last -> T?", "The last element, or `nil` when there is none."),
    ];
    const TO_SLICE: (&str, &str, &str) = ("to_slice", "def to_slice -> []T", "A slice of every element.");
    const STRING: &[(&str, &str, &str)] = &[
        ("size", "def size -> Int", "The length in bytes."),
        ("empty?", "def empty? -> Bool", "Whether the string has no bytes."),
        ("include?", "def include?(s: String) -> Bool", "Whether `s` occurs in the string."),
        ("index", "def index(s: String) -> Int?", "Where `s` first occurs, in bytes, or `nil`."),
        ("starts_with?", "def starts_with?(s: String) -> Bool", "Whether the string starts with `s`."),
        ("ends_with?", "def ends_with?(s: String) -> Bool", "Whether the string ends with `s`."),
        ("to_sym", "def to_sym -> Symbol", "The string as a `Symbol`, at compile time."),
        ("to_cstr", "def to_cstr -> CString", "A NUL-terminated copy in `context.temp_allocator`."),
    ];
    const DYNAMIC: &[(&str, &str, &str)] = &[
        ("capacity", "def capacity -> Int", "How many elements fit before the array grows."),
        ("push", "def push(x: T)", "Appends `x`, growing the array with its allocator; also `<<`."),
        ("insert", "def insert(i: Int, x: T)", "Inserts `x` at index `i`."),
        ("delete_at", "def delete_at(i: Int)", "Removes the element at index `i`."),
        ("concat", "def concat(xs: []T)", "Appends every element of `xs`."),
        ("resize", "def resize(n: Int)", "Sets the length to `n`; new elements are zero."),
        ("reserve", "def reserve(n: Int)", "Makes room for `n` elements."),
        ("clear", "def clear", "Removes every element, keeping the memory."),
        ("pop", "def pop -> T?", "Removes and returns the last element, or `nil`."),
    ];
    const MAP: &[(&str, &str, &str)] = &[
        ("has_key?", "def has_key?(k: K) -> Bool", "Whether the map holds the key `k`."),
        ("delete", "def delete(k: K) -> Bool", "Removes the key `k`; whether it was there."),
    ];
    const MATRIX: &[(&str, &str, &str)] = &[
        ("transpose", "def transpose -> matrix[C, R]T", "The matrix with rows and columns swapped."),
        ("row", "def row(i: Int) -> [C]T", "Row `i`, as an array."),
        ("column", "def column(i: Int) -> [R]T", "Column `i`, as an array."),
    ];
    const PROC: &[(&str, &str, &str)] = &[("call", "def call(…)", "Calls the proc, like `f(…)`.")];
    const EVERY: &[(&str, &str, &str)] = &[
        ("to_s", "def to_s -> String", "The value as text, as `puts` prints it."),
        ("inspect", "def inspect -> String", "The value as text for debugging."),
    ];
    let mut out: Vec<(&str, String, &str)> = Vec::new();
    let mut add = |list: &[(&'static str, &'static str, &'static str)]| {
        out.extend(list.iter().map(|(name, detail, doc)| (*name, detail.to_string(), *doc)));
    };
    match shape {
        Shape::Number | Shape::Enum | Shape::Rune => add(CONVERT),
        Shape::String => add(STRING),
        Shape::Array { .. } => {
            add(SIZED);
            add(ENDS);
            add(&[TO_SLICE]);
        }
        Shape::Slice => {
            add(SIZED);
            add(ENDS);
        }
        Shape::Dynamic => {
            add(SIZED);
            add(DYNAMIC);
            add(ENDS);
            add(&[TO_SLICE]);
        }
        Shape::Map => add(SIZED),
        Shape::Matrix => add(MATRIX),
        Shape::Proc => add(PROC),
        Shape::Bool | Shape::Struct | Shape::Other => {}
    }
    if shape == Shape::Map {
        add(MAP);
    }
    let mut list: Vec<Candidate> = out
        .into_iter()
        .map(|(name, detail, doc)| Candidate {
            label: name.into(),
            kind: CandidateKind::Method,
            detail,
            doc: Some(doc.into()),
            origin: Some("builtin".into()),
        })
        .collect();
    if let Shape::Array { len, numeric: true } = shape {
        for (i, (xyzw, rgba)) in ["x", "y", "z", "w"].iter().zip(["r", "g", "b", "a"]).enumerate().take(len.min(4) as usize) {
            for name in [*xyzw, rgba] {
                list.push(Candidate {
                    label: name.into(),
                    kind: CandidateKind::Field,
                    detail: format!("{name}: T"),
                    doc: Some(format!("Element {i}; swizzles like `xy` and `rgb` build arrays.")),
                    origin: Some("builtin".into()),
                });
            }
        }
    }
    for (name, detail, doc) in EVERY {
        list.push(Candidate {
            label: (*name).into(),
            kind: CandidateKind::Method,
            detail: (*detail).into(),
            doc: Some((*doc).into()),
            origin: Some("builtin".into()),
        });
    }
    list
}

/// Candidates without repeated names: the first one wins, as lookup finds
/// it.
#[derive(Default)]
struct List(Vec<Candidate>);

impl List {
    fn push(&mut self, c: Candidate) {
        if !self.0.iter().any(|other| other.label == c.label) {
            self.0.push(c);
        }
    }

    fn extend(&mut self, list: Vec<Candidate>) {
        for c in list {
            self.push(c);
        }
    }

    /// The members of groups that `keep` accepts and the viewer may use,
    /// with their origins. `ty` is the type the groups belong to, whose
    /// private methods are usable inside it.
    fn groups(
        &mut self,
        analysis: &Analysis,
        groups: &[MemberGroup],
        viewer: Viewer,
        ty: SymbolId,
        keep: impl Fn(&wid_sema::index::Symbol) -> bool,
    ) {
        let index = &analysis.index;
        let items = analysis.items(true);
        for group in groups {
            let origin = match group.origin {
                Origin::Own => None,
                ref other => Some(items.origin(other).via),
            };
            for &m in &group.members {
                let s = index.symbol(m);
                if !keep(s) {
                    continue;
                }
                // A type's private method is usable in the type's methods,
                // a package's private declaration in the package.
                let usable = !s.private
                    || match s.owner.map(|o| index.symbol(o).kind) {
                        Some(SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Union | SymbolKind::Module)
                        | Some(SymbolKind::Extension) => viewer.inside == Some(ty),
                        _ => s.package == viewer.package,
                    };
                if !usable {
                    continue;
                }
                if let Some(c) = symbol(analysis, m, origin.clone()) {
                    self.push(c);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use wid_sema::uses::Shape;

    use super::builtin_methods;

    #[test]
    fn builtin_methods_fit_the_kind() {
        let names = |shape| builtin_methods(shape).into_iter().map(|c| c.label).collect::<Vec<_>>();
        assert_eq!(names(Shape::Bool), ["to_s", "inspect"]);
        assert!(names(Shape::String).starts_with(&["size".to_string(), "empty?".to_string()]));
        let dynamic = names(Shape::Dynamic);
        for name in ["push", "pop", "first", "to_slice", "to_s"] {
            assert!(dynamic.iter().any(|n| n == name), "{name} in {dynamic:?}");
        }
        let vec2 = names(Shape::Array { len: 2, numeric: true });
        assert!(vec2.iter().any(|n| n == "y") && !vec2.iter().any(|n| n == "z"), "{vec2:?}");
        assert!(names(Shape::Map).iter().any(|n| n == "has_key?"));
    }
}

//! The symbol index: every declaration of a checked program as plain data
//! (its kind, name, place, doc comment and declaration line), and how a
//! symbol path such as `Ball.update` or `rl.draw_circle_v` resolves in a
//! package.
//!
//! [`check_program_indexed`](crate::check_program_indexed) builds it after
//! checking, from the declarations the checker collected: so it holds the
//! chosen `comptime if` branches, what macros generated and the names a
//! `cimport` without `as:` merged into a package, even when the program has
//! errors elsewhere. `wid doc` builds its pages from it; `wid query` and the
//! LSP are meant to share it.

use std::collections::BTreeMap;
use std::path::PathBuf;

use wid_diagnostics::Span;

use crate::input::PackageId;

/// Identifies a symbol in [`Index::symbols`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, PartialOrd, Ord)]
pub struct SymbolId(pub u32);

/// What a symbol declares.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum SymbolKind {
    /// `NAME = value`, `NAME: T = value`.
    Constant,
    /// A constant whose value is a type: `Vec2 = [2]F32`, `Key = C.int`.
    TypeAlias,
    /// `struct Name … end`.
    Struct,
    /// `enum Name … end`.
    Enum,
    /// `union Name = A | B`.
    Union,
    /// `module Name … end`.
    Module,
    /// `def name …`, at package level or in a type, module or `extend`.
    Method,
    /// `macro def name …`.
    Macro,
    /// `overload :name, :a, :b`.
    Overload,
    /// `extend T1, T2 … end`.
    Extension,
}

impl SymbolKind {
    /// The kind as JSON names it: `type_alias`, `struct`, `method`, ….
    pub fn as_str(self) -> &'static str {
        match self {
            SymbolKind::Constant => "constant",
            SymbolKind::TypeAlias => "type_alias",
            SymbolKind::Struct => "struct",
            SymbolKind::Enum => "enum",
            SymbolKind::Union => "union",
            SymbolKind::Module => "module",
            SymbolKind::Method => "method",
            SymbolKind::Macro => "macro",
            SymbolKind::Overload => "overload",
            SymbolKind::Extension => "extension",
        }
    }

    /// The kind in prose, with its article: "a struct", "an overload set".
    pub fn a_describe(self) -> &'static str {
        match self {
            SymbolKind::Constant => "a constant",
            SymbolKind::TypeAlias => "a type alias",
            SymbolKind::Struct => "a struct",
            SymbolKind::Enum => "an enum",
            SymbolKind::Union => "a union",
            SymbolKind::Module => "a module",
            SymbolKind::Method => "a method",
            SymbolKind::Macro => "a macro",
            SymbolKind::Overload => "an overload set",
            SymbolKind::Extension => "an extension",
        }
    }

    /// Whether the symbol is a type that has members (fields, enum members
    /// or methods).
    pub fn has_members(self) -> bool {
        matches!(
            self,
            SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Union | SymbolKind::Module | SymbolKind::TypeAlias
        )
    }
}

/// A written type or name, with the symbol it resolves to where it is
/// written, if it names one.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Link {
    /// The text, as the printer renders it: `Greeter`, `geo.Ball`,
    /// `Pool($T, $N)`, `[]$T`.
    pub text: String,
    /// The declaration it names.
    pub symbol: Option<SymbolId>,
}

/// One field of a struct.
#[derive(Clone, Debug)]
pub struct FieldDoc {
    /// The field name.
    pub name: String,
    /// The type, as written.
    pub ty: String,
    /// The default `T.new` uses, as written.
    pub default: Option<String>,
    /// `using name: T`: its members are promoted.
    pub using: bool,
    /// For a `using` field, the struct whose members it promotes.
    pub promotes: Option<SymbolId>,
    /// The doc comment.
    pub doc: Option<String>,
    /// The field's name.
    pub span: Span,
}

impl FieldDoc {
    /// The field as declared: `hp: Int = 100`, `using base: Entity`.
    pub fn declaration(&self) -> String {
        let using = if self.using { "using " } else { "" };
        match &self.default {
            Some(d) => format!("{using}{}: {} = {d}", self.name, self.ty),
            None => format!("{using}{}: {}", self.name, self.ty),
        }
    }
}

/// One member of an enum.
#[derive(Clone, Debug)]
pub struct MemberDoc {
    /// The member name.
    pub name: String,
    /// Its value, when written.
    pub value: Option<String>,
    /// The doc comment.
    pub doc: Option<String>,
    /// The member's name.
    pub span: Span,
}

impl MemberDoc {
    /// The member as declared: `north`, `east = 4`.
    pub fn declaration(&self) -> String {
        match &self.value {
            Some(v) => format!("{} = {v}", self.name),
            None => self.name.clone(),
        }
    }
}

/// Where a declaration of a `cimport` package comes from in C.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CDoc {
    /// The C name: `DrawCircleV`, `struct rAudioBuffer`.
    pub name: String,
    /// The header the `cimport` names, as written: `raylib.h`.
    pub header: String,
    /// Where C declares it, as `file:line`, when known.
    pub declared_at: Option<String>,
}

/// One declaration.
#[derive(Clone, Debug)]
pub struct Symbol {
    /// The name; operators are spelled as written (`+`, `[]=`), unary minus
    /// as `-@`. An extension has none of its own: its name is its
    /// declaration line, `extend String`.
    pub name: String,
    /// What it declares.
    pub kind: SymbolKind,
    /// The package whose file declares it: for a C declaration, the
    /// `cimport` package, even when a `cimport` without `as:` merged it into
    /// another one.
    pub package: PackageId,
    /// The name as written (for an extension, its first target).
    pub span: Span,
    /// Whether it was declared `private`.
    pub private: bool,
    /// The struct, enum, union, module or `extend` that declares it.
    pub owner: Option<SymbolId>,
    /// `def self.name`: a type-level method.
    pub is_static: bool,
    /// The doc comment directly above it.
    pub doc: Option<String>,
    /// The declaration line, without attributes, `private` or body (see
    /// [`wid_syntax::print::Printer::item_header`]).
    pub signature: String,
    /// The attributes written before it, each as `name` or `name(args)`.
    pub attributes: Vec<String>,
    /// A struct's fields, in declaration order.
    pub fields: Vec<FieldDoc>,
    /// An enum's members, in declaration order.
    pub enum_members: Vec<MemberDoc>,
    /// The methods, constants and overload sets declared inside it, in the
    /// order they were collected: written ones, then those `comptime if`
    /// branches and macros added.
    pub members: Vec<SymbolId>,
    /// The modules its `include`s name.
    pub includes: Vec<Link>,
    /// An extension's targets; a union's variants; a type alias's type; an
    /// overload set's members.
    pub links: Vec<Link>,
    /// For a declaration of a `cimport` package, its C origin.
    pub c: Option<CDoc>,
}

/// One package.
#[derive(Clone, Debug)]
pub struct PackageDoc {
    /// The name used for qualification: `fmt`.
    pub name: String,
    /// The import path: `core:fmt`, `./physics`, `.` for the root, or
    /// `cimport:raylib.h`.
    pub path: String,
    /// The package directory.
    pub dir: PathBuf,
    /// The package doc: the comment block that opens one of its files (see
    /// `SPEC.md`, "Toolchain and CLI").
    pub doc: Option<String>,
    /// The names visible at package level: its own declarations and those a
    /// `cimport` without `as:` merged in.
    pub scope: BTreeMap<String, SymbolId>,
    /// The package-level declarations in source order (files in name order),
    /// with merged `cimport` declarations where the `cimport` is written and
    /// generated ones where the macro call is.
    pub items: Vec<SymbolId>,
    /// The `import` and `cimport … as:` names its files bind, with the
    /// package each names (the first file's, when files disagree).
    pub imports: BTreeMap<String, PackageId>,
    /// For a `cimport` package, the header as written.
    pub header: Option<String>,
}

/// Every declaration of a program. See the module docs.
#[derive(Clone, Debug, Default)]
pub struct Index {
    /// The packages, by [`PackageId`].
    pub packages: Vec<PackageDoc>,
    /// The symbols, by [`SymbolId`].
    pub symbols: Vec<Symbol>,
    /// The prelude, whose public names every file sees.
    pub prelude: Option<PackageId>,
    /// The builtin type names (`Int`, `String`, …), which extensions can
    /// add methods to.
    pub builtin_types: Vec<&'static str>,
}

/// What a symbol path names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// An imported package, named by its alias: `rl`.
    Package(PackageId),
    /// A declaration.
    Symbol(SymbolId),
    /// A field of a struct (`owner`), maybe promoted from a `using` field.
    Field {
        /// The struct that declares the field.
        owner: SymbolId,
        /// The field, in the owner's [`Symbol::fields`].
        index: usize,
        /// For a field promoted into another struct, that struct and the
        /// `using` fields it goes through, joined with dots (`base`).
        promoted: Option<(SymbolId, String)>,
    },
    /// A member of an enum.
    EnumMember {
        /// The enum.
        owner: SymbolId,
        /// The member, in the owner's [`Symbol::enum_members`].
        index: usize,
    },
    /// A builtin type such as `String`, documented by the extensions that
    /// add methods to it.
    Builtin(String),
}

/// Why a symbol path doesn't resolve.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PathError {
    /// The segment that failed, counted from 0.
    pub segment: usize,
    /// What went wrong.
    pub kind: PathErrorKind,
}

/// The ways a symbol path fails.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PathErrorKind {
    /// No declaration, import or builtin type has the name in the package.
    UnknownName {
        /// The package searched.
        package: PackageId,
        /// The import name the path reached it through (`rl`), if any.
        via: Option<String>,
        /// The public names there, sorted.
        candidates: Vec<String>,
    },
    /// The type (or module) has no member with the name.
    NoMember {
        /// What was searched.
        owner: Target,
        /// Its members' names, sorted.
        candidates: Vec<String>,
    },
    /// The path continues after something that has no members.
    NoMembers {
        /// What the path named so far.
        target: Target,
    },
    /// The name is private, and private symbols weren't asked for.
    Private {
        /// The private declaration.
        symbol: SymbolId,
    },
}

/// Where methods a type answers to come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    /// Declared in the type's own body.
    Own,
    /// Mixed in by `include Module`, written in `by` (the type, a module it
    /// includes, or an extension of it).
    Include {
        /// The module.
        module: SymbolId,
        /// The declaration whose `include` names the module.
        by: SymbolId,
    },
    /// Added by an extension.
    Extend {
        /// The `extend` declaration.
        extension: SymbolId,
    },
    /// Promoted from a `using` field: `path` is the field (`base`, or
    /// `base.pos` when promoted twice) and `ty` its struct.
    Using {
        /// The fields to go through, joined with dots.
        path: String,
        /// The struct that declares the members.
        ty: SymbolId,
    },
}

/// Methods (and constants) a type answers to that share an origin.
#[derive(Clone, Debug)]
pub struct MemberGroup {
    /// Where they come from.
    pub origin: Origin,
    /// The member symbols, in declaration order.
    pub members: Vec<SymbolId>,
}

/// A field a struct has, its own or promoted.
#[derive(Clone, Debug)]
pub struct FieldRef {
    /// The struct that declares it.
    pub owner: SymbolId,
    /// The field in the owner's [`Symbol::fields`].
    pub index: usize,
    /// `None` for the struct's own field; else the `using` fields it is
    /// promoted through, joined with dots.
    pub promoted_through: Option<String>,
}

impl Index {
    /// The symbol `id`.
    pub fn symbol(&self, id: SymbolId) -> &Symbol {
        &self.symbols[id.0 as usize]
    }

    /// The package `id`.
    pub fn package(&self, id: PackageId) -> &PackageDoc {
        &self.packages[id.0 as usize]
    }

    /// Whether a symbol is shown when private ones aren't: it isn't
    /// private, and neither is the type that declares it.
    pub fn is_public(&self, id: SymbolId) -> bool {
        let s = self.symbol(id);
        !s.private && s.owner.is_none_or(|o| self.is_public(o))
    }

    /// The symbol path of a declaration in its package: `Ball.update`.
    pub fn path_of(&self, id: SymbolId) -> String {
        let s = self.symbol(id);
        match s.owner {
            Some(o) if self.symbol(o).kind != SymbolKind::Extension => format!("{}.{}", self.path_of(o), s.name),
            _ => s.name.clone(),
        }
    }

    /// Resolves a symbol path in `pkg`: a package-level name, an import
    /// name, a builtin type or a prelude name, then members of types. With
    /// `private`, private declarations resolve too; otherwise naming one is
    /// [`PathErrorKind::Private`].
    pub fn resolve(&self, pkg: PackageId, path: &[&str], private: bool) -> Result<Target, PathError> {
        let Some((first, rest)) = path.split_first() else {
            return Err(PathError {
                segment: 0,
                kind: PathErrorKind::UnknownName { package: pkg, via: None, candidates: self.names_in(pkg, true) },
            });
        };
        let mut target = self.resolve_first(pkg, first, private)?;
        for (i, segment) in rest.iter().enumerate() {
            let at = i + 1;
            target = match target {
                Target::Package(p) => {
                    let via = Some(path[..at].join("."));
                    match self.package(p).scope.get(*segment) {
                        Some(&id) if private || !self.symbol(id).private => Target::Symbol(id),
                        Some(&id) => {
                            return Err(PathError { segment: at, kind: PathErrorKind::Private { symbol: id } });
                        }
                        None => {
                            return Err(PathError {
                                segment: at,
                                kind: PathErrorKind::UnknownName {
                                    package: p,
                                    via,
                                    candidates: self.names_in(p, false),
                                },
                            });
                        }
                    }
                }
                other => self.member(&other, segment, private).map_err(|kind| PathError { segment: at, kind })?,
            };
        }
        Ok(target)
    }

    /// The first segment of a path.
    fn resolve_first(&self, pkg: PackageId, name: &str, private: bool) -> Result<Target, PathError> {
        let p = self.package(pkg);
        let found = p
            .scope
            .get(name)
            .copied()
            .map(Target::Symbol)
            .or_else(|| p.imports.get(name).map(|&id| Target::Package(id)))
            .or_else(|| {
                let prelude = self.package(self.prelude?);
                prelude.scope.get(name).filter(|&&id| !self.symbol(id).private).map(|&id| Target::Symbol(id))
            })
            .or_else(|| self.builtin_types.contains(&name).then(|| Target::Builtin(name.to_string())));
        match found {
            Some(Target::Symbol(id)) if !private && self.symbol(id).private => {
                Err(PathError { segment: 0, kind: PathErrorKind::Private { symbol: id } })
            }
            Some(target) => Ok(target),
            None => Err(PathError {
                segment: 0,
                kind: PathErrorKind::UnknownName { package: pkg, via: None, candidates: self.names_in(pkg, true) },
            }),
        }
    }

    /// The public names of a package, sorted: with `local`, also its
    /// import names, the prelude's names and the builtin types.
    pub fn names_in(&self, pkg: PackageId, local: bool) -> Vec<String> {
        let p = self.package(pkg);
        let mut names: Vec<String> =
            p.scope.iter().filter(|(_, id)| !self.symbol(**id).private).map(|(n, _)| n.clone()).collect();
        if local {
            names.extend(p.imports.keys().cloned());
            if let Some(prelude) = self.prelude {
                names.extend(
                    self.package(prelude)
                        .scope
                        .iter()
                        .filter(|(_, id)| !self.symbol(**id).private)
                        .map(|(n, _)| n.clone()),
                );
            }
            names.extend(self.builtin_types.iter().map(|s| s.to_string()));
        }
        names.sort();
        names.dedup();
        names
    }

    /// The member `name` of what a path named so far.
    fn member(&self, target: &Target, name: &str, private: bool) -> Result<Target, PathErrorKind> {
        let owner = match target {
            Target::Symbol(id) => {
                let s = self.symbol(*id);
                if !s.kind.has_members() {
                    return Err(PathErrorKind::NoMembers { target: target.clone() });
                }
                Some(self.alias_target(*id))
            }
            Target::Builtin(_) => None,
            Target::Field { .. } | Target::EnumMember { .. } | Target::Package(_) => {
                return Err(PathErrorKind::NoMembers { target: target.clone() });
            }
        };
        if let Some(owner) = owner {
            let s = self.symbol(owner);
            if let Some(index) = s.enum_members.iter().position(|m| m.name == name) {
                return Ok(Target::EnumMember { owner, index });
            }
            if let Some(f) =
                self.fields_of(owner).into_iter().find(|f| self.symbol(f.owner).fields[f.index].name == name)
            {
                let promoted = f.promoted_through.map(|path| (owner, path));
                return Ok(Target::Field { owner: f.owner, index: f.index, promoted });
            }
        }
        let groups = match (owner, target) {
            (Some(owner), _) => self.member_groups(owner),
            (None, Target::Builtin(ty)) => self.builtin_groups(ty),
            _ => Vec::new(),
        };
        for group in &groups {
            for &m in &group.members {
                if self.symbol(m).name == name || (self.symbol(m).name == "-@" && name == "-") {
                    if !private && self.symbol(m).private {
                        return Err(PathErrorKind::Private { symbol: m });
                    }
                    return Ok(Target::Symbol(m));
                }
            }
        }
        let mut candidates: Vec<String> = Vec::new();
        if let Some(owner) = owner {
            candidates.extend(self.symbol(owner).enum_members.iter().map(|m| m.name.clone()));
            candidates.extend(self.fields_of(owner).iter().map(|f| self.symbol(f.owner).fields[f.index].name.clone()));
        }
        for group in &groups {
            candidates.extend(
                group
                    .members
                    .iter()
                    .filter(|&&m| private || !self.symbol(m).private)
                    .map(|&m| self.symbol(m).name.clone()),
            );
        }
        candidates.sort();
        candidates.dedup();
        let owner = owner.map_or_else(|| target.clone(), Target::Symbol);
        Err(PathErrorKind::NoMember { owner, candidates })
    }

    /// The type a type alias stands for, when it names a declared type
    /// (through further aliases); any other symbol is itself.
    pub fn alias_target(&self, id: SymbolId) -> SymbolId {
        let mut id = id;
        for _ in 0..16 {
            let s = self.symbol(id);
            match (s.kind, s.links.first().and_then(|l| l.symbol)) {
                (SymbolKind::TypeAlias, Some(next)) => id = next,
                _ => break,
            }
        }
        id
    }

    /// Every field of a struct: its own, then those `using` fields promote,
    /// depth first.
    pub fn fields_of(&self, ty: SymbolId) -> Vec<FieldRef> {
        let mut out = Vec::new();
        let mut seen = vec![ty];
        self.collect_fields(ty, None, &mut out, &mut seen);
        out
    }

    fn collect_fields(&self, ty: SymbolId, through: Option<&str>, out: &mut Vec<FieldRef>, seen: &mut Vec<SymbolId>) {
        let s = self.symbol(ty);
        for index in 0..s.fields.len() {
            out.push(FieldRef { owner: ty, index, promoted_through: through.map(str::to_string) });
        }
        for f in &s.fields {
            let Some(inner) = f.promotes else { continue };
            if seen.contains(&inner) {
                continue;
            }
            seen.push(inner);
            let path = match through {
                Some(t) => format!("{t}.{}", f.name),
                None => f.name.clone(),
            };
            self.collect_fields(inner, Some(&path), out, seen);
        }
    }

    /// The methods, constants and overload sets a type or module answers
    /// to, grouped by where they come from: its own, the modules it (or an
    /// extension of it) includes, extensions, then `using` promotion. This
    /// follows the order method lookup uses (SPEC "Data and behavior").
    pub fn member_groups(&self, ty: SymbolId) -> Vec<MemberGroup> {
        let mut groups = Vec::new();
        let s = self.symbol(ty);
        groups.push(MemberGroup { origin: Origin::Own, members: s.members.clone() });
        let mut seen = vec![ty];
        self.include_groups(ty, &mut groups, &mut seen);
        for ext in self.extensions_of(|link| link.symbol == Some(ty)) {
            groups.push(MemberGroup {
                origin: Origin::Extend { extension: ext },
                members: self.symbol(ext).members.clone(),
            });
            self.include_groups(ext, &mut groups, &mut seen);
        }
        if matches!(s.kind, SymbolKind::Struct) {
            let mut visited = vec![ty];
            self.using_groups(ty, None, &mut groups, &mut visited);
        }
        groups.retain(|g| !g.members.is_empty() || g.origin == Origin::Own);
        groups
    }

    /// The methods extensions add to a builtin type such as `String`.
    pub fn builtin_groups(&self, name: &str) -> Vec<MemberGroup> {
        let mut groups = Vec::new();
        let mut seen = Vec::new();
        for ext in self.extensions_of(|link| link.symbol.is_none() && link.text == name) {
            groups.push(MemberGroup {
                origin: Origin::Extend { extension: ext },
                members: self.symbol(ext).members.clone(),
            });
            self.include_groups(ext, &mut groups, &mut seen);
        }
        groups
    }

    /// Every extension with a target that `pred` accepts, in package and
    /// source order.
    fn extensions_of(&self, pred: impl Fn(&Link) -> bool) -> Vec<SymbolId> {
        let mut out = Vec::new();
        for p in &self.packages {
            for &id in &p.items {
                let s = self.symbol(id);
                if s.kind == SymbolKind::Extension && s.links.iter().any(&pred) && !out.contains(&id) {
                    out.push(id);
                }
            }
        }
        out
    }

    /// The groups of the modules `by` includes, and the modules those
    /// include, each once.
    fn include_groups(&self, by: SymbolId, groups: &mut Vec<MemberGroup>, seen: &mut Vec<SymbolId>) {
        for link in &self.symbol(by).includes {
            let Some(module) = link.symbol else { continue };
            if seen.contains(&module) {
                continue;
            }
            seen.push(module);
            groups.push(MemberGroup {
                origin: Origin::Include { module, by },
                members: self.symbol(module).members.clone(),
            });
            self.include_groups(module, groups, seen);
        }
    }

    /// The groups of the structs a struct's `using` fields promote, depth
    /// first, with their own included and extension methods.
    fn using_groups(
        &self,
        ty: SymbolId,
        through: Option<&str>,
        groups: &mut Vec<MemberGroup>,
        seen: &mut Vec<SymbolId>,
    ) {
        for f in &self.symbol(ty).fields {
            let Some(inner) = f.promotes else { continue };
            if seen.contains(&inner) {
                continue;
            }
            seen.push(inner);
            let path = match through {
                Some(t) => format!("{t}.{}", f.name),
                None => f.name.clone(),
            };
            let mut inner_groups = Vec::new();
            inner_groups.push(MemberGroup { origin: Origin::Own, members: self.symbol(inner).members.clone() });
            let mut modules = Vec::new();
            self.include_groups(inner, &mut inner_groups, &mut modules);
            for ext in self.extensions_of(|link| link.symbol == Some(inner)) {
                inner_groups.push(MemberGroup {
                    origin: Origin::Extend { extension: ext },
                    members: self.symbol(ext).members.clone(),
                });
            }
            let members: Vec<SymbolId> = inner_groups.into_iter().flat_map(|g| g.members).collect();
            groups.push(MemberGroup { origin: Origin::Using { path: path.clone(), ty: inner }, members });
            self.using_groups(inner, Some(&path), groups, seen);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::sync::Arc;

    use wid_diagnostics::FileId;

    use super::{Index, Origin, PathErrorKind, SymbolKind, Target};
    use crate::input::{CheckOptions, FileInput, PackageId, PackageInput, ProgramInput};

    const SRC: &str = "\
# Says things.
module Greeter
  def greet -> Int = 1
end

struct Entity
  hp: Int
  def move
  end
end

# The player.
struct Player
  using base: Entity
  include Greeter
  def heal
  end
  private def secret
  end
end

extend Player
  def boost
  end
end

Hero = Player
";

    /// The index of a one-file library package.
    fn index(src: &str) -> Index {
        let (ast, diags) = wid_syntax::parse_file(FileId(0), src);
        assert!(diags.is_empty(), "{diags:?}");
        let file = FileInput {
            ast,
            imports: HashMap::new(),
            text: Arc::from(src),
            display: "main.wid".into(),
            deferred: HashMap::new(),
            struct_literals: Vec::new(),
        };
        let package = PackageInput {
            name: "main".into(),
            path: ".".into(),
            dir: ".".into(),
            files: vec![file],
            c_sources: Vec::new(),
            cimport: None,
        };
        let options = CheckOptions { library: true, ..CheckOptions::default() };
        let input = ProgramInput { packages: vec![package], options, prelude: None };
        let (_, diags, index, _) = crate::check_program_indexed(&input);
        assert!(!diags.has_errors(), "{diags:?}");
        index
    }

    #[test]
    fn paths_reach_members_from_every_origin() {
        let index = index(SRC);
        let root = PackageId(0);
        let resolve = |path: &[&str]| index.resolve(root, path, false);
        for (path, want) in [
            (&["Player", "heal"][..], "Player.heal"),
            (&["Player", "greet"], "Greeter.greet"),
            (&["Player", "boost"], "boost"),
            (&["Player", "move"], "Entity.move"),
            (&["Hero", "heal"], "Player.heal"),
        ] {
            match resolve(path) {
                Ok(Target::Symbol(id)) => assert_eq!(index.path_of(id), want),
                other => panic!("{path:?}: {other:?}"),
            }
        }
        let Ok(Target::Field { owner, promoted: Some((into, through)), .. }) = resolve(&["Player", "hp"]) else {
            panic!("`Player.hp` is a promoted field");
        };
        assert_eq!((index.symbol(owner).name.as_str(), index.symbol(into).name.as_str()), ("Entity", "Player"));
        assert_eq!(through, "base");
        let player = index.package(root).scope["Player"];
        assert_eq!(index.symbol(player).doc.as_deref(), Some("The player."));
        assert_eq!(index.symbol(player).kind, SymbolKind::Struct);
        let origins: Vec<&str> = index
            .member_groups(player)
            .iter()
            .map(|g| match g.origin {
                Origin::Own => "own",
                Origin::Include { .. } => "include",
                Origin::Extend { .. } => "extend",
                Origin::Using { .. } => "using",
            })
            .collect();
        assert_eq!(origins, ["own", "include", "extend", "using"]);
    }

    #[test]
    fn failures_say_what_went_wrong() {
        let index = index(SRC);
        let root = PackageId(0);
        let err = index.resolve(root, &["Player", "secret"], false).expect_err("private");
        assert!(matches!(err.kind, PathErrorKind::Private { .. }), "{err:?}");
        assert!(index.resolve(root, &["Player", "secret"], true).is_ok());
        let err = index.resolve(root, &["Plyer"], false).expect_err("unknown");
        assert_eq!(err.segment, 0);
        let PathErrorKind::UnknownName { candidates, .. } = err.kind else { panic!("{err:?}") };
        assert!(candidates.iter().any(|c| c == "Player"));
        let err = index.resolve(root, &["Player", "heal", "x"], false).expect_err("no members");
        assert_eq!(err.segment, 2);
        assert!(matches!(err.kind, PathErrorKind::NoMembers { .. }));
        let err = index.resolve(root, &["Player", "hepl"], false).expect_err("no member");
        let PathErrorKind::NoMember { candidates, .. } = err.kind else { panic!("{err:?}") };
        assert!(candidates.iter().any(|c| c == "heal") && !candidates.iter().any(|c| c == "secret"));
    }
}

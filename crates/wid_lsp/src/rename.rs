//! References and rename, from the uses the checker records (`wid query
//! refs`).
//!
//! What a position names is found in the check of its package; its uses
//! are gathered from every check that read the file declaring it, so a
//! package open in the editor that imports it is covered too. A rename
//! edits the declaration and every use, and is refused, with the reason,
//! when its result wouldn't be valid or can't be made safely: the new name
//! breaks the rules for what is renamed, it is already taken where the old
//! one is visible, the declaration isn't the user's to change (in `core`,
//! `vendor`, C, a macro's output or outside the workspace), a use comes
//! from a macro, or the old name is written in code the checker doesn't
//! check, whose uses can't be told apart (SPEC "Toolchain and CLI" →
//! "LSP").

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use wid_diagnostics::{FileId, SourceFile, Span};
use wid_query::Analysis;
use wid_sema::PackageId;
use wid_sema::index::{Index, SymbolId, SymbolKind};
use wid_sema::uses::{RefKind, RefTarget, TypedKind};
use wid_syntax::lexer::{NameShape, lex, name_shape};
use wid_syntax::token::TokenKind;

/// What is renamed, for the rules its new name follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NameKind {
    /// A struct, enum, union, module or type alias.
    Type,
    /// A constant.
    Constant,
    /// A method; whether it returns `Bool` (a name ending in `?` must) and
    /// whether it is type-level (`def self.…`, which can't be `new`).
    Method { returns_bool: bool, type_level: bool },
    /// A macro, which returns `Code`.
    Macro,
    /// An overload set.
    Overload,
    /// A field.
    Field,
    /// A member of an enum.
    EnumMember,
    /// A local variable.
    Variable,
    /// A parameter.
    Parameter,
    /// An import name.
    Package,
}

impl NameKind {
    /// What it is, with its article.
    fn a_describe(self) -> &'static str {
        match self {
            NameKind::Type => "a type",
            NameKind::Constant => "a constant",
            NameKind::Method { .. } => "a method",
            NameKind::Macro => "a macro",
            NameKind::Overload => "an overload set",
            NameKind::Field => "a field",
            NameKind::EnumMember => "an enum member",
            NameKind::Variable => "a variable",
            NameKind::Parameter => "a parameter",
            NameKind::Package => "an import name",
        }
    }

    /// What it is, with "the".
    fn the_describe(self) -> String {
        let what = self.a_describe();
        format!("the {}", what.split_once(' ').map_or(what, |(_, rest)| rest))
    }
}

/// Checks that `new` can name what `old` names, a `kind`: one name as the
/// lexer reads it, not a keyword, capitalized for a type or constant and
/// lowercase (or `_`) for anything else, ending in `?` or `!` only for a
/// method or overload set (`?` only when the method returns `Bool`, never
/// for a macro), not a builtin type's name for a type, and not `new` for a
/// type-level method.
pub(crate) fn check_name(kind: NameKind, old: &str, new: &str) -> Result<(), String> {
    const REST: &str = "followed by letters, digits and `_`";
    let what = kind.a_describe();
    let shape = name_shape(new);
    match shape {
        NameShape::Invalid if new.is_empty() => return Err("the new name is empty".into()),
        NameShape::Invalid => {
            return Err(format!(
                "`{new}` isn't a name: a name is a letter or `_` {REST}, and a method's may end in `?` or `!`"
            ));
        }
        NameShape::Keyword => return Err(format!("`{new}` is a keyword, so it can't name {what}")),
        NameShape::Operator => return Err(format!("`{new}` is an operator; rename can't make {what} an operator")),
        NameShape::Const | NameShape::Ident { .. } => {}
    }
    let capital = matches!(kind, NameKind::Type | NameKind::Constant);
    if capital && shape != NameShape::Const {
        let fixed = capitalize(new);
        return Err(format!("{}'s name starts with a capital letter, {REST}: `{fixed}`, not `{new}`", capital_what(what)));
    }
    if !capital && shape == NameShape::Const {
        let fixed = lowercase(new);
        return Err(format!(
            "{}'s name starts with a lowercase letter or `_`, {REST}: `{fixed}`, not `{new}`; a capital letter starts a constant or a type",
            capital_what(what)
        ));
    }
    if shape == (NameShape::Ident { suffixed: true }) {
        let suffix = &new[new.len() - 1..];
        match kind {
            NameKind::Method { returns_bool: false, .. } if suffix == "?" => {
                return Err(format!(
                    "a name ending in `?` asks a yes-or-no question, so a method named so returns `Bool` (E0330), and `{old}` doesn't"
                ));
            }
            NameKind::Macro if suffix == "?" => {
                return Err("a macro returns `Code`, so its name can't end in `?` (E0330)".into());
            }
            NameKind::Method { .. } | NameKind::Overload | NameKind::Macro => {}
            _ => {
                return Err(format!(
                    "only a method's name can end in `?` or `!`, and this is {what}: `{}`",
                    &new[..new.len() - 1]
                ));
            }
        }
    }
    if kind == NameKind::Type && wid_sema::is_reserved_type_name(new) {
        return Err(format!("`{new}` is a builtin type, which no declaration can take the name of"));
    }
    if matches!(kind, NameKind::Method { type_level: true, .. }) && new == "new" {
        return Err("`new` is the builtin constructor, `Type.new(field: value)`, so a type-level method can't take it".into());
    }
    Ok(())
}

fn capital_what(what: &str) -> String {
    let mut out = what.to_string();
    if let Some(first) = out.get(..1) {
        let upper = first.to_uppercase();
        out.replace_range(..1, &upper);
    }
    out
}

fn capitalize(name: &str) -> String {
    let trimmed = name.trim_start_matches('_');
    let mut chars = trimmed.chars();
    match chars.next() {
        Some(c) => c.to_uppercase().chain(chars).collect(),
        None => name.to_string(),
    }
}

fn lowercase(name: &str) -> String {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) => c.to_lowercase().chain(chars).collect(),
        None => name.to_string(),
    }
}

/// What a position names, in one check.
#[derive(Clone, Debug)]
pub(crate) struct Subject {
    /// What the name refers to, in the check it was found in.
    pub(crate) target: RefTarget,
    /// The name as declared.
    pub(crate) name: String,
    /// What it is.
    pub(crate) kind: NameKind,
    /// Where it is declared: the file and the name's byte range (for an
    /// import name, the `import` or `cimport`); `None` for an import name
    /// the check has no declaration for.
    pub(crate) declared: Option<(PathBuf, u32, u32)>,
    /// The name at the position, as bytes of its file.
    pub(crate) at: (u32, u32),
}

/// A place to edit or show: a file on disk and a byte range of it.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Place {
    pub(crate) path: PathBuf,
    pub(crate) start: u32,
    pub(crate) end: u32,
}

/// Where `span` is: its file on disk (the macro call's, for generated
/// code) and its range.
fn place(analysis: &Analysis, span: Span) -> Option<Place> {
    let file = analysis.sources.file(span.file);
    file.path.is_absolute().then(|| Place { path: file.path.clone(), start: span.start, end: span.end })
}

/// Where `name` is inside `span` as a whole word, when it is there once:
/// `amount` in the parameter `amount: Int`, `MAX` in `geo.MAX`, `f` in
/// `:f`.
fn narrow(text: &str, span: Span, name: &str) -> Option<Span> {
    let is_name = |c: char| c.is_alphanumeric() || c == '_';
    let mut found = None;
    for (i, _) in text.match_indices(name) {
        let before = text[..i].chars().next_back();
        let after = text[i + name.len()..].chars().next();
        let whole = !before.is_some_and(is_name) && !after.is_some_and(|c| is_name(c) || c == '?' || c == '!');
        if whole {
            if found.is_some() {
                return None;
            }
            found = Some(i);
        }
    }
    let i = found? as u32;
    Some(Span::new(span.file, span.start + i, span.start + i + name.len() as u32))
}

/// The import name an `import` or `cimport` binds: its `as:` symbol, or
/// the last part of the imported path.
fn import_alias(text: &str) -> Option<String> {
    if let Some(at) = text.find("as:") {
        let rest = text[at + 3..].trim_start().strip_prefix(':')?;
        let end = rest.find(|c: char| !(c.is_alphanumeric() || c == '_')).unwrap_or(rest.len());
        return Some(rest[..end].to_string());
    }
    let path = text.split('"').nth(1)?;
    let last = path.rsplit(['/', ':']).next()?;
    Some(last.to_string())
}

/// What the name at byte `offset` of `file` refers to, with the cursor
/// just past a name counting as on it. `Ok(None)` where no name is; an
/// error for a name that can't be renamed whatever the new name is.
pub(crate) fn subject_at(analysis: &Analysis, file: FileId, offset: u32) -> Result<Option<Subject>, String> {
    let text = &analysis.sources.file(file).text;
    let mut found = wid_query::ref_at(analysis, file, offset);
    if found.is_none()
        && let Some(c) = text[..offset as usize].chars().next_back()
        && (c.is_alphanumeric() || matches!(c, '_' | '?' | '!'))
    {
        found = wid_query::ref_at(analysis, file, offset - c.len_utf8() as u32);
    }
    let Some(r) = found else { return Ok(None) };
    let index = &analysis.index;
    let written = analysis.sources.slice(r.span);
    let (name, kind, declared_span) = match &r.target {
        RefTarget::Symbol(id) => {
            let s = index.symbol(*id);
            let kind = match s.kind {
                SymbolKind::Constant => NameKind::Constant,
                SymbolKind::TypeAlias
                | SymbolKind::Struct
                | SymbolKind::Enum
                | SymbolKind::Union
                | SymbolKind::Module => NameKind::Type,
                SymbolKind::Method => {
                    let returns =
                        analysis.uses.typed(s.span).is_some_and(|t| t.ty.ends_with("-> Bool"));
                    NameKind::Method { returns_bool: returns, type_level: s.is_static }
                }
                SymbolKind::Macro => NameKind::Macro,
                SymbolKind::Overload => NameKind::Overload,
                SymbolKind::Extension => return Ok(None),
            };
            if name_shape(&s.name) == NameShape::Operator {
                return Err(format!("`{}` is an operator method; operators keep their names", s.name));
            }
            (s.name.clone(), kind, Some(s.span))
        }
        RefTarget::Field { owner, index: i } => {
            let f = index.symbol(*owner).fields.get(*i).ok_or("the field is gone")?;
            (f.name.clone(), NameKind::Field, Some(f.span))
        }
        RefTarget::EnumMember { owner, index: i } => {
            let m = index.symbol(*owner).enum_members.get(*i).ok_or("the enum member is gone")?;
            (m.name.clone(), NameKind::EnumMember, Some(m.span))
        }
        RefTarget::Local { binding, parameter } => {
            let name = wid_query::target_name(analysis, &r.target).unwrap_or_default();
            let kind = if *parameter { NameKind::Parameter } else { NameKind::Variable };
            (name, kind, Some(*binding))
        }
        RefTarget::Builtin(name) => return Err(format!("`{name}` is a builtin type, which keeps its name")),
        RefTarget::Package(_) => {
            let name = match r.kind {
                RefKind::Declaration => import_alias(written).ok_or("this import binds no name")?,
                _ => written.to_string(),
            };
            let declared = (r.kind == RefKind::Declaration).then_some(r.span);
            (name, NameKind::Package, declared)
        }
    };
    let at = match (&r.target, r.kind) {
        (RefTarget::Package(_), RefKind::Declaration) => {
            let alias = written.find("as:").and_then(|i| narrow(&written[i..], Span::new(r.span.file, r.span.start + i as u32, r.span.end), &name));
            alias.unwrap_or(r.span)
        }
        _ => narrow(written, r.span, &name).unwrap_or(r.span),
    };
    let declared = declared_span.and_then(|span| place(analysis, span)).map(|p| (p.path, p.start, p.end));
    let declared = declared.or_else(|| match &r.target {
        RefTarget::Package(p) => {
            // The `import` of this file that binds the name.
            let decl = analysis.uses.refs.iter().find(|d| {
                d.kind == RefKind::Declaration
                    && d.target == RefTarget::Package(*p)
                    && d.span.file == file
                    && import_alias(analysis.sources.slice(d.span)).as_deref() == Some(name.as_str())
            })?;
            place(analysis, decl.span).map(|p| (p.path, p.start, p.end))
        }
        _ => None,
    });
    Ok(Some(Subject { target: r.target.clone(), name, kind, declared, at: (at.start, at.end) }))
}

/// The target a subject's declaration has in another check of the same
/// files; `None` when that check doesn't know it.
fn target_in(analysis: &Analysis, subject: &Subject) -> Option<RefTarget> {
    let (path, start, end) = subject.declared.as_ref()?;
    let file = analysis.sources.find_by_path(path)?;
    let span = Span::new(file, *start, *end);
    match &subject.target {
        RefTarget::Local { parameter, .. } => Some(RefTarget::Local { binding: span, parameter: *parameter }),
        RefTarget::Package(_) => None,
        _ => analysis
            .uses
            .refs
            .iter()
            .find(|r| r.span == span && r.kind == RefKind::Declaration)
            .map(|r| r.target.clone()),
    }
}

/// A use of a subject, in a check.
pub(crate) struct Use {
    /// Where it is written: the name, or the macro call that generated it.
    pub(crate) place: Place,
    pub(crate) kind: RefKind,
    pub(crate) via_macro: Option<String>,
}

/// Every use of a subject found in `analyses` (the subject's own check
/// first), each once: in the subject's own check, the uses of its target;
/// in the others, those of the target declared at the same place. An
/// import name's uses are those in its own file. The declaration comes
/// first, even when the checker has no use of it recorded (a parameter of
/// a method that nothing calls).
pub(crate) fn uses(analyses: &[&Analysis], subject: &Subject) -> Vec<Use> {
    let mut out: Vec<Use> = Vec::new();
    let mut seen: BTreeSet<Place> = BTreeSet::new();
    if let Some((path, start, end)) = &subject.declared {
        let place = Place { path: path.clone(), start: *start, end: *end };
        seen.insert(place.clone());
        out.push(Use { place, kind: RefKind::Declaration, via_macro: None });
    }
    for (i, analysis) in analyses.iter().enumerate() {
        let target = if i == 0 { Some(subject.target.clone()) } else { target_in(analysis, subject) };
        let Some(target) = target else { continue };
        let own_file = subject.declared.as_ref().map(|(p, _, _)| p.clone());
        for r in wid_query::uses_of(analysis, &target) {
            let Some(place) = place(analysis, r.span) else { continue };
            if subject.kind == NameKind::Package && own_file.as_ref() != Some(&place.path) {
                continue;
            }
            if seen.insert(place.clone()) {
                out.push(Use { place, kind: r.kind, via_macro: r.via_macro.clone() });
            }
        }
    }
    out
}

/// The checks a rename reads, and the folders it may edit.
pub(crate) struct Scope<'a> {
    /// The checks, the one of the package the position is in first.
    pub(crate) analyses: Vec<&'a Analysis>,
    /// The workspace folders; a rename edits only files in them.
    pub(crate) workspace: Vec<PathBuf>,
    /// The Wid root, whose `core` and `vendor` a rename never edits.
    pub(crate) wid_root: PathBuf,
}

impl Scope<'_> {
    fn in_workspace(&self, path: &Path) -> bool {
        self.workspace.iter().any(|w| path.starts_with(w))
    }

    fn in_library(&self, path: &Path) -> bool {
        ["core", "vendor"].iter().any(|c| path.starts_with(self.wid_root.join(c)))
    }
}

/// Shows a place to the user: `main.wid:3:5`.
fn shown(analysis: &Analysis, place: &Place) -> String {
    match analysis.sources.find_by_path(&place.path) {
        Some(id) => {
            let file = analysis.sources.file(id);
            let (line, column) = file.line_col(place.start);
            format!("{}:{line}:{column}", file.display)
        }
        None => place.path.display().to_string(),
    }
}

/// The edits that rename a subject to `new`: for each place, the bytes to
/// replace and the text to put there.
pub(crate) fn plan(scope: &Scope<'_>, subject: &Subject, new: &str) -> Result<Vec<(Place, String)>, String> {
    let analysis = *scope.analyses.first().ok_or("the package isn't checked")?;
    let old = subject.name.as_str();
    check_name(subject.kind, old, new)?;
    let the = subject.kind.the_describe();
    refuse_declaration(scope, analysis, subject)?;
    if new == old {
        return Ok(Vec::new());
    }
    let uses = uses(&scope.analyses, subject);
    let mut edits: Vec<(Place, String)> = Vec::new();
    let mut texts: HashMap<PathBuf, String> = HashMap::new();
    for u in &uses {
        if let Some(m) = &u.via_macro {
            return Err(format!(
                "`{old}` is used in the code the macro `{m}` generates ({}); rename it there by hand",
                shown(analysis, &u.place)
            ));
        }
        if !scope.in_workspace(&u.place.path) || scope.in_library(&u.place.path) {
            return Err(format!(
                "`{old}` is used in `{}`, outside the workspace, which a rename doesn't change",
                u.place.path.display()
            ));
        }
        let text = match texts.get(&u.place.path) {
            Some(text) => text,
            None => {
                let text = file_text(&scope.analyses, &u.place.path).ok_or("a file of the rename isn't loaded")?;
                texts.entry(u.place.path.clone()).or_insert(text)
            }
        };
        let written = text.get(u.place.start as usize..u.place.end as usize).unwrap_or_default();
        let span = Span::new(FileId(0), u.place.start, u.place.end);
        if subject.kind == NameKind::Package && u.kind == RefKind::Declaration {
            edits.push(import_edit(&u.place, written, old, new));
            continue;
        }
        let Some(at) = narrow(written, span, old) else {
            return Err(format!("`{old}` isn't written as such at {}, so it can't be renamed there", shown(analysis, &u.place)));
        };
        edits.push((Place { path: u.place.path.clone(), start: at.start, end: at.end }, new.to_string()));
    }
    collisions(scope, analysis, subject, new, &uses, &the)?;
    unchecked(scope, subject)?;
    edits.sort();
    edits.dedup();
    Ok(edits)
}

/// The edit that renames an import name where it is bound: its `as:`
/// symbol, or a new `as:` after the imported path.
fn import_edit(place: &Place, written: &str, old: &str, new: &str) -> (Place, String) {
    if let Some(at) = written.find("as:")
        && let Some(found) = narrow(&written[at..], Span::new(FileId(0), place.start + at as u32, place.end), old)
    {
        return (Place { path: place.path.clone(), start: found.start, end: found.end }, new.to_string());
    }
    // `import "./geo"`: name it after the path's closing quote.
    let close = written.match_indices('"').nth(1).map_or(written.len(), |(i, _)| i + 1);
    let at = place.start + close as u32;
    (Place { path: place.path.clone(), start: at, end: at }, format!(", as: :{new}"))
}

/// The text of a file as the checks read it.
fn file_text(analyses: &[&Analysis], path: &Path) -> Option<String> {
    analyses.iter().find_map(|a| a.sources.find_by_path(path).map(|id| a.sources.file(id).text.to_string()))
}

/// Refuses what isn't the user's to rename: a declaration in C, in `core`
/// or `vendor`, in a macro's output, or outside the workspace.
fn refuse_declaration(scope: &Scope<'_>, analysis: &Analysis, subject: &Subject) -> Result<(), String> {
    let index = &analysis.index;
    let old = &subject.name;
    let symbol = match &subject.target {
        RefTarget::Symbol(id) => Some(*id),
        RefTarget::Field { owner, .. } | RefTarget::EnumMember { owner, .. } => Some(*owner),
        _ => None,
    };
    if let Some(id) = symbol {
        let s = index.symbol(id);
        let package = index.package(s.package);
        if let Some(c) = &s.c {
            return Err(format!("`{old}` comes from C (`{}` in `{}`), which a rename doesn't change", c.name, c.header));
        }
        if package.path.starts_with("cimport:") {
            return Err(format!("`{old}` comes from C, by a `cimport`, which a rename doesn't change"));
        }
        if package.path.starts_with("core:") || package.path.starts_with("vendor:") {
            return Err(format!(
                "`{old}` is declared in `{}`, part of Wid's own library, which a rename doesn't change",
                package.path
            ));
        }
        if s.span.file.expansion_index().is_some() {
            let (call, m) = wid_query::written(&analysis.sources, s.span);
            let at = place(analysis, call).map(|p| shown(analysis, &p)).unwrap_or_default();
            return Err(format!(
                "`{old}` is declared by the code the macro `{}` generates ({at}); rename it in the macro call or the macro",
                m.unwrap_or_default()
            ));
        }
    }
    if let RefTarget::Local { binding, .. } = &subject.target
        && binding.file.expansion_index().is_some()
    {
        return Err(format!("`{old}` is declared in the code a macro generates; rename it in the macro"));
    }
    match &subject.declared {
        Some((path, _, _)) if scope.in_library(path) => {
            Err(format!("`{old}` is declared in `{}`, part of Wid's own library, which a rename doesn't change", path.display()))
        }
        Some((path, _, _)) if !scope.in_workspace(path) => {
            Err(format!("`{old}` is declared in `{}`, outside the workspace, which a rename doesn't change", path.display()))
        }
        Some(_) => Ok(()),
        None => Err(format!("`{old}` has no declaration a rename can change")),
    }
}

/// Every name a type answers to: its fields (its own and promoted), enum
/// members and members from every origin, with where each is declared.
fn names_of_type(index: &Index, ty: SymbolId) -> Vec<(String, Span)> {
    let ty = index.alias_target(ty);
    let s = index.symbol(ty);
    let mut out: Vec<(String, Span)> = Vec::new();
    for f in index.fields_of(ty) {
        let field = &index.symbol(f.owner).fields[f.index];
        out.push((field.name.clone(), field.span));
    }
    out.extend(s.enum_members.iter().map(|m| (m.name.clone(), m.span)));
    for group in index.member_groups(ty) {
        out.extend(group.members.iter().map(|&m| (index.symbol(m).name.clone(), index.symbol(m).span)));
    }
    out
}

/// The types whose members include `member` (from any origin), or whose
/// fields include field `field` of `owner`.
fn types_reaching(index: &Index, matches: impl Fn(SymbolId) -> bool) -> Vec<SymbolId> {
    (0..index.symbols.len() as u32)
        .map(SymbolId)
        .filter(|&id| {
            matches!(index.symbol(id).kind, SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Union | SymbolKind::Module)
        })
        .filter(|&id| matches(id))
        .collect()
}

/// Refuses a new name that is already taken where the old one is visible,
/// or that would capture a use: a local of that name where a renamed
/// method is called without a receiver.
fn collisions(
    scope: &Scope<'_>,
    analysis: &Analysis,
    subject: &Subject,
    new: &str,
    uses: &[Use],
    the: &str,
) -> Result<(), String> {
    let index = &analysis.index;
    let taken = |what: &str, span: Span| -> String {
        let at = place(analysis, span).map(|p| format!(" ({})", shown(analysis, &p))).unwrap_or_default();
        format!("`{new}` is already {what}{at}, so {the} can't take its name")
    };
    let package_names = |pkg: PackageId| -> Result<(), String> {
        let p = index.package(pkg);
        if let Some(&id) = p.scope.get(new) {
            let s = index.symbol(id);
            return Err(taken(&format!("{} in this package", s.kind.a_describe()), s.span));
        }
        if p.imports.contains_key(new) {
            return Err(format!("`{new}` is already an import name in this package, so {the} can't take its name"));
        }
        if let Some(prelude) = index.prelude.filter(|&pr| pr != pkg)
            && let Some(&id) = index.package(prelude).scope.get(new)
            && !index.symbol(id).private
        {
            return Err(format!(
                "`{new}` is the prelude's {}, which every file sees, so {the} can't take its name without hiding it",
                index.symbol(id).kind.a_describe().trim_start_matches("a ").trim_start_matches("an ")
            ));
        }
        Ok(())
    };
    match &subject.target {
        RefTarget::Symbol(id) => {
            let s = index.symbol(*id);
            match s.owner {
                None => package_names(s.package)?,
                Some(owner) => {
                    let o = index.symbol(owner);
                    let mut types = types_reaching(index, |t| {
                        index.member_groups(t).iter().any(|g| g.members.contains(id))
                    });
                    if o.kind == SymbolKind::Extension {
                        types.extend(o.links.iter().filter_map(|l| l.symbol));
                    }
                    for ty in types {
                        if let Some((_, span)) = names_of_type(index, ty).into_iter().find(|(n, _)| n == new) {
                            let what = format!("a member of `{}`", index.path_of(ty));
                            return Err(taken(&what, span));
                        }
                    }
                    // The other extensions of the builtin types it extends.
                    for link in o.links.iter().filter(|l| l.symbol.is_none()) {
                        for group in index.builtin_groups(&link.text) {
                            if let Some(&m) = group.members.iter().find(|&&m| index.symbol(m).name == new) {
                                return Err(taken(&format!("a method of `{}`", link.text), index.symbol(m).span));
                            }
                        }
                    }
                }
            }
            // A local of the new name where the method is called bare
            // would be read instead.
            if matches!(subject.kind, NameKind::Method { .. } | NameKind::Overload | NameKind::Macro) {
                bare_uses(scope, analysis, subject, new, uses)?;
            }
        }
        RefTarget::Field { owner, index: i } => {
            let types = types_reaching(index, |t| index.fields_of(t).iter().any(|f| f.owner == *owner && f.index == *i));
            for ty in types {
                if let Some((_, span)) = names_of_type(index, ty).into_iter().find(|(n, _)| n == new) {
                    return Err(taken(&format!("a member of `{}`", index.path_of(ty)), span));
                }
            }
        }
        RefTarget::EnumMember { owner, .. } => {
            if let Some((_, span)) = names_of_type(index, *owner).into_iter().find(|(n, _)| n == new) {
                return Err(taken(&format!("a member of `{}`", index.path_of(*owner)), span));
            }
        }
        RefTarget::Local { binding, .. } => {
            // Any use of the new name in the method would change meaning.
            let extent = wid_query::declaration_at(analysis, binding.file, binding.start)
                .map(|id| index.symbol(id).span)
                .and_then(|name| analysis.extents.declaration(&analysis.sources, name));
            let file = analysis.sources.file(analysis.sources.real_file(binding.file));
            let (start, end) = extent.map_or((0, file.text.len() as u32), |e| (e.start, e.end));
            if let Some(at) = words(file).into_iter().find(|(w, s)| w == new && s.start >= start && s.end <= end) {
                let p = Place { path: file.path.clone(), start: at.1.start, end: at.1.end };
                return Err(format!(
                    "`{new}` is already used in the same method ({}), so {the} can't take its name",
                    shown(analysis, &p)
                ));
            }
        }
        RefTarget::Package(_) => {
            if let Some(pkg) = analysis.root() {
                package_names(pkg)?;
            }
        }
        RefTarget::Builtin(_) => {}
    }
    Ok(())
}

/// Refuses renaming a method to `new` where a use calls it without a
/// receiver and a local named `new` is visible there.
fn bare_uses(scope: &Scope<'_>, analysis: &Analysis, subject: &Subject, new: &str, uses: &[Use]) -> Result<(), String> {
    let mut parsed: HashMap<PathBuf, (wid_syntax::ast::File, String)> = HashMap::new();
    for u in uses.iter().filter(|u| u.kind == RefKind::Call || u.kind == RefKind::Read) {
        let Some(text) = file_text(&scope.analyses, &u.place.path) else { continue };
        let before = text[..u.place.start as usize].trim_end_matches(char::is_whitespace);
        if before.ends_with('.') || before.ends_with(':') {
            continue;
        }
        let (file, text) = parsed
            .entry(u.place.path.clone())
            .or_insert_with(|| (wid_syntax::parse_file(FileId(0), &text).0, text.clone()));
        let locals = crate::syntax::locals_at(file, text, u.place.start as usize);
        if locals.iter().any(|l| l.name == new) {
            return Err(format!(
                "`{new}` is a local variable where `{}` is called without a receiver ({}), so the call would read the variable instead",
                subject.name,
                shown(analysis, &u.place)
            ));
        }
    }
    Ok(())
}

/// Every name written in a file, as the lexer reads it, with its span:
/// identifiers, constants, `@field` and `:symbol` names.
fn words(file: &SourceFile) -> Vec<(String, Span)> {
    let lexed = lex(file.id, &file.text);
    lexed
        .tokens
        .iter()
        .filter_map(|t| {
            let text = file.text.get(t.span.start as usize..t.span.end as usize)?;
            let (skip, word) = match t.kind {
                TokenKind::Ident | TokenKind::Const => (0, text),
                TokenKind::IVar => (1, text.get(1..)?),
                TokenKind::Symbol => (1, text.get(1..)?),
                _ => return None,
            };
            Some((word.to_string(), Span::new(t.span.file, t.span.start + skip, t.span.end)))
        })
        .collect()
}

/// Refuses a rename when the old name is written in the workspace's
/// files where the checker recorded nothing: code it doesn't check (a
/// generic method nothing instantiates, a module nothing includes, a
/// `comptime if` branch not taken, a macro's `quote`), whose names can't
/// be told apart, so a use of the renamed declaration there would be left
/// behind.
fn unchecked(scope: &Scope<'_>, subject: &Subject) -> Result<(), String> {
    let old = subject.name.as_str();
    let mut checked: BTreeSet<PathBuf> = BTreeSet::new();
    for analysis in &scope.analyses {
        for file in analysis.sources.files() {
            if !file.path.is_absolute()
                || !scope.in_workspace(&file.path)
                || scope.in_library(&file.path)
                || checked.contains(&file.path)
            {
                continue;
            }
            checked.insert(file.path.clone());
            for (word, span) in words(file) {
                if word != old || recorded(analysis, span) {
                    continue;
                }
                let at = Place { path: file.path.clone(), start: span.start, end: span.end };
                return Err(format!(
                    "`{old}` is also written at {}, in code the checker doesn't check (like a generic method nothing calls, or a macro's `quote`), so a rename can't tell whether it means the same `{old}`",
                    shown(analysis, &at)
                ));
            }
        }
    }
    Ok(())
}

/// Whether the checker recorded what the name at `span` is: a use around
/// it, a binding, parameter, field, written type or declaration around
/// it, or an expression that is just the name.
fn recorded(analysis: &Analysis, span: Span) -> bool {
    let around = |s: Span| s.file == span.file && s.start <= span.start && span.end <= s.end;
    analysis.uses.refs.iter().any(|r| around(r.span))
        || analysis.uses.types.iter().any(|t| match t.kind {
            TypedKind::Expression | TypedKind::Call => t.span == span,
            _ => around(t.span),
        })
}

/// The uses of a subject as an LSP client wants them for references:
/// written places, with the declaration when asked for.
pub(crate) fn references(analyses: &[&Analysis], subject: &Subject, declaration: bool) -> Vec<Place> {
    uses(analyses, subject)
        .into_iter()
        .filter(|u| declaration || u.kind != RefKind::Declaration)
        .map(|u| u.place)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{NameKind, check_name, import_alias, narrow};
    use wid_diagnostics::{FileId, Span};

    #[test]
    fn new_names_follow_the_rules_for_what_is_renamed() {
        let method = NameKind::Method { returns_bool: false, type_level: false };
        let predicate = NameKind::Method { returns_bool: true, type_level: false };
        assert_eq!(check_name(method, "go", "run"), Ok(()));
        assert_eq!(check_name(method, "go", "run!"), Ok(()));
        assert_eq!(check_name(predicate, "ok?", "valid?"), Ok(()));
        assert_eq!(check_name(NameKind::Variable, "x", "_x"), Ok(()));
        assert_eq!(check_name(NameKind::Type, "Ball", "Orb"), Ok(()));
        assert_eq!(check_name(NameKind::Constant, "MAX", "Limit"), Ok(()));
        let refused = |kind, old, new: &str| check_name(kind, old, new).expect_err(new);
        assert!(refused(method, "go", "end").contains("`end` is a keyword"));
        assert!(refused(NameKind::Variable, "x", "self").contains("keyword"));
        assert!(refused(method, "go", "Run").contains("lowercase"));
        assert!(refused(NameKind::Type, "Ball", "orb").contains("`Orb`, not `orb`"));
        assert!(refused(NameKind::Constant, "MAX", "max").contains("capital"));
        assert!(refused(method, "go", "ready?").contains("returns `Bool` (E0330)"));
        assert!(refused(NameKind::Macro, "m", "m?").contains("`Code`"));
        assert_eq!(check_name(NameKind::Macro, "m", "m!"), Ok(()));
        assert!(refused(NameKind::Field, "hp", "hp?").contains("only a method's name"));
        assert!(refused(NameKind::Variable, "x", "a b").contains("isn't a name"));
        assert!(refused(NameKind::Variable, "x", "2x").contains("isn't a name"));
        assert!(refused(NameKind::Variable, "x", "").contains("empty"));
        assert!(refused(method, "go", "+").contains("operator"));
        assert!(refused(NameKind::Type, "Ball", "Int").contains("builtin type"));
        let type_level = NameKind::Method { returns_bool: false, type_level: true };
        assert!(refused(type_level, "create", "new").contains("constructor"));
        assert_eq!(check_name(method, "go", "new"), Ok(()));
    }

    #[test]
    fn names_are_found_inside_what_was_recorded() {
        let at = |text: &str, name: &str| {
            narrow(text, Span::new(FileId(0), 10, 10 + text.len() as u32), name).map(|s| (s.start, s.end))
        };
        assert_eq!(at("amount: Int", "amount"), Some((10, 16)));
        assert_eq!(at("geo.MAX", "MAX"), Some((14, 17)));
        assert_eq!(at(":heal", "heal"), Some((11, 15)));
        assert_eq!(at("|x|", "x"), Some((11, 12)));
        assert_eq!(at("heal?", "heal"), None);
        assert_eq!(at("a + a", "a"), None);
        assert_eq!(import_alias("import \"./geo\"").as_deref(), Some("geo"));
        assert_eq!(import_alias("import \"core:fmt\", as: :f").as_deref(), Some("f"));
        assert_eq!(import_alias("cimport \"raylib.h\", as: :rl, prefix: \"x\"").as_deref(), Some("rl"));
    }
}

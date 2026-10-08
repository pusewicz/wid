//! `refs`, `calls` and `type`: answers read from what the checker recorded
//! as it resolved names and lowered code ([`wid_sema::uses`]).
//!
//! A use in code a macro generated is reported at the macro call that
//! generated it (the outermost one, which is in a file), with the macro as
//! that call names it. A use's context is the declaration around it: the
//! smallest one whose extent holds it, other than the declaration it is
//! the name of.

use std::collections::HashMap;
use std::path::Path;

use wid_diagnostics::{FileId, SourceMap, Span};
use wid_sema::PackageId;
use wid_sema::index::{SymbolId, SymbolKind, Target};
use wid_sema::uses::{Ref, RefKind, RefTarget, Typed, TypedKind};

use crate::item::{Item, Location};
use crate::{Analysis, Failure};

/// A use of a symbol.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefItem {
    /// Where it is: the name, or for a use in generated code, the macro
    /// call.
    pub location: Location,
    /// How the symbol is used.
    pub kind: RefKind,
    /// The path of the declaration the use is in (`Player.heal`); `None` at
    /// the top level of a file, like an `import`.
    pub context: Option<String>,
    /// For a use in code a macro generated, the macro as its call names it.
    pub via_macro: Option<String>,
}

/// What is at a position.
#[derive(Clone, Debug)]
pub struct TypeItem {
    /// The name or code at the position.
    pub location: Location,
    /// The expression, binding, parameter, written type or declaration name
    /// whose type is given.
    pub span: Location,
    /// The type as Wid displays it; `None` for what has none (a package,
    /// a module, a macro, an overload set).
    pub ty: Option<String>,
    /// For code checked once per generic instance with different types,
    /// each instance's type; empty otherwise.
    pub instances: Vec<String>,
    /// `expression`, `call`, `local`, `parameter`, `field`, `type`,
    /// `declaration` or `package`.
    pub kind: &'static str,
    /// The declaration (or local) the name at the position refers to.
    pub refers_to: Option<Item>,
}

/// A position as written: `main.wid:12:5`, 1-based.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Position {
    /// The file, as given.
    pub file: String,
    /// The line.
    pub line: u32,
    /// The column, in characters.
    pub column: u32,
}

impl Position {
    /// Reads `file:line:column`; `None` unless both numbers are positive.
    pub fn parse(text: &str) -> Option<Position> {
        let mut parts = text.rsplitn(3, ':');
        let column = parts.next()?.parse().ok().filter(|c| *c > 0)?;
        let line = parts.next()?.parse().ok().filter(|l| *l > 0)?;
        let file = parts.next().filter(|f| !f.is_empty())?;
        Some(Position { file: file.to_string(), line, column })
    }
}

/// Why `type` found nothing at a position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PositionError {
    /// It isn't `file:line:column` with positive numbers.
    Malformed,
    /// No file of the program has the name.
    UnknownFile {
        /// The files of the package asked about, as diagnostics show them.
        candidates: Vec<String>,
    },
    /// The file has fewer lines.
    NoLine {
        /// How many lines it has.
        lines: u32,
    },
    /// The line is shorter.
    NoColumn {
        /// The last column on the line: one past its last character.
        last: u32,
    },
    /// Nothing recorded covers the position.
    Nothing {
        /// The recorded spans nearest to it, nearest first: their start
        /// line and column, and their text.
        nearest: Vec<Nearby>,
    },
}

/// A recorded span near a position.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Nearby {
    /// The span.
    pub span: Span,
    /// Its 1-based start line.
    pub line: u32,
    /// Its 1-based start column.
    pub column: u32,
    /// Its text, up to the end of its first line, shortened when long.
    pub text: String,
}

/// Where code at `span` is written: for code a macro generated, the call
/// that generated it (the outermost) and the macro as the call names it.
pub fn written(sources: &SourceMap, span: Span) -> (Span, Option<String>) {
    match sources.expansion_chain(span).last() {
        Some(outermost) => (outermost.call_site, Some(outermost.name.clone())),
        None => (span, None),
    }
}

/// What a symbol path's target is in the recorded uses.
fn ref_target(target: Target) -> RefTarget {
    match target {
        Target::Symbol(id) => RefTarget::Symbol(id),
        Target::Field { owner, index, .. } => RefTarget::Field { owner, index },
        Target::EnumMember { owner, index } => RefTarget::EnumMember { owner, index },
        Target::Package(p) => RefTarget::Package(p),
        Target::Builtin(name) => RefTarget::Builtin(name),
    }
}

/// Every use of what a symbol path names in `pkg`, private ones too, in
/// the program's code that the checker lowered, sorted by file, line and
/// column. Uses in the code of `cimport` packages are left out.
pub fn refs(analysis: &Analysis, pkg: PackageId, path: &str) -> Result<Vec<RefItem>, Failure> {
    let target = ref_target(crate::resolve(&analysis.index, pkg, path, true)?);
    let items = analysis.items(true);
    let contexts = Contexts::new(analysis);
    let mut out: Vec<(Span, RefItem)> = Vec::new();
    for r in analysis.uses.refs_to(&target) {
        let (span, via_macro) = written(&analysis.sources, r.span);
        if analysis.sources.file(span.file).display.starts_with("cimport:") {
            continue;
        }
        let Some(location) = items.location(span) else { continue };
        let context = contexts.around(analysis, span);
        out.push((span, RefItem { location, kind: r.kind, context, via_macro }));
    }
    out.sort_by(|(a, x), (b, y)| {
        let key = |l: &Location| (l.file.clone(), l.line, l.column, l.end_line, l.end_column);
        key(&x.location).cmp(&key(&y.location)).then(a.cmp(b)).then(x.kind.cmp(&y.kind))
    });
    out.dedup_by(|(_, x), (_, y)| x == y);
    Ok(out.into_iter().map(|(_, r)| r).collect())
}

/// The calls of what a symbol path names: its `call` uses.
pub fn calls(analysis: &Analysis, pkg: PackageId, path: &str) -> Result<Vec<RefItem>, Failure> {
    Ok(refs(analysis, pkg, path)?.into_iter().filter(|r| r.kind == RefKind::Call).collect())
}

/// The declarations whose code holds a span, for a use's context.
struct Contexts {
    /// By file, each declaration's extent, symbol and name.
    by_file: HashMap<FileId, Vec<(Span, SymbolId, Span)>>,
}

impl Contexts {
    fn new(analysis: &Analysis) -> Contexts {
        let mut by_file: HashMap<FileId, Vec<(Span, SymbolId, Span)>> = HashMap::new();
        for (i, s) in analysis.index.symbols.iter().enumerate() {
            if s.span == Span::default() || s.span.file.expansion_index().is_some() || s.c.is_some() {
                continue;
            }
            let extent = analysis.extents.declaration(&analysis.sources, s.span).unwrap_or(s.span);
            by_file.entry(extent.file).or_default().push((extent, SymbolId(i as u32), s.span));
        }
        Contexts { by_file }
    }

    /// The path of the smallest declaration around `span`, other than one
    /// `span` is the name or the whole of.
    fn around(&self, analysis: &Analysis, span: Span) -> Option<String> {
        let holds = |outer: Span| outer.start <= span.start && span.end <= outer.end && outer != span;
        self.by_file
            .get(&span.file)?
            .iter()
            .filter(|(extent, _, name)| holds(*extent) && *name != span)
            .min_by_key(|(extent, _, _)| (extent.len(), std::cmp::Reverse(extent.start)))
            .map(|(_, id, _)| analysis.index.path_of(*id))
    }
}

/// Finds a file of the program by the name a position gives it: as
/// diagnostics show it, as it was read, or the end of its path.
pub fn find_file(sources: &SourceMap, name: &str) -> Option<FileId> {
    let wanted = Path::new(name.strip_prefix("./").unwrap_or(name));
    let files = sources.files();
    files
        .iter()
        .find(|f| f.display == name || f.path == wanted)
        .or_else(|| {
            files.iter().find(|f| f.path.ends_with(wanted) || (wanted.is_absolute() && wanted.ends_with(&f.path)))
        })
        .map(|f| f.id)
}

/// The files of a package, as diagnostics show them, sorted.
fn package_files(analysis: &Analysis, pkg: PackageId) -> Vec<String> {
    // `./a/b.wid` is `a/b.wid`, and `.` is the empty path.
    let plain = |p: &Path| -> std::path::PathBuf {
        p.components().filter(|c| !matches!(c, std::path::Component::CurDir)).collect()
    };
    let dir = plain(&analysis.index.package(pkg).dir);
    let mut names: Vec<String> = analysis
        .sources
        .files()
        .iter()
        .filter(|f| f.path.parent().is_some_and(|p| plain(p) == dir) || plain(&f.path) == dir)
        .map(|f| f.display.clone())
        .collect();
    names.sort();
    names
}

/// What is at a position (`file:line:column`) of a file of the program:
/// the innermost expression, binding, parameter, written type or
/// declaration name the checker recorded there, its type, and what the
/// name there refers to.
pub fn type_at(analysis: &Analysis, pkg: PackageId, position: &str) -> Result<TypeItem, Failure> {
    let fail = |e| Err(Failure::Position(e));
    let Some(pos) = Position::parse(position) else { return fail(PositionError::Malformed) };
    let Some(file) = find_file(&analysis.sources, &pos.file) else {
        return fail(PositionError::UnknownFile { candidates: package_files(analysis, pkg) });
    };
    let source = analysis.sources.file(file);
    // A last newline ends the last line rather than starting another.
    let lines = source.line_count() as u32 - u32::from(source.text.ends_with('\n'));
    if pos.line > lines {
        return fail(PositionError::NoLine { lines });
    }
    let last = source.line_text_by_index(pos.line as usize - 1).chars().count() as u32 + 1;
    if pos.column > last {
        return fail(PositionError::NoColumn { last });
    }
    let offset = source.offset_of(pos.line, pos.column).unwrap_or_default();
    let uses = &analysis.uses;
    let covers = |s: Span| s.file == file && s.start <= offset && offset < s.end;
    let within = |inner: Span, outer: Span| outer.start <= inner.start && inner.end <= outer.end;
    let typed = uses.types.iter().filter(|t| covers(t.span)).min_by_key(|t| (t.span.len(), t.span.start));
    let named = uses
        .refs
        .iter()
        .filter(|r| covers(r.span) && typed.is_none_or(|t| within(r.span, t.span)))
        .min_by_key(|r| (r.span.len(), preference(analysis, r)));
    let own = |r: &Ref| declared_at(analysis, &r.target).and_then(|at| uses.typed(at));
    // A name inside a larger expression has a type of its own, except a
    // method a call names and a generic type a written type starts with
    // (`Pool` in `Pool(Ball, 4)`), whose call or instance is meant.
    let heads = |t: &Typed, r: &Ref| {
        r.kind == RefKind::Call
            || (r.kind == RefKind::Type && t.kind == TypedKind::Type && t.span.start == r.span.start)
    };
    let (entry, at): (Option<&Typed>, Span) = match (typed, named) {
        (Some(t), Some(r)) if r.span != t.span && !heads(t, r) => (own(r), r.span),
        (Some(t), _) => (Some(t), t.span),
        (None, Some(r)) => (own(r), r.span),
        (None, None) => return fail(PositionError::Nothing { nearest: nearest(analysis, file, offset) }),
    };
    let kind = match named {
        Some(r) => match &r.target {
            RefTarget::Local { parameter: true, .. } => "parameter",
            RefTarget::Local { .. } => "local",
            RefTarget::Field { .. } => "field",
            RefTarget::Package(_) => "package",
            RefTarget::Builtin(_) => "type",
            _ => match r.kind {
                RefKind::Call => "call",
                RefKind::Type => "type",
                RefKind::Declaration => entry.map_or("declaration", |t| t.kind.as_str()),
                RefKind::Read | RefKind::Write | RefKind::Import => "expression",
            },
        },
        None => entry.map_or("expression", |t| t.kind.as_str()),
    };
    let items = analysis.items(true);
    let location_of = |span: Span| {
        items.location(span).unwrap_or_else(|| Location {
            file: source.display.clone(),
            line: pos.line,
            column: pos.column,
            end_line: pos.line,
            end_column: pos.column,
        })
    };
    let (ty, instances) = match (entry, named.map(|r| &r.target)) {
        (_, Some(RefTarget::Package(_))) | (None, _) => (None, Vec::new()),
        (Some(t), _) => (Some(t.ty.clone()), t.instances.clone()),
    };
    Ok(TypeItem {
        location: location_of(named.map_or(at, |r| r.span)),
        span: location_of(at),
        ty,
        instances,
        kind,
        refers_to: named.and_then(|r| refers_to(analysis, r)),
    })
}

/// Which of the uses at one span a position names: a declaration, then a
/// variable or field, a type, a package, and last a call, since an
/// operator method is recorded at its left operand or the whole operation.
/// A call of an overload set names the member it chose before the set.
fn preference(analysis: &Analysis, r: &Ref) -> (u8, bool) {
    let kind = match r.kind {
        RefKind::Declaration => 0,
        RefKind::Read | RefKind::Write => 1,
        RefKind::Type => 2,
        RefKind::Import => 3,
        RefKind::Call => 4,
    };
    let set = matches!(r.target, RefTarget::Symbol(id) if analysis.index.symbol(id).kind == SymbolKind::Overload);
    (kind, set)
}

/// Where what a use refers to is declared, as recorded.
fn declared_at(analysis: &Analysis, target: &RefTarget) -> Option<Span> {
    let index = &analysis.index;
    Some(match target {
        RefTarget::Symbol(id) => index.symbol(*id).span,
        RefTarget::Field { owner, index: i } => index.symbol(*owner).fields.get(*i)?.span,
        RefTarget::EnumMember { owner, index: i } => index.symbol(*owner).enum_members.get(*i)?.span,
        RefTarget::Local { binding, .. } => *binding,
        RefTarget::Package(_) | RefTarget::Builtin(_) => return None,
    })
}

/// The item of what a use refers to, as `def` gives it; for a local, an
/// item of its own (`local` or `parameter`).
fn refers_to(analysis: &Analysis, r: &Ref) -> Option<Item> {
    let items = analysis.items(true);
    let index = &analysis.index;
    Some(match &r.target {
        RefTarget::Symbol(id) => items.full_item(*id),
        RefTarget::Field { owner, index: i } => {
            (*i < index.symbol(*owner).fields.len()).then(|| items.field_item(*owner, *i, None))?
        }
        RefTarget::EnumMember { owner, index: i } => {
            (*i < index.symbol(*owner).enum_members.len()).then(|| items.member_item(*owner, *i))?
        }
        RefTarget::Package(p) => items.package_item(*p, analysis.sources.slice(r.span)),
        RefTarget::Builtin(name) => items.target_item(&Target::Builtin(name.clone()), name),
        RefTarget::Local { binding, parameter } => {
            // A parameter's binding is the parameter as written.
            let text = analysis.sources.slice(*binding);
            let name = text.split(':').next().unwrap_or(text).trim();
            let signature = match analysis.uses.typed(*binding) {
                _ if *parameter => text.to_string(),
                Some(t) => format!("{name}: {}", t.ty),
                None => name.to_string(),
            };
            let kind = if *parameter { "parameter" } else { "local" };
            Item {
                location: items.location(*binding),
                span: items.location(*binding),
                package: String::new(),
                ..Item::blank(kind, name, name, &signature)
            }
        }
    })
}

/// The recorded spans nearest to `offset` in `file`, at most three:
/// those on its line first, then by distance.
fn nearest(analysis: &Analysis, file: FileId, offset: u32) -> Vec<Nearby> {
    let source = analysis.sources.file(file);
    let (line, _) = source.line_col(offset);
    let spans = analysis.uses.types.iter().map(|t| t.span).chain(analysis.uses.refs.iter().map(|r| r.span));
    let mut found: Vec<(u32, u32, Span)> = spans
        .filter(|s| s.file == file && !s.is_empty())
        .map(|s| {
            let distance = if offset < s.start { s.start - offset } else { offset.saturating_sub(s.end) + 1 };
            let (at, _) = source.line_col(s.start);
            (at.abs_diff(line), distance, s)
        })
        .collect();
    found.sort_by_key(|(lines, distance, s)| (*lines, *distance, s.len(), s.start));
    let mut out: Vec<Nearby> = Vec::new();
    for (_, _, span) in found {
        if out.len() == 3 {
            break;
        }
        // One per place: the smallest span starting there.
        if out.iter().any(|n| n.span.start == span.start) {
            continue;
        }
        let (line, column) = source.line_col(span.start);
        let first = source.slice(span).lines().next().unwrap_or_default();
        let text = match first.char_indices().nth(40) {
            Some((cut, _)) => format!("{}…", &first[..cut]),
            None => first.to_string(),
        };
        out.push(Nearby { span, line, column, text });
    }
    out
}

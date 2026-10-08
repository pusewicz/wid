//! Completion and signature help.
//!
//! Both read the buffer as it is now ([`crate::syntax`]) and answer from
//! the package's latest check, then from its last check whose files all
//! parsed: completion must work mid-edit, where `ball.` alone doesn't
//! parse, without checking again on every keystroke. Those checks saw an
//! older text, so a position is carried over to it through the edit
//! between them ([`Snapshot`]): text before the first change and after the
//! last stays where it was.
//!
//! What a receiver reaches is what the checker recorded for it (its type,
//! or the package or type it names); where that is missing (the receiver
//! was just typed, or sits in code the checker didn't lower), it is
//! resolved by name: a local's recorded or written type, `self`, `@field`,
//! an import name, a type or constant, and fields and members after them.

use std::ops::Range;
use std::path::Path;

use wid_diagnostics::{FileId, Span};
use wid_query::Analysis;
use wid_query::complete::{self, Candidate, CandidateKind, Viewer};
use wid_sema::PackageId;
use wid_sema::index::{Origin, SymbolId, SymbolKind, Target};
use wid_sema::uses::{Members, RefKind, RefTarget, Shape, TypedKind};

use crate::syntax::{self, CallSite, Context, Local};

/// One check of the package, read at positions of the buffer as it is now.
pub(crate) struct Snapshot<'a> {
    pub(crate) analysis: &'a Analysis,
    /// The buffer's file in the check.
    pub(crate) file: FileId,
    /// The bytes the two texts share at their start and at their end.
    prefix: usize,
    suffix: usize,
    old_len: usize,
    new_len: usize,
}

impl<'a> Snapshot<'a> {
    /// The check `analysis` of the file at `path`, whose text is `text`
    /// now; `None` when the check didn't read the file.
    pub(crate) fn new(analysis: &'a Analysis, path: &Path, text: &str) -> Option<Snapshot<'a>> {
        let file = analysis.sources.find_by_path(path)?;
        let old = analysis.sources.file(file).text.as_bytes();
        let new = text.as_bytes();
        let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        let room = old.len().min(new.len()) - prefix;
        let suffix = old.iter().rev().zip(new.iter().rev()).take(room).take_while(|(a, b)| a == b).count();
        Some(Snapshot { analysis, file, prefix, suffix, old_len: old.len(), new_len: new.len() })
    }

    /// Where byte `offset` of the buffer was in the checked text, unless
    /// it is inside what changed.
    fn old(&self, offset: usize) -> Option<u32> {
        if offset <= self.prefix {
            Some(offset as u32)
        } else if offset >= self.new_len - self.suffix {
            Some((offset - self.new_len + self.old_len) as u32)
        } else {
            None
        }
    }

    /// Where byte `offset` was, or the start of the change it is in.
    fn old_or_change(&self, offset: usize) -> u32 {
        self.old(offset).unwrap_or(self.prefix as u32)
    }

    /// The span bytes `range` of the buffer were in the checked text.
    fn old_span(&self, range: &Range<usize>) -> Option<Span> {
        let (start, end) = (self.old(range.start)?, self.old(range.end)?);
        (start <= end && end as usize - start as usize == range.len()).then(|| Span::new(self.file, start, end))
    }

    fn root(&self) -> Option<PackageId> {
        self.analysis.root()
    }

    /// The type whose method holds byte `offset` (the type an `extend`
    /// extends), whose private methods and `@fields` are usable there.
    fn inside(&self, offset: usize) -> Option<SymbolId> {
        let index = &self.analysis.index;
        let at = wid_query::declaration_at(self.analysis, self.file, self.old_or_change(offset))?;
        let mut id = at;
        for _ in 0..4 {
            let s = index.symbol(id);
            match s.kind {
                SymbolKind::Struct | SymbolKind::Enum | SymbolKind::Union | SymbolKind::Module => return Some(id),
                SymbolKind::Extension => return s.links.iter().find_map(|l| l.symbol),
                _ => id = s.owner?,
            }
        }
        None
    }

    fn viewer(&self, offset: usize) -> Option<Viewer> {
        Some(Viewer { package: self.root()?, inside: self.inside(offset) })
    }

    /// The type recorded for a binding: at it, or for one whose recorded
    /// span is wider (`|x|`), the smallest binding around it.
    fn binding_type(&self, binding: Span) -> Option<&'a wid_sema::uses::Typed> {
        let uses = &self.analysis.uses;
        uses.typed(binding).or_else(|| {
            uses.types
                .iter()
                .filter(|t| matches!(t.kind, TypedKind::Local | TypedKind::Parameter))
                .filter(|t| t.span.file == binding.file && t.span.start <= binding.start && binding.end <= t.span.end)
                .min_by_key(|t| t.span.len())
        })
    }
}

/// What a receiver reaches.
#[derive(Clone, Debug)]
enum Reached {
    /// A value, whose type reaches these members.
    Value(Members),
    /// A type or module named as such: `Ball.`.
    Type(SymbolId),
    /// A package, by its import name: `geo.`.
    Package(PackageId),
}

/// The members of a declared type, as [`Members`] holds them.
fn members_of_decl(analysis: &Analysis, id: SymbolId) -> Members {
    let id = analysis.index.alias_target(id);
    let shape = match analysis.index.symbol(id).kind {
        SymbolKind::Struct => Shape::Struct,
        SymbolKind::Enum => Shape::Enum,
        _ => Shape::Other,
    };
    Members { decl: Some(id), shape, extensions: Vec::new() }
}

/// What a type written as text reaches: a declared type (`Ball`,
/// `geo.Ball`, `^Ball?`, `Pool(Ball, 4)`) or a builtin one (`Int`,
/// `String`, `[]Int`).
fn members_of_written(analysis: &Analysis, pkg: PackageId, written: &str) -> Option<Members> {
    let mut text = written.trim();
    loop {
        let before = text;
        text = text.trim_start_matches('^').trim_end_matches('?').trim();
        if let Some(inner) = text.strip_prefix('(').and_then(|t| t.strip_suffix(')')) {
            text = inner.trim();
        }
        if text == before {
            break;
        }
    }
    let index = &analysis.index;
    let extensions = |name: &str| -> Vec<SymbolId> {
        index
            .builtin_groups(name)
            .iter()
            .filter_map(|g| match g.origin {
                Origin::Extend { extension } => Some(extension),
                _ => None,
            })
            .collect()
    };
    let shape = if text.starts_with("[]") {
        Shape::Slice
    } else if text.starts_with("[dynamic]") {
        Shape::Dynamic
    } else if text.starts_with("map[") {
        Shape::Map
    } else if text.starts_with('[') {
        let len = text[1..].split(']').next().and_then(|n| n.trim().parse().ok()).unwrap_or(0);
        let elem = text.split(']').nth(1).unwrap_or_default();
        let numeric = matches!(elem, "Int" | "UInt" | "F32" | "F64") || elem.starts_with(['I', 'U']);
        Shape::Array { len, numeric }
    } else {
        let path = text.split('(').next().unwrap_or(text).trim();
        let segments: Vec<&str> = path.split('.').collect();
        match index.resolve(pkg, &segments, true).ok()? {
            Target::Symbol(id) if index.symbol(id).kind.has_members() => return Some(members_of_decl(analysis, id)),
            Target::Builtin(name) => {
                let shape = match name.as_str() {
                    "String" => Shape::String,
                    "Bool" => Shape::Bool,
                    "Rune" => Shape::Rune,
                    n if n.starts_with(['I', 'U', 'F']) => Shape::Number,
                    _ => Shape::Other,
                };
                return Some(Members { decl: None, shape, extensions: extensions(&name) });
            }
            _ => return None,
        }
    };
    let pattern = match shape {
        Shape::Slice | Shape::Dynamic | Shape::Array { .. } => {
            let elem = text.rsplit(']').next().unwrap_or_default();
            let mut list = extensions("[]$T");
            list.extend(extensions(&format!("[]{elem}")));
            list
        }
        _ => Vec::new(),
    };
    Some(Members { decl: None, shape, extensions: pattern })
}

/// What the receiver at bytes `receiver` of the buffer reaches, from what
/// a check recorded at it, or by its names.
fn reach(snap: &Snapshot<'_>, text: &str, receiver: &Range<usize>, locals: &[Local]) -> Option<Reached> {
    let analysis = snap.analysis;
    let uses = &analysis.uses;
    if let Some(span) = snap.old_span(receiver) {
        for r in uses.refs.iter().filter(|r| r.span == span) {
            match &r.target {
                RefTarget::Package(p) => return Some(Reached::Package(*p)),
                RefTarget::Symbol(id) if r.kind == RefKind::Type && analysis.index.symbol(*id).kind.has_members() => {
                    return Some(Reached::Type(*id));
                }
                _ => {}
            }
        }
        let typed = uses.typed(span).filter(|t| t.members.is_some()).or_else(|| {
            uses.types
                .iter()
                .filter(|t| t.span.file == span.file && t.span.end == span.end && t.span.start >= span.start)
                .filter(|t| t.members.is_some())
                .max_by_key(|t| t.span.len())
        });
        if let Some(members) = typed.and_then(|t| uses.members_of(t)) {
            return Some(Reached::Value(members.clone()));
        }
    }
    by_name(snap, &text[receiver.clone()], receiver.start, locals)
}

/// What a receiver written as names joined by `.` reaches, read name by
/// name: `self`, `@field`, a local, an import name, a type or constant,
/// then fields, members and package members.
fn by_name(snap: &Snapshot<'_>, written: &str, at: usize, locals: &[Local]) -> Option<Reached> {
    let analysis = snap.analysis;
    let index = &analysis.index;
    let pkg = snap.root()?;
    let mut segments = written.split('.').map(str::trim);
    let first = segments.next()?;
    let mut reached = if first == "self" {
        Reached::Value(members_of_decl(analysis, snap.inside(at)?))
    } else if let Some(field) = first.strip_prefix('@') {
        field_reach(snap, snap.inside(at)?, field)?
    } else if let Some(local) = locals.iter().find(|l| l.name == first) {
        let typed = snap.old_span(&(local.binding.start as usize..local.binding.end as usize));
        let members = typed.and_then(|b| snap.binding_type(b)).and_then(|t| analysis.uses.members_of(t)).cloned();
        match members {
            Some(m) => Reached::Value(m),
            None => Reached::Value(members_of_written(analysis, pkg, local.written.as_deref()?)?),
        }
    } else {
        match index.resolve(pkg, &[first], true).ok()? {
            Target::Package(p) => Reached::Package(p),
            Target::Symbol(id) => symbol_reach(snap, id)?,
            _ => return None,
        }
    };
    for segment in segments {
        reached = match reached {
            Reached::Package(p) => match index.package(p).scope.get(segment) {
                Some(&id) => symbol_reach(snap, id)?,
                None => return None,
            },
            Reached::Type(ty) => {
                let ty = index.alias_target(ty);
                let s = index.symbol(ty);
                if s.enum_members.iter().any(|m| m.name == segment) {
                    Reached::Value(members_of_decl(analysis, ty))
                } else {
                    let found = index.member_groups(ty).iter().flat_map(|g| g.members.clone()).find(|&m| {
                        let m = index.symbol(m);
                        m.name == segment && m.kind == SymbolKind::Constant
                    })?;
                    symbol_reach(snap, found)?
                }
            }
            Reached::Value(members) => field_reach(snap, members.decl?, segment)?,
        };
    }
    Some(reached)
}

/// What a declaration reaches as a receiver: a type its members, a
/// constant its value's.
fn symbol_reach(snap: &Snapshot<'_>, id: SymbolId) -> Option<Reached> {
    let analysis = snap.analysis;
    let s = analysis.index.symbol(id);
    if s.kind.has_members() {
        return Some(Reached::Type(id));
    }
    if s.kind == SymbolKind::Constant {
        let members = analysis.uses.typed(s.span).and_then(|t| analysis.uses.members_of(t));
        return members.cloned().map(Reached::Value);
    }
    None
}

/// What the field `name` of the struct `ty` holds, as its declaration was
/// recorded, or as its type is written.
fn field_reach(snap: &Snapshot<'_>, ty: SymbolId, name: &str) -> Option<Reached> {
    let analysis = snap.analysis;
    let index = &analysis.index;
    let f = index.fields_of(ty).into_iter().find(|f| index.symbol(f.owner).fields[f.index].name == name)?;
    let owner = index.symbol(f.owner);
    let field = &owner.fields[f.index];
    let recorded = analysis.uses.typed(field.span).and_then(|t| analysis.uses.members_of(t)).cloned();
    match recorded {
        Some(m) => Some(Reached::Value(m)),
        None => Some(Reached::Value(members_of_written(analysis, owner.package, &field.ty)?)),
    }
}

/// The candidates at byte `offset` of `text` (the buffer of the file at
/// `path`), from the snapshots in order: the first that knows what a
/// receiver reaches answers for it, and the names in scope come from all
/// of them. Locals come from the buffer itself.
pub(crate) fn candidates(snaps: &[Snapshot<'_>], text: &str, offset: usize) -> (Context, Vec<(Candidate, u8)>) {
    let context = syntax::context(text, offset);
    let (parsed, _) = wid_syntax::parse_file(FileId(0), text);
    let locals = syntax::locals_at(&parsed, text, offset);
    let mut out: Vec<(Candidate, u8)> = Vec::new();
    // A name comes once, from the first place that offers it.
    let push = |c: Candidate, group: u8, out: &mut Vec<(Candidate, u8)>| {
        if !out.iter().any(|(other, _)| other.label == c.label) {
            out.push((c, group));
        }
    };
    match &context {
        Context::Nothing => {}
        Context::Member { receiver, .. } => {
            for snap in snaps {
                let Some(viewer) = snap.viewer(offset) else { continue };
                let Some(reached) = reach(snap, text, receiver, &locals) else { continue };
                let list = match reached {
                    Reached::Value(members) => complete::value_members(snap.analysis, &members, viewer),
                    Reached::Type(ty) => complete::type_members(snap.analysis, ty, viewer),
                    Reached::Package(p) => complete::package_members(snap.analysis, p, viewer),
                };
                for c in list {
                    push(c, 0, &mut out);
                }
                break;
            }
        }
        Context::Ivar { .. } => {
            for snap in snaps {
                let Some(ty) = snap.inside(offset) else { continue };
                if snap.analysis.index.symbol(ty).kind == SymbolKind::Struct {
                    for c in complete::fields(snap.analysis, ty) {
                        push(c, 0, &mut out);
                    }
                    break;
                }
            }
        }
        Context::Name { .. } => {
            for local in locals.iter().rev() {
                let ty = snaps.iter().find_map(|snap| {
                    let binding = snap.old_span(&(local.binding.start as usize..local.binding.end as usize))?;
                    snap.binding_type(binding).map(|t| t.ty.clone())
                });
                let detail = match (&ty, &local.written) {
                    (Some(ty), _) | (None, Some(ty)) => format!("{}: {ty}", local.name),
                    (None, None) => local.name.clone(),
                };
                let what = if local.parameter { "parameter" } else { "local variable" };
                let c = Candidate {
                    label: local.name.clone(),
                    kind: CandidateKind::Variable,
                    detail,
                    doc: Some(format!("A {what}.")),
                    origin: None,
                };
                push(c, 0, &mut out);
            }
            for snap in snaps {
                let Some(viewer) = snap.viewer(offset) else { continue };
                if let Some(ty) = viewer.inside {
                    for c in complete::self_methods(snap.analysis, ty, viewer) {
                        push(c, 1, &mut out);
                    }
                }
                for c in complete::scope_names(snap.analysis, viewer.package) {
                    let group = match c.kind {
                        CandidateKind::Keyword => 4,
                        _ if c.origin.as_deref() == Some("builtin") => 3,
                        _ => 2,
                    };
                    push(c, group, &mut out);
                }
            }
        }
    }
    (context, out)
}

/// One signature of a call: its declaration line, the byte ranges of its
/// parameters in it, and its doc.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Signature {
    pub(crate) label: String,
    pub(crate) parameters: Vec<(Range<usize>, String)>,
    pub(crate) doc: Option<String>,
}

/// The parameters of a declaration line: what is between the parentheses
/// after the name, split at top-level commas, leaving out a block
/// parameter (`&blk: …`), which no argument list passes.
fn parameters(label: &str) -> Vec<(Range<usize>, String)> {
    let Some(open) = label.find('(') else { return Vec::new() };
    let bytes = label.as_bytes();
    let mut out = Vec::new();
    let mut depth = 0usize;
    let mut start = open + 1;
    for (i, &b) in bytes.iter().enumerate().skip(open) {
        match b {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    let piece = &label[start..i];
                    if !piece.trim().is_empty() {
                        let lead = piece.len() - piece.trim_start().len();
                        out.push(start + lead..start + piece.trim_end().len());
                    }
                    break;
                }
            }
            b',' if depth == 1 => {
                let piece = &label[start..i];
                let lead = piece.len() - piece.trim_start().len();
                out.push(start + lead..start + piece.trim_end().len());
                start = i + 1;
            }
            _ => {}
        }
    }
    out.into_iter()
        .filter(|r| !label[r.clone()].starts_with('&'))
        .map(|r| {
            let text = &label[r.clone()];
            let name = text.trim_start_matches('*').split(':').next().unwrap_or(text).trim().to_string();
            (r, name)
        })
        .collect()
}

fn signature_of(analysis: &Analysis, id: SymbolId) -> Signature {
    let s = analysis.index.symbol(id);
    let label = s.signature.clone();
    Signature {
        parameters: parameters(&label),
        label,
        doc: s.doc.as_deref().map(wid_syntax::docs::first_paragraph),
    }
}

/// The signatures of what a call names (one per member of an overload
/// set), the one to show first, and the parameter the cursor is at.
pub(crate) fn signatures(snaps: &[Snapshot<'_>], text: &str, offset: usize) -> Option<(Vec<Signature>, usize, usize)> {
    let call = syntax::call_at(text, offset)?;
    let (parsed, _) = wid_syntax::parse_file(FileId(0), text);
    let locals = syntax::locals_at(&parsed, text, offset);
    snaps.iter().find_map(|snap| signatures_in(snap, text, offset, &call, &locals))
}

fn signatures_in(
    snap: &Snapshot<'_>,
    text: &str,
    offset: usize,
    call: &CallSite,
    locals: &[Local],
) -> Option<(Vec<Signature>, usize, usize)> {
    let analysis = snap.analysis;
    let index = &analysis.index;
    let name = &text[call.callee.clone()];
    // What the checker resolved the callee to, when it saw this call.
    let recorded = snap.old_span(&call.callee).and_then(|span| {
        let mut found: Vec<SymbolId> = analysis
            .uses
            .refs
            .iter()
            .filter(|r| r.span == span && r.kind == RefKind::Call)
            .filter_map(|r| match r.target {
                RefTarget::Symbol(id) => Some(id),
                _ => None,
            })
            .collect();
        // An overload set before the member the call chose.
        found.sort_by_key(|&id| index.symbol(id).kind != SymbolKind::Overload);
        found.first().copied()
    });
    let callee = match recorded {
        Some(id) => Callee::Symbol(id),
        None => resolve_callee(snap, text, offset, call, locals, name)?,
    };
    let signatures = match callee {
        Callee::Symbol(id) => {
            let s = index.symbol(id);
            if s.kind == SymbolKind::Overload {
                s.links.iter().filter_map(|l| l.symbol).map(|m| signature_of(analysis, m)).collect()
            } else {
                vec![signature_of(analysis, id)]
            }
        }
        Callee::New(ty) => {
            let s = index.symbol(ty);
            let fields: Vec<String> = s.fields.iter().map(|f| f.declaration()).collect();
            let label = format!("{}.new({})", s.name, fields.join(", "));
            vec![Signature { parameters: parameters(&label), label, doc: None }]
        }
    };
    if signatures.is_empty() {
        return None;
    }
    let active = signatures
        .iter()
        .position(|s| match &call.named {
            Some(n) => s.parameters.iter().any(|(_, p)| p == n),
            None => s.parameters.len() > call.argument,
        })
        .unwrap_or(0);
    let parameter = match &call.named {
        Some(n) => signatures[active].parameters.iter().position(|(_, p)| p == n).unwrap_or(call.argument),
        None => call.argument,
    };
    Some((signatures, active, parameter))
}

/// What a call names, found by name.
enum Callee {
    Symbol(SymbolId),
    /// `T.new` of the struct `T`.
    New(SymbolId),
}

fn resolve_callee(
    snap: &Snapshot<'_>,
    text: &str,
    offset: usize,
    call: &CallSite,
    locals: &[Local],
    name: &str,
) -> Option<Callee> {
    let analysis = snap.analysis;
    let index = &analysis.index;
    let named = |groups: Vec<wid_sema::index::MemberGroup>| {
        groups.into_iter().flat_map(|g| g.members).find(|&m| index.symbol(m).name == name)
    };
    match &call.receiver {
        Some(receiver) => match reach(snap, text, receiver, locals)? {
            Reached::Value(members) => {
                let mut groups = members.decl.map(|d| index.member_groups(d)).unwrap_or_default();
                groups.extend(index.extension_groups(&members.extensions));
                named(groups).map(Callee::Symbol)
            }
            Reached::Type(ty) => {
                let ty = index.alias_target(ty);
                if name == "new" && index.symbol(ty).kind == SymbolKind::Struct {
                    return Some(Callee::New(ty));
                }
                named(index.member_groups(ty)).map(Callee::Symbol)
            }
            Reached::Package(p) => index.package(p).scope.get(name).copied().map(Callee::Symbol),
        },
        None => {
            if let Some(ty) = snap.inside(offset)
                && let Some(m) = named(index.member_groups(ty))
            {
                return Some(Callee::Symbol(m));
            }
            match index.resolve(snap.root()?, &[name], true).ok()? {
                Target::Symbol(id) => Some(Callee::Symbol(id)),
                _ => None,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::parameters;

    #[test]
    fn parameters_are_found_in_the_declaration_line() {
        let label = "def move(by: Int, to: Vec2 = [1.0, 2.0], &blk: block(Int)) -> Int";
        let found: Vec<(&str, &str)> =
            parameters(label).iter().map(|(r, name)| (&label[r.clone()], name.as_str())).collect();
        assert_eq!(found, [("by: Int", "by"), ("to: Vec2 = [1.0, 2.0]", "to")]);
        assert!(parameters("def size -> Int").is_empty());
        assert!(parameters("def none()").is_empty());
    }
}

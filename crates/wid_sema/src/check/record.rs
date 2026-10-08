//! Recording what each name refers to and what type each expression has,
//! as the checker resolves them ([`crate::uses`]).
//!
//! The checker calls the `note_*` methods where it resolves a name, a call,
//! a field, a written type or a binding, and [`Checker::expr`] notes every
//! expression's type. Each is a no-op unless the check indexes
//! ([`check_program_indexed`](super::check_program_indexed)), so `wid build`
//! and `wid check` pay one branch per call and allocate nothing.
//!
//! Code lowered more than once is recorded once per span. A span's type is
//! the last one the first lowering gave it (an untyped literal lowered again
//! with a parameter's type takes that type); other lowerings of it, like
//! the instances of a generic method, add their types to `instances`.
//! [`Checker::build_uses`] turns the recording into a [`Uses`] after
//! checking, adding every declaration's own name.

use std::collections::{HashMap, HashSet};

use wid_diagnostics::Span;
use wid_syntax::Name;
use wid_syntax::ast::{self, ExprKind as E, ItemKind};

use super::{Checker, ConstState, DeclId, DeclKind};
use crate::index::SymbolId;
use crate::input::PackageId;
use crate::ir;
use crate::types::{TyId, TyKind};
use crate::uses::{Ref, RefKind, RefTarget, Typed, TypedKind, Uses};

/// What a recorded name refers to, before it becomes a [`RefTarget`].
#[derive(Clone, Debug)]
enum Raw {
    Decl(DeclId),
    /// A field of the struct `DeclId`, by name.
    Field(DeclId, Name),
    /// A member of the enum `DeclId`, by name.
    Member(DeclId, Name),
    Package(PackageId),
    Builtin(&'static str),
    /// A local, by where it is declared.
    Local(Span),
}

/// The types a span was given, by lowering, and what it is.
struct Slot {
    kind: TypedKind,
    by_lowering: Vec<(u32, TyId)>,
}

/// One lowering of code: the function whose body is being lowered, its
/// generic bindings, and the generic instance being lowered inside it.
type LoweringKey = (Option<DeclId>, Vec<(Name, TyId)>, Option<String>);

/// What the checker resolved so far (see the module docs).
#[derive(Default)]
pub(crate) struct Recorder {
    refs: Vec<(Span, Raw, RefKind)>,
    /// Names assigned to: their reads are writes.
    writes: HashSet<Span>,
    types: HashMap<Span, Slot>,
    /// Bindings that are parameters.
    params: HashSet<Span>,
    lowerings: HashMap<LoweringKey, u32>,
}

/// How much a typed span's kind says: a binding or a field beats a call,
/// which beats a plain expression.
fn rank(kind: TypedKind) -> u8 {
    match kind {
        TypedKind::Expression => 0,
        TypedKind::Call => 1,
        _ => 2,
    }
}

impl Checker<'_> {
    /// Records that the name at `span` refers to the declaration `decl`.
    pub(crate) fn note_ref(&mut self, span: Span, decl: DeclId, kind: RefKind) {
        self.note(span, Raw::Decl(decl), kind);
    }

    fn note(&mut self, span: Span, raw: Raw, kind: RefKind) {
        if span == Span::default() {
            return;
        }
        if let Some(r) = self.recorder.as_deref_mut() {
            r.refs.push((span, raw, kind));
        }
    }

    /// Records a read of the constant `decl` written at `span`: for a
    /// qualified one (`geo.MAX`, `Ball.LIMIT`), at its last name.
    pub(crate) fn note_const(&mut self, span: Span, decl: DeclId) {
        if self.recorder.is_none() {
            return;
        }
        let text = self.source_text(span);
        let span = match text.rfind('.') {
            Some(dot) if text[dot + 1..].chars().all(|c| c.is_alphanumeric() || c == '_') => {
                Span::new(span.file, span.start + dot as u32 + 1, span.end)
            }
            _ => span,
        };
        self.note_ref(span, decl, RefKind::Read);
    }

    /// Records the type of the code at `span`.
    pub(crate) fn note_type(&mut self, span: Span, ty: TyId, kind: TypedKind) {
        if self.recorder.is_none() || span == Span::default() {
            return;
        }
        let frame = self.body.frames.first();
        let key: LoweringKey = (
            frame.and_then(|f| f.decl),
            frame.map(|f| f.subst.to_vec()).unwrap_or_default(),
            self.instance_stack.last().map(|i| i.0.clone()),
        );
        let Some(r) = self.recorder.as_deref_mut() else { return };
        let next = r.lowerings.len() as u32;
        let lowering = *r.lowerings.entry(key).or_insert(next);
        let slot = r.types.entry(span).or_insert(Slot { kind, by_lowering: Vec::new() });
        match rank(kind).cmp(&rank(slot.kind)) {
            std::cmp::Ordering::Less => {}
            std::cmp::Ordering::Greater => {
                slot.kind = kind;
                slot.by_lowering = vec![(lowering, ty)];
            }
            std::cmp::Ordering::Equal => match slot.by_lowering.iter_mut().find(|(l, _)| *l == lowering) {
                Some(entry) => entry.1 = ty,
                None => slot.by_lowering.push((lowering, ty)),
            },
        }
    }

    /// Records the type of an expression the checker lowered.
    pub(crate) fn note_expr(&mut self, e: &ast::Expr, v: &ir::Expr) {
        if self.recorder.is_none() {
            return;
        }
        let call = matches!(e.kind, E::Call(_))
            || matches!(v.kind, ir::ExprKind::Call { .. } | ir::ExprKind::CallIndirect { .. });
        self.note_type(e.span, v.ty, if call { TypedKind::Call } else { TypedKind::Expression });
    }

    /// Records a use of the field `name` of the struct type `ty` (the
    /// struct that declares it) at `span`, with the field's type.
    pub(crate) fn note_field(&mut self, span: Span, ty: TyId, name: Name, kind: RefKind) {
        if self.recorder.is_none() {
            return;
        }
        let Some(decl) = self.type_decl(ty) else { return };
        self.note(span, Raw::Field(decl, name), kind);
        if let Some((_, field_ty)) = self.field_index(ty, name) {
            self.note_type(span, field_ty, TypedKind::Field);
        }
    }

    /// Records a use of the member `name` of the enum type `ty` at `span`.
    pub(crate) fn note_member(&mut self, span: Span, ty: TyId, name: Name) {
        if self.recorder.is_none() {
            return;
        }
        if let Some(decl) = self.type_decl(ty) {
            self.note(span, Raw::Member(decl, name), RefKind::Read);
        }
    }

    /// Records the import name at `span`, which names `pkg`.
    pub(crate) fn note_package(&mut self, span: Span, pkg: PackageId) {
        self.note(span, Raw::Package(pkg), RefKind::Import);
    }

    /// Records the builtin type `name` written as a type at `span`.
    pub(crate) fn note_builtin(&mut self, span: Span, name: &'static str) {
        self.note(span, Raw::Builtin(name), RefKind::Type);
    }

    /// Records a read of the local declared at `binding`.
    pub(crate) fn note_local(&mut self, span: Span, binding: Span) {
        self.note(span, Raw::Local(binding), RefKind::Read);
    }

    /// Records a local declared at `span` with type `ty`.
    pub(crate) fn note_binding(&mut self, span: Span, ty: TyId) {
        if self.recorder.is_none() {
            return;
        }
        self.note(span, Raw::Local(span), RefKind::Declaration);
        self.note_type(span, ty, TypedKind::Local);
    }

    /// Records that the local declared at `span` is a parameter.
    pub(crate) fn note_param(&mut self, span: Span) {
        if let Some(r) = self.recorder.as_deref_mut() {
            r.params.insert(span);
        }
    }

    /// Records that an assignment writes `target`: a variable, `@field` or
    /// `value.field`, whose read there is a write.
    pub(crate) fn note_write(&mut self, target: &ast::Expr) {
        if self.recorder.is_none() {
            return;
        }
        let name = match &target.kind {
            E::Paren(inner) => return self.note_write(inner),
            E::Ident(name) => {
                // A plain assignment to a variable never lowers its name.
                if let Some(var) = self.find_var_at(*name, target.span) {
                    let (binding, ty) = (var.span, var.ty);
                    self.note(target.span, Raw::Local(binding), RefKind::Write);
                    self.note_type(target.span, ty, TypedKind::Expression);
                }
                target.span
            }
            E::IVar(_) => target.span,
            E::Member { name, .. } => name.span,
            _ => return,
        };
        if let Some(r) = self.recorder.as_deref_mut() {
            r.writes.insert(name);
        }
    }

    /// What the recording holds, as [`Uses`], with every declaration's own
    /// name: empty when the check didn't index.
    pub(super) fn build_uses(&self) -> Uses {
        let Some(r) = self.recorder.as_deref() else { return Uses::default() };
        // The names of declarations. A local "declared" there is one the
        // checker made up (the block parameters of a block method checked
        // on its own).
        let own: HashSet<Span> = self.decls.iter().map(|d| d.span).collect();
        let mut params = r.params.clone();
        for sig in self.sigs.values() {
            params.extend(sig.params.iter().map(|p| p.span));
        }
        let mut refs = Vec::with_capacity(r.refs.len() + self.decls.len());
        for (span, raw, kind) in &r.refs {
            let kind = if *kind == RefKind::Read && r.writes.contains(span) { RefKind::Write } else { *kind };
            let target = match raw {
                // Resolving a type names it at its own declaration too.
                Raw::Decl(d) if self.decls[d.0 as usize].span == *span => continue,
                Raw::Decl(d) => RefTarget::Symbol(SymbolId(d.0)),
                Raw::Field(d, name) => match self.field_position(*d, *name) {
                    Some(index) => RefTarget::Field { owner: SymbolId(d.0), index },
                    None => continue,
                },
                Raw::Member(d, name) => match self.member_position(*d, *name) {
                    Some(index) => RefTarget::EnumMember { owner: SymbolId(d.0), index },
                    None => continue,
                },
                Raw::Package(p) => RefTarget::Package(*p),
                Raw::Builtin(name) => RefTarget::Builtin(name.to_string()),
                Raw::Local(binding) if own.contains(binding) => continue,
                Raw::Local(binding) => RefTarget::Local { binding: *binding, parameter: params.contains(binding) },
            };
            refs.push(Ref { span: *span, target, kind });
        }
        let mut types = Vec::new();
        let mut typed: HashSet<Span> = HashSet::new();
        let shown = |slot: &Slot| {
            let mut all: Vec<String> = Vec::new();
            for (_, t) in &slot.by_lowering {
                let s = self.types.display(*t);
                if !all.contains(&s) {
                    all.push(s);
                }
            }
            let ty = all.first().cloned().unwrap_or_default();
            if all.len() < 2 {
                all.clear();
            }
            (ty, all)
        };
        for (i, d) in self.decls.iter().enumerate() {
            let id = SymbolId(i as u32);
            if d.span == Span::default() {
                continue;
            }
            if !matches!(d.kind, DeclKind::Extend(_)) {
                refs.push(Ref { span: d.span, target: RefTarget::Symbol(id), kind: RefKind::Declaration });
                if let Some(ty) = self.declared_type(DeclId(i as u32)) {
                    let kind = TypedKind::Declaration;
                    types.push(Typed { span: d.span, ty, instances: Vec::new(), kind });
                    typed.insert(d.span);
                }
            }
            match d.kind {
                DeclKind::Struct(s) => {
                    for (index, f) in struct_fields(s).enumerate() {
                        let target = RefTarget::Field { owner: id, index };
                        refs.push(Ref { span: f.name.span, target, kind: RefKind::Declaration });
                        if let Some(slot) = r.types.get(&f.ty.span)
                            && typed.insert(f.name.span)
                        {
                            let (ty, instances) = shown(slot);
                            types.push(Typed { span: f.name.span, ty, instances, kind: TypedKind::Field });
                        }
                    }
                }
                DeclKind::Enum(e) => {
                    let ty = self.decl_types.get(&DeclId(i as u32)).map(|t| self.types.display(*t));
                    for (index, m) in e.members.iter().enumerate() {
                        let target = RefTarget::EnumMember { owner: id, index };
                        refs.push(Ref { span: m.name.span, target, kind: RefKind::Declaration });
                        if let Some(ty) = &ty
                            && typed.insert(m.name.span)
                        {
                            let kind = TypedKind::Declaration;
                            types.push(Typed { span: m.name.span, ty: ty.clone(), instances: Vec::new(), kind });
                        }
                    }
                }
                _ => {}
            }
        }
        for imports in self.file_imports.values() {
            for (pkg, span) in imports.values() {
                refs.push(Ref { span: *span, target: RefTarget::Package(*pkg), kind: RefKind::Declaration });
            }
        }
        refs.sort();
        refs.dedup();
        for (span, slot) in &r.types {
            let made_up = own.contains(span) && slot.kind == TypedKind::Local;
            if made_up || !typed.insert(*span) {
                continue;
            }
            let kind = match slot.kind {
                TypedKind::Local if params.contains(span) => TypedKind::Parameter,
                kind => kind,
            };
            let (ty, instances) = shown(slot);
            types.push(Typed { span: *span, ty, instances, kind });
        }
        // Parameters of methods no lowering declared, like a block
        // method's, which is inlined where it is called.
        for sig in self.sigs.values() {
            for p in &sig.params {
                if p.span != Span::default() && typed.insert(p.span) {
                    let ty = self.types.display(p.ty);
                    types.push(Typed { span: p.span, ty, instances: Vec::new(), kind: TypedKind::Parameter });
                }
            }
        }
        types.sort_by_key(|t| t.span);
        Uses { refs, types }
    }

    /// The type a declaration's name has: a method's proc type, a
    /// constant's type, or the type a type declaration declares.
    fn declared_type(&self, decl: DeclId) -> Option<String> {
        let d = &self.decls[decl.0 as usize];
        match d.kind {
            DeclKind::Fn(f) if !f.is_macro => {
                let sig = self.sigs.get(&decl)?;
                let params: Vec<String> = sig.params.iter().map(|p| self.types.display(p.ty)).collect();
                let ret = match self.types.kind(sig.ret) {
                    TyKind::Void => String::new(),
                    _ => format!(" -> {}", self.types.display(sig.ret)),
                };
                Some(format!("proc({}){ret}", params.join(", ")))
            }
            DeclKind::Const(_) => match (self.decl_types.get(&decl), self.consts.get(&decl)) {
                (Some(t), _) => Some(self.types.display(*t)),
                (None, Some(ConstState::Done { typed, .. })) => Some(self.types.display(typed.ty)),
                _ => None,
            },
            DeclKind::Struct(s) if !s.generics.is_empty() => Some(generic_shape(d.name, &s.generics)),
            DeclKind::Union(u) if !u.generics.is_empty() => Some(generic_shape(d.name, &u.generics)),
            DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Union(_) => {
                self.decl_types.get(&decl).map(|t| self.types.display(*t))
            }
            _ => None,
        }
    }

    /// The position of the field `name` among the fields written in the
    /// struct `decl`, as the index lists them.
    fn field_position(&self, decl: DeclId, name: Name) -> Option<usize> {
        match self.decls[decl.0 as usize].kind {
            DeclKind::Struct(s) => struct_fields(s).position(|f| f.name.name == name),
            _ => None,
        }
    }

    /// The position of the member `name` of the enum `decl`.
    fn member_position(&self, decl: DeclId, name: Name) -> Option<usize> {
        match self.decls[decl.0 as usize].kind {
            DeclKind::Enum(e) => e.members.iter().position(|m| m.name.name == name),
            _ => None,
        }
    }
}

/// The fields written in a struct's body, in order.
fn struct_fields(s: &ast::StructDecl) -> impl Iterator<Item = &ast::FieldDecl> {
    s.body.iter().filter_map(|item| match &item.kind {
        ItemKind::Field(f) => Some(&**f),
        _ => None,
    })
}

/// A generic type's name with its parameters: `Pool(T, N)`.
fn generic_shape(name: Name, generics: &[ast::GenericParam]) -> String {
    let names: Vec<&str> = generics.iter().map(|g| g.name.as_str()).collect();
    format!("{name}({})", names.join(", "))
}

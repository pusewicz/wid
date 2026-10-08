//! Struct and enum types: creation, field resolution, layout and construction.

use wid_diagnostics::{Applicability, Diagnostic, Span, codes, did_you_mean};
use wid_syntax::Name;
use wid_syntax::ast::{self, ItemKind};

use super::body::Frame;
use super::items::ConstValue;
use super::ty::TyCtx;
use super::{Checker, DeclId, DeclKind, mangle_ident};
use crate::ir::{self, ExprKind};
use crate::types::{EnumInfo, FieldInfo, IntTy, MAX_TYPE_SIZE, StructInfo, TyId, TyKind, aggregate_wide, offsets};

impl<'a> Checker<'a> {
    /// Returns the type of a struct declaration, resolving its fields and
    /// layout the first time.
    pub fn struct_type(&mut self, decl: DeclId) -> TyId {
        if let Some(&t) = self.decl_types.get(&decl) {
            if self.shallow == 0
                && let Some(i) = self.pending_types.iter().position(|d| *d == decl)
            {
                self.pending_types.remove(i);
                self.fill_struct(decl, t);
            }
            return t;
        }
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Struct(s) = d.kind else { unreachable!("struct_type on a non-struct") };
        if !s.generics.is_empty() {
            let params: Vec<TyId> = s.generics.iter().map(|g| self.types.intern(TyKind::Param(g.name.name))).collect();
            let ty = self.struct_instance(decl, params, d.span);
            self.decl_types.insert(decl, ty);
            return ty;
        }
        let prefix = self.pkg_prefix(d.loc.pkg);
        let name = d.name.as_str().to_string();
        let extern_name = d.item.attr("extern").map(|a| {
            a.args.first().and_then(super::attrs::string_literal).map_or_else(|| name.clone(), str::to_string)
        });
        let ty = self.types.new_struct(StructInfo {
            name: name.clone(),
            c_name: extern_name.clone().unwrap_or_else(|| format!("{prefix}__{}", mangle_ident(&name))),
            fields: Vec::new(),
            size: 0,
            align: 1,
            complete: false,
            foreign: extern_name.is_some(),
            opaque: extern_name.is_some() && d.item.has_attr("opaque"),
            span: d.span,
        });
        self.decl_types.insert(decl, ty);
        let TyKind::Struct(sid) = *self.types.kind(ty) else { unreachable!() };
        self.struct_decls.insert(sid, decl);
        if self.shallow > 0 {
            self.pending_types.push(decl);
            return ty;
        }
        self.fill_struct(decl, ty);
        ty
    }

    /// Resolves a struct's fields and computes its layout.
    fn fill_struct(&mut self, decl: DeclId, ty: TyId) {
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Struct(s) = d.kind else { unreachable!("fill_struct on a non-struct") };
        let name = d.name.as_str().to_string();
        let TyKind::Struct(sid) = *self.types.kind(ty) else { unreachable!() };
        self.resolving.push(decl);

        let ctx = TyCtx { loc: d.loc, self_ty: Some(ty), subst: Default::default() };
        let mut fields: Vec<FieldInfo> = Vec::new();
        for item in &s.body {
            let ItemKind::Field(f) = &item.kind else { continue };
            let fty = self.resolve_type(&f.ty, &ctx);
            self.check_not_opaque(fty, f.ty.span);
            if let Some(prev) = fields.iter().find(|x| x.name == f.name.name) {
                self.report(
                    Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("field `{}` is declared twice", f.name.as_str()),
                    )
                    .primary(f.name.span, "second declaration")
                    .secondary(prev.span, "first declaration"),
                );
                continue;
            }
            if let Some(&m) = self.members.get(&decl).and_then(|m| m.get(&f.name.name)) {
                let mspan = self.decls[m.0 as usize].span;
                self.report_field_clash(f.name, mspan);
            }
            if f.using
                && !matches!(self.types.kind(fty), TyKind::Struct(_) | TyKind::Unknown)
                && !matches!(self.types.kind(fty), TyKind::Pointer(p) if matches!(self.types.kind(*p), TyKind::Struct(_)))
            {
                let shown = self.types.display(fty);
                self.report(
                    Diagnostic::error(
                        codes::TYPE_MISMATCH,
                        format!("`using` needs a struct or a pointer to one, not `{shown}`"),
                    )
                    .primary(f.ty.span, "members of this type cannot be promoted"),
                );
            }
            let c_conv = self.field_c_conv(d.loc.pkg, &name, f.name.as_str());
            let c_name = item
                .attr("extern")
                .and_then(|a| a.args.first())
                .and_then(super::attrs::string_literal)
                .map(str::to_string);
            if let Some(attr) = item.attr("extern")
                && !d.item.has_attr("extern")
            {
                self.report(
                    Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, "`@[extern]` on a field needs an `@[extern]` struct")
                        .primary(attr.span, "only C structs have fields with C names")
                        .help("remove `@[extern]` from the field"),
                );
            }
            fields.push(FieldInfo {
                name: f.name.name,
                ty: fty,
                offset: 0,
                using: f.using,
                span: f.name.span,
                c_conv,
                c_name,
            });
        }
        self.resolving.pop();

        for f in &mut fields {
            if let Some(culprit) = self.by_value_incomplete(f.ty) {
                let culprit_name = self.types.display(culprit);
                let field_ty = self.types.display(f.ty);
                self.report(
                    Diagnostic::error(codes::RECURSIVE_TYPE, format!("`{name}` contains itself"))
                        .primary(f.span, format!("this field's `{field_ty}` is stored by value"))
                        .note(format!(
                            "`{culprit_name}` would need to contain a full copy of itself, so it has no finite size"
                        ))
                        .suggest(
                            "store a pointer instead",
                            vec![wid_diagnostics::Edit {
                                span: self.field_type_span(s, f.name),
                                replacement: format!("^{field_ty}"),
                            }],
                            Applicability::MaybeIncorrect,
                        ),
                );
                f.ty = self.types.unknown();
            }
        }
        let spans: Vec<Span> = fields
            .iter()
            .map(|f| Some(self.field_type_span(s, f.name)).filter(|t| *t != Span::default()).unwrap_or(f.span))
            .collect();
        self.check_fields_size(&name, &mut fields, &spans);
        let parts: Vec<(u64, u64)> = fields.iter().map(|f| self.types.layout(f.ty)).collect();
        let offs = offsets(&parts);
        for (f, off) in fields.iter_mut().zip(offs) {
            f.offset = off;
        }
        let (mut size, mut align) = crate::types::aggregate(&parts);
        if let (Some(s), Some(a)) = (layout_attr(d.item, "size"), layout_attr(d.item, "align")) {
            (size, align) = (s, a);
        }
        let info = &mut self.types.structs[sid.0 as usize];
        info.fields = fields;
        info.size = size;
        info.align = align;
        info.complete = true;
        self.fill_pending_types();
    }

    /// Reports a member that has the name of one of its struct's fields.
    fn report_field_clash(&mut self, field: ast::Ident, member_span: Span) {
        self.report(
            Diagnostic::error(
                codes::DUPLICATE_DEFINITION,
                format!("`{}` is both a field and a method", field.as_str()),
            )
            .primary(member_span, "method declared here")
            .secondary(field.span, "field declared here")
            .help("rename one of them; `@name` reads the field and `name` calls the method"),
        );
    }

    /// Checks a member collected after its struct's fields were resolved,
    /// which `fill_struct` no longer sees: one that a `comptime if` or a
    /// macro adds once the struct's layout is needed.
    pub(super) fn check_member_after_fields(&mut self, owner: DeclId, name: Name, span: Span) {
        let DeclKind::Struct(s) = self.decls[owner.0 as usize].kind else { return };
        let Some(&ty) = self.decl_types.get(&owner) else { return };
        let filled = matches!(*self.types.kind(ty), TyKind::Struct(sid) if self.types.struct_info(sid).complete);
        if !filled || !s.generics.is_empty() {
            return;
        }
        let field = s.body.iter().find_map(|i| match &i.kind {
            ItemKind::Field(f) if f.name.name == name => Some(f.name),
            _ => None,
        });
        if let Some(field) = field {
            self.report_field_clash(field, span);
        }
    }

    /// Resolves the structs and unions that were only named behind
    /// indirections, once no declaration is mid-resolution.
    pub fn fill_pending_types(&mut self) {
        while self.resolving.is_empty() && self.shallow == 0 && !self.pending_types.is_empty() {
            let decl = self.pending_types.remove(0);
            let Some(&ty) = self.decl_types.get(&decl) else { continue };
            match self.decls[decl.0 as usize].kind {
                DeclKind::Struct(_) => self.fill_struct(decl, ty),
                DeclKind::Union(_) => self.fill_union(decl, ty, Default::default()),
                _ => {}
            }
        }
        self.check_pending_sizes();
    }

    fn field_type_span(&self, s: &ast::StructDecl, name: Name) -> Span {
        s.body
            .iter()
            .find_map(|i| match &i.kind {
                ItemKind::Field(f) if f.name.name == name => Some(f.ty.span),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Returns a struct that `ty` stores by value but whose layout is not
    /// known yet, which means the types contain each other.
    pub fn by_value_incomplete(&self, ty: TyId) -> Option<TyId> {
        match self.types.kind(ty) {
            TyKind::Struct(id) => (!self.types.struct_info(*id).complete).then_some(ty),
            TyKind::Union(id) => (!self.types.union_info(*id).complete).then_some(ty),
            TyKind::Array(inner, _) | TyKind::Matrix(inner, _, _) => self.by_value_incomplete(*inner),
            TyKind::Optional(inner) if !self.types.optional_is_pointer(ty) => self.by_value_incomplete(*inner),
            TyKind::Tuple(elems) => elems.iter().find_map(|e| self.by_value_incomplete(*e)),
            _ => None,
        }
    }

    /// Returns the type of an enum declaration.
    pub fn enum_type(&mut self, decl: DeclId) -> TyId {
        if let Some(&t) = self.decl_types.get(&decl) {
            return t;
        }
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Enum(e) = d.kind else { unreachable!("enum_type on a non-enum") };
        let ctx = TyCtx { loc: d.loc, self_ty: None, subst: Default::default() };
        let backing = match &e.backing {
            Some(t) => {
                let bt = self.resolve_type(t, &ctx);
                match self.types.kind(bt) {
                    TyKind::Int(i) => *i,
                    TyKind::Unknown => IntTy::Int,
                    _ => {
                        let shown = self.types.display(bt);
                        self.report(
                            Diagnostic::error(
                                codes::TYPE_MISMATCH,
                                format!("enums are backed by an integer type, not `{shown}`"),
                            )
                            .primary(t.span, "use `U8`, `I32`, `Int` or another integer type"),
                        );
                        IntTy::Int
                    }
                }
            }
            None => IntTy::Int,
        };
        let (lo, hi) = backing.range();
        let mut members: Vec<(Name, i128)> = Vec::new();
        let mut next = 0i128;
        // Whether the member before fits; one that follows it without a
        // value isn't reported again.
        let mut prev_fits = true;
        for m in &e.members {
            let errors = self.diags.error_count();
            let value = match &m.value {
                Some(v) => match self.eval_const(v, d.loc) {
                    Some(ConstValue::Int(i)) => i,
                    // Evaluating it reported why, like a call without `comptime`.
                    None if self.diags.error_count() > errors => next,
                    _ => {
                        self.report(
                            Diagnostic::error(codes::COMPTIME_ONLY, "enum values must be constant integers")
                                .primary(v.span, "not a constant integer"),
                        );
                        next
                    }
                },
                None => next,
            };
            let fits = lo <= value && value <= hi;
            if !fits && (prev_fits || m.value.is_some()) {
                self.report(
                    Diagnostic::error(
                        codes::CONSTANT_OVERFLOW,
                        format!("`{value}` does not fit in the enum's backing type `{}`", backing.name()),
                    )
                    .primary(m.name.span, format!("`{}` holds values from {lo} to {hi}", backing.name())),
                );
            }
            if members.iter().any(|(n, _)| *n == m.name.name) {
                self.report(
                    Diagnostic::error(
                        codes::DUPLICATE_DEFINITION,
                        format!("enum member `{}` is declared twice", m.name.as_str()),
                    )
                    .primary(m.name.span, "second declaration"),
                );
                continue;
            }
            if m.value.is_none() {
                self.member_names_macro(decl, m, value);
            }
            members.push((m.name.name, value));
            prev_fits = fits;
            // A value past `Int`'s range was reported, so the next one may
            // stop at the limit of the 128 bits constants are computed in.
            next = value.saturating_add(1);
        }
        if members.is_empty() {
            self.report(
                Diagnostic::error(codes::UNEXPECTED_TOKEN, format!("enum `{}` has no members", d.name))
                    .primary(d.span, "list the members on their own lines inside the enum"),
            );
        }
        let prefix = self.pkg_prefix(d.loc.pkg);
        let ty = self.types.new_enum(EnumInfo {
            name: d.name.as_str().to_string(),
            c_name: format!("{prefix}__{}", mangle_ident(d.name.as_str())),
            backing,
            members,
            foreign: false,
            span: d.span,
        });
        if let TyKind::Enum(id) = *self.types.kind(ty) {
            self.enum_decls.insert(id, decl);
        }
        self.decl_types.insert(decl, ty);
        ty
    }

    /// Reports a member written as a name alone that also names a macro
    /// visible in the enum's body (E0914): the line was likely meant to call
    /// the macro, which needs `()` there. The member is kept, and the enum's
    /// missing methods aren't reported, as after a failed macro call.
    fn member_names_macro(&mut self, decl: DeclId, member: &ast::EnumMember, value: i128) {
        let d = &self.decls[decl.0 as usize];
        let (enum_name, enum_loc) = (d.name, d.loc);
        // Where the name resolves: for an enum a macro generated, the
        // macro's file.
        let loc = self.virtual_file(member.name.span.file).map_or(enum_loc, |v| v.loc);
        let name = member.name.name;
        let Some(found) = self.lookup_pkg(loc.pkg, name).or_else(|| self.lookup_prelude(name)) else { return };
        let m = &self.decls[found.0 as usize];
        if !self.is_macro(found) || (m.private && m.loc.pkg != loc.pkg) {
            return;
        }
        let macro_span = m.span;
        let text = name.as_str();
        self.report(
            Diagnostic::error(
                codes::ENUM_MEMBER_MACRO,
                format!("`{text}` names a macro, but alone on a line in an enum it declares a member"),
            )
            .primary(member.name.span, format!("this declares the member `:{text}` of `{enum_name}`"))
            .secondary(macro_span, format!("`{text}` is a macro"))
            .note("in an `enum` body a name alone on a line is a member, so a macro without arguments is called with `()` there")
            .suggest(
                "call the macro",
                vec![wid_diagnostics::Edit { span: member.name.span.shrink_to_end(), replacement: "()".into() }],
                Applicability::MachineApplicable,
            )
            .help(format!("to keep the member, write its value: `{text} = {value}`")),
        );
        self.macros.failed_owners.insert(decl);
    }

    /// Returns the value of enum member `name`, reporting unknown members.
    pub fn enum_member(&mut self, ty: TyId, name: Name, span: Span) -> ir::Expr {
        let TyKind::Enum(id) = *self.types.kind(ty) else { unreachable!("enum_member on a non-enum") };
        let info = self.types.enum_info(id).clone();
        if let Some((_, v)) = info.members.iter().find(|(n, _)| *n == name) {
            self.note_member(span, ty, name);
            return ir::Expr::new(ExprKind::Int(*v), ty);
        }
        let names: Vec<&'static str> = info.members.iter().map(|(n, _)| n.as_str()).collect();
        let mut diag = Diagnostic::error(codes::NO_SUCH_MEMBER, format!("`{}` has no member `{name}`", info.name))
            .primary(span, format!("not a member of `{}`", info.name));
        // An enum without members was reported where it is declared.
        if !names.is_empty() {
            diag =
                diag.note(format!("members: {}", names.iter().map(|n| format!(":{n}")).collect::<Vec<_>>().join(", ")));
        }
        if let Some(best) = did_you_mean(name.as_str(), names.iter().copied()) {
            let text = self.source_text(span);
            let replacement = if text.starts_with(':') { format!(":{best}") } else { best.to_string() };
            diag = diag.suggest_replace(
                format!("did you mean `{best}`?"),
                span,
                replacement,
                Applicability::MaybeIncorrect,
            );
        }
        self.report(diag);
        ir::Expr::new(ExprKind::Zero, ty)
    }

    /// Finds a field by name in a struct type.
    pub fn field_index(&self, ty: TyId, name: Name) -> Option<(u32, TyId)> {
        let TyKind::Struct(id) = self.types.kind(ty) else { return None };
        self.types
            .struct_info(*id)
            .fields
            .iter()
            .position(|f| f.name == name)
            .map(|i| (i as u32, self.types.struct_info(*id).fields[i].ty))
    }

    /// Reports `T.new(*xs)` (E0302): Wid has no argument spreading, as in
    /// a method call. The collection is checked on its own and fills no
    /// field. For an array, slice or dynamic array, the fix (to review)
    /// passes its elements one by one to the fields left after the values
    /// before it (`positional`), when there are a few.
    fn spread_into_new(&mut self, info: &StructInfo, value: &ast::Expr, positional: usize) {
        let v = self.expr(value, None);
        let text = self.source_text(value.span);
        let star = Span { start: value.span.start.saturating_sub(1), ..value.span };
        let diag = Diagnostic::error(codes::ARG_COUNT, "Wid has no argument spreading")
            .primary(value.span, "`*` cannot spread a collection into arguments")
            .note(format!("`{}.new` takes a value for each field, by position or by name", info.name));
        let len = match self.types.kind(self.types.base(v.ty)) {
            TyKind::Array(_, n) => Some(*n as usize),
            TyKind::Slice(_) | TyKind::Dynamic(_) => Some(usize::MAX),
            _ => None,
        };
        let left = info.fields.len().saturating_sub(positional);
        let count = len.map_or(0, |n| n.min(left));
        let diag = if (1..=4).contains(&count)
            && super::items::is_simple_operand(&text)
            && self.source_text(star).starts_with('*')
        {
            let elems: Vec<String> = (0..count).map(|i| format!("{text}[{i}]")).collect();
            diag.suggest_replace("pass the elements one by one", star, elems.join(", "), Applicability::MaybeIncorrect)
        } else {
            diag.help(format!("pass the elements of `{text}` one by one, or name the fields they are for"))
        };
        self.report(diag);
    }

    /// Lowers `T.new(…)` for a struct: every field from an argument, its
    /// declared default, or zero.
    pub fn struct_new(&mut self, ty: TyId, args: &[ast::Arg], span: Span) -> ir::Expr {
        let TyKind::Struct(sid) = *self.types.kind(ty) else { unreachable!("struct_new on a non-struct") };
        let info = self.types.struct_info(sid).clone();
        let mut slots: Vec<Option<&ast::Expr>> = vec![None; info.fields.len()];
        let mut positional = 0usize;
        for arg in args {
            if arg.splat {
                self.spread_into_new(&info, &arg.value, positional);
                continue;
            }
            let idx = match arg.name {
                None => {
                    if positional >= info.fields.len() {
                        self.report(
                            Diagnostic::error(
                                codes::ARG_COUNT,
                                format!(
                                    "`{}` has {} field{}, but more values were given",
                                    info.name,
                                    info.fields.len(),
                                    if info.fields.len() == 1 { "" } else { "s" }
                                ),
                            )
                            .primary(arg.value.span, "no field left for this value"),
                        );
                        continue;
                    }
                    positional += 1;
                    positional - 1
                }
                Some(n) => match info.fields.iter().position(|f| f.name == n.name) {
                    Some(i) => i,
                    // A field a macro generated, rejected with E0913.
                    None if self.field_rejected(ty, n.name) => {
                        self.expr(&arg.value, None);
                        continue;
                    }
                    None => {
                        let names: Vec<&'static str> = info.fields.iter().map(|f| f.name.as_str()).collect();
                        let mut diag = Diagnostic::error(
                            codes::BAD_NAMED_ARG,
                            format!("`{}` has no field `{}`", info.name, n.as_str()),
                        )
                        .primary(n.span, "unknown field")
                        .note(format!("fields: {}", names.join(", ")));
                        let unset: Vec<&'static str> =
                            names.iter().enumerate().filter(|(i, _)| slots[*i].is_none()).map(|(_, n)| *n).collect();
                        if let Some(best) = did_you_mean(n.as_str(), unset.iter().copied())
                            .or_else(|| (unset.len() == 1).then(|| unset[0]))
                        {
                            diag = diag.suggest_replace(
                                format!("did you mean `{best}`?"),
                                n.span,
                                best,
                                Applicability::MaybeIncorrect,
                            );
                        }
                        self.report(diag);
                        self.expr(&arg.value, None);
                        continue;
                    }
                },
            };
            if slots[idx].is_some() {
                self.report(
                    Diagnostic::error(
                        codes::BAD_NAMED_ARG,
                        format!("field `{}` is given twice", info.fields[idx].name),
                    )
                    .primary(arg.value.span, "second value"),
                );
            }
            if let Some(n) = arg.name {
                self.note_field(n.span, ty, n.name, crate::uses::RefKind::Write);
            }
            slots[idx] = Some(&arg.value);
        }
        let decl = self.struct_decls.get(&sid).copied();
        let mut values: Vec<ir::Expr> = Vec::with_capacity(info.fields.len());
        for (i, field) in info.fields.iter().enumerate() {
            let v = match slots[i] {
                Some(e) => {
                    self.begin_block();
                    let v = self.expr_coerced(e, field.ty);
                    let stmts = self.end_block().stmts;
                    if !stmts.is_empty() || !v.is_pure() {
                        self.spill_impure(&mut values);
                    }
                    for s in stmts {
                        self.emit(s);
                    }
                    v
                }
                None => match decl.and_then(|d| self.field_default(d, field.name)) {
                    Some((default, loc)) => {
                        self.begin_block();
                        let v = self.lower_in_package(default, field.ty, loc);
                        let stmts = self.end_block().stmts;
                        if !stmts.is_empty() || !v.is_pure() {
                            self.spill_impure(&mut values);
                        }
                        for s in stmts {
                            self.emit(s);
                        }
                        v
                    }
                    None => ir::Expr::new(ExprKind::Zero, field.ty),
                },
            };
            values.push(v);
        }
        let _ = span;
        ir::Expr::new(ExprKind::Aggregate(values), ty)
    }

    /// Spills every impure expression in `values` to a temporary, keeping
    /// left-to-right evaluation when a later value has effects.
    pub fn spill_impure(&mut self, values: &mut [ir::Expr]) {
        for v in values.iter_mut() {
            if !v.is_pure() {
                let ty = v.ty;
                let taken = std::mem::replace(v, ir::Expr::new(ExprKind::Zero, ty));
                *v = self.spill(taken);
            }
        }
    }

    /// Checks the field defaults of a struct that isn't generic against
    /// their fields' types, once, so a mistake in one is reported whether or
    /// not a `new` takes it. The code is discarded: each `new` that takes a
    /// default lowers it where it is called (see [`Self::struct_new`]),
    /// which reports nothing new.
    pub(super) fn check_field_defaults(&mut self, decl: DeclId) {
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Struct(s) = d.kind else { return };
        if !s.generics.is_empty()
            || !s.body.iter().any(|i| matches!(&i.kind, ItemKind::Field(f) if f.default.is_some()))
        {
            return;
        }
        let ty = self.struct_type(decl);
        let TyKind::Struct(sid) = *self.types.kind(ty) else { return };
        let fields: Vec<(Name, TyId)> = self.types.struct_info(sid).fields.iter().map(|f| (f.name, f.ty)).collect();
        let saved = std::mem::take(&mut self.body);
        let void = self.types.void();
        self.body.frames.push(Frame {
            loc: d.loc,
            scopes: Vec::new(),
            ret: void,
            self_ty: None,
            self_local: None,
            fn_name: d.name.as_str().to_string(),
            block: None,
            subst: Default::default(),
            no_bounds: false,
            is_proc: false,
            decl: None,
            site: None,
        });
        self.body.exits.push(super::body::Exit::Function { frame: 0 });
        self.begin_block();
        for (name, fty) in fields {
            if let Some((default, loc)) = self.field_default(decl, name) {
                let _ = self.lower_in_package(default, fty, loc);
            }
        }
        self.end_block();
        self.body = saved;
    }

    fn field_default(&self, decl: DeclId, name: Name) -> Option<(&'a ast::Expr, super::DeclLoc)> {
        let d = &self.decls[decl.0 as usize];
        let DeclKind::Struct(s) = d.kind else { return None };
        let loc = d.loc;
        s.body.iter().find_map(|i| match &i.kind {
            ItemKind::Field(f) if f.name.name == name => f.default.as_ref().map(|e| (e, loc)),
            _ => None,
        })
    }

    /// Lowers an expression written in another declaration (like a field
    /// default) so that names resolve in that declaration's package and no
    /// local variable of the current method is visible.
    pub fn lower_in_package(&mut self, e: &ast::Expr, ty: TyId, loc: super::DeclLoc) -> ir::Expr {
        let ret = self.frame().ret;
        self.body.frames.push(Frame {
            loc,
            scopes: vec![super::body::Scope::default()],
            ret,
            self_ty: None,
            self_local: None,
            fn_name: self.frame().fn_name.clone(),
            block: None,
            subst: Default::default(),
            no_bounds: false,
            is_proc: false,
            decl: None,
            site: None,
        });
        let v = self.expr_coerced(e, ty);
        self.body.frames.pop();
        v
    }
}

impl<'a> Checker<'a> {
    /// Returns the type of a non-generic union declaration.
    pub fn union_type(&mut self, decl: DeclId) -> TyId {
        if let Some(&t) = self.decl_types.get(&decl) {
            if self.shallow == 0
                && let Some(i) = self.pending_types.iter().position(|d| *d == decl)
            {
                self.pending_types.remove(i);
                self.fill_union(decl, t, Default::default());
            }
            return t;
        }
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Union(u) = d.kind else { unreachable!("union_type on a non-union") };
        if !u.generics.is_empty() {
            let names: Vec<&str> = u.generics.iter().map(|g| g.name.as_str()).collect();
            self.report(
                Diagnostic::error(codes::GENERIC_ARGS, format!("`{}` needs type arguments", d.name))
                    .primary(d.span, format!("write it like `{}({})`", d.name, names.join(", "))),
            );
            return self.types.unknown();
        }
        let prefix = self.pkg_prefix(d.loc.pkg);
        let c_name = format!("{prefix}__{}", mangle_ident(d.name.as_str()));
        let ty = self.new_union_type(decl, d.name.as_str().to_string(), c_name, Default::default());
        self.decl_types.insert(decl, ty);
        if self.shallow > 0 {
            self.pending_types.push(decl);
            return ty;
        }
        self.fill_union(decl, ty, Default::default());
        ty
    }

    /// Returns the instance of a generic union for `args`, creating it the
    /// first time.
    pub fn union_instance(&mut self, decl: DeclId, args: Vec<TyId>, span: Span) -> TyId {
        let key = (decl, args.clone());
        if let Some(&t) = self.union_insts.get(&key) {
            return t;
        }
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Union(u) = d.kind else { unreachable!("union_instance on a non-union") };
        if !self.check_generic_args(decl, &args, &[], span) {
            return self.types.unknown();
        }
        let count = self.union_insts.keys().filter(|(k, _)| *k == decl).count();
        let shown: Vec<String> = args.iter().map(|a| self.types.display(*a)).collect();
        let prefix = self.pkg_prefix(d.loc.pkg);
        let subst: super::generics::Subst =
            std::rc::Rc::new(u.generics.iter().map(|g| g.name.name).zip(args.iter().copied()).collect());
        let ty = self.new_union_type(
            decl,
            format!("{}({})", d.name, shown.join(", ")),
            format!("{prefix}__{}__{}", mangle_ident(d.name.as_str()), count + 1),
            subst.clone(),
        );
        self.union_insts.insert(key, ty);
        // Each instance resolves its own variants (see `struct_instance`).
        let deferrals = self.value_deferrals;
        self.fill_union(decl, ty, subst);
        self.value_deferrals = deferrals;
        ty
    }

    fn new_union_type(&mut self, decl: DeclId, name: String, c_name: String, subst: super::generics::Subst) -> TyId {
        let span = self.decls[decl.0 as usize].span;
        let ty = self.types.new_union(crate::types::UnionInfo {
            name,
            c_name,
            variants: Vec::new(),
            size: 0,
            align: 1,
            complete: false,
            span,
        });
        if let TyKind::Union(uid) = *self.types.kind(ty) {
            self.union_args.insert(uid, (decl, subst));
        }
        ty
    }

    /// Resolves a union's variants with `subst` applied and computes its layout.
    fn fill_union(&mut self, decl: DeclId, ty: TyId, subst: super::generics::Subst) {
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Union(u) = d.kind else { unreachable!("fill_union on a non-union") };
        let ctx = TyCtx { loc: d.loc, self_ty: Some(ty), subst };
        let mut variants: Vec<TyId> = Vec::new();
        self.resolving.push(decl);
        let mut resolved = Vec::new();
        for v in &u.variants {
            resolved.push(self.resolve_type(v, &ctx));
        }
        self.resolving.pop();
        for (v, vt) in u.variants.iter().zip(resolved) {
            if variants.contains(&vt) {
                let shown = self.types.display(vt);
                self.report(
                    Diagnostic::error(codes::DUPLICATE_DEFINITION, format!("`{shown}` is listed twice in this union"))
                        .primary(v.span, "duplicate variant"),
                );
                continue;
            }
            if self.by_value_incomplete(vt).is_some() || vt == ty {
                let text = self.source_text(v.span);
                self.report(
                    Diagnostic::error(codes::RECURSIVE_TYPE, format!("`{}` contains itself", d.name))
                        .primary(v.span, "this variant stores the union by value")
                        .note("a type cannot contain a full copy of itself, so it would have no finite size")
                        .suggest_replace(
                            "store a pointer instead",
                            v.span,
                            format!("^{text}"),
                            Applicability::MaybeIncorrect,
                        ),
                );
                continue;
            }
            if self.types.is_nilable(vt) && !matches!(self.types.kind(vt), TyKind::Unknown) {
                let shown = self.types.display(vt);
                let mut diag =
                    Diagnostic::error(codes::TYPE_MISMATCH, format!("a union variant cannot be nil-able (`{shown}`)"))
                        .primary(v.span, "unions already have a nil state");
                let text = self.source_text(v.span);
                diag = match text.strip_suffix('?') {
                    Some(plain) => diag.suggest_replace(
                        "use the non-optional type; a union value can be `nil` by itself",
                        v.span,
                        plain.to_string(),
                        Applicability::MachineApplicable,
                    ),
                    None => diag.help("use a non-optional type; a union value can be `nil` by itself"),
                };
                self.report(diag);
            }
            let union_name = self.types.display(ty);
            if self.variant_too_large(&union_name, vt, v.span) {
                continue;
            }
            variants.push(vt);
        }
        // A union without variants gets a `char` in C, like an empty struct.
        let mut payload = (1u64, 1u64);
        for v in &variants {
            let (s, a) = self.types.layout(*v);
            payload = (payload.0.max(s), payload.1.max(a));
        }
        let (size, align) = crate::types::aggregate(&[(4, 4), payload]);
        let TyKind::Union(uid) = *self.types.kind(ty) else { unreachable!() };
        let info = &mut self.types.unions[uid.0 as usize];
        info.variants = variants;
        info.size = size;
        info.align = align;
        info.complete = true;
        self.fill_pending_types();
    }

    /// Returns the index of `variant` in a union type.
    pub fn union_variant(&self, union_ty: TyId, variant: TyId) -> Option<u32> {
        let TyKind::Union(id) = self.types.kind(self.types.base(union_ty)) else { return None };
        self.types.union_info(*id).variants.iter().position(|v| *v == variant).map(|i| i as u32)
    }

    // ----- the size limit ------------------------------------------------------------

    /// Checks the size of a type written at `span` against the limit
    /// ([`MAX_TYPE_SIZE`]): returns the type, or the unknown type after
    /// reporting it (E0329). `within` is the generic instance whose fields
    /// or signature are being resolved. A type that stores a struct whose
    /// fields aren't resolved yet (an array behind a pointer) is checked
    /// once they are.
    pub(super) fn check_type_size(&mut self, ty: TyId, span: Span, within: Option<TyId>) -> TyId {
        if self.by_value_incomplete(ty).is_some() {
            self.size_checks.push((ty, span, within));
            return ty;
        }
        if self.report_too_large(ty, span, within) { self.types.unknown() } else { ty }
    }

    /// Checks the types [`Checker::check_type_size`] left for later whose
    /// structs are laid out now.
    pub(super) fn check_pending_sizes(&mut self) {
        if self.size_checks.is_empty() || !self.resolving.is_empty() || self.shallow != 0 {
            return;
        }
        for (ty, span, within) in std::mem::take(&mut self.size_checks) {
            if self.by_value_incomplete(ty).is_some() {
                self.size_checks.push((ty, span, within));
            } else {
                self.report_too_large(ty, span, within);
            }
        }
    }

    /// Reports a type over the size limit (E0329) at `span`, where it is
    /// written, and returns whether it was.
    pub(super) fn report_too_large(&mut self, ty: TyId, span: Span, within: Option<TyId>) -> bool {
        let Some(size) = self.types.oversize(ty) else { return false };
        let shown = self.types.display(ty);
        let elem = match self.types.kind(ty) {
            TyKind::Array(elem, _) => Some(*elem),
            _ => None,
        };
        self.too_large_error(format!("`{shown}` would take {size} bytes"), elem, span, within);
        true
    }

    /// Reports a call of a generic method whose instance would return a
    /// type over the size limit (E0329), and returns whether it would.
    pub(super) fn report_too_large_return(&mut self, ret: TyId, method: Name, span: Span) -> bool {
        let Some(size) = self.types.oversize(ret) else { return false };
        let shown = self.types.display(ret);
        let label = format!("`{method}` would return `{shown}` here, which would take {size} bytes");
        let mut diag = Diagnostic::error(codes::TYPE_TOO_LARGE, "this type is too large")
            .primary(span, label)
            .note(size_limit_note());
        diag = diag.help(format!("pass a smaller value, or make `{method}` return a pointer or a slice"));
        self.report(diag);
        true
    }

    /// Reports an array type whose length doesn't even fit in a `u64`
    /// (E0329).
    pub(super) fn report_long_array(&mut self, elem: TyId, len: i128, span: Span, within: Option<TyId>) {
        let shown = format!("[{len}]{}", self.types.display(elem));
        let size = len.unsigned_abs().saturating_mul(self.types.wide_layout(elem).0.max(1));
        self.too_large_error(format!("`{shown}` would take {size} bytes"), Some(elem), span, within);
    }

    fn too_large_error(&mut self, label: String, elem: Option<TyId>, span: Span, within: Option<TyId>) {
        let mut diag = Diagnostic::error(codes::TYPE_TOO_LARGE, "this type is too large")
            .primary(span, label)
            .note(size_limit_note());
        if let Some(t) = within {
            diag = diag.note(format!("in `{}`", self.types.display(t)));
        }
        let help = match elem {
            Some(elem) => {
                let elem = self.types.display(elem);
                format!(
                    "use a smaller length; for data sized at run time, allocate a slice with `alloc([]{elem}, n)` or use a `[dynamic]{elem}`"
                )
            }
            None => "make its parts smaller, or keep large data behind a pointer or in a slice allocated at run time"
                .to_string(),
        };
        self.report(diag.help(help));
    }

    /// The generic struct or union instance whose fields, variants or
    /// methods a type context resolves, if it is one.
    pub(super) fn resolving_instance(&self, self_ty: Option<TyId>) -> Option<TyId> {
        let ty = self_ty?;
        let generic = match self.types.kind(ty) {
            TyKind::Struct(id) => self.struct_args.get(id).is_some_and(|args| !args.is_empty()),
            TyKind::Union(id) => self.union_args.get(id).is_some_and(|(_, args)| !args.is_empty()),
            _ => false,
        };
        generic.then_some(ty)
    }

    /// Checks that a struct's fields fit in the size limit together. A
    /// struct over it is reported (E0329) at the field that takes it over,
    /// whose type `spans` gives; that field becomes unknown, and so does
    /// each later one that still goes over, so the struct gets a layout.
    pub(super) fn check_fields_size(&mut self, name: &str, fields: &mut [FieldInfo], spans: &[Span]) {
        let limit = u128::from(MAX_TYPE_SIZE);
        let mut reported = false;
        loop {
            let parts: Vec<(u128, u64)> = fields.iter().map(|f| self.types.wide_layout(f.ty)).collect();
            let size_of = |n: usize| aggregate_wide(parts[..n].iter().copied()).0;
            if size_of(parts.len()) <= limit {
                return;
            }
            let Some(over) = (1..=parts.len()).find(|&n| size_of(n) > limit) else { return };
            let field = &mut fields[over - 1];
            if !reported {
                reported = true;
                let span = spans.get(over - 1).copied().unwrap_or(field.span);
                let total = size_of(parts.len());
                let mut diag = Diagnostic::error(codes::TYPE_TOO_LARGE, format!("`{name}` is too large"))
                    .primary(span, format!("with this field, `{name}` would take {} bytes", size_of(over)));
                if total > size_of(over) {
                    diag = diag.note(format!("all its fields together would take {total} bytes"));
                }
                self.report(
                    diag.note(size_limit_note())
                        .help("keep large data behind a pointer, or in a slice allocated at run time"),
                );
            }
            field.ty = self.types.unknown();
        }
    }

    /// Whether a union variant is too large for a union, with its tag: if
    /// so, reports it (E0329) at `span`, where the variant is written.
    fn variant_too_large(&mut self, union_name: &str, variant: TyId, span: Span) -> bool {
        let size = aggregate_wide([(4, 4), self.types.wide_layout(variant)]).0;
        if size <= u128::from(MAX_TYPE_SIZE) {
            return false;
        }
        self.report(
            Diagnostic::error(codes::TYPE_TOO_LARGE, format!("`{union_name}` is too large"))
                .primary(span, format!("with this variant and its tag, `{union_name}` would take {size} bytes"))
                .note(size_limit_note())
                .help("store the variant behind a pointer, like `^T`"),
        );
        true
    }
}

/// The note that says what the size limit is.
fn size_limit_note() -> String {
    format!(
        "a type can take at most {MAX_TYPE_SIZE} bytes (2^61 - 1), the most that every supported C compiler accepts"
    )
}

/// The number in a `@[size(n)]` or `@[align(n)]` attribute.
fn layout_attr(item: &ast::Item, name: &str) -> Option<u64> {
    match item.attr(name)?.args.first()?.kind {
        ast::ExprKind::Int(n) => u64::try_from(n).ok(),
        _ => None,
    }
}

//! Declaration collection, constants and function signatures.

use wid_diagnostics::{Applicability, Diagnostic, Span, codes};
use wid_syntax::Name;
use wid_syntax::ast::{self, ItemKind};

use super::{Checker, ConstState, Decl, DeclId, DeclKind, DeclLoc, FnSig, ParamSig};
use crate::input::PackageId;
use crate::ir::{self, ExprKind};
use crate::types::{TyId, TyKind};

/// A compile-time constant value, possibly untyped.
#[derive(Clone, Debug)]
pub(crate) enum ConstValue {
    Int(i128),
    Float(f64),
    Bool(bool),
    Str(String),
    /// A value computed by compile-time code, with its type.
    Typed(ir::Expr),
}

impl ConstValue {
    /// Wraps a typed constant, keeping literal scalars as plain values so
    /// they work as array lengths and generic arguments.
    pub fn from_typed(e: ir::Expr) -> ConstValue {
        match &e.kind {
            ExprKind::Int(v) => ConstValue::Int(*v),
            ExprKind::Float(v) => ConstValue::Float(*v),
            ExprKind::Bool(v) => ConstValue::Bool(*v),
            ExprKind::Str(v) => ConstValue::Str(v.clone()),
            _ => ConstValue::Typed(e),
        }
    }
}

/// Whether an expression needs the interpreter to be evaluated at compile
/// time, rather than plain folding of literals.
fn needs_interpreter(e: &ast::Expr) -> bool {
    use ast::ExprKind as E;
    match &e.kind {
        E::Comptime(_)
        | E::ComptimeIf(_)
        | E::Call(_)
        | E::Member { .. }
        | E::Index { .. }
        | E::Array(_)
        | E::Ternary { .. }
        | E::If(_)
        | E::Case(_)
        | E::Zero
        | E::Nil => true,
        E::Str(parts) => parts.iter().any(|p| matches!(p, ast::StrPart::Interp(_))),
        E::Paren(inner) | E::Unary { expr: inner, .. } => needs_interpreter(inner),
        E::Binary { lhs, rhs, .. } => needs_interpreter(lhs) || needs_interpreter(rhs),
        _ => false,
    }
}

impl<'a> Checker<'a> {
    /// Registers every declaration of every package.
    pub(super) fn collect(&mut self) {
        let input = self.input;
        let mut merges = Vec::new();
        for (p, pkg) in input.packages.iter().enumerate() {
            for (f, file) in pkg.files.iter().enumerate() {
                let loc = DeclLoc { pkg: PackageId(p as u32), file: f };
                merges.extend(self.collect_items(&file.ast.items, loc));
            }
        }
        for (pkg, cpkg, span) in merges {
            self.merge_cimport(pkg, cpkg, span);
        }
        self.resolve_comptime_ifs();
    }

    /// Collects the declarations of a file, or of a chosen `comptime if`
    /// branch at the top level of one.
    pub(super) fn collect_conditional(&mut self, items: &'a [ast::Item], loc: DeclLoc) {
        for (pkg, cpkg, span) in self.collect_items(items, loc) {
            self.merge_cimport(pkg, cpkg, span);
        }
    }

    /// Collects a declaration of a `comptime if` branch inside a type.
    pub(super) fn collect_member_item(&mut self, item: &'a ast::Item, loc: DeclLoc, owner: DeclId) {
        if let ItemKind::Field(field) = &item.kind
            && matches!(self.decls[owner.0 as usize].kind, DeclKind::Struct(_))
        {
            self.report(
                Diagnostic::error(codes::COMPTIME_ONLY, "fields can't be declared inside `comptime if`")
                    .primary(field.name.span, "a struct's fields are fixed for every target")
                    .help("declare the field outside the `comptime if`; it can hold a value that is unused on some targets"),
            );
            return;
        }
        self.collect_item(item, loc, Some(owner));
    }

    /// Collects declarations and imports; returns the `cimport`s without
    /// `as:`, whose names join the package once every package is collected.
    fn collect_items(&mut self, items: &'a [ast::Item], loc: DeclLoc) -> Vec<(PackageId, PackageId, Span)> {
        let input = self.input;
        let pkg_id = loc.pkg;
        let f = loc.file;
        let file = &input.packages[pkg_id.0 as usize].files[f];
        let mut merges = Vec::new();
        let mut imports = self.file_imports.remove(&(pkg_id, f)).unwrap_or_default();
        for item in items {
            let i = item.span.start;
            match &item.kind {
                ItemKind::ComptimeIf(c) => {
                    self.check_item_attributes(item);
                    self.pending_ifs.push(super::comptime::PendingIf { loc, item: c, owner: None });
                }
                ItemKind::Import(import) => {
                    let Some(&target) = file.imports.get(&i) else {
                        let name = match import.alias {
                            Some(alias) => alias.name,
                            None => Name::new(&import_name(&import.path)),
                        };
                        self.failed_imports.insert((pkg_id, f, name));
                        continue;
                    };
                    let name = match import.alias {
                        Some(alias) => alias.name,
                        None => Name::new(&input.packages[target.0 as usize].name),
                    };
                    if let Some((_, prev)) = imports.insert(name, (target, item.span)) {
                        self.report(
                            Diagnostic::error(codes::DUPLICATE_DEFINITION, format!("`{name}` is imported twice"))
                                .primary(item.span, "second import")
                                .secondary(prev, "first import")
                                .help("give one of them a different name with `as: :other_name`"),
                        );
                    }
                }
                ItemKind::Cimport(c) => {
                    let alias = cimport_as(c);
                    let Some(&target) = file.imports.get(&i) else {
                        match alias {
                            Some(alias) => {
                                self.failed_imports.insert((pkg_id, f, Name::new(&alias)));
                            }
                            None => {
                                self.failed_merges.insert(pkg_id);
                            }
                        }
                        continue;
                    };
                    let name = Name::new(&input.packages[target.0 as usize].name);
                    if alias.is_none() {
                        merges.push((pkg_id, target, item.span));
                    } else if let Some((_, prev)) = imports.insert(name, (target, item.span)) {
                        self.report(
                            Diagnostic::error(codes::DUPLICATE_DEFINITION, format!("`{name}` is imported twice"))
                                .primary(item.span, "second import")
                                .secondary(prev, "first import")
                                .help("give one of them a different name with `as: :other_name`"),
                        );
                    }
                    let c_types =
                        input.packages[target.0 as usize].files.first().and_then(|g| g.imports.values().next());
                    if let Some(&c_types) = c_types {
                        imports.entry(Name::new("C")).or_insert((c_types, item.span));
                    }
                }
                _ => self.collect_item(item, loc, None),
            }
        }
        self.file_imports.insert((pkg_id, f), imports);
        merges
    }

    /// Adds the declarations of a `cimport` without `as:` to the namespace
    /// of the package that wrote it, so they are visible unqualified and to
    /// the package's importers. A name both declare is an error.
    fn merge_cimport(&mut self, pkg: PackageId, cpkg: PackageId, item_span: Span) {
        let header = self.input.packages[cpkg.0 as usize].path.trim_start_matches("cimport:").to_string();
        let mut names: Vec<(Name, DeclId)> = self.pkg_scopes[cpkg.0 as usize].iter().map(|(n, d)| (*n, *d)).collect();
        names.sort_by_key(|(_, d)| d.0);
        for (name, decl) in names {
            let Some(&prev) = self.pkg_scopes[pkg.0 as usize].get(&name) else {
                self.pkg_scopes[pkg.0 as usize].insert(name, decl);
                continue;
            };
            let prev_pkg = self.decls[prev.0 as usize].loc.pkg;
            let c_name = self.c_name_of(cpkg, name).unwrap_or_else(|| name.as_str().to_string());
            let diag = if self.c_binding(prev_pkg).is_some() {
                let other = self.input.packages[prev_pkg.0 as usize].path.trim_start_matches("cimport:").to_string();
                Diagnostic::error(codes::DUPLICATE_DEFINITION, format!("`{name}` is declared by two headers"))
                    .primary(item_span, format!("`{header}` declares `{c_name}`"))
                    .note(format!("`{other}` declares it too, and both `cimport`s add their names to this package"))
                    .help("give one of the `cimport`s its own namespace with `as:`, or rename the name with `names:`")
            } else {
                let prev_span = self.decls[prev.0 as usize].span;
                let diag = Diagnostic::error(
                    codes::DUPLICATE_DEFINITION,
                    format!("`{name}` is declared here and by `{header}`"),
                )
                .primary(prev_span, "declared in this package")
                .secondary(item_span, format!("this `cimport` adds `{c_name}` from C as `{name}`"))
                .note("a `cimport` without `as:` adds the header's names to the package");
                let has_names = self.c_binding(cpkg).is_some_and(|b| {
                    let (p, f, i) = b.origin;
                    self.input.packages[p.0 as usize].files.get(f).and_then(|file| file.ast.item_at(i as u32)).is_some_and(
                        |item| matches!(&item.kind, ItemKind::Cimport(c) if c.options.iter().any(|o| o.name.as_str() == "names")),
                    )
                });
                if has_names {
                    diag.help(format!(
                        "add `{c_name}: :c_{name}` to the `cimport`'s `names:`, or rename this declaration"
                    ))
                } else {
                    diag.suggest(
                        "give the C one another name, or rename this declaration".to_string(),
                        vec![wid_diagnostics::Edit {
                            span: item_span.shrink_to_end(),
                            replacement: format!(", names: {{{c_name}: :c_{name}}}"),
                        }],
                        Applicability::MaybeIncorrect,
                    )
                }
            };
            self.report(diag);
        }
        self.merged_cimports.entry(pkg).or_default().push(cpkg);
    }

    fn collect_item(&mut self, item: &'a ast::Item, loc: DeclLoc, owner: Option<DeclId>) {
        self.check_item_attributes(item);
        let (name, span, kind) = match &item.kind {
            ItemKind::Def(f) if owner.is_some() && f.params.is_empty() && matches!(f.name.as_str(), "-" | "~") => {
                (Name::new(&format!("{}@", f.name.as_str())), f.name.span, DeclKind::Fn(f))
            }
            ItemKind::Def(f) => (f.name.name, f.name.span, DeclKind::Fn(f)),
            ItemKind::Const(c) => (c.name.name, c.name.span, DeclKind::Const(c)),
            ItemKind::Struct(s) => (s.name.name, s.name.span, DeclKind::Struct(s)),
            ItemKind::Enum(e) => (e.name.name, e.name.span, DeclKind::Enum(e)),
            ItemKind::Union(u) => (u.name.name, u.name.span, DeclKind::Union(u)),
            ItemKind::Module(m) => (m.name.name, m.name.span, DeclKind::Module(m)),
            ItemKind::Overload(o) => (o.name.name, o.name.span, DeclKind::Overload(o)),
            ItemKind::MacroCall(expr) => {
                if owner.is_some() {
                    let text = self.source_text(expr.span);
                    self.report(
                        Diagnostic::error(
                            codes::MACRO_FAILED,
                            format!("`{text}` cannot expand: macros cannot run yet"),
                        )
                        .primary(expr.span, "a macro call inside a type")
                        .help("write the methods out, for example `def hp = @hp` for a reader"),
                    );
                } else {
                    self.report(
                        Diagnostic::error(codes::TOP_LEVEL_STATEMENT, "statements must be inside a method")
                            .primary(expr.span, "this call is outside any `def`")
                            .help("move it into `def main … end`, which runs when the program starts"),
                    );
                }
                return;
            }
            ItemKind::Field(f) => {
                match owner.map(|o| &self.decls[o.0 as usize].kind) {
                    None => self.report(
                        Diagnostic::error(codes::TOP_LEVEL_STATEMENT, "fields belong inside a `struct`")
                            .primary(f.name.span, "this field is outside any struct")
                            .note("Wid has no package-level variables")
                            .help(format!(
                                "for a fixed value use a constant, like `{} = …`; for state, keep it in a struct that methods receive",
                                f.name.as_str().to_uppercase()
                            )),
                    ),
                    Some(DeclKind::Struct(_)) => {}
                    Some(DeclKind::Module(_)) => self.report(
                        Diagnostic::error(codes::UNEXPECTED_TOKEN, "modules cannot declare fields")
                            .primary(f.name.span, "a module only holds methods and constants")
                            .help(format!(
                                "declare `{}` in each struct that includes the module; module methods read it with `@{}`",
                                f.name.as_str(),
                                f.name.as_str()
                            )),
                    ),
                    Some(other) => {
                        let what = other.describe();
                        let article = if what.starts_with(['a', 'e', 'i', 'o', 'u']) { "an" } else { "a" };
                        let help = if matches!(other, DeclKind::Enum(_)) {
                            format!("enum values carry no data; keep `{}` in a struct next to the enum", f.name.as_str())
                        } else {
                            "`extend` adds methods only; declare the field in the struct itself".to_string()
                        };
                        self.report(
                            Diagnostic::error(
                                codes::UNEXPECTED_TOKEN,
                                format!("{article} {what} cannot declare fields"),
                            )
                            .primary(f.name.span, "fields belong inside a `struct`")
                            .help(help),
                        );
                    }
                }
                return;
            }
            ItemKind::Include(t) => {
                if owner.is_none() {
                    self.report(
                        Diagnostic::error(
                            codes::TOP_LEVEL_STATEMENT,
                            "`include` belongs inside a `struct`, `enum`, `module` or `extend`",
                        )
                        .primary(t.span, "nothing to include this into")
                        .help("move it into the body of the type that should get the module's methods"),
                    );
                }
                return;
            }
            ItemKind::Extend(e) => {
                if owner.is_some() {
                    self.report(
                        Diagnostic::error(codes::UNEXPECTED_TOKEN, "`extend` belongs at the top level of a file")
                            .primary(item.span, "move it out of this declaration"),
                    );
                    return;
                }
                let id = DeclId(self.decls.len() as u32);
                let span = e.targets.first().map_or(item.span, |t| t.span);
                self.decls.push(Decl {
                    name: Name::new("<extend>"),
                    span,
                    loc,
                    private: item.private,
                    item,
                    kind: DeclKind::Extend(e),
                    owner: None,
                });
                self.register_extend(id);
                for member in &e.body {
                    self.collect_item(member, loc, Some(id));
                }
                return;
            }
            ItemKind::Cimport(_) => {
                self.report(
                    Diagnostic::error(codes::UNEXPECTED_TOKEN, "`cimport` belongs at the top level of a file")
                        .primary(item.span, "move it out of this declaration"),
                );
                return;
            }
            ItemKind::ComptimeIf(c) => {
                self.pending_ifs.push(super::comptime::PendingIf { loc, item: c, owner });
                return;
            }
            ItemKind::Import(_) | ItemKind::Error => return,
        };
        if let (Some(o), DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Union(_) | DeclKind::Module(_)) =
            (owner, &kind)
        {
            let what = self.decls[o.0 as usize].kind.a_describe();
            self.report(
                Diagnostic::error(codes::UNEXPECTED_TOKEN, format!("types cannot be declared inside {what}"))
                    .primary(span, "move this declaration to the top level of the file"),
            );
            return;
        }
        if let (Some(o), DeclKind::Fn(f)) = (owner, &kind)
            && f.is_static
            && name.as_str() == "new"
            && matches!(self.decls[o.0 as usize].kind, DeclKind::Struct(_))
        {
            self.report(
                Diagnostic::error(codes::DUPLICATE_DEFINITION, "`new` is the builtin constructor")
                    .primary(span, "`Type.new(field: value)` always builds a value field by field")
                    .suggest_replace(
                        "give a custom constructor its own name",
                        span,
                        "create",
                        Applicability::MaybeIncorrect,
                    ),
            );
            return;
        }
        if owner.is_none() && super::ty::is_reserved_type_name(name.as_str()) {
            self.report(
                Diagnostic::error(codes::DUPLICATE_DEFINITION, format!("`{name}` is a builtin type"))
                    .primary(span, "this name always means the builtin type")
                    .note("builtin types like `Int`, `String` and `Bool` can't be redeclared, so code reads the same in every package")
                    .suggest_replace("give it a name of its own", span, format!("My{name}"), Applicability::HasPlaceholders),
            );
            return;
        }
        let id = DeclId(self.decls.len() as u32);
        self.decls.push(Decl { name, span, loc, private: item.private, item, kind, owner });
        let scope = match owner {
            None => &mut self.pkg_scopes[loc.pkg.0 as usize],
            Some(o) => self.members.entry(o).or_default(),
        };
        if let Some(&prev) = scope.get(&name) {
            let prev_decl = &self.decls[prev.0 as usize];
            let both_fns =
                matches!(prev_decl.kind, DeclKind::Fn(_)) && matches!(self.decls[id.0 as usize].kind, DeclKind::Fn(_));
            let prev_span = prev_decl.span;
            if both_fns {
                let text = name.as_str();
                let (a, b) = self.overload_member_names(prev, id);
                self.report(
                    Diagnostic::error(codes::IMPLICIT_OVERLOAD, format!("`{text}` is defined twice"))
                        .primary(span, "second definition")
                        .secondary(prev_span, "first definition")
                        .note("Wid has no implicit overloading: every definition needs its own name")
                        .help(format!(
                            "rename them (for example `{a}` and `{b}`) and group them with `overload :{text}, :{a}, :{b}`"
                        )),
                );
            } else {
                self.report(
                    Diagnostic::error(codes::DUPLICATE_DEFINITION, format!("`{name}` is defined twice"))
                        .primary(span, "second definition")
                        .secondary(prev_span, "first definition"),
                );
            }
            return;
        }
        scope.insert(name, id);
        let body: &'a [ast::Item] = match &item.kind {
            ItemKind::Struct(s) => &s.body,
            ItemKind::Enum(e) => &e.body,
            ItemKind::Module(m) => &m.body,
            _ => &[],
        };
        for member in body {
            self.collect_item(member, loc, Some(id));
        }
    }

    /// Suggested names for two functions that should become members of an
    /// overload set, based on the name and the first parameter's type, like
    /// `clamp_int` and `clamp_f32` or `mul_vec2` and `mul_transform`.
    fn overload_member_names(&self, a: DeclId, b: DeclId) -> (String, String) {
        let suggest = |decl: DeclId, fallback: &str| {
            let d = &self.decls[decl.0 as usize];
            let base = match d.name.as_str() {
                "+" => "add",
                "-" => "sub",
                "*" => "mul",
                "/" => "div",
                "%" => "rem",
                "==" => "eq",
                "<=>" => "cmp",
                "[]" => "get",
                "[]=" => "set",
                "-@" => "neg",
                "~@" => "bit_not",
                other => other,
            };
            let suffix = match d.kind {
                DeclKind::Fn(f) => f.params.first().map(|p| type_suffix(&self.source_text(p.ty.span))),
                _ => None,
            };
            match suffix {
                Some(s) if !s.is_empty() => format!("{base}_{s}"),
                _ => format!("{base}_{fallback}"),
            }
        };
        let (x, y) = (suggest(a, "a"), suggest(b, "b"));
        if x == y { (format!("{x}_a"), format!("{y}_b")) } else { (x, y) }
    }

    /// Returns true when a function needs no generic instantiation.
    pub fn decl_is_concrete(&self, decl: DeclId) -> bool {
        let d = &self.decls[decl.0 as usize];
        let DeclKind::Fn(_) = d.kind else { return false };
        if !self.generic_names(decl).is_empty() {
            return false;
        }
        match d.owner {
            None => true,
            Some(o) => match self.decls[o.0 as usize].kind {
                DeclKind::Struct(s) => s.generics.is_empty(),
                DeclKind::Enum(_) => true,
                _ => false,
            },
        }
    }

    /// Returns the type a member declaration belongs to, if its owner is a type.
    pub fn owner_type(&mut self, decl: DeclId) -> Option<TyId> {
        let owner = self.decls[decl.0 as usize].owner?;
        match self.decls[owner.0 as usize].kind {
            DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Union(_) => {
                let span = self.decls[owner.0 as usize].span;
                Some(self.decl_as_type(owner, span))
            }
            DeclKind::Extend(_) | DeclKind::Module(_) => Some(self.types.intern(TyKind::Param(Name::new("Self")))),
            _ => None,
        }
    }

    /// Looks up a package-level declaration.
    pub fn lookup_pkg(&self, pkg: PackageId, name: Name) -> Option<DeclId> {
        self.pkg_scopes[pkg.0 as usize].get(&name).copied()
    }

    /// Looks up a name in the prelude package, which every file sees.
    pub fn lookup_prelude(&self, name: Name) -> Option<DeclId> {
        let prelude = self.input.prelude?;
        let decl = self.pkg_scopes[prelude.0 as usize].get(&name).copied()?;
        (!self.decls[decl.0 as usize].private).then_some(decl)
    }

    /// Looks up an import alias visible in a file.
    pub fn lookup_import(&self, loc: DeclLoc, name: Name) -> Option<PackageId> {
        self.file_imports.get(&(loc.pkg, loc.file)).and_then(|m| m.get(&name)).map(|(p, _)| *p)
    }

    /// Whether a constant's value names a type, making the constant a type
    /// alias: `Texture2D = Texture`, `Key = C.int`.
    pub fn is_type_alias_value(&self, value: &ast::Expr, loc: DeclLoc, depth: u32) -> bool {
        match &value.kind {
            ast::ExprKind::Type(_) => true,
            ast::ExprKind::Member { recv, .. } => {
                matches!(recv.kind, ast::ExprKind::Ident(p) | ast::ExprKind::Const(p) if self.lookup_import(loc, p).is_some())
            }
            ast::ExprKind::Const(n) if super::ty::PRIMITIVE_NAMES.contains(&n.as_str()) => true,
            ast::ExprKind::Const(n) if depth < 16 => {
                match self.lookup_pkg(loc.pkg, *n).or_else(|| self.lookup_prelude(*n)) {
                    Some(decl) => match self.decls[decl.0 as usize].kind {
                        DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Union(_) => true,
                        DeclKind::Const(c) => {
                            self.is_type_alias_value(&c.value, self.decls[decl.0 as usize].loc, depth + 1)
                        }
                        _ => false,
                    },
                    None => self.c_binding(loc.pkg).is_some_and(|b| b.record_names.values().any(|w| w == n.as_str())),
                }
            }
            _ => false,
        }
    }

    /// Resolves the signature of a function declaration.
    pub fn fn_sig(&mut self, decl: DeclId) -> FnSig {
        if let Some(sig) = self.sigs.get(&decl) {
            return sig.clone();
        }
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Fn(f) = d.kind else { unreachable!("fn_sig on a non-function") };
        let owner_ty = self.owner_type(decl);
        let subst = self.template_subst(decl);
        let ctx = super::ty::TyCtx { loc: d.loc, self_ty: owner_ty, subst };
        let mut params = Vec::new();
        for p in &f.params {
            let ty = self.resolve_type(&p.ty, &ctx);
            params.push(ParamSig { name: p.name.name, ty, span: p.span });
        }
        let block = f.block.as_ref().and_then(|b| self.block_sig(b, &ctx));
        let ret = match &f.ret {
            Some(t) => self.resolve_type(t, &ctx),
            None => self.types.void(),
        };
        let receiver = if f.is_static { None } else { owner_ty };
        if f.is_static && owner_ty.is_none() {
            self.report(
                Diagnostic::error(codes::SELF_OUTSIDE_METHOD, "`def self.` only makes sense inside a type")
                    .primary(f.sig_span, "there is no type here for `self` to name")
                    .help("remove `self.` to declare a package-level method"),
            );
        }
        let sig = FnSig { params, ret, receiver, block, c_variadic: f.c_variadic.is_some() };
        self.sigs.insert(decl, sig.clone());
        sig
    }

    /// Evaluates a constant declaration.
    pub fn const_value(&mut self, decl: DeclId) -> Option<ir::Expr> {
        self.resolve_const(decl).map(|(_, typed)| typed)
    }

    /// Returns the untyped value of a constant declared without a type.
    pub fn const_untyped(&mut self, decl: DeclId) -> Option<ConstValue> {
        self.resolve_const(decl).and_then(|(untyped, _)| untyped)
    }

    /// Evaluates a constant once, caching its untyped value (when declared
    /// without a type) and its typed value. Reports cycles between constants.
    fn resolve_const(&mut self, decl: DeclId) -> Option<(Option<ConstValue>, ir::Expr)> {
        match self.consts.get(&decl) {
            Some(ConstState::Done { untyped, typed }) => return Some((untyped.clone(), typed.clone())),
            Some(ConstState::Failed) => return None,
            Some(ConstState::Resolving) => {
                self.report_const_cycle(decl);
                return None;
            }
            None => {}
        }
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Const(c) = d.kind else { return None };
        if d.item.has_attr("extern") {
            self.consts.insert(decl, ConstState::Failed);
            return None;
        }
        self.consts.insert(decl, ConstState::Resolving);
        self.const_stack.push(decl);
        let errors_before = self.diags.error_count();
        let ctx = super::ty::TyCtx { loc: d.loc, self_ty: None, subst: Default::default() };
        let declared = c.ty.as_ref().map(|t| self.resolve_type(t, &ctx));
        let folded = self.fold_const(&c.value, d.loc);
        let value = match folded {
            Some(v) => Some(v),
            None => self.interpret_const(&c.value, declared, d.loc).map(ConstValue::Typed),
        };
        self.const_stack.pop();
        let untyped = match &value {
            Some(v) if declared.is_none() && !matches!(v, ConstValue::Typed(_)) => Some(v.clone()),
            _ => None,
        };
        let typed = match (value, declared) {
            (Some(v), Some(ty)) => self.typed_const(v, ty, c.value.span),
            (Some(v), None) => Some(self.default_const(v)),
            (None, _) => {
                let already_reported = self.diags.error_count() > errors_before;
                let alias = self.is_type_alias_value(&c.value, d.loc, 0);
                if !already_reported && !alias && !matches!(c.value.kind, ast::ExprKind::Type(_) | ast::ExprKind::Error)
                {
                    self.report(
                        Diagnostic::error(codes::COMPTIME_ONLY, "constant value is not known at compile time")
                            .primary(c.value.span, "constants must be literals or arithmetic on other constants"),
                    );
                }
                None
            }
        };
        match typed {
            Some(typed) => {
                self.consts.insert(decl, ConstState::Done { untyped: untyped.clone(), typed: typed.clone() });
                Some((untyped, typed))
            }
            None => {
                self.consts.insert(decl, ConstState::Failed);
                None
            }
        }
    }

    /// Reports a constant whose value refers back to itself, naming the
    /// constants on the cycle.
    fn report_const_cycle(&mut self, decl: DeclId) {
        let start = self.const_stack.iter().position(|d| *d == decl).unwrap_or(0);
        let chain: Vec<DeclId> = self.const_stack[start..].to_vec();
        let name = self.decls[decl.0 as usize].name;
        let span = self.decls[decl.0 as usize].span;
        let mut diag = Diagnostic::error(codes::RECURSIVE_TYPE, format!("constant `{name}` depends on itself"));
        if chain.len() > 1 {
            let mut path: Vec<String> = chain.iter().map(|d| format!("`{}`", self.decls[d.0 as usize].name)).collect();
            path.push(format!("`{name}`"));
            diag = diag.primary(span, format!("its value needs {}", path[1..].join(", which needs ")));
            for d in &chain[1..] {
                let other = &self.decls[d.0 as usize];
                diag = diag.secondary(other.span, format!("`{}` is part of the cycle", other.name));
            }
        } else {
            diag = diag.primary(span, "its value refers back to it");
        }
        diag = diag
            .note("a constant's value is computed at compile time, so it cannot be defined in terms of itself")
            .help("give one of the constants a literal value");
        self.report(diag);
    }

    /// Evaluates an expression at compile time: literal arithmetic folds and
    /// stays untyped; anything else (`comptime`, struct literals, `T.size`,
    /// method calls in `comptime`) runs in the interpreter.
    pub fn eval_const(&mut self, expr: &ast::Expr, loc: DeclLoc) -> Option<ConstValue> {
        if let Some(v) = self.fold_const(expr, loc) {
            return Some(v);
        }
        self.interpret_const(expr, None, loc).map(ConstValue::from_typed)
    }

    /// Runs a constant expression in the interpreter, when folding can't
    /// evaluate it. Method calls need an explicit `comptime`.
    pub fn interpret_const(&mut self, expr: &ast::Expr, expected: Option<TyId>, loc: DeclLoc) -> Option<ir::Expr> {
        use super::comptime::ComptimeCode;
        match &expr.kind {
            ast::ExprKind::Comptime(body) => {
                self.comptime_value(ComptimeCode::Stmts(body), expected, loc, expr.span, true)
            }
            _ if needs_interpreter(expr) => {
                self.comptime_value(ComptimeCode::Expr(expr), expected, loc, expr.span, false)
            }
            _ => None,
        }
    }

    /// Folds literal arithmetic on constants, returning `None` when the
    /// expression is anything else.
    pub fn fold_const(&mut self, expr: &ast::Expr, loc: DeclLoc) -> Option<ConstValue> {
        use ast::ExprKind as E;
        Some(match &expr.kind {
            E::Int(v) => ConstValue::Int(i128::try_from(*v).ok()?),
            E::Float(v) => ConstValue::Float(*v),
            E::True => ConstValue::Bool(true),
            E::False => ConstValue::Bool(false),
            E::Str(parts) => {
                let mut s = String::new();
                for p in parts {
                    match p {
                        ast::StrPart::Text(t) => s.push_str(t),
                        ast::StrPart::Interp(_) => return None,
                    }
                }
                ConstValue::Str(s)
            }
            E::Paren(inner) => self.fold_const(inner, loc)?,
            E::Const(name) => {
                let decl = self.lookup_pkg(loc.pkg, *name)?;
                match self.decls[decl.0 as usize].kind {
                    DeclKind::Const(_) => {
                        if let Some(v) = self.const_untyped(decl) {
                            v
                        } else {
                            match ConstValue::from_typed(self.const_value(decl)?) {
                                ConstValue::Typed(_) => return None,
                                v => v,
                            }
                        }
                    }
                    _ => return None,
                }
            }
            E::Unary { op, expr: inner } => {
                let v = self.fold_const(inner, loc)?;
                match (op, v) {
                    (ast::UnOp::Neg, ConstValue::Int(i)) => ConstValue::Int(-i),
                    (ast::UnOp::Neg, ConstValue::Float(f)) => ConstValue::Float(-f),
                    (ast::UnOp::Not, ConstValue::Bool(b)) => ConstValue::Bool(!b),
                    (ast::UnOp::BitNot, ConstValue::Int(i)) => ConstValue::Int(!i),
                    _ => return None,
                }
            }
            E::Binary { op, lhs, rhs } => {
                let l = self.fold_const(lhs, loc)?;
                let r = self.fold_const(rhs, loc)?;
                fold_binary(*op, l, r)?
            }
            _ => return None,
        })
    }

    /// Gives an untyped constant its default type.
    pub fn default_const(&mut self, v: ConstValue) -> ir::Expr {
        match v {
            ConstValue::Int(i) => ir::Expr::new(ExprKind::Int(i), self.types.int()),
            ConstValue::Float(f) => ir::Expr::new(ExprKind::Float(f), self.types.f64()),
            ConstValue::Bool(b) => ir::Expr::new(ExprKind::Bool(b), self.types.bool()),
            ConstValue::Str(s) => ir::Expr::new(ExprKind::Str(s), self.types.string()),
            ConstValue::Typed(e) => e,
        }
    }

    /// Converts an untyped constant to `ty`, reporting range and kind errors.
    pub fn typed_const(&mut self, v: ConstValue, ty: TyId, span: Span) -> Option<ir::Expr> {
        if let ConstValue::Typed(e) = v {
            if e.ty == ty || matches!(self.types.kind(e.ty), TyKind::Unknown) {
                return Some(e);
            }
            return match ConstValue::from_typed(e.clone()) {
                ConstValue::Typed(_) => {
                    let (want, found) = (self.types.display(ty), self.types.display(e.ty));
                    self.report(
                        Diagnostic::error(codes::TYPE_MISMATCH, format!("expected `{want}`, found `{found}`"))
                            .primary(span, format!("this has type `{found}`")),
                    );
                    None
                }
                scalar => self.typed_const(scalar, ty, span),
            };
        }
        let base = self.types.base(ty);
        let kind = self.types.kind(base).clone();
        let ok = match (&v, &kind) {
            (ConstValue::Int(i), TyKind::Int(it)) => {
                let (lo, hi) = it.range();
                if *i < lo || *i > hi {
                    let wider = crate::types::IntTy::ALL.into_iter().find(|t| {
                        let (l, h) = t.range();
                        l <= *i && *i <= h && (l < 0) == (lo < 0) && h > hi
                    });
                    let mut diag =
                        Diagnostic::error(codes::CONSTANT_OVERFLOW, format!("`{i}` does not fit in `{}`", it.name()))
                            .primary(span, format!("`{}` holds values from {lo} to {hi}", it.name()));
                    diag = match wider {
                        Some(w) => diag.help(format!("use a wider type like `{}`, or a value in range", w.name())),
                        None => diag.help("use a value in range"),
                    };
                    self.report(diag);
                    return None;
                }
                Some(ExprKind::Int(*i))
            }
            (ConstValue::Int(i), TyKind::Float(_)) => Some(ExprKind::Float(*i as f64)),
            (ConstValue::Float(f), TyKind::Float(_)) => Some(ExprKind::Float(*f)),
            (ConstValue::Bool(b), TyKind::Bool) => Some(ExprKind::Bool(*b)),
            (ConstValue::Str(s), TyKind::String | TyKind::CString) => Some(ExprKind::Str(s.clone())),
            (ConstValue::Str(s), TyKind::Rune) if s.chars().count() == 1 => {
                Some(ExprKind::Int(i128::from(u32::from(s.chars().next().unwrap_or('\0')))))
            }
            _ => None,
        };
        match ok {
            Some(kind) => Some(ir::Expr::new(kind, ty)),
            None => {
                let found = self.default_const(v.clone());
                let found_name = self.types.display(found.ty);
                let want = self.types.display(ty);
                let mut diag =
                    Diagnostic::error(codes::TYPE_MISMATCH, format!("expected `{want}`, found `{found_name}`"))
                        .primary(span, format!("this has type `{found_name}`"));
                if matches!(v, ConstValue::Float(_)) && matches!(kind, TyKind::Int(_)) {
                    diag = diag
                        .help(format!("floats don't convert to integers implicitly; use `.to({want})` to truncate"));
                }
                self.report(diag);
                None
            }
        }
    }
}

fn fold_binary(op: ast::BinOp, l: ConstValue, r: ConstValue) -> Option<ConstValue> {
    use ConstValue::*;
    use ast::BinOp as B;
    Some(match (l, r) {
        (Int(a), Int(b)) => match op {
            B::Add => Int(a.checked_add(b)?),
            B::Sub => Int(a.checked_sub(b)?),
            B::Mul => Int(a.checked_mul(b)?),
            B::Div => Int(a.checked_div(b)?),
            B::Rem => Int(a.checked_rem(b)?),
            B::Pow => Int(a.checked_pow(u32::try_from(b).ok()?)?),
            B::BitAnd => Int(a & b),
            B::BitOr => Int(a | b),
            B::BitXor => Int(a ^ b),
            B::Shl => Int(a.checked_shl(u32::try_from(b).ok()?)?),
            B::Shr => Int(a.checked_shr(u32::try_from(b).ok()?)?),
            B::Eq => Bool(a == b),
            B::Ne => Bool(a != b),
            B::Lt => Bool(a < b),
            B::Le => Bool(a <= b),
            B::Gt => Bool(a > b),
            B::Ge => Bool(a >= b),
            _ => return None,
        },
        (Float(a), Float(b)) => fold_float(op, a, b)?,
        (Int(a), Float(b)) => fold_float(op, a as f64, b)?,
        (Float(a), Int(b)) => fold_float(op, a, b as f64)?,
        (Bool(a), Bool(b)) => match op {
            B::And => Bool(a && b),
            B::Or => Bool(a || b),
            B::Eq => Bool(a == b),
            B::Ne => Bool(a != b),
            _ => return None,
        },
        (Str(a), Str(b)) => match op {
            B::Eq => Bool(a == b),
            B::Ne => Bool(a != b),
            _ => return None,
        },
        _ => return None,
    })
}

fn fold_float(op: ast::BinOp, a: f64, b: f64) -> Option<ConstValue> {
    use ConstValue::*;
    use ast::BinOp as B;
    Some(match op {
        B::Add => Float(a + b),
        B::Sub => Float(a - b),
        B::Mul => Float(a * b),
        B::Div => Float(a / b),
        B::Rem => Float(a % b),
        B::Pow => Float(a.powf(b)),
        B::Eq => Bool(a == b),
        B::Ne => Bool(a != b),
        B::Lt => Bool(a < b),
        B::Le => Bool(a <= b),
        B::Gt => Bool(a > b),
        B::Ge => Bool(a >= b),
        _ => return None,
    })
}

/// Suggests a conversion when a value of one numeric type is used as another.
/// `text` is the source of the value, used to decide whether it needs parentheses.
pub(crate) fn conversion_help(diag: Diagnostic, span: Span, text: &str, want: &str) -> Diagnostic {
    let edits = if is_simple_operand(text) {
        vec![wid_diagnostics::Edit { span: span.shrink_to_end(), replacement: format!(".to({want})") }]
    } else {
        vec![
            wid_diagnostics::Edit { span: span.shrink_to_start(), replacement: "(".into() },
            wid_diagnostics::Edit { span: span.shrink_to_end(), replacement: format!(").to({want})") },
        ]
    };
    diag.suggest(format!("convert it explicitly with `.to({want})`"), edits, Applicability::MaybeIncorrect)
}

/// Returns true when `text` can take a method call without parentheses:
/// names, literals, member chains and calls, but no top-level operators.
pub(crate) fn is_simple_operand(text: &str) -> bool {
    let mut depth = 0i32;
    let mut prev = ' ';
    for (i, c) in text.chars().enumerate() {
        match c {
            '(' | '[' => depth += 1,
            ')' | ']' => depth -= 1,
            '"' => return false,
            _ if depth > 0 => {}
            '?' | '!' if prev.is_alphanumeric() || prev == '_' => {}
            '-' if i == 0 => return false,
            c if c.is_alphanumeric() || c == '_' || c == '.' || c == '@' => {}
            _ => return false,
        }
        prev = c;
    }
    !text.is_empty()
}

/// A type's source text as a name suffix: `Vec2` → `vec2`, `[]U8` → `u8s`.
fn type_suffix(text: &str) -> String {
    let plural = text.starts_with('[');
    let mut out = String::new();
    for (i, c) in text.chars().filter(|c| c.is_alphanumeric()).enumerate() {
        if c.is_uppercase() && i > 0 && out.chars().last().is_some_and(|l| l.is_lowercase()) {
            out.push('_');
        }
        out.extend(c.to_lowercase());
    }
    if plural && !out.is_empty() {
        out.push('s');
    }
    out
}

/// The name an import binds when it has no `as:`: the last path segment,
/// like `physics` for `"./game/physics"` or `fmt` for `"core:fmt"`.
fn import_name(path: &str) -> String {
    let last = path.rsplit(['/', ':']).find(|s| !s.is_empty() && *s != "." && *s != "..").unwrap_or(path);
    let mut name: String = last.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if name.is_empty() || name.starts_with(|c: char| c.is_ascii_digit()) {
        name.insert(0, '_');
    }
    name
}

/// The namespace a `cimport` declares with `as:`, if it has one. Without
/// one, its declarations join the package's own namespace.
pub(crate) fn cimport_as(c: &ast::Cimport) -> Option<String> {
    c.options.iter().find_map(|option| match &option.value {
        ast::CimportValue::Expr(ast::Expr { kind: ast::ExprKind::Symbol(sym), .. }) if option.name.as_str() == "as" => {
            Some(sym.as_str().to_string())
        }
        _ => None,
    })
}

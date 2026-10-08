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
/// time, rather than plain folding of literals. `names_macro` says whether
/// a bare name is a macro, called without `()` like `X = five`.
fn needs_interpreter(e: &ast::Expr, names_macro: &impl Fn(&ast::Expr, Name) -> bool) -> bool {
    use ast::ExprKind as E;
    match &e.kind {
        E::Ident(name) => names_macro(e, *name),
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
        E::Paren(inner) | E::Unary { expr: inner, .. } => needs_interpreter(inner, names_macro),
        E::Binary { lhs, rhs, .. } => needs_interpreter(lhs, names_macro) || needs_interpreter(rhs, names_macro),
        _ => false,
    }
}

impl<'a> Checker<'a> {
    /// Whether a constant expression written at `loc` needs the
    /// interpreter (see [`needs_interpreter`]).
    fn const_needs_interpreter(&self, e: &ast::Expr, loc: DeclLoc) -> bool {
        needs_interpreter(e, &|ident, name| {
            // A macro's own code names macros where the macro is.
            let loc = self.virtual_file(ident.span.file).map_or(loc, |v| v.loc);
            self.lookup_pkg(loc.pkg, name).or_else(|| self.lookup_prelude(name)).is_some_and(|d| self.is_macro(d))
        })
    }

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
        self.resolve_pending();
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
                    let pending = super::comptime::PendingIf { loc, item: c, owner: None };
                    self.pending_decls.push(super::comptime::Pending::If(pending));
                }
                // The loader never saw it: its offsets are in a macro's file.
                ItemKind::Import(_) | ItemKind::Cimport(_) if self.generated_import(item) => {}
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

    /// Reports a `macro def` named like an operator (E0915): operators are
    /// methods of types, which the program runs, and a macro only expands
    /// where it is called by name.
    fn report_operator_macro(&mut self, f: &ast::FnDecl) {
        let op = f.name.as_str();
        let unary = f.params.len() == 1;
        let named = super::operator_name(op, unary).and_then(|n| n.strip_prefix("op_")).unwrap_or("expand");
        // Only a binary operator can also be a package-level `def`.
        let (usage, runs, keep) = match op {
            "[]" => ("a[i]".to_string(), "a method of `a`'s type", "in the operand's type"),
            "[]=" => ("a[i] = v".to_string(), "a method of `a`'s type", "in the operand's type"),
            _ if unary => (format!("{op}a"), "a method of `a`'s type", "in the operand's type"),
            _ => (
                format!("a {op} b"),
                "a method of `a`'s type, or a package-level `def`,",
                "in the left operand's type, or at package level",
            ),
        };
        self.report(
            Diagnostic::error(codes::OPERATOR_MACRO, format!("a macro can't be named like the operator `{op}`"))
                .primary(f.name.span, "operators are methods, not macros")
                .note(format!(
                    "`{usage}` calls {runs} when the program runs, while a macro only expands where it is called by name"
                ))
                .suggest_replace(
                    format!("give the macro a name and call it by that name, like `{named}(…)`"),
                    f.name.span,
                    named,
                    Applicability::MaybeIncorrect,
                )
                .help(format!("to keep the operator, define it with `def {op}` {keep}; its body may call a macro")),
        );
    }

    pub(super) fn collect_item(&mut self, item: &'a ast::Item, loc: DeclLoc, owner: Option<DeclId>) {
        self.check_item_attributes(item);
        let (name, span, kind) = match &item.kind {
            ItemKind::Def(f) if f.is_macro && owner.is_some() => {
                self.report(
                    Diagnostic::error(codes::UNEXPECTED_TOKEN, "a `macro def` belongs at the top level of a file")
                        .primary(f.name.span, "a macro inside a type")
                        .note("macros are package members, called like `name(…)` or `pkg.name(…)`")
                        .help("move the `macro def` out of this declaration"),
                );
                return;
            }
            ItemKind::Def(f) if f.is_macro && super::overloads::is_operator(f.name.as_str()) => {
                // Kept, so uses of the operator aren't reported again.
                self.report_operator_macro(f);
                (f.name.name, f.name.span, DeclKind::Fn(f))
            }
            ItemKind::Def(f) if owner.is_some() && f.params.is_empty() && matches!(f.name.as_str(), "-" | "~") => {
                (Name::new(&format!("{}@", f.name.as_str())), f.name.span, DeclKind::Fn(f))
            }
            ItemKind::Def(f) => (f.name.name, f.name.span, DeclKind::Fn(f)),
            ItemKind::Const(c) => (c.name.name, c.name.span, DeclKind::Const(c)),
            ItemKind::Struct(s) => (s.name.name, s.name.span, DeclKind::Struct(s)),
            ItemKind::Enum(e) => (e.name.name, e.name.span, DeclKind::Enum(e)),
            ItemKind::Union(u) => (u.name.name, u.name.span, DeclKind::Union(u)),
            ItemKind::Module(m) => (m.name.name, m.name.span, DeclKind::Module),
            ItemKind::Overload(o) => (o.name.name, o.name.span, DeclKind::Overload(o)),
            // Expanded once every declaration outside it is known.
            ItemKind::MacroCall(_) => {
                self.reject_call_attributes(item);
                let pending = super::decl_macros::PendingMacro { loc, item, owner };
                self.pending_decls.push(super::comptime::Pending::Macro(pending));
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
                    Some(DeclKind::Module) => self.report(
                        Diagnostic::error(codes::UNEXPECTED_TOKEN, "modules cannot declare fields")
                            .primary(f.name.span, "a module only holds methods and constants")
                            .help(format!(
                                "declare `{}` in each struct that includes the module; module methods read it with `@{}`",
                                f.name.as_str(),
                                f.name.as_str()
                            )),
                    ),
                    Some(other) => {
                        let what = other.a_describe();
                        let help = if matches!(other, DeclKind::Enum(_)) {
                            format!("enum values carry no data; keep `{}` in a struct next to the enum", f.name.as_str())
                        } else {
                            "`extend` adds methods only; declare the field in the struct itself".to_string()
                        };
                        self.report(
                            Diagnostic::error(
                                codes::UNEXPECTED_TOKEN,
                                format!("{what} cannot declare fields"),
                            )
                            .primary(f.name.span, "fields belong inside a `struct`")
                            .help(help),
                        );
                    }
                }
                return;
            }
            // Splices exist only inside `quote`, whose declarations are never
            // collected; the parser reports one anywhere else.
            ItemKind::Splice(_) => return,
            ItemKind::Include(t) => {
                match owner {
                    None => self.report(
                        Diagnostic::error(
                            codes::TOP_LEVEL_STATEMENT,
                            "`include` belongs inside a `struct`, `enum`, `module` or `extend`",
                        )
                        .primary(t.span, "nothing to include this into")
                        .help("move it into the body of the type that should get the module's methods"),
                    ),
                    Some(o) => {
                        self.include_items.entry(o).or_default().push(item);
                        // Added after the type's includes were resolved, by
                        // a `comptime if` or a macro.
                        if self.includes.contains_key(&o) {
                            self.resolve_include(o, item);
                        }
                    }
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
            ItemKind::Import(_) | ItemKind::Cimport(_) if self.generated_import(item) => return,
            ItemKind::Import(_) | ItemKind::Cimport(_) => {
                self.import_in_body(item, loc);
                return;
            }
            ItemKind::ComptimeIf(c) => {
                let pending = super::comptime::PendingIf { loc, item: c, owner };
                self.pending_decls.push(super::comptime::Pending::If(pending));
                return;
            }
            ItemKind::Error => return,
        };
        if let (Some(o), DeclKind::Struct(_) | DeclKind::Enum(_) | DeclKind::Union(_) | DeclKind::Module) =
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
        if let Some(o) = owner {
            self.check_member_after_fields(o, name, span);
        }
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
            DeclKind::Extend(_) | DeclKind::Module => Some(self.types.intern(TyKind::Param(Name::new("Self")))),
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
            // Parentheses group a type: `X = (Int)`.
            ast::ExprKind::Paren(inner) => self.is_type_alias_value(inner, loc, depth),
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
        let subst = self.template_subst(decl);
        let sig = self.resolve_sig(decl, subst);
        if f.is_static && self.owner_type(decl).is_none() {
            self.report(
                Diagnostic::error(codes::SELF_OUTSIDE_METHOD, "`def self.` only makes sense inside a type")
                    .primary(f.sig_span, "there is no type here for `self` to name")
                    .help("remove `self.` to declare a package-level method"),
            );
        }
        self.sigs.insert(decl, sig.clone());
        sig
    }

    /// Resolves the types of a function's signature with `subst` binding
    /// its generic parameters: placeholders for the template, or an
    /// instance's types and values.
    pub(super) fn resolve_sig(&mut self, decl: DeclId, subst: super::generics::Subst) -> FnSig {
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Fn(f) = d.kind else { unreachable!("resolve_sig on a non-function") };
        let owner_ty = self.owner_type(decl).map(|t| self.subst_type(t, &subst));
        let ctx = super::ty::TyCtx { loc: d.loc, self_ty: owner_ty, subst };
        let deferrals = self.value_deferrals;
        let mut params = Vec::new();
        for p in &f.params {
            let mut ty = self.resolve_type(&p.ty, &ctx);
            // A macro's `*names: T` collects its arguments into a `[]T`.
            if p.splat && f.is_macro {
                ty = self.types.slice(ty);
            }
            params.push(ParamSig { name: p.name.name, ty, span: p.span });
        }
        let block = f.block.as_ref().and_then(|b| self.block_sig(b, &ctx));
        let ret = match &f.ret {
            Some(t) => self.resolve_type(t, &ctx),
            None => self.types.void(),
        };
        let receiver = if f.is_static { None } else { owner_ty };
        let per_instance = self.value_deferrals > deferrals || self.extends_value_params(decl);
        FnSig { params, ret, receiver, block, c_variadic: f.c_variadic.is_some(), per_instance }
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
        // `BUF: [4]U8 = ---`: there are no package-level variables, and a
        // constant needs a value computed while compiling.
        if let ast::ExprKind::Uninit = c.value.kind {
            let name = d.name;
            let span = c.value.span;
            let why = "a constant's value is computed while compiling, and `---` only opts a variable out of zeroing";
            let note = match d.owner {
                Some(_) => why.to_string(),
                None => format!("Wid has no package-level variables, so `{name}` is a constant: {why}"),
            };
            let mut diag = Diagnostic::error(codes::NOT_A_VALUE, format!("the constant `{name}` needs a value"))
                .primary(span, "`---` leaves it without one")
                .note(note)
                .suggest_replace("give it the value it should hold", span, "…", Applicability::HasPlaceholders);
            if c.ty.is_some() {
                diag = diag.suggest_replace("or make it the zero value", span, "{}", Applicability::MaybeIncorrect);
            }
            self.report(diag.help("for state that changes, keep it in a struct that methods receive"));
            self.consts.insert(decl, ConstState::Failed);
            return None;
        }
        self.consts.insert(decl, ConstState::Resolving);
        self.const_stack.push(decl);
        let errors_before = self.diags.error_count();
        let ctx = super::ty::TyCtx { loc: d.loc, self_ty: None, subst: Default::default() };
        let declared = c.ty.as_ref().map(|t| self.resolve_type(t, &ctx));
        let folded = match self.fold_const_for(&c.value, d.loc, &[], declared) {
            // `Y: Int = 1 << 70` is named as 2^70 (reported).
            Some(v)
                if declared.is_some_and(|t| self.types.is_int(t))
                    && self.shift_overflows(&c.value, &v, d.loc, declared) =>
            {
                None
            }
            folded => folded,
        };
        let value = match folded {
            Some(v) => Some(v),
            None if self.const_needs_interpreter(&c.value, d.loc) => {
                self.interpret_const(&c.value, declared, d.loc).map(ConstValue::Typed)
            }
            // Names, or arithmetic on them, that didn't fold: `X = Foo`
            // with `Foo` undefined, `B = A` with `A` a struct constant, or
            // `D: Dir = :north`. Checked as code, an undefined name is E0201
            // with its did-you-mean, and the rest evaluates like `comptime`.
            // A type alias (`Vec2 = [2]F32`) is resolved as a type instead.
            None if self.diags.error_count() == errors_before
                && !self.is_type_alias_value(&c.value, d.loc, 0)
                && !super::runtime::holds_parse_error(&c.value) =>
            {
                let code = super::comptime::ComptimeCode::Expr(&c.value);
                self.comptime_value(code, declared, d.loc, c.value.span, false).map(ConstValue::Typed)
            }
            None => None,
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
                if !already_reported
                    && !alias
                    && !matches!(c.value.kind, ast::ExprKind::Type(_))
                    && !super::runtime::holds_parse_error(&c.value)
                {
                    self.report(
                        Diagnostic::error(codes::COMPTIME_ONLY, "constant value is not known at compile time")
                            .primary(c.value.span, "the compiler can't compute this value while compiling")
                            .note(
                                "a constant's value is computed at compile time: literals, other constants, \
                                 struct literals, or a method call with `comptime`",
                            ),
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
        self.eval_const_in(expr, loc, &[])
    }

    /// Like [`Checker::eval_const`], where the generic value parameters
    /// bound in `subst` (`N` in `Pool(Ball, 64)`) are constants too.
    pub fn eval_const_in(&mut self, expr: &ast::Expr, loc: DeclLoc, subst: &[(Name, TyId)]) -> Option<ConstValue> {
        if let Some(v) = self.fold_const_in(expr, loc, subst) {
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
            _ if self.const_needs_interpreter(expr, loc) => {
                self.comptime_value(ComptimeCode::Expr(expr), expected, loc, expr.span, false)
            }
            _ => None,
        }
    }

    /// Folds literal arithmetic on constants, returning `None` when the
    /// expression is anything else.
    pub fn fold_const(&mut self, expr: &ast::Expr, loc: DeclLoc) -> Option<ConstValue> {
        self.fold_const_in(expr, loc, &[])
    }

    /// Like [`Checker::fold_const`], where the generic value parameters
    /// bound in `subst` are constants too.
    pub fn fold_const_in(&mut self, expr: &ast::Expr, loc: DeclLoc, subst: &[(Name, TyId)]) -> Option<ConstValue> {
        self.fold_const_for(expr, loc, subst, None)
    }

    /// Like [`Checker::fold_const_in`], for a value of type `target`, which
    /// an error names (`Int` when it is `None`). A shift whose result
    /// doesn't fit in the 128 bits constants are folded in, like
    /// `1 << 200`, is reported (E0311) and folds to 0, so nothing else
    /// reports it again.
    pub fn fold_const_for(
        &mut self,
        expr: &ast::Expr,
        loc: DeclLoc,
        subst: &[(Name, TyId)],
        target: Option<TyId>,
    ) -> Option<ConstValue> {
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
            E::Paren(inner) => self.fold_const_for(inner, loc, subst, target)?,
            E::Const(name) => {
                // A value parameter, like `N` in a method of `Pool(Ball, 64)`.
                if let Some(t) = super::generics::lookup(subst, *name)
                    && let TyKind::ConstValue(v) = self.types.kind(t)
                {
                    return Some(ConstValue::Int(*v));
                }
                // A macro's own code names constants where the macro is.
                let loc = self.virtual_file(expr.span.file).map_or(loc, |v| v.loc);
                let decl = self.lookup_pkg(loc.pkg, *name)?;
                match self.decls[decl.0 as usize].kind {
                    DeclKind::Const(_) => {
                        self.note_ref(expr.span, decl, crate::uses::RefKind::Read);
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
                let v = self.fold_const_for(inner, loc, subst, target)?;
                match (op, v) {
                    // `-(-2^127)` is past the 128 bits constants fold in.
                    (ast::UnOp::Neg, ConstValue::Int(i)) => match i.checked_neg() {
                        Some(n) => ConstValue::Int(n),
                        None => return self.int_overflow(expr.span, Past::Exactly(power_of_two(1, 127)), target),
                    },
                    (ast::UnOp::Neg, ConstValue::Float(f)) => ConstValue::Float(-f),
                    (ast::UnOp::Not, ConstValue::Bool(b)) => ConstValue::Bool(!b),
                    (ast::UnOp::BitNot, ConstValue::Int(i)) => ConstValue::Int(!i),
                    _ => return None,
                }
            }
            E::Binary { op, lhs, rhs } => {
                // Operands of a comparison have a type of their own, and a
                // shift's amount has nothing to do with its value's.
                let operand = if op.is_comparison() { None } else { target };
                let l = self.fold_const_for(lhs, loc, subst, operand)?;
                let amount = if *op == ast::BinOp::Shl { None } else { operand };
                let r = self.fold_const_for(rhs, loc, subst, amount)?;
                if matches!(op, ast::BinOp::Shl | ast::BinOp::Shr)
                    && let (ConstValue::Int(_), ConstValue::Int(b)) = (&l, &r)
                    && *b < 0
                {
                    self.report_negative_shift(*op, lhs.span, rhs.span, *b);
                    return Some(ConstValue::Int(0));
                }
                if *op == ast::BinOp::Shl
                    && let (ConstValue::Int(a), ConstValue::Int(b)) = (&l, &r)
                    && shift_value(*a, *b) == Some(None)
                {
                    self.report_shift_overflow(expr.span, *a, *b, target);
                    return Some(ConstValue::Int(0));
                }
                if let (ConstValue::Int(a), ConstValue::Int(b)) = (&l, &r)
                    && let Some(past) = past_128_bits(*op, *a, *b)
                {
                    return self.int_overflow(expr.span, past, target);
                }
                fold_binary(*op, l, r)?
            }
            _ => return None,
        })
    }

    /// Reports a constant shift, `expr` (in parentheses or not), whose
    /// folded value `v` doesn't fit in `target` (E0311), naming the value
    /// as a power of two. Returns whether it did. Untyped constant
    /// arithmetic may go past every integer type in between, so this
    /// checks where the value gets its type.
    pub(super) fn shift_overflows(
        &mut self,
        expr: &ast::Expr,
        v: &ConstValue,
        loc: DeclLoc,
        target: Option<TyId>,
    ) -> bool {
        let mut e = expr;
        while let ast::ExprKind::Paren(inner) = &e.kind {
            e = inner;
        }
        let ast::ExprKind::Binary { op: ast::BinOp::Shl, lhs, rhs } = &e.kind else { return false };
        if !matches!(v, ConstValue::Int(_)) {
            return false;
        }
        match (self.fold_const(lhs, loc), self.fold_const(rhs, loc)) {
            (Some(ConstValue::Int(a)), Some(ConstValue::Int(b))) => self.report_shift_overflow(e.span, a, b, target),
            _ => false,
        }
    }

    /// Reports `a << b` at `span` when its value doesn't fit in `target`,
    /// or in `Int` when that isn't an integer type (E0311), and returns
    /// whether it did.
    fn report_shift_overflow(&mut self, span: Span, a: i128, b: i128, target: Option<TyId>) -> bool {
        use crate::types::IntTy;
        let Some(value) = shift_value(a, b) else { return false };
        let it = match target.map(|t| self.types.kind(self.types.base(t))) {
            Some(TyKind::Int(it)) => *it,
            _ => IntTy::Int,
        };
        let (name, (lo, hi)) = (it.name(), it.range());
        if value.is_some_and(|v| lo <= v && v <= hi) {
            return false;
        }
        let text = self.source_text(span);
        let shown = power_of_two(a, b);
        let mut diag =
            Diagnostic::error(codes::CONSTANT_OVERFLOW, format!("`{text}` is {shown}, which doesn't fit in `{name}`"))
                .primary(span, format!("`{name}` holds values from {lo} to {hi}"));
        diag = match max_shift(a, lo, hi) {
            Some(k) => diag.help(format!("the largest shift of `{a}` that fits in `{name}` is `{a} << {k}`")),
            None => diag.help("use a value in range"),
        };
        // A type that holds the value, of the same signedness if one does.
        let wider = value.and_then(|v| {
            let holds = |t: &IntTy| {
                let (l, h) = t.range();
                l <= v && v <= h && !matches!(t, IntTy::Int | IntTy::UInt)
            };
            let same = IntTy::ALL.into_iter().find(|t| holds(t) && (t.range().0 < 0) == (lo < 0));
            same.or_else(|| IntTy::ALL.into_iter().find(holds))
        });
        if let Some(w) = wider {
            diag = diag.help(format!("or give the value a type that holds it, like `{}`", w.name()));
        }
        self.report(diag);
        true
    }

    /// Reports a shift by a constant negative `amount`, the value of the
    /// operand at `rhs` (E0311), with a fix that shifts the other way, as
    /// Ruby reads it: `1 << -2` is `1 >> 2`.
    pub(super) fn report_negative_shift(&mut self, op: ast::BinOp, lhs: Span, rhs: Span, amount: i128) {
        let other = if op == ast::BinOp::Shl { ">>" } else { "<<" };
        // `x <<= -1` is fixed as `x >>= 1`.
        let assign = if self.source_text(Span::new(lhs.file, lhs.end, rhs.start)).contains('=') { "=" } else { "" };
        let text = self.source_text(lhs.to(rhs));
        let fixed = format!("{other}{assign} {}", amount.unsigned_abs());
        self.report(
            Diagnostic::error(codes::CONSTANT_OVERFLOW, format!("`{text}` shifts by a negative amount"))
                .primary(rhs, format!("this is {amount}"))
                .note("a shift amount counts bit positions, so it can't be negative")
                .suggest(
                    format!("to shift the other way, write `{fixed}`"),
                    vec![wid_diagnostics::Edit {
                        span: Span::new(lhs.file, lhs.end, rhs.end),
                        replacement: format!(" {fixed}"),
                    }],
                    Applicability::MaybeIncorrect,
                ),
        );
    }

    /// Reports a constant integer operation at `span` whose exact value is
    /// past the 128 bits constants are folded in, like `2 ** 200` (E0311),
    /// and folds it to 0 so nothing reports it again. No integer type
    /// holds such a value, so the error doesn't depend on `target`; for a
    /// float `target` it isn't folded, and the interpreter computes it in
    /// floating point.
    fn int_overflow(&mut self, span: Span, past: Past, target: Option<TyId>) -> Option<ConstValue> {
        if target.is_some_and(|t| self.types.is_float(t)) {
            return None;
        }
        let shown = match past {
            Past::Exactly(v) => v,
            Past::Above => "at least 2^127".to_string(),
            Past::Below => "less than -2^127".to_string(),
        };
        let (lo, _) = crate::types::IntTy::I64.range();
        let (_, hi) = crate::types::IntTy::U64.range();
        let text = self.source_text(span);
        self.report(
            Diagnostic::error(
                codes::CONSTANT_OVERFLOW,
                format!("`{text}` is {shown}, which doesn't fit in any integer type"),
            )
            .primary(span, format!("integer types hold values from {lo} to {hi}"))
            .help("use a value in range; a float type, like `F64`, holds larger values"),
        );
        Some(ConstValue::Int(0))
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
            if self.optional_inner(ty) == Some(e.ty) {
                return Some(self.opt_some(e, ty));
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
        // Where a `T?` is expected, an untyped constant is a `T`, then a `T?`.
        let target = self.optional_inner(ty).unwrap_or(ty);
        let base = self.types.base(target);
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
            Some(kind) if target == ty => Some(ir::Expr::new(kind, ty)),
            Some(kind) => Some(self.opt_some(ir::Expr::new(kind, target), ty)),
            None => {
                let found = self.default_const(v.clone());
                let found_name = self.types.display(found.ty);
                let want = self.types.display(ty);
                let mut diag =
                    Diagnostic::error(codes::TYPE_MISMATCH, format!("expected `{want}`, found `{found_name}`"))
                        .primary(span, format!("this has type `{found_name}`"));
                if matches!(v, ConstValue::Float(_)) && matches!(kind, TyKind::Int(_)) {
                    let target = self.types.display(target);
                    diag = diag
                        .help(format!("floats don't convert to integers implicitly; use `.to({target})` to truncate"));
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
            // `-2^127 % -1` is 0, as at run time.
            B::Rem if b == -1 => Int(0),
            B::Rem => Int(a.checked_rem(b)?),
            B::Pow => Int(a.checked_pow(u32::try_from(b).ok()?)?),
            B::BitAnd => Int(a & b),
            B::BitOr => Int(a | b),
            B::BitXor => Int(a ^ b),
            B::Shl => Int(shift_value(a, b)??),
            B::Shr => match u32::try_from(b).ok()? {
                b if b < 128 => Int(a >> b),
                _ => Int(if a < 0 { -1 } else { 0 }),
            },
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

/// The value of a constant `a << b`: `None` for a negative amount, which
/// isn't folded, and `Some(None)` when the value doesn't fit in the 128
/// bits constants are folded in, like `1 << 200`.
fn shift_value(a: i128, b: i128) -> Option<Option<i128>> {
    let b = u32::try_from(b).ok()?;
    Some(match a {
        0 => Some(0),
        _ if b < 128 && a.wrapping_shl(b) >> b == a => Some(a << b),
        _ => None,
    })
}

/// Where the exact value of a constant integer operation lies when it is
/// past the 128 bits constants are folded in.
enum Past {
    /// A value known exactly, like `2^200`.
    Exactly(String),
    /// At least 2^127.
    Above,
    /// Less than -2^127.
    Below,
}

/// Where the value of a constant `a op b` lies when it is past the 128 bits
/// constants are folded in, like `2 ** 200`; `None` when it isn't. A
/// shift is checked by [`shift_value`].
fn past_128_bits(op: ast::BinOp, a: i128, b: i128) -> Option<Past> {
    use ast::BinOp as B;
    let negative = match op {
        B::Add if a.checked_add(b).is_none() => a < 0,
        B::Sub if a.checked_sub(b).is_none() => a < 0,
        B::Mul if a.checked_mul(b).is_none() => (a < 0) != (b < 0),
        // `-2^127 / -1`
        B::Div if a == i128::MIN && b == -1 => return Some(Past::Exactly(power_of_two(1, 127))),
        B::Pow
            if b >= 0 && a.unsigned_abs() >= 2 && u32::try_from(b).ok().is_none_or(|e| a.checked_pow(e).is_none()) =>
        {
            let negative = a < 0 && b % 2 == 1;
            // `2 ** 200` is 2^200, and `(-4) ** 101` is -2^202.
            if a.unsigned_abs().is_power_of_two()
                && let Some(exp) = b.checked_mul(i128::from(a.unsigned_abs().trailing_zeros()))
            {
                let sign = if negative { "-" } else { "" };
                return Some(Past::Exactly(format!("{sign}2^{exp}")));
            }
            negative
        }
        _ => return None,
    };
    Some(if negative { Past::Below } else { Past::Above })
}

/// `a << b` as a power of two, like `2^200`, `-2^64` or `3 * 2^130`.
fn power_of_two(a: i128, b: i128) -> String {
    let sign = if a < 0 { "-" } else { "" };
    // `4 << 200` is 2^202.
    let zeros = a.unsigned_abs().trailing_zeros();
    let odd = a.unsigned_abs() >> zeros;
    let exp = b.saturating_add(i128::from(zeros));
    match odd {
        1 => format!("{sign}2^{exp}"),
        _ => format!("{sign}{odd} * 2^{exp}"),
    }
}

/// The largest `k` for which `a << k` is within `lo..=hi`, if `a` is.
fn max_shift(a: i128, lo: i128, hi: i128) -> Option<u32> {
    if a == 0 || a < lo || a > hi {
        return None;
    }
    (0..127u32).take_while(|&k| a.checked_mul(1i128 << k).is_some_and(|v| lo <= v && v <= hi)).last()
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
pub(super) fn import_name(path: &str) -> String {
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

//! Macro calls among declarations: at package level and in `struct`, `enum`,
//! `module` and `extend` bodies.
//!
//! `collect_item` queues each call as a [`PendingMacro`], with the pending
//! `comptime if`s, and `resolve_pending` expands them in source order once
//! every declaration outside them is known ([`Checker::expand_item_macro`]):
//!
//! 1. the call resolves by the names of the file it is written in (for code
//!    a macro generated, the macro's file), qualified ones (`lib.name args`)
//!    through that file's imports. A name that isn't a macro is a statement
//!    outside a method (E0108);
//! 2. the macro runs through the expansion core (`Checker::expand_code`) in
//!    a frame of its own. When the macro's own code or the call's arguments
//!    use `Self`, the frame's `Self` is the type whose body holds the call
//!    (`owner_self`): there the macro runs as the instance for that type;
//! 3. the code it returns becomes declarations ([`lines_to_items`]), which
//!    go through `collect_item` like written ones. A struct's fields and an
//!    enum's members are fixed before its macros run, so a generated field
//!    in a struct body is E0913; so is a generated `import` or `cimport`
//!    anywhere, since packages are loaded before any macro runs
//!    (`Checker::generated_import`). Calls among the generated declarations
//!    are queued again, so nested expansions count against the budgets like
//!    the ones in code.

use wid_diagnostics::{Applicability, Diagnostic, Span, codes, did_you_mean};
use wid_syntax::Name;
use wid_syntax::ast::{self, ExprKind as E, Ident, ItemKind, StmtKind};

use super::body::{Frame, Scope};
use super::expr::BUILTINS;
use super::macros::{MacroCall, self_in};
use super::{Checker, DeclId, DeclKind, DeclLoc};
use crate::types::{TyId, TyKind};

/// A macro call among declarations, waiting for every declaration outside
/// it to be known.
#[derive(Clone, Copy)]
pub(crate) struct PendingMacro<'a> {
    /// The package and file whose declarations the call adds to.
    pub loc: DeclLoc,
    /// The call, an [`ItemKind::MacroCall`].
    pub item: &'a ast::Item,
    /// The struct, enum, module or `extend` whose body holds the call.
    pub owner: Option<DeclId>,
}

/// The parts of a call among declarations.
struct ItemCall<'e> {
    /// `lib` in `lib.name args`.
    pkg: Option<Ident>,
    name: Ident,
    args: &'e [ast::Arg],
    block: Option<&'e ast::BlockArg>,
}

/// Reads a call among declarations: `name`, `name args`, `name(args)`,
/// `lib.name`, `lib.name args` or `lib.name(args)`.
fn item_call(e: &ast::Expr) -> Option<ItemCall<'_>> {
    match &e.kind {
        E::Ident(n) => Some(ItemCall { pkg: None, name: Ident { name: *n, span: e.span }, args: &[], block: None }),
        E::Call(c) => match &c.callee {
            ast::Callee::Name(n) => Some(ItemCall { pkg: None, name: *n, args: &c.args, block: c.block.as_ref() }),
            ast::Callee::Method { recv: ast::Expr { kind: E::Ident(p), span }, name, safe: false } => {
                let pkg = Some(Ident { name: *p, span: *span });
                Some(ItemCall { pkg, name: *name, args: &c.args, block: c.block.as_ref() })
            }
            ast::Callee::Method { .. } => None,
        },
        E::Member { recv, name, safe: false } => match recv.kind {
            E::Ident(p) => {
                let pkg = Some(Ident { name: p, span: recv.span });
                Some(ItemCall { pkg, name: *name, args: &[], block: None })
            }
            _ => None,
        },
        _ => None,
    }
}

/// Turns the lines of generated code into declarations, for code that
/// lands among declarations. A `StmtKind::Item` line gives its declaration;
/// a call or a name (`other_macro :x`, `lib.make`) a macro call; a constant
/// (`NAME = v`, also with a spliced name, and `NAME: T = v`) a constant;
/// `name: T` a field; a `comptime if` a declaration-level `comptime if`
/// whose branches follow the same rules. Lines that can only be statements
/// go to `statements`.
pub(crate) fn lines_to_items(stmts: Vec<ast::Stmt>, statements: &mut Vec<ast::Stmt>) -> Vec<ast::Item> {
    let mut items = Vec::with_capacity(stmts.len());
    for stmt in stmts {
        let item = |kind: ItemKind, attrs: Vec<ast::Attribute>| ast::Item {
            kind,
            span: stmt.span,
            attrs,
            private: false,
            doc: None,
        };
        match stmt.kind {
            StmtKind::Item(item) => items.push(*item),
            StmtKind::Expr(e) if item_call(&e).is_some() => {
                items.push(item(ItemKind::MacroCall(Box::new(e)), stmt.attrs));
            }
            StmtKind::Expr(ast::Expr { kind: E::ComptimeIf(if_expr), span }) => {
                match comptime_if_item(if_expr, statements) {
                    Ok(c) => items.push(item(ItemKind::ComptimeIf(Box::new(c)), stmt.attrs)),
                    Err(if_expr) => statements.push(ast::Stmt {
                        kind: StmtKind::Expr(ast::Expr { kind: E::ComptimeIf(if_expr), span }),
                        span: stmt.span,
                        attrs: stmt.attrs,
                    }),
                }
            }
            StmtKind::Decl { mut names, ty, value, uninit: false } if names.len() == 1 => {
                let name = names.remove(0);
                let kind = match value {
                    Some(value) if name.as_str().starts_with(char::is_uppercase) => {
                        ItemKind::Const(Box::new(ast::ConstDecl { name, ty: Some(ty), value }))
                    }
                    default => ItemKind::Field(Box::new(ast::FieldDecl { name, ty, default, using: false })),
                };
                items.push(item(kind, stmt.attrs));
            }
            StmtKind::Assign { mut targets, op: None, mut values }
                if targets.len() == 1 && values.len() == 1 && matches!(targets[0].kind, E::Const(_)) =>
            {
                let target = targets.remove(0);
                let E::Const(name) = target.kind else { continue };
                let name = Ident { name, span: target.span };
                let value = values.remove(0);
                items.push(item(ItemKind::Const(Box::new(ast::ConstDecl { name, ty: None, value })), stmt.attrs));
            }
            // A parse error, already reported.
            StmtKind::Error => {}
            kind => statements.push(ast::Stmt { kind, span: stmt.span, attrs: stmt.attrs }),
        }
    }
    items
}

/// A `comptime if` among generated lines as a declaration-level one: its
/// branches become declarations, and `elsif`s nest in the `else` branch.
/// One that binds a variable stays a statement.
fn comptime_if_item(
    if_expr: Box<ast::IfExpr>,
    statements: &mut Vec<ast::Stmt>,
) -> Result<ast::ComptimeIfItem, Box<ast::IfExpr>> {
    let binds = |c: &ast::Cond| matches!(c, ast::Cond::Bind { .. });
    if if_expr.unless || binds(&if_expr.cond) || if_expr.elifs.iter().any(|(c, _)| binds(c)) {
        return Err(if_expr);
    }
    let cond_expr = |c: ast::Cond| match c {
        ast::Cond::Expr(e) => e,
        ast::Cond::Bind { value, .. } => value,
    };
    let if_expr = *if_expr;
    let then = lines_to_items(if_expr.then, statements);
    let mut else_ = if_expr.else_.map(|b| lines_to_items(b, statements)).unwrap_or_default();
    for (cond, body) in if_expr.elifs.into_iter().rev() {
        let cond = cond_expr(cond);
        let span = cond.span;
        let then = lines_to_items(body, statements);
        let nested = ast::ComptimeIfItem { cond, then, else_ };
        else_ = vec![ast::Item {
            kind: ItemKind::ComptimeIf(Box::new(nested)),
            span,
            attrs: Vec::new(),
            private: false,
            doc: None,
        }];
    }
    Ok(ast::ComptimeIfItem { cond: cond_expr(if_expr.cond), then, else_ })
}

/// Marks a generated declaration `private`, with the declarations of its
/// `comptime if` branches, for a call written `private m …`.
fn make_private(item: &mut ast::Item) {
    item.private = true;
    if let ItemKind::ComptimeIf(c) = &mut item.kind {
        for item in c.then.iter_mut().chain(c.else_.iter_mut()) {
            make_private(item);
        }
    }
}

impl<'a> Checker<'a> {
    /// Expands a macro call among declarations and collects the
    /// declarations it generates.
    pub(super) fn expand_item_macro(&mut self, p: PendingMacro<'a>) {
        let ItemKind::MacroCall(expr) = &p.item.kind else { return };
        let Some(call) = self.resolve_item_macro(expr, p) else { return };
        let Some(code) = self.expand_among_declarations(&call, p) else {
            self.failed_among_declarations(p);
            return;
        };
        let items = self.declarations_from(code, &call.shown, p);
        let stmts: Vec<ast::Stmt> = items
            .into_iter()
            .map(|item| ast::Stmt { span: item.span, kind: StmtKind::Item(Box::new(item)), attrs: Vec::new() })
            .collect();
        let generated: &'a [ast::Stmt] = self.generated.alloc(stmts).as_slice();
        for stmt in generated {
            if let StmtKind::Item(item) = &stmt.kind {
                self.collect_item(item, p.loc, p.owner);
            }
        }
    }

    /// Records that a macro call among declarations failed to expand or
    /// named no macro. What it would have generated is unknown, so names
    /// and members missing because of it aren't reported: anywhere in the
    /// package for a call at package level, and on the type (or the types
    /// including the module, or extended) for a call in a body.
    fn failed_among_declarations(&mut self, p: PendingMacro<'a>) {
        match p.owner {
            Some(owner) => self.macros.failed_owners.insert(owner),
            None => self.macros.failed_packages.insert(p.loc.pkg),
        };
    }

    /// Whether a macro call failed among declarations that add members to
    /// `ty` (see [`Checker::failed_among_declarations`]): in the body of its
    /// struct or enum, of a module it includes, or of an `extend` of it. A
    /// member missing on `ty` may be one the call would have generated.
    pub(super) fn members_incomplete(&mut self, ty: TyId) -> bool {
        if self.macros.failed_owners.is_empty() {
            return false;
        }
        if let Some(decl) = self.type_decl(ty)
            && self.owner_failed(decl)
        {
            return true;
        }
        let extends = self.extends.clone();
        for ext in extends {
            if !self.owner_failed(ext) {
                continue;
            }
            for pattern in self.extend_targets(ext) {
                let mut bindings = vec![(Name::new("Self"), ty)];
                if self.unify(pattern, ty, &mut bindings) {
                    return true;
                }
            }
        }
        false
    }

    /// Whether `name` is a field that a macro called in the body of `ty`'s
    /// struct generated and E0913 rejected. That error explains its uses
    /// (`x.name`, `@name`, `T.new(name: …)`), which aren't reported missing.
    pub(super) fn field_rejected(&self, ty: TyId, name: Name) -> bool {
        !self.macros.rejected_fields.is_empty()
            && self.type_decl(ty).is_some_and(|d| self.macros.rejected_fields.contains(&(d, name)))
    }

    /// Whether a macro call failed in the body of `owner` or of a module it
    /// includes, directly or through other modules.
    pub(super) fn owner_failed(&mut self, owner: DeclId) -> bool {
        let mut pending = vec![owner];
        let mut seen = Vec::new();
        while let Some(d) = pending.pop() {
            if self.macros.failed_owners.contains(&d) {
                return true;
            }
            seen.push(d);
            let modules = self.includes_of(d);
            pending.extend(modules.into_iter().filter(|m| !seen.contains(m)));
        }
        false
    }

    /// Where the names of code written at `span` resolve: the macro's file
    /// for code a macro generated, else the file the declarations are in.
    fn names_at(&self, span: Span, fallback: DeclLoc) -> DeclLoc {
        self.virtual_file(span.file).map_or(fallback, |v| v.loc)
    }

    /// Finds the macro a call among declarations names, reporting a name
    /// that isn't one.
    fn resolve_item_macro(&mut self, expr: &'a ast::Expr, p: PendingMacro<'a>) -> Option<MacroCall<'a>> {
        let Some(parts) = item_call(expr) else {
            self.call_among_declarations(expr.span, p.owner, None);
            return None;
        };
        let loc = self.names_at(parts.name.span, p.loc);
        let (found, shown, scope) = match parts.pkg {
            None => {
                let found = self.lookup_pkg(loc.pkg, parts.name.name).or_else(|| self.lookup_prelude(parts.name.name));
                (found, parts.name.as_str().to_string(), None)
            }
            Some(pkg) => {
                let Some(target) = self.lookup_import(loc, pkg.name) else {
                    if !self.import_failed(loc, pkg.name) {
                        let imports: Vec<&'static str> = self
                            .file_imports
                            .get(&(loc.pkg, loc.file))
                            .map(|m| m.keys().map(|n| n.as_str()).collect())
                            .unwrap_or_default();
                        self.undefined_here(pkg.name, pkg.span, imports, "package");
                    }
                    self.failed_among_declarations(p);
                    return None;
                };
                let shown = format!("{}.{}", pkg.name, parts.name.name);
                (self.lookup_pkg(target, parts.name.name), shown, Some(target))
            }
        };
        match found {
            Some(decl) if self.is_macro(decl) => {
                self.check_visible(decl, parts.name.span);
                let (args, block, name_span, span) = (parts.args, parts.block, parts.name.span, expr.span);
                Some(MacroCall { decl, shown, args, block, name_span, span })
            }
            Some(decl) => {
                self.call_among_declarations(expr.span, p.owner, Some(decl));
                None
            }
            None if scope.is_none() && BUILTINS.contains(&parts.name.as_str()) => {
                self.call_among_declarations(expr.span, p.owner, None);
                None
            }
            None => {
                // A call that failed before this one isn't taken to explain
                // it: generating a macro for later calls is rare, and an
                // undefined macro is itself the error to fix.
                let explained = match scope {
                    None => self.failed_merges.contains(&loc.pkg),
                    Some(target) => self.report_not_imported(target, parts.name.name, parts.name.span),
                };
                if !explained {
                    let bare = matches!(expr.kind, E::Ident(_));
                    self.undefined_macro(&parts, &shown, scope.unwrap_or(loc.pkg), scope.is_none(), bare, p.owner);
                }
                self.failed_among_declarations(p);
                None
            }
        }
    }

    /// Reports an undefined name like `Checker::undefined`, for code with
    /// no method around it.
    fn undefined_here(&mut self, name: Name, span: Span, candidates: Vec<&'static str>, what: &str) {
        let mut diag = Diagnostic::error(codes::UNDEFINED_NAME, format!("undefined {what} `{name}`"))
            .primary(span, "not found in this scope");
        if let Some(best) = did_you_mean(name.as_str(), candidates.iter().copied()) {
            diag = diag.suggest_replace(
                format!("a similar name exists: `{best}`"),
                span,
                best,
                Applicability::MaybeIncorrect,
            );
        }
        self.report(diag);
    }

    /// Reports a call among declarations whose name no macro has (E0201).
    fn undefined_macro(
        &mut self,
        parts: &ItemCall<'_>,
        shown: &str,
        pkg: crate::input::PackageId,
        with_prelude: bool,
        bare: bool,
        owner: Option<DeclId>,
    ) {
        let name = parts.name.as_str();
        let mut macros: Vec<&'static str> = self.pkg_scopes[pkg.0 as usize]
            .iter()
            .filter(|(_, d)| self.is_macro(**d) && (with_prelude || !self.decls[d.0 as usize].private))
            .map(|(n, _)| n.as_str())
            .collect();
        if with_prelude && let Some(prelude) = self.input.prelude {
            macros.extend(
                self.pkg_scopes[prelude.0 as usize]
                    .iter()
                    .filter(|(_, d)| self.is_macro(**d) && !self.decls[d.0 as usize].private)
                    .map(|(n, _)| n.as_str()),
            );
        }
        macros.sort_unstable();
        let label = match parts.pkg {
            Some(pkg) => format!("`{}` has no macro with this name", pkg.name),
            None => "no macro with this name is in scope".to_string(),
        };
        let mut diag = Diagnostic::error(codes::UNDEFINED_NAME, format!("undefined macro `{shown}`"))
            .primary(parts.name.span, label)
            .note("a call among declarations runs a macro, and the declarations it generates take the call's place");
        let in_enum = owner.is_some_and(|o| matches!(self.decls[o.0 as usize].kind, DeclKind::Enum(_)));
        if matches!(name, "attr_reader" | "attr_writer" | "attr_accessor") && parts.pkg.is_none() {
            // Ruby's accessors: Wid's fields are public instead.
            let field = parts.args.iter().find_map(|a| match a.value.kind {
                E::Symbol(s) => Some(s.as_str()),
                _ => None,
            });
            let field = field.unwrap_or("hp");
            let set = if name == "attr_writer" { " = …" } else { "" };
            diag = diag
                .note("Wid has no accessor macros: fields are always public, and code reads and writes them directly")
                .help(format!(
                    "remove the call and use the field itself, like `hero.{field}{set}` or `@{field}{set}` in a method"
                ));
        } else if in_enum && bare {
            diag = diag
                .note("an enum's members are fixed before the macros in its body run, so a macro can't add members")
                .help("list the member in the enum itself, or have a macro called at package level generate the whole `enum`");
        } else if let Some(best) = did_you_mean(name, macros.iter().copied()) {
            diag = diag.suggest_replace(
                format!("a macro with a similar name exists: `{best}`"),
                parts.name.span,
                best,
                Applicability::MaybeIncorrect,
            );
        }
        self.report(diag);
    }

    /// Reports a call among declarations of something that isn't a macro: a
    /// statement outside a method (E0108).
    fn call_among_declarations(&mut self, span: Span, owner: Option<DeclId>, callee: Option<DeclId>) {
        let mut diag = Diagnostic::error(codes::TOP_LEVEL_STATEMENT, "statements must be inside a method")
            .primary(span, "this call is outside any `def`");
        if let Some(d) = callee {
            let d = &self.decls[d.0 as usize];
            let what = d.kind.a_describe();
            diag = diag
                .note(format!("`{}` is {what}, not a macro; only a macro call can stand among declarations", d.name));
        }
        // A macro generated it.
        let generated_by = self
            .virtual_file(span.file)
            .and_then(|v| self.macros.expansions.get(v.expansion as usize))
            .map(|e| e.name.clone());
        diag = match (generated_by, owner) {
            (Some(m), _) => {
                diag.help(format!("put the call inside a `def` in the `quote`, or call `{m}` inside a method"))
            }
            (None, None) => diag.help("move it into `def main … end`, which runs when the program starts"),
            (None, Some(_)) => diag.help("move it into a method of this type"),
        };
        self.report(diag);
    }

    /// Runs the macro of a call among declarations and builds its code, in
    /// a frame whose `Self` is the type whose body holds the call when the
    /// macro or the arguments use `Self`.
    fn expand_among_declarations(&mut self, call: &MacroCall<'a>, p: PendingMacro<'a>) -> Option<Vec<ast::Stmt>> {
        let in_args = call.args.iter().find_map(|a| self_in(&a.value));
        let needs_self = in_args.is_some() || self.macro_self_use(call.decl).is_some();
        let self_ty = if needs_self { self.owner_self(p.owner) } else { None };
        if let Some(at) = in_args
            && self_ty.is_none_or(|t| self.has_params(t))
        {
            self.report_no_self(&call.shown, call.span, at, self_ty.is_some(), false);
            return None;
        }
        let void = self.types.void();
        let saved = std::mem::take(&mut self.body);
        self.body.frames.push(Frame {
            loc: p.loc,
            scopes: vec![Scope::default()],
            ret: void,
            self_ty,
            self_local: None,
            fn_name: "declarations".into(),
            block: None,
            subst: Default::default(),
            no_bounds: false,
            is_proc: false,
            decl: None,
            site: None,
        });
        self.begin_block();
        let site = self.enter_site(call.span);
        let code = self.expand_code(call);
        self.leave_site(site);
        self.end_block();
        self.body = saved;
        code
    }

    /// The `Self` of a macro called among declarations: the struct or enum
    /// whose body holds the call. In a `module` or `extend` body, and in a
    /// generic struct's, it stands for many types (a placeholder).
    fn owner_self(&mut self, owner: Option<DeclId>) -> Option<TyId> {
        let o = owner?;
        let d = &self.decls[o.0 as usize];
        match d.kind {
            DeclKind::Struct(s) if s.generics.is_empty() => {
                let span = d.span;
                Some(self.decl_as_type(o, span))
            }
            DeclKind::Enum(_) => {
                let span = d.span;
                Some(self.decl_as_type(o, span))
            }
            DeclKind::Struct(_) | DeclKind::Module | DeclKind::Extend(_) => {
                Some(self.types.intern(TyKind::Param(Name::new("Self"))))
            }
            _ => None,
        }
    }

    /// Turns the code a macro generated among declarations into
    /// declarations, reporting lines that can't be one.
    fn declarations_from(&mut self, code: Vec<ast::Stmt>, shown: &str, p: PendingMacro<'a>) -> Vec<ast::Item> {
        let mut statements = Vec::new();
        let generated = lines_to_items(code, &mut statements);
        for stmt in statements {
            self.report(
                Diagnostic::error(codes::TOP_LEVEL_STATEMENT, "statements must be inside a method")
                    .primary(stmt.span, "this statement is outside any `def`")
                    .note(format!(
                        "`{shown}` is called among declarations, so its code must be declarations: methods, constants, types, `include`s or macro calls"
                    ))
                    .help(format!("put the statement inside a `def` in the `quote`, or call `{shown}` inside a method")),
            );
        }
        let in_struct = p.owner.filter(|o| matches!(self.decls[o.0 as usize].kind, DeclKind::Struct(_)));
        let mut items = Vec::with_capacity(generated.len());
        for mut item in generated {
            if let (Some(o), ItemKind::Field(f)) = (in_struct, &item.kind) {
                let name = self.decls[o.0 as usize].name;
                self.report(
                    Diagnostic::error(codes::MACRO_DECLARATION, format!("a macro can't add fields to `{name}`"))
                        .primary(f.name.span, format!("`{shown}` generates this field"))
                        .note("a struct's layout is fixed before the macros in its body run, which is what lets them read `Self.fields`")
                        .help(format!(
                            "declare `{}` in `{name}` itself, and let the macro generate the methods that use it",
                            f.name.as_str()
                        )),
                );
                self.macros.rejected_fields.insert((o, f.name.name));
                continue;
            }
            if p.item.private {
                make_private(&mut item);
            }
            items.push(item);
        }
        items
    }

    /// Reports an `import` or `cimport` a macro generated (E0913), which
    /// can't work: packages are loaded and headers imported before any
    /// macro runs. Returns whether the item is one.
    pub(super) fn generated_import(&mut self, item: &ast::Item) -> bool {
        if item.span.file.expansion_index().is_none() {
            return false;
        }
        // Uses of what it would have named, in the macro's file where the
        // generated code resolves names, are explained by this error.
        let loc = self.names_at(item.span, DeclLoc { pkg: crate::input::PackageId(0), file: 0 });
        let what = match &item.kind {
            ItemKind::Import(import) => {
                let name = match import.alias {
                    Some(alias) => alias.name,
                    None => Name::new(&super::items::import_name(&import.path)),
                };
                self.failed_imports.insert((loc.pkg, loc.file, name));
                "an `import`"
            }
            ItemKind::Cimport(c) => {
                match super::items::cimport_as(c) {
                    Some(alias) => {
                        self.failed_imports.insert((loc.pkg, loc.file, Name::new(&alias)));
                    }
                    None => {
                        self.failed_merges.insert(loc.pkg);
                    }
                }
                "a `cimport`"
            }
            _ => return false,
        };
        self.report(
            Diagnostic::error(codes::MACRO_DECLARATION, format!("a macro can't generate {what}"))
                .primary(item.span, format!("{what} in generated code"))
                .note("packages are loaded and C headers imported before any macro runs")
                .help("write it in the file that defines the macro: names in the `quote`'s own code resolve with that file's imports, so the generated code can use the package without importing it"),
        );
        true
    }

    /// Reports attributes written on a macro call among declarations
    /// (E0328): they would apply to nothing.
    pub(super) fn reject_call_attributes(&mut self, item: &ast::Item) {
        for attr in &item.attrs {
            let name = attr.name.as_str();
            self.report(
                Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, format!("`@[{name}]` doesn't apply to a macro call"))
                    .primary(attr.span, "a macro call takes no attributes")
                    .note("attributes belong on the declarations the macro generates")
                    .help(format!("remove `@[{name}]`, or write it on the declarations inside the macro's `quote`")),
            );
        }
    }
}

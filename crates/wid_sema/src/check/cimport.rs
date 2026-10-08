//! What the checker does for packages that `cimport` generated: `types:`
//! mappings, the conversions where Wid's C types differ from a header's,
//! `@[extern]` constants, opaque structs and declarations that were not
//! imported.

use std::collections::HashMap;

use wid_diagnostics::{Applicability, Diagnostic, Span, and_list, codes, did_you_mean};
use wid_syntax::Name;
use wid_syntax::ast;

use super::ty::TyCtx;
use super::{Checker, DeclId, DeclKind, DeclLoc, FnSig};
use crate::input::{CBinding, CFunction, CSkipped, CSlot, PackageId};
use crate::ir::{self, CCall, ExprKind, GlobalId};
use crate::types::{CConv, TyId, TyKind};

/// The C headers, macros and libraries every `cimport` adds to the build.
#[derive(Default)]
pub(crate) struct CBuild {
    pub includes: Vec<ir::CInclude>,
    pub link_libs: Vec<String>,
    pub c_flags: Vec<String>,
    pub link_flags: Vec<String>,
}

impl CBuild {
    /// Adds one import, keeping the first copy of repeated flags.
    pub fn add(&mut self, b: &CBinding) {
        let include =
            ir::CInclude { header: b.include.clone(), defines: b.defines.clone(), implement: b.implement.clone() };
        match self.includes.iter_mut().find(|i| i.header == include.header) {
            Some(existing) => {
                for d in include.defines {
                    if !existing.defines.contains(&d) {
                        existing.defines.push(d);
                    }
                }
                existing.implement = existing.implement.take().or(include.implement);
            }
            None => self.includes.push(include),
        }
        for lib in &b.link_libs {
            if !self.link_libs.contains(lib) {
                self.link_libs.push(lib.clone());
            }
        }
        extend_unique_pairs(&mut self.c_flags, &b.c_flags);
        extend_unique_pairs(&mut self.link_flags, &b.link_flags);
    }
}

/// Appends flags that aren't there yet, treating `-isystem dir` and
/// `-framework Name` as one flag.
fn extend_unique_pairs(into: &mut Vec<String>, flags: &[String]) {
    let mut i = 0;
    while i < flags.len() {
        let pair = matches!(flags[i].as_str(), "-isystem" | "-framework" | "-I" | "-L") && i + 1 < flags.len();
        let group = if pair { &flags[i..i + 2] } else { &flags[i..i + 1] };
        let present = into.windows(group.len()).any(|w| w == group);
        if !present {
            into.extend(group.iter().cloned());
        }
        i += group.len();
    }
}

impl<'a> Checker<'a> {
    /// The binding of a package `cimport` generated.
    pub fn c_binding(&self, pkg: PackageId) -> Option<&'a CBinding> {
        self.input.packages.get(pkg.0 as usize).and_then(|p| p.cimport.as_ref())
    }

    /// The Wid type `types:` maps a record of a cimport package to.
    pub fn c_type_override(&mut self, pkg: PackageId, name: Name) -> Option<TyId> {
        self.c_binding(pkg)?;
        if !self.c_overrides.contains_key(&pkg) {
            self.c_overrides.insert(pkg, HashMap::new());
            let map = self.resolve_type_mappings(pkg);
            self.c_overrides.insert(pkg, map);
        }
        self.c_overrides.get(&pkg).and_then(|m| m.get(&name)).copied()
    }

    /// Resolves the `types:` option of the `cimport` that generated `pkg`,
    /// in the scope of the file that wrote it, checking each mapping
    /// against the layout C reports.
    fn resolve_type_mappings(&mut self, pkg: PackageId) -> HashMap<Name, TyId> {
        let mut out = HashMap::new();
        let Some(binding) = self.c_binding(pkg) else { return out };
        let (origin_pkg, file, index) = binding.origin;
        let Some(item) =
            self.input.packages[origin_pkg.0 as usize].files.get(file).and_then(|f| f.ast.item_at(index as u32))
        else {
            return out;
        };
        let ast::ItemKind::Cimport(c) = &item.kind else { return out };
        let Some(option) = c.options.iter().find(|o| o.name.as_str() == "types") else { return out };
        let ast::CimportValue::Hash { entries, .. } = &option.value else { return out };
        let ctx = TyCtx { loc: DeclLoc { pkg: origin_pkg, file }, self_ty: None, subst: Default::default() };
        for entry in entries {
            let Some(wid_name) = binding.record_names.get(&entry.key) else {
                let candidates: Vec<&str> = binding.record_names.keys().map(String::as_str).collect();
                let mut diag = Diagnostic::error(
                    codes::CIMPORT_OPTION,
                    format!("`{}` is not a struct or union in `{}`", entry.key, c.header),
                )
                .primary(entry.key_span, "`types:` maps C records to Wid types");
                if let Some(best) = did_you_mean(&entry.key, candidates.iter().copied()) {
                    diag = diag.suggest_replace(
                        format!("did you mean `{best}`?"),
                        entry.key_span,
                        best,
                        Applicability::MaybeIncorrect,
                    );
                }
                self.report(diag);
                continue;
            };
            let ast::ExprKind::Type(texpr) = &entry.value.kind else { continue };
            let ty = self.resolve_type(texpr, &ctx);
            if matches!(self.types.kind(ty), TyKind::Unknown) {
                out.insert(Name::new(wid_name), ty);
                continue;
            }
            let record = &binding.records[wid_name];
            let (size, align) = self.types.layout(ty);
            if (size, align) != (record.size, record.align) {
                let shown = self.types.display(ty);
                self.report(
                    Diagnostic::error(
                        codes::C_LAYOUT_MISMATCH,
                        format!("`{shown}` can't stand in for the C type `{}`", entry.key),
                    )
                    .primary(entry.value.span, format!("`{shown}` is {size} bytes, aligned to {align}"))
                    .secondary(
                        entry.key_span,
                        format!("`{}` is {} bytes, aligned to {}", entry.key, record.size, record.align),
                    )
                    .note("a mapped type is copied byte for byte, so both must have the same size and alignment")
                    .help(format!(
                        "map `{}` to a Wid type with the same fields, or remove it from `types:`",
                        entry.key
                    )),
                );
                out.insert(Name::new(wid_name), self.types.unknown());
                continue;
            }
            out.insert(Name::new(wid_name), ty);
        }
        out
    }

    /// How a value of a C slot converts to and from Wid in package `pkg`.
    pub fn slot_conv(&mut self, pkg: PackageId, slot: &CSlot) -> Option<CConv> {
        if slot.array {
            return Some(CConv::Array);
        }
        if slot.pointer {
            return Some(CConv::Pointer(slot.c_type.clone()));
        }
        let record = slot.record.as_ref()?;
        let binding = self.c_binding(pkg)?;
        let wid_name = binding.record_names.get(record)?;
        self.c_type_override(pkg, Name::new(wid_name)).map(|_| CConv::Mapped(slot.c_type.clone()))
    }

    /// The conversion of a field of an imported C struct, if any.
    pub fn field_c_conv(&mut self, pkg: PackageId, record: &str, field: &str) -> Option<CConv> {
        let slot = self.c_binding(pkg)?.records.get(record)?.fields.get(field)?.clone();
        self.slot_conv(pkg, &slot)
    }

    /// For a C function that an included header declares, the conversions
    /// its calls need. `name` is the Wid name and `symbol` the C name.
    pub fn header_call(&mut self, pkg: PackageId, name: Name, symbol: &str, sig: &FnSig) -> Option<CCall> {
        let (owner, function): (PackageId, CFunction) =
            match self.c_binding(pkg).and_then(|b| b.functions.get(name.as_str())) {
                Some(f) => (pkg, f.clone()),
                None => self.input.packages.iter().enumerate().find_map(|(i, p)| {
                    let b = p.cimport.as_ref()?;
                    let f = b.externs.get(symbol).or_else(|| b.functions.values().find(|f| f.c_name == symbol))?;
                    Some((PackageId(i as u32), f.clone()))
                })?,
            };
        let mut params = Vec::new();
        for (i, _) in sig.params.iter().enumerate() {
            let conv = function.params.get(i).and_then(|slot| self.slot_conv(owner, slot));
            params.push(conv);
        }
        let ret = function.ret.as_ref().and_then(|slot| self.slot_conv(owner, slot));
        Some(CCall { params, ret })
    }

    /// Reads an `@[extern]` constant: a C value read by name.
    pub fn extern_const(&mut self, decl: DeclId, span: Span) -> ir::Expr {
        let d = self.decls[decl.0 as usize].clone();
        let DeclKind::Const(c) = d.kind else { unreachable!("extern_const on a non-constant") };
        let ty = match &c.ty {
            Some(t) => {
                let ctx = TyCtx { loc: d.loc, self_ty: None, subst: Default::default() };
                self.resolve_type(t, &ctx)
            }
            None => self.types.unknown(),
        };
        if let Some(&id) = self.extern_globals.get(&decl) {
            return ir::Expr::new(ExprKind::Global(id), ty);
        }
        let symbol = d
            .item
            .attr("extern")
            .and_then(|a| a.args.first())
            .and_then(super::attrs::string_literal)
            .map_or_else(|| d.name.as_str().to_string(), str::to_string);
        let slot = self.c_binding(d.loc.pkg).and_then(|b| b.values.get(d.name.as_str())).cloned();
        let c_conv = slot.and_then(|s| self.slot_conv(d.loc.pkg, &s));
        let id = GlobalId(self.globals.len() as u32);
        self.globals.push(ir::Global {
            c_name: symbol,
            ty,
            init: None,
            constant: true,
            foreign: true,
            c_conv,
            embed: None,
            comptime_only: false,
        });
        self.extern_globals.insert(decl, id);
        let _ = span;
        ir::Expr::new(ExprKind::Global(id), ty)
    }

    /// Whether a declaration is an `@[extern]` constant.
    pub fn is_extern_const(&self, decl: DeclId) -> bool {
        let d = &self.decls[decl.0 as usize];
        matches!(d.kind, DeclKind::Const(_)) && d.item.has_attr("extern")
    }

    /// The declaration `cimport` left out under this Wid or C name, if any.
    /// For a package that merged `cimport`s, it looks in each of them.
    /// Returns the cimport package with the declaration.
    pub fn c_skipped(&self, pkg: PackageId, name: Name) -> Option<(PackageId, &'a CSkipped)> {
        let find = |p: PackageId| {
            self.c_binding(p)?.skipped.iter().find(|s| s.wid_name == name.as_str() || s.c_name == name.as_str())
        };
        if let Some(s) = find(pkg) {
            return Some((pkg, s));
        }
        self.merged_cimports.get(&pkg)?.iter().find_map(|&p| find(p).map(|s| (p, s)))
    }

    /// The C name of a declaration a cimport package declares as `name`.
    pub fn c_name_of(&self, pkg: PackageId, name: Name) -> Option<String> {
        let binding = self.c_binding(pkg)?;
        let text = name.as_str();
        binding
            .functions
            .get(text)
            .map(|f| f.c_name.clone())
            .or_else(|| binding.record_names.iter().find(|(_, w)| w.as_str() == text).map(|(c, _)| c.clone()))
    }

    /// Reports a use of a C declaration that was not imported. Returns
    /// whether `name` was one.
    pub fn report_not_imported(&mut self, pkg: PackageId, name: Name, span: Span) -> bool {
        let Some((pkg, skipped)) = self.c_skipped(pkg, name) else { return false };
        let header = self.input.packages[pkg.0 as usize].path.trim_start_matches("cimport:").to_string();
        if !skipped.collides_with.is_empty() {
            self.report_name_collision(pkg, skipped, span);
            return true;
        }
        let mut diag =
            Diagnostic::error(codes::NOT_IMPORTED, format!("`{}` from `{header}` was not imported", skipped.c_name))
                .primary(span, format!("`{}` {}", skipped.c_name, skipped.reason))
                .note(format!("C declares it at {}", skipped.location));
        diag = if skipped.reason.contains("function-like macro") {
            diag.help("write a small C function in a `.c` file of the package that calls the macro, and declare it with `@[extern]`")
        } else if skipped.reason.contains("mutable global") {
            diag.help("add C functions that read and write the variable to a `.c` file of the package, and declare them with `@[extern]`")
        } else {
            diag.help("wrap it in a C function with a simpler signature, in a `.c` file of the package, and declare that with `@[extern]`")
        };
        self.report(diag);
        true
    }

    /// Reports a use of a Wid name that several C declarations would get.
    fn report_name_collision(&mut self, pkg: PackageId, skipped: &CSkipped, span: Span) {
        let mut all = skipped.collides_with.clone();
        all.push(skipped.c_name.clone());
        all.sort();
        let listed: Vec<String> = all.iter().map(|c| format!("`{c}`")).collect();
        let mut diag = Diagnostic::error(
            codes::C_NAME_COLLISION,
            format!(
                "`{}` is ambiguous: {} {} become `{}` in Wid",
                skipped.wid_name,
                and_list(&listed),
                if all.len() == 2 { "both" } else { "all" },
                skipped.wid_name
            ),
        )
        .primary(span, if all.len() == 2 { "cimport left both out" } else { "cimport left all of them out" })
        .note("cimport turns C names into Wid names with fixed rules, and these end up the same");
        if let Some(binding) = self.c_binding(pkg) {
            let (origin, file, index) = binding.origin;
            if let Some(item) =
                self.input.packages[origin.0 as usize].files.get(file).and_then(|f| f.ast.item_at(index as u32))
            {
                let renames: Vec<String> = all
                    .iter()
                    .skip(1)
                    .enumerate()
                    .map(|(i, c)| format!("{c}: :{}_{}", skipped.wid_name, i + 2))
                    .collect();
                diag = diag.suggest(
                    format!("give the others their own names, so `{}` means `{}`", skipped.wid_name, all[0]),
                    vec![wid_diagnostics::Edit {
                        span: item.span.shrink_to_end(),
                        replacement: format!(", names: {{{}}}", renames.join(", ")),
                    }],
                    Applicability::HasPlaceholders,
                );
            }
        }
        self.report(diag);
    }

    /// Reports a field of an imported C struct that was not imported, like
    /// a bit-field. Returns whether `name` was one.
    pub fn report_skipped_field(&mut self, ty: TyId, name: Name, span: Span) -> bool {
        let TyKind::Struct(id) = *self.types.kind(ty) else { return false };
        let Some(&decl) = self.struct_decls.get(&id) else { return false };
        let d = &self.decls[decl.0 as usize];
        let Some(record) = self.c_binding(d.loc.pkg).and_then(|b| b.records.get(d.name.as_str())) else { return false };
        let Some((c_name, reason)) = record.skipped_fields.iter().find(|(c, _)| c == name.as_str()) else {
            return false;
        };
        let shown = d.name;
        self.report(
            Diagnostic::error(codes::NOT_IMPORTED, format!("field `{c_name}` of `{shown}` was not imported"))
                .primary(span, format!("`{c_name}` {reason}"))
                .help(
                    "read or write it with a small C function in a `.c` file of the package, declared with `@[extern]`",
                ),
        );
        true
    }

    /// Reports an opaque C struct used by value.
    pub fn check_not_opaque(&mut self, ty: TyId, span: Span) {
        let TyKind::Struct(id) = *self.types.kind(ty) else { return };
        let info = self.types.struct_info(id);
        if !info.opaque {
            return;
        }
        let name = info.name.clone();
        self.report(
            Diagnostic::error(
                codes::OPAQUE_BY_VALUE,
                format!("`{name}` is opaque, so it can only be used through a pointer"),
            )
            .primary(span, format!("this needs a whole `{name}`"))
            .note("C hides the fields and size of an opaque struct; only the library that defines it can create one")
            .suggest(
                "point at it instead",
                vec![wid_diagnostics::Edit { span: span.shrink_to_start(), replacement: "^".into() }],
                Applicability::MaybeIncorrect,
            ),
        );
    }
}

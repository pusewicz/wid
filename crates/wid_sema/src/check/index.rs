//! Builds the symbol index ([`crate::index`]) from the declarations the
//! checker collected. It only reads the checker's tables, so it reports
//! nothing and changes nothing: names resolve with the same pure lookups
//! as the checker's (`lookup_pkg`, `lookup_import`, `lookup_prelude`), from
//! the file where each name is written.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use wid_diagnostics::{FileId, Span};
use wid_syntax::ast::{self, ItemKind};
use wid_syntax::docs::DocComments;
use wid_syntax::print::{Context, Printer};

use super::{Checker, DeclId, DeclKind, DeclLoc};
use crate::index::{CDoc, FieldDoc, Index, Link, MemberDoc, PackageDoc, Symbol, SymbolId, SymbolKind};
use crate::input::PackageId;
use crate::types::TyId;

/// What the printer reads from the checker: literal text and spliced types.
struct PrintCx<'c, 'a> {
    checker: &'c Checker<'a>,
}

impl Context for PrintCx<'_, '_> {
    fn source(&self, span: Span) -> Option<&str> {
        self.checker.source_texts.get(&span.file)?.get(span.start as usize..span.end as usize)
    }

    fn spliced_type(&self, id: u32) -> String {
        if (id as usize) < self.checker.types.len() {
            self.checker.types.display(TyId(id))
        } else {
            "<spliced type>".to_string()
        }
    }
}

impl<'a> Checker<'a> {
    /// Builds the index of every declaration the checker collected.
    pub(super) fn build_index(&self) -> Index {
        let cx = PrintCx { checker: self };
        let printer = Printer::new(&cx);
        let input = self.input;
        let mut file_index: HashMap<FileId, (PackageId, usize)> = HashMap::new();
        for (p, pkg) in input.packages.iter().enumerate() {
            for (f, file) in pkg.files.iter().enumerate() {
                file_index.insert(file.ast.file, (PackageId(p as u32), f));
            }
        }
        let mut docs: HashMap<FileId, DocComments<'a>> = HashMap::new();
        for pkg in &input.packages {
            for file in &pkg.files {
                docs.insert(file.ast.file, DocComments::new(&file.ast, &file.text));
            }
        }
        let mut members: Vec<Vec<SymbolId>> = vec![Vec::new(); self.decls.len()];
        for (i, d) in self.decls.iter().enumerate() {
            if let Some(o) = d.owner {
                members[o.0 as usize].push(SymbolId(i as u32));
            }
        }
        let mut symbols = Vec::with_capacity(self.decls.len());
        for (i, d) in self.decls.iter().enumerate() {
            let id = DeclId(i as u32);
            let item = d.item;
            let signature = printer.item_header(item).unwrap_or_default();
            let kind = match d.kind {
                DeclKind::Fn(f) if f.is_macro => SymbolKind::Macro,
                DeclKind::Fn(_) => SymbolKind::Method,
                DeclKind::Const(c) if self.is_type_alias_value(&c.value, d.loc, 0) => SymbolKind::TypeAlias,
                DeclKind::Const(_) => SymbolKind::Constant,
                DeclKind::Struct(_) => SymbolKind::Struct,
                DeclKind::Enum(_) => SymbolKind::Enum,
                DeclKind::Union(_) => SymbolKind::Union,
                DeclKind::Module => SymbolKind::Module,
                DeclKind::Overload(_) => SymbolKind::Overload,
                DeclKind::Extend(_) => SymbolKind::Extension,
            };
            let name = match kind {
                SymbolKind::Extension => signature.clone(),
                _ => d.name.as_str().to_string(),
            };
            let mut fields = Vec::new();
            let mut enum_members = Vec::new();
            let mut links = Vec::new();
            match d.kind {
                DeclKind::Struct(s) => {
                    for member in &s.body {
                        let ItemKind::Field(f) = &member.kind else { continue };
                        let promotes = if f.using {
                            self.written_type_decl(self.index_names_loc(f.ty.span, d.loc), &f.ty).map(|t| SymbolId(t.0))
                        } else {
                            None
                        };
                        fields.push(FieldDoc {
                            name: f.name.as_str().to_string(),
                            ty: printer.ty(&f.ty),
                            default: f.default.as_ref().map(|e| printer.expr(e)),
                            using: f.using,
                            promotes,
                            doc: member.doc.clone(),
                            span: f.name.span,
                        });
                    }
                }
                DeclKind::Enum(e) => {
                    for m in &e.members {
                        enum_members.push(MemberDoc {
                            name: m.name.as_str().to_string(),
                            value: m.value.as_ref().map(|v| printer.expr(v)),
                            doc: docs.get(&m.name.span.file).and_then(|c| c.before(m.name.span.start)),
                            span: m.name.span,
                        });
                    }
                }
                DeclKind::Extend(e) => {
                    for t in &e.targets {
                        let symbol =
                            self.written_type_decl(self.index_names_loc(t.span, d.loc), t).map(|t| SymbolId(t.0));
                        links.push(Link { text: printer.ty(t), symbol });
                    }
                }
                DeclKind::Union(u) => {
                    for v in &u.variants {
                        let symbol =
                            self.written_type_decl(self.index_names_loc(v.span, d.loc), v).map(|t| SymbolId(t.0));
                        links.push(Link { text: printer.ty(v), symbol });
                    }
                }
                DeclKind::Const(c) if kind == SymbolKind::TypeAlias => {
                    let symbol = self
                        .written_value_decl(self.index_names_loc(c.value.span, d.loc), &c.value)
                        .map(|t| SymbolId(t.0));
                    links.push(Link { text: printer.expr(&c.value), symbol });
                }
                DeclKind::Overload(o) => {
                    for m in &o.members {
                        let symbol = match d.owner {
                            Some(owner) => self.members.get(&owner).and_then(|scope| scope.get(&m.name)).copied(),
                            None => self.lookup_pkg(d.loc.pkg, m.name),
                        };
                        links.push(Link { text: m.as_str().to_string(), symbol: symbol.map(|s| SymbolId(s.0)) });
                    }
                }
                _ => {}
            }
            let includes = self
                .include_items
                .get(&id)
                .map(|items| {
                    items
                        .iter()
                        .filter_map(|item| match &item.kind {
                            ItemKind::Include(t) => {
                                let symbol = self
                                    .written_type_decl(self.index_names_loc(t.span, d.loc), t)
                                    .filter(|m| matches!(self.decls[m.0 as usize].kind, DeclKind::Module))
                                    .map(|m| SymbolId(m.0));
                                Some(Link { text: printer.ty(t), symbol })
                            }
                            _ => None,
                        })
                        .collect()
                })
                .unwrap_or_default();
            let is_static = matches!(d.kind, DeclKind::Fn(f) if f.is_static);
            symbols.push(Symbol {
                name,
                kind,
                package: d.loc.pkg,
                span: d.span,
                private: d.private,
                owner: d.owner.map(|o| SymbolId(o.0)),
                is_static,
                doc: item.doc.clone(),
                signature,
                attributes: item.attrs.iter().map(|a| printer.attribute(a)).collect(),
                fields,
                enum_members,
                members: std::mem::take(&mut members[i]),
                includes,
                links,
                c: self.index_c_doc(d.loc.pkg, d.name.as_str(), item),
            });
        }
        let packages = input
            .packages
            .iter()
            .enumerate()
            .map(|(p, pkg)| {
                let pid = PackageId(p as u32);
                let scope: BTreeMap<String, SymbolId> =
                    self.pkg_scopes[p].iter().map(|(n, d)| (n.as_str().to_string(), SymbolId(d.0))).collect();
                let mut imports = BTreeMap::new();
                for f in 0..pkg.files.len() {
                    if let Some(map) = self.file_imports.get(&(pid, f)) {
                        let mut names: Vec<_> = map.iter().map(|(n, (target, _))| (n.as_str(), *target)).collect();
                        names.sort();
                        for (name, target) in names {
                            imports.entry(name.to_string()).or_insert(target);
                        }
                    }
                }
                PackageDoc {
                    name: pkg.name.clone(),
                    path: pkg.path.clone(),
                    dir: pkg.dir.clone(),
                    doc: self.index_package_doc(pid, &docs),
                    scope,
                    items: self.index_package_items(pid, &file_index),
                    imports,
                    header: pkg.cimport.as_ref().map(|_| pkg.path.trim_start_matches("cimport:").to_string()),
                }
            })
            .collect();
        Index { packages, symbols, prelude: input.prelude, builtin_types: super::ty::PRIMITIVE_NAMES.to_vec() }
    }

    /// Where names written at `span` resolve: for generated code, the
    /// macro's file; otherwise the declaration's.
    fn index_names_loc(&self, span: Span, fallback: DeclLoc) -> DeclLoc {
        self.virtual_file(span.file).map_or(fallback, |v| v.loc)
    }

    /// The declaration a written type names, through pointers and
    /// optionals (`using base: ^Entity`) and type aliases, if it names one.
    fn written_type_decl(&self, loc: DeclLoc, t: &ast::TypeExpr) -> Option<DeclId> {
        match &t.kind {
            ast::TypeKind::Path { segments, .. } => {
                let decl = match segments.as_slice() {
                    [only] => self.lookup_pkg(loc.pkg, only.name).or_else(|| self.lookup_prelude(only.name)),
                    [pkg, name] => self.lookup_import(loc, pkg.name).and_then(|p| self.lookup_pkg(p, name.name)),
                    _ => None,
                }?;
                self.index_follow_alias(decl, 0)
            }
            ast::TypeKind::Pointer(inner) | ast::TypeKind::Optional(inner) => self.written_type_decl(loc, inner),
            _ => None,
        }
    }

    /// The declaration a constant's value names when it is a type, like
    /// `Texture` in `Texture2D = Texture`.
    fn written_value_decl(&self, loc: DeclLoc, value: &ast::Expr) -> Option<DeclId> {
        let decl = match &value.kind {
            ast::ExprKind::Type(t) => return self.written_type_decl(loc, t),
            ast::ExprKind::Paren(inner) => return self.written_value_decl(loc, inner),
            ast::ExprKind::Const(n) => self.lookup_pkg(loc.pkg, *n).or_else(|| self.lookup_prelude(*n))?,
            ast::ExprKind::Member { recv, name, .. } => match recv.kind {
                ast::ExprKind::Ident(p) | ast::ExprKind::Const(p) => {
                    self.lookup_import(loc, p).and_then(|p| self.lookup_pkg(p, name.name))?
                }
                _ => return None,
            },
            _ => return None,
        };
        Some(decl)
    }

    /// A type declaration, following type aliases to the type they name.
    fn index_follow_alias(&self, decl: DeclId, depth: u32) -> Option<DeclId> {
        let d = &self.decls[decl.0 as usize];
        match d.kind {
            DeclKind::Const(c) if depth < 16 => {
                let next = self.written_value_decl(d.loc, &c.value)?;
                self.index_follow_alias(next, depth + 1)
            }
            _ => Some(decl),
        }
    }

    /// For a declaration of a `cimport` package, its C name and header.
    fn index_c_doc(&self, pkg: PackageId, name: &str, item: &ast::Item) -> Option<CDoc> {
        let binding = self.c_binding(pkg)?;
        let c_name = extern_name(item)
            .or_else(|| binding.functions.get(name).map(|f| f.c_name.clone()))
            .or_else(|| binding.records.get(name).map(|r| r.c_type.clone()))
            .or_else(|| binding.record_names.iter().find(|(_, w)| w.as_str() == name).map(|(c, _)| c.clone()))
            .unwrap_or_else(|| name.to_string());
        Some(CDoc {
            name: c_name,
            header: self.input.packages[pkg.0 as usize].path.trim_start_matches("cimport:").to_string(),
            declared_at: binding.locations.get(name).cloned(),
        })
    }

    /// The package doc: the opening comment block of the file named after
    /// the package, else of the first file that has one.
    fn index_package_doc(&self, pkg: PackageId, docs: &HashMap<FileId, DocComments<'a>>) -> Option<String> {
        let p = &self.input.packages[pkg.0 as usize];
        let dir_name = p.dir.file_name().map(|n| n.to_string_lossy().into_owned());
        let named = |display: &str| {
            let stem = Path::new(display).file_stem().map(|s| s.to_string_lossy().into_owned());
            stem.is_some_and(|s| s == p.name || Some(&s) == dir_name.as_ref())
        };
        let doc_of = |f: &crate::input::FileInput| docs.get(&f.ast.file).and_then(DocComments::package_doc);
        p.files.iter().filter(|f| named(&f.display)).find_map(doc_of).or_else(|| p.files.iter().find_map(doc_of))
    }

    /// A package's top-level declarations in source order, with the
    /// declarations of the `cimport`s merged into it where the `cimport`
    /// is written, and generated ones where the macro call is.
    fn index_package_items(&self, pkg: PackageId, file_index: &HashMap<FileId, (PackageId, usize)>) -> Vec<SymbolId> {
        let mut keyed: Vec<((usize, u32, u32), SymbolId)> = Vec::new();
        for (i, d) in self.decls.iter().enumerate() {
            if d.owner.is_some() || d.loc.pkg != pkg {
                continue;
            }
            let at = self.index_anchor(d.item.span);
            let (file, start) = match file_index.get(&at.file) {
                Some(&(p, f)) if p == pkg => (f, at.start),
                _ => (d.loc.file, d.item.span.start),
            };
            keyed.push(((file, start, i as u32), SymbolId(i as u32)));
        }
        for &cpkg in self.merged_cimports.get(&pkg).map(Vec::as_slice).unwrap_or_default() {
            let Some(binding) = self.c_binding(cpkg) else { continue };
            let (_, file, start) = binding.origin;
            for (i, d) in self.decls.iter().enumerate() {
                if d.owner.is_none()
                    && d.loc.pkg == cpkg
                    && self.pkg_scopes[pkg.0 as usize].get(&d.name) == Some(&DeclId(i as u32))
                {
                    keyed.push(((file, start as u32, i as u32), SymbolId(i as u32)));
                }
            }
        }
        keyed.sort_by_key(|(key, _)| *key);
        keyed.into_iter().map(|(_, id)| id).collect()
    }

    /// The span in a real file that code at `span` stands for: generated
    /// code stands where its outermost macro call is.
    fn index_anchor(&self, mut span: Span) -> Span {
        for _ in 0..256 {
            let Some(v) = self.virtual_file(span.file) else { break };
            match self.macros.expansions.get(v.expansion as usize) {
                Some(e) => span = e.call_site,
                None => break,
            }
        }
        span
    }
}

/// The string `@[extern("name")]` gives, if the item has one.
fn extern_name(item: &ast::Item) -> Option<String> {
    let attr = item.attr("extern")?;
    match &attr.args.first()?.kind {
        ast::ExprKind::Str(parts) => Some(
            parts
                .iter()
                .map(|p| match p {
                    ast::StrPart::Text(t) => t.as_str(),
                    ast::StrPart::Interp(_) => "",
                })
                .collect(),
        ),
        _ => None,
    }
}

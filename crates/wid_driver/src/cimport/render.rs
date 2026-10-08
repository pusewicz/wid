//! Turns an imported C header into Wid source: the package `cimport` adds to
//! the program, and the text `wid cimport --dump` prints.
//!
//! Names follow fixed rules so that every program sees the same API:
//! functions and parameters become `snake_case`, types `PascalCase`, and
//! constants keep their C spelling (or become `SCREAMING_CASE` when they
//! start with a lowercase letter). `strip_prefix:` runs first and `names:`
//! overrides any single name.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;

use wid_cimport::{
    CModule, CType, Field, FnSig, Function, IntType, Item, ItemKind, MacroKind, MacroValue, Named, NamedKind, Record,
    RecordKind,
};
use wid_sema::{CBinding, CFunction, CRecord, CSkipped, CSlot};
use wid_syntax::token::Keyword;

/// How C names become Wid names.
#[derive(Clone, Debug, Default)]
pub struct Naming {
    /// Prefixes removed from every name, compared ignoring case.
    pub strip_prefixes: Vec<String>,
    /// `rename: :keep`: keep C names, only fixing the case of their first
    /// letter.
    pub keep: bool,
    /// `names:` overrides, by C name.
    pub names: HashMap<String, String>,
    /// The C records that `types:` maps to Wid types; they are not declared.
    pub mapped: HashSet<String>,
}

/// The Wid view of a header.
pub struct Rendered {
    /// The Wid source of the package.
    pub source: String,
    /// The C details that the source can't express.
    pub binding: CBinding,
}

/// Where a C type appears, which decides what it may be.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Pos {
    Param,
    Return,
    Field,
    Typedef,
    Pointee,
    Value,
}

/// What kind of Wid name a declaration gets.
#[derive(Clone, Copy, PartialEq, Eq)]
enum NameKind {
    Function,
    Type,
    Constant,
}

/// Renders `module` as Wid source under the given naming rules.
pub fn render(module: &CModule, naming: &Naming, header_display: &str) -> Rendered {
    let mut r = Renderer {
        module,
        naming,
        names: HashMap::new(),
        taken: HashMap::new(),
        skipped: Vec::new(),
        externals: BTreeMap::new(),
        binding: CBinding::default(),
        out: String::new(),
    };
    r.assign_names();
    r.header(header_display);
    for (index, item) in module.items.iter().enumerate() {
        r.item(index, item);
    }
    r.externals();
    r.skipped_summary();
    let mut binding = r.binding;
    binding.skipped = r.skipped;
    for function in &module.probed {
        if let Ok(f) = cfunction(module, &function.name, &function.sig) {
            binding.externs.insert(function.name.clone(), f);
        }
    }
    Rendered { source: r.out, binding }
}

struct Renderer<'m> {
    module: &'m CModule,
    naming: &'m Naming,
    /// The Wid name of each item, by index.
    names: HashMap<usize, String>,
    /// Which C name owns each Wid name.
    taken: HashMap<String, String>,
    skipped: Vec<CSkipped>,
    /// Opaque C types from outside the import, by spelling, with their Wid
    /// names.
    externals: BTreeMap<String, String>,
    binding: CBinding,
    out: String,
}

impl Renderer<'_> {
    /// Gives every item its Wid name. Names two declarations would share go
    /// to neither: both are left out, and a use of the name explains why.
    fn assign_names(&mut self) {
        let mut claims: Vec<(String, String, String, Option<usize>)> = Vec::new();
        for (index, item) in self.module.items.iter().enumerate() {
            if item.name().is_some_and(is_reserved) {
                continue;
            }
            let (c_name, kind) = match &item.kind {
                ItemKind::Function(f) => (f.name.clone(), NameKind::Function),
                ItemKind::Record(_) | ItemKind::Enum(_) | ItemKind::Typedef(_) => match item.name() {
                    Some(name) => (name.to_string(), NameKind::Type),
                    None => (String::new(), NameKind::Type),
                },
                ItemKind::Global(g) => (g.name.clone(), NameKind::Constant),
                ItemKind::Macro(m) => match &m.kind {
                    MacroKind::Expr { ty: CType::FnPtr(_), .. } if is_identifier(&m.body) => {
                        (m.name.clone(), NameKind::Function)
                    }
                    MacroKind::FunctionLike { .. } => {
                        let wid = self.wid_name(&m.name, NameKind::Function);
                        self.names.insert(index, wid);
                        continue;
                    }
                    MacroKind::Other => {
                        let wid = self.wid_name(&m.name, NameKind::Constant);
                        self.names.insert(index, wid);
                        continue;
                    }
                    MacroKind::Expr { .. } => (m.name.clone(), NameKind::Constant),
                },
            };
            if !c_name.is_empty() {
                let wid = self.wid_name(&c_name, kind);
                if let ItemKind::Record(_) = &item.kind {
                    self.binding.record_names.insert(c_name.clone(), wid.clone());
                }
                claims.push((wid, c_name, location_text(&item.location), Some(index)));
            }
            if let ItemKind::Enum(e) = &item.kind {
                for constant in e.constants.iter().filter(|c| !is_reserved(&c.name)) {
                    let wid = self.wid_name(&constant.name, NameKind::Constant);
                    claims.push((wid, constant.name.clone(), location_text(&constant.location), None));
                }
            }
        }
        let mut owners: HashMap<String, Vec<(String, String)>> = HashMap::new();
        for (wid, c_name, location, _) in &claims {
            let list = owners.entry(wid.clone()).or_default();
            if !list.iter().any(|(c, _)| c == c_name) {
                list.push((c_name.clone(), location.clone()));
            }
        }
        for (wid, c_name, _, index) in claims {
            let list = &owners[&wid];
            if list.len() > 1 || self.taken.contains_key(&wid) {
                continue;
            }
            self.taken.insert(wid.clone(), c_name);
            if let Some(index) = index {
                self.names.insert(index, wid);
            }
        }
        let mut collided: Vec<(&String, &Vec<(String, String)>)> = owners.iter().filter(|(_, l)| l.len() > 1).collect();
        collided.sort();
        for (wid, list) in collided {
            for (c_name, location) in list {
                let others: Vec<String> = list.iter().filter(|(c, _)| c != c_name).map(|(c, _)| c.clone()).collect();
                let quoted: Vec<String> = others.iter().map(|c| format!("`{c}`")).collect();
                self.skipped.push(CSkipped {
                    c_name: c_name.clone(),
                    wid_name: wid.clone(),
                    reason: format!("has the same Wid name as {}", wid_diagnostics::and_list(&quoted)),
                    location: location.clone(),
                    collides_with: others,
                });
            }
        }
    }

    /// The Wid name for a C name of the given kind.
    fn wid_name(&self, c_name: &str, kind: NameKind) -> String {
        if let Some(name) = self.naming.names.get(c_name) {
            return name.clone();
        }
        let stripped = strip(c_name, &self.naming.strip_prefixes);
        let name = match (kind, self.naming.keep) {
            (NameKind::Function, false) => snake_case(stripped),
            (NameKind::Function, true) => lower_first(stripped),
            (NameKind::Type, false) => pascal_case(stripped),
            (NameKind::Type, true) => upper_first(stripped),
            (NameKind::Constant, _) => constant_case(stripped),
        };
        escape_keyword(name)
    }

    /// Writes the file header.
    fn header(&mut self, header_display: &str) {
        let _ = writeln!(self.out, "# The Wid view of {header_display}, generated by `cimport`.");
        let _ = writeln!(self.out, "# C declares everything here; Wid only calls it.");
        let _ = writeln!(self.out, "# target: {}", self.module.target.triple);
        self.out.push('\n');
        self.out.push_str("import \"core:c\", as: :C\n");
    }

    /// Records a declaration that is not imported.
    fn skip(&mut self, c_name: &str, wid_name: &str, reason: String, item: &Item) {
        self.skipped.push(CSkipped {
            c_name: c_name.to_string(),
            wid_name: wid_name.to_string(),
            reason,
            location: location_text(&item.location),
            collides_with: Vec::new(),
        });
    }

    /// Renders one item.
    fn item(&mut self, index: usize, item: &Item) {
        let Some(name) = self.names.get(&index).cloned() else {
            if let ItemKind::Enum(e) = &item.kind {
                self.enumeration(None, e.underlying.clone(), &e.constants, item);
            }
            return;
        };
        match &item.kind {
            ItemKind::Function(f) => self.function(&name, f, item),
            ItemKind::Record(record) => self.record(&name, record, item),
            ItemKind::Enum(e) => {
                let c_name = item.name().unwrap_or_default().to_string();
                self.enumeration(Some((&name, &c_name)), e.underlying.clone(), &e.constants, item);
            }
            ItemKind::Typedef(t) => match self.wid_type(&t.ty, Pos::Typedef) {
                Ok(ty) => {
                    self.blank_and_doc(item);
                    let _ = writeln!(self.out, "{name} = {ty}");
                }
                Err(reason) => self.skip(&t.name, &name, reason, item),
            },
            ItemKind::Global(g) => {
                if !g.quals.is_const || g.is_thread_local {
                    let reason = "is a mutable global variable; read or write it through a C function".to_string();
                    self.skip(&g.name, &name, reason, item);
                    return;
                }
                self.value(&name, &g.name, &g.ty, item);
            }
            ItemKind::Macro(m) => match &m.kind {
                MacroKind::Expr { ty: CType::FnPtr(sig), .. } if is_identifier(&m.body) => {
                    let function =
                        Function { name: m.name.clone(), sig: (**sig).clone(), is_inline: false, is_static: false };
                    self.function(&name, &function, item);
                }
                MacroKind::Expr { value: Some(value), .. } => self.constant(&name, value, item),
                MacroKind::Expr { ty, value: None } => self.value(&name, &m.name, ty, item),
                MacroKind::Other => {
                    let reason = format!("is a macro that does not expand to a value (`{}`)", m.body);
                    self.skip(&m.name, &name, reason, item);
                }
                MacroKind::FunctionLike { .. } => {
                    self.skip(&m.name, &name, "is a function-like macro".to_string(), item);
                    let constant = self.wid_name(&m.name, NameKind::Constant);
                    if constant != name {
                        self.skip(&m.name, &constant, "is a function-like macro".to_string(), item);
                    }
                }
            },
        }
    }

    /// Writes a blank line and the item's documentation as comments.
    fn blank_and_doc(&mut self, item: &Item) {
        self.out.push('\n');
        if let Some(doc) = &item.doc {
            for line in doc_lines(doc) {
                let _ = writeln!(self.out, "# {line}");
            }
        }
    }

    /// Renders a function.
    fn function(&mut self, name: &str, f: &Function, item: &Item) {
        let mut params = Vec::new();
        let mut used = HashSet::new();
        for (i, p) in f.sig.params.iter().enumerate() {
            let ty = match self.wid_type(&p.ty, Pos::Param) {
                Ok(ty) => ty,
                Err(reason) => {
                    let what = p
                        .name
                        .as_deref()
                        .map_or_else(|| format!("parameter {}", i + 1), |n| format!("parameter `{n}`"));
                    self.skip(&f.name, name, format!("has {what}, which {reason}"), item);
                    return;
                }
            };
            let mut pname = escape_keyword(snake_case(p.name.as_deref().unwrap_or("")));
            if pname.is_empty() || pname.starts_with(|c: char| c.is_ascii_digit()) {
                pname = format!("arg{i}");
            }
            while !used.insert(pname.clone()) {
                pname.push('_');
            }
            params.push(format!("{pname}: {ty}"));
        }
        if f.sig.variadic {
            params.push("...".to_string());
        }
        let ret = match &f.sig.ret {
            CType::Void => String::new(),
            ty => match self.wid_type(ty, Pos::Return) {
                Ok(ty) => format!(" -> {ty}"),
                Err(reason) => {
                    self.skip(&f.name, name, format!("returns a value that {reason}"), item);
                    return;
                }
            },
        };
        let Ok(cf) = cfunction(self.module, &f.name, &f.sig) else {
            self.skip(&f.name, name, "has a type C can't spell in a cast".to_string(), item);
            return;
        };
        self.binding.functions.insert(name.to_string(), cf);
        self.blank_and_doc(item);
        let _ = writeln!(self.out, "@[extern(\"{}\")]", f.name);
        let _ = writeln!(self.out, "def {name}({}){ret} end", params.join(", "));
    }

    /// Renders a struct or union.
    fn record(&mut self, name: &str, record: &Record, item: &Item) {
        let c_name = item.name().unwrap_or_default().to_string();
        if self.naming.mapped.contains(&c_name) {
            self.out.push('\n');
            let _ = writeln!(self.out, "# `{c_name}` is mapped to a Wid type with `types:`.");
            let (size, align) = record.body.as_ref().map_or((0, 0), |b| (b.size, b.align));
            self.binding.records.insert(
                name.to_string(),
                CRecord {
                    c_type: record_spelling(record),
                    size,
                    align,
                    fields: HashMap::new(),
                    skipped_fields: Vec::new(),
                },
            );
            return;
        }
        let spelling = record_spelling(record);
        self.blank_and_doc(item);
        let Some(body) = &record.body else {
            let _ = writeln!(self.out, "@[extern(\"{spelling}\"), opaque]");
            let _ = writeln!(self.out, "struct {name}\nend");
            self.binding.records.insert(
                name.to_string(),
                CRecord { c_type: spelling, size: 0, align: 0, fields: HashMap::new(), skipped_fields: Vec::new() },
            );
            return;
        };
        let kind = match record.kind {
            RecordKind::Struct => "struct",
            RecordKind::Union => "union",
        };
        let _ = writeln!(self.out, "# C {kind}: {} bytes, aligned to {}", body.size, body.align);
        let _ = writeln!(self.out, "@[extern(\"{spelling}\"), size({}), align({})]", body.size, body.align);
        let _ = writeln!(self.out, "struct {name}");
        let mut fields = HashMap::new();
        let mut skipped_fields = Vec::new();
        let mut lines = String::new();
        self.fields(&body.fields, &mut fields, &mut skipped_fields, &mut lines);
        self.out.push_str(&lines);
        self.out.push_str("end\n");
        self.binding.records.insert(
            name.to_string(),
            CRecord { c_type: spelling, size: body.size, align: body.align, fields, skipped_fields },
        );
    }

    /// Renders the fields of a record, flattening anonymous members.
    fn fields(
        &mut self,
        fields: &[Field],
        slots: &mut HashMap<String, CSlot>,
        skipped: &mut Vec<(String, String)>,
        out: &mut String,
    ) {
        for field in fields {
            let Some(fname) = &field.name else {
                if let CType::Record(inner) = &field.ty
                    && let Some(body) = &inner.body
                {
                    self.fields(&body.fields, slots, skipped, out);
                }
                continue;
            };
            if let Some(bits) = field.bit_width {
                let _ = writeln!(out, "  # `{fname}` is a {bits}-bit bit-field, which Wid can't address");
                skipped.push((fname.clone(), format!("is a {bits}-bit bit-field, which Wid can't address")));
                continue;
            }
            match self.wid_type(&field.ty, Pos::Field) {
                Ok(ty) => {
                    if let Some(doc) = &field.doc {
                        for line in doc_lines(doc) {
                            let _ = writeln!(out, "  # {line}");
                        }
                    }
                    let wid = escape_keyword(if self.naming.keep { lower_first(fname) } else { snake_case(fname) });
                    let wid = if wid.is_empty() || wid.starts_with(|c: char| c.is_ascii_digit()) {
                        format!("f_{fname}")
                    } else {
                        wid
                    };
                    if &wid == fname {
                        let _ = writeln!(out, "  {wid}: {ty}");
                    } else {
                        let _ = writeln!(out, "  @[extern(\"{fname}\")] {wid}: {ty}");
                    }
                    if let Ok(slot) = slot(self.module, &field.ty) {
                        slots.insert(wid, slot);
                    }
                }
                Err(reason) => {
                    let _ = writeln!(out, "  # `{fname}` is not imported: it {reason}");
                    skipped.push((fname.clone(), reason));
                }
            }
        }
    }

    /// Renders an enum as an integer type and its constants.
    fn enumeration(
        &mut self,
        name: Option<(&str, &str)>,
        underlying: CType,
        constants: &[wid_cimport::EnumConstant],
        item: &Item,
    ) {
        self.blank_and_doc(item);
        if let Some((name, c_name)) = name {
            let base = self.wid_type(&underlying, Pos::Typedef).unwrap_or_else(|_| "C.int".to_string());
            let _ = writeln!(self.out, "# C enum `{c_name}`: its constants are plain integers");
            let _ = writeln!(self.out, "{name} = {base}");
        }
        for constant in constants {
            let wid = self.wid_name(&constant.name, NameKind::Constant);
            if self.taken.get(&wid) != Some(&constant.name) {
                continue;
            }
            if let Some(doc) = &constant.doc {
                for line in doc_lines(doc) {
                    let _ = writeln!(self.out, "# {line}");
                }
            }
            let _ = writeln!(self.out, "{wid} = {}", constant.value);
        }
    }

    /// Renders a macro with a constant value.
    fn constant(&mut self, name: &str, value: &MacroValue, item: &Item) {
        let text = match value {
            MacroValue::Int(v) | MacroValue::Char(v) => v.to_string(),
            MacroValue::Bool(b) => b.to_string(),
            MacroValue::Float(f) if f.is_finite() => float_text(*f),
            MacroValue::Float(_) => {
                let c_name = item.name().unwrap_or_default().to_string();
                self.skip(&c_name, name, "is an infinite or NaN float".to_string(), item);
                return;
            }
            MacroValue::Str(bytes) => wid_string(bytes),
        };
        self.blank_and_doc(item);
        let _ = writeln!(self.out, "{name} = {text}");
    }

    /// Renders a macro or constant global that C reads by name.
    fn value(&mut self, name: &str, c_name: &str, ty: &CType, item: &Item) {
        let wid = match self.wid_type(ty, Pos::Value) {
            Ok(wid) => wid,
            Err(reason) => {
                self.skip(c_name, name, format!("has a type that {reason}"), item);
                return;
            }
        };
        let Ok(slot) = slot(self.module, ty) else {
            self.skip(c_name, name, "has a type C can't spell in a cast".to_string(), item);
            return;
        };
        self.binding.values.insert(name.to_string(), slot);
        self.blank_and_doc(item);
        let _ = writeln!(self.out, "@[extern(\"{c_name}\")]");
        let _ = writeln!(self.out, "{name}: {wid} = ---");
    }

    /// Declares the opaque types from outside the import that pointers use.
    fn externals(&mut self) {
        let externals = std::mem::take(&mut self.externals);
        for (spelling, name) in &externals {
            self.out.push('\n');
            let _ = writeln!(self.out, "# Declared outside {}", self.module.root.display());
            let _ = writeln!(self.out, "@[extern(\"{spelling}\"), opaque]");
            let _ = writeln!(self.out, "struct {name}\nend");
            self.binding.records.insert(
                name.clone(),
                CRecord {
                    c_type: spelling.clone(),
                    size: 0,
                    align: 0,
                    fields: HashMap::new(),
                    skipped_fields: Vec::new(),
                },
            );
        }
        self.externals = externals;
    }

    /// Lists what was left out, so the dump explains every gap.
    fn skipped_summary(&mut self) {
        let mut lines: Vec<String> =
            self.skipped.iter().map(|s| format!("#   {} ({}): {}", s.c_name, s.location, s.reason)).collect();
        if lines.is_empty() {
            return;
        }
        lines.sort();
        lines.dedup();
        self.out.push_str("\n# Not imported:\n");
        for line in lines {
            self.out.push_str(&line);
            self.out.push('\n');
        }
    }

    /// The Wid type for a C type at `pos`, or why there is none.
    fn wid_type(&mut self, ty: &CType, pos: Pos) -> Result<String, String> {
        match ty {
            CType::Void => Err("is `void`".to_string()),
            CType::Bool => Ok("Bool".to_string()),
            CType::Char { .. } => Ok("C.char".to_string()),
            CType::Int(i) => int_type(i),
            CType::Float(f) => match f.bits {
                32 => Ok("C.float".to_string()),
                64 => Ok("C.double".to_string()),
                _ => Err(format!("uses `{}`, which Wid has no type for", f.spelling)),
            },
            CType::Pointer(p) => self.pointer_type(&p.pointee),
            CType::Array(a) => match (pos, a.len) {
                (Pos::Field | Pos::Typedef | Pos::Value, Some(n)) => {
                    let elem = self.wid_type(&a.element, Pos::Field)?;
                    Ok(format!("[{n}]{elem}"))
                }
                (Pos::Field, None) => Err("is a flexible array member".to_string()),
                _ => Err("is an array of unknown size".to_string()),
            },
            CType::FnPtr(sig) => self.proc_type(sig),
            CType::Function(sig) if pos == Pos::Typedef => self.proc_type(sig),
            CType::Function(_) => Err("is a function type".to_string()),
            CType::Named(named) => self.named_type(named, pos),
            CType::Record(_) => Err("has an anonymous struct or union type".to_string()),
            CType::Atomic(_) => Err("is `_Atomic`".to_string()),
            CType::Opaque(spelling) if pos == Pos::Pointee => Ok(self.external(spelling)),
            CType::Opaque(spelling) => Err(format!("uses `{spelling}` by value; Wid can only point at it")),
        }
    }

    /// The Wid type for a pointer to `pointee`.
    fn pointer_type(&mut self, pointee: &CType) -> Result<String, String> {
        match resolve_typedefs(self.module, pointee) {
            CType::Char { .. } => return Ok("CString?".to_string()),
            CType::Int(IntType { bits: 8, signed: false, .. }) => return Ok("[^]U8?".to_string()),
            CType::Void => return Ok("RawPtr?".to_string()),
            CType::Function(sig) => return self.proc_type(&sig),
            _ => {}
        }
        let inner = self.wid_type(pointee, Pos::Pointee)?;
        if inner.ends_with('?') || inner.contains(' ') { Ok(format!("^({inner})?")) } else { Ok(format!("^{inner}?")) }
    }

    /// The Wid type of a C function pointer.
    fn proc_type(&mut self, sig: &FnSig) -> Result<String, String> {
        let mut params = Vec::new();
        for (i, p) in sig.params.iter().enumerate() {
            let ty = self
                .wid_type(&p.ty, Pos::Param)
                .map_err(|reason| format!("is a callback whose parameter {} {reason}", i + 1))?;
            let pname = escape_keyword(snake_case(p.name.as_deref().unwrap_or("")));
            if pname.is_empty() || pname.starts_with(|c: char| c.is_ascii_digit()) {
                params.push(ty);
            } else {
                params.push(format!("{pname}: {ty}"));
            }
        }
        if sig.variadic {
            params.push("...".to_string());
        }
        let ret = match &sig.ret {
            CType::Void => String::new(),
            ty => {
                let ty =
                    self.wid_type(ty, Pos::Return).map_err(|reason| format!("is a callback whose result {reason}"))?;
                format!(" -> {ty}")
            }
        };
        Ok(format!("@[c] proc({}){ret}", params.join(", ")))
    }

    /// The Wid name of a record, enum or typedef.
    fn named_type(&mut self, named: &Named, pos: Pos) -> Result<String, String> {
        let Some(target) = self.module.resolve(named) else {
            return Err(format!("uses `{}`, which the header does not define", named.name));
        };
        let Some(index) = self.module.items.iter().position(|item| std::ptr::eq(item, target)) else {
            return Err(format!("uses `{}`, which the header does not define", named.name));
        };
        let item = &self.module.items[index];
        let Some(name) = self.names.get(&index).cloned() else {
            return Err(format!("uses `{}`, whose name collides with another declaration", named.name));
        };
        match &item.kind {
            ItemKind::Record(record) if record.is_opaque() && pos != Pos::Pointee => {
                Err(format!("passes the opaque type `{name}` by value; Wid can only point at it"))
            }
            ItemKind::Typedef(t) => {
                if let Some(reason) = self.skipped.iter().find(|s| s.c_name == t.name).map(|s| s.reason.clone()) {
                    return Err(format!("uses `{}`, which is not imported: it {reason}", t.name));
                }
                let target = t.ty.clone();
                self.wid_type(&target, Pos::Typedef).map_err(|reason| format!("uses `{}`, which {reason}", t.name))?;
                if pos != Pos::Pointee
                    && matches!(resolve_typedefs(self.module, &target), CType::Named(n) if self.is_opaque(&n))
                {
                    return Err(format!("passes the opaque type `{name}` by value; Wid can only point at it"));
                }
                Ok(name)
            }
            _ => Ok(name),
        }
    }

    /// Whether a named type is an opaque record.
    fn is_opaque(&self, named: &Named) -> bool {
        matches!(self.module.resolve(named), Some(Item { kind: ItemKind::Record(r), .. }) if r.is_opaque())
    }

    /// The Wid name of an opaque type declared outside the import.
    fn external(&mut self, spelling: &str) -> String {
        if let Some(name) = self.externals.get(spelling) {
            return name.clone();
        }
        let bare = spelling.trim_start_matches("struct ").trim_start_matches("union ").trim_start_matches("enum ");
        let mut name = escape_keyword(pascal_case(bare));
        while self.taken.get(&name).is_some_and(|owner| owner != spelling) {
            name.push('_');
        }
        self.taken.insert(name.clone(), spelling.to_string());
        self.externals.insert(spelling.to_string(), name.clone());
        name
    }
}

/// The C details of a function, for the checker.
fn cfunction(module: &CModule, name: &str, sig: &FnSig) -> Result<CFunction, ()> {
    let params = sig.params.iter().map(|p| slot(module, &p.ty)).collect::<Result<Vec<_>, _>>()?;
    let ret = match &sig.ret {
        CType::Void => None,
        ty => Some(slot(module, ty)?),
    };
    Ok(CFunction { c_name: name.to_string(), params, ret, variadic: sig.variadic })
}

/// The C details of a type, for the checker.
fn slot(module: &CModule, ty: &CType) -> Result<CSlot, ()> {
    let c_type = spelling(ty, "").ok_or(())?;
    let record = match resolve_typedefs(module, ty) {
        CType::Named(named) if matches!(named.kind, NamedKind::Struct | NamedKind::Union | NamedKind::Typedef) => {
            module.resolve(&named).and_then(|item| match &item.kind {
                ItemKind::Record(_) => item.name().map(str::to_string),
                _ => None,
            })
        }
        _ => None,
    };
    let array = matches!(ty, CType::Array(_));
    let pointer = matches!(resolve_typedefs(module, ty), CType::Pointer(_) | CType::FnPtr(_));
    Ok(CSlot { c_type, record, array, pointer })
}

/// Follows typedefs declared in the import until reaching another type.
fn resolve_typedefs(module: &CModule, ty: &CType) -> CType {
    let mut ty = ty.clone();
    for _ in 0..32 {
        let CType::Named(named) = &ty else { break };
        match module.resolve(named) {
            Some(Item { kind: ItemKind::Typedef(t), .. }) => ty = t.ty.clone(),
            Some(Item { kind: ItemKind::Record(r), .. }) => {
                let name = r.typedef_name.clone().or_else(|| r.tag.clone()).unwrap_or_default();
                let kind = if r.typedef_name.is_some() {
                    NamedKind::Typedef
                } else if r.kind == RecordKind::Union {
                    NamedKind::Union
                } else {
                    NamedKind::Struct
                };
                return CType::Named(Named { kind, name });
            }
            _ => break,
        }
    }
    ty
}

/// How C spells a record type.
fn record_spelling(record: &Record) -> String {
    match (&record.typedef_name, &record.tag, record.kind) {
        (Some(name), _, _) => name.clone(),
        (None, Some(tag), RecordKind::Struct) => format!("struct {tag}"),
        (None, Some(tag), RecordKind::Union) => format!("union {tag}"),
        (None, None, _) => String::new(),
    }
}

/// Spells a C type around the declarator `decl` (empty for a cast), or
/// `None` for types C can't name, like anonymous records.
fn spelling(ty: &CType, decl: &str) -> Option<String> {
    let join = |base: &str, decl: &str| if decl.is_empty() { base.to_string() } else { format!("{base} {decl}") };
    Some(match ty {
        CType::Void => join("void", decl),
        CType::Bool => join("bool", decl),
        CType::Char { .. } => join("char", decl),
        CType::Int(i) => join(&i.spelling, decl),
        CType::Float(f) => join(&f.spelling, decl),
        CType::Pointer(p) => {
            let inner = format!("*{decl}");
            let inner =
                if matches!(p.pointee, CType::Array(_) | CType::Function(_)) { format!("({inner})") } else { inner };
            let base = spelling(&p.pointee, &inner)?;
            if p.pointee_quals.is_const { format!("const {base}") } else { base }
        }
        CType::Array(a) => {
            let len = a.len.map(|n| n.to_string()).unwrap_or_default();
            spelling(&a.element, &format!("{decl}[{len}]"))?
        }
        CType::FnPtr(sig) => spelling(&sig.ret, &format!("(*{decl})({})", param_list(sig)?))?,
        CType::Function(sig) => spelling(&sig.ret, &format!("{decl}({})", param_list(sig)?))?,
        CType::Named(n) => {
            let base = match n.kind {
                NamedKind::Struct => format!("struct {}", n.name),
                NamedKind::Union => format!("union {}", n.name),
                NamedKind::Enum => format!("enum {}", n.name),
                NamedKind::Typedef => n.name.clone(),
            };
            join(&base, decl)
        }
        CType::Record(_) => return None,
        CType::Atomic(inner) => join(&format!("_Atomic({})", spelling(inner, "")?), decl),
        CType::Opaque(s) => join(s, decl),
    })
}

/// The parameter list of a function type, for spelling.
fn param_list(sig: &FnSig) -> Option<String> {
    let mut params: Vec<String> = sig.params.iter().map(|p| spelling(&p.ty, "")).collect::<Option<_>>()?;
    if sig.variadic {
        params.push("...".to_string());
    }
    if params.is_empty() {
        params.push("void".to_string());
    }
    Some(params.join(", "))
}

/// The Wid name of a C integer type: a `C.` name when the widths agree, a
/// sized integer otherwise.
fn int_type(i: &IntType) -> Result<String, String> {
    let known: &[(&str, &str, u32, bool)] = &[
        ("int", "int", 32, true),
        ("unsigned int", "uint", 32, false),
        ("short", "short", 16, true),
        ("unsigned short", "ushort", 16, false),
        ("long", "long", 64, true),
        ("unsigned long", "ulong", 64, false),
        ("long long", "longlong", 64, true),
        ("unsigned long long", "ulonglong", 64, false),
        ("signed char", "schar", 8, true),
        ("unsigned char", "uchar", 8, false),
        ("size_t", "size_t", 64, false),
        ("ssize_t", "ssize_t", 64, true),
        ("ptrdiff_t", "ptrdiff_t", 64, true),
        ("intptr_t", "intptr_t", 64, true),
        ("uintptr_t", "uintptr_t", 64, false),
        ("int8_t", "int8_t", 8, true),
        ("uint8_t", "uint8_t", 8, false),
        ("int16_t", "int16_t", 16, true),
        ("uint16_t", "uint16_t", 16, false),
        ("int32_t", "int32_t", 32, true),
        ("uint32_t", "uint32_t", 32, false),
        ("int64_t", "int64_t", 64, true),
        ("uint64_t", "uint64_t", 64, false),
        ("wchar_t", "wchar_t", 32, true),
    ];
    if let Some((_, name, _, _)) = known
        .iter()
        .find(|(spelling, _, bits, signed)| *spelling == i.spelling && *bits == i.bits && *signed == i.signed)
    {
        return Ok(format!("C.{name}"));
    }
    match i.bits {
        8 | 16 | 32 | 64 => Ok(format!("{}{}", if i.signed { "I" } else { "U" }, i.bits)),
        _ => Err(format!("uses `{}`, which Wid has no type for", i.spelling)),
    }
}

/// Removes the first matching prefix, ignoring case, unless that would leave
/// nothing or a name starting with a digit.
fn strip<'n>(name: &'n str, prefixes: &[String]) -> &'n str {
    let mut prefixes: Vec<&String> = prefixes.iter().collect();
    prefixes.sort_by_key(|p| std::cmp::Reverse(p.len()));
    for prefix in prefixes {
        if name.len() > prefix.len()
            && name.is_char_boundary(prefix.len())
            && name[..prefix.len()].eq_ignore_ascii_case(prefix)
        {
            let rest = name[prefix.len()..].trim_start_matches('_');
            if rest.starts_with(|c: char| c.is_ascii_alphabetic()) {
                return rest;
            }
        }
    }
    name
}

/// `InitWindow` → `init_window`, `GetFPS` → `get_fps`, `logLevel` →
/// `log_level`.
pub fn snake_case(name: &str) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_ascii_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase());
            if prev.is_ascii_lowercase() || prev.is_ascii_digit() || (prev.is_ascii_uppercase() && next_lower) {
                out.push('_');
            }
        }
        out.push(c.to_ascii_lowercase());
    }
    let mut collapsed = String::new();
    for c in out.chars() {
        if c == '_' && collapsed.ends_with('_') {
            continue;
        }
        collapsed.push(c);
    }
    collapsed.trim_matches('_').to_string()
}

/// `io_callbacks` → `IoCallbacks`, `rAudioBuffer` → `RAudioBuffer`.
fn pascal_case(name: &str) -> String {
    name.split('_').filter(|part| !part.is_empty()).map(upper_first).collect()
}

/// Constants keep their spelling unless they start with a lowercase letter,
/// which a Wid constant can't: `default` → `DEFAULT`.
fn constant_case(name: &str) -> String {
    if name.starts_with(|c: char| c.is_ascii_lowercase()) {
        snake_case(name).to_ascii_uppercase()
    } else {
        name.to_string()
    }
}

/// Uppercases the first letter.
fn upper_first(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map(|c| c.to_ascii_uppercase().to_string() + chars.as_str()).unwrap_or_default()
}

/// Lowercases the first letter.
fn lower_first(name: &str) -> String {
    let mut chars = name.chars();
    chars.next().map(|c| c.to_ascii_lowercase().to_string() + chars.as_str()).unwrap_or_default()
}

/// Appends `_` to names that are Wid keywords.
/// A Wid name that is a keyword or a builtin type gets a trailing `_`.
fn escape_keyword(name: String) -> String {
    if Keyword::from_ident(&name).is_some()
        || matches!(name.as_str(), "proc" | "block")
        || wid_sema::is_reserved_type_name(&name)
    {
        name + "_"
    } else {
        name
    }
}

/// Whether C reserves a name for the implementation (`__x`, `_X`), as the C
/// library's internals use; those are left out.
fn is_reserved(name: &str) -> bool {
    name.starts_with("__") || (name.starts_with('_') && name[1..].starts_with(|c: char| c.is_ascii_uppercase()))
}

/// Whether a macro body is a single identifier, as in a function alias
/// `#define GetMouseRay GetScreenToWorldRay`.
fn is_identifier(text: &str) -> bool {
    !text.is_empty()
        && text.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && text.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `file.h:12`.
fn location_text(location: &wid_cimport::Location) -> String {
    let file = location.file.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    format!("{file}:{}", location.line)
}

/// The text of a C comment without its markers, one entry per line.
fn doc_lines(doc: &str) -> Vec<String> {
    let mut lines: Vec<String> = doc
        .lines()
        .map(|line| {
            let line = line.trim();
            let line = line.trim_start_matches("/**").trim_start_matches("/*!").trim_start_matches("/*");
            let line = line.trim_start_matches("///").trim_start_matches("//!").trim_start_matches("//");
            let line = line.trim_end_matches("*/");
            let line = line.trim();
            let line = line.strip_prefix('*').map_or(line, str::trim_start);
            line.trim_end().to_string()
        })
        .collect();
    while lines.first().is_some_and(String::is_empty) {
        lines.remove(0);
    }
    while lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    lines
}

/// A float literal Wid reads back exactly.
fn float_text(value: f64) -> String {
    let text = format!("{value:?}");
    if text.contains(['.', 'e', 'E']) { text } else { format!("{text}.0") }
}

/// A Wid string literal for these bytes.
fn wid_string(bytes: &[u8]) -> String {
    let mut out = String::from("\"");
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\\""),
            b'\\' => out.push_str("\\\\"),
            b'#' => out.push_str("\\#"),
            b'\n' => out.push_str("\\n"),
            b'\t' => out.push_str("\\t"),
            b'\r' => out.push_str("\\r"),
            0x20..=0x7e => out.push(b as char),
            _ => {
                let _ = write!(out, "\\x{b:02X}");
            }
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snake_case_follows_word_boundaries() {
        assert_eq!(snake_case("InitWindow"), "init_window");
        assert_eq!(snake_case("GetFPS"), "get_fps");
        assert_eq!(snake_case("DrawCircleV"), "draw_circle_v");
        assert_eq!(snake_case("logLevel"), "log_level");
        assert_eq!(snake_case("LoadGPUTexture"), "load_gpu_texture");
        assert_eq!(snake_case("Vector2Add"), "vector2_add");
        assert_eq!(snake_case("load"), "load");
    }

    #[test]
    fn prefixes_strip_ignoring_case() {
        let prefixes = vec!["stbi_".to_string()];
        assert_eq!(strip("stbi_load", &prefixes), "load");
        assert_eq!(strip("STBI_default", &prefixes), "default");
        assert_eq!(strip("stbi_", &prefixes), "stbi_");
        assert_eq!(strip("other", &prefixes), "other");
        assert_eq!(constant_case("default"), "DEFAULT");
        assert_eq!(pascal_case("io_callbacks"), "IoCallbacks");
    }

    #[test]
    fn spells_declarators() {
        let int = CType::Int(IntType { bits: 32, signed: true, spelling: "int".into() });
        let ptr = CType::Pointer(Box::new(wid_cimport::PointerType {
            pointee: CType::Char { signed: true },
            pointee_quals: wid_cimport::Quals { is_const: true, ..Default::default() },
        }));
        assert_eq!(spelling(&ptr, "").as_deref(), Some("const char *"));
        let callback = CType::FnPtr(Box::new(FnSig {
            params: vec![wid_cimport::Param { name: None, ty: int.clone(), quals: Default::default() }],
            ret: CType::Void,
            variadic: false,
            prototyped: true,
        }));
        assert_eq!(spelling(&callback, "").as_deref(), Some("void (*)(int)"));
    }
}

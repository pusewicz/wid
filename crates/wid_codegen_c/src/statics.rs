//! Static storage: values computed at compile time and files read by
//! `embed`, emitted as `static` objects with constant initializers.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use wid_sema::ir::{self, ExprKind, GlobalId};
use wid_sema::types::{CConv, FloatTy, TyKind};

use crate::{Gen, call_name, float_literal, visit_block, visit_expr};

impl Gen<'_> {
    /// The globals that the given functions read, with the globals their
    /// initializers refer to.
    pub(crate) fn used_globals(&self, functions: &[&ir::Function]) -> BTreeSet<GlobalId> {
        let mut used = BTreeSet::new();
        let note = |e: &ir::Expr, used: &mut BTreeSet<GlobalId>| {
            if let ExprKind::Global(g) | ExprKind::ConstGlobal(g) = e.kind {
                used.insert(g);
            }
        };
        for f in functions {
            if let Some(body) = &f.body {
                visit_block(body, &mut |e| note(e, &mut used));
            }
        }
        let mut pending: Vec<GlobalId> = used.iter().copied().collect();
        while let Some(g) = pending.pop() {
            if let Some(init) = &self.p.globals[g.0 as usize].init {
                let mut found = BTreeSet::new();
                visit_expr(init, &mut |e| note(e, &mut found));
                for f in found {
                    if used.insert(f) {
                        pending.push(f);
                    }
                }
            }
        }
        used
    }

    /// Defines the static objects of the program's globals, in id order so
    /// every initializer only names objects defined before it.
    pub(crate) fn static_globals(&mut self, used: &BTreeSet<GlobalId>) -> String {
        let mut out = String::new();
        for &id in used {
            let g = &self.p.globals[id.0 as usize];
            if g.foreign || g.comptime_only {
                continue;
            }
            let cty = self.c_type(g.ty);
            let name = g.c_name.clone();
            if let Some(embed) = &g.embed {
                let path = embed.path.display().to_string();
                let n = embed.bytes.len();
                // `#embed` names its file like `#include`, without escapes, so a
                // path that can't be written that way is spelled out as bytes.
                if path.contains(['"', '\\', '\n']) {
                    let bytes: Vec<String> = embed.bytes.iter().map(u8::to_string).collect();
                    let _ = writeln!(out, "static {cty} {name} = {{.data = {{{}}}}};", bytes.join(", "));
                } else {
                    let _ = writeln!(out, "static {cty} {name} = {{.data = {{\n#embed \"{path}\" limit({n})\n}}}};");
                }
                continue;
            }
            let constant = if g.constant { "const " } else { "" };
            let init = match &g.init {
                Some(e) => self.static_init(e),
                None => "{}".into(),
            };
            let _ = writeln!(out, "static {constant}{cty} {name} = {init};");
        }
        out
    }

    /// A constant initializer for a value computed at compile time.
    fn static_init(&mut self, e: &ir::Expr) -> String {
        let base = self.p.types.base(e.ty);
        match &e.kind {
            ExprKind::Int(_) | ExprKind::Bool(_) => self.expr(e),
            ExprKind::Float(v) => {
                let kind = match self.p.types.kind(base) {
                    TyKind::Float(f) => *f,
                    _ => FloatTy::F64,
                };
                float_literal(*v, kind)
            }
            ExprKind::Str(s) => {
                let lit = crate::c_string_literal(s.as_bytes());
                match self.p.types.kind(base) {
                    TyKind::CString => lit,
                    _ => format!("{{(const uint8_t *){lit}, {}}}", s.len()),
                }
            }
            ExprKind::Nil | ExprKind::Zero => "{}".into(),
            ExprKind::FnRef(f) => call_name(&self.p.functions[f.0 as usize]),
            ExprKind::OptSome(inner) => {
                let v = self.static_init(inner);
                if self.p.types.optional_is_pointer(base) { v } else { format!("{{.value = {v}, .has = true}}") }
            }
            ExprKind::UnionWrap { variant, value } => {
                let v = self.static_init(value);
                format!("{{.tag = {}, .as.v{variant} = {v}}}", variant + 1)
            }
            ExprKind::SliceOf { base: array, lo, hi, .. } => {
                let (ExprKind::ConstGlobal(g), ExprKind::Int(lo), ExprKind::Int(hi)) =
                    (&array.kind, &lo.kind, &hi.kind)
                else {
                    return "{}".into();
                };
                let name = &self.p.globals[g.0 as usize].c_name;
                format!("{{.data = {name}.data + {lo}, .len = {}}}", hi - lo)
            }
            ExprKind::Aggregate(elems) => self.static_aggregate(e, elems),
            _ => self.expr(e),
        }
    }

    fn static_aggregate(&mut self, e: &ir::Expr, elems: &[ir::Expr]) -> String {
        let values: Vec<String> = elems.iter().map(|x| self.static_init(x)).collect();
        match self.p.types.kind(e.ty).clone() {
            TyKind::Struct(id) => {
                let info = self.p.types.struct_info(id).clone();
                let parts: Vec<String> = info
                    .fields
                    .iter()
                    .zip(values)
                    .zip(elems)
                    .map(|((f, v), x)| {
                        let name = match (&f.c_name, info.foreign) {
                            (Some(c), _) => c.clone(),
                            (None, true) => f.name.as_str().to_string(),
                            (None, false) => crate::types::field_ident(f.name.as_str()),
                        };
                        let v = if info.foreign && matches!(f.c_conv, Some(CConv::Array | CConv::Mapped(_))) {
                            match &x.kind {
                                ExprKind::Aggregate(inner) => {
                                    let parts: Vec<String> = inner.iter().map(|i| self.static_init(i)).collect();
                                    format!("{{{}}}", parts.join(", "))
                                }
                                _ => "{}".into(),
                            }
                        } else {
                            v
                        };
                        format!(".{name} = {v}")
                    })
                    .collect();
                format!("{{{}}}", parts.join(", "))
            }
            TyKind::Tuple(_) => {
                let parts: Vec<String> = values.iter().enumerate().map(|(i, v)| format!(".f{i} = {v}")).collect();
                format!("{{{}}}", parts.join(", "))
            }
            TyKind::Array(..) | TyKind::Matrix(..) => format!("{{.data = {{{}}}}}", values.join(", ")),
            _ => format!("{{{}}}", values.join(", ")),
        }
    }
}

//! Per-type helper functions generated on demand: printing and equality.

use std::fmt::Write as _;

use wid_sema::types::{FloatTy, TyId, TyKind};

use crate::{Gen, c_string_literal};

impl Gen<'_> {
    /// Returns a C statement that writes `value` (a C expression of type `ty`)
    /// to the writer `w`. `inspect` is a C boolean expression selecting the
    /// quoted form.
    pub(crate) fn print_stmt(&mut self, ty: TyId, value: &str, inspect: &str, w: &str) -> String {
        let base = self.p.types.base(ty);
        match self.p.types.kind(base).clone() {
            TyKind::String => {
                format!("if ({inspect}) wid_w_str_inspect({w}, {value}); else wid_w_str({w}, {value});")
            }
            TyKind::CString => format!("wid_w_cstr({w}, {value});"),
            TyKind::Bool => format!("wid_w_bool({w}, {value});"),
            TyKind::Rune => format!("if ({inspect}) wid_w_rune_inspect({w}, {value}); else wid_w_rune({w}, {value});"),
            TyKind::Float(FloatTy::F32) => format!("wid_w_f32({w}, {value});"),
            TyKind::Float(FloatTy::F64) => format!("wid_w_f64({w}, {value});"),
            TyKind::Int(i) if i.signed() => format!("wid_w_int({w}, (int64_t)({value}));"),
            TyKind::Int(_) => format!("wid_w_uint({w}, (uint64_t)({value}));"),
            TyKind::Pointer(_) | TyKind::MultiPointer(_) | TyKind::RawPtr => {
                format!("wid_w_ptr({w}, (const void *)({value}));")
            }
            TyKind::Optional(_) if self.p.types.optional_is_pointer(base) => {
                format!("wid_w_ptr({w}, (const void *)({value}));")
            }
            TyKind::Unknown | TyKind::Void | TyKind::Never | TyKind::Nil | TyKind::Symbol | TyKind::TypeValue(_) => {
                "(void)0;".into()
            }
            _ => {
                let name = self.print_helper(base);
                format!("{name}({w}, {value}, {inspect});")
            }
        }
    }

    /// Returns a C expression that writes `value` with a fixed inspect mode.
    pub(crate) fn print_expr(&mut self, ty: TyId, value: &str, inspect: bool, w: &str) -> String {
        let base = self.p.types.base(ty);
        match self.p.types.kind(base) {
            TyKind::String if inspect => format!("wid_w_str_inspect({w}, {value})"),
            TyKind::String => format!("wid_w_str({w}, {value})"),
            TyKind::Rune if inspect => format!("wid_w_rune_inspect({w}, {value})"),
            TyKind::Rune => format!("wid_w_rune({w}, {value})"),
            _ => {
                let stmt = self.print_stmt(ty, value, if inspect { "true" } else { "false" }, w);
                stmt.trim_end_matches(';').to_string()
            }
        }
    }

    /// Returns the name of the print helper for an aggregate type, emitting it
    /// the first time.
    fn print_helper(&mut self, ty: TyId) -> String {
        let cty = self.c_type(ty);
        let name = format!("wid_print_{}", cty.replace([' ', '*'], "_"));
        if !self.helpers_done.insert(("print", ty)) {
            return name;
        }
        let _ = writeln!(self.helper_protos, "static void {name}(wid_Writer *w, {cty} v, bool inspect);");
        let mut body = String::new();
        let _ = writeln!(
            body,
            "[[maybe_unused]] static void {name}(wid_Writer *w, {cty} v, [[maybe_unused]] bool inspect) {{"
        );
        let lit = |s: &str| c_string_literal(s.as_bytes());
        match self.p.types.kind(ty).clone() {
            TyKind::Struct(id) => {
                let info = self.p.types.struct_info(id).clone();
                let _ = writeln!(body, "    wid_w_cstr(w, {});", lit(&format!("{}(", info.name)));
                for (i, f) in info.fields.iter().enumerate() {
                    let sep = if i == 0 { "" } else { ", " };
                    let _ = writeln!(body, "    wid_w_cstr(w, {});", lit(&format!("{sep}{}: ", f.name)));
                    let access = self.field_access("v", false, true, ty, i as u32);
                    let stmt = self.print_stmt(f.ty, &access, "true", "w");
                    let _ = writeln!(body, "    {stmt}");
                }
                body.push_str("    wid_w_char(w, ')');\n");
            }
            TyKind::Enum(id) => {
                let info = self.p.types.enum_info(id).clone();
                body.push_str("    switch ((int64_t)v) {\n");
                for (n, val) in &info.members {
                    let plain = lit(n.as_str());
                    let sym = lit(&format!(":{n}"));
                    let _ = writeln!(body, "    case {val}: wid_w_cstr(w, inspect ? {sym} : {plain}); return;");
                }
                body.push_str("    default: break;\n    }\n");
                let _ = writeln!(body, "    wid_w_cstr(w, {});", lit(&format!("{}(", info.name)));
                body.push_str("    wid_w_int(w, (int64_t)v);\n    wid_w_char(w, ')');\n");
            }
            TyKind::Array(elem, n) => {
                body.push_str("    wid_w_char(w, '[');\n");
                let _ = writeln!(body, "    for (wid_Int i = 0; i < {n}; i++) {{");
                body.push_str("        if (i > 0) wid_w_cstr(w, \", \");\n");
                let stmt = self.print_stmt(elem, "v.data[i]", "true", "w");
                let _ = writeln!(body, "        {stmt}");
                body.push_str("    }\n    wid_w_char(w, ']');\n");
            }
            TyKind::Matrix(elem, r, c) => {
                body.push_str("    wid_w_cstr(w, \"matrix[\");\n");
                let _ = writeln!(body, "    for (wid_Int row = 0; row < {r}; row++) {{");
                body.push_str("        if (row > 0) wid_w_cstr(w, \", \");\n        wid_w_char(w, '[');\n");
                let _ = writeln!(body, "        for (wid_Int col = 0; col < {c}; col++) {{");
                body.push_str("            if (col > 0) wid_w_cstr(w, \", \");\n");
                let stmt = self.print_stmt(elem, &format!("v.data[col * {r} + row]"), "true", "w");
                let _ = writeln!(body, "            {stmt}");
                body.push_str("        }\n        wid_w_char(w, ']');\n    }\n    wid_w_char(w, ']');\n");
            }
            TyKind::Slice(elem) | TyKind::Dynamic(elem) => {
                body.push_str("    wid_w_char(w, '[');\n    for (wid_Int i = 0; i < v.len; i++) {\n");
                body.push_str("        if (i > 0) wid_w_cstr(w, \", \");\n");
                let stmt = self.print_stmt(elem, "v.data[i]", "true", "w");
                let _ = writeln!(body, "        {stmt}");
                body.push_str("    }\n    wid_w_char(w, ']');\n");
            }
            TyKind::Map(k, val) => {
                let info = self.map_info(ty);
                let kt = self.c_type(k);
                let vt = self.c_type(val);
                body.push_str("    wid_w_char(w, '{');\n    bool first = true;\n");
                let _ = writeln!(
                    body,
                    "    for (wid_Int i = wid_map_next(&v.raw, &{info}, 0); i >= 0; i = wid_map_next(&v.raw, &{info}, i + 1)) {{"
                );
                body.push_str("        if (!first) wid_w_cstr(w, \", \");\n        first = false;\n");
                let ks = self.print_stmt(k, &format!("*({kt} *)wid_map_key(&v.raw, &{info}, i)"), "true", "w");
                let vs = self.print_stmt(val, &format!("*({vt} *)wid_map_value(&v.raw, &{info}, i)"), "true", "w");
                let _ = writeln!(body, "        {ks}\n        wid_w_cstr(w, \" => \");\n        {vs}");
                body.push_str("    }\n    wid_w_char(w, '}');\n");
            }
            TyKind::Tuple(elems) => {
                body.push_str("    wid_w_char(w, '(');\n");
                for (i, e) in elems.iter().enumerate() {
                    if i > 0 {
                        body.push_str("    wid_w_cstr(w, \", \");\n");
                    }
                    let stmt = self.print_stmt(*e, &format!("v.f{i}"), "true", "w");
                    let _ = writeln!(body, "    {stmt}");
                }
                body.push_str("    wid_w_char(w, ')');\n");
            }
            TyKind::Union(id) => {
                let info = self.p.types.union_info(id).clone();
                body.push_str("    switch (v.tag) {\n");
                for (i, vt) in info.variants.iter().enumerate() {
                    let stmt = self.print_stmt(*vt, &format!("v.as.v{i}"), "inspect", "w");
                    let _ = writeln!(body, "    case {}: {stmt} return;", i + 1);
                }
                body.push_str("    default: wid_w_cstr(w, \"nil\"); return;\n    }\n");
            }
            TyKind::Optional(inner) => {
                let stmt = self.print_stmt(inner, "v.value", "inspect", "w");
                let _ = writeln!(body, "    if (!v.has) {{ wid_w_cstr(w, \"nil\"); return; }}\n    {stmt}");
            }
            TyKind::Error => {
                body.push_str("    switch (v) {\n    case 0: wid_w_cstr(w, \"nil\"); return;\n");
                for (i, n) in self.p.errors.iter().enumerate() {
                    let plain = lit(n.as_str());
                    let sym = lit(&format!(":{n}"));
                    let _ = writeln!(body, "    case {}: wid_w_cstr(w, inspect ? {sym} : {plain}); return;", i + 1);
                }
                body.push_str(
                    "    default: wid_w_cstr(w, \"Error(\"); wid_w_uint(w, v); wid_w_char(w, ')'); return;\n    }\n",
                );
            }
            _ => {
                let shown = self.p.types.display(ty);
                let _ = writeln!(body, "    wid_w_cstr(w, {});", lit(&format!("<{shown}>")));
            }
        }
        body.push_str("}\n\n");
        self.helper_bodies.push_str(&body);
        name
    }

    /// Returns the name of the static `wid_MapInfo` describing a map type.
    pub(crate) fn map_info(&mut self, ty: TyId) -> String {
        let name = format!("wid_mapinfo_{}", ty.0);
        if !self.helpers_done.insert(("mapinfo", ty)) {
            return name;
        }
        let TyKind::Map(k, v) = self.p.types.kind(ty).clone() else { return name };
        let (ks, ka) = self.p.types.layout(k);
        let (vs, va) = self.p.types.layout(v);
        let align = ka.max(va).max(8);
        let key_off = 16u64.div_ceil(ka.max(1)) * ka.max(1);
        let val_off = (key_off + ks).div_ceil(va.max(1)) * va.max(1);
        let slot = (val_off + vs).div_ceil(align) * align;
        let string_key = matches!(self.p.types.kind(self.p.types.base(k)), TyKind::String);
        let _ = writeln!(
            self.helper_protos,
            "static const wid_MapInfo {name} = {{{ks}, {vs}, {slot}, {key_off}, {val_off}, {align}, {string_key}}};"
        );
        name
    }

    /// Returns a C expression comparing `a` and `b` of type `ty` for equality.
    pub(crate) fn eq_expr(&mut self, ty: TyId, a: &str, b: &str) -> String {
        let base = self.p.types.base(ty);
        match self.p.types.kind(base).clone() {
            TyKind::String => format!("wid_string_eq({a}, {b})"),
            TyKind::Struct(_) | TyKind::Array(..) | TyKind::Matrix(..) | TyKind::Tuple(_) => {
                let name = self.eq_helper(base);
                format!("{name}({a}, {b})")
            }
            TyKind::Optional(_) if !self.p.types.optional_is_pointer(base) => {
                let name = self.eq_helper(base);
                format!("{name}({a}, {b})")
            }
            _ => format!("({a} == {b})"),
        }
    }

    fn eq_helper(&mut self, ty: TyId) -> String {
        let cty = self.c_type(ty);
        let name = format!("wid_eq_{}", cty.replace([' ', '*'], "_"));
        if !self.helpers_done.insert(("eq", ty)) {
            return name;
        }
        let _ = writeln!(self.helper_protos, "static bool {name}({cty} a, {cty} b);");
        let mut body = String::new();
        let _ = writeln!(body, "[[maybe_unused]] static bool {name}({cty} a, {cty} b) {{");
        match self.p.types.kind(ty).clone() {
            TyKind::Struct(id) => {
                let info = self.p.types.struct_info(id).clone();
                for (i, f) in info.fields.iter().enumerate() {
                    let a = self.field_access("a", false, true, ty, i as u32);
                    let b = self.field_access("b", false, true, ty, i as u32);
                    let cmp = self.eq_expr(f.ty, &a, &b);
                    let _ = writeln!(body, "    if (!{cmp}) return false;");
                }
            }
            TyKind::Tuple(elems) => {
                for (i, e) in elems.iter().enumerate() {
                    let cmp = self.eq_expr(*e, &format!("a.f{i}"), &format!("b.f{i}"));
                    let _ = writeln!(body, "    if (!{cmp}) return false;");
                }
            }
            TyKind::Optional(inner) => {
                let cmp = self.eq_expr(inner, "a.value", "b.value");
                let _ = writeln!(
                    body,
                    "    if (a.has != b.has) return false;\n    if (!a.has) return true;\n    return {cmp};"
                );
            }
            TyKind::Matrix(elem, r, c) => {
                let cmp = self.eq_expr(elem, "a.data[i]", "b.data[i]");
                let n = r * c;
                let _ = writeln!(body, "    for (wid_Int i = 0; i < {n}; i++) if (!{cmp}) return false;");
            }
            TyKind::Array(elem, n) => {
                let cmp = self.eq_expr(elem, "a.data[i]", "b.data[i]");
                let _ = writeln!(body, "    for (wid_Int i = 0; i < {n}; i++) if (!{cmp}) return false;");
            }
            _ => {}
        }
        body.push_str("    return true;\n}\n\n");
        self.helper_bodies.push_str(&body);
        name
    }
}

impl Gen<'_> {
    /// Returns the name of an element-wise operator helper for numeric arrays,
    /// where either side may be a scalar.
    /// Emits (once) a helper for a matrix-matrix, matrix-vector or
    /// vector-matrix product. Matrices are column-major: element (row, col)
    /// lives at `data[col * rows + row]`.
    pub(crate) fn matrix_product_helper(&mut self, l: TyId, r: TyId, result: TyId) -> String {
        let lt = self.c_type(l);
        let rt = self.c_type(r);
        let res = self.c_type(result);
        let name = format!("wid_matmul_{}_{}", lt.replace([' ', '*'], "_"), rt.replace([' ', '*'], "_"));
        if !self.vector_helpers.insert(name.clone()) {
            return name;
        }
        let (elem, body) = match (self.p.types.kind(l).clone(), self.p.types.kind(r).clone()) {
            (TyKind::Matrix(e, rows, k), TyKind::Matrix(_, _, cols)) => (
                e,
                format!(
                    "    for (wid_Int col = 0; col < {cols}; col++) {{\n        for (wid_Int row = 0; row < {rows}; row++) {{\n            ELEM s = 0;\n            for (wid_Int k = 0; k < {k}; k++) s += a.data[k * {rows} + row] * b.data[col * {k} + k];\n            r.data[col * {rows} + row] = s;\n        }}\n    }}\n"
                ),
            ),
            (TyKind::Matrix(e, rows, cols), _) => (
                e,
                format!(
                    "    for (wid_Int row = 0; row < {rows}; row++) {{\n        ELEM s = 0;\n        for (wid_Int k = 0; k < {cols}; k++) s += a.data[k * {rows} + row] * b.data[k];\n        r.data[row] = s;\n    }}\n"
                ),
            ),
            (_, TyKind::Matrix(e, rows, cols)) => (
                e,
                format!(
                    "    for (wid_Int col = 0; col < {cols}; col++) {{\n        ELEM s = 0;\n        for (wid_Int k = 0; k < {rows}; k++) s += a.data[k] * b.data[col * {rows} + k];\n        r.data[col] = s;\n    }}\n"
                ),
            ),
            _ => unreachable!("matrix_product_helper needs a matrix operand"),
        };
        let et = self.c_type(elem);
        let body = body.replace("ELEM", &et);
        let _ = writeln!(self.helper_protos, "static {res} {name}({lt} a, {rt} b);");
        let _ = writeln!(
            self.helper_bodies,
            "[[maybe_unused]] static {res} {name}({lt} a, {rt} b) {{\n    {res} r = {{}};\n{body}    return r;\n}}\n"
        );
        name
    }

    pub(crate) fn vector_helper(&mut self, op: wid_sema::ir::BinaryOp, l: TyId, r: TyId, result: TyId) -> String {
        use wid_sema::ir::BinaryOp as B;
        let (elem, n) = match (self.p.types.kind(l).clone(), self.p.types.kind(r).clone()) {
            (TyKind::Array(e, n), _) | (_, TyKind::Array(e, n)) => (e, n),
            (TyKind::Matrix(e, rows, cols), _) | (_, TyKind::Matrix(e, rows, cols)) => (e, u64::from(rows * cols)),
            _ => (l, 1),
        };
        let (sym, key) = match op {
            B::Add => ("+", "vadd"),
            B::Sub => ("-", "vsub"),
            B::Mul => ("*", "vmul"),
            B::Div => ("/", "vdiv"),
            B::Rem => ("%", "vrem"),
            B::BitAnd => ("&", "vand"),
            B::BitOr => ("|", "vor"),
            _ => ("^", "vxor"),
        };
        let lt = self.c_type(l);
        let rt = self.c_type(r);
        let res = self.c_type(result);
        let name = format!("wid_{key}_{}_{}", lt.replace([' ', '*'], "_"), rt.replace([' ', '*'], "_"));
        if !self.vector_helpers.insert(name.clone()) {
            return name;
        }
        let la = if matches!(self.p.types.kind(l), TyKind::Array(..) | TyKind::Matrix(..)) { "a.data[i]" } else { "a" };
        let ra = if matches!(self.p.types.kind(r), TyKind::Array(..) | TyKind::Matrix(..)) { "b.data[i]" } else { "b" };
        let is_float = matches!(self.p.types.kind(self.p.types.base(elem)), TyKind::Float(_));
        let et = self.c_type(elem);
        let body_op = if is_float && op == B::Rem {
            format!("({et})fmod({la}, {ra})")
        } else if !is_float && matches!(op, B::Div | B::Rem) {
            let s = crate::types::int_suffix_of(&self.p.types, elem);
            let f = if op == B::Div { "div" } else { "rem" };
            format!("wid_{f}_{s}({la}, {ra}, (wid_Location){{}})")
        } else {
            format!("({et})({la} {sym} {ra})")
        };
        let _ = writeln!(self.helper_protos, "static {res} {name}({lt} a, {rt} b);");
        let _ = writeln!(
            self.helper_bodies,
            "[[maybe_unused]] static {res} {name}({lt} a, {rt} b) {{\n    {res} r;\n    for (wid_Int i = 0; i < {n}; i++) r.data[i] = {body_op};\n    return r;\n}}\n"
        );
        name
    }
}

//! Crossing between Wid's C types and an imported header's: includes,
//! casts, `memcpy` conversions for `types:` mappings, and fields of C
//! structs.

use std::fmt::Write as _;

use wid_sema::ir::{self, CInclude, ExprKind};
use wid_sema::types::{CConv, TyId, TyKind};

use crate::{Gen, strip_parens};

/// The `#define`s and `#include`s for the imported headers. Implementation
/// macros (`implement:`) are not defined here: the driver compiles each
/// implementation in a translation unit of its own.
pub(crate) fn c_includes(includes: &[CInclude]) -> String {
    let mut out = String::new();
    for inc in includes {
        for d in &inc.defines {
            let (name, value) = d.split_once('=').unwrap_or((d, ""));
            let _ = writeln!(out, "#define {name} {value}");
        }
        let _ = writeln!(out, "#include {}", inc.header);
    }
    out
}

/// The source of the translation unit that compiles a header-only
/// library's implementation: its macros, the implementation macro and the
/// header.
pub fn implementation_unit(include: &CInclude) -> Option<String> {
    let implement = include.implement.as_ref()?;
    let mut out = String::from(crate::GENERATED_HEADER);
    out.push('\n');
    for d in &include.defines {
        let (name, value) = d.split_once('=').unwrap_or((d, ""));
        let _ = writeln!(out, "#define {name} {value}");
    }
    let _ = writeln!(out, "#define {implement}");
    let _ = writeln!(out, "#include {}", include.header);
    Some(out)
}

/// Whether an expression names an object C can take the address of.
pub(crate) fn is_lvalue(e: &ir::Expr) -> bool {
    match &e.kind {
        ExprKind::Local(_) | ExprKind::Deref(_) | ExprKind::Index { .. } => true,
        ExprKind::Field { base, .. } => matches!(base.kind, ExprKind::Deref(_)) || is_lvalue(base),
        _ => false,
    }
}

impl Gen<'_> {
    /// Accesses field `index` of a struct of type `ty` written as `base`
    /// (through `->` when `arrow`), converting a C struct's field to its
    /// Wid type where the two differ.
    pub(crate) fn field_access(&mut self, base: &str, arrow: bool, lvalue: bool, ty: TyId, index: u32) -> String {
        let name = self.field_name(ty, index);
        let access = if arrow { format!("{base}->{name}") } else { format!("{base}.{name}") };
        let TyKind::Struct(id) = *self.p.types.kind(ty) else { return access };
        let field = self.p.types.struct_info(id).fields[index as usize].clone();
        let Some(conv) = field.c_conv else { return access };
        let wid = self.c_type(field.ty);
        match conv {
            CConv::Array => format!("(*({wid} *){access})"),
            CConv::Pointer(_) | CConv::Mapped(_) if lvalue || arrow => format!("(*({wid} *)&{access})"),
            CConv::Pointer(_) => format!("(({wid}){access})"),
            CConv::Mapped(c) => {
                let helper = self.mapped_helper(&c, field.ty, false);
                format!("{helper}({access})")
            }
        }
    }

    /// Converts a Wid value of type `ty` to the C type a header expects.
    pub(crate) fn conv_to_c(&mut self, conv: &CConv, value: &str, ty: TyId) -> String {
        match conv {
            CConv::Pointer(c) => format!("(({c}){})", value),
            CConv::Mapped(c) => {
                let helper = self.mapped_helper(c, ty, true);
                format!("{helper}({})", strip_parens(value))
            }
            CConv::Array => value.to_string(),
        }
    }

    /// Converts a value a header produced to the Wid type `ty`.
    pub(crate) fn conv_from_c(&mut self, conv: &CConv, value: &str, ty: TyId) -> String {
        let wid = self.c_type(ty);
        match conv {
            CConv::Pointer(_) => format!("(({wid}){value})"),
            CConv::Array => format!("(*({wid} *){value})"),
            CConv::Mapped(c) => {
                let helper = self.mapped_helper(c, ty, false);
                format!("{helper}({})", strip_parens(value))
            }
        }
    }

    /// Emits (once) the function that copies between the C type `c` and the
    /// Wid type `ty` that `types:` maps it to, and returns its name.
    fn mapped_helper(&mut self, c: &str, ty: TyId, to_c: bool) -> String {
        let wid = self.c_type(ty);
        let tag: String = c.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' }).collect();
        let wid_tag: String = wid.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' }).collect();
        let name = if to_c { format!("wid_to_{tag}_from_{wid_tag}") } else { format!("wid_from_{tag}_to_{wid_tag}") };
        if self.vector_helpers.insert(name.clone()) {
            let (from, to) = if to_c { (wid.as_str(), c) } else { (c, wid.as_str()) };
            let _ = writeln!(
                self.helper_bodies,
                "static_assert(sizeof({c}) == sizeof({wid}) && alignof({c}) == alignof({wid}), \"`types:` maps `{c}` to a Wid type of a different layout\");"
            );
            let _ = writeln!(
                self.helper_bodies,
                "[[maybe_unused]] static inline {to} {name}({from} v) {{\n    {to} r;\n    memcpy(&r, &v, sizeof r);\n    return r;\n}}"
            );
        }
        name
    }

    /// Builds a value of an `@[extern]` struct. Fields whose C type differs
    /// from Wid's are converted; arrays and mapped records are copied in by a
    /// helper, since C can't initialize them from a Wid value.
    pub(crate) fn foreign_aggregate(&mut self, ty: TyId, elems: &[ir::Expr], values: Vec<String>) -> String {
        let cty = self.c_type(ty);
        let TyKind::Struct(id) = *self.p.types.kind(ty) else { unreachable!("foreign_aggregate on a non-struct") };
        let info = self.p.types.struct_info(id).clone();
        let needs_helper = info.fields.iter().any(|f| matches!(f.c_conv, Some(CConv::Array | CConv::Mapped(_))));
        if !needs_helper {
            // Fields left at zero are omitted: C zeroes them, and naming two
            // members of a C union would overwrite one with the other.
            let parts: Vec<String> = info
                .fields
                .iter()
                .zip(values)
                .zip(elems)
                .filter(|(_, e)| !matches!(e.kind, ExprKind::Zero))
                .map(|((f, v), _)| {
                    let name = f.c_name.clone().unwrap_or_else(|| f.name.as_str().to_string());
                    match &f.c_conv {
                        Some(CConv::Pointer(c)) => format!(".{name} = ({c})({v})"),
                        _ => format!(".{name} = {v}"),
                    }
                })
                .collect();
            return format!("(({cty}){{{}}})", parts.join(", "));
        }
        let tag: String = cty.chars().map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '_' }).collect();
        let name = format!("wid_make_{tag}");
        if self.vector_helpers.insert(name.clone()) {
            let mut params = Vec::new();
            let mut body = String::new();
            for (i, f) in info.fields.iter().enumerate() {
                let wid = self.c_type(f.ty);
                params.push(format!("{wid} a{i}"));
                let field = f.c_name.clone().unwrap_or_else(|| f.name.as_str().to_string());
                let line = match &f.c_conv {
                    Some(CConv::Array | CConv::Mapped(_)) => {
                        format!("    memcpy(&r.{field}, &a{i}, sizeof r.{field});")
                    }
                    Some(CConv::Pointer(c)) => format!("    r.{field} = ({c})a{i};"),
                    None => format!("    r.{field} = a{i};"),
                };
                body.push_str(&line);
                body.push('\n');
            }
            let _ = writeln!(
                self.helper_bodies,
                "[[maybe_unused]] static inline {cty} {name}({}) {{\n    {cty} r = {{}};\n{body}    return r;\n}}",
                params.join(", ")
            );
        }
        let args: Vec<&str> = values.iter().map(|v| strip_parens(v)).collect();
        format!("{name}({})", args.join(", "))
    }
}

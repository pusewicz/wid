//! Turning compile-time values into constants of the run-time program, and
//! the rules for what may cross from compile time to run time.
//!
//! Numbers, booleans, strings, enums, procs that name Wid methods, and fixed
//! arrays, structs, tuples, optionals and unions of those cross as they are.
//! A slice crosses with a copy of its elements in static storage. Pointers
//! into compile-time memory, dynamic arrays, maps, allocators and `Type`
//! values don't: they would point at memory that only exists in the compiler.

use super::memory::{Addr, FN_TAG, Memory, SHIFT};
use super::{i64_at, int_from, slice, word};
use crate::ir::{self, ExprKind};
use crate::types::{FloatTy, TyId, TyKind, TypeTable};

/// A value that can't be used at run time.
#[derive(Clone, Debug)]
pub(crate) struct Escape {
    /// What the value holds, like "a pointer into compile-time memory".
    pub what: String,
    /// Where inside the value, like "field `next` of `Node`"; empty for the
    /// value itself.
    pub path: String,
    /// How to fix it.
    pub help: String,
}

/// The result of crossing to run time: an expression, and the globals it
/// needs, which the caller registers starting at the id it passed in.
pub(crate) struct Converted {
    /// The value as an IR constant.
    pub expr: ir::Expr,
    /// New read-only data, in order: the global ids `first..first + len`.
    pub globals: Vec<ir::Global>,
}

/// The offset of a union's payload, after its 4-byte tag.
pub(super) fn union_payload(types: &TypeTable, ty: TyId) -> u64 {
    match types.kind(types.base(ty)) {
        TyKind::Union(id) => {
            let align = types.union_info(*id).variants.iter().map(|v| types.align_of(*v)).max().unwrap_or(1).max(1);
            4u64.div_ceil(align) * align
        }
        _ => 0,
    }
}

struct Conv<'a> {
    types: &'a mut TypeTable,
    mem: &'a Memory,
    first: u32,
    globals: Vec<ir::Global>,
    path: Vec<String>,
}

/// Converts the bytes of a compile-time value of type `ty` into a constant.
/// New globals are numbered from `first`.
pub(crate) fn to_ir(
    types: &mut TypeTable,
    mem: &Memory,
    ty: TyId,
    bytes: &[u8],
    first: u32,
) -> Result<Converted, Escape> {
    let mut conv = Conv { types, mem, first, globals: Vec::new(), path: Vec::new() };
    let expr = conv.value(ty, bytes)?;
    Ok(Converted { expr, globals: conv.globals })
}

impl Conv<'_> {
    fn escape(&self, what: impl Into<String>, help: impl Into<String>) -> Escape {
        Escape { what: what.into(), path: self.path.join(""), help: help.into() }
    }

    fn read(&self, addr: Addr, len: u64) -> Result<Vec<u8>, Escape> {
        self.mem.read(addr, len).map(<[u8]>::to_vec).map_err(|_| {
            self.escape(
                "a pointer to memory that was freed or never existed",
                "return the data itself, not a pointer to it",
            )
        })
    }

    fn value(&mut self, ty: TyId, v: &[u8]) -> Result<ir::Expr, Escape> {
        let base = self.types.base(ty);
        let kind = self.types.kind(base).clone();
        let new = |k: ExprKind| ir::Expr::new(k, ty);
        Ok(match kind {
            TyKind::Int(i) => new(ExprKind::Int(int_from(v, i.size(), i.signed()))),
            TyKind::Enum(id) => {
                let b = self.types.enum_info(id).backing;
                new(ExprKind::Int(int_from(v, b.size(), b.signed())))
            }
            TyKind::Rune => new(ExprKind::Int(int_from(v, 4, true))),
            TyKind::Error => new(ExprKind::Int(int_from(v, 4, false))),
            TyKind::TypeId => new(ExprKind::Int(int_from(v, 8, false))),
            TyKind::Float(FloatTy::F32) => new(ExprKind::Float(f64::from(f32::from_le_bytes(word(v))))),
            TyKind::Float(FloatTy::F64) => new(ExprKind::Float(f64::from_le_bytes(word(v)))),
            TyKind::Bool => new(ExprKind::Bool(v.first().copied().unwrap_or(0) != 0)),
            TyKind::Void | TyKind::Never | TyKind::Unknown | TyKind::Nil => new(ExprKind::Zero),
            TyKind::Type => {
                return Err(self.escape(
                    "a `Type`, which exists only at compile time",
                    "use the information you need from it instead, like `t.name` or `t.size`",
                ));
            }
            TyKind::Symbol => {
                return Err(self.escape(
                    "a `Symbol`, which exists only at compile time",
                    "use its name as a string instead, like `name.to_s`",
                ));
            }
            TyKind::Code => {
                return Err(self.escape(
                    "a `Code` value, which exists only while macros run",
                    "return the code from a `macro def`, and call the macro where the code should go",
                ));
            }
            TyKind::String => {
                let len = i64_at(v, 8).max(0) as u64;
                let bytes = if len == 0 { Vec::new() } else { self.read(u64::from_le_bytes(word(v)), len)? };
                match String::from_utf8(bytes) {
                    Ok(s) => new(ExprKind::Str(s)),
                    Err(_) => {
                        return Err(self.escape(
                            "a string that is not valid UTF-8",
                            "keep binary data in a `[]U8` instead of a `String`",
                        ));
                    }
                }
            }
            TyKind::CString => {
                let p = u64::from_le_bytes(word(v));
                if p == 0 {
                    return Ok(new(ExprKind::Nil));
                }
                let bytes = self
                    .mem
                    .read_cstr(p)
                    .map_err(|_| self.escape("a `CString` that points at freed memory", "return a `String` instead"))?;
                match String::from_utf8(bytes) {
                    Ok(s) => new(ExprKind::Str(s)),
                    Err(_) => return Err(self.escape("a `CString` that is not valid UTF-8", "return a `[]U8` instead")),
                }
            }
            TyKind::Proc(_) => {
                let p = u64::from_le_bytes(word(v));
                match p >> SHIFT {
                    _ if p == 0 => new(ExprKind::Nil),
                    FN_TAG => new(ExprKind::FnRef(ir::FnId((p & ((1 << SHIFT) - 1)) as u32))),
                    _ => {
                        return Err(self.escape(
                            "a procedure of the compiler (like the compile-time allocator)",
                            "build allocators and contexts at run time",
                        ));
                    }
                }
            }
            TyKind::Pointer(_) | TyKind::MultiPointer(_) | TyKind::RawPtr => {
                if u64::from_le_bytes(word(v)) == 0 {
                    return Ok(new(ExprKind::Nil));
                }
                return Err(self.escape(
                    "a pointer into compile-time memory",
                    "return the data itself (a struct, a fixed array or a slice) instead of a pointer to it",
                ));
            }
            TyKind::Optional(inner) => {
                if self.types.optional_is_pointer(base) {
                    if u64::from_le_bytes(word(v)) == 0 {
                        return Ok(new(ExprKind::Nil));
                    }
                    let value = self.value(inner, v)?;
                    return Ok(new(ExprKind::OptSome(Box::new(value))));
                }
                let s = self.types.size_of(inner);
                if v.get(s as usize).copied().unwrap_or(0) == 0 {
                    return Ok(new(ExprKind::Nil));
                }
                let value = self.value(inner, &slice(v, 0, s))?;
                new(ExprKind::OptSome(Box::new(value)))
            }
            TyKind::Struct(id) => {
                if base == self.types.allocator_ty || base == self.types.context_ty || base == self.types.logger_ty {
                    let shown = self.types.display(base);
                    return Err(self.escape(
                        format!("a compile-time `{shown}`"),
                        "allocators, loggers and contexts made at compile time can't be used at run time; create them in run-time code",
                    ));
                }
                let info = self.types.struct_info(id).clone();
                let mut elems = Vec::with_capacity(info.fields.len());
                for f in &info.fields {
                    self.path.push(format!(".{}", f.name));
                    let size = self.types.size_of(f.ty);
                    elems.push(self.value(f.ty, &slice(v, f.offset, size))?);
                    self.path.pop();
                }
                new(ExprKind::Aggregate(elems))
            }
            TyKind::Tuple(ts) => {
                let parts: Vec<(u64, u64)> = ts.iter().map(|t| self.types.layout(*t)).collect();
                let mut elems = Vec::with_capacity(ts.len());
                for (i, (t, off)) in ts.iter().zip(crate::types::offsets(&parts)).enumerate() {
                    self.path.push(format!(".{i}"));
                    let size = self.types.size_of(*t);
                    elems.push(self.value(*t, &slice(v, off, size))?);
                    self.path.pop();
                }
                new(ExprKind::Aggregate(elems))
            }
            TyKind::Array(elem, n) => new(ExprKind::Aggregate(self.elements(elem, v, n)?)),
            TyKind::Matrix(elem, r, c) => {
                new(ExprKind::Aggregate(self.elements(elem, v, u64::from(r) * u64::from(c))?))
            }
            TyKind::Union(id) => {
                let tag = u32::from_le_bytes(word(v));
                let variants = self.types.union_info(id).variants.clone();
                let Some(&vt) = tag.checked_sub(1).and_then(|i| variants.get(i as usize)) else {
                    return Ok(new(ExprKind::Nil));
                };
                let at = union_payload(self.types, base);
                let size = self.types.size_of(vt);
                let value = self.value(vt, &slice(v, at, size))?;
                new(ExprKind::UnionWrap { variant: tag - 1, value: Box::new(value) })
            }
            TyKind::Slice(elem) => {
                let len = i64_at(v, 8).max(0) as u64;
                if len == 0 {
                    return Ok(new(ExprKind::Zero));
                }
                let size = self.types.size_of(elem);
                let data = self.read(u64::from_le_bytes(word(v)), len * size)?;
                let elems = self.elements(elem, &data, len)?;
                let array = self.types.intern(TyKind::Array(elem, len));
                let id = ir::GlobalId(self.first + self.globals.len() as u32);
                self.globals.push(ir::Global {
                    c_name: format!("wid_const_{}", id.0),
                    ty: array,
                    init: Some(ir::Expr::new(ExprKind::Aggregate(elems), array)),
                    constant: false,
                    foreign: false,
                    c_conv: None,
                    embed: None,
                    comptime_only: false,
                });
                let int = self.types.int();
                ir::Expr::new(
                    ExprKind::SliceOf {
                        base: Box::new(ir::Expr::new(ExprKind::ConstGlobal(id), array)),
                        lo: Box::new(ir::Expr::new(ExprKind::Int(0), int)),
                        hi: Box::new(ir::Expr::new(ExprKind::Int(i128::from(len)), int)),
                        checked: false,
                        span: Default::default(),
                    },
                    ty,
                )
            }
            TyKind::Dynamic(_) => {
                return Err(self.escape(
                    "a dynamic array, whose storage belongs to a compile-time allocator",
                    "return a fixed array (`[N]T`) or a slice (`[]T`) of the elements instead",
                ));
            }
            TyKind::Map(..) => {
                return Err(self.escape(
                    "a map, whose storage belongs to a compile-time allocator",
                    "return the entries as a fixed array or slice of structs, and build the map at run time",
                ));
            }
            TyKind::Any => {
                return Err(self.escape("an `Any`, which points into compile-time memory", "return the value itself"));
            }
            _ => {
                let shown = self.types.display(ty);
                return Err(self.escape(
                    format!("a value of type `{shown}`, which can't be stored in the program"),
                    "return a number, string, struct, array or slice instead",
                ));
            }
        })
    }

    fn elements(&mut self, elem: TyId, v: &[u8], n: u64) -> Result<Vec<ir::Expr>, Escape> {
        let size = self.types.size_of(elem);
        let mut out = Vec::with_capacity(n as usize);
        for i in 0..n {
            self.path.push(format!("[{i}]"));
            out.push(self.value(elem, &slice(v, i * size, size))?);
            self.path.pop();
        }
        Ok(out)
    }
}

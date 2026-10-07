//! Builtin operations, allocators, containers and the C functions the
//! interpreter implements itself. Each mirrors its counterpart in
//! `runtime/wid_runtime.h`, so compile-time results match run-time ones.

use wid_diagnostics::Span;

use super::memory::{Addr, Region};
use super::{FailKind, HEAP_PROC, Interp, LOG_PROC, Num, R, TEMP_PROC, i64_at, slice, word};
use crate::ir::{self, Builtin};
use crate::types::{TyId, TyKind};

/// Allocator modes, as `wid_AllocMode` numbers them.
const MODE_ALLOC: u8 = 0;
const MODE_FREE: u8 = 1;
const MODE_FREE_ALL: u8 = 2;
const MODE_RESIZE: u8 = 3;

/// The slot layout of one map type, as `wid_MapInfo` describes it.
#[derive(Clone, Copy, Debug)]
struct MapInfo {
    key_size: u64,
    slot_size: u64,
    key_offset: u64,
    value_offset: u64,
    align: u64,
    string_key: bool,
}

impl<'c> Interp<'c> {
    /// Runs a builtin operation.
    pub(super) fn builtin(&mut self, op: Builtin, args: &'c [ir::Expr], span: Span, ty: TyId) -> R<Vec<u8>> {
        use Builtin as B;
        let arg = |i: usize| &args[i];
        match op {
            B::Print { newline, inspect } => {
                for a in args {
                    let v = self.eval(a)?;
                    let mut text = Vec::new();
                    self.format(a.ty, &v, inspect, &mut text)?;
                    self.output.extend_from_slice(&text);
                }
                if newline {
                    self.output.push(b'\n');
                }
                Ok(Vec::new())
            }
            B::Panic => {
                let v = self.eval(arg(0))?;
                let msg = self.string_bytes(&v)?;
                Err(self.fail_at(span, format!("panic: {}", String::from_utf8_lossy(&msg))))
            }
            B::Assert => {
                let c = self.eval(arg(0))?;
                if Self::truthy(&c) {
                    return Ok(Vec::new());
                }
                let v = self.eval(arg(1))?;
                let msg = self.string_bytes(&v)?;
                Err(self.fail_at(span, String::from_utf8_lossy(&msg).into_owned()))
            }
            B::SizeOf => Ok(self.encode(ty, Num::I(i128::from(self.size(arg(0).ty))))),
            B::Len => {
                let v = self.eval(arg(0))?;
                let n = match self.kind(arg(0).ty) {
                    TyKind::Array(_, n) => i128::from(*n),
                    _ => i128::from(i64_at(&v, 8)),
                };
                Ok(self.encode(ty, Num::I(n)))
            }
            B::TypeInfo => {
                Err(self.fail_at(span, "`type_info` describes values at run time; it can't run at compile time"))
            }
            B::BuilderNew => {
                let a = self.eval(arg(0))?;
                let mut w = vec![0u8; 48];
                w[32..48].copy_from_slice(&slice(&a, 0, 16));
                Ok(w)
            }
            B::Write { inspect } => {
                let w = Self::ptr(&self.eval(arg(0))?);
                let v = self.eval(arg(1))?;
                let mut text = Vec::new();
                self.format(arg(1).ty, &v, inspect, &mut text)?;
                self.writer_write(w, &text, span)?;
                Ok(Vec::new())
            }
            B::BuilderString => {
                let w = Self::ptr(&self.eval(arg(0))?);
                let buf = self.rd_u64(w + 8)?;
                let len = self.rd_u64(w + 16)?;
                Ok(pair(buf, len))
            }
            B::DefaultContext => self.rd(self.default_ctx, 64),
            B::Alloc => {
                let TyKind::Pointer(inner) = self.p.types.kind(ty) else { return Ok(vec![0; 8]) };
                let (size, align) = self.p.types.layout(*inner);
                let a = self.eval(arg(0))?;
                let p = self.alloc_with(&a, size as i64, align as i64, span)?;
                Ok(p.to_le_bytes().to_vec())
            }
            B::AllocSlice => {
                let TyKind::Slice(inner) = self.p.types.kind(ty) else { return Ok(vec![0; 16]) };
                let (size, align) = self.p.types.layout(*inner);
                let n = self.eval(arg(0))?;
                let n = self.int(arg(0).ty, &n);
                let a = self.eval(arg(1))?;
                let bytes = match n.checked_mul(i128::from(size)) {
                    Some(b) if n >= 0 && b <= i128::from(i64::MAX) => b as i64,
                    _ => return Err(self.fail_at(span, format!("cannot allocate {n} elements of {size} bytes"))),
                };
                let p = self.alloc_with(&a, bytes, align as i64, span)?;
                Ok(pair(p, n as u64))
            }
            B::Free => self.free_builtin(args, span),
            B::FreeAll => {
                let a = self.eval(arg(0))?;
                if Self::ptr(&a) != 0 {
                    self.allocator_call(&a, MODE_FREE_ALL, 0, 0, 0, 0, span)?;
                }
                Ok(Vec::new())
            }
            B::ToCString => {
                let s = self.eval(arg(0))?;
                let a = self.eval(arg(1))?;
                let bytes = self.string_bytes(&s)?;
                let out = self.alloc_with(&a, bytes.len() as i64 + 1, 1, span)?;
                self.wr(out, &bytes)?;
                self.wr(out + bytes.len() as u64, &[0])?;
                Ok(out.to_le_bytes().to_vec())
            }
            B::DynPush | B::DynInsert => {
                let d = Self::ptr(&self.eval(arg(0))?);
                let (size, align) = self.elem_layout(arg(0).ty);
                let index = if op == B::DynInsert {
                    let i = self.eval(arg(1))?;
                    Some(self.int(arg(1).ty, &i) as i64)
                } else {
                    None
                };
                let slot = self.dyn_insert(d, index, size, align, span)?;
                Ok(slot.to_le_bytes().to_vec())
            }
            B::DynRemove => {
                let d = Self::ptr(&self.eval(arg(0))?);
                let (size, _) = self.elem_layout(arg(0).ty);
                let i = self.eval(arg(1))?;
                let i = self.int(arg(1).ty, &i) as i64;
                let len = self.rd_u64(d + 8)? as i64;
                self.check_index(i128::from(i), i128::from(len), span)?;
                let data = self.rd_u64(d)?;
                let tail = (len - i - 1) as u64 * size;
                self.mem
                    .copy(data + i as u64 * size, data + (i as u64 + 1) * size, tail)
                    .map_err(|e| self.mem_fail(e, span))?;
                self.wr_u64(d + 8, (len - 1) as u64)?;
                Ok(Vec::new())
            }
            B::DynReserve => {
                let d = Self::ptr(&self.eval(arg(0))?);
                let (size, align) = self.elem_layout(arg(0).ty);
                let n = self.eval(arg(1))?;
                let n = self.int(arg(1).ty, &n) as i64;
                self.dyn_reserve(d, n, size, align, span)?;
                Ok(Vec::new())
            }
            B::DynAppend => {
                let d = Self::ptr(&self.eval(arg(0))?);
                let (size, align) = self.elem_layout(arg(0).ty);
                let xs = self.eval(arg(1))?;
                let (src, count) = (Self::ptr(&xs), i64_at(&xs, 8));
                if count <= 0 {
                    return Ok(Vec::new());
                }
                let items = self.rd(src, count as u64 * size)?;
                let len = self.rd_u64(d + 8)? as i64;
                let need = len + count;
                let cap = self.rd_u64(d + 16)? as i64;
                if need > cap {
                    let mut new_cap = if cap > 0 { cap } else { 8 };
                    while new_cap < need {
                        new_cap *= 2;
                    }
                    self.dyn_reserve(d, new_cap, size, align, span)?;
                }
                let data = self.rd_u64(d)?;
                self.wr(data + len as u64 * size, &items)?;
                self.wr_u64(d + 8, need as u64)?;
                Ok(Vec::new())
            }
            B::DynResize => {
                let d = Self::ptr(&self.eval(arg(0))?);
                let (size, align) = self.elem_layout(arg(0).ty);
                let n = self.eval(arg(1))?;
                let n = self.int(arg(1).ty, &n) as i64;
                if n < 0 {
                    return Err(self.fail_at(span, format!("cannot resize a dynamic array to {n} elements")));
                }
                let cap = self.rd_u64(d + 16)? as i64;
                if n > cap {
                    self.dyn_reserve(d, n, size, align, span)?;
                }
                let len = self.rd_u64(d + 8)? as i64;
                if n > len {
                    let data = self.rd_u64(d)?;
                    self.mem
                        .fill(data + len as u64 * size, 0, (n - len) as u64 * size)
                        .map_err(|e| self.mem_fail(e, span))?;
                }
                self.wr_u64(d + 8, n as u64)?;
                Ok(Vec::new())
            }
            B::MapPut | B::MapFind | B::MapRemove => {
                let m = Self::ptr(&self.eval(arg(0))?);
                let info = self.map_info(arg(0).ty);
                let key = self.eval(arg(1))?;
                let key_ty = arg(1).ty;
                let key_addr = self.temp(&key, key_ty)?;
                match op {
                    B::MapPut => {
                        let fallback = self.rd(self.ctx, 16)?;
                        let slot = self.map_put(m, info, key_addr, &fallback, span)?;
                        Ok(slot.to_le_bytes().to_vec())
                    }
                    B::MapFind => Ok(self.map_find(m, info, key_addr)?.to_le_bytes().to_vec()),
                    _ => Ok(vec![u8::from(self.map_remove(m, info, key_addr)?)]),
                }
            }
            B::MapNext | B::MapKeyAt | B::MapValueAt => {
                let m = Self::ptr(&self.eval(arg(0))?);
                let info = self.map_info(arg(0).ty);
                let i = self.eval(arg(1))?;
                let i = self.int(arg(1).ty, &i) as i64;
                match op {
                    B::MapNext => {
                        let next = self.map_next(m, info, i)?;
                        Ok(self.encode(ty, Num::I(i128::from(next))))
                    }
                    _ => {
                        let slots = self.rd_u64(m)?;
                        let off = if op == B::MapKeyAt { info.key_offset } else { info.value_offset };
                        Ok((slots + i as u64 * info.slot_size + off).to_le_bytes().to_vec())
                    }
                }
            }
            B::Utf8Decode => {
                let s = self.eval(arg(0))?;
                let bytes = self.string_bytes(&s)?;
                let i = self.eval(arg(1))?;
                let i = self.int(arg(1).ty, &i) as usize;
                let out = Self::ptr(&self.eval(arg(2))?);
                let (rune, width) = decode_utf8(&bytes, i);
                self.wr(out, &rune.to_le_bytes())?;
                Ok(self.encode(ty, Num::I(i128::from(width))))
            }
            B::StringFind => {
                let s = self.eval(arg(0))?;
                let n = self.eval(arg(1))?;
                let (hay, needle) = (self.string_bytes(&s)?, self.string_bytes(&n)?);
                let at = if needle.is_empty() {
                    0
                } else {
                    hay.windows(needle.len()).position(|w| w == needle.as_slice()).map_or(-1, |p| p as i128)
                };
                Ok(self.encode(ty, Num::I(at)))
            }
            B::StringBytes | B::BytesString => self.eval(arg(0)),
            B::SliceData => Ok(slice(&self.eval(arg(0))?, 0, 8)),
            B::CStringString => {
                let p = Self::ptr(&self.eval(arg(0))?);
                if p == 0 {
                    return self.string_value(ty, b"");
                }
                let bytes = self.mem.read_cstr(p).map_err(|e| self.mem_fail(e, span))?;
                Ok(pair(p, bytes.len() as u64))
            }
            B::CallerLocation => self.location(span),
            B::Bounds => {
                let i = self.eval(arg(0))?;
                let i = self.int(arg(0).ty, &i);
                let n = self.eval(arg(1))?;
                let n = self.int(arg(1).ty, &n);
                self.check_index(i, n, span)?;
                Ok(self.encode(ty, Num::I(i)))
            }
            B::StringCmp => {
                let a = self.eval(arg(0))?;
                let b = self.eval(arg(1))?;
                let (a, b) = (self.string_bytes(&a)?, self.string_bytes(&b)?);
                let c = match a.cmp(&b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                Ok(self.encode(ty, Num::I(c)))
            }
            B::TypeName | B::TypeSize | B::TypeAlign | B::TypeFields => {
                let t = self.eval(arg(0))?;
                let t = TyId(u64::from_le_bytes(word(&t)) as u32);
                if t.0 as usize >= self.p.types.len() {
                    return Err(self.fail_at(span, "this `Type` value does not name a type"));
                }
                match op {
                    B::TypeName => {
                        let name = self.p.types.display(t);
                        self.string_value(ty, name.as_bytes())
                    }
                    B::TypeSize => Ok(self.encode(ty, Num::I(i128::from(self.p.types.size_of(t))))),
                    B::TypeAlign => Ok(self.encode(ty, Num::I(i128::from(self.p.types.align_of(t))))),
                    _ => self.type_fields(t, span),
                }
            }
        }
    }

    /// The bytes of a `String` value.
    pub(super) fn string_bytes(&self, v: &[u8]) -> R<Vec<u8>> {
        let len = i64_at(v, 8);
        if len <= 0 {
            return Ok(Vec::new());
        }
        self.rd(Self::ptr(v), len as u64)
    }

    /// `T.fields`: a slice of `FieldInfo` in static memory.
    fn type_fields(&mut self, t: TyId, span: Span) -> R<Vec<u8>> {
        let types = self.p.types;
        let TyKind::Struct(id) = types.kind(types.base(t)) else {
            let shown = types.display(t);
            return Err(self.fail_at(span, format!("`{shown}` is not a struct, so it has no fields")));
        };
        let Some(info_ty) = self.p.field_info else {
            return Err(self.fail_at(span, "`FieldInfo` is missing from the prelude"));
        };
        let TyKind::Struct(info_id) = types.kind(info_ty) else {
            return Err(self.fail_at(span, "`FieldInfo` is not a struct"));
        };
        let layout = types.struct_info(*info_id).fields.clone();
        let fields = types.struct_info(*id).fields.clone();
        let (size, align) = types.layout(info_ty);
        let data = self.alloc_in(Region::Static, size * fields.len() as u64, align)?;
        for (i, f) in fields.iter().enumerate() {
            let base = data + i as u64 * size;
            for slot in &layout {
                let at = base + slot.offset;
                match slot.name.as_str() {
                    "name" => {
                        let name = f.name.as_str().as_bytes().to_vec();
                        let s = self.string_value(slot.ty, &name)?;
                        self.wr(at, &s)?;
                    }
                    "type" => self.wr_u64(at, u64::from(f.ty.0))?,
                    "offset" => self.wr_u64(at, f.offset)?,
                    _ => {}
                }
            }
        }
        Ok(pair(data, fields.len() as u64))
    }

    /// A `Location` for `span`.
    pub(super) fn location(&mut self, span: Span) -> R<Vec<u8>> {
        let (file, line, column) = (self.p.positions)(span);
        let proc_name = self.stack.last().map(|a| a.func.display.clone()).unwrap_or_default();
        let file = self.mem.intern(file.as_bytes()).map_err(|e| self.mem_fail(e, span))?;
        let proc_name = self.mem.intern(proc_name.as_bytes()).map_err(|e| self.mem_fail(e, span))?;
        let mut out = Vec::with_capacity(32);
        out.extend_from_slice(&file.to_le_bytes());
        out.extend_from_slice(&(line as i32).to_le_bytes());
        out.extend_from_slice(&(column as i32).to_le_bytes());
        out.extend_from_slice(&proc_name.to_le_bytes());
        Ok(out)
    }

    fn elem_layout(&self, ptr_ty: TyId) -> (u64, u64) {
        let pointee = match self.p.types.kind(ptr_ty) {
            TyKind::Pointer(t) => *t,
            _ => ptr_ty,
        };
        match self.kind(pointee) {
            TyKind::Dynamic(e) | TyKind::Slice(e) | TyKind::Array(e, _) => self.p.types.layout(*e),
            _ => (1, 1),
        }
    }

    fn free_builtin(&mut self, args: &'c [ir::Expr], span: Span) -> R<Vec<u8>> {
        let v = self.eval(&args[0])?;
        let ty = args[0].ty;
        match self.p.types.kind(ty) {
            TyKind::Pointer(inner) => match self.kind(*inner) {
                TyKind::Dynamic(elem) => {
                    let d = Self::ptr(&v);
                    let size = self.p.types.size_of(*elem);
                    let data = self.rd_u64(d)?;
                    if data != 0 {
                        let cap = self.rd_u64(d + 16)?;
                        let a = self.rd(d + 24, 16)?;
                        self.free_with(&a, data, (cap * size) as i64, span)?;
                    }
                    self.wr(d, &[0u8; 24])?;
                }
                TyKind::Map(..) => {
                    let m = Self::ptr(&v);
                    let info = self.map_info(ty);
                    let slots = self.rd_u64(m)?;
                    if slots != 0 {
                        let cap = self.rd_u64(m + 16)?;
                        let a = self.rd(m + 24, 16)?;
                        self.free_with(&a, slots, (cap * info.slot_size) as i64, span)?;
                    }
                    self.wr(m, &[0u8; 24])?;
                }
                _ => {
                    let size = self.p.types.size_of(*inner);
                    let a = self.eval(&args[1])?;
                    self.free_with(&a, Self::ptr(&v), size as i64, span)?;
                }
            },
            TyKind::Slice(elem) => {
                let size = self.p.types.size_of(*elem);
                let a = self.eval(&args[1])?;
                self.free_with(&a, Self::ptr(&v), i64_at(&v, 8) * size as i64, span)?;
            }
            TyKind::String => {
                let a = self.eval(&args[1])?;
                self.free_with(&a, Self::ptr(&v), i64_at(&v, 8), span)?;
            }
            TyKind::CString => {
                let p = Self::ptr(&v);
                if p != 0 {
                    let len = self.mem.read_cstr(p).map_err(|e| self.mem_fail(e, span))?.len();
                    let a = self.eval(&args[1])?;
                    self.free_with(&a, p, len as i64 + 1, span)?;
                }
            }
            _ => {}
        }
        Ok(Vec::new())
    }

    // ----- allocators ------------------------------------------------------------

    /// Calls an allocator's procedure the way the runtime does.
    #[allow(clippy::too_many_arguments)]
    fn allocator_call(
        &mut self,
        allocator: &[u8],
        mode: u8,
        size: i64,
        align: i64,
        old: Addr,
        old_size: i64,
        span: Span,
    ) -> R<Addr> {
        let mut proc_addr = Self::ptr(allocator);
        if proc_addr == 0 {
            proc_addr = HEAP_PROC;
        }
        let data = slice(allocator, 8, 8);
        let loc = self.location(span)?;
        let args = vec![
            data,
            vec![mode],
            size.to_le_bytes().to_vec(),
            align.to_le_bytes().to_vec(),
            old.to_le_bytes().to_vec(),
            old_size.to_le_bytes().to_vec(),
            loc,
        ];
        let r = self.call_ptr(proc_addr, args, span)?;
        Ok(Self::ptr(&r))
    }

    fn alloc_with(&mut self, allocator: &[u8], size: i64, align: i64, span: Span) -> R<Addr> {
        if size < 0 {
            return Err(self.fail_at(span, format!("cannot allocate a negative size ({size} bytes)")));
        }
        if size == 0 {
            return Ok(0);
        }
        let p = self.allocator_call(allocator, MODE_ALLOC, size, align, 0, 0, span)?;
        if p == 0 {
            return Err(self.fail_at(span, format!("out of memory allocating {size} bytes")));
        }
        Ok(p)
    }

    fn free_with(&mut self, allocator: &[u8], p: Addr, size: i64, span: Span) -> R<()> {
        if p != 0 {
            self.allocator_call(allocator, MODE_FREE, 0, 0, p, size, span)?;
        }
        Ok(())
    }

    fn resize_with(&mut self, allocator: &[u8], p: Addr, old: i64, new: i64, align: i64, span: Span) -> R<Addr> {
        if new == 0 {
            self.free_with(allocator, p, old, span)?;
            return Ok(0);
        }
        let r = self.allocator_call(allocator, MODE_RESIZE, new, align, p, old, span)?;
        if r == 0 {
            return Err(self.fail_at(span, format!("out of memory resizing to {new} bytes")));
        }
        Ok(r)
    }

    /// Runs one of the interpreter's own procedures: the compile-time heap and
    /// temporary allocators, and the logger.
    pub(super) fn native_proc(&mut self, fp: Addr, args: Vec<Vec<u8>>, span: Span) -> R<Vec<u8>> {
        if fp == LOG_PROC {
            if let Some(msg) = args.get(2) {
                let text = self.string_bytes(msg)?;
                self.output.extend_from_slice(&text);
                self.output.push(b'\n');
            }
            return Ok(Vec::new());
        }
        if fp != HEAP_PROC && fp != TEMP_PROC {
            return Err(self.fail_at(span, "called a value that is not a method"));
        }
        let temp = fp == TEMP_PROC;
        let get = |i: usize| args.get(i).map(Vec::as_slice).unwrap_or(&[]);
        let mode = get(1).first().copied().unwrap_or(0);
        let size = i64_at(get(2), 0);
        let align = i64_at(get(3), 0);
        let old = Self::ptr(get(4));
        let old_size = i64_at(get(5), 0);
        let fresh = |it: &mut Self, size: i64, align: i64| -> R<Addr> {
            let p = it.alloc_in(Region::Heap, size.max(0) as u64, align.max(1) as u64)?;
            if temp {
                it.temp_blocks.push(p);
            }
            Ok(p)
        };
        let result = match mode {
            MODE_ALLOC => fresh(self, size, align)?,
            MODE_FREE => {
                if old != 0 {
                    self.mem.free(old).map_err(|e| self.mem_fail(e, span))?;
                    self.temp_blocks.retain(|b| *b != old);
                }
                0
            }
            MODE_FREE_ALL => {
                if temp {
                    for b in std::mem::take(&mut self.temp_blocks) {
                        let _ = self.mem.free(b);
                    }
                }
                0
            }
            _ => {
                let p = fresh(self, size, align)?;
                if old != 0 {
                    let n = old_size.min(size).max(0) as u64;
                    self.mem.copy(p, old, n).map_err(|e| self.mem_fail(e, span))?;
                    self.mem.free(old).map_err(|e| self.mem_fail(e, span))?;
                    self.temp_blocks.retain(|b| *b != old);
                }
                p
            }
        };
        Ok(result.to_le_bytes().to_vec())
    }

    /// Runs a function implemented in C. Pure math and memory functions run
    /// natively; anything else fails, since C code can't run in the compiler.
    pub(super) fn native(
        &mut self,
        symbol: &str,
        func: &'c ir::Function,
        args: Vec<Vec<u8>>,
        site: Span,
    ) -> R<Vec<u8>> {
        let f64_arg = |i: usize| f64::from_le_bytes(word(args.get(i).map(Vec::as_slice).unwrap_or(&[])));
        let f32_arg = |i: usize| f32::from_le_bytes(word(args.get(i).map(Vec::as_slice).unwrap_or(&[])));
        let unary64: Option<fn(f64) -> f64> = match symbol {
            "sqrt" => Some(f64::sqrt),
            "cbrt" => Some(f64::cbrt),
            "sin" => Some(f64::sin),
            "cos" => Some(f64::cos),
            "tan" => Some(f64::tan),
            "asin" => Some(f64::asin),
            "acos" => Some(f64::acos),
            "atan" => Some(f64::atan),
            "sinh" => Some(f64::sinh),
            "cosh" => Some(f64::cosh),
            "tanh" => Some(f64::tanh),
            "exp" => Some(f64::exp),
            "exp2" => Some(f64::exp2),
            "log" => Some(f64::ln),
            "log10" => Some(f64::log10),
            "log2" => Some(f64::log2),
            "floor" => Some(f64::floor),
            "ceil" => Some(f64::ceil),
            "round" => Some(f64::round),
            "trunc" => Some(f64::trunc),
            "fabs" => Some(f64::abs),
            _ => None,
        };
        if let Some(f) = unary64 {
            return Ok(f(f64_arg(0)).to_le_bytes().to_vec());
        }
        let unary32: Option<fn(f32) -> f32> = match symbol {
            "sqrtf" => Some(f32::sqrt),
            "cbrtf" => Some(f32::cbrt),
            "sinf" => Some(f32::sin),
            "cosf" => Some(f32::cos),
            "tanf" => Some(f32::tan),
            "asinf" => Some(f32::asin),
            "acosf" => Some(f32::acos),
            "atanf" => Some(f32::atan),
            "sinhf" => Some(f32::sinh),
            "coshf" => Some(f32::cosh),
            "tanhf" => Some(f32::tanh),
            "expf" => Some(f32::exp),
            "exp2f" => Some(f32::exp2),
            "logf" => Some(f32::ln),
            "log10f" => Some(f32::log10),
            "log2f" => Some(f32::log2),
            "floorf" => Some(f32::floor),
            "ceilf" => Some(f32::ceil),
            "roundf" => Some(f32::round),
            "truncf" => Some(f32::trunc),
            "fabsf" => Some(f32::abs),
            _ => None,
        };
        if let Some(f) = unary32 {
            return Ok(f(f32_arg(0)).to_le_bytes().to_vec());
        }
        let binary = match symbol {
            "pow" => Some(f64_arg(0).powf(f64_arg(1)).to_le_bytes().to_vec()),
            "atan2" => Some(f64_arg(0).atan2(f64_arg(1)).to_le_bytes().to_vec()),
            "hypot" => Some(f64_arg(0).hypot(f64_arg(1)).to_le_bytes().to_vec()),
            "fmod" => Some((f64_arg(0) % f64_arg(1)).to_le_bytes().to_vec()),
            "powf" => Some(f32_arg(0).powf(f32_arg(1)).to_le_bytes().to_vec()),
            "atan2f" => Some(f32_arg(0).atan2(f32_arg(1)).to_le_bytes().to_vec()),
            "hypotf" => Some(f32_arg(0).hypot(f32_arg(1)).to_le_bytes().to_vec()),
            "fmodf" => Some((f32_arg(0) % f32_arg(1)).to_le_bytes().to_vec()),
            _ => None,
        };
        if let Some(v) = binary {
            return Ok(v);
        }
        let get = |i: usize| args.get(i).map(Vec::as_slice).unwrap_or(&[]);
        match symbol {
            "wid_mem_copy" => {
                let n = i64_at(get(2), 0);
                if n > 0 {
                    self.mem
                        .copy(Self::ptr(get(0)), Self::ptr(get(1)), n as u64)
                        .map_err(|e| self.mem_fail(e, site))?;
                }
                Ok(Vec::new())
            }
            "wid_mem_set" => {
                let n = i64_at(get(2), 0);
                if n > 0 {
                    let byte = get(1).first().copied().unwrap_or(0);
                    self.mem.fill(Self::ptr(get(0)), byte, n as u64).map_err(|e| self.mem_fail(e, site))?;
                }
                Ok(Vec::new())
            }
            "wid_mem_compare" => {
                let n = i64_at(get(2), 0).max(0) as u64;
                let a = self.rd(Self::ptr(get(0)), n)?;
                let b = self.rd(Self::ptr(get(1)), n)?;
                let c: i64 = match a.cmp(&b) {
                    std::cmp::Ordering::Less => -1,
                    std::cmp::Ordering::Equal => 0,
                    std::cmp::Ordering::Greater => 1,
                };
                Ok(c.to_le_bytes().to_vec())
            }
            "wid_heap_allocator" => Ok(pair(HEAP_PROC, 0)),
            "wid_temp_allocator" => Ok(pair(TEMP_PROC, 0)),
            "wid_fmt_float" => {
                let v = f64_arg(0);
                let precision = i64_at(get(1), 0);
                let style = get(2).first().copied().unwrap_or(b'f');
                let buf = Self::ptr(get(3));
                let cap = i64_at(get(4), 0).max(0) as usize;
                let text = super::format::fmt_float(v, precision, style);
                let n = text.len().min(cap);
                self.wr(buf, &text.as_bytes()[..n])?;
                Ok((n as i64).to_le_bytes().to_vec())
            }
            "wid_parse_float" => {
                let s = self.string_bytes(get(0))?;
                let out = Self::ptr(get(1));
                match super::format::parse_float(&s) {
                    Some(v) => {
                        self.wr(out, &v.to_le_bytes())?;
                        Ok(vec![1])
                    }
                    None => Ok(vec![0]),
                }
            }
            _ => {
                let mut f = self.fail_at(
                    site,
                    format!(
                        "calls `{}`, which is implemented in C (`{symbol}`) and only runs in the built program",
                        func.display
                    ),
                );
                f.kind = FailKind::Foreign;
                f.help = Some("compute the value at run time instead, or write the computation in Wid".into());
                Err(f)
            }
        }
    }

    // ----- dynamic arrays -----------------------------------------------------------

    fn dyn_reserve(&mut self, d: Addr, cap: i64, size: u64, align: u64, span: Span) -> R<()> {
        let old_cap = self.rd_u64(d + 16)? as i64;
        if cap <= old_cap {
            return Ok(());
        }
        let mut allocator = self.rd(d + 24, 16)?;
        if Self::ptr(&allocator) == 0 {
            allocator = self.rd(self.ctx, 16)?;
            self.wr(d + 24, &allocator)?;
        }
        let (old_bytes, new_bytes) = match (old_cap.checked_mul(size as i64), cap.checked_mul(size as i64)) {
            (Some(o), Some(n)) => (o, n),
            _ => return Err(self.fail_at(span, format!("dynamic array of {cap} elements is too large"))),
        };
        let data = self.rd_u64(d)?;
        let fresh = self.resize_with(&allocator, data, old_bytes, new_bytes, align as i64, span)?;
        self.wr_u64(d, fresh)?;
        self.wr_u64(d + 16, cap as u64)?;
        Ok(())
    }

    /// Appends (or inserts at `index`) one zeroed element; returns its address.
    fn dyn_insert(&mut self, d: Addr, index: Option<i64>, size: u64, align: u64, span: Span) -> R<Addr> {
        let len = self.rd_u64(d + 8)? as i64;
        if let Some(i) = index
            && (i < 0 || i > len)
        {
            return Err(self.fail_check(span, format!("insert position {i} is out of bounds for length {len}")));
        }
        let cap = self.rd_u64(d + 16)? as i64;
        if len == cap {
            self.dyn_reserve(d, if cap > 0 { cap * 2 } else { 8 }, size, align, span)?;
        }
        let data = self.rd_u64(d)?;
        let at = index.unwrap_or(len) as u64;
        if at < len as u64 {
            self.mem
                .copy(data + (at + 1) * size, data + at * size, (len as u64 - at) * size)
                .map_err(|e| self.mem_fail(e, span))?;
        }
        let slot = data + at * size;
        self.mem.fill(slot, 0, size).map_err(|e| self.mem_fail(e, span))?;
        self.wr_u64(d + 8, (len + 1) as u64)?;
        Ok(slot)
    }

    // ----- maps ----------------------------------------------------------------------

    fn map_info(&self, ptr_ty: TyId) -> MapInfo {
        let types = self.p.types;
        let map_ty = match types.kind(ptr_ty) {
            TyKind::Pointer(t) => *t,
            _ => ptr_ty,
        };
        let (k, v) = match self.kind(map_ty) {
            TyKind::Map(k, v) => (*k, *v),
            _ => (map_ty, map_ty),
        };
        let (ks, ka) = types.layout(k);
        let (vs, va) = types.layout(v);
        let align = ka.max(va).max(8);
        let key_offset = 16u64.div_ceil(ka.max(1)) * ka.max(1);
        let value_offset = (key_offset + ks).div_ceil(va.max(1)) * va.max(1);
        let slot_size = (value_offset + vs).div_ceil(align) * align;
        let string_key = matches!(self.kind(k), TyKind::String);
        MapInfo { key_size: ks, slot_size, key_offset, value_offset, align, string_key }
    }

    fn map_key_bytes(&self, info: MapInfo, key: Addr) -> R<Vec<u8>> {
        if info.string_key {
            let s = self.rd(key, 16)?;
            return self.string_bytes(&s);
        }
        self.rd(key, info.key_size)
    }

    fn map_hash(&self, info: MapInfo, key: Addr) -> R<u64> {
        let bytes = self.map_key_bytes(info, key)?;
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for b in bytes {
            h ^= u64::from(b);
            h = h.wrapping_mul(0x0000_0100_0000_01b3);
        }
        Ok(if h == 0 { 1 } else { h })
    }

    fn map_probe(&self, m: Addr, info: MapInfo, key: Addr, hash: u64) -> R<(i64, bool)> {
        let cap = self.rd_u64(m + 16)? as i64;
        let slots = self.rd_u64(m)?;
        if cap == 0 {
            return Ok((-1, false));
        }
        let mask = cap - 1;
        let mut i = (hash & mask as u64) as i64;
        let wanted = self.map_key_bytes(info, key)?;
        for _ in 0..cap {
            let slot = slots + i as u64 * info.slot_size;
            let head = self.rd(slot, 16)?;
            if head[0] == 0 {
                return Ok((i, false));
            }
            if u64::from_le_bytes(word(&head[8..])) == hash
                && self.map_key_bytes(info, slot + info.key_offset)? == wanted
            {
                return Ok((i, true));
            }
            i = (i + 1) & mask;
        }
        Ok((-1, false))
    }

    fn map_find(&self, m: Addr, info: MapInfo, key: Addr) -> R<Addr> {
        if self.rd_u64(m + 8)? == 0 {
            return Ok(0);
        }
        let hash = self.map_hash(info, key)?;
        let (i, found) = self.map_probe(m, info, key, hash)?;
        let slots = self.rd_u64(m)?;
        Ok(if found { slots + i as u64 * info.slot_size + info.value_offset } else { 0 })
    }

    fn map_grow(&mut self, m: Addr, info: MapInfo, span: Span) -> R<()> {
        let old_slots = self.rd_u64(m)?;
        let old_cap = self.rd_u64(m + 16)? as i64;
        let cap = if old_cap > 0 { old_cap * 2 } else { 8 };
        let allocator = self.rd(m + 24, 16)?;
        let bytes = cap as u64 * info.slot_size;
        let fresh = self.alloc_with(&allocator, bytes as i64, info.align as i64, span)?;
        self.wr_u64(m, fresh)?;
        self.wr_u64(m + 16, cap as u64)?;
        self.wr_u64(m + 8, 0)?;
        for i in 0..old_cap.max(0) as u64 {
            let slot = old_slots + i * info.slot_size;
            let content = self.rd(slot, info.slot_size)?;
            if content[0] != 1 {
                continue;
            }
            let hash = u64::from_le_bytes(word(&content[8..]));
            let (j, _) = self.map_probe(m, info, slot + info.key_offset, hash)?;
            self.wr(fresh + j as u64 * info.slot_size, &content)?;
            let len = self.rd_u64(m + 8)?;
            self.wr_u64(m + 8, len + 1)?;
        }
        if old_slots != 0 {
            self.free_with(&allocator, old_slots, old_cap * info.slot_size as i64, span)?;
        }
        Ok(())
    }

    fn map_put(&mut self, m: Addr, info: MapInfo, key: Addr, fallback: &[u8], span: Span) -> R<Addr> {
        if Self::ptr(&self.rd(m + 24, 16)?) == 0 {
            self.wr(m + 24, fallback)?;
        }
        let len = self.rd_u64(m + 8)? as i64;
        let cap = self.rd_u64(m + 16)? as i64;
        if (len + 1) * 4 > cap * 3 {
            self.map_grow(m, info, span)?;
        }
        let hash = self.map_hash(info, key)?;
        let (i, found) = self.map_probe(m, info, key, hash)?;
        let slots = self.rd_u64(m)?;
        let slot = slots + i as u64 * info.slot_size;
        if !found {
            let key_bytes = self.rd(key, info.key_size)?;
            self.mem.fill(slot, 0, info.slot_size).map_err(|e| self.mem_fail(e, span))?;
            self.wr(slot, &[1])?;
            self.wr_u64(slot + 8, hash)?;
            self.wr(slot + info.key_offset, &key_bytes)?;
            let len = self.rd_u64(m + 8)?;
            self.wr_u64(m + 8, len + 1)?;
        }
        Ok(slot + info.value_offset)
    }

    fn map_remove(&mut self, m: Addr, info: MapInfo, key: Addr) -> R<bool> {
        if self.rd_u64(m + 8)? == 0 {
            return Ok(false);
        }
        let hash = self.map_hash(info, key)?;
        let (found_at, found) = self.map_probe(m, info, key, hash)?;
        if !found {
            return Ok(false);
        }
        let slots = self.rd_u64(m)?;
        let mask = self.rd_u64(m + 16)? as i64 - 1;
        let at = |i: i64| slots + i as u64 * info.slot_size;
        let (mut i, mut j) = (found_at, found_at);
        loop {
            self.wr(at(i), &[0])?;
            loop {
                j = (j + 1) & mask;
                let head = self.rd(at(j), 16)?;
                if head[0] == 0 {
                    let len = self.rd_u64(m + 8)?;
                    self.wr_u64(m + 8, len - 1)?;
                    return Ok(true);
                }
                let ideal = (u64::from_le_bytes(word(&head[8..])) & mask as u64) as i64;
                let between = if i <= j { i < ideal && ideal <= j } else { i < ideal || ideal <= j };
                if !between {
                    break;
                }
            }
            let content = self.rd(at(j), info.slot_size)?;
            self.wr(at(i), &content)?;
            i = j;
        }
    }

    fn map_next(&self, m: Addr, info: MapInfo, mut i: i64) -> R<i64> {
        let cap = self.rd_u64(m + 16)? as i64;
        let slots = self.rd_u64(m)?;
        while i < cap {
            if self.rd(slots + i as u64 * info.slot_size, 1)?[0] == 1 {
                return Ok(i);
            }
            i += 1;
        }
        Ok(-1)
    }

    // ----- writers ---------------------------------------------------------------------

    /// Appends bytes to a builder writer, growing its buffer through its
    /// allocator like `wid_w_bytes`.
    fn writer_write(&mut self, w: Addr, bytes: &[u8], span: Span) -> R<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        if self.rd_u64(w)? != 0 {
            self.output.extend_from_slice(bytes);
            return Ok(());
        }
        let len = self.rd_u64(w + 16)? as i64;
        let cap = self.rd_u64(w + 24)? as i64;
        let need = len + bytes.len() as i64;
        if need > cap {
            let mut new_cap = if cap > 0 { cap } else { 64 };
            while new_cap < need {
                new_cap *= 2;
            }
            let allocator = self.rd(w + 32, 16)?;
            let buf = self.rd_u64(w + 8)?;
            let fresh = self.resize_with(&allocator, buf, cap, new_cap, 1, span)?;
            self.wr_u64(w + 8, fresh)?;
            self.wr_u64(w + 24, new_cap as u64)?;
        }
        let buf = self.rd_u64(w + 8)?;
        self.wr(buf + len as u64, bytes)?;
        self.wr_u64(w + 16, need as u64)?;
        Ok(())
    }
}

/// Two words: a pointer and a length (a string or slice value).
fn pair(a: u64, b: u64) -> Vec<u8> {
    let mut out = a.to_le_bytes().to_vec();
    out.extend_from_slice(&b.to_le_bytes());
    out
}

/// Decodes the UTF-8 rune at byte `i`, like `wid_utf8_decode`.
fn decode_utf8(s: &[u8], i: usize) -> (i32, i64) {
    let Some(&c) = s.get(i) else { return (0xFFFD, 1) };
    if c < 0x80 {
        return (i32::from(c), 1);
    }
    let width = if c & 0xE0 == 0xC0 {
        2
    } else if c & 0xF0 == 0xE0 {
        3
    } else if c & 0xF8 == 0xF0 {
        4
    } else {
        0
    };
    if width == 0 || i + width > s.len() {
        return (0xFFFD, 1);
    }
    let mut value = u32::from(c) & (0x7F >> width);
    for k in 1..width {
        let b = s[i + k];
        if b & 0xC0 != 0x80 {
            return (0xFFFD, 1);
        }
        value = (value << 6) | u32::from(b & 0x3F);
    }
    (value as i32, width as i64)
}

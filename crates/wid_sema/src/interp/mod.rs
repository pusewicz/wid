//! The compile-time interpreter. It runs lowered IR inside the compiler for
//! `comptime`, constant initializers and `comptime if` conditions.
//!
//! Values are bytes in their C layout ([`memory`]), so every construct means
//! exactly what it means in the generated program. Compile-time code always
//! runs with the checks of a `-debug` build: bounds, nil and integer overflow.

mod builtins;
mod convert;
mod format;
mod memory;
mod type_info;

use std::collections::HashMap;

use wid_diagnostics::Span;
use wid_syntax::Name;

use crate::ir::{self, BinaryOp, ExprKind, FnId, GlobalId, LabelId, LocalId, Stmt, UnaryOp};
use crate::types::{Abi, FloatTy, IntTy, TyId, TyKind, TypeTable};

pub(crate) use convert::to_ir;
pub(crate) use memory::Memory;
use memory::{Addr, FN_TAG, MemError, NATIVE_TAG, Region, SHIFT};

/// Limits that keep compile-time code from hanging or exhausting the compiler.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Limits {
    /// Statements and calls one evaluation may run.
    pub steps: u64,
    /// How deep calls may nest.
    pub depth: usize,
    /// Bytes of memory one evaluation may use.
    pub memory: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Limits { steps: 20_000_000, depth: 256, memory: 256 << 20 }
    }
}

/// Why compile-time code stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum FailKind {
    /// The code failed: a panic, a failed assertion, a bad index.
    Error,
    /// The code exceeded a [`Limits`] budget.
    Limit,
    /// The code called a C function.
    Foreign,
}

/// A failure of compile-time code, with the calls that led to it.
#[derive(Clone, Debug)]
pub(crate) struct Failure {
    /// What went wrong.
    pub kind: FailKind,
    /// The explanation.
    pub message: String,
    /// Where it went wrong.
    pub span: Span,
    /// The methods running at the time, outermost first, each with the call
    /// that entered it.
    pub chain: Vec<(String, Span)>,
    /// A suggestion.
    pub help: Option<String>,
    /// Whether a run-time check (bounds, nil, overflow) failed.
    pub checked: bool,
}

/// Resolves a span to a file name, line and column for `caller_location`.
pub(crate) type Positions<'c> = dyn Fn(Span) -> (String, u32, u32) + 'c;

/// The parts of the checked program the interpreter reads.
pub(crate) struct Program<'c> {
    /// Every type.
    pub types: &'c TypeTable,
    /// Lowered functions; `None` while a function is still being lowered.
    pub functions: &'c [Option<ir::Function>],
    /// Package-level values.
    pub globals: &'c [ir::Global],
    /// Members of the builtin `Error` set.
    pub errors: &'c [Name],
    /// Source positions.
    pub positions: &'c Positions<'c>,
    /// The `FieldInfo` struct of the prelude, for `T.fields`.
    pub field_info: Option<TyId>,
}

/// A running call.
struct Activation<'c> {
    func: &'c ir::Function,
    locals: Vec<Addr>,
    /// The statement being run.
    span: Span,
    /// The call that entered the function.
    site: Span,
}

/// How a statement finished.
enum Flow {
    Normal,
    Goto(LabelId),
    Return(Option<Vec<u8>>),
}

/// A value read as a number.
#[derive(Clone, Copy, Debug)]
enum Num {
    I(i128),
    F(f64),
}

/// How a type is stored when it is a single scalar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scalar {
    Int(IntTy),
    Float(FloatTy),
    Bool,
    /// A pointer, function pointer or pointer-like optional.
    Ptr,
    None,
}

type R<T> = Result<T, Failure>;

/// The native procedure behind the compile-time heap allocator.
const HEAP_PROC: u64 = (NATIVE_TAG << SHIFT) | 1;
/// The native procedure behind the compile-time temporary allocator.
const TEMP_PROC: u64 = (NATIVE_TAG << SHIFT) | 2;
/// The native procedure behind the compile-time logger.
const LOG_PROC: u64 = (NATIVE_TAG << SHIFT) | 3;

/// The compile-time interpreter.
pub(crate) struct Interp<'c> {
    p: Program<'c>,
    mem: Memory,
    limits: Limits,
    steps: u64,
    stack: Vec<Activation<'c>>,
    ctx: Addr,
    default_ctx: Addr,
    /// What `puts`, `print`, `p` and the logger wrote.
    pub output: Vec<u8>,
    global_addrs: HashMap<GlobalId, Addr>,
    temp_blocks: Vec<Addr>,
    /// The `type_info` table of each type described so far.
    type_infos: HashMap<TyId, Addr>,
}

impl<'c> Interp<'c> {
    /// Creates an interpreter over a checked program.
    pub fn new(p: Program<'c>, limits: Limits) -> Self {
        let mut it = Interp {
            p,
            mem: Memory::new(limits.memory),
            limits,
            steps: 0,
            stack: Vec::new(),
            ctx: 0,
            default_ctx: 0,
            output: Vec::new(),
            global_addrs: HashMap::new(),
            temp_blocks: Vec::new(),
            type_infos: HashMap::new(),
        };
        if let Ok(ctx) = it.mem.alloc(Region::Static, 64, 8) {
            let mut bytes = vec![0u8; 64];
            bytes[0..8].copy_from_slice(&HEAP_PROC.to_le_bytes());
            bytes[16..24].copy_from_slice(&TEMP_PROC.to_le_bytes());
            bytes[32..40].copy_from_slice(&LOG_PROC.to_le_bytes());
            let _ = it.mem.write(ctx, &bytes);
            it.ctx = ctx;
            it.default_ctx = ctx;
        }
        it
    }

    /// Ends the run, keeping the memory that a result's pointers refer to.
    pub fn into_memory(self) -> Memory {
        self.mem
    }

    /// Runs a parameterless function and returns its result's bytes.
    pub fn run(&mut self, func: &'c ir::Function, site: Span) -> R<Vec<u8>> {
        self.invoke(func, Vec::new(), site)
    }

    // ----- failures ------------------------------------------------------------

    fn here(&self) -> Span {
        self.stack.last().map_or(Span::default(), |a| a.span)
    }

    fn chain(&self) -> Vec<(String, Span)> {
        self.stack.iter().map(|a| (a.func.display.clone(), a.site)).collect()
    }

    /// A failure at the statement being run.
    fn fail(&self, message: impl Into<String>) -> Failure {
        self.fail_at(self.here(), message)
    }

    /// A failure at `span`, or at the current statement when `span` is unset.
    fn fail_at(&self, span: Span, message: impl Into<String>) -> Failure {
        let span = if span == Span::default() { self.here() } else { span };
        Failure {
            kind: FailKind::Error,
            message: message.into(),
            span,
            chain: self.chain(),
            help: None,
            checked: false,
        }
    }

    /// A failed run-time check (bounds, nil, overflow) at `span`.
    fn fail_check(&self, span: Span, message: impl Into<String>) -> Failure {
        Failure { checked: true, ..self.fail_at(span, message) }
    }

    fn mem_fail(&self, e: MemError, span: Span) -> Failure {
        match e {
            MemError::Nil => self.fail_check(span, "dereferenced a nil pointer"),
            MemError::Invalid(a) => {
                self.fail_at(span, format!("read or wrote memory that is not part of a live value (address {a:#x})"))
            }
            MemError::BadFree(a) => {
                self.fail_at(span, format!("freed memory that was not allocated or was already freed (address {a:#x})"))
            }
            MemError::Limit(n) => {
                let mut f = self.fail_at(span, format!("compile-time code used more than {} MiB of memory", n >> 20));
                f.kind = FailKind::Limit;
                f
            }
        }
    }

    fn tick(&mut self) -> R<()> {
        self.steps += 1;
        if self.steps > self.limits.steps {
            let mut f =
                self.fail(format!("compile-time code ran for more than {} steps", group_digits(self.limits.steps)));
            f.kind = FailKind::Limit;
            f.help = Some("look for a loop that never ends, or move the work to run time".into());
            return Err(f);
        }
        Ok(())
    }

    // ----- memory helpers --------------------------------------------------------

    fn rd(&self, addr: Addr, len: u64) -> R<Vec<u8>> {
        self.mem.read(addr, len).map(<[u8]>::to_vec).map_err(|e| self.mem_fail(e, Span::default()))
    }

    fn wr(&mut self, addr: Addr, bytes: &[u8]) -> R<()> {
        self.mem.write(addr, bytes).map_err(|e| self.mem_fail(e, Span::default()))
    }

    fn rd_u64(&self, addr: Addr) -> R<u64> {
        self.mem.read_u64(addr).map_err(|e| self.mem_fail(e, Span::default()))
    }

    fn wr_u64(&mut self, addr: Addr, v: u64) -> R<()> {
        self.mem.write_u64(addr, v).map_err(|e| self.mem_fail(e, Span::default()))
    }

    fn alloc_in(&mut self, region: Region, size: u64, align: u64) -> R<Addr> {
        self.mem.alloc(region, size, align).map_err(|e| self.mem_fail(e, Span::default()))
    }

    /// Copies a value into a fresh stack slot and returns its address.
    fn temp(&mut self, bytes: &[u8], ty: TyId) -> R<Addr> {
        let (size, align) = self.p.types.layout(ty);
        let addr = self.alloc_in(Region::Stack, size.max(bytes.len() as u64), align)?;
        self.wr(addr, bytes)?;
        Ok(addr)
    }

    // ----- types -------------------------------------------------------------------

    fn size(&self, ty: TyId) -> u64 {
        self.p.types.size_of(ty)
    }

    fn kind(&self, ty: TyId) -> &'c TyKind {
        self.p.types.kind(self.p.types.base(ty))
    }

    fn scalar(&self, ty: TyId) -> Scalar {
        let types = self.p.types;
        match self.kind(ty) {
            TyKind::Int(i) => Scalar::Int(*i),
            TyKind::Enum(id) => Scalar::Int(types.enum_info(*id).backing),
            TyKind::Rune => Scalar::Int(IntTy::I32),
            TyKind::Error => Scalar::Int(IntTy::U32),
            TyKind::TypeId | TyKind::Type => Scalar::Int(IntTy::U64),
            TyKind::Float(f) => Scalar::Float(*f),
            TyKind::Bool => Scalar::Bool,
            TyKind::Pointer(_) | TyKind::MultiPointer(_) | TyKind::RawPtr | TyKind::CString | TyKind::Proc(_) => {
                Scalar::Ptr
            }
            TyKind::Optional(_) if types.optional_is_pointer(types.base(ty)) => Scalar::Ptr,
            _ => Scalar::None,
        }
    }

    /// The offset and size of field `index` of a struct, tuple or container.
    fn field(&self, ty: TyId, index: u32) -> (u64, u64) {
        let types = self.p.types;
        let i = index as usize;
        match self.kind(ty) {
            TyKind::Struct(id) => {
                let f = &types.struct_info(*id).fields[i];
                (f.offset, types.size_of(f.ty))
            }
            TyKind::Tuple(elems) => {
                let parts: Vec<(u64, u64)> = elems.iter().map(|e| types.layout(*e)).collect();
                (crate::types::offsets(&parts)[i], types.size_of(elems[i]))
            }
            TyKind::Dynamic(_) | TyKind::Map(..) => [(0, 8), (8, 8), (16, 8), (24, 16)][i.min(3)],
            TyKind::Slice(_) | TyKind::String => [(0, 8), (8, 8)][i.min(1)],
            _ => (0, types.size_of(ty)),
        }
    }

    /// The offset of a union's payload, after its 4-byte tag.
    fn union_payload(&self, ty: TyId) -> u64 {
        convert::union_payload(self.p.types, ty)
    }

    fn optional_is_pointer(&self, ty: TyId) -> bool {
        self.p.types.optional_is_pointer(self.p.types.base(ty))
    }

    // ----- numbers -------------------------------------------------------------------

    fn decode(&self, ty: TyId, bytes: &[u8]) -> Num {
        match self.scalar(ty) {
            Scalar::Int(it) => Num::I(int_from(bytes, it.size(), it.signed())),
            Scalar::Float(FloatTy::F32) => Num::F(f64::from(f32::from_le_bytes(word(bytes)))),
            Scalar::Float(FloatTy::F64) => Num::F(f64::from_le_bytes(word(bytes))),
            Scalar::Bool => Num::I(i128::from(bytes.first().copied().unwrap_or(0) != 0)),
            Scalar::Ptr | Scalar::None => Num::I(int_from(bytes, 8, false)),
        }
    }

    fn encode(&self, ty: TyId, n: Num) -> Vec<u8> {
        match self.scalar(ty) {
            Scalar::Int(it) => {
                let v = match n {
                    Num::I(v) => v,
                    Num::F(f) => f as i128,
                };
                int_bytes(v, it.size())
            }
            Scalar::Float(FloatTy::F32) => (as_f64(n) as f32).to_le_bytes().to_vec(),
            Scalar::Float(FloatTy::F64) => as_f64(n).to_le_bytes().to_vec(),
            Scalar::Bool => vec![u8::from(match n {
                Num::I(v) => v != 0,
                Num::F(f) => f != 0.0,
            })],
            Scalar::Ptr => int_bytes(as_int(n), 8),
            Scalar::None => {
                let mut v = int_bytes(as_int(n), 8);
                v.resize(self.size(ty) as usize, 0);
                v
            }
        }
    }

    fn int(&self, ty: TyId, bytes: &[u8]) -> i128 {
        as_int(self.decode(ty, bytes))
    }

    fn truthy(bytes: &[u8]) -> bool {
        bytes.first().copied().unwrap_or(0) != 0
    }

    fn ptr(bytes: &[u8]) -> Addr {
        u64::from_le_bytes(word(bytes))
    }

    // ----- statements --------------------------------------------------------------

    fn local(&self, l: LocalId) -> Addr {
        self.stack.last().map_or(0, |a| a.locals.get(l.0 as usize).copied().unwrap_or(0))
    }

    fn exec_block(&mut self, block: &'c ir::Block) -> R<Flow> {
        for stmt in &block.stmts {
            match self.exec(stmt)? {
                Flow::Normal => {}
                other => return Ok(other),
            }
        }
        Ok(Flow::Normal)
    }

    fn exec(&mut self, stmt: &'c Stmt) -> R<Flow> {
        self.tick()?;
        match stmt {
            Stmt::Let { local, init } => {
                let addr = self.local(*local);
                let ty = self.local_ty(*local);
                let bytes = match init {
                    Some(e) => self.eval(e)?,
                    None => vec![0; self.size(ty) as usize],
                };
                self.wr(addr, &bytes)?;
            }
            Stmt::LetUninit(local) => {
                let addr = self.local(*local);
                let size = self.size(self.local_ty(*local));
                self.mem.fill(addr, 0, size).map_err(|e| self.mem_fail(e, Span::default()))?;
            }
            Stmt::Assign { target, value } => {
                let v = self.eval(value)?;
                let addr = self.place(target)?;
                self.wr(addr, &v)?;
            }
            Stmt::Expr(e) => {
                self.eval(e)?;
            }
            Stmt::If { cond, then, else_ } => {
                let c = self.eval(cond)?;
                return self.exec_block(if Self::truthy(&c) { then } else { else_ });
            }
            Stmt::Loop { body, continue_label, break_label } => loop {
                self.tick()?;
                match self.exec_block(body)? {
                    Flow::Normal => {}
                    Flow::Goto(l) if l == *continue_label => {}
                    Flow::Goto(l) if l == *break_label => break,
                    other => return Ok(other),
                }
            },
            Stmt::Labeled { body, end_label } => {
                return match self.exec_block(body)? {
                    Flow::Goto(l) if l == *end_label => Ok(Flow::Normal),
                    other => Ok(other),
                };
            }
            Stmt::Scope(b) => return self.exec_block(b),
            Stmt::Goto(l) => return Ok(Flow::Goto(*l)),
            Stmt::Return(value) => {
                let v = match value {
                    Some(e) => Some(self.eval(e)?),
                    None => None,
                };
                return Ok(Flow::Return(v));
            }
            Stmt::Unreachable => return Err(self.fail("reached code that should be unreachable")),
            Stmt::Line(span) => {
                if let Some(a) = self.stack.last_mut() {
                    a.span = *span;
                }
            }
            Stmt::WithContext(b) => {
                let bytes = self.rd(self.ctx, 64)?;
                let ctx_ty = self.p.types.context_ty;
                let copy = self.temp(&bytes, ctx_ty)?;
                let saved = std::mem::replace(&mut self.ctx, copy);
                let flow = self.exec_block(b);
                self.ctx = saved;
                return flow;
            }
        }
        Ok(Flow::Normal)
    }

    fn local_ty(&self, l: LocalId) -> TyId {
        self.stack.last().map_or(TyId(0), |a| a.func.locals[l.0 as usize].ty)
    }

    // ----- calls -----------------------------------------------------------------------

    fn call(&mut self, id: FnId, args: Vec<Vec<u8>>, site: Span) -> R<Vec<u8>> {
        let Some(Some(func)) = self.p.functions.get(id.0 as usize) else {
            let mut f = self.fail_at(site, "this calls a method that is still being compiled");
            f.help = Some(
                "a method cannot use the result of `comptime` code that calls the method itself; move the compile-time work into its own method"
                    .into(),
            );
            return Err(f);
        };
        if let Some(symbol) = &func.foreign {
            return self.native(symbol, func, args, site);
        }
        self.invoke(func, args, site)
    }

    fn invoke(&mut self, func: &'c ir::Function, args: Vec<Vec<u8>>, site: Span) -> R<Vec<u8>> {
        self.tick()?;
        if self.stack.len() >= self.limits.depth {
            let mut f = self.fail_at(site, format!("calls nest more than {} deep", self.limits.depth));
            f.kind = FailKind::Limit;
            f.help = Some("look for recursion that never stops".into());
            return Err(f);
        }
        let Some(body) = &func.body else {
            return Err(self.fail_at(site, format!("`{}` has no body to run", func.display)));
        };
        let mark = self.mem.stack_mark();
        let mut locals = Vec::with_capacity(func.locals.len());
        for l in &func.locals {
            let (size, align) = self.p.types.layout(l.ty);
            locals.push(self.alloc_in(Region::Stack, size, align)?);
        }
        for (p, v) in func.params.iter().zip(args) {
            let addr = locals[p.0 as usize];
            self.wr(addr, &v)?;
        }
        let saved_ctx = self.ctx;
        if func.abi == Abi::C {
            self.ctx = self.default_ctx;
        }
        self.stack.push(Activation { func, locals, span: func.span, site });
        let flow = self.exec_block(body);
        self.stack.pop();
        self.ctx = saved_ctx;
        let flow = flow?;
        self.mem.stack_reset(mark);
        Ok(match flow {
            Flow::Return(Some(v)) => v,
            _ => vec![0; self.size(func.ret) as usize],
        })
    }

    /// Calls through a proc value.
    fn call_ptr(&mut self, fp: Addr, args: Vec<Vec<u8>>, span: Span) -> R<Vec<u8>> {
        match fp >> SHIFT {
            _ if fp == 0 => Err(self.fail_check(span, "called a nil proc")),
            FN_TAG => self.call(FnId((fp & ((1 << SHIFT) - 1)) as u32), args, span),
            NATIVE_TAG => self.native_proc(fp, args, span),
            _ => Err(self.fail_at(span, "called a value that is not a method")),
        }
    }

    // ----- expressions -------------------------------------------------------------------

    fn eval(&mut self, e: &'c ir::Expr) -> R<Vec<u8>> {
        let size = self.size(e.ty);
        match &e.kind {
            ExprKind::Int(v) => Ok(self.encode(e.ty, Num::I(*v))),
            ExprKind::Float(v) => Ok(self.encode(e.ty, Num::F(*v))),
            ExprKind::Bool(b) => Ok(vec![u8::from(*b)]),
            ExprKind::Str(s) => self.string_value(e.ty, s.as_bytes()),
            ExprKind::Nil | ExprKind::Zero => Ok(vec![0; size as usize]),
            ExprKind::Local(l) => self.rd(self.local(*l), size),
            ExprKind::Global(g) | ExprKind::ConstGlobal(g) => {
                let addr = self.global_addr(*g)?;
                self.rd(addr, size)
            }
            ExprKind::FnRef(f) => Ok(((FN_TAG << SHIFT) | u64::from(f.0)).to_le_bytes().to_vec()),
            ExprKind::Field { base, index } => {
                let (offset, fsize) = self.field(base.ty, *index);
                if is_place(base) {
                    let addr = self.place(base)?;
                    self.rd(addr + offset, fsize)
                } else {
                    let v = self.eval(base)?;
                    Ok(slice(&v, offset, fsize))
                }
            }
            ExprKind::Deref(p) => {
                let v = self.eval(p)?;
                self.rd(Self::ptr(&v), size)
            }
            ExprKind::AddrOf(inner) => Ok(self.place_or_temp(inner)?.to_le_bytes().to_vec()),
            ExprKind::Call { func, args } => {
                let mut values = Vec::with_capacity(args.len());
                for a in args {
                    values.push(self.eval(a)?);
                }
                let site = self.here();
                self.call(*func, values, site)
            }
            ExprKind::CallIndirect { callee, args, span } => {
                let fp = Self::ptr(&self.eval(callee)?);
                let mut values = Vec::with_capacity(args.len());
                for a in args {
                    values.push(self.eval(a)?);
                }
                self.call_ptr(fp, values, *span)
            }
            ExprKind::Builtin { op, args, span } => self.builtin(*op, args, *span, e.ty),
            ExprKind::Unary { op, expr } => {
                let v = self.eval(expr)?;
                Ok(match (op, self.decode(expr.ty, &v)) {
                    (UnaryOp::Not, _) => vec![u8::from(!Self::truthy(&v))],
                    (UnaryOp::Neg, Num::F(f)) => self.encode(e.ty, Num::F(-f)),
                    (UnaryOp::Neg, Num::I(i)) => self.encode(e.ty, Num::I(i.wrapping_neg())),
                    (UnaryOp::BitNot, n) => self.encode(e.ty, Num::I(!as_int(n))),
                })
            }
            ExprKind::Binary { op, lhs, rhs, span } => self.binary(*op, lhs, rhs, *span, e.ty),
            ExprKind::Cast { expr, .. } => {
                let v = self.eval(expr)?;
                self.cast(expr.ty, &v, e.ty)
            }
            ExprKind::Select { cond, then, else_ } => {
                let c = self.eval(cond)?;
                if Self::truthy(&c) { self.eval(then) } else { self.eval(else_) }
            }
            ExprKind::Context => Ok(self.ctx.to_le_bytes().to_vec()),
            ExprKind::Aggregate(elems) => self.aggregate(e.ty, elems),
            ExprKind::ErrorTag(name) => {
                let index = self.p.errors.iter().position(|n| n == name).map_or(0, |i| i + 1);
                Ok((index as u32).to_le_bytes().to_vec())
            }
            ExprKind::OptSome(inner) => {
                let v = self.eval(inner)?;
                if self.optional_is_pointer(e.ty) {
                    return Ok(v);
                }
                let mut out = vec![0; size as usize];
                let n = v.len().min(out.len());
                out[..n].copy_from_slice(&v[..n]);
                if let Some(flag) = out.get_mut(v.len()) {
                    *flag = 1;
                }
                Ok(out)
            }
            ExprKind::OptIsSome(inner) => {
                let v = self.eval(inner)?;
                if self.optional_is_pointer(inner.ty) {
                    return Ok(vec![u8::from(Self::ptr(&v) != 0)]);
                }
                let flag = match self.kind(inner.ty) {
                    TyKind::Optional(t) => self.size(*t) as usize,
                    _ => 0,
                };
                Ok(vec![u8::from(v.get(flag).copied().unwrap_or(0) != 0)])
            }
            ExprKind::OptGet(inner) => {
                let v = self.eval(inner)?;
                Ok(slice(&v, 0, size))
            }
            ExprKind::UnionWrap { variant, value } => {
                let v = self.eval(value)?;
                let mut out = vec![0; size as usize];
                out[..4].copy_from_slice(&(variant + 1).to_le_bytes());
                let at = self.union_payload(e.ty) as usize;
                let end = (at + v.len()).min(out.len());
                out[at..end].copy_from_slice(&v[..end - at]);
                Ok(out)
            }
            ExprKind::UnionTag(inner) => {
                let v = self.eval(inner)?;
                let tag = u32::from_le_bytes(word(&v));
                Ok(self.encode(e.ty, Num::I(i128::from(tag))))
            }
            ExprKind::UnionGet { value, .. } => {
                let v = self.eval(value)?;
                Ok(slice(&v, self.union_payload(value.ty), size))
            }
            ExprKind::Index { base, index, span, .. } => {
                if is_place(base) || !matches!(self.kind(base.ty), TyKind::Array(..) | TyKind::Matrix(..)) {
                    let addr = self.index_addr(base, index, *span)?;
                    return self.rd(addr, size);
                }
                let v = self.eval(base)?;
                let i = self.eval(index)?;
                let i = self.int(index.ty, &i);
                let n = self.fixed_len(base.ty);
                self.check_index(i, n, *span)?;
                Ok(slice(&v, i as u64 * size, size))
            }
            ExprKind::SliceOf { base, lo, hi, span, .. } => {
                let lo_v = self.eval(lo)?;
                let lo = self.int(lo.ty, &lo_v);
                let hi_v = self.eval(hi)?;
                let hi = self.int(hi.ty, &hi_v);
                let (data, len, stride) = self.view(base)?;
                if let Some(len) = len
                    && (lo < 0 || hi < lo || hi > len)
                {
                    return Err(self.fail_check(*span, format!("slice {lo}...{hi} is out of bounds for length {len}")));
                }
                let mut out = Vec::with_capacity(16);
                out.extend_from_slice(&(data.wrapping_add(lo as u64 * stride)).to_le_bytes());
                out.extend_from_slice(&((hi - lo) as i64).to_le_bytes());
                Ok(out)
            }
        }
    }

    /// The data pointer, length (when known) and element size of something
    /// that can be indexed or sliced.
    fn view(&mut self, base: &'c ir::Expr) -> R<(Addr, Option<i128>, u64)> {
        match self.kind(base.ty) {
            TyKind::Array(elem, n) => {
                let stride = self.size(*elem);
                let addr = self.place_or_temp(base)?;
                Ok((addr, Some(i128::from(*n)), stride))
            }
            TyKind::Matrix(elem, r, c) => {
                let stride = self.size(*elem);
                let addr = self.place_or_temp(base)?;
                Ok((addr, Some(i128::from(*r) * i128::from(*c)), stride))
            }
            TyKind::MultiPointer(elem) => {
                let stride = self.size(*elem);
                let v = self.eval(base)?;
                Ok((Self::ptr(&v), None, stride))
            }
            TyKind::String => {
                let v = self.eval(base)?;
                Ok((Self::ptr(&v), Some(i128::from(i64_at(&v, 8))), 1))
            }
            TyKind::Slice(elem) | TyKind::Dynamic(elem) => {
                let stride = self.size(*elem);
                let v = self.eval(base)?;
                Ok((Self::ptr(&v), Some(i128::from(i64_at(&v, 8))), stride))
            }
            _ => Err(self.fail("indexed a value that cannot be indexed")),
        }
    }

    fn fixed_len(&self, ty: TyId) -> i128 {
        match self.kind(ty) {
            TyKind::Array(_, n) => i128::from(*n),
            TyKind::Matrix(_, r, c) => i128::from(*r) * i128::from(*c),
            _ => 0,
        }
    }

    fn check_index(&self, i: i128, len: i128, span: Span) -> R<()> {
        if i < 0 || i >= len {
            return Err(self.fail_check(span, format!("index {i} is out of bounds for length {len}")));
        }
        Ok(())
    }

    fn index_addr(&mut self, base: &'c ir::Expr, index: &'c ir::Expr, span: Span) -> R<Addr> {
        let (data, len, stride) = self.view(base)?;
        let i = self.eval(index)?;
        let i = self.int(index.ty, &i);
        if let Some(len) = len {
            self.check_index(i, len, span)?;
        }
        Ok(data.wrapping_add((i as u64).wrapping_mul(stride)))
    }

    /// The address of a place expression.
    fn place(&mut self, e: &'c ir::Expr) -> R<Addr> {
        match &e.kind {
            ExprKind::Local(l) => Ok(self.local(*l)),
            ExprKind::Global(g) | ExprKind::ConstGlobal(g) => self.global_addr(*g),
            ExprKind::Deref(p) => {
                let v = self.eval(p)?;
                let addr = Self::ptr(&v);
                if addr == 0 {
                    return Err(self.fail_check(Span::default(), "dereferenced a nil pointer"));
                }
                Ok(addr)
            }
            ExprKind::Field { base, index } => {
                let (offset, _) = self.field(base.ty, *index);
                Ok(self.place_or_temp(base)? + offset)
            }
            ExprKind::Index { base, index, span, .. } => self.index_addr(base, index, *span),
            ExprKind::OptGet(inner) => self.place_or_temp(inner),
            ExprKind::UnionGet { value, .. } => {
                let at = self.union_payload(value.ty);
                Ok(self.place_or_temp(value)? + at)
            }
            _ => {
                let v = self.eval(e)?;
                self.temp(&v, e.ty)
            }
        }
    }

    fn place_or_temp(&mut self, e: &'c ir::Expr) -> R<Addr> {
        if is_place(e) {
            self.place(e)
        } else {
            let v = self.eval(e)?;
            self.temp(&v, e.ty)
        }
    }

    /// The address of a global, materializing its value the first time.
    fn global_addr(&mut self, g: GlobalId) -> R<Addr> {
        if let Some(&a) = self.global_addrs.get(&g) {
            return Ok(a);
        }
        let global = &self.p.globals[g.0 as usize];
        if global.foreign {
            let mut f =
                self.fail(format!("reads the C value `{}`, which only exists when the program runs", global.c_name));
            f.kind = FailKind::Foreign;
            return Err(f);
        }
        let (size, align) = self.p.types.layout(global.ty);
        let addr = self.alloc_in(Region::Static, size, align)?;
        self.global_addrs.insert(g, addr);
        if let Some(embed) = &global.embed {
            let bytes = embed.bytes.clone();
            self.wr(addr, &bytes)?;
        } else if let Some(init) = &global.init {
            let v = self.eval(init)?;
            self.wr(addr, &v)?;
        }
        Ok(addr)
    }

    fn string_value(&mut self, ty: TyId, bytes: &[u8]) -> R<Vec<u8>> {
        let addr = self.mem.intern(bytes).map_err(|e| self.mem_fail(e, Span::default()))?;
        if matches!(self.kind(ty), TyKind::CString) {
            return Ok(addr.to_le_bytes().to_vec());
        }
        let mut out = addr.to_le_bytes().to_vec();
        out.extend_from_slice(&(bytes.len() as i64).to_le_bytes());
        Ok(out)
    }

    fn aggregate(&mut self, ty: TyId, elems: &'c [ir::Expr]) -> R<Vec<u8>> {
        let types = self.p.types;
        let mut out = vec![0u8; self.size(ty) as usize];
        let offsets: Vec<u64> = match self.kind(ty) {
            TyKind::Struct(id) => types.struct_info(*id).fields.iter().map(|f| f.offset).collect(),
            TyKind::Tuple(ts) => {
                let parts: Vec<(u64, u64)> = ts.iter().map(|t| types.layout(*t)).collect();
                crate::types::offsets(&parts)
            }
            TyKind::Array(elem, _) | TyKind::Matrix(elem, _, _) => {
                let stride = types.size_of(*elem);
                (0..elems.len() as u64).map(|i| i * stride).collect()
            }
            TyKind::Dynamic(_) | TyKind::Map(..) => vec![0, 8, 16, 24],
            _ => {
                let mut at = 0u64;
                elems
                    .iter()
                    .map(|e| {
                        let (s, a) = types.layout(e.ty);
                        let o = at.div_ceil(a.max(1)) * a.max(1);
                        at = o + s;
                        o
                    })
                    .collect()
            }
        };
        for (e, off) in elems.iter().zip(offsets) {
            let v = self.eval(e)?;
            let start = off as usize;
            let end = (start + v.len()).min(out.len());
            if start < end {
                out[start..end].copy_from_slice(&v[..end - start]);
            }
        }
        Ok(out)
    }

    fn cast(&self, from: TyId, v: &[u8], to: TyId) -> R<Vec<u8>> {
        let (fs, ts) = (self.scalar(from), self.scalar(to));
        if fs == Scalar::None || ts == Scalar::None {
            let mut out = v.to_vec();
            out.resize(self.size(to) as usize, 0);
            return Ok(out);
        }
        let n = self.decode(from, v);
        if let (Num::F(f), Scalar::Int(it)) = (n, ts) {
            let t = f.trunc();
            let (lo, hi) = it.range();
            if !t.is_finite() || t < lo as f64 || t > hi as f64 {
                return Err(self.fail(format!("`{f}` does not fit in `{}`", it.name())));
            }
        }
        Ok(self.encode(to, n))
    }

    fn binary(&mut self, op: BinaryOp, lhs: &'c ir::Expr, rhs: &'c ir::Expr, span: Span, ty: TyId) -> R<Vec<u8>> {
        use BinaryOp as B;
        if matches!(op, B::And | B::Or) {
            let l = self.eval(lhs)?;
            let l = Self::truthy(&l);
            if l == (op == B::Or) {
                return Ok(vec![u8::from(l)]);
            }
            let r = self.eval(rhs)?;
            return Ok(vec![u8::from(Self::truthy(&r))]);
        }
        let l = self.eval(lhs)?;
        let r = self.eval(rhs)?;
        let shaped = |t: TyId| matches!(self.p.types.kind(t), TyKind::Array(..) | TyKind::Matrix(..));
        let is_matrix = |t: TyId| matches!(self.p.types.kind(t), TyKind::Matrix(..));
        if op == B::Mul && shaped(lhs.ty) && shaped(rhs.ty) && (is_matrix(lhs.ty) || is_matrix(rhs.ty)) {
            return self.matrix_product(lhs.ty, &l, rhs.ty, &r, ty);
        }
        if (shaped(lhs.ty) || shaped(rhs.ty)) && !matches!(op, B::Eq | B::Ne) {
            return self.elementwise(op, lhs.ty, &l, rhs.ty, &r, ty, span);
        }
        match op {
            B::Eq | B::Ne => {
                let eq = self.equal(lhs.ty, &l, &r)?;
                Ok(vec![u8::from(eq == (op == B::Eq))])
            }
            _ => {
                if let TyKind::MultiPointer(elem) = self.kind(lhs.ty)
                    && matches!(op, B::Add | B::Sub)
                {
                    let stride = self.size(*elem) as i128;
                    let a = self.int(lhs.ty, &l);
                    let b = self.int(rhs.ty, &r);
                    if matches!(self.kind(rhs.ty), TyKind::MultiPointer(_)) {
                        return Ok(self.encode(ty, Num::I((a - b) / stride.max(1))));
                    }
                    let delta = if op == B::Add { b * stride } else { -b * stride };
                    return Ok(self.encode(ty, Num::I(a + delta)));
                }
                let a = self.decode(lhs.ty, &l);
                let b = self.decode(rhs.ty, &r);
                self.scalar_op(op, lhs.ty, a, b, ty, span)
            }
        }
    }

    fn scalar_op(&self, op: BinaryOp, operand: TyId, a: Num, b: Num, ty: TyId, span: Span) -> R<Vec<u8>> {
        use BinaryOp as B;
        let cmp = |o: std::cmp::Ordering| -> Vec<u8> {
            use std::cmp::Ordering as O;
            let r = match op {
                B::Lt => o == O::Less,
                B::Le => o != O::Greater,
                B::Gt => o == O::Greater,
                B::Ge => o != O::Less,
                _ => false,
            };
            vec![u8::from(r)]
        };
        match (a, b, self.scalar(operand)) {
            (_, _, Scalar::Float(ft)) => {
                let (x, y) = (as_f64(a), as_f64(b));
                let single = ft == FloatTy::F32;
                let f = |v: f64| if single { f64::from(v as f32) } else { v };
                let value = match op {
                    B::Add => f(x + y),
                    B::Sub => f(x - y),
                    B::Mul => f(x * y),
                    B::Div => f(x / y),
                    B::Rem => f(x % y),
                    B::Pow => {
                        if single {
                            f64::from((x as f32).powf(y as f32))
                        } else {
                            x.powf(y)
                        }
                    }
                    B::Cmp => {
                        let c = i128::from(x > y) - i128::from(x < y);
                        return Ok(self.encode(ty, Num::I(c)));
                    }
                    B::Lt | B::Le | B::Gt | B::Ge => {
                        return Ok(match x.partial_cmp(&y) {
                            Some(o) => cmp(o),
                            None => vec![0],
                        });
                    }
                    _ => return Err(self.fail_at(span, "unsupported operation on floats")),
                };
                Ok(self.encode(ty, Num::F(value)))
            }
            _ => {
                let (x, y) = (as_int(a), as_int(b));
                let it = match self.scalar(operand) {
                    Scalar::Int(it) => it,
                    _ => IntTy::U64,
                };
                let (lo, hi) = it.range();
                let bits = it.size() * 8;
                let checked = |v: Option<i128>, sym: &str| -> R<Vec<u8>> {
                    match v {
                        Some(v) if v >= lo && v <= hi => Ok(self.encode(ty, Num::I(v))),
                        _ => Err(self.fail_check(span, format!("integer overflow in `{sym}`"))),
                    }
                };
                match op {
                    B::Add => checked(x.checked_add(y), "+"),
                    B::Sub => checked(x.checked_sub(y), "-"),
                    B::Mul => checked(x.checked_mul(y), "*"),
                    B::Div | B::Rem => {
                        if y == 0 {
                            return Err(self.fail_check(span, "division by zero"));
                        }
                        let v = if op == B::Div { x.wrapping_div(y) } else { x.wrapping_rem(y) };
                        let v = if v > hi { if op == B::Div { lo } else { 0 } } else { v };
                        Ok(self.encode(ty, Num::I(v)))
                    }
                    B::Shl | B::Shr => {
                        let shift = (y as u128 & u128::from(u64::MAX)) as u64;
                        let v = if shift >= bits {
                            if op == B::Shr && it.signed() && x < 0 { -1 } else { 0 }
                        } else if op == B::Shl {
                            x.wrapping_shl(shift as u32)
                        } else {
                            x >> shift
                        };
                        Ok(self.encode(ty, Num::I(v)))
                    }
                    B::BitAnd => Ok(self.encode(ty, Num::I(x & y))),
                    B::BitOr => Ok(self.encode(ty, Num::I(x | y))),
                    B::BitXor => Ok(self.encode(ty, Num::I(x ^ y))),
                    B::Pow => {
                        if y < 0 {
                            return Err(self.fail_at(span, "negative exponent in integer `**`"));
                        }
                        let mut result: i128 = 1;
                        let (mut base, mut exp) = (x, y);
                        let wrap = |v: i128| int_from(&int_bytes(v, it.size()), it.size(), it.signed());
                        while exp > 0 {
                            if exp & 1 == 1 {
                                result = wrap(result.wrapping_mul(base));
                            }
                            base = wrap(base.wrapping_mul(base));
                            exp >>= 1;
                        }
                        Ok(self.encode(ty, Num::I(result)))
                    }
                    B::Cmp => Ok(self.encode(ty, Num::I(i128::from(x > y) - i128::from(x < y)))),
                    B::Lt | B::Le | B::Gt | B::Ge => Ok(cmp(x.cmp(&y))),
                    B::Eq | B::Ne | B::And | B::Or => Ok(vec![0]),
                }
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn elementwise(&self, op: BinaryOp, lt: TyId, l: &[u8], rt: TyId, r: &[u8], ty: TyId, span: Span) -> R<Vec<u8>> {
        let elem_of = |t: TyId| match self.p.types.kind(t) {
            TyKind::Array(e, n) => Some((*e, *n)),
            TyKind::Matrix(e, rows, cols) => Some((*e, u64::from(*rows) * u64::from(*cols))),
            _ => None,
        };
        let (elem, n) = elem_of(lt).or_else(|| elem_of(rt)).unwrap_or((lt, 1));
        let es = self.size(elem);
        let mut out = Vec::with_capacity(self.size(ty) as usize);
        for i in 0..n {
            let a = if elem_of(lt).is_some() { slice(l, i * es, es) } else { l.to_vec() };
            let b = if elem_of(rt).is_some() { slice(r, i * es, es) } else { r.to_vec() };
            let (x, y) = (self.decode(elem, &a), self.decode(elem, &b));
            let v = match (op, self.scalar(elem)) {
                (BinaryOp::Div | BinaryOp::Rem, Scalar::Int(_)) => self.scalar_op(op, elem, x, y, elem, span)?,
                (_, Scalar::Int(_)) => {
                    let (x, y) = (as_int(x), as_int(y));
                    let v = match op {
                        BinaryOp::Add => x.wrapping_add(y),
                        BinaryOp::Sub => x.wrapping_sub(y),
                        BinaryOp::Mul => x.wrapping_mul(y),
                        BinaryOp::BitAnd => x & y,
                        BinaryOp::BitOr => x | y,
                        _ => x ^ y,
                    };
                    self.encode(elem, Num::I(v))
                }
                _ => self.scalar_op(op, elem, x, y, elem, span)?,
            };
            out.extend_from_slice(&v);
        }
        Ok(out)
    }

    fn matrix_product(&self, lt: TyId, l: &[u8], rt: TyId, r: &[u8], ty: TyId) -> R<Vec<u8>> {
        let dims = |t: TyId| match self.p.types.kind(t) {
            TyKind::Matrix(e, rows, cols) => (*e, u64::from(*rows), u64::from(*cols)),
            TyKind::Array(e, n) => (*e, *n, 1),
            _ => (t, 1, 1),
        };
        let (elem, lr, lc) = dims(lt);
        let (_, _, rc) = dims(rt);
        let es = self.size(elem);
        let mut out = vec![0u8; self.size(ty) as usize];
        for col in 0..rc {
            for row in 0..lr {
                let mut acc = Num::I(0);
                let mut first = true;
                for k in 0..lc {
                    let a = self.decode(elem, &slice(l, (k * lr + row) * es, es));
                    let b = self.decode(elem, &slice(r, (col * lc + k) * es, es));
                    let prod = match (a, b) {
                        (Num::F(x), Num::F(y)) => Num::F(x * y),
                        (x, y) => Num::I(as_int(x).wrapping_mul(as_int(y))),
                    };
                    acc = if first {
                        prod
                    } else {
                        match (acc, prod) {
                            (Num::F(x), Num::F(y)) => Num::F(x + y),
                            (x, y) => Num::I(as_int(x).wrapping_add(as_int(y))),
                        }
                    };
                    first = false;
                    if let (Scalar::Float(FloatTy::F32), Num::F(x)) = (self.scalar(elem), acc) {
                        acc = Num::F(f64::from(x as f32));
                    }
                }
                let v = self.encode(elem, acc);
                let at = ((col * lr + row) * es) as usize;
                if at + v.len() <= out.len() {
                    out[at..at + v.len()].copy_from_slice(&v);
                }
            }
        }
        Ok(out)
    }

    /// Compares two values of the same type for equality, the way `==` does.
    fn equal(&self, ty: TyId, a: &[u8], b: &[u8]) -> R<bool> {
        let types = self.p.types;
        Ok(match self.kind(ty) {
            TyKind::String => {
                let (la, lb) = (i64_at(a, 8), i64_at(b, 8));
                la == lb && self.rd(Self::ptr(a), la.max(0) as u64)? == self.rd(Self::ptr(b), lb.max(0) as u64)?
            }
            TyKind::Struct(id) => {
                for f in &types.struct_info(*id).fields {
                    let s = types.size_of(f.ty);
                    if !self.equal(f.ty, &slice(a, f.offset, s), &slice(b, f.offset, s))? {
                        return Ok(false);
                    }
                }
                true
            }
            TyKind::Array(elem, _) | TyKind::Matrix(elem, _, _) => {
                let s = types.size_of(*elem);
                let n = a.len() as u64 / s.max(1);
                for i in 0..n {
                    if !self.equal(*elem, &slice(a, i * s, s), &slice(b, i * s, s))? {
                        return Ok(false);
                    }
                }
                true
            }
            TyKind::Tuple(ts) => {
                let parts: Vec<(u64, u64)> = ts.iter().map(|t| types.layout(*t)).collect();
                for (t, off) in ts.iter().zip(crate::types::offsets(&parts)) {
                    let s = types.size_of(*t);
                    if !self.equal(*t, &slice(a, off, s), &slice(b, off, s))? {
                        return Ok(false);
                    }
                }
                true
            }
            TyKind::Optional(inner) if !self.optional_is_pointer(ty) => {
                let s = types.size_of(*inner);
                let (ha, hb) = (a.get(s as usize).copied().unwrap_or(0), b.get(s as usize).copied().unwrap_or(0));
                match (ha != 0, hb != 0) {
                    (false, false) => true,
                    (true, true) => self.equal(*inner, &slice(a, 0, s), &slice(b, 0, s))?,
                    _ => false,
                }
            }
            TyKind::Float(_) => as_f64(self.decode(ty, a)) == as_f64(self.decode(ty, b)),
            _ => a == b,
        })
    }
}

/// Whether an expression names storage, so its address can be taken.
fn is_place(e: &ir::Expr) -> bool {
    match &e.kind {
        ExprKind::Local(_)
        | ExprKind::Global(_)
        | ExprKind::ConstGlobal(_)
        | ExprKind::Deref(_)
        | ExprKind::Index { .. } => true,
        ExprKind::Field { base, .. } | ExprKind::OptGet(base) | ExprKind::UnionGet { value: base, .. } => {
            is_place(base)
        }
        _ => false,
    }
}

/// The first eight bytes of a value as an array, zero-padded.
fn word<const N: usize>(bytes: &[u8]) -> [u8; N] {
    let mut out = [0u8; N];
    let n = bytes.len().min(N);
    out[..n].copy_from_slice(&bytes[..n]);
    out
}

/// `len` bytes of `v` from `offset`, zero-padded past its end.
fn slice(v: &[u8], offset: u64, len: u64) -> Vec<u8> {
    let start = (offset as usize).min(v.len());
    let end = (start + len as usize).min(v.len());
    let mut out = v[start..end].to_vec();
    out.resize(len as usize, 0);
    out
}

fn i64_at(v: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(word(&v[offset.min(v.len())..]))
}

/// Reads a little-endian integer of `size` bytes.
fn int_from(bytes: &[u8], size: u64, signed: bool) -> i128 {
    let raw = u64::from_le_bytes(word(&bytes[..(size as usize).min(bytes.len())]));
    let bits = size * 8;
    if bits >= 64 {
        return if signed { i128::from(raw as i64) } else { i128::from(raw) };
    }
    let masked = raw & ((1u64 << bits) - 1);
    if signed && masked >> (bits - 1) == 1 { i128::from(masked) - (1i128 << bits) } else { i128::from(masked) }
}

/// The low `size` bytes of `v`, little-endian (wrapping like a C cast).
fn int_bytes(v: i128, size: u64) -> Vec<u8> {
    v.to_le_bytes()[..size as usize].to_vec()
}

fn as_int(n: Num) -> i128 {
    match n {
        Num::I(v) => v,
        Num::F(f) => f as i128,
    }
}

fn as_f64(n: Num) -> f64 {
    match n {
        Num::I(v) => v as f64,
        Num::F(f) => f,
    }
}

/// Formats a count with thousands separators.
fn group_digits(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

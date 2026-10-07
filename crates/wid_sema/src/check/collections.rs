//! Arrays, slices, dynamic arrays, maps and strings: literals, indexing,
//! slicing, container methods, swizzles and iteration.

use wid_diagnostics::{Diagnostic, Span, codes};
use wid_syntax::ast::{self, ExprKind as E};

use super::Checker;
use super::body::Exit;
use super::members::is_place;
use crate::ir::{self, Builtin, ExprKind, LocalId, Stmt};
use crate::types::{TyId, TyKind};

impl<'a> Checker<'a> {
    /// Whether indexing at this point is bounds-checked: on unless the build
    /// turns checks off or the code is under `@[no_bounds_check]`.
    fn bounds_checked(&self) -> bool {
        self.input.options.bounds_checks
            && !self.no_bounds_check
            && !self.body.frames.last().is_some_and(|f| f.no_bounds)
    }

    pub(super) fn int_value(&mut self, e: &ast::Expr) -> ir::Expr {
        let int = self.types.int();
        let v = self.expr(e, Some(int));
        if self.types.is_int(v.ty) && v.ty != int {
            return self.convert(v, int, e.span);
        }
        self.coerce(v, int, e.span)
    }

    /// Makes a value safe to evaluate several times: places are kept, other
    /// values are stored in a temporary.
    pub fn stable(&mut self, v: ir::Expr) -> ir::Expr {
        if v.is_constant() || (is_place(&v) && stable_place(&v)) { v } else { self.spill(v) }
    }

    /// Lowers an array literal `[a, b, c]`.
    pub fn array_literal(&mut self, elems: &[ast::Expr], expected: Option<TyId>, span: Span) -> ir::Expr {
        let expected_kind = expected.map(|t| self.types.kind(self.types.base(t)).clone());
        match expected_kind {
            Some(TyKind::Matrix(..)) => self.matrix_literal(elems, expected.expect("checked above"), span),
            Some(TyKind::Array(elem, n)) => {
                let ty = expected.expect("checked above");
                if elems.len() as u64 != n {
                    let shown = self.types.display(ty);
                    self.report(
                        Diagnostic::error(
                            codes::TYPE_MISMATCH,
                            format!("expected {n} elements for `{shown}`, found {}", elems.len()),
                        )
                        .primary(span, format!("this literal has {} elements", elems.len())),
                    );
                }
                let values = self.lower_elems(elems, elem);
                ir::Expr::new(ExprKind::Aggregate(values), ty)
            }
            Some(TyKind::Slice(elem)) => {
                let arr = self.types.intern(TyKind::Array(elem, elems.len() as u64));
                let values = self.lower_elems(elems, elem);
                let tmp = self.spill(ir::Expr::new(ExprKind::Aggregate(values), arr));
                self.full_slice(tmp, expected.expect("checked above"))
            }
            Some(TyKind::Dynamic(_)) => {
                self.report(
                    Diagnostic::error(codes::TYPE_MISMATCH, "an array literal cannot become a dynamic array")
                        .primary(span, "dynamic arrays grow with an allocator")
                        .help("create it with `[dynamic]T.new` and append with `<<`"),
                );
                ir::Expr::new(ExprKind::Zero, expected.expect("checked above"))
            }
            _ => {
                let Some(first) = elems.first() else {
                    self.report(
                        Diagnostic::error(codes::CANNOT_INFER, "cannot infer the type of an empty array")
                            .primary(span, "no elements to infer from")
                            .help("give it a type, like `xs: [0]Int = []`, or use `[dynamic]Int.new`"),
                    );
                    return ir::Expr::new(ExprKind::Zero, self.types.unknown());
                };
                let v = self.expr(first, None);
                let elem = self.value_type(v.ty, first.span);
                let mut values = vec![v];
                for e in &elems[1..] {
                    self.begin_block();
                    let x = self.expr_coerced(e, elem);
                    let stmts = self.end_block().stmts;
                    if !stmts.is_empty() || !x.is_pure() {
                        self.spill_impure(&mut values);
                    }
                    for s in stmts {
                        self.emit(s);
                    }
                    values.push(x);
                }
                let ty = self.types.intern(TyKind::Array(elem, elems.len() as u64));
                ir::Expr::new(ExprKind::Aggregate(values), ty)
            }
        }
    }

    pub(super) fn lower_elems(&mut self, elems: &[ast::Expr], elem: TyId) -> Vec<ir::Expr> {
        let mut values: Vec<ir::Expr> = Vec::new();
        for e in elems {
            self.begin_block();
            let x = self.expr_coerced(e, elem);
            let stmts = self.end_block().stmts;
            if !stmts.is_empty() || !x.is_pure() {
                self.spill_impure(&mut values);
            }
            for s in stmts {
                self.emit(s);
            }
            values.push(x);
        }
        values
    }

    /// A slice of every element of an array, dynamic array or slice place.
    pub fn full_slice(&mut self, base: ir::Expr, slice_ty: TyId) -> ir::Expr {
        let int = self.types.int();
        let len = self.len_of(base.clone());
        ir::Expr::new(
            ExprKind::SliceOf {
                base: Box::new(base),
                lo: Box::new(ir::Expr::new(ExprKind::Int(0), int)),
                hi: Box::new(len),
                checked: false,
                span: Span::default(),
            },
            slice_ty,
        )
    }

    /// The element count of a container value.
    pub fn len_of(&mut self, v: ir::Expr) -> ir::Expr {
        let int = self.types.int();
        if let TyKind::Array(_, n) = self.types.kind(self.types.base(v.ty)) {
            return ir::Expr::new(ExprKind::Int(i128::from(*n)), int);
        }
        ir::Expr::new(ExprKind::Builtin { op: Builtin::Len, args: vec![v], span: Span::default() }, int)
    }

    /// Returns the element type of an indexable type.
    pub fn elem_type(&self, ty: TyId) -> Option<TyId> {
        match self.types.kind(self.types.base(ty)) {
            TyKind::Array(e, _) | TyKind::Slice(e) | TyKind::Dynamic(e) | TyKind::MultiPointer(e) => Some(*e),
            TyKind::Matrix(e, _, _) => Some(*e),
            _ => None,
        }
    }

    /// Lowers `recv[args]`.
    pub fn index_expr(&mut self, recv: &ast::Expr, args: &[ast::Expr], span: Span) -> ir::Expr {
        let v = self.expr(recv, None);
        let v = match self.types.kind(v.ty).clone() {
            TyKind::Pointer(inner)
                if self.elem_type(inner).is_some() || matches!(self.types.kind(inner), TyKind::Map(..)) =>
            {
                ir::Expr::new(ExprKind::Deref(Box::new(v)), inner)
            }
            _ => v,
        };
        let ty = v.ty;
        let kind = self.types.kind(self.types.base(ty)).clone();
        if matches!(kind, TyKind::Unknown) {
            for a in args.iter().filter(|a| !matches!(a.kind, E::Range { .. })) {
                self.expr(a, None);
            }
            return ir::Expr::new(ExprKind::Zero, ty);
        }
        if let Some(decl) = self.operator_method(ty, "[]") {
            let recv_ptr = self.address_of(v);
            let call_args: Vec<ast::Arg> =
                args.iter().map(|a| ast::Arg { name: None, value: a.clone(), splat: false }).collect();
            if let super::DeclKind::Overload(_) = self.decls[decl.0 as usize].kind {
                return self.call_overloaded(decl, Some(recv_ptr), Some(ty), &call_args, recv.span, span);
            }
            return self.call_fn(decl, Some(recv_ptr), &call_args, None, recv.span, span);
        }
        if let TyKind::Matrix(..) = kind {
            return self.matrix_index(v, args, span);
        }
        let [arg] = args else {
            self.report(
                Diagnostic::error(codes::ARG_COUNT, "indexing takes one index")
                    .primary(span, format!("found {} indices", args.len())),
            );
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        };
        if let TyKind::Map(k, val) = kind {
            let m = self.stable(v);
            let ptr = self.address_of(m);
            let key = self.expr_coerced(arg, k);
            let key = self.stable(key);
            let val_ptr = self.types.pointer(val);
            let found = ir::Expr::new(ExprKind::Builtin { op: Builtin::MapFind, args: vec![ptr, key], span }, val_ptr);
            let found = self.spill(found);
            let opt = self.types.optional(val);
            let bool_ty = self.types.bool();
            let null = ir::Expr::new(ExprKind::Nil, val_ptr);
            let some = ir::Expr::new(
                ExprKind::Binary { op: ir::BinaryOp::Ne, lhs: Box::new(found.clone()), rhs: Box::new(null), span },
                bool_ty,
            );
            let value = ir::Expr::new(ExprKind::Deref(Box::new(found)), val);
            let wrapped = self.opt_some(value, opt);
            let nil = ir::Expr::new(ExprKind::Nil, opt);
            return ir::Expr::new(
                ExprKind::Select { cond: Box::new(some), then: Box::new(wrapped), else_: Box::new(nil) },
                opt,
            );
        }
        let is_string = matches!(kind, TyKind::String);
        if !is_string && self.elem_type(ty).is_none() {
            let shown = self.types.display(ty);
            self.report(
                Diagnostic::error(codes::NO_OPERATOR, format!("`{shown}` cannot be indexed"))
                    .primary(recv.span, "not an array, slice, string or map")
                    .help("to make a struct indexable, define `def [](i: Int) -> T`"),
            );
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        }
        let base = self.stable(v);
        if let E::Range { lo, hi, inclusive } = &arg.kind {
            return self.slice_expr(base, lo.as_deref(), hi.as_deref(), *inclusive, span);
        }
        let index = self.int_value(arg);
        let index = self.stable(index);
        let elem = if is_string { self.types.u8() } else { self.elem_type(ty).expect("checked above") };
        let checked = self.bounds_checked() && !matches!(kind, TyKind::MultiPointer(_));
        ir::Expr::new(ExprKind::Index { base: Box::new(base), index: Box::new(index), checked, span }, elem)
    }

    fn slice_expr(
        &mut self,
        base: ir::Expr,
        lo: Option<&ast::Expr>,
        hi: Option<&ast::Expr>,
        inclusive: bool,
        span: Span,
    ) -> ir::Expr {
        let int = self.types.int();
        let lo = match lo {
            Some(e) => {
                let v = self.int_value(e);
                self.stable(v)
            }
            None => ir::Expr::new(ExprKind::Int(0), int),
        };
        let hi = match hi {
            Some(e) => {
                let v = self.int_value(e);
                let v = if inclusive {
                    ir::Expr::new(
                        ExprKind::Binary {
                            op: ir::BinaryOp::Add,
                            lhs: Box::new(v),
                            rhs: Box::new(ir::Expr::new(ExprKind::Int(1), int)),
                            span,
                        },
                        int,
                    )
                } else {
                    v
                };
                self.stable(v)
            }
            None => {
                if matches!(self.types.kind(base.ty), TyKind::MultiPointer(_)) {
                    self.report(
                        Diagnostic::error(codes::TYPE_MISMATCH, "slicing a multi-pointer needs an end index")
                            .primary(span, "a `[^]T` does not know its length"),
                    );
                }
                let len = self.len_of(base.clone());
                self.stable(len)
            }
        };
        let result = match self.types.kind(self.types.base(base.ty)) {
            TyKind::String => self.types.string(),
            _ => {
                let elem = self.elem_type(base.ty).expect("indexable");
                self.types.slice(elem)
            }
        };
        let checked = self.bounds_checked();
        ir::Expr::new(
            ExprKind::SliceOf { base: Box::new(base), lo: Box::new(lo), hi: Box::new(hi), checked, span },
            result,
        )
    }

    /// Lowers `x[i] = v` and `x[i] op= v` through the `[]=` and `[]` methods
    /// of a struct or enum; returns false when `target` is not such an index.
    pub fn assign_index_method(
        &mut self,
        target: &ast::Expr,
        op: Option<ast::BinOp>,
        value: &ast::Expr,
        span: Span,
    ) -> bool {
        let E::Index { recv, args } = &target.kind else { return false };
        self.begin_block();
        let probe = self.expr(recv, None);
        self.end_block();
        let ty = match self.types.kind(probe.ty) {
            TyKind::Pointer(inner) => *inner,
            _ => probe.ty,
        };
        if !matches!(self.types.kind(ty), TyKind::Struct(_) | TyKind::Enum(_)) {
            return false;
        }
        let shown = self.types.display(ty);
        let Some(setter) = self.operator_method(ty, "[]=") else {
            self.report(
                Diagnostic::error(codes::NOT_ASSIGNABLE, format!("`{shown}` has no `[]=` method"))
                    .primary(target.span, "cannot assign through this index")
                    .help(format!("define `def []=(index: Int, value: T)` in `{shown}` to support `x[i] = value`")),
            );
            return true;
        };
        let getter = match op {
            Some(_) => match self.operator_method(ty, "[]") {
                Some(g) => Some(g),
                None => {
                    self.report(
                        Diagnostic::error(codes::NOT_ASSIGNABLE, format!("`{shown}` has `[]=` but no `[]` method"))
                            .primary(target.span, "a compound assignment reads the old value first")
                            .help(format!("define `def [](index: Int) -> T` in `{shown}`, or assign with `=`")),
                    );
                    return true;
                }
            },
            None => None,
        };
        let base = self.expr(recv, None);
        let base = match self.types.kind(base.ty).clone() {
            TyKind::Pointer(inner) => ir::Expr::new(ExprKind::Deref(Box::new(base)), inner),
            _ => base,
        };
        let ptr = self.address_of(base);
        let ptr = self.stable(ptr);
        let mut index_values = Vec::new();
        for a in args {
            let v = self.expr(a, None);
            index_values.push(self.stable(v));
        }
        let index_exprs: Vec<&ast::Expr> = args.iter().collect();
        let new_value = match (op, getter) {
            (Some(op), Some(getter)) => {
                let old = self.call_with_values(
                    getter,
                    Some(ptr.clone()),
                    Some(ty),
                    &index_exprs,
                    index_values.clone(),
                    target.span,
                    span,
                );
                let old = self.stable(old);
                let expected = self.rhs_expected(op, old.ty);
                let rhs = self.expr(value, expected);
                self.combine_values(op, old, value, rhs, target.span, span)
            }
            _ => self.expr(value, None),
        };
        let value_expr = if op.is_some() { target } else { value };
        let mut exprs = index_exprs;
        exprs.push(value_expr);
        let mut values = index_values;
        values.push(new_value);
        let call = self.call_with_values(setter, Some(ptr), Some(ty), &exprs, values, target.span, span);
        self.emit(Stmt::Expr(call));
        true
    }

    /// Lowers `m[k] = v` for maps; returns false when `target` is not a map index.
    pub fn assign_map_index(&mut self, target: &ast::Expr, value: &ast::Expr) -> bool {
        let E::Index { recv, args } = &target.kind else { return false };
        let probe_ty = {
            self.begin_block();
            let v = self.expr(recv, None);
            self.end_block();
            v.ty
        };
        let probe_ty = match self.types.kind(probe_ty) {
            TyKind::Pointer(inner) => *inner,
            _ => probe_ty,
        };
        let TyKind::Map(k, val) = self.types.kind(probe_ty).clone() else { return false };
        let m = self.expr(recv, None);
        let m = match self.types.kind(m.ty).clone() {
            TyKind::Pointer(inner) => ir::Expr::new(ExprKind::Deref(Box::new(m)), inner),
            _ => m,
        };
        if !is_place(&m) {
            self.report(
                Diagnostic::error(codes::NOT_ASSIGNABLE, "cannot store into a temporary map")
                    .primary(recv.span, "store the map in a variable first"),
            );
            return true;
        }
        let ptr = self.address_of(m);
        let [key] = args.as_slice() else {
            self.report(Diagnostic::error(codes::ARG_COUNT, "a map index takes one key").primary(target.span, ""));
            return true;
        };
        let key = self.expr_coerced(key, k);
        let key = self.stable(key);
        let v = self.expr_coerced(value, val);
        let v = if v.is_constant() { v } else { self.spill(v) };
        let slot = self.map_slot(ptr, key, val, target.span);
        self.emit(Stmt::Assign { target: ir::Expr::new(ExprKind::Deref(Box::new(slot)), val), value: v });
        true
    }

    /// Lowers `m[k] ||= v` (store when the key is missing) and `m[k] &&= v`
    /// (store when it is present). Returns false when `target` is not a map
    /// entry.
    pub fn map_logical_assign(&mut self, target: &ast::Expr, is_or: bool, value: &ast::Expr) -> bool {
        let E::Index { recv, args } = &target.kind else { return false };
        self.begin_block();
        let probe = self.expr(recv, None);
        self.end_block();
        let map_ty = match self.types.kind(probe.ty) {
            TyKind::Pointer(inner) => *inner,
            _ => probe.ty,
        };
        let TyKind::Map(k, val) = self.types.kind(map_ty).clone() else { return false };
        let [key] = args.as_slice() else { return false };
        let m = self.expr(recv, None);
        let m = match self.types.kind(m.ty).clone() {
            TyKind::Pointer(inner) => ir::Expr::new(ExprKind::Deref(Box::new(m)), inner),
            _ => m,
        };
        if !is_place(&m) {
            self.report(
                Diagnostic::error(codes::NOT_ASSIGNABLE, "cannot store into a temporary map")
                    .primary(recv.span, "this map is a temporary value")
                    .help("store the map in a variable first"),
            );
            return true;
        }
        let ptr = self.address_of(m);
        let ptr = self.stable(ptr);
        let key = self.expr_coerced(key, k);
        let key = self.stable(key);
        let raw = self.types.rawptr();
        let bool_ty = self.types.bool();
        let found = ir::Expr::new(
            ExprKind::Builtin { op: Builtin::MapFind, args: vec![ptr.clone(), key.clone()], span: target.span },
            raw,
        );
        let null = ir::Expr::new(ExprKind::Nil, raw);
        let op = if is_or { ir::BinaryOp::Eq } else { ir::BinaryOp::Ne };
        let cond = ir::Expr::new(
            ExprKind::Binary { op, lhs: Box::new(found), rhs: Box::new(null), span: target.span },
            bool_ty,
        );
        self.begin_block();
        let v = self.expr_coerced(value, val);
        let v = if v.is_constant() { v } else { self.spill(v) };
        let slot = self.map_slot(ptr, key, val, target.span);
        self.emit(Stmt::Assign { target: ir::Expr::new(ExprKind::Deref(Box::new(slot)), val), value: v });
        let then = self.end_block();
        self.emit(Stmt::If { cond, then, else_: ir::Block::default() });
        true
    }

    /// For `m[k] op= v`, returns the value slot of `k` as a place, inserting
    /// a zero value first when the key is missing.
    pub fn map_index_slot(&mut self, target: &ast::Expr) -> Option<ir::Expr> {
        let E::Index { recv, args } = &target.kind else { return None };
        self.begin_block();
        let probe = self.expr(recv, None);
        self.end_block();
        let map_ty = match self.types.kind(probe.ty) {
            TyKind::Pointer(inner) => *inner,
            _ => probe.ty,
        };
        let TyKind::Map(k, val) = self.types.kind(map_ty).clone() else { return None };
        let m = self.expr(recv, None);
        let m = match self.types.kind(m.ty).clone() {
            TyKind::Pointer(inner) => ir::Expr::new(ExprKind::Deref(Box::new(m)), inner),
            _ => m,
        };
        let [key] = args.as_slice() else { return None };
        let ptr = self.address_of(m);
        let key = self.expr_coerced(key, k);
        let key = self.stable(key);
        let slot = self.map_slot(ptr, key, val, target.span);
        let slot = self.spill(slot);
        Some(ir::Expr::new(ExprKind::Deref(Box::new(slot)), val))
    }

    /// Returns a pointer to the value slot of `key`, inserting a zero value
    /// when the key is missing.
    pub fn map_slot(&mut self, map_ptr: ir::Expr, key: ir::Expr, val: TyId, span: Span) -> ir::Expr {
        let val_ptr = self.types.pointer(val);
        ir::Expr::new(ExprKind::Builtin { op: Builtin::MapPut, args: vec![map_ptr, key], span }, val_ptr)
    }

    /// Methods on arrays, slices, dynamic arrays, maps and strings.
    pub fn container_method(
        &mut self,
        v: &ir::Expr,
        name: ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<ir::Expr> {
        let ty = v.ty;
        let kind = self.types.kind(self.types.base(ty)).clone();
        let int = self.types.int();
        let bool_ty = self.types.bool();
        let void = self.types.void();
        let text = name.as_str();
        if let Some(result) = self.matrix_method(v, name, args, span) {
            return Some(result);
        }
        let needs_place = |this: &mut Self, v: &ir::Expr| -> bool {
            if is_place(v) {
                return true;
            }
            this.report(
                Diagnostic::error(
                    codes::NOT_ASSIGNABLE,
                    format!("`{text}` changes the container, but this is a temporary copy"),
                )
                .primary(name.span, "call it on a variable or field"),
            );
            false
        };
        let one_arg = |this: &mut Self| -> Option<&ast::Expr> {
            match args {
                [a] => Some(&a.value),
                _ => {
                    this.report(
                        Diagnostic::error(codes::ARG_COUNT, format!("`{text}` takes one argument"))
                            .primary(span, format!("found {}", args.len())),
                    );
                    None
                }
            }
        };
        match (&kind, text) {
            (TyKind::Array(..) | TyKind::Slice(_) | TyKind::Dynamic(_) | TyKind::Map(..), "size") => {
                self.expect_no_args(args, name);
                Some(self.len_of(v.clone()))
            }
            (TyKind::Array(..) | TyKind::Slice(_) | TyKind::Dynamic(_) | TyKind::Map(..), "empty?") => {
                self.expect_no_args(args, name);
                let len = self.len_of(v.clone());
                let zero = ir::Expr::new(ExprKind::Int(0), int);
                Some(ir::Expr::new(
                    ExprKind::Binary { op: ir::BinaryOp::Eq, lhs: Box::new(len), rhs: Box::new(zero), span },
                    bool_ty,
                ))
            }
            (TyKind::Dynamic(_), "capacity") => {
                self.expect_no_args(args, name);
                let field = 2;
                Some(ir::Expr::new(ExprKind::Field { base: Box::new(v.clone()), index: field }, int))
            }
            (TyKind::Dynamic(elem), "push" | "<<") => {
                let value = one_arg(self)?;
                if !needs_place(self, v) {
                    return Some(ir::Expr::new(ExprKind::Zero, void));
                }
                self.dyn_push(v.clone(), *elem, value, span);
                Some(ir::Expr::new(ExprKind::Zero, void))
            }
            (TyKind::Dynamic(elem), "insert") => {
                let [i, x] = args else {
                    self.report(
                        Diagnostic::error(codes::ARG_COUNT, "`insert` takes an index and a value").primary(span, ""),
                    );
                    return Some(ir::Expr::new(ExprKind::Zero, void));
                };
                if !needs_place(self, v) {
                    return Some(ir::Expr::new(ExprKind::Zero, void));
                }
                let x = self.expr_coerced(&x.value, *elem);
                let x = if x.is_constant() { x } else { self.spill(x) };
                let i = self.int_value(&i.value);
                let i = self.stable(i);
                let ptr = self.address_of(v.clone());
                let elem_ptr = self.types.pointer(*elem);
                let slot =
                    ir::Expr::new(ExprKind::Builtin { op: Builtin::DynInsert, args: vec![ptr, i], span }, elem_ptr);
                self.emit(Stmt::Assign { target: ir::Expr::new(ExprKind::Deref(Box::new(slot)), *elem), value: x });
                Some(ir::Expr::new(ExprKind::Zero, void))
            }
            (TyKind::Dynamic(_), "delete_at") => {
                let i = one_arg(self)?;
                if !needs_place(self, v) {
                    return Some(ir::Expr::new(ExprKind::Zero, void));
                }
                let i = self.int_value(i);
                let ptr = self.address_of(v.clone());
                Some(ir::Expr::new(ExprKind::Builtin { op: Builtin::DynRemove, args: vec![ptr, i], span }, void))
            }
            (TyKind::Dynamic(elem), "concat") => {
                let xs = one_arg(self)?;
                if !needs_place(self, v) {
                    return Some(ir::Expr::new(ExprKind::Zero, void));
                }
                let slice_ty = self.types.slice(*elem);
                let ptr = self.address_of(v.clone());
                let ptr = self.stable(ptr);
                let xs = self.expr_coerced(xs, slice_ty);
                let xs = self.stable(xs);
                Some(ir::Expr::new(ExprKind::Builtin { op: Builtin::DynAppend, args: vec![ptr, xs], span }, void))
            }
            (TyKind::Dynamic(_), "resize") => {
                let n = one_arg(self)?;
                if !needs_place(self, v) {
                    return Some(ir::Expr::new(ExprKind::Zero, void));
                }
                let n = self.int_value(n);
                let ptr = self.address_of(v.clone());
                Some(ir::Expr::new(ExprKind::Builtin { op: Builtin::DynResize, args: vec![ptr, n], span }, void))
            }
            (TyKind::Dynamic(_), "reserve") => {
                let n = one_arg(self)?;
                if !needs_place(self, v) {
                    return Some(ir::Expr::new(ExprKind::Zero, void));
                }
                let n = self.int_value(n);
                let ptr = self.address_of(v.clone());
                Some(ir::Expr::new(ExprKind::Builtin { op: Builtin::DynReserve, args: vec![ptr, n], span }, void))
            }
            (TyKind::Dynamic(_), "clear") => {
                self.expect_no_args(args, name);
                if !needs_place(self, v) {
                    return Some(ir::Expr::new(ExprKind::Zero, void));
                }
                let len = ir::Expr::new(ExprKind::Field { base: Box::new(v.clone()), index: 1 }, int);
                self.emit(Stmt::Assign { target: len, value: ir::Expr::new(ExprKind::Int(0), int) });
                Some(ir::Expr::new(ExprKind::Zero, void))
            }
            (TyKind::Dynamic(elem), "pop") => {
                self.expect_no_args(args, name);
                if !needs_place(self, v) {
                    return Some(ir::Expr::new(ExprKind::Zero, void));
                }
                let opt = self.types.optional(*elem);
                let result = self.new_local(None, opt);
                self.emit(Stmt::Let { local: result, init: Some(ir::Expr::new(ExprKind::Nil, opt)) });
                let len = ir::Expr::new(ExprKind::Field { base: Box::new(v.clone()), index: 1 }, int);
                let zero = ir::Expr::new(ExprKind::Int(0), int);
                let one = ir::Expr::new(ExprKind::Int(1), int);
                let has = ir::Expr::new(
                    ExprKind::Binary { op: ir::BinaryOp::Gt, lhs: Box::new(len.clone()), rhs: Box::new(zero), span },
                    bool_ty,
                );
                let dec = ir::Expr::new(
                    ExprKind::Binary { op: ir::BinaryOp::Sub, lhs: Box::new(len.clone()), rhs: Box::new(one), span },
                    int,
                );
                let item = ir::Expr::new(
                    ExprKind::Index { base: Box::new(v.clone()), index: Box::new(len.clone()), checked: false, span },
                    *elem,
                );
                let wrapped = self.opt_some(item, opt);
                self.emit(Stmt::If {
                    cond: has,
                    then: ir::Block {
                        stmts: vec![
                            Stmt::Assign { target: len, value: dec },
                            Stmt::Assign { target: ir::Expr::new(ExprKind::Local(result), opt), value: wrapped },
                        ],
                    },
                    else_: ir::Block::default(),
                });
                Some(ir::Expr::new(ExprKind::Local(result), opt))
            }
            (TyKind::Array(elem, _) | TyKind::Slice(elem) | TyKind::Dynamic(elem), "first" | "last") => {
                self.expect_no_args(args, name);
                let base = self.stable(v.clone());
                let len = self.len_of(base.clone());
                let len = self.stable(len);
                let opt = self.types.optional(*elem);
                let zero = ir::Expr::new(ExprKind::Int(0), int);
                let index = if text == "first" {
                    zero.clone()
                } else {
                    let one = ir::Expr::new(ExprKind::Int(1), int);
                    ir::Expr::new(
                        ExprKind::Binary {
                            op: ir::BinaryOp::Sub,
                            lhs: Box::new(len.clone()),
                            rhs: Box::new(one),
                            span,
                        },
                        int,
                    )
                };
                let has = ir::Expr::new(
                    ExprKind::Binary { op: ir::BinaryOp::Gt, lhs: Box::new(len), rhs: Box::new(zero), span },
                    bool_ty,
                );
                let item = ir::Expr::new(
                    ExprKind::Index { base: Box::new(base), index: Box::new(index), checked: false, span },
                    *elem,
                );
                let wrapped = self.opt_some(item, opt);
                let nil = ir::Expr::new(ExprKind::Nil, opt);
                Some(ir::Expr::new(
                    ExprKind::Select { cond: Box::new(has), then: Box::new(wrapped), else_: Box::new(nil) },
                    opt,
                ))
            }
            (TyKind::Array(elem, _) | TyKind::Dynamic(elem), "to_slice") => {
                self.expect_no_args(args, name);
                let base = self.stable(v.clone());
                let slice = self.types.slice(*elem);
                Some(self.full_slice(base, slice))
            }
            (TyKind::Map(k, _), "has_key?" | "delete") => {
                let key = one_arg(self)?;
                let m = self.stable(v.clone());
                if text == "delete" && !needs_place(self, &m) {
                    return Some(ir::Expr::new(ExprKind::Zero, bool_ty));
                }
                let ptr = self.address_of(m);
                let key = self.expr_coerced(key, *k);
                let key = self.stable(key);
                if text == "delete" {
                    return Some(ir::Expr::new(
                        ExprKind::Builtin { op: Builtin::MapRemove, args: vec![ptr, key], span },
                        bool_ty,
                    ));
                }
                let raw = self.types.rawptr();
                let found = ir::Expr::new(ExprKind::Builtin { op: Builtin::MapFind, args: vec![ptr, key], span }, raw);
                let null = ir::Expr::new(ExprKind::Nil, raw);
                Some(ir::Expr::new(
                    ExprKind::Binary { op: ir::BinaryOp::Ne, lhs: Box::new(found), rhs: Box::new(null), span },
                    bool_ty,
                ))
            }
            (TyKind::String, "include?" | "index" | "starts_with?" | "ends_with?") => {
                let needle = one_arg(self)?;
                let string = self.types.string();
                let s = self.stable(v.clone());
                let n = self.expr_coerced(needle, string);
                let n = self.stable(n);
                match text {
                    "include?" | "index" => {
                        let found =
                            ir::Expr::new(ExprKind::Builtin { op: Builtin::StringFind, args: vec![s, n], span }, int);
                        let found = self.spill(found);
                        let zero = ir::Expr::new(ExprKind::Int(0), int);
                        let has = ir::Expr::new(
                            ExprKind::Binary {
                                op: ir::BinaryOp::Ge,
                                lhs: Box::new(found.clone()),
                                rhs: Box::new(zero),
                                span,
                            },
                            bool_ty,
                        );
                        if text == "include?" {
                            return Some(has);
                        }
                        let opt = self.types.optional(int);
                        let wrapped = self.opt_some(found, opt);
                        let nil = ir::Expr::new(ExprKind::Nil, opt);
                        Some(ir::Expr::new(
                            ExprKind::Select { cond: Box::new(has), then: Box::new(wrapped), else_: Box::new(nil) },
                            opt,
                        ))
                    }
                    _ => {
                        let s_len = self.len_of(s.clone());
                        let n_len = self.len_of(n.clone());
                        let fits = ir::Expr::new(
                            ExprKind::Binary {
                                op: ir::BinaryOp::Ge,
                                lhs: Box::new(s_len.clone()),
                                rhs: Box::new(n_len.clone()),
                                span,
                            },
                            bool_ty,
                        );
                        let lo = if text == "starts_with?" {
                            ir::Expr::new(ExprKind::Int(0), int)
                        } else {
                            ir::Expr::new(
                                ExprKind::Binary {
                                    op: ir::BinaryOp::Sub,
                                    lhs: Box::new(s_len.clone()),
                                    rhs: Box::new(n_len.clone()),
                                    span,
                                },
                                int,
                            )
                        };
                        let hi = if text == "starts_with?" { n_len } else { s_len };
                        let part = ir::Expr::new(
                            ExprKind::SliceOf {
                                base: Box::new(s),
                                lo: Box::new(lo),
                                hi: Box::new(hi),
                                checked: false,
                                span,
                            },
                            string,
                        );
                        let eq = ir::Expr::new(
                            ExprKind::Binary { op: ir::BinaryOp::Eq, lhs: Box::new(part), rhs: Box::new(n), span },
                            bool_ty,
                        );
                        let fls = ir::Expr::new(ExprKind::Bool(false), bool_ty);
                        Some(ir::Expr::new(
                            ExprKind::Select { cond: Box::new(fits), then: Box::new(eq), else_: Box::new(fls) },
                            bool_ty,
                        ))
                    }
                }
            }
            (TyKind::Array(elem, n), _) if *n <= 4 && is_swizzle(text) && self.types.is_numeric(*elem) => {
                let indices: Vec<u64> = text.chars().map(swizzle_index).collect();
                if indices.iter().any(|i| *i >= *n) {
                    self.report(
                        Diagnostic::error(
                            codes::NO_SUCH_MEMBER,
                            format!("`{text}` reaches past the {n} elements of this array"),
                        )
                        .primary(name.span, "component out of range"),
                    );
                    return Some(ir::Expr::new(ExprKind::Zero, self.types.unknown()));
                }
                let base = self.stable(v.clone());
                let items: Vec<ir::Expr> = indices
                    .iter()
                    .map(|i| {
                        ir::Expr::new(
                            ExprKind::Index {
                                base: Box::new(base.clone()),
                                index: Box::new(ir::Expr::new(ExprKind::Int(i128::from(*i)), int)),
                                checked: false,
                                span,
                            },
                            *elem,
                        )
                    })
                    .collect();
                if items.len() == 1 {
                    return items.into_iter().next();
                }
                let ty = self.types.intern(TyKind::Array(*elem, items.len() as u64));
                Some(ir::Expr::new(ExprKind::Aggregate(items), ty))
            }
            _ => None,
        }
    }

    fn expect_no_args(&mut self, args: &[ast::Arg], name: ast::Ident) {
        if let Some(first) = args.first() {
            self.report(
                Diagnostic::error(codes::ARG_COUNT, format!("`{}` takes no arguments", name.as_str()))
                    .primary(first.value.span, "remove this argument"),
            );
        }
    }

    /// Appends `value` to the dynamic array place `d`.
    pub fn dyn_push(&mut self, d: ir::Expr, elem: TyId, value: &ast::Expr, span: Span) {
        let x = self.expr_coerced(value, elem);
        let x = if x.is_constant() { x } else { self.spill(x) };
        let ptr = self.address_of(d);
        let elem_ptr = self.types.pointer(elem);
        let slot = ir::Expr::new(ExprKind::Builtin { op: Builtin::DynPush, args: vec![ptr], span }, elem_ptr);
        self.emit(Stmt::Assign { target: ir::Expr::new(ExprKind::Deref(Box::new(slot)), elem), value: x });
    }

    /// `[dynamic]T.new` and `map[K]V.new`: empty containers that will grow
    /// with `context.allocator` (or `allocator:`).
    pub fn container_new(&mut self, ty: TyId, args: &[ast::Arg], span: Span) -> ir::Expr {
        let allocator_ty = self.types.allocator_ty;
        let mut allocator = None;
        for a in args {
            match a.name {
                Some(n) if n.as_str() == "allocator" => allocator = Some(self.expr_coerced(&a.value, allocator_ty)),
                _ => {
                    self.report(
                        Diagnostic::error(codes::BAD_NAMED_ARG, "containers take only `allocator:` when created")
                            .primary(a.value.span, "unexpected argument")
                            .help("reserve room afterwards with `.reserve(n)`"),
                    );
                }
            }
        }
        let allocator = allocator.unwrap_or_else(|| self.context_field("allocator"));
        let int = self.types.int();
        let raw = self.types.rawptr();
        let zero = ir::Expr::new(ExprKind::Int(0), int);
        let _ = span;
        let data = ir::Expr::new(ExprKind::Nil, raw);
        ir::Expr::new(ExprKind::Aggregate(vec![data, zero.clone(), zero, allocator]), ty)
    }

    // ----- iteration ------------------------------------------------------------

    /// Lowers `for x in xs`, `for &x in xs`, `for x, i in xs` over arrays,
    /// slices, dynamic arrays, strings (runes) and maps (`for k, v in m`).
    pub fn lower_for_collection(&mut self, f: &ast::ForExpr, iter: ir::Expr, span: Span) {
        let kind = self.types.kind(self.types.base(iter.ty)).clone();
        match kind {
            TyKind::Map(k, v) => self.for_map(f, iter, k, v, span),
            TyKind::String => self.for_string(f, iter, span),
            TyKind::Array(elem, _) | TyKind::Slice(elem) | TyKind::Dynamic(elem) => {
                self.for_indexed(f, iter, elem, span)
            }
            TyKind::Unknown => {}
            _ => {
                let shown = self.types.display(iter.ty);
                self.report(
                    Diagnostic::error(codes::TYPE_MISMATCH, format!("cannot iterate over `{shown}`"))
                        .primary(f.iter.span, "not a range, array, slice, dynamic array, string or map"),
                );
            }
        }
    }

    fn for_indexed(&mut self, f: &ast::ForExpr, iter: ir::Expr, elem: TyId, span: Span) {
        let int = self.types.int();
        let bool_ty = self.types.bool();
        let by_ref = f.bindings.first().is_some_and(|b| b.by_ref);
        let base = if is_place(&iter) {
            let ptr = self.address_of(iter);
            let ptr = self.spill(ptr);
            let pointee = match self.types.kind(ptr.ty) {
                TyKind::Pointer(t) => *t,
                _ => ptr.ty,
            };
            ir::Expr::new(ExprKind::Deref(Box::new(ptr)), pointee)
        } else {
            if by_ref {
                self.report(
                    Diagnostic::error(codes::BY_REF_NOT_PLACE, "cannot bind elements of a temporary by reference")
                        .primary(f.iter.span, "this value is a temporary copy")
                        .help("store it in a variable first, or drop the `&`"),
                );
            }
            self.spill(iter)
        };
        if f.bindings.len() > 2 {
            self.report(
                Diagnostic::error(codes::ARG_COUNT, "`for` binds at most an element and its index")
                    .primary(span, "write `for x, i in xs`"),
            );
        }
        let counter = self.new_local(None, int);
        self.emit(Stmt::Let { local: counter, init: Some(ir::Expr::new(ExprKind::Int(0), int)) });
        let counter_e = ir::Expr::new(ExprKind::Local(counter), int);
        let len = self.len_of(base.clone());
        let done = ir::Expr::new(
            ExprKind::Binary { op: ir::BinaryOp::Ge, lhs: Box::new(counter_e.clone()), rhs: Box::new(len), span },
            bool_ty,
        );
        let item = ir::Expr::new(
            ExprKind::Index { base: Box::new(base), index: Box::new(counter_e.clone()), checked: false, span },
            elem,
        );
        self.counted_loop(
            f,
            counter_e,
            done,
            |this| {
                if let Some(b) = f.bindings.first() {
                    if b.by_ref {
                        let ptr_ty = this.types.pointer(elem);
                        let local = this.declare_var(b.name.name, elem, b.name.span, false);
                        this.body.locals[local.0 as usize].ty = ptr_ty;
                        this.set_indirect(local);
                        let addr = ir::Expr::new(ExprKind::AddrOf(Box::new(item.clone())), ptr_ty);
                        this.emit(Stmt::Let { local, init: Some(addr) });
                    } else {
                        let local = this.declare_var(b.name.name, elem, b.name.span, false);
                        this.emit(Stmt::Let { local, init: Some(item.clone()) });
                    }
                }
                if let Some(b) = f.bindings.get(1) {
                    let local = this.declare_var(b.name.name, int, b.name.span, false);
                    let c = ir::Expr::new(ExprKind::Local(counter), int);
                    this.emit(Stmt::Let { local, init: Some(c) });
                }
            },
            None,
        );
    }

    fn for_string(&mut self, f: &ast::ForExpr, iter: ir::Expr, span: Span) {
        let int = self.types.int();
        let bool_ty = self.types.bool();
        let rune = self.types.rune();
        let s = self.spill(iter);
        let offset = self.new_local(None, int);
        self.emit(Stmt::Let { local: offset, init: Some(ir::Expr::new(ExprKind::Int(0), int)) });
        let offset_e = ir::Expr::new(ExprKind::Local(offset), int);
        let width = self.new_local(None, int);
        self.emit(Stmt::Let { local: width, init: Some(ir::Expr::new(ExprKind::Int(0), int)) });
        let width_e = ir::Expr::new(ExprKind::Local(width), int);
        let len = self.len_of(s.clone());
        let done = ir::Expr::new(
            ExprKind::Binary { op: ir::BinaryOp::Ge, lhs: Box::new(offset_e.clone()), rhs: Box::new(len), span },
            bool_ty,
        );
        if f.bindings.first().is_some_and(|b| b.by_ref) {
            let name_span = f.bindings[0].name.span;
            let amp = Span { start: name_span.start.saturating_sub(1), end: name_span.start, ..name_span };
            self.report(
                Diagnostic::error(codes::BY_REF_NOT_PLACE, "string characters cannot be bound by reference")
                    .primary(name_span, "strings are immutable")
                    .suggest_replace(
                        "bind each character by value",
                        amp,
                        "",
                        wid_diagnostics::Applicability::MachineApplicable,
                    ),
            );
        }
        let step = Some(width_e.clone());
        self.counted_loop(
            f,
            offset_e.clone(),
            done,
            |this| {
                let r = this.new_local(None, rune);
                this.emit(Stmt::Let { local: r, init: Some(ir::Expr::new(ExprKind::Int(0), rune)) });
                let rune_ptr = this.types.pointer(rune);
                let r_addr =
                    ir::Expr::new(ExprKind::AddrOf(Box::new(ir::Expr::new(ExprKind::Local(r), rune))), rune_ptr);
                let decode = ir::Expr::new(
                    ExprKind::Builtin {
                        op: Builtin::Utf8Decode,
                        args: vec![s.clone(), offset_e.clone(), r_addr],
                        span,
                    },
                    int,
                );
                this.emit(Stmt::Assign { target: width_e.clone(), value: decode });
                if let Some(b) = f.bindings.first() {
                    let local = this.declare_var(b.name.name, rune, b.name.span, false);
                    this.emit(Stmt::Let { local, init: Some(ir::Expr::new(ExprKind::Local(r), rune)) });
                }
                if let Some(b) = f.bindings.get(1) {
                    let local = this.declare_var(b.name.name, int, b.name.span, false);
                    this.emit(Stmt::Let { local, init: Some(offset_e.clone()) });
                }
            },
            step,
        );
    }

    fn for_map(&mut self, f: &ast::ForExpr, iter: ir::Expr, k: TyId, v: TyId, span: Span) {
        let int = self.types.int();
        let bool_ty = self.types.bool();
        let m = if is_place(&iter) { iter } else { self.spill(iter) };
        let ptr = self.address_of(m);
        let ptr = self.spill(ptr);
        let slot = self.new_local(None, int);
        let next = |this: &mut Self, from: ir::Expr| {
            ir::Expr::new(
                ExprKind::Builtin { op: Builtin::MapNext, args: vec![ptr.clone(), from], span },
                this.types.int(),
            )
        };
        let start = next(self, ir::Expr::new(ExprKind::Int(0), int));
        self.emit(Stmt::Let { local: slot, init: Some(start) });
        let slot_e = ir::Expr::new(ExprKind::Local(slot), int);
        let zero = ir::Expr::new(ExprKind::Int(0), int);
        let done = ir::Expr::new(
            ExprKind::Binary { op: ir::BinaryOp::Lt, lhs: Box::new(slot_e.clone()), rhs: Box::new(zero), span },
            bool_ty,
        );
        let one = ir::Expr::new(ExprKind::Int(1), int);
        let after = ir::Expr::new(
            ExprKind::Binary { op: ir::BinaryOp::Add, lhs: Box::new(slot_e.clone()), rhs: Box::new(one), span },
            int,
        );
        let advance = next(self, after);
        if f.bindings.iter().any(|b| b.by_ref) {
            let edits: Vec<wid_diagnostics::Edit> = f
                .bindings
                .iter()
                .filter(|b| b.by_ref)
                .map(|b| wid_diagnostics::Edit {
                    span: Span { start: b.name.span.start.saturating_sub(1), end: b.name.span.start, ..b.name.span },
                    replacement: String::new(),
                })
                .collect();
            self.report(
                Diagnostic::error(codes::BY_REF_NOT_PLACE, "map entries are bound by value")
                    .primary(span, "a map's keys and values cannot be bound by reference")
                    .note("to change a value, assign through the key: `m[key] = value`")
                    .suggest("bind them by value", edits, wid_diagnostics::Applicability::MachineApplicable),
            );
        }
        self.custom_loop(f, done, Stmt::Assign { target: slot_e.clone(), value: advance }, |this| {
            if let Some(b) = f.bindings.first() {
                let kp = this.types.pointer(k);
                let key_ptr = ir::Expr::new(
                    ExprKind::Builtin { op: Builtin::MapKeyAt, args: vec![ptr.clone(), slot_e.clone()], span },
                    kp,
                );
                let local = this.declare_var(b.name.name, k, b.name.span, false);
                this.emit(Stmt::Let { local, init: Some(ir::Expr::new(ExprKind::Deref(Box::new(key_ptr)), k)) });
            }
            if let Some(b) = f.bindings.get(1) {
                let vp = this.types.pointer(v);
                let val_ptr = ir::Expr::new(
                    ExprKind::Builtin { op: Builtin::MapValueAt, args: vec![ptr.clone(), slot_e.clone()], span },
                    vp,
                );
                let local = this.declare_var(b.name.name, v, b.name.span, false);
                this.emit(Stmt::Let { local, init: Some(ir::Expr::new(ExprKind::Deref(Box::new(val_ptr)), v)) });
            }
        });
    }

    /// Builds `loop { if done break; { bindings; body } next: counter += step }`.
    fn counted_loop(
        &mut self,
        f: &ast::ForExpr,
        counter: ir::Expr,
        done: ir::Expr,
        bind: impl FnOnce(&mut Self),
        step: Option<ir::Expr>,
    ) {
        let int = self.types.int();
        let step = step.unwrap_or_else(|| ir::Expr::new(ExprKind::Int(1), int));
        let advance = ir::Expr::new(
            ExprKind::Binary {
                op: ir::BinaryOp::Add,
                lhs: Box::new(counter.clone()),
                rhs: Box::new(step),
                span: f.iter.span,
            },
            int,
        );
        self.custom_loop(f, done, Stmt::Assign { target: counter, value: advance }, bind);
    }

    fn custom_loop(&mut self, f: &ast::ForExpr, done: ir::Expr, advance: Stmt, bind: impl FnOnce(&mut Self)) {
        let break_label = self.new_label();
        let next_label = self.new_label();
        let loop_continue = self.new_label();
        self.begin_block();
        self.body.exits.push(Exit::Loop { break_label, continue_label: next_label });
        self.push_scope();
        bind(self);
        self.lower_stmts(&f.body, super::body::Dest::Discard);
        self.pop_scope();
        self.body.exits.pop();
        let body = self.end_block();
        let stmts = vec![
            Stmt::If {
                cond: done,
                then: ir::Block { stmts: vec![Stmt::Goto(break_label)] },
                else_: ir::Block::default(),
            },
            Stmt::Labeled { body, end_label: next_label },
            advance,
        ];
        self.emit(Stmt::Loop { body: ir::Block { stmts }, continue_label: loop_continue, break_label });
    }

    /// Marks a local as holding a pointer that reads and writes go through.
    pub fn set_indirect(&mut self, local: LocalId) {
        if let Some(var) = self
            .body
            .frames
            .last_mut()
            .and_then(|f| f.scopes.iter_mut().rev().find_map(|s| s.vars.iter_mut().find(|v| v.local == local)))
        {
            var.indirect = true;
        }
    }

    /// Converts arrays and dynamic arrays to slices where a slice is expected.
    pub fn slice_of_container(&mut self, v: ir::Expr, slice_ty: TyId) -> ir::Expr {
        let base = if is_place(&v) { v } else { self.spill(v) };
        self.full_slice(base, slice_ty)
    }
}

/// Returns true when evaluating a place twice reads the same location
/// without repeating side effects (bounds checks may repeat).
fn stable_place(e: &ir::Expr) -> bool {
    match &e.kind {
        ExprKind::Local(_) | ExprKind::Global(_) | ExprKind::Context => true,
        ExprKind::Field { base, .. } | ExprKind::OptGet(base) | ExprKind::UnionGet { value: base, .. } => {
            stable_place(base)
        }
        ExprKind::Deref(inner) => stable_place(inner) || inner.is_pure(),
        ExprKind::Index { base, index, .. } => stable_place(base) && (index.is_pure() || stable_place(index)),
        _ => false,
    }
}

fn is_swizzle(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 4
        && (name.chars().all(|c| "xyzw".contains(c)) || name.chars().all(|c| "rgba".contains(c)))
}

fn swizzle_index(c: char) -> u64 {
    match c {
        'x' | 'r' => 0,
        'y' | 'g' => 1,
        'z' | 'b' => 2,
        _ => 3,
    }
}

//! `matrix[R, C]T`: small numeric matrices stored column-major, written
//! row by row in literals, with `m[row, col]`, element-wise `+ -`, scaling,
//! and matrix-matrix and matrix-vector products.

use wid_diagnostics::{Diagnostic, Span, codes};
use wid_syntax::ast::{self, ExprKind as E};

use super::Checker;
use super::items::ConstValue;
use super::ty::TyCtx;
use crate::ir::{self, Builtin, ExprKind};
use crate::types::{TyId, TyKind};

/// The largest number of rows or columns a matrix may have.
const MAX_DIM: i128 = 16;

impl Checker<'_> {
    /// Resolves `matrix[rows, cols]elem`.
    pub(super) fn resolve_matrix(
        &mut self,
        rows: &ast::Expr,
        cols: &ast::Expr,
        elem: &ast::TypeExpr,
        ctx: &TyCtx,
    ) -> TyId {
        let elem_ty = self.resolve_type(elem, ctx);
        let dim = |this: &mut Self, e: &ast::Expr, what: &str| -> Option<u32> {
            match this.eval_const(e, ctx.loc) {
                Some(ConstValue::Int(n)) if (1..=MAX_DIM).contains(&n) => Some(n as u32),
                Some(ConstValue::Int(n)) => {
                    this.report(
                        Diagnostic::error(
                            codes::TYPE_MISMATCH,
                            format!("a matrix needs 1 to {MAX_DIM} {what}, not {n}"),
                        )
                        .primary(e.span, "out of range"),
                    );
                    None
                }
                _ => {
                    this.report(
                        Diagnostic::error(
                            codes::COMPTIME_ONLY,
                            format!("matrix {what} must be a compile-time integer"),
                        )
                        .primary(e.span, "not a constant integer")
                        .help("write it like `matrix[4, 4]F32`"),
                    );
                    None
                }
            }
        };
        let r = dim(self, rows, "rows");
        let c = dim(self, cols, "columns");
        if matches!(self.types.kind(elem_ty), TyKind::Unknown) {
            return elem_ty;
        }
        if !self.types.is_numeric(elem_ty) {
            let shown = self.types.display(elem_ty);
            self.report(
                Diagnostic::error(codes::TYPE_MISMATCH, format!("matrix elements must be numbers, not `{shown}`"))
                    .primary(elem.span, "not a number type")
                    .help("use an integer or float type, like `matrix[2, 2]F32`"),
            );
            return self.types.unknown();
        }
        match (r, c) {
            (Some(r), Some(c)) => self.types.intern(TyKind::Matrix(elem_ty, r, c)),
            _ => self.types.unknown(),
        }
    }

    /// Lowers a literal for `matrix[R, C]T`: `R * C` elements written row by
    /// row, stored column by column.
    pub(super) fn matrix_literal(&mut self, elems: &[ast::Expr], ty: TyId, span: Span) -> ir::Expr {
        let TyKind::Matrix(elem, r, c) = *self.types.kind(self.types.base(ty)) else {
            unreachable!("matrix_literal on a non-matrix")
        };
        let (r, c) = (r as usize, c as usize);
        if elems.len() != r * c {
            let shown = self.types.display(ty);
            self.report(
                Diagnostic::error(
                    codes::TYPE_MISMATCH,
                    format!("expected {} elements for `{shown}`, found {}", r * c, elems.len()),
                )
                .primary(span, format!("this literal has {} elements", elems.len()))
                .note(format!("write the {r} rows of {c} elements one after another")),
            );
            return ir::Expr::new(ExprKind::Zero, ty);
        }
        let written = self.lower_elems(elems, elem);
        let mut stored = vec![ir::Expr::new(ExprKind::Zero, elem); r * c];
        for (i, v) in written.into_iter().enumerate() {
            let (row, col) = (i / c, i % c);
            stored[col * r + row] = v;
        }
        ir::Expr::new(ExprKind::Aggregate(stored), ty)
    }

    /// Lowers `m[row, col]` as a place.
    pub(super) fn matrix_index(&mut self, m: ir::Expr, args: &[ast::Expr], span: Span) -> ir::Expr {
        let TyKind::Matrix(elem, r, c) = *self.types.kind(self.types.base(m.ty)) else {
            unreachable!("matrix_index on a non-matrix")
        };
        let [row, col] = args else {
            let shown = self.types.display(m.ty);
            self.report(
                Diagnostic::error(
                    codes::ARG_COUNT,
                    format!("{} `{shown}` is indexed by row and column", wid_diagnostics::a_or_an(&shown)),
                )
                .primary(
                    span,
                    if args.len() == 1 {
                        "found one index".to_string()
                    } else {
                        format!("found {} indices", args.len())
                    },
                )
                .help("write `m[row, col]`; `m.row(i)` and `m.column(j)` copy out a whole row or column"),
            );
            return ir::Expr::new(ExprKind::Zero, self.types.unknown());
        };
        let row = self.int_value(row);
        let row = self.stable(row);
        let col = self.int_value(col);
        let col = self.stable(col);
        let flat = self.flat_index(row, col, r, c, span);
        ir::Expr::new(ExprKind::Index { base: Box::new(m), index: Box::new(flat), checked: false, span }, elem)
    }

    /// `col * rows + row`, with both checked against the matrix shape when
    /// bounds checks are on.
    fn flat_index(&mut self, row: ir::Expr, col: ir::Expr, r: u32, c: u32, span: Span) -> ir::Expr {
        let int = self.types.int();
        let checked = self.input.options.bounds_checks && !self.no_bounds_check;
        let check = |i: ir::Expr, n: u32| {
            if checked {
                let len = ir::Expr::new(ExprKind::Int(i128::from(n)), int);
                ir::Expr::new(ExprKind::Builtin { op: Builtin::Bounds, args: vec![i, len], span }, int)
            } else {
                i
            }
        };
        let row = check(row, r);
        let col = check(col, c);
        let rows = ir::Expr::new(ExprKind::Int(i128::from(r)), int);
        let scaled = ir::Expr::new(
            ExprKind::Binary { op: ir::BinaryOp::Mul, lhs: Box::new(col), rhs: Box::new(rows), span },
            int,
        );
        ir::Expr::new(ExprKind::Binary { op: ir::BinaryOp::Add, lhs: Box::new(scaled), rhs: Box::new(row), span }, int)
    }

    /// The type of `l * r` when one side is a matrix and the other a matrix
    /// or vector, reporting shapes that do not fit. `None` means the operands
    /// are not a product of that kind.
    pub(super) fn matrix_product(&mut self, l: TyId, r: TyId, span: Span) -> Option<TyId> {
        let lk = self.types.kind(self.types.base(l)).clone();
        let rk = self.types.kind(self.types.base(r)).clone();
        let (shape, fits) = match (lk, rk) {
            (TyKind::Matrix(e, rows, k), TyKind::Matrix(e2, k2, cols)) if e == e2 => {
                (TyKind::Matrix(e, rows, cols), k == k2)
            }
            (TyKind::Matrix(e, rows, cols), TyKind::Array(e2, n)) if e == e2 => {
                (TyKind::Array(e, u64::from(rows)), u64::from(cols) == n)
            }
            (TyKind::Array(e, n), TyKind::Matrix(e2, rows, cols)) if e == e2 => {
                (TyKind::Array(e, u64::from(cols)), u64::from(rows) == n)
            }
            _ => return None,
        };
        if !fits {
            let (ls, rs) = (self.types.display(l), self.types.display(r));
            self.report(
                Diagnostic::error(codes::NO_OPERATOR, format!("cannot multiply `{ls}` by `{rs}`"))
                    .primary(span, "the shapes do not line up")
                    .note("in `a * b` the columns of `a` must match the rows of `b`; a vector on the right is a column, on the left a row")
                    .help("check the order of the operands, or transpose one with `.transpose`"),
            );
            return Some(self.types.unknown());
        }
        Some(self.types.intern(shape))
    }

    /// `m.transpose`, `m.row(i)` and `m.column(j)`.
    pub(super) fn matrix_method(
        &mut self,
        v: &ir::Expr,
        name: ast::Ident,
        args: &[ast::Arg],
        span: Span,
    ) -> Option<ir::Expr> {
        let TyKind::Matrix(elem, r, c) = *self.types.kind(self.types.base(v.ty)) else { return None };
        let m = self.stable(v.clone());
        let int = self.types.int();
        let read = |index: ir::Expr| {
            ir::Expr::new(
                ExprKind::Index { base: Box::new(m.clone()), index: Box::new(index), checked: false, span },
                elem,
            )
        };
        match name.as_str() {
            "transpose" => {
                let ty = self.types.intern(TyKind::Matrix(elem, c, r));
                let mut stored = vec![ir::Expr::new(ExprKind::Zero, elem); (r * c) as usize];
                for col in 0..r {
                    for row in 0..c {
                        let src = ir::Expr::new(ExprKind::Int(i128::from(row * r + col)), int);
                        stored[(col * c + row) as usize] = read(src);
                    }
                }
                Some(ir::Expr::new(ExprKind::Aggregate(stored), ty))
            }
            "row" | "column" => {
                let [arg] = args else {
                    self.report(
                        Diagnostic::error(codes::ARG_COUNT, format!("`{}` takes one index", name.as_str()))
                            .primary(span, format!("like `m.{}(0)`", name.as_str())),
                    );
                    return Some(ir::Expr::new(ExprKind::Zero, self.types.unknown()));
                };
                let i = self.int_value(&arg.value);
                let i = self.stable(i);
                let is_row = name.as_str() == "row";
                let n = if is_row { c } else { r };
                let ty = self.types.intern(TyKind::Array(elem, u64::from(n)));
                let mut out = Vec::new();
                for k in 0..n {
                    let k_expr = ir::Expr::new(ExprKind::Int(i128::from(k)), int);
                    let flat = if is_row {
                        self.flat_index(i.clone(), k_expr, r, c, span)
                    } else {
                        self.flat_index(k_expr, i.clone(), r, c, span)
                    };
                    out.push(read(flat));
                }
                Some(ir::Expr::new(ExprKind::Aggregate(out), ty))
            }
            _ => None,
        }
    }

    /// `matrix[N, N]T.identity`.
    pub(super) fn matrix_identity(&mut self, ty: TyId, span: Span) -> ir::Expr {
        let TyKind::Matrix(elem, r, c) = *self.types.kind(self.types.base(ty)) else {
            unreachable!("matrix_identity on a non-matrix")
        };
        if r != c {
            let shown = self.types.display(ty);
            self.report(
                Diagnostic::error(codes::NO_SUCH_MEMBER, format!("`{shown}` has no identity"))
                    .primary(span, "only square matrices have one"),
            );
            return ir::Expr::new(ExprKind::Zero, ty);
        }
        let one = if self.types.is_float(elem) { ExprKind::Float(1.0) } else { ExprKind::Int(1) };
        let values =
            (0..r * c)
                .map(|i| {
                    if i % (r + 1) == 0 {
                        ir::Expr::new(one.clone(), elem)
                    } else {
                        ir::Expr::new(ExprKind::Zero, elem)
                    }
                })
                .collect();
        ir::Expr::new(ExprKind::Aggregate(values), ty)
    }
}

/// Returns true when an expression is an array literal of `n` elements.
pub(super) fn is_literal_of_len(e: &ast::Expr, n: u32) -> bool {
    matches!(&e.kind, E::Array(elems) if elems.len() == n as usize)
}

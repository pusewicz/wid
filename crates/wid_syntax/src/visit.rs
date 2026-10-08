//! Walks over the syntax tree.
//!
//! [`VisitMut`] walks a tree it may change, [`Visit`] one it only reads.
//! Both have one method per kind of node. Each default method calls the
//! matching `walk_*` function (for [`Visit`], the one in [`shared`]),
//! which visits the node's children, so an implementation overrides only
//! the nodes it handles and calls `walk_*` to keep descending. Every
//! [`Span`](wid_diagnostics::Span) in the tree is passed to
//! [`VisitMut::visit_span`].
//!
//! Lists where the grammar takes any number of elements have their own
//! method ([`VisitMut::visit_stmts`], [`VisitMut::visit_items`],
//! [`VisitMut::visit_args`] and [`VisitMut::visit_exprs`]), so a visitor can
//! replace one element with several. Macro expansion uses this to clone a
//! `quote` into generated code.

pub use mutable::*;
pub use shared::Visit;

/// Defines a visitor trait and its `walk_*` functions. With `mut`, they
/// take `&mut` nodes; without it, `&` nodes.
macro_rules! visitor {
    ($doc:literal, $Visitor:ident $(, $mut:ident)?) => {
        use wid_diagnostics::Span;

        use crate::ast::*;

        #[doc = $doc]
        pub trait $Visitor {
            /// Every span of the tree.
            fn visit_span(&mut self, _span: & $($mut)? Span) {}

            /// A name with its span: declared names, member names, callees,
            /// parameters and bindings.
            fn visit_ident(&mut self, ident: & $($mut)? Ident) {
                walk_ident(self, ident);
            }

            /// A statement list: a body or a block.
            fn visit_stmts(&mut self, stmts: & $($mut)? Vec<Stmt>) {
                walk_stmts(self, stmts);
            }

            /// One statement.
            fn visit_stmt(&mut self, stmt: & $($mut)? Stmt) {
                walk_stmt(self, stmt);
            }

            /// A declaration list: a file, a type body or a `comptime if` branch.
            fn visit_items(&mut self, items: & $($mut)? Vec<Item>) {
                walk_items(self, items);
            }

            /// One declaration.
            fn visit_item(&mut self, item: & $($mut)? Item) {
                walk_item(self, item);
            }

            /// One expression.
            fn visit_expr(&mut self, expr: & $($mut)? Expr) {
                walk_expr(self, expr);
            }

            /// A comma-separated list of expressions: array elements, returned and
            /// yielded values, index arguments, `when` patterns, assignment targets
            /// and values, and attribute arguments.
            fn visit_exprs(&mut self, exprs: & $($mut)? Vec<Expr>) {
                walk_exprs(self, exprs);
            }

            /// The arguments of a call.
            fn visit_args(&mut self, args: & $($mut)? Vec<Arg>) {
                walk_args(self, args);
            }

            /// A type expression.
            fn visit_type(&mut self, ty: & $($mut)? TypeExpr) {
                walk_type(self, ty);
            }

            /// A `quote do … end` with its splices.
            fn visit_quote(&mut self, quote: & $($mut)? QuoteExpr) {
                walk_quote(self, quote);
            }
        }

        /// Visits an identifier's span.
        pub fn walk_ident<V: $Visitor + ?Sized>(v: &mut V, ident: & $($mut)? Ident) {
            v.visit_span(& $($mut)? ident.span);
        }

        /// Visits each statement of a list.
        pub fn walk_stmts<V: $Visitor + ?Sized>(v: &mut V, stmts: & $($mut)? Vec<Stmt>) {
            for stmt in stmts {
                v.visit_stmt(stmt);
            }
        }

        /// Visits each declaration of a list.
        pub fn walk_items<V: $Visitor + ?Sized>(v: &mut V, items: & $($mut)? Vec<Item>) {
            for item in items {
                v.visit_item(item);
            }
        }

        /// Visits each expression of a list.
        pub fn walk_exprs<V: $Visitor + ?Sized>(v: &mut V, exprs: & $($mut)? Vec<Expr>) {
            for e in exprs {
                v.visit_expr(e);
            }
        }

        /// Visits each argument of a call: its name and its value.
        pub fn walk_args<V: $Visitor + ?Sized>(v: &mut V, args: & $($mut)? Vec<Arg>) {
            for arg in args {
                if let Some(name) = & $($mut)? arg.name {
                    v.visit_ident(name);
                }
                v.visit_expr(& $($mut)? arg.value);
            }
        }

        fn walk_attrs<V: $Visitor + ?Sized>(v: &mut V, attrs: & $($mut)? [Attribute]) {
            for attr in attrs {
                v.visit_ident(& $($mut)? attr.name);
                v.visit_exprs(& $($mut)? attr.args);
                v.visit_span(& $($mut)? attr.span);
            }
        }

        /// Visits a statement's attributes, its children and its span.
        pub fn walk_stmt<V: $Visitor + ?Sized>(v: &mut V, stmt: & $($mut)? Stmt) {
            walk_attrs(v, & $($mut)? stmt.attrs);
            match & $($mut)? stmt.kind {
                StmtKind::Expr(e) => v.visit_expr(e),
                StmtKind::Decl { names, ty, value, .. } => {
                    for name in names {
                        v.visit_ident(name);
                    }
                    v.visit_type(ty);
                    if let Some(value) = value {
                        v.visit_expr(value);
                    }
                }
                StmtKind::Assign { targets, values, .. } => {
                    v.visit_exprs(targets);
                    v.visit_exprs(values);
                }
                StmtKind::Return(values) => v.visit_exprs(values),
                StmtKind::Break(value) | StmtKind::Next(value) => {
                    if let Some(value) = value {
                        v.visit_expr(value);
                    }
                }
                StmtKind::Defer(body) => v.visit_stmts(body),
                StmtKind::Guard { names, value, err, else_body } => {
                    for name in names {
                        v.visit_ident(name);
                    }
                    v.visit_expr(value);
                    if let Some(err) = err {
                        v.visit_ident(err);
                    }
                    v.visit_stmts(else_body);
                }
                StmtKind::Item(item) => v.visit_item(item),
                StmtKind::Error => {}
            }
            v.visit_span(& $($mut)? stmt.span);
        }

        fn walk_params<V: $Visitor + ?Sized>(v: &mut V, params: & $($mut)? [Param]) {
            for p in params {
                v.visit_ident(& $($mut)? p.name);
                v.visit_type(& $($mut)? p.ty);
                if let Some(default) = & $($mut)? p.default {
                    v.visit_expr(default);
                }
                v.visit_span(& $($mut)? p.span);
            }
        }

        fn walk_generics<V: $Visitor + ?Sized>(v: &mut V, generics: & $($mut)? [GenericParam]) {
            for g in generics {
                v.visit_ident(& $($mut)? g.name);
                if let Some(ty) = & $($mut)? g.ty {
                    v.visit_type(ty);
                }
                v.visit_span(& $($mut)? g.span);
            }
        }

        /// Visits a declaration's attributes, its children and its span.
        pub fn walk_item<V: $Visitor + ?Sized>(v: &mut V, item: & $($mut)? Item) {
            walk_attrs(v, & $($mut)? item.attrs);
            match & $($mut)? item.kind {
                ItemKind::Import(import) => {
                    v.visit_span(& $($mut)? import.path_span);
                    if let Some(alias) = & $($mut)? import.alias {
                        v.visit_ident(alias);
                    }
                }
                ItemKind::Cimport(c) => {
                    v.visit_span(& $($mut)? c.header_span);
                    for option in & $($mut)? c.options {
                        v.visit_ident(& $($mut)? option.name);
                        match & $($mut)? option.value {
                            CimportValue::Expr(e) => v.visit_expr(e),
                            CimportValue::Hash { entries, span } => {
                                for entry in entries {
                                    v.visit_span(& $($mut)? entry.key_span);
                                    v.visit_expr(& $($mut)? entry.value);
                                }
                                v.visit_span(span);
                            }
                        }
                        v.visit_span(& $($mut)? option.span);
                    }
                }
                ItemKind::Def(f) => {
                    v.visit_ident(& $($mut)? f.name);
                    walk_params(v, & $($mut)? f.params);
                    if let Some(block) = & $($mut)? f.block {
                        v.visit_ident(& $($mut)? block.name);
                        v.visit_type(& $($mut)? block.ty);
                        v.visit_span(& $($mut)? block.span);
                    }
                    if let Some(ret) = & $($mut)? f.ret {
                        v.visit_type(ret);
                    }
                    match & $($mut)? f.body {
                        FnBody::Block(stmts) => v.visit_stmts(stmts),
                        FnBody::Expr(e) => v.visit_expr(e),
                    }
                    v.visit_span(& $($mut)? f.sig_span);
                    if let Some(span) = & $($mut)? f.c_variadic {
                        v.visit_span(span);
                    }
                }
                ItemKind::Struct(s) => {
                    v.visit_ident(& $($mut)? s.name);
                    walk_generics(v, & $($mut)? s.generics);
                    v.visit_items(& $($mut)? s.body);
                }
                ItemKind::Enum(e) => {
                    v.visit_ident(& $($mut)? e.name);
                    if let Some(backing) = & $($mut)? e.backing {
                        v.visit_type(backing);
                    }
                    for member in & $($mut)? e.members {
                        v.visit_ident(& $($mut)? member.name);
                        if let Some(value) = & $($mut)? member.value {
                            v.visit_expr(value);
                        }
                    }
                    v.visit_items(& $($mut)? e.body);
                }
                ItemKind::Union(u) => {
                    v.visit_ident(& $($mut)? u.name);
                    walk_generics(v, & $($mut)? u.generics);
                    for variant in & $($mut)? u.variants {
                        v.visit_type(variant);
                    }
                }
                ItemKind::Module(m) => {
                    v.visit_ident(& $($mut)? m.name);
                    v.visit_items(& $($mut)? m.body);
                }
                ItemKind::Extend(e) => {
                    for target in & $($mut)? e.targets {
                        v.visit_type(target);
                    }
                    v.visit_items(& $($mut)? e.body);
                }
                ItemKind::Const(c) => {
                    v.visit_ident(& $($mut)? c.name);
                    if let Some(ty) = & $($mut)? c.ty {
                        v.visit_type(ty);
                    }
                    v.visit_expr(& $($mut)? c.value);
                }
                ItemKind::Overload(o) => {
                    v.visit_ident(& $($mut)? o.name);
                    for member in & $($mut)? o.members {
                        v.visit_ident(member);
                    }
                }
                ItemKind::Include(t) => v.visit_type(t),
                ItemKind::Field(f) => {
                    v.visit_ident(& $($mut)? f.name);
                    v.visit_type(& $($mut)? f.ty);
                    if let Some(default) = & $($mut)? f.default {
                        v.visit_expr(default);
                    }
                }
                ItemKind::MacroCall(e) => v.visit_expr(e),
                ItemKind::ComptimeIf(c) => {
                    v.visit_expr(& $($mut)? c.cond);
                    v.visit_items(& $($mut)? c.then);
                    v.visit_items(& $($mut)? c.else_);
                }
                ItemKind::Splice(_) | ItemKind::Error => {}
            }
            v.visit_span(& $($mut)? item.span);
        }

        fn walk_cond<V: $Visitor + ?Sized>(v: &mut V, cond: & $($mut)? Cond) {
            match cond {
                Cond::Expr(e) => v.visit_expr(e),
                Cond::Bind { name, value } => {
                    v.visit_ident(name);
                    v.visit_expr(value);
                }
            }
        }

        fn walk_if<V: $Visitor + ?Sized>(v: &mut V, if_expr: & $($mut)? IfExpr) {
            walk_cond(v, & $($mut)? if_expr.cond);
            v.visit_stmts(& $($mut)? if_expr.then);
            for (cond, body) in & $($mut)? if_expr.elifs {
                walk_cond(v, cond);
                v.visit_stmts(body);
            }
            if let Some(body) = & $($mut)? if_expr.else_ {
                v.visit_stmts(body);
            }
        }

        fn walk_block_params<V: $Visitor + ?Sized>(v: &mut V, params: & $($mut)? [BlockParam]) {
            for p in params {
                v.visit_ident(& $($mut)? p.name);
            }
        }

        /// Visits an expression's children and its span.
        pub fn walk_expr<V: $Visitor + ?Sized>(v: &mut V, expr: & $($mut)? Expr) {
            match & $($mut)? expr.kind {
                ExprKind::Int(_)
                | ExprKind::Float(_)
                | ExprKind::Symbol(_)
                | ExprKind::Nil
                | ExprKind::True
                | ExprKind::False
                | ExprKind::SelfRef
                | ExprKind::Zero
                | ExprKind::Uninit
                | ExprKind::Ident(_)
                | ExprKind::Const(_)
                | ExprKind::IVar(_)
                | ExprKind::Splice(_)
                | ExprKind::Error => {}
                ExprKind::Str(parts) => {
                    for part in parts {
                        if let StrPart::Interp(e) = part {
                            v.visit_expr(e);
                        }
                    }
                }
                ExprKind::Array(elems) => v.visit_exprs(elems),
                ExprKind::Type(t) => v.visit_type(t),
                ExprKind::Member { recv, name, .. } => {
                    v.visit_expr(recv);
                    v.visit_ident(name);
                }
                ExprKind::Call(call) => {
                    match & $($mut)? call.callee {
                        Callee::Name(name) | Callee::IVar(name) => v.visit_ident(name),
                        Callee::Method { recv, name, .. } => {
                            v.visit_expr(recv);
                            v.visit_ident(name);
                        }
                    }
                    v.visit_args(& $($mut)? call.args);
                    if let Some(block) = & $($mut)? call.block {
                        walk_block_params(v, & $($mut)? block.params);
                        v.visit_stmts(& $($mut)? block.body);
                        v.visit_span(& $($mut)? block.span);
                    }
                }
                ExprKind::Index { recv, args } => {
                    v.visit_expr(recv);
                    v.visit_exprs(args);
                }
                ExprKind::Unary { expr: inner, .. }
                | ExprKind::Deref(inner)
                | ExprKind::AddrOf(inner)
                | ExprKind::Paren(inner) => v.visit_expr(inner),
                ExprKind::Binary { lhs, rhs, .. } => {
                    v.visit_expr(lhs);
                    v.visit_expr(rhs);
                }
                ExprKind::Ternary { cond, then, else_ } => {
                    v.visit_expr(cond);
                    v.visit_expr(then);
                    v.visit_expr(else_);
                }
                ExprKind::Range { lo, hi, .. } => {
                    if let Some(lo) = lo {
                        v.visit_expr(lo);
                    }
                    if let Some(hi) = hi {
                        v.visit_expr(hi);
                    }
                }
                ExprKind::If(if_expr) | ExprKind::ComptimeIf(if_expr) => walk_if(v, if_expr),
                ExprKind::While { cond, body, .. } => {
                    walk_cond(v, cond);
                    v.visit_stmts(body);
                }
                ExprKind::For(f) => {
                    walk_block_params(v, & $($mut)? f.bindings);
                    v.visit_expr(& $($mut)? f.iter);
                    v.visit_stmts(& $($mut)? f.body);
                }
                ExprKind::Loop(body) | ExprKind::Comptime(body) => v.visit_stmts(body),
                ExprKind::Case(case) => {
                    if let Some(subject) = & $($mut)? case.subject {
                        v.visit_expr(subject);
                    }
                    for when in & $($mut)? case.whens {
                        v.visit_exprs(& $($mut)? when.patterns);
                        v.visit_stmts(& $($mut)? when.body);
                        v.visit_span(& $($mut)? when.span);
                    }
                    if let Some(body) = & $($mut)? case.else_ {
                        v.visit_stmts(body);
                    }
                }
                ExprKind::Lambda(lambda) => {
                    walk_params(v, & $($mut)? lambda.params);
                    if let Some(ret) = & $($mut)? lambda.ret {
                        v.visit_type(ret);
                    }
                    v.visit_stmts(& $($mut)? lambda.body);
                }
                ExprKind::Yield(args) => v.visit_exprs(args),
                ExprKind::Quote(quote) => v.visit_quote(quote),
            }
            v.visit_span(& $($mut)? expr.span);
        }

        /// Visits a type expression's children and its span.
        pub fn walk_type<V: $Visitor + ?Sized>(v: &mut V, ty: & $($mut)? TypeExpr) {
            match & $($mut)? ty.kind {
                TypeKind::Path { segments, args } => {
                    for segment in segments {
                        v.visit_ident(segment);
                    }
                    for arg in args {
                        match arg {
                            GenericArg::Type(t) => v.visit_type(t),
                            GenericArg::Expr(e) => v.visit_expr(e),
                        }
                    }
                }
                TypeKind::Param(name) => v.visit_ident(name),
                TypeKind::Pointer(inner)
                | TypeKind::MultiPointer(inner)
                | TypeKind::Slice(inner)
                | TypeKind::Dynamic(inner)
                | TypeKind::Optional(inner)
                | TypeKind::Distinct(inner) => v.visit_type(inner),
                TypeKind::Array(len, elem) => {
                    v.visit_expr(len);
                    v.visit_type(elem);
                }
                TypeKind::Map(k, value) => {
                    v.visit_type(k);
                    v.visit_type(value);
                }
                TypeKind::Proc { params, ret, .. } | TypeKind::Block { params, ret } => {
                    for p in params {
                        v.visit_type(p);
                    }
                    if let Some(ret) = ret {
                        v.visit_type(ret);
                    }
                }
                TypeKind::Tuple(elems) => {
                    for e in elems {
                        v.visit_type(e);
                    }
                }
                TypeKind::Matrix { rows, cols, elem } => {
                    v.visit_expr(rows);
                    v.visit_expr(cols);
                    v.visit_type(elem);
                }
                TypeKind::Splice(_) | TypeKind::Spliced(_) | TypeKind::Error => {}
            }
            v.visit_span(& $($mut)? ty.span);
        }

        /// Visits a `quote`'s body and its splice expressions.
        pub fn walk_quote<V: $Visitor + ?Sized>(v: &mut V, quote: & $($mut)? QuoteExpr) {
            v.visit_stmts(& $($mut)? quote.body);
            v.visit_exprs(& $($mut)? quote.splices);
        }
    };
}

mod mutable {
    visitor!("A visitor over a syntax tree it may change. See the module documentation.", VisitMut, mut);
}

/// [`Visit`], a visitor over a syntax tree it only reads, and its `walk_*`
/// functions.
// Lists are `&Vec` to match `VisitMut`, where a visitor may grow them.
#[allow(clippy::ptr_arg)]
pub mod shared {
    visitor!("A visitor over a syntax tree it only reads. See the module documentation.", Visit);
}

#[cfg(test)]
mod tests {
    use wid_diagnostics::{FileId, Span};

    use super::VisitMut;
    use crate::ast::{ExprKind, ItemKind, StmtKind};
    use crate::parse_file;

    /// Moves every span to another file, the way macro expansion re-homes a
    /// cloned `quote`.
    struct Rehome(FileId, usize);

    impl VisitMut for Rehome {
        fn visit_span(&mut self, span: &mut Span) {
            span.file = self.0;
            self.1 += 1;
        }
    }

    #[test]
    fn every_span_is_visited() {
        let src = "macro def m(name: Symbol) -> Code\n  quote do\n    for x in [1, 2]\n      p x, #{name}\n    end\n    @#{name} = :#{name}\n  end\nend\n";
        let (mut file, diags) = parse_file(FileId(0), src);
        assert!(diags.is_empty(), "{diags:?}");
        let ItemKind::Def(f) = &mut file.items[0].kind else { panic!("a def") };
        let crate::ast::FnBody::Block(body) = &mut f.body else { panic!("a block body") };
        let StmtKind::Expr(e) = &mut body[0].kind else { panic!("an expression") };
        let ExprKind::Quote(q) = &mut e.kind else { panic!("a quote") };
        let mut v = Rehome(FileId(7), 0);
        v.visit_quote(q);
        assert!(v.1 > 15, "visited {} spans", v.1);
        let text = format!("{q:?}");
        assert!(!text.contains("FileId(0)"), "a span was missed: {text}");
    }
}

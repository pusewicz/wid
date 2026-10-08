//! Odin's `:=` and `or_return`, which Wid writes as `=` and `guard`: each is
//! one error with a fix, and the statement is read as what the fix writes.

use wid_diagnostics::{Applicability, Diagnostic, Edit, Span, codes};

use super::{Parser, T};
use crate::ast::{Expr, ExprKind, Ident, Stmt, StmtKind, TypeExpr, TypeKind};
use crate::intern::Name;

/// What a `return` in the method being parsed gives back, read from the
/// return type it declares.
enum Returns {
    /// No return type: the method can't pass an error on.
    Nothing,
    /// `T?`: it returns `nil` on failure.
    Optional,
    /// One value, the error itself (`-> Error`, `-> Failure`).
    Error,
    /// Several values, the error last (`-> (Level, Error)`).
    Values(usize),
}

impl Parser<'_> {
    /// Whether the statement starts with `name :=` or `a, b :=`.
    pub(super) fn at_walrus(&self) -> bool {
        let mut i = 0;
        loop {
            let Some(len) = self.name_len(i) else { return false };
            match self.nth(i + len).kind {
                T::Colon => {
                    let eq = self.nth(i + len + 1);
                    return eq.kind == T::Eq && !eq.space_before;
                }
                T::Comma => i += len + 1,
                _ => return false,
            }
        }
    }

    /// Whether the line ended after `at` and what starts the next line can't
    /// be the value after it: a declaration, an `end`, the end of the file,
    /// or a statement indented no deeper than the statement's first line
    /// (see [`Parser::statement_ahead`]).
    fn value_cut(&self, at: Span) -> bool {
        let next = self.peek();
        let line = self.line_of(next.span.start);
        if line == self.line_of(at.start) {
            return false;
        }
        super::starts_declaration(next.kind)
            || matches!(next.kind, T::Kw(super::K::End) | T::Eof)
            || (self.indent_of_line(line) <= self.indent_of_line(self.stmt_line) && self.statement_ahead())
    }

    /// Whether `or_return` is next.
    pub(super) fn at_or_return(&self) -> bool {
        self.at(T::Ident) && self.text_of(self.peek().span) == "or_return"
    }

    /// Parses `a, b := v`, Odin's and Go's declaration, as `a, b = v`: the
    /// first assignment declares a name in Wid. The error is left to
    /// `or_return` when one follows, whose fix rewrites the whole line.
    pub(super) fn parse_walrus(&mut self) -> StmtKind {
        let mut targets = Vec::new();
        loop {
            let name = self.parse_name("a variable name");
            targets.push(Expr { kind: ExprKind::Ident(name.name), span: name.span });
            if !self.eat(T::Comma) {
                break;
            }
        }
        let colon = self.bump().span;
        let walrus = colon.to(self.bump().span);
        // `x :=` at the end of its line, before a line that starts a
        // statement of its own: the value is missing too, which this one
        // error says, and the next line is parsed on its own.
        let missing = self.value_cut(walrus);
        let values = if missing {
            self.restore_line_end(walrus);
            vec![Expr { kind: ExprKind::Error, span: walrus.shrink_to_end() }]
        } else {
            self.skip_newlines();
            self.parse_expr_list(true)
        };
        if !self.at_or_return() {
            let mut diag = Diagnostic::error(codes::UNEXPECTED_TOKEN, "Wid writes `=` where Odin writes `:=`")
                .primary(walrus, "Odin's declaration operator")
                .note("the first assignment to a name declares it, as in `count = 0`");
            diag = if missing {
                diag.suggest_replace("write `=`", walrus, "=", Applicability::MaybeIncorrect)
                    .help("and write the value after it, on this line")
            } else {
                diag.suggest_replace("write `=`", walrus, "=", Applicability::MachineApplicable)
            };
            self.report(diag);
        }
        self.declare_targets(&targets);
        StmtKind::Assign { targets, op: None, values }
    }

    /// Reads `or_return` after the statement `kind`, which started at
    /// `start`: Odin's shorthand for passing an error on. It is one error,
    /// whose fix writes the `guard` that does it, and the statement is read
    /// as that `guard`, so the names it binds have the values' types.
    pub(super) fn or_return(&mut self, start: Span, kind: StmtKind) -> StmtKind {
        let keyword = self.bump().span;
        let diag = Diagnostic::error(codes::UNEXPECTED_TOKEN, "Wid writes a `guard` where Odin writes `or_return`")
            .primary(keyword, "Odin's error propagation operator")
            .note("Wid has no shorthand propagation operators: a `guard` handles the error where it happens, and its `else` branch passes it on");
        let (names, value) = match kind {
            StmtKind::Assign { targets, op: None, mut values }
                if values.len() == 1 && targets.iter().all(|t| matches!(t.kind, ExprKind::Ident(_))) =>
            {
                let names: Vec<Ident> = targets
                    .iter()
                    .filter_map(|t| match t.kind {
                        ExprKind::Ident(name) => Some(Ident { name, span: t.span }),
                        _ => None,
                    })
                    .collect();
                (names, values.remove(0))
            }
            StmtKind::Expr(value) => (Vec::new(), value),
            other => {
                self.report(diag.help(
                    "bind the values with `guard a = f() else |err| … end`, and pass `err` on in its `else` branch",
                ));
                return other;
            }
        };
        let line = self.line_of(start.start);
        let indent = " ".repeat(self.indent_of_line(line));
        let written: Vec<&str> = names.iter().map(|n| self.text_of(n.span)).collect();
        let head = if written.is_empty() {
            format!("guard {}", self.text_of(value.span))
        } else {
            format!("guard {} = {}", written.join(", "), self.text_of(value.span))
        };
        let (binding, exit, applicability) = match self.returns() {
            Returns::Nothing => ("", "return".to_string(), Applicability::MaybeIncorrect),
            Returns::Optional => ("", "return nil".to_string(), Applicability::MachineApplicable),
            Returns::Error => (" |err|", "return err".to_string(), Applicability::MachineApplicable),
            Returns::Values(n) => {
                let zeros = vec!["{}"; n.saturating_sub(1)].join(", ");
                (" |err|", format!("return {zeros}, err"), Applicability::MachineApplicable)
            }
        };
        // Without names, the fix fits a call that returns only an error;
        // one that returns values too needs a name for each.
        let applicability = if written.is_empty() { Applicability::MaybeIncorrect } else { applicability };
        let fix = format!("{head} else{binding}\n{indent}  {exit}\n{indent}end");
        let mut diag = diag.suggest(
            "handle the error with `guard`",
            vec![Edit { span: start.to(keyword), replacement: fix }],
            applicability,
        );
        if matches!(self.returns(), Returns::Nothing) {
            diag =
                diag.help("this method returns nothing, so it can't pass the error on: handle it in the `else` branch");
        }
        self.report(diag);
        if names.is_empty() {
            // `f() or_return` drops every value but the error; it is read as
            // `_ = f()`, which checks `f()` for any number of values.
            let discard = Expr { kind: ExprKind::Ident(Name::new("_")), span: value.span.shrink_to_start() };
            return StmtKind::Assign { targets: vec![discard], op: None, values: vec![value] };
        }
        // The `else` branch was reported; the checker takes it as leaving.
        let else_body = vec![Stmt { kind: StmtKind::Error, span: keyword, attrs: Vec::new() }];
        StmtKind::Guard { names, value, err: None, else_body }
    }

    /// What a `return` gives back in the method being parsed.
    fn returns(&self) -> Returns {
        match self.returns.last() {
            None | Some(None) => Returns::Nothing,
            Some(Some(TypeExpr { kind: TypeKind::Optional(_), .. })) => Returns::Optional,
            Some(Some(TypeExpr { kind: TypeKind::Tuple(elems), .. })) => Returns::Values(elems.len()),
            Some(Some(_)) => Returns::Error,
        }
    }
}

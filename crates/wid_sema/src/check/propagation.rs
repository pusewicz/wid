//! Zig's `try`, which Wid writes with `guard`: one error with a fix, and the
//! call is read as the values before the error, so code after it checks.

use wid_diagnostics::{Applicability, Diagnostic, Edit, Span, codes};
use wid_syntax::ast::{self, Ident};

use super::Checker;
use crate::ir::{self, ExprKind};
use crate::types::{TyId, TyKind};

impl<'a> Checker<'a> {
    /// Reports `try value` when no method is named `try` (E0201): Wid has no
    /// shorthand propagation operators. Its fix writes the `guard` that
    /// passes the error on, when the call ends a statement that assigns it
    /// (`x = try f()`) or stands alone. Returns the values before the error,
    /// of their types, for the code around the call.
    pub(super) fn try_habit(&mut self, name: Ident, args: &[ast::Arg], span: Span) -> Option<ir::Expr> {
        let [arg] = args else { return None };
        if name.as_str() != "try" || arg.name.is_some() || arg.splat {
            return None;
        }
        let value = self.expr(&arg.value, None);
        let mut diag = Diagnostic::error(codes::UNDEFINED_NAME, "Wid writes a `guard` where Zig writes `try`")
            .primary(name.span, "Zig's error propagation operator")
            .note("Wid has no shorthand propagation operators: a `guard` handles the error where it happens, and its `else` branch passes it on");
        diag = match self.try_as_guard(span, arg.value.span) {
            Some((edit, applicability)) => diag.suggest("handle the error with `guard`", vec![edit], applicability),
            None => diag.help(
                "bind the values with `guard a = f() else |err| … end` on a line of their own, and use them here",
            ),
        };
        if matches!(self.types.kind(self.frame().ret), TyKind::Void) {
            diag =
                diag.help("this method returns nothing, so it can't pass the error on: handle it in the `else` branch");
        }
        self.report(diag);
        let payload = self.payload_type(value.ty);
        Some(ir::Expr::new(ExprKind::Zero, payload))
    }

    /// The values a `guard` binds from a value of type `ty`: those before
    /// the error, an optional's value, or nothing for an error alone.
    fn payload_type(&mut self, ty: TyId) -> TyId {
        match self.types.kind(ty).clone() {
            TyKind::Tuple(elems) if elems.last().is_some_and(|t| self.types.is_nilable(*t)) => {
                self.types.tuple(elems[..elems.len() - 1].to_vec())
            }
            TyKind::Error | TyKind::Union(_) => self.types.void(),
            TyKind::Optional(inner) => inner,
            _ => ty,
        }
    }

    /// The `guard` that `try value` (the call at `call`) means, replacing the
    /// statement it ends: `x = try f()` or `try f()`.
    fn try_as_guard(&mut self, call: Span, value: Span) -> Option<(Edit, Applicability)> {
        let stmt = self.macros.line;
        // Only a file of the program: a macro's code is fixed in its `quote`.
        if !self.source_texts.contains_key(&stmt.file)
            || stmt.file != call.file
            || stmt.start > call.start
            || stmt.end != call.end
        {
            return None;
        }
        let before = self.source_text(Span::new(call.file, stmt.start, call.start));
        let before = before.trim_end();
        let names = match before.strip_suffix('=') {
            Some(names) if !names.ends_with(['=', '!', '<', '>', '+', '-', '*', '/', '%', '|', '&', '~']) => {
                let names: Vec<&str> = names.split(',').map(str::trim).collect();
                let plain = |n: &&str| {
                    n.chars().next().is_some_and(|c| c == '_' || c.is_ascii_lowercase())
                        && n.chars().all(|c| c == '_' || c.is_ascii_alphanumeric())
                };
                if !names.iter().all(plain) {
                    return None;
                }
                names.join(", ")
            }
            None if before.is_empty() => String::new(),
            _ => return None,
        };
        let value = self.source_text(value);
        let head = if names.is_empty() { format!("guard {value}") } else { format!("guard {names} = {value}") };
        let (binding, exit, applicability) = match self.types.kind(self.frame().ret).clone() {
            TyKind::Void => ("", "return".to_string(), Applicability::MaybeIncorrect),
            TyKind::Optional(_) => ("", "return nil".to_string(), Applicability::MachineApplicable),
            TyKind::Error | TyKind::Union(_) => (" |err|", "return err".to_string(), Applicability::MachineApplicable),
            TyKind::Tuple(elems) => {
                let zeros = vec!["{}"; elems.len() - 1].join(", ");
                (" |err|", format!("return {zeros}, err"), Applicability::MachineApplicable)
            }
            _ => ("", "return {}".to_string(), Applicability::MaybeIncorrect),
        };
        let line = self.source_text(Span::new(stmt.file, self.line_start(stmt), stmt.start));
        let indent: String = line.chars().take_while(|c| *c == ' ' || *c == '\t').collect();
        let replacement = format!("{head} else{binding}\n{indent}  {exit}\n{indent}end");
        Some((Edit { span: stmt, replacement }, applicability))
    }

    /// Where the line holding the start of `span` starts.
    fn line_start(&self, span: Span) -> u32 {
        let text = self.source_texts.get(&span.file).map_or("", |t| t.as_ref());
        let at = (span.start as usize).min(text.len());
        text[..at].rfind('\n').map_or(0, |i| i as u32 + 1)
    }
}

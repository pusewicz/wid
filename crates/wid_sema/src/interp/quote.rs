//! `quote do … end` at compile time: running a `quote` records a fragment,
//! the template plus the values of its splices, and gives back its number
//! as a `Code` value. The checker builds the generated code from the
//! fragments once the macro returns (`check/macros.rs`).

use wid_diagnostics::Span;
use wid_syntax::Name;

use super::{Fragment, Interp, R, SpliceValue, as_f64, i64_at, slice, word};
use crate::ir;
use crate::types::{TyId, TyKind};

impl<'c> Interp<'c> {
    /// Runs `quote` number `template`: evaluates its splices in order and
    /// records them.
    pub(super) fn quote(&mut self, template: u32, splices: &'c [ir::Expr], span: Span) -> R<Vec<u8>> {
        let mut values = Vec::with_capacity(splices.len());
        for e in splices {
            let v = self.eval(e)?;
            values.push(self.splice_value(e.ty, &v, span)?);
        }
        self.fragments.push(Fragment { template, values });
        let code = self.first_fragment + self.fragments.len() as u64;
        Ok(code.to_le_bytes().to_vec())
    }

    /// Reads a splice's value by its type. The checker only lets a `quote`
    /// splice the types handled here.
    fn splice_value(&self, ty: TyId, v: &[u8], span: Span) -> R<SpliceValue> {
        let types = self.p.types;
        let word_at = |at: u64| u64::from_le_bytes(word(&slice(v, at, 8)));
        Ok(match self.kind(ty) {
            TyKind::Code => SpliceValue::Code(word_at(0)),
            TyKind::Symbol => SpliceValue::Symbol(self.symbol(word_at(0), span)?),
            TyKind::Type => {
                let t = TyId(word_at(0) as u32);
                if t.0 as usize >= types.len() {
                    return Err(self.fail_at(span, "this `Type` value does not name a type"));
                }
                SpliceValue::Type(t)
            }
            TyKind::Bool => SpliceValue::Bool(Self::truthy(v)),
            TyKind::Int(_) => SpliceValue::Int(self.int(ty, v)),
            TyKind::Float(_) => SpliceValue::Float(as_f64(self.decode(ty, v))),
            TyKind::String => SpliceValue::Str(String::from_utf8_lossy(&self.string_bytes(v)?).into_owned()),
            TyKind::Array(elem, _) | TyKind::Slice(elem) | TyKind::Dynamic(elem)
                if matches!(self.kind(*elem), TyKind::Code | TyKind::Symbol) =>
            {
                let words = match self.kind(ty) {
                    TyKind::Array(_, n) => (0..*n).map(|i| word_at(i * 8)).collect(),
                    _ => {
                        let (data, len) = (word_at(0), i64_at(v, 8).max(0) as u64);
                        let bytes = self.rd(data, len * 8)?;
                        (0..len).map(|i| u64::from_le_bytes(word(&slice(&bytes, i * 8, 8)))).collect::<Vec<_>>()
                    }
                };
                if matches!(self.kind(*elem), TyKind::Code) {
                    SpliceValue::Codes(words)
                } else {
                    SpliceValue::Symbols(words.into_iter().map(|w| self.symbol(w, span)).collect::<R<_>>()?)
                }
            }
            _ => {
                let shown = types.display(ty);
                return Err(self.fail_at(span, format!("a `{shown}` can't be spliced into code")));
            }
        })
    }

    /// The name a `Symbol` value holds.
    fn symbol(&self, value: u64, span: Span) -> R<Name> {
        u32::try_from(value)
            .ok()
            .and_then(Name::from_index)
            .ok_or_else(|| self.fail_at(span, "this `Symbol` value does not name anything"))
    }
}

//! Map keys: a map finds a key with the built-in `==`, so a key type is one
//! whose equality is the built-in one.

use wid_diagnostics::{Diagnostic, Span, codes};

use super::{Checker, MapKeySite};
use crate::types::{TyId, TyKind};

/// Why a type can't be a map key.
enum KeyProblem {
    /// The type has no `==`, like a slice, a union or a proc.
    NoEquality(TyId),
    /// A struct defines its own `==`, which a map doesn't call.
    OwnEquality(TyId),
}

impl<'a> Checker<'a> {
    /// Reports each map type written with a key type whose `==` isn't the
    /// built-in one (E0332), once every type is complete. A map finds a key
    /// by hashing and comparing it the way the built-in `==` does.
    pub(super) fn check_map_keys(&mut self) {
        for MapKeySite { key, span, within, call } in std::mem::take(&mut self.map_keys) {
            let Some(problem) = self.key_problem(key) else { continue };
            let shown = self.types.display(key);
            let mut diag = Diagnostic::error(codes::INVALID_MAP_KEY, format!("`{shown}` can't be a map key"))
                .primary(span, "this key type");
            if let Some(t) = within {
                diag = diag.note(format!("in `{}`", self.types.display(t)));
            }
            if let Some((name, site)) = call {
                diag = diag.secondary(site, format!("`{name}` is checked for these types because of this call"));
            }
            let diag = match problem {
                KeyProblem::NoEquality(inner) => {
                    let what = self.types.display(inner);
                    let (because, help) = match self.types.kind(self.types.base(inner)) {
                        TyKind::Slice(_) | TyKind::Dynamic(_) => (
                            format!("`{what}` has no `==`: it views or owns memory that may change"),
                            "key it by a fixed array (`[4]Int`) or a `String`".to_string(),
                        ),
                        TyKind::Union(_) => (
                            format!("`{what}` has no `==`"),
                            "key it by an enum, or by a struct of the values that tell its variants apart".to_string(),
                        ),
                        _ => (
                            format!("`{what}` has no `==`"),
                            "key it by a number, a string, an enum or a struct of them".to_string(),
                        ),
                    };
                    let note = if inner == key { because } else { format!("{because}, and `{shown}` holds one") };
                    diag.note(note).help(help)
                }
                KeyProblem::OwnEquality(own) => {
                    let what = self.types.display(own);
                    diag.note(format!(
                        "`{what}` defines its own `==`, and a map finds keys with the built-in one, comparing every field"
                    ))
                    .help(format!("key the map by the values `{what}`'s `==` compares, like one of its fields"))
                }
            };
            self.report(diag);
        }
    }

    /// Why `ty` can't be a map key, if it can't: it, or a value it holds,
    /// has no `==`, or is a struct with its own.
    fn key_problem(&mut self, ty: TyId) -> Option<KeyProblem> {
        if self.has_params(ty) {
            return None;
        }
        let base = self.types.base(ty);
        match self.types.kind(base).clone() {
            TyKind::Unknown => None,
            TyKind::Struct(id) => {
                if self.operator_method(base, "==").is_some() {
                    return Some(KeyProblem::OwnEquality(ty));
                }
                let fields: Vec<TyId> = self.types.struct_info(id).fields.iter().map(|f| f.ty).collect();
                fields.into_iter().find_map(|f| self.key_problem(f))
            }
            TyKind::Array(elem, _) | TyKind::Matrix(elem, _, _) | TyKind::Optional(elem) => self.key_problem(elem),
            TyKind::Tuple(elems) => elems.into_iter().find_map(|e| self.key_problem(e)),
            _ if self.is_comparable(base) => None,
            _ => Some(KeyProblem::NoEquality(ty)),
        }
    }

    /// Records a map key type written at `span`, checked once types are
    /// complete (see [`Checker::check_map_keys`]).
    pub(super) fn note_map_key(&mut self, key: TyId, span: Span, within: Option<TyId>) {
        let call = self.instance_stack.first().map(|(name, site, _, _)| (name.clone(), *site));
        self.map_keys.push(MapKeySite { key, span, within, call });
    }
}

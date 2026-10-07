//! `@[test]` methods: signature checks and, in test builds, the list of tests
//! plus the `core:testing` runner the generated `main` calls.

use wid_diagnostics::{Applicability, Diagnostic, codes};
use wid_syntax::Name;

use super::{Checker, DeclId, DeclKind};
use crate::input::PackageId;
use crate::ir::{self, FnId};
use crate::types::TyKind;

/// The package that defines `T` and `run_test`.
const TESTING_PACKAGE: &str = "core:testing";

impl Checker<'_> {
    /// Checks the signature of every `@[test]` method in the root package and,
    /// in test builds, returns the tests in source order with the runner.
    pub(super) fn collect_tests(&mut self) -> (Vec<ir::TestCase>, Option<FnId>) {
        let testing_pkg =
            self.input.packages.iter().position(|p| p.path == TESTING_PACKAGE).map(|i| PackageId(i as u32));
        let t_ty = testing_pkg
            .and_then(|p| self.lookup_pkg(p, Name::new("T")))
            .filter(|d| matches!(self.decls[d.0 as usize].kind, DeclKind::Struct(_)))
            .map(|d| self.decl_as_type(d, self.decls[d.0 as usize].span));
        let mut tests = Vec::new();
        for i in 0..self.decls.len() {
            let decl = DeclId(i as u32);
            let d = &self.decls[i];
            let DeclKind::Fn(f) = d.kind else { continue };
            if d.loc.pkg != PackageId(0) || !d.item.has_attr("test") {
                continue;
            }
            let (name, owner) = (d.name, d.owner);
            let sig = self.fn_sig(decl);
            let takes_t = match (sig.params.as_slice(), t_ty) {
                ([p], Some(t)) => matches!(self.types.kind(p.ty), TyKind::Pointer(inner) if *inner == t),
                _ => false,
            };
            let ok = owner.is_none()
                && takes_t
                && matches!(self.types.kind(sig.ret), TyKind::Void)
                && sig.block.is_none()
                && self.generic_names(decl).is_empty();
            if !ok {
                let param = f.params.first().map_or_else(|| "t".to_string(), |p| p.name.as_str().to_string());
                let label = if owner.is_some() {
                    "a test must be a package-level method"
                } else {
                    "a test takes `^testing.T` and returns nothing"
                };
                let mut diag =
                    Diagnostic::error(codes::TEST_SIGNATURE, format!("test `{name}` has the wrong signature"))
                        .primary(f.sig_span, label)
                        .note("`wid test` calls each `@[test]` method with a `^testing.T` that records failures");
                if owner.is_some() {
                    diag = diag.help("move the test out of the type: tests are package-level methods");
                } else {
                    diag = diag.suggest_replace(
                        "use the test signature",
                        f.sig_span,
                        format!("def {name}({param}: ^testing.T)"),
                        Applicability::MaybeIncorrect,
                    );
                }
                if testing_pkg.is_none() {
                    diag = diag.help("import the test helpers with `import \"core:testing\"`");
                }
                self.report(diag);
                continue;
            }
            if self.input.options.testing {
                let func = self.fn_instance(decl);
                let span = self.decls[i].span;
                tests.push(ir::TestCase { name: name.as_str().to_string(), func, span });
            }
        }
        if tests.is_empty() {
            return (tests, None);
        }
        let runner = testing_pkg
            .and_then(|p| self.lookup_pkg(p, Name::new("run_test")))
            .filter(|d| matches!(self.decls[d.0 as usize].kind, DeclKind::Fn(_)))
            .map(|d| self.fn_instance(d));
        (tests, runner)
    }
}

//! Validation of `@[…]` attributes. Each declaration's attributes are checked
//! once, when it is collected, so unused and generic methods get the same
//! errors as called ones.

use wid_diagnostics::{Applicability, Diagnostic, codes, did_you_mean};
use wid_syntax::ast::{self, ItemKind};

use super::Checker;

/// What an attribute accepts inside its parentheses.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Args {
    None,
    OptionalString,
    Int,
}

/// Attributes a `def` accepts, with their arguments.
const METHOD_ATTRS: &[(&str, Args)] = &[
    ("c", Args::None),
    ("export", Args::OptionalString),
    ("extern", Args::OptionalString),
    ("no_bounds_check", Args::None),
    ("test", Args::None),
];

/// Attributes a struct accepts: `extern` structs are defined by C.
const STRUCT_ATTRS: &[(&str, Args)] =
    &[("extern", Args::OptionalString), ("opaque", Args::None), ("size", Args::Int), ("align", Args::Int)];

/// Attributes a constant accepts: an `extern` constant is a C value read by
/// name.
const CONST_ATTRS: &[(&str, Args)] = &[("extern", Args::OptionalString)];

/// Attributes a statement accepts.
const STATEMENT_ATTRS: &[&str] = &["no_bounds_check"];

/// Every attribute name, for did-you-mean suggestions.
fn all_names() -> impl Iterator<Item = &'static str> {
    METHOD_ATTRS.iter().chain(STRUCT_ATTRS).map(|(n, _)| *n)
}

impl Checker<'_> {
    /// Checks the attributes written before a declaration.
    pub(super) fn check_item_attributes(&mut self, item: &ast::Item) {
        if let ItemKind::Def(f) = &item.kind
            && item.attrs.is_empty()
        {
            self.check_extern_method(item, f);
        }
        if item.attrs.is_empty() {
            return;
        }
        let what = match &item.kind {
            ItemKind::Def(f) => {
                self.check_method_attributes(item, f);
                return;
            }
            ItemKind::Import(_) => "an `import`",
            ItemKind::Cimport(_) => "a `cimport`",
            ItemKind::Struct(s) => {
                self.check_attr_table(item, STRUCT_ATTRS, "struct");
                self.check_struct_attributes(item, s);
                return;
            }
            ItemKind::Enum(_) => "an enum",
            ItemKind::Union(_) => "a union",
            ItemKind::Module(_) => "a module",
            ItemKind::Extend(_) => "an `extend` block",
            ItemKind::Const(c) => {
                self.check_attr_table(item, CONST_ATTRS, "constant");
                self.check_const_attributes(item, c);
                return;
            }
            ItemKind::Overload(_) => "an overload set",
            ItemKind::Include(_) => "an `include`",
            ItemKind::Field(_) if item.attrs.iter().all(|a| a.name.as_str() == "extern") => {
                self.check_attr_table(item, CONST_ATTRS, "field");
                return;
            }
            ItemKind::Field(_) => "a field",
            ItemKind::MacroCall(_) | ItemKind::ComptimeIf(_) | ItemKind::Splice(_) | ItemKind::Error => return,
        };
        for attr in &item.attrs {
            let name = attr.name.as_str();
            let mut diag = if all_names().any(|n| n == name) {
                Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, format!("`@[{name}]` only applies to methods"))
                    .primary(attr.name.span, format!("this is {what}, not a `def`"))
            } else {
                self.unknown_attribute(attr, std::iter::empty())
            };
            diag = diag.note(format!("{what} takes no attributes")).help(format!("remove `@[{name}]`"));
            self.report(diag);
        }
    }

    /// Checks a method's attributes: known names, their arguments and the
    /// combinations that make sense.
    fn check_method_attributes(&mut self, item: &ast::Item, f: &ast::FnDecl) {
        self.check_attr_table(item, METHOD_ATTRS, "method");
        self.check_extern_method(item, f);
    }

    /// Checks attribute names and arguments against the table of what this
    /// kind of declaration accepts.
    fn check_attr_table(&mut self, item: &ast::Item, table: &[(&str, Args)], kind: &str) {
        let mut seen: Vec<&str> = Vec::new();
        for attr in &item.attrs {
            let name = attr.name.as_str();
            let Some(&(_, args)) = table.iter().find(|(n, _)| *n == name) else {
                let known: Vec<&str> = table.iter().map(|(n, _)| *n).collect();
                let diag = if all_names().any(|n| n == name) {
                    Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, format!("`@[{name}]` does not apply to a {kind}"))
                        .primary(attr.name.span, format!("not a {kind} attribute"))
                        .help(format!("remove `@[{name}]`"))
                } else if did_you_mean(name, known.iter().copied()).is_some() {
                    self.unknown_attribute(attr, known.iter().copied())
                } else {
                    self.unknown_attribute(attr, known.iter().copied()).help(format!("remove `@[{name}]`"))
                };
                self.report(diag.note(format!("{kind} attributes: {}", known.join(", "))));
                continue;
            };
            if seen.contains(&name) {
                self.report(
                    Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, format!("`@[{name}]` is written twice"))
                        .primary(attr.span, "repeated here")
                        .help("remove the second one"),
                );
                continue;
            }
            seen.push(name);
            match (args, attr.args.as_slice()) {
                (Args::Int, [arg]) if matches!(arg.kind, ast::ExprKind::Int(n) if n > 0) => {}
                (Args::Int, _) => {
                    self.report(
                        Diagnostic::error(
                            codes::UNKNOWN_ATTRIBUTE,
                            format!("`@[{name}]` takes a positive number of bytes"),
                        )
                        .primary(attr.span, format!("write it like `@[{name}(8)]`")),
                    );
                }
                (_, []) => {}
                (Args::OptionalString, [arg]) if string_literal(arg).is_some() => {}
                (Args::OptionalString, _) => {
                    self.report(
                        Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, format!("`@[{name}]` takes one string"))
                            .primary(attr.span, "expected a C symbol name like `\"on_audio\"`")
                            .help(format!("write `@[{name}(\"symbol\")]`, or `@[{name}]` to use the method's name")),
                    );
                }
                (Args::None, [first, ..]) => {
                    self.report(
                        Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, format!("`@[{name}]` takes no arguments"))
                            .primary(first.span, "unexpected argument")
                            .suggest_replace("remove the arguments", attr.span, name, Applicability::MachineApplicable),
                    );
                }
            }
        }
    }

    /// Checks what an `@[extern]` method may and may not have.
    fn check_extern_method(&mut self, item: &ast::Item, f: &ast::FnDecl) {
        if let Some(dots) = f.c_variadic
            && !item.has_attr("extern")
        {
            self.report(
                Diagnostic::error(codes::C_VARIADIC, "only `@[extern]` methods take C variadic arguments")
                    .primary(dots, "`...` passes extra arguments the C way, which only C code can read")
                    .help("take the extra values as a slice with a `*rest: T` parameter instead"),
            );
        }
        if item.has_attr("extern") {
            let empty = matches!(&f.body, ast::FnBody::Block(b) if b.is_empty());
            if !empty {
                self.report(
                    Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, "an `@[extern]` method has no body")
                        .primary(f.sig_span, "its code lives in C")
                        .help("remove the body, leaving `def … end`"),
                );
            }
            if let Some(other) = ["export", "test", "no_bounds_check"].into_iter().find(|n| item.has_attr(n))
                && let Some(attr) = item.attr(other)
            {
                self.report(
                    Diagnostic::error(
                        codes::UNKNOWN_ATTRIBUTE,
                        format!("`@[{other}]` cannot be combined with `@[extern]`"),
                    )
                    .primary(attr.span, "an `@[extern]` method is defined in C, not in Wid")
                    .help(format!("remove `@[{other}]`")),
                );
            }
        }
    }

    /// Checks the combinations of struct attributes: only `@[extern]`
    /// structs take a layout or `opaque`, and an opaque one has no fields.
    fn check_struct_attributes(&mut self, item: &ast::Item, s: &ast::StructDecl) {
        let is_extern = item.has_attr("extern");
        for name in ["opaque", "size", "align"] {
            if let Some(attr) = item.attr(name)
                && !is_extern
            {
                self.report(
                    Diagnostic::error(
                        codes::UNKNOWN_ATTRIBUTE,
                        format!("`@[{name}]` only applies to `@[extern]` structs"),
                    )
                    .primary(attr.span, "this struct is laid out by Wid")
                    .help("add `extern(\"CName\")` if C defines the struct, or remove the attribute"),
                );
            }
        }
        if let Some(attr) = item.attr("opaque") {
            let field = s.body.iter().find(|i| matches!(i.kind, ItemKind::Field(_)));
            if let Some(field) = field {
                self.report(
                    Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, "an `@[opaque]` struct has no fields")
                        .primary(field.span, "a field of an opaque struct")
                        .secondary(attr.span, "declared opaque here")
                        .help("remove the fields, or remove `opaque` and declare every field"),
                );
            }
            if let Some(layout) = item.attr("size").or_else(|| item.attr("align")) {
                self.report(
                    Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, "an `@[opaque]` struct has no known layout")
                        .primary(layout.span, "an opaque struct is only used through pointers")
                        .help("remove this attribute"),
                );
            }
        }
        if let (Some(one), None) | (None, Some(one)) = (item.attr("size"), item.attr("align")) {
            let missing = if one.name.as_str() == "size" { "align" } else { "size" };
            self.report(
                Diagnostic::error(
                    codes::UNKNOWN_ATTRIBUTE,
                    format!("`@[{}]` needs `@[{missing}]` too", one.name.as_str()),
                )
                .primary(one.span, "a layout needs both the size and the alignment")
                .help(format!("add `{missing}(N)` with the value C reports for `{missing}of`")),
            );
        }
    }

    /// Checks an `@[extern]` constant: C provides its value, so it has a
    /// declared type and `---` in place of a value.
    fn check_const_attributes(&mut self, item: &ast::Item, c: &ast::ConstDecl) {
        let Some(attr) = item.attr("extern") else { return };
        if c.ty.is_none() {
            self.report(
                Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, "an `@[extern]` constant needs a type")
                    .primary(c.name.span, "C provides the value, so Wid can't infer its type")
                    .suggest(
                        "declare the type",
                        vec![wid_diagnostics::Edit {
                            span: c.name.span.shrink_to_end(),
                            replacement: ": C.int".into(),
                        }],
                        Applicability::HasPlaceholders,
                    ),
            );
        }
        if !matches!(c.value.kind, ast::ExprKind::Uninit) {
            self.report(
                Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, "an `@[extern]` constant has no Wid value")
                    .primary(c.value.span, "C provides the value")
                    .secondary(attr.span, "declared `extern` here")
                    .suggest_replace("write `---` instead", c.value.span, "---", Applicability::MachineApplicable),
            );
        }
    }

    /// Checks the attributes written before a statement and reports whether
    /// it carries `@[no_bounds_check]`.
    pub(super) fn check_statement_attributes(&mut self, attrs: &[ast::Attribute]) -> bool {
        let mut no_bounds = false;
        for attr in attrs {
            if attr.name.as_str() == "no_bounds_check" {
                no_bounds = true;
                if let Some(first) = attr.args.first() {
                    self.report(
                        Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, "`@[no_bounds_check]` takes no arguments")
                            .primary(first.span, "unexpected argument")
                            .suggest_replace(
                                "remove the arguments",
                                attr.span,
                                "no_bounds_check",
                                Applicability::MachineApplicable,
                            ),
                    );
                }
                continue;
            }
            let name = attr.name.as_str();
            let diag = if all_names().any(|n| n == name) {
                Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, format!("`@[{name}]` does not apply to a statement"))
                    .primary(attr.name.span, "it goes before a `def`")
            } else {
                self.unknown_attribute(attr, STATEMENT_ATTRS.iter().copied())
            };
            self.report(diag.note("statements accept only `@[no_bounds_check]`"));
        }
        no_bounds
    }

    /// Builds the error for an attribute name Wid does not know, suggesting
    /// the closest of `known`.
    fn unknown_attribute<'k>(&self, attr: &ast::Attribute, known: impl Iterator<Item = &'k str>) -> Diagnostic {
        let name = attr.name.as_str();
        let mut diag = Diagnostic::error(codes::UNKNOWN_ATTRIBUTE, format!("unknown attribute `@[{name}]`"))
            .primary(attr.name.span, "not an attribute Wid knows");
        if let Some(best) = did_you_mean(name, known) {
            diag = diag.suggest_replace(
                format!("did you mean `{best}`?"),
                attr.name.span,
                best,
                Applicability::MaybeIncorrect,
            );
        }
        diag
    }
}

/// The text of a plain string literal without interpolation.
pub(crate) fn string_literal(e: &ast::Expr) -> Option<&str> {
    match &e.kind {
        ast::ExprKind::Str(parts) if parts.len() == 1 => match &parts[0] {
            ast::StrPart::Text(t) => Some(t),
            ast::StrPart::Interp(_) => None,
        },
        ast::ExprKind::Str(parts) if parts.is_empty() => Some(""),
        _ => None,
    }
}

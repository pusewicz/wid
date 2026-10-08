//! The diagnostic data model shared by every compiler stage.

use crate::codes::Code;
use crate::source::Span;

/// How serious a diagnostic is.
#[derive(Clone, Copy, PartialEq, Eq, Debug, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// Compilation cannot succeed.
    Error,
    /// Compilation succeeds but something looks wrong.
    Warning,
}

impl Severity {
    /// Returns the lowercase name used in rendered output.
    pub fn as_str(self) -> &'static str {
        match self {
            Severity::Error => "error",
            Severity::Warning => "warning",
        }
    }
}

/// A source span with an explanation attached.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Label {
    /// The highlighted range.
    pub span: Span,
    /// What the range has to do with the problem.
    pub message: String,
    /// Primary labels mark the problem itself; secondary labels add context.
    pub primary: bool,
    /// Whether this secondary label marks where code that a macro call
    /// spliced in from its call site landed in the code the call generated:
    /// the renderers list the macro calls behind it (see
    /// [`Diagnostic::chain_span`]).
    pub splice: bool,
}

/// How safely a suggested fix can be applied by a tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Applicability {
    /// The edit is known to be correct and can be applied automatically.
    MachineApplicable,
    /// The edit is probably right but should be reviewed.
    MaybeIncorrect,
    /// The edit contains placeholders the user must fill in.
    HasPlaceholders,
}

impl Applicability {
    /// Returns the snake_case name used in JSON output.
    pub fn as_str(self) -> &'static str {
        match self {
            Applicability::MachineApplicable => "machine_applicable",
            Applicability::MaybeIncorrect => "maybe_incorrect",
            Applicability::HasPlaceholders => "has_placeholders",
        }
    }
}

/// Replaces the text covered by `span` with `replacement`.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Edit {
    /// The range to replace; empty spans insert.
    pub span: Span,
    /// The new text.
    pub replacement: String,
}

/// A piece of advice, optionally carrying concrete edits.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Help {
    /// The advice in plain English.
    pub message: String,
    /// Edits that implement the advice; may be empty.
    pub edits: Vec<Edit>,
    /// How safe the edits are to apply automatically.
    pub applicability: Applicability,
}

/// One problem found in the program.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Diagnostic {
    /// How serious the problem is.
    pub severity: Severity,
    /// The stable error code.
    pub code: Code,
    /// A one-line summary.
    pub message: String,
    /// Highlighted source ranges.
    pub labels: Vec<Label>,
    /// Extra facts that explain the problem.
    pub notes: Vec<String>,
    /// Suggested ways to fix the problem.
    pub helps: Vec<Help>,
}

impl Diagnostic {
    /// Starts an error diagnostic.
    pub fn error(code: Code, message: impl Into<String>) -> Self {
        Diagnostic {
            severity: Severity::Error,
            code,
            message: message.into(),
            labels: Vec::new(),
            notes: Vec::new(),
            helps: Vec::new(),
        }
    }

    /// Starts a warning diagnostic.
    pub fn warning(code: Code, message: impl Into<String>) -> Self {
        Diagnostic { severity: Severity::Warning, ..Diagnostic::error(code, message) }
    }

    /// Adds the primary label.
    pub fn primary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label { span, message: message.into(), primary: true, splice: false });
        self
    }

    /// Adds a secondary label.
    pub fn secondary(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label { span, message: message.into(), primary: false, splice: false });
        self
    }

    /// Adds a note.
    pub fn note(mut self, message: impl Into<String>) -> Self {
        self.notes.push(message.into());
        self
    }

    /// Adds advice without edits.
    pub fn help(mut self, message: impl Into<String>) -> Self {
        self.helps.push(Help {
            message: message.into(),
            edits: Vec::new(),
            applicability: Applicability::MaybeIncorrect,
        });
        self
    }

    /// Adds advice with concrete edits.
    pub fn suggest(mut self, message: impl Into<String>, edits: Vec<Edit>, applicability: Applicability) -> Self {
        self.helps.push(Help { message: message.into(), edits, applicability });
        self
    }

    /// Adds advice that replaces `span` with `replacement`.
    pub fn suggest_replace(
        self,
        message: impl Into<String>,
        span: Span,
        replacement: impl Into<String>,
        applicability: Applicability,
    ) -> Self {
        self.suggest(message, vec![Edit { span, replacement: replacement.into() }], applicability)
    }

    /// Adds a secondary label at `span`, where the code the problem is in,
    /// which a macro call spliced in from its call site, landed in the code
    /// the call generated.
    pub fn splice(mut self, span: Span, message: impl Into<String>) -> Self {
        self.labels.push(Label { span, message: message.into(), primary: false, splice: true });
        self
    }

    /// Returns the span of the first primary label, if any.
    pub fn primary_span(&self) -> Option<Span> {
        self.labels.iter().find(|l| l.primary).or(self.labels.first()).map(|l| l.span)
    }

    /// The span whose macro calls the renderers list
    /// ([`crate::SourceMap::expansion_chain`]): where spliced code landed
    /// ([`Diagnostic::splice`]), else the primary span.
    pub fn chain_span(&self) -> Option<Span> {
        self.labels.iter().find(|l| l.splice).map(|l| l.span).or_else(|| self.primary_span())
    }
}

/// A collection of diagnostics produced by one or more stages.
#[derive(Clone, Debug, Default)]
pub struct Diagnostics {
    list: Vec<Diagnostic>,
}

impl Diagnostics {
    /// Creates an empty collection.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a diagnostic unless an identical one was already reported.
    pub fn push(&mut self, diag: Diagnostic) {
        if !self.list.contains(&diag) {
            self.list.push(diag);
        }
    }

    /// Moves every diagnostic from `other` into `self`.
    pub fn extend(&mut self, other: Diagnostics) {
        for d in other.list {
            self.push(d);
        }
    }

    /// Returns true if any error was reported.
    pub fn has_errors(&self) -> bool {
        self.list.iter().any(|d| d.severity == Severity::Error)
    }

    /// Returns the number of errors.
    pub fn error_count(&self) -> usize {
        self.list.iter().filter(|d| d.severity == Severity::Error).count()
    }

    /// Returns the number of warnings.
    pub fn warning_count(&self) -> usize {
        self.list.iter().filter(|d| d.severity == Severity::Warning).count()
    }

    /// Returns true when the collection is empty.
    pub fn is_empty(&self) -> bool {
        self.list.is_empty()
    }

    /// Returns the diagnostics in report order.
    pub fn iter(&self) -> impl Iterator<Item = &Diagnostic> {
        self.list.iter()
    }

    /// Sorts diagnostics by file and position so output is deterministic.
    pub fn sort(&mut self) {
        self.list.sort_by_key(|d| {
            let span = d.primary_span().unwrap_or_default();
            (span.file, span.start, span.end)
        });
    }

    /// Consumes the collection, returning the underlying list.
    pub fn into_vec(self) -> Vec<Diagnostic> {
        self.list
    }
}

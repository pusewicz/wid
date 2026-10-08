//! `wid fmt`: rewrites a package's files, or one file, in the canonical
//! style of [`wid_syntax::fmt`] (SPEC "Toolchain and CLI"). Formatting is
//! pure syntax: it never loads imports, type-checks or generates code.
//!
//! A file that doesn't parse cleanly is reported and left as it is. A file
//! is only rewritten when its formatted text parses to the same tree with
//! the same comments; when it wouldn't, that is a bug in the formatter,
//! reported as such, and the file is left as it is too.

use std::path::{Path, PathBuf};

use wid_diagnostics::{Diagnostic, Diagnostics, SourceMap, codes};

use crate::Options;
use crate::cmdline::{CommandLine, FileRequest, Pending, file_target};
pub use crate::doc::Printed;
use crate::loader::clean_path;

/// One file `wid fmt` read.
#[derive(Clone, Debug)]
pub struct FmtFile {
    /// Where the file is.
    pub path: PathBuf,
    /// The path as messages show it.
    pub display: String,
    /// The canonical text, when the file parsed cleanly and could be
    /// formatted.
    pub formatted: Option<String>,
    /// Whether the canonical text differs from the file.
    pub changed: bool,
}

/// The result of `wid fmt`.
#[derive(Debug)]
pub struct FmtOutput {
    /// Every file read.
    pub sources: SourceMap,
    /// Parse errors, and why a file couldn't be read or written.
    pub diags: Diagnostics,
    /// The files, in the order they were read.
    pub files: Vec<FmtFile>,
    /// Files the formatter couldn't format without changing their syntax
    /// tree: a bug in the formatter. They are left as they are.
    pub internal_errors: Vec<String>,
    /// Whether this was `-check`, which changes nothing.
    pub check: bool,
}

impl FmtOutput {
    /// The files that are (or with `-check`, would be) rewritten.
    pub fn changed(&self) -> impl Iterator<Item = &FmtFile> {
        self.files.iter().filter(|f| f.changed)
    }
}

/// Formats the target of `opts`: every `.wid` file of a package directory
/// (its `_test.wid` files too), or one file (a `.wid` file, or any file
/// with `-file`). Unless `check` is set, rewrites the files that change.
pub fn fmt(opts: &Options, check: bool) -> FmtOutput {
    let mut out = FmtOutput {
        sources: SourceMap::new(),
        diags: Diagnostics::new(),
        files: Vec::new(),
        internal_errors: Vec::new(),
        check,
    };
    let paths = match files(opts) {
        Ok(paths) => paths,
        Err(TargetError::Plain(diag)) => {
            out.diags.push(diag);
            return out;
        }
        Err(TargetError::CommandLine(cmd, pending)) => {
            let file = cmd.add(&mut out.sources);
            out.diags.push(pending(file));
            return out;
        }
    };
    let cwd = std::env::current_dir().unwrap_or_default();
    for path in paths {
        let display = clean_path(path.strip_prefix(&cwd).unwrap_or(&path)).display().to_string();
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) => {
                out.diags.push(Diagnostic::error(codes::UNKNOWN_IMPORT, format!("cannot read `{display}`: {e}")));
                continue;
            }
        };
        let file = out.sources.add(path.clone(), display.clone(), text.as_str());
        let (ast, diags) = wid_syntax::parse_file(file, &text);
        if !diags.is_empty() {
            out.diags.extend(diags);
            out.files.push(FmtFile { path, display, formatted: None, changed: false });
            continue;
        }
        let formatted = wid_syntax::fmt::format(&text, &ast);
        let (again, again_diags) = wid_syntax::parse_file(file, &formatted);
        if !again_diags.is_empty() || !wid_syntax::fmt::same_tree(&ast, &again) {
            out.internal_errors.push(display.clone());
            out.files.push(FmtFile { path, display, formatted: None, changed: false });
            continue;
        }
        let changed = formatted != text;
        if changed
            && !check
            && let Err(e) = std::fs::write(&path, &formatted)
        {
            out.diags.push(Diagnostic::error(codes::UNKNOWN_IMPORT, format!("cannot write `{display}`: {e}")));
        }
        out.files.push(FmtFile { path, display, formatted: Some(formatted), changed });
    }
    out
}

/// Why there is nothing to format: a plain error, or one that points into
/// the command line, as `-file` errors do for every command.
enum TargetError {
    Plain(Diagnostic),
    CommandLine(CommandLine, Pending),
}

/// The files to format, or why there are none.
fn files(opts: &Options) -> Result<Vec<PathBuf>, TargetError> {
    let target = &opts.target;
    let shown = target.display();
    let error = |message: String| TargetError::Plain(Diagnostic::error(codes::UNKNOWN_IMPORT, message));
    if opts.file_mode {
        // The errors point into the command line `wid fmt main.wid -file`.
        let mut cmd = CommandLine::new("wid fmt");
        let text = target.to_string_lossy().into_owned();
        let arg = (!text.is_empty()).then(|| (text.as_str(), cmd.arg("", &text), target.clone()));
        cmd.flag("-file");
        let request = FileRequest { cmd: &cmd, arg, code: codes::UNKNOWN_IMPORT, verb: "format", prefix: "" };
        return match file_target(Path::new("."), request) {
            Ok(path) => Ok(vec![path]),
            Err(pending) => Err(TargetError::CommandLine(cmd, pending)),
        };
    }
    if target.is_file() {
        return if is_wid(target) {
            Ok(vec![target.clone()])
        } else {
            Err(TargetError::Plain(
                Diagnostic::error(codes::UNKNOWN_IMPORT, format!("`{shown}` is not a `.wid` file"))
                    .help(format!("to format it anyway, add `-file`: `wid fmt {shown} -file`")),
            ))
        };
    }
    if !target.is_dir() {
        return Err(error(format!("directory `{shown}` does not exist")));
    }
    let mut files: Vec<PathBuf> = std::fs::read_dir(target)
        .map(|rd| rd.flatten().map(|e| e.path()).filter(|p| is_wid(p) && p.is_file()).collect())
        .unwrap_or_default();
    if files.is_empty() {
        return Err(error(format!("`{shown}` contains no `.wid` files")));
    }
    files.sort();
    Ok(files)
}

fn is_wid(path: &Path) -> bool {
    path.extension().is_some_and(|e| e == "wid")
}

/// Renders a result the way `wid fmt` prints it: the files that changed
/// (with `-check`, that would change), one per line on stdout, and the
/// diagnostics on stderr; with `json_errors`, one JSON document on stdout
/// with `changed`, `errors`, `warnings` and `diagnostics`. Formatter bugs
/// are reported on stderr either way.
pub fn print(out: &FmtOutput, json_errors: bool, color: bool) -> Printed {
    let changed: Vec<&str> = out.changed().map(|f| f.display.as_str()).collect();
    let mut stderr = String::new();
    let stdout = if json_errors {
        let diagnostics: Vec<serde_json::Value> =
            out.diags.iter().map(|d| wid_diagnostics::to_json(d, &out.sources)).collect();
        let doc = serde_json::json!({
            "changed": changed,
            "errors": out.diags.error_count(),
            "warnings": out.diags.warning_count(),
            "diagnostics": diagnostics,
        });
        serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n"
    } else {
        if !out.diags.is_empty() {
            let opts = wid_diagnostics::RenderOptions { color };
            stderr = wid_diagnostics::render_all_with(&out.diags, &out.sources, opts, "could not format due to");
        }
        changed.iter().map(|c| format!("{c}\n")).collect()
    };
    for file in &out.internal_errors {
        stderr.push_str(&format!(
            "error: formatting `{file}` would change what it means, so it was left unchanged; \
             this is a bug in `wid fmt`, please report it\n"
        ));
    }
    let success = !out.diags.has_errors() && out.internal_errors.is_empty() && !(out.check && !changed.is_empty());
    Printed { stdout, stderr, success }
}

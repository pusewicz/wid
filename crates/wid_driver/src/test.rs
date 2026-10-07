//! `wid test`: builds a package with its tests and runs each test in its own
//! process, turning failures, panics and leaks into diagnostics.

use std::path::Path;
use std::process::Command;

use std::fmt::Write as _;

use wid_diagnostics::{Diagnostic, RenderOptions, SourceMap, Span, codes, render};

use crate::{Checked, Options, build, scratch_dir};

/// How one test ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestStatus {
    /// Every expectation held and nothing leaked.
    Passed,
    /// An expectation failed or memory leaked.
    Failed,
    /// The test panicked.
    Panicked,
    /// The test process died some other way, like a signal.
    Crashed,
}

impl TestStatus {
    /// The word `wid test` prints for the status.
    pub fn label(self) -> &'static str {
        match self {
            TestStatus::Passed => "ok",
            TestStatus::Failed => "FAILED",
            TestStatus::Panicked => "PANICKED",
            TestStatus::Crashed => "CRASHED",
        }
    }
}

/// The result of one test.
#[derive(Debug)]
pub struct TestResult {
    /// The test method's name.
    pub name: String,
    /// How it ended.
    pub status: TestStatus,
    /// What the test printed, plus its `t.log` messages.
    pub output: String,
    /// One diagnostic per failure, panic or leak.
    pub failures: Vec<Diagnostic>,
}

/// The result of `wid test`.
pub struct TestRun {
    /// The checked program; its diagnostics include build errors.
    pub checked: Checked,
    /// The tests that ran, in source order.
    pub results: Vec<TestResult>,
    /// How many tests `filter` skipped.
    pub filtered_out: usize,
}

impl TestRun {
    /// Returns true when the build succeeded and every test passed.
    pub fn passed(&self) -> bool {
        !self.checked.diags.has_errors() && self.results.iter().all(|r| r.status == TestStatus::Passed)
    }
}

/// Renders what `wid test` prints: one line per test, the failures with the
/// output of each failed test, and a summary line.
pub fn render_test_report(run: &TestRun, color: bool) -> String {
    let paint = |text: &str, code: &str| if color { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_string() };
    let mut out = String::new();
    let count = run.results.len();
    let _ = writeln!(out, "running {count} test{}", if count == 1 { "" } else { "s" });
    for r in &run.results {
        let label = r.status.label();
        let shown = if r.status == TestStatus::Passed { paint(label, "32") } else { paint(label, "31") };
        let _ = writeln!(out, "test {} ... {shown}", r.name);
    }
    let failed: Vec<&TestResult> = run.results.iter().filter(|r| r.status != TestStatus::Passed).collect();
    out.push('\n');
    if !failed.is_empty() {
        out.push_str("failures:\n\n");
        for r in &failed {
            for (i, d) in r.failures.iter().enumerate() {
                if i > 0 {
                    out.push('\n');
                }
                out.push_str(&render(d, &run.checked.sources, RenderOptions { color }));
            }
            if !r.output.is_empty() {
                let _ = writeln!(out, "\n---- output of `{}` ----\n{}", r.name, r.output.trim_end());
            }
            out.push('\n');
        }
    }
    let verdict = if failed.is_empty() { paint("ok", "32") } else { paint("FAILED", "31") };
    let _ = writeln!(
        out,
        "test result: {verdict}. {} passed; {} failed; {} filtered out",
        count - failed.len(),
        failed.len(),
        run.filtered_out
    );
    out
}

/// Builds the package in `opts` with its `_test.wid` files and runs every
/// `@[test]` method whose name contains `filter`.
pub fn test(opts: &Options, filter: Option<&str>) -> TestRun {
    let mut opts = opts.clone();
    opts.testing = true;
    let dir = match scratch_dir() {
        Ok(d) => d,
        Err(e) => {
            let mut checked = crate::check(&opts);
            checked
                .diags
                .push(Diagnostic::error(codes::C_COMPILER_FAILED, format!("cannot create a build directory: {e}")));
            return TestRun { checked, results: Vec::new(), filtered_out: 0 };
        }
    };
    let exe = dir.join(if cfg!(windows) { "tests.exe" } else { "tests" });
    opts.out = Some(exe.clone());
    let built = build(&opts);
    let mut run = TestRun { checked: built.checked, results: Vec::new(), filtered_out: 0 };
    if let (Some(exe), Some(program)) = (built.exe, &run.checked.program) {
        let tests: Vec<(usize, String, Span)> =
            program.tests.iter().enumerate().map(|(i, t)| (i, t.name.clone(), t.span)).collect();
        for (index, name, span) in tests {
            if filter.is_some_and(|f| !name.contains(f)) {
                run.filtered_out += 1;
                continue;
            }
            run.results.push(run_one(&exe, index, &name, span, &run.checked.sources));
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    run
}

/// Runs test `index` of the test binary and interprets what it reported.
fn run_one(exe: &Path, index: usize, name: &str, test_span: Span, sources: &SourceMap) -> TestResult {
    let output = match Command::new(exe).arg(index.to_string()).output() {
        Ok(o) => o,
        Err(e) => {
            return TestResult {
                name: name.to_string(),
                status: TestStatus::Crashed,
                output: String::new(),
                failures: vec![
                    Diagnostic::error(codes::TEST_FAILED, format!("test `{name}` could not run: {e}"))
                        .primary(test_span, "this test"),
                ],
            };
        }
    };
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let mut result =
        TestResult { name: name.to_string(), status: TestStatus::Passed, output: stdout, failures: Vec::new() };
    let rest = parse_records(&stderr, name, test_span, sources, &mut result);
    match output.status.code() {
        Some(0) => {}
        Some(1) => result.status = TestStatus::Failed,
        Some(101) => {
            result.status = TestStatus::Panicked;
            result.failures.push(panic_diagnostic(&rest, name, test_span, sources));
        }
        _ => {
            result.status = if rest.contains("panic: ") { TestStatus::Panicked } else { TestStatus::Crashed };
            if result.status == TestStatus::Panicked {
                result.failures.push(panic_diagnostic(&rest, name, test_span, sources));
            } else {
                result.failures.push(
                    Diagnostic::error(codes::TEST_FAILED, format!("test `{name}` crashed ({})", output.status))
                        .primary(test_span, "this test")
                        .help("run it under a debugger, or build with `-debug -sanitize:address`"),
                );
            }
        }
    }
    if result.status != TestStatus::Panicked {
        result.output.push_str(&rest);
    }
    if result.status == TestStatus::Failed && result.failures.is_empty() {
        result.failures.push(
            Diagnostic::error(codes::TEST_FAILED, format!("test `{name}` failed")).primary(test_span, "this test"),
        );
    }
    result
}

/// Pulls the `wid-test` records `core:testing` writes out of a test's stderr
/// and returns the text that was not part of one.
fn parse_records(stderr: &str, name: &str, test_span: Span, sources: &SourceMap, result: &mut TestResult) -> String {
    const MARK: &str = "\nwid-test ";
    let mut rest = String::new();
    let mut text = stderr;
    while let Some(at) = text.find(MARK) {
        rest.push_str(&text[..at]);
        let after = &text[at + MARK.len()..];
        let Some(header_end) = after.find('\n') else {
            rest.push_str(&text[at..]);
            text = "";
            break;
        };
        let header: Vec<&str> = after[..header_end].splitn(3, ' ').collect();
        let body = &after[header_end + 1..];
        let (Some(kind), Some(loc), Some(len)) = (header.first(), header.get(1), header.get(2)) else {
            rest.push_str(&text[at..at + MARK.len()]);
            text = after;
            continue;
        };
        let len: usize = len.parse().unwrap_or(0).min(body.len());
        let message = &body[..len];
        text = body[len..].strip_prefix('\n').unwrap_or(&body[len..]);
        match *kind {
            "failure" => {
                let mut diag = Diagnostic::error(codes::TEST_FAILED, format!("test `{name}` failed"));
                diag = match location_span(loc, sources) {
                    Some(span) => diag.primary(span, message.to_string()),
                    None => diag.note(format!("{message} (at {loc})")),
                };
                result.failures.push(diag.secondary(test_span, "in this test"));
            }
            _ => {
                let (file, line) =
                    loc.rsplit_once(':').and_then(|(rest, _)| rest.rsplit_once(':')).unwrap_or((loc, ""));
                result.output.push_str(&format!("{file}:{line}: {message}\n"));
            }
        }
    }
    rest.push_str(text);
    rest
}

/// Turns the runtime's `panic: …` report into a diagnostic at the panic site.
fn panic_diagnostic(stderr: &str, name: &str, test_span: Span, sources: &SourceMap) -> Diagnostic {
    let report = stderr.find("panic: ").map_or("", |i| &stderr[i + "panic: ".len()..]);
    let (message, at) = match report.rfind("\n  at ") {
        Some(i) => (&report[..i], report[i + "\n  at ".len()..].lines().next().unwrap_or("")),
        None => (report.trim_end(), ""),
    };
    let loc = at.split(" in `").next().unwrap_or(at);
    let mut lines = message.lines();
    let headline = lines.next().unwrap_or("panicked");
    let mut diag = Diagnostic::error(codes::TEST_FAILED, format!("test `{name}` panicked: {headline}"));
    diag = match location_span(loc, sources) {
        Some(span) => diag.primary(span, "panicked here").secondary(test_span, "in this test"),
        None => diag.primary(test_span, "this test panicked"),
    };
    for line in lines {
        diag = diag.note(line.trim().to_string());
    }
    diag
}

/// Maps `file:line:column`, as the runtime prints locations, to the rest of
/// that source line.
fn location_span(loc: &str, sources: &SourceMap) -> Option<Span> {
    let (rest, col) = loc.rsplit_once(':')?;
    let (file, line) = rest.rsplit_once(':')?;
    let (line, col): (u32, u32) = (line.parse().ok()?, col.parse().ok()?);
    let id = sources.find_by_display(file)?;
    let source = sources.file(id);
    let start = source.offset_of(line, col)?;
    let text = source.line_text_by_index(line as usize - 1);
    let line_end = source.line_start(line as usize - 1) + text.trim_end().len() as u32;
    Some(Span::new(id, start, line_end.max(start + 1)))
}

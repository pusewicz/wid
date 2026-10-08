//! The language test suite.
//!
//! - `tests/run/NAME.wid` (or a directory `tests/run/NAME/`) is built and run.
//!   Its stdout must match `NAME.stdout`. When `NAME.stderr` exists, stderr
//!   must match it too, and `NAME.exitcode` holds a non-zero exit status.
//!   Every program is compiled with clang and, when available, gcc, using
//!   `-std=c23 -Wall -Wextra -Wpedantic -Werror`.
//! - `tests/ui/NAME.wid` (or a directory package `tests/ui/NAME/`, for
//!   imports) is checked and its rendered diagnostics must match
//!   `NAME.stderr`; with `-json-errors` in `NAME.flags`, the JSON document
//!   `wid check -json-errors` prints must.
//! - `tests/test/NAME/` is run with `wid test`, built with the flags in
//!   `tests/test/NAME.flags` when it exists; the report must match
//!   `tests/test/NAME.stdout`.
//! - Every `core/` package with `_test.wid` files is run with `wid test`, and
//!   all of its tests must pass.
//! - `tests/doc/NAME.args` holds the arguments of a `wid doc` command, run
//!   from `tests/doc/` (or from `tests/doc/DIR` when the first argument is
//!   `-in:DIR`, for the forms that document the package in `.`). Its stdout
//!   must match `NAME.stdout` and its stderr `NAME.stderr`; a missing file
//!   expects nothing. The directories there are the packages they document.
//! - `tests/query/NAME.args` holds the arguments of a `wid query` command,
//!   run from the repository root, so packages are named like
//!   `-in:tests/doc/shapes`. Its stdout must match `NAME.stdout` and its
//!   stderr `NAME.stderr`, as for `tests/doc`; a malformed command line
//!   expects the usage error the CLI prints. The directories there are
//!   packages too.
//! - `tests/fmt/NAME.wid` is formatted as `wid fmt NAME.wid -check` would
//!   (the file is never rewritten): the canonical text must match
//!   `NAME.out` and format to itself. A file that doesn't parse has no
//!   `NAME.out`, and `NAME.stderr` holds its diagnostics. `NAME.flags` may
//!   list `-check` and `-json-errors`; then `NAME.stdout` holds what
//!   `wid fmt` prints (a missing file expects nothing).
//!
//! Set `WID_BLESS=1` to rewrite expectations, `WID_TEST_FILTER=text` to run a
//! subset, and `WID_TEST_CC=clang,gcc-16` to choose compilers.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use wid_diagnostics::{RenderOptions, render_all, render_json};
use wid_driver::Options;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root exists")
}

#[derive(Clone, Debug)]
enum Case {
    Run {
        name: String,
        target: PathBuf,
        file_mode: bool,
        expect_base: PathBuf,
    },
    Ui {
        name: String,
        file: PathBuf,
    },
    /// `wid test` on a package; `expect` holds the report, or `None` when
    /// every test must simply pass.
    Test {
        name: String,
        target: PathBuf,
        expect: Option<PathBuf>,
    },
    /// `wid doc` with the arguments in `args`.
    Doc {
        name: String,
        args: PathBuf,
    },
    /// `wid query` with the arguments in `args`.
    Query {
        name: String,
        args: PathBuf,
    },
    /// `wid fmt` on one file.
    Fmt {
        name: String,
        file: PathBuf,
    },
}

fn collect(root: &Path, filter: &str) -> Vec<Case> {
    let mut cases = Vec::new();
    let run_dir = root.join("tests/run");
    if let Ok(rd) = std::fs::read_dir(&run_dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
            if path.is_dir() {
                cases.push(Case::Run {
                    name: format!("run/{name}"),
                    target: path.clone(),
                    file_mode: false,
                    expect_base: path,
                });
            } else if path.extension().is_some_and(|e| e == "wid") {
                cases.push(Case::Run {
                    name: format!("run/{name}"),
                    target: path.clone(),
                    file_mode: true,
                    expect_base: path.with_extension(""),
                });
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join("tests/ui")) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "wid") || path.is_dir() {
                let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                cases.push(Case::Ui { name: format!("ui/{name}"), file: path });
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join("tests/test")) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                let expect = Some(path.with_extension("stdout"));
                cases.push(Case::Test { name: format!("test/{name}"), target: path, expect });
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join("tests/doc")) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "args") {
                let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                cases.push(Case::Doc { name: format!("doc/{name}"), args: path });
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join("tests/query")) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "args") {
                let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                cases.push(Case::Query { name: format!("query/{name}"), args: path });
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join("tests/fmt")) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "wid") {
                let name = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                cases.push(Case::Fmt { name: format!("fmt/{name}"), file: path });
            }
        }
    }
    if let Ok(rd) = std::fs::read_dir(root.join("core")) {
        for entry in rd.flatten() {
            let path = entry.path();
            let has_tests = std::fs::read_dir(&path)
                .is_ok_and(|files| files.flatten().any(|f| f.file_name().to_string_lossy().ends_with("_test.wid")));
            if path.is_dir() && has_tests {
                let name = path.file_name().unwrap_or_default().to_string_lossy().into_owned();
                cases.push(Case::Test { name: format!("core/{name}"), target: path, expect: None });
            }
        }
    }
    cases.retain(|c| name_of(c).contains(filter));
    cases.sort_by(|a, b| name_of(a).cmp(name_of(b)));
    cases
}

fn name_of(c: &Case) -> &str {
    match c {
        Case::Run { name, .. }
        | Case::Ui { name, .. }
        | Case::Test { name, .. }
        | Case::Doc { name, .. }
        | Case::Query { name, .. }
        | Case::Fmt { name, .. } => name,
    }
}

fn compilers() -> Vec<String> {
    if let Ok(list) = std::env::var("WID_TEST_CC") {
        return list.split(',').map(str::trim).filter(|s| !s.is_empty()).map(String::from).collect();
    }
    let mut out = vec!["clang".to_string()];
    for gcc in ["gcc-16", "gcc-15"] {
        if Command::new(gcc).arg("--version").output().is_ok_and(|o| o.status.success()) {
            out.push(gcc.to_string());
            break;
        }
    }
    out
}

/// Applies build flags listed in a `NAME.flags` file (`-debug`,
/// `-no-bounds-check`, `-o:speed`, `-define:NAME=value`, `-target:os_arch`).
/// Returns whether it lists `-json-errors`, which makes a ui case expect the
/// JSON document `wid check -json-errors` prints.
fn apply_flags(opts: &mut Options, path: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(path) else { return false };
    let mut json = false;
    for flag in text.split_whitespace() {
        match flag {
            "-debug" => opts.debug = true,
            "-no-bounds-check" => opts.bounds_checks = false,
            "-json-errors" => json = true,
            other => {
                if let Some(level) = other.strip_prefix("-o:").and_then(wid_driver::OptLevel::parse) {
                    opts.opt = Some(level);
                } else if let Some(define) = other.strip_prefix("-define:") {
                    let (name, value) = define.split_once('=').unwrap_or((define, "true"));
                    opts.defines.insert(name.to_string(), value.to_string());
                } else if let Some(target) = other.strip_prefix("-target:") {
                    let (os, arch) = wid_sema::parse_target(target).expect("a valid -target: flag");
                    opts.target_os = os;
                    opts.target_arch = arch;
                } else {
                    panic!("unknown flag `{other}` in {}", path.display());
                }
            }
        }
    }
    json
}

/// Makes rendered output independent of where the repository lives.
fn normalize(text: &str, root: &Path) -> String {
    let root = format!("{}/", root.display());
    text.replace(&root, "").replace("\r\n", "\n")
}

fn expectation(path: &Path) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn compare(label: &str, path: &Path, actual: &str, bless: bool, failures: &mut Vec<String>) {
    let expected = expectation(path);
    if expected.as_deref() == Some(actual) {
        return;
    }
    if bless {
        if actual.is_empty() && path.extension().is_none_or(|e| e != "stdout") {
            let _ = std::fs::remove_file(path);
        } else {
            std::fs::write(path, actual).expect("write expectation");
        }
        return;
    }
    let expected = expected.unwrap_or_else(|| format!("<missing {}>", path.display()));
    failures.push(format!("{label} differs:\n{}", diff(&expected, actual)));
}

fn diff(expected: &str, actual: &str) -> String {
    let e: Vec<&str> = expected.lines().collect();
    let a: Vec<&str> = actual.lines().collect();
    let mut out = String::new();
    let max = e.len().max(a.len());
    for i in 0..max {
        match (e.get(i), a.get(i)) {
            (Some(x), Some(y)) if x == y => out.push_str(&format!("   {x}\n")),
            (x, y) => {
                if let Some(x) = x {
                    out.push_str(&format!(" - {x}\n"));
                }
                if let Some(y) = y {
                    out.push_str(&format!(" + {y}\n"));
                }
            }
        }
    }
    out
}

/// Compares output that may be empty: a missing file expects none.
fn compare_optional(label: &str, path: &Path, actual: &str, bless: bool, failures: &mut Vec<String>) {
    if !actual.is_empty() || path.exists() {
        if actual.is_empty() && bless {
            let _ = std::fs::remove_file(path);
            return;
        }
        compare(label, path, actual, bless, failures);
    }
}

/// Runs the `wid doc` command in an args file (see the module docs).
fn run_doc(args: &Path, root: &Path, bless: bool) -> Vec<String> {
    let mut failures = Vec::new();
    let text = std::fs::read_to_string(args).expect("read the args file");
    let base = root.join("tests/doc");
    let mut opts = Options::new(".");
    opts.wid_root = Some(root.to_path_buf());
    let mut request = wid_driver::doc::DocRequest { args: Vec::new(), private: false, dir: base.clone() };
    let (mut json, mut json_errors) = (false, false);
    for (i, arg) in text.split_whitespace().enumerate() {
        match arg {
            "-json" => json = true,
            "-json-errors" => json_errors = true,
            "-private" => request.private = true,
            "-file" => opts.file_mode = true,
            _ if i == 0 && arg.starts_with("-in:") => request.dir = base.join(&arg[4..]),
            _ if arg.starts_with('-') => panic!("unknown flag `{arg}` in {}", args.display()),
            _ => request.args.push(arg.to_string()),
        }
    }
    let out = wid_driver::doc::doc(&opts, &request);
    let printed = wid_driver::doc::print(&out, json, json_errors, false);
    compare_optional("stdout", &args.with_extension("stdout"), &normalize(&printed.stdout, root), bless, &mut failures);
    compare_optional("stderr", &args.with_extension("stderr"), &normalize(&printed.stderr, root), bless, &mut failures);
    failures
}

/// Runs the `wid query` command in an args file (see the module docs).
fn run_query(args: &Path, root: &Path, bless: bool) -> Vec<String> {
    let mut failures = Vec::new();
    let text = std::fs::read_to_string(args).expect("read the args file");
    let mut opts = Options::new(".");
    opts.wid_root = Some(root.to_path_buf());
    let (mut positional, mut package) = (Vec::new(), None);
    for arg in text.split_whitespace() {
        match arg {
            "-file" => opts.file_mode = true,
            "-json-errors" => {}
            _ if arg.starts_with("-in:") => package = Some(arg[4..].to_string()),
            _ if arg.starts_with('-') => panic!("unknown flag `{arg}` in {}", args.display()),
            _ => positional.push(arg.to_string()),
        }
    }
    let (stdout, stderr) = match wid_driver::query::Query::parse(&positional) {
        Ok(query) => {
            let request = wid_driver::query::QueryRequest { query, package, dir: root.to_path_buf() };
            let printed = wid_driver::query::print(&wid_driver::query::query(&opts, &request));
            (printed.stdout, printed.stderr)
        }
        // What the CLI prints for a malformed command line.
        Err(message) => (String::new(), format!("error: {message}\nrun `wid help query` for usage\n")),
    };
    compare_optional("stdout", &args.with_extension("stdout"), &normalize(&stdout, root), bless, &mut failures);
    compare_optional("stderr", &args.with_extension("stderr"), &normalize(&stderr, root), bless, &mut failures);
    failures
}

/// Formats a `tests/fmt` file (see the module docs).
fn run_fmt(file: &Path, root: &Path, bless: bool) -> Vec<String> {
    let mut failures = Vec::new();
    let flags = std::fs::read_to_string(file.with_extension("flags")).ok();
    let mut json = false;
    for flag in flags.as_deref().unwrap_or_default().split_whitespace() {
        match flag {
            "-check" => {}
            "-json-errors" => json = true,
            _ => panic!("unknown flag `{flag}` in {}", file.with_extension("flags").display()),
        }
    }
    let mut opts = Options::new(file);
    opts.file_mode = true;
    let out = wid_driver::fmt::fmt(&opts, true);
    let printed = wid_driver::fmt::print(&out, json, false);
    if !out.internal_errors.is_empty() {
        failures.push(format!("formatting changed the syntax tree:\n{}", printed.stderr));
    }
    let formatted = out.files.first().and_then(|f| f.formatted.clone());
    let out_path = file.with_extension("out");
    match &formatted {
        Some(text) => {
            compare("formatted text", &out_path, text, bless, &mut failures);
            let (ast, diags) = wid_syntax::parse_file(wid_diagnostics::FileId(0), text);
            if !diags.is_empty() || wid_syntax::fmt::format(text, &ast) != *text {
                failures.push("formatting the formatted text changes it again".into());
            }
        }
        None if out_path.exists() && bless => {
            let _ = std::fs::remove_file(&out_path);
        }
        None if out_path.exists() => failures.push("the file was not formatted, but it has a `.out`".into()),
        None => {}
    }
    compare_optional("stderr", &file.with_extension("stderr"), &normalize(&printed.stderr, root), bless, &mut failures);
    if flags.is_some() {
        let stdout = normalize(&printed.stdout, root);
        compare_optional("stdout", &file.with_extension("stdout"), &stdout, bless, &mut failures);
    }
    failures
}

fn run_case(case: &Case, root: &Path, ccs: &[String], bless: bool) -> Vec<String> {
    let mut failures = Vec::new();
    match case {
        Case::Doc { args, .. } => return run_doc(args, root, bless),
        Case::Query { args, .. } => return run_query(args, root, bless),
        Case::Fmt { file, .. } => return run_fmt(file, root, bless),
        Case::Ui { file, .. } => {
            let mut opts = Options::new(file);
            opts.file_mode = !file.is_dir();
            opts.wid_root = Some(root.to_path_buf());
            let json = apply_flags(&mut opts, &file.with_extension("flags"));
            let checked = wid_driver::check(&opts);
            let rendered = if json {
                render_json(&checked.diags, &checked.sources) + "\n"
            } else {
                render_all(&checked.diags, &checked.sources, RenderOptions { color: false })
            };
            let rendered = normalize(&rendered, root);
            if checked.diags.is_empty() {
                failures.push("expected diagnostics, but the file checked cleanly".into());
            }
            compare("diagnostics", &file.with_extension("stderr"), &rendered, bless, &mut failures);
        }
        Case::Test { target, expect, .. } => {
            for cc in ccs {
                let mut opts = Options::new(target);
                opts.wid_root = Some(root.to_path_buf());
                opts.cc = Some(cc.clone());
                opts.strict_c = true;
                let _ = apply_flags(&mut opts, &target.with_extension("flags"));
                let run = wid_driver::test(&opts, None);
                if run.checked.diags.has_errors() {
                    let rendered = render_all(&run.checked.diags, &run.checked.sources, RenderOptions { color: false });
                    failures.push(format!("[{cc}] build failed:\n{}", normalize(&rendered, root)));
                    continue;
                }
                let report = normalize(&wid_driver::render_test_report(&run, false), root);
                match expect {
                    Some(path) => compare(&format!("[{cc}] report"), path, &report, bless, &mut failures),
                    None if !run.passed() => failures.push(format!("[{cc}] tests failed:\n{report}")),
                    None => {}
                }
                if bless {
                    return failures;
                }
            }
        }
        Case::Run { target, file_mode, expect_base, name } => {
            for cc in ccs {
                let mut opts = Options::new(target);
                opts.file_mode = *file_mode;
                opts.wid_root = Some(root.to_path_buf());
                opts.cc = Some(cc.clone());
                opts.strict_c = true;
                let _ = apply_flags(&mut opts, &PathBuf::from(format!("{}.flags", expect_base.display())));
                let out_dir = std::env::temp_dir().join(format!("wid-suite-{}", std::process::id()));
                std::fs::create_dir_all(&out_dir).expect("temp dir");
                let exe = out_dir.join(format!("{name}-{cc}").replace(['/', '\\'], "_"));
                opts.out = Some(exe.clone());
                let built = wid_driver::build(&opts);
                if built.exe.is_none() {
                    let rendered =
                        render_all(&built.checked.diags, &built.checked.sources, RenderOptions { color: false });
                    failures.push(format!("[{cc}] build failed:\n{}", normalize(&rendered, root)));
                    continue;
                }
                let output = Command::new(&exe).current_dir(root).output().expect("run test program");
                let _ = std::fs::remove_file(&exe);
                let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
                let stderr = normalize(&String::from_utf8_lossy(&output.stderr), root);
                let code = output.status.code().unwrap_or(-1);
                let stdout_path = PathBuf::from(format!("{}.stdout", expect_base.display()));
                let stderr_path = PathBuf::from(format!("{}.stderr", expect_base.display()));
                let code_path = PathBuf::from(format!("{}.exitcode", expect_base.display()));
                compare(&format!("[{cc}] stdout"), &stdout_path, &stdout, bless, &mut failures);
                if stderr_path.exists() || (bless && !stderr.is_empty()) {
                    compare(&format!("[{cc}] stderr"), &stderr_path, &stderr, bless, &mut failures);
                } else if !stderr.is_empty() {
                    failures.push(format!("[{cc}] unexpected stderr:\n{stderr}"));
                }
                let expected_code: i32 = expectation(&code_path).and_then(|s| s.trim().parse().ok()).unwrap_or(0);
                if code != expected_code {
                    if bless {
                        if code == 0 {
                            let _ = std::fs::remove_file(&code_path);
                        } else {
                            std::fs::write(&code_path, format!("{code}\n")).expect("write exit code");
                        }
                    } else {
                        failures.push(format!("[{cc}] exit code {code}, expected {expected_code}"));
                    }
                }
                if bless {
                    // One compiler defines the expectation; the others must agree.
                    return failures;
                }
            }
        }
    }
    failures
}

/// Every error code that appears in a UI (or `wid doc` or `wid query`)
/// expectation must be documented.
fn check_code_docs(root: &Path) -> (Vec<String>, Vec<String>) {
    let mut errors = Vec::new();
    let mut covered = std::collections::HashSet::new();
    for dir in ["tests/ui", "tests/doc", "tests/query", "tests/fmt"] {
        let Ok(rd) = std::fs::read_dir(root.join(dir)) else { continue };
        for entry in rd.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "stderr") {
                let text = std::fs::read_to_string(&path).unwrap_or_default();
                for info in wid_diagnostics::codes::ALL {
                    if text.contains(&format!("[{}]", info.code)) || text.contains(&format!("\"{}\"", info.code)) {
                        covered.insert(info.code.as_str());
                    }
                }
            }
        }
    }
    let mut missing_tests = Vec::new();
    for info in wid_diagnostics::codes::ALL {
        let code = info.code.as_str();
        let documented = root.join("docs/errors").join(format!("{code}.md")).exists();
        if covered.contains(code) && !documented {
            errors.push(format!("{code} appears in tests/ui but docs/errors/{code}.md is missing"));
        }
        if !covered.contains(code) || !documented {
            missing_tests.push(format!("{code} ({})", info.title));
        }
    }
    (errors, missing_tests)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--list") {
        return;
    }
    let root = repo_root();
    let filter = std::env::var("WID_TEST_FILTER").unwrap_or_default();
    let bless = std::env::var("WID_BLESS").is_ok_and(|v| v == "1");
    let ccs = compilers();
    let mut cases = collect(&root, &filter);
    // libclang is loaded once, before the workers start: loading sets an
    // environment variable. Without it, the `cimport` and `vendor_` cases are skipped.
    if let Err(e) = wid_cimport::libclang() {
        cases.retain(|c| !name_of(c).contains("cimport") && !name_of(c).starts_with("vendor_"));
        eprintln!("skipping cimport cases: {e}");
    }
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<(String, Vec<String>)>> = Mutex::new(Vec::new());
    let workers = std::thread::available_parallelism().map_or(4, |n| n.get()).min(16);
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, Ordering::Relaxed);
                    let Some(case) = cases.get(i) else { break };
                    let failures = run_case(case, &root, &ccs, bless);
                    results.lock().expect("results lock").push((name_of(case).to_string(), failures));
                }
            });
        }
    });
    let mut results = results.into_inner().expect("results lock");
    results.sort();
    let mut failed = 0;
    for (name, failures) in &results {
        if failures.is_empty() {
            println!("ok     {name}");
        } else {
            failed += 1;
            println!("FAILED {name}");
            for f in failures {
                for line in f.lines() {
                    println!("       {line}");
                }
            }
        }
    }
    let (doc_errors, uncovered) = if filter.is_empty() { check_code_docs(&root) } else { (Vec::new(), Vec::new()) };
    for e in &doc_errors {
        println!("FAILED {e}");
    }
    if !uncovered.is_empty() && std::env::var("WID_SHOW_UNCOVERED").is_ok() {
        println!("\ncodes without a documented tests/ui case:");
        for c in &uncovered {
            println!("  {c}");
        }
    }
    println!(
        "\n{} cases, {} passed, {} failed (compilers: {}){}",
        results.len(),
        results.len() - failed,
        failed,
        ccs.join(", "),
        if bless { ", blessed" } else { "" }
    );
    if failed > 0 || !doc_errors.is_empty() {
        std::process::exit(1);
    }
}

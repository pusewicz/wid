//! The `wid` command-line tool. Commands and flags follow Odin's CLI:
//! `wid run .`, `wid build . -o:speed -out:game`.

mod args;

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;

use wid_diagnostics::{Diagnostics, RenderOptions, SourceMap, render_all_with, render_json, to_json};
use wid_driver::Options;

use args::{Command, Parsed};

fn main() -> ExitCode {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    let parsed = match args::parse(&argv) {
        Ok(p) => p,
        Err(message) => {
            eprintln!("{}", error_line(&message));
            eprintln!("run `wid help` for usage");
            return ExitCode::from(2);
        }
    };
    match parsed.command {
        Command::Help => {
            print!("{}", args::usage(parsed.help_topic.as_deref()));
            ExitCode::SUCCESS
        }
        Command::Version => {
            println!("wid {}", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Command::Explain => explain(&parsed),
        Command::Cimport => cimport(&parsed),
        Command::Check => check(&parsed),
        Command::Build => build(&parsed),
        Command::Run => run(&parsed),
        Command::Test => test(&parsed),
        Command::Doc => doc(&parsed),
        Command::Fmt => fmt(&parsed),
        Command::Query => query(&parsed),
    }
}

fn color_stderr() -> bool {
    std::io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none()
}

fn error_line(message: &str) -> String {
    if color_stderr() { format!("\x1b[1;31merror\x1b[0m: {message}") } else { format!("error: {message}") }
}

/// How the summary line of `build`, `run`, `check` and `test` starts.
const COMPILE: &str = "could not compile due to";

/// Prints diagnostics in the requested format and returns true on errors.
/// With errors, the summary line after them is `failure` and the counts.
fn report(diags: &Diagnostics, sources: &SourceMap, parsed: &Parsed, failure: &str) -> bool {
    if parsed.json_errors {
        println!("{}", render_json(diags, sources));
    } else if !diags.is_empty() {
        eprint!("{}", render_all_with(diags, sources, RenderOptions { color: color_stderr() }, failure));
    }
    diags.has_errors()
}

fn options(parsed: &Parsed) -> Options {
    // A package defaults to the one in `.`; `-file` needs a file named.
    let default = if parsed.file_mode { PathBuf::new() } else { PathBuf::from(".") };
    let mut opts = Options::new(parsed.target.clone().unwrap_or(default));
    opts.file_mode = parsed.file_mode;
    opts.command = args::command_name(parsed.command).to_string();
    opts.out = parsed.out.clone();
    opts.opt = parsed.opt;
    opts.debug = parsed.debug;
    opts.keep_c = parsed.keep_c;
    opts.cc = parsed.cc.clone();
    opts.defines = parsed.defines.clone();
    if let Some((os, arch)) = &parsed.target_os_arch {
        opts.target_os = os.clone();
        opts.target_arch = arch.clone();
    }
    opts.collections = parsed.collections.clone();
    opts.bounds_checks = !parsed.no_bounds_check;
    opts.sanitize = parsed.sanitize.clone();
    opts
}

/// `wid cimport --dump <header>`: prints the Wid view of a C header.
fn cimport(parsed: &Parsed) -> ExitCode {
    let Some(header) = parsed.target.as_ref().map(|t| t.to_string_lossy().into_owned()) else {
        eprintln!("{}", error_line("`wid cimport` needs a header, like `wid cimport --dump raylib.h`"));
        return ExitCode::from(2);
    };
    let request = wid_driver::DumpRequest {
        header,
        strip_prefixes: parsed.strip_prefixes.clone(),
        defines: parsed.raw_defines.clone(),
        include_dirs: parsed.include_dirs.clone(),
        pkg_config: parsed.pkg_config.clone(),
    };
    let (sources, result) = wid_driver::cimport_dump(&request);
    match result {
        Ok(source) => {
            print!("{source}");
            ExitCode::SUCCESS
        }
        Err(diags) => {
            report(&diags, &sources, parsed, "could not import the header due to");
            ExitCode::FAILURE
        }
    }
}

/// `wid doc`: prints the documentation on stdout (as JSON with `-json`) and
/// the diagnostics on stderr (as JSON with `-json-errors`). The page is
/// shown even when the package has errors; the exit status is 1 then, and
/// when the request fails.
fn doc(parsed: &Parsed) -> ExitCode {
    let opts = options(parsed);
    let request =
        wid_driver::doc::DocRequest { args: parsed.doc_args.clone(), private: parsed.private, dir: PathBuf::new() };
    let out = wid_driver::doc::doc(&opts, &request);
    let printed = wid_driver::doc::print(&out, parsed.json, parsed.json_errors, color_stderr());
    eprint!("{}", printed.stderr);
    print!("{}", printed.stdout);
    if printed.success { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

/// `wid fmt`: rewrites the files that aren't in the canonical style (with
/// `-check`, changes nothing) and lists them on stdout; parse errors go to
/// stderr. With `-json-errors`, stdout holds one JSON document instead. The
/// exit status is 1 when a file has errors, and with `-check` when a file
/// would change.
fn fmt(parsed: &Parsed) -> ExitCode {
    let opts = options(parsed);
    let out = wid_driver::fmt::fmt(&opts, parsed.check);
    let printed = wid_driver::fmt::print(&out, parsed.json_errors, color_stderr());
    eprint!("{}", printed.stderr);
    print!("{}", printed.stdout);
    if printed.success { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

/// `wid query`: prints the answer as one JSON document on stdout and the
/// diagnostics as JSON on stderr. A package with errors is still answered;
/// the exit status is 1 then, and when the request fails. A malformed
/// command line is a usage error (status 2), as for every command.
fn query(parsed: &Parsed) -> ExitCode {
    let query = match wid_driver::query::Query::parse(&parsed.query_args) {
        Ok(query) => query,
        Err(message) => {
            eprintln!("{}", error_line(&message));
            eprintln!("run `wid help query` for usage");
            return ExitCode::from(2);
        }
    };
    let opts = options(parsed);
    let request = wid_driver::query::QueryRequest { query, package: parsed.query_in.clone(), dir: PathBuf::new() };
    let out = wid_driver::query::query(&opts, &request);
    let printed = wid_driver::query::print(&out);
    eprint!("{}", printed.stderr);
    print!("{}", printed.stdout);
    if printed.success { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

fn explain(parsed: &Parsed) -> ExitCode {
    let Some(code) = parsed.target.as_ref().map(|t| t.to_string_lossy().into_owned()) else {
        println!("Error codes (run `wid explain <CODE>` for details):\n");
        for info in wid_diagnostics::codes::ALL {
            println!("  {}  {}", info.code, info.title);
        }
        return ExitCode::SUCCESS;
    };
    match wid_diagnostics::explain(&code) {
        Some(text) => {
            print!("{text}");
            ExitCode::SUCCESS
        }
        None => {
            match wid_diagnostics::codes::lookup(&code) {
                Some(info) => {
                    eprintln!("{}", error_line(&format!("{} ({}) has no long explanation yet", info.code, info.title)))
                }
                None => eprintln!(
                    "{}",
                    error_line(&format!("`{code}` is not a Wid error code; run `wid explain` to list them"))
                ),
            }
            ExitCode::from(1)
        }
    }
}

fn check(parsed: &Parsed) -> ExitCode {
    let mut opts = options(parsed);
    opts.check_all_packages = false;
    let checked = wid_driver::check(&opts);
    if report(&checked.diags, &checked.sources, parsed, COMPILE) { ExitCode::from(1) } else { ExitCode::SUCCESS }
}

fn build(parsed: &Parsed) -> ExitCode {
    let opts = options(parsed);
    let built = wid_driver::build(&opts);
    if report(&built.checked.diags, &built.checked.sources, parsed, COMPILE) {
        return ExitCode::from(1);
    }
    ExitCode::SUCCESS
}

fn test(parsed: &Parsed) -> ExitCode {
    let opts = options(parsed);
    let run = wid_driver::test(&opts, parsed.filter.as_deref());
    if parsed.json_errors {
        println!("{}", test_json(&run));
        return if run.passed() { ExitCode::SUCCESS } else { ExitCode::from(1) };
    }
    if report(&run.checked.diags, &run.checked.sources, parsed, COMPILE) {
        return ExitCode::from(1);
    }
    let color = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    print!("{}", wid_driver::render_test_report(&run, color));
    if run.passed() { ExitCode::SUCCESS } else { ExitCode::from(1) }
}

/// The JSON document `wid test -json-errors` prints: build diagnostics,
/// every test's status and output, and each failure as a diagnostic.
fn test_json(run: &wid_driver::TestRun) -> String {
    let sources = &run.checked.sources;
    let tests: Vec<serde_json::Value> = run
        .results
        .iter()
        .map(|r| {
            serde_json::json!({
                "name": r.name,
                "status": format!("{:?}", r.status).to_lowercase(),
                "output": r.output,
                "failures": r.failures.iter().map(|d| to_json(d, sources)).collect::<Vec<_>>(),
            })
        })
        .collect();
    let passed = run.results.iter().filter(|r| r.status == wid_driver::TestStatus::Passed).count();
    let doc = serde_json::json!({
        "errors": run.checked.diags.error_count(),
        "diagnostics": run.checked.diags.iter().map(|d| to_json(d, sources)).collect::<Vec<_>>(),
        "passed": passed,
        "failed": run.results.len() - passed,
        "filtered_out": run.filtered_out,
        "tests": tests,
    });
    serde_json::to_string_pretty(&doc).unwrap_or_default()
}

fn run(parsed: &Parsed) -> ExitCode {
    let mut opts = options(parsed);
    let temp_out = opts.out.is_none();
    if temp_out {
        let name = wid_driver::default_output(&opts);
        let dir = std::env::temp_dir().join(format!("wid-run-{}", std::process::id()));
        if std::fs::create_dir_all(&dir).is_err() {
            eprintln!("{}", error_line("cannot create a temporary directory"));
            return ExitCode::from(1);
        }
        opts.out = Some(dir.join(name));
    }
    let built = wid_driver::build(&opts);
    if report(&built.checked.diags, &built.checked.sources, parsed, COMPILE) {
        return ExitCode::from(1);
    }
    let Some(exe) = built.exe else { return ExitCode::from(1) };
    // A bare relative name would be looked up on PATH; run the file we built.
    let exe = if exe.is_relative() { std::env::current_dir().map(|d| d.join(&exe)).unwrap_or(exe) } else { exe };
    let status = std::process::Command::new(&exe).args(&parsed.program_args).status();
    if temp_out {
        let _ = std::fs::remove_file(&exe);
        if let Some(dir) = exe.parent() {
            let _ = std::fs::remove_dir(dir);
        }
    }
    match status {
        Ok(s) => match s.code() {
            Some(code) => ExitCode::from(code.clamp(0, 255) as u8),
            None => {
                #[cfg(unix)]
                {
                    use std::os::unix::process::ExitStatusExt;
                    if let Some(sig) = s.signal() {
                        return ExitCode::from((128 + sig).clamp(0, 255) as u8);
                    }
                }
                ExitCode::from(1)
            }
        },
        Err(e) => {
            eprintln!("{}", error_line(&format!("cannot run {}: {e}", exe.display())));
            ExitCode::from(1)
        }
    }
}

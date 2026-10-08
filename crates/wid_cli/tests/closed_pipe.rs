//! A closed stdout or stderr, as in `wid query outline | head -c 10`, is not
//! an error: `wid` drops that output without panicking and exits with the
//! status the command has anyway (SPEC "Toolchain and CLI").

use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Output, Stdio};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root exists")
}

/// `wid ARGS`, run from the repository root.
fn wid(args: &str) -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_wid"));
    cmd.args(args.split_whitespace()).current_dir(root()).env("WID_ROOT", root()).env("NO_COLOR", "1");
    cmd
}

/// A pipe whose reader is already closed, so the first write to it fails.
fn closed_pipe() -> Stdio {
    let (reader, writer) = std::io::pipe().expect("create a pipe");
    drop(reader);
    writer.into()
}

/// A file with an error, for the commands that report one.
fn broken_file(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wid-closed-pipe-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create a scratch directory");
    let file = dir.join("broken.wid");
    std::fs::write(&file, "def main\n  puts nope\nend\n").expect("write broken.wid");
    file
}

/// Asserts that `wid ARGS` exited with `status`, without the panic that
/// `println!` gives on a closed pipe (status 101 and a message on stderr).
fn assert_quiet(args: &str, output: &Output, status: i32) {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(!stderr.contains("panicked"), "`wid {args}` panicked:\n{stderr}");
    assert_eq!(output.status.code(), Some(status), "`wid {args}` exit status; stderr:\n{stderr}");
}

/// Runs `wid ARGS` with stdout closed before it starts and stderr captured.
fn stdout_closed(args: &str, status: i32) -> String {
    let output = wid(args).stdout(closed_pipe()).stderr(Stdio::piped()).output().expect("run wid");
    assert_quiet(args, &output, status);
    String::from_utf8_lossy(&output.stderr).into_owned()
}

/// Runs `wid ARGS` with stderr closed before it starts and stdout captured.
fn stderr_closed(args: &str, status: i32) -> String {
    let output = wid(args).stdout(Stdio::piped()).stderr(closed_pipe()).output().expect("run wid");
    assert_quiet(args, &output, status);
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn reader_that_stops_early() {
    // About 70 KB, more than a pipe holds, so `wid` is still writing when the
    // reader goes away after 10 bytes, like `head -c 10`.
    let args = "query outline -in:core:builtin";
    let mut child = wid(args).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().expect("run wid");
    let mut stdout = child.stdout.take().expect("stdout is piped");
    let mut first = [0u8; 10];
    stdout.read_exact(&mut first).expect("read the first bytes");
    drop(stdout);
    let output = child.wait_with_output().expect("wait for wid");
    assert_eq!(&first, b"{\n  \"packa");
    assert_quiet(args, &output, 0);
    assert_eq!(String::from_utf8_lossy(&output.stderr), "");
}

#[test]
fn every_command_with_stdout_closed() {
    for args in [
        "help",
        "help query",
        "help fmt",
        "version",
        "explain",
        "explain E0101",
        "doc core:fmt",
        "doc core:fmt -json",
        "doc core:strings Builder",
        "query outline -in:core:fmt",
        "query def -in:core:fmt int",
    ] {
        assert_eq!(stdout_closed(args, 0), "", "`wid {args}` wrote to stderr");
    }
    let broken = broken_file("stdout");
    let file = broken.display();
    // The diagnostics go to stdout as JSON, and the status says there were errors.
    assert_eq!(stdout_closed(&format!("check {file} -file -json-errors"), 1), "");
    assert_eq!(stdout_closed(&format!("build {file} -file -json-errors"), 1), "");
    // The report of failing tests.
    stdout_closed("test tests/test/sample -cc:clang", 1);
    // The files `wid fmt -check` would change, and its JSON.
    assert_eq!(stdout_closed("fmt tests/fmt/spacing.wid -file -check", 1), "");
    assert_eq!(stdout_closed("fmt tests/fmt/parse_error.wid -file -check -json-errors", 1), "");
    let _ = std::fs::remove_dir_all(broken.parent().expect("the scratch directory"));
}

#[test]
fn every_command_with_stderr_closed() {
    // Usage errors, for `wid` and for `wid query`.
    stderr_closed("build -bogus", 2);
    stderr_closed("query nothing", 2);
    stderr_closed("explain E9999", 1);
    stderr_closed("doc core:nope", 1);
    stderr_closed("query outline -in:core:nope", 1);
    let broken = broken_file("stderr");
    let file = broken.display();
    stderr_closed(&format!("check {file} -file"), 1);
    stderr_closed("fmt tests/fmt/parse_error.wid -file -check", 1);
    stderr_closed(&format!("run {file} -file"), 1);
    stderr_closed(&format!("test {} -cc:clang", broken.parent().expect("the scratch directory").display()), 1);
    // The page still goes to stdout when the diagnostics can't.
    let page = stderr_closed(&format!("doc {file} -file"), 1);
    assert!(page.contains("METHODS"), "the page is printed:\n{page}");
    let _ = std::fs::remove_dir_all(broken.parent().expect("the scratch directory"));
}

/// `wid run` exits with the program's status, and a program writing to a
/// closed pipe gets `SIGPIPE` on Unix: 128 + 13.
#[cfg(unix)]
#[test]
fn run_reports_the_program_status() {
    let dir = std::env::temp_dir().join(format!("wid-closed-pipe-run-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create a scratch directory");
    let file = dir.join("loud.wid");
    std::fs::write(&file, "def main\n  100000.times do |i|\n    puts i\n  end\nend\n").expect("write loud.wid");
    let args = format!("run {} -file -cc:clang", file.display());
    let output = wid(&args).stdout(closed_pipe()).stderr(Stdio::piped()).output().expect("run wid");
    assert_quiet(&args, &output, 141);
    let _ = std::fs::remove_dir_all(&dir);
}

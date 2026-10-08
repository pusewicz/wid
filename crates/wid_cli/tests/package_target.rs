//! The errors for a package target that isn't a package directory, from
//! `wid check`, `build`, `run` and `test` without `-file`: they point into
//! the command line, like the `-file` errors (`file_flag.rs`). And a flag
//! of another command, which is a usage error.

use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch directory holding a package `game/`, `hello.wid`, `README.md`,
/// an empty `empty/`, `docs/` with a text file and a subdirectory,
/// `apps/one/` (a package one level down) and `checks/` with only tests.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wid-package-target-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["game", "empty", "docs/notes", "apps/one", "checks"] {
        std::fs::create_dir_all(dir.join(sub)).expect("create a scratch directory");
    }
    let main = "def main\n  puts 1\nend\n";
    for file in ["game/main.wid", "hello.wid", "apps/one/main.wid"] {
        std::fs::write(dir.join(file), main).expect("write a scratch file");
    }
    std::fs::write(dir.join("README.md"), "# Notes\n").expect("write README.md");
    std::fs::write(dir.join("docs/plan.txt"), "plan\n").expect("write docs/plan.txt");
    std::fs::write(dir.join("checks/a_test.wid"), "@[test]\ndef works(t: ^Testing)\nend\n").expect("write a test");
    dir
}

/// Runs `wid ARGS` in `dir` and returns its exit status and stderr.
fn run(dir: &Path, args: &str) -> (Option<i32>, String) {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Command::new(env!("CARGO_BIN_EXE_wid"))
        .args(args.split_whitespace())
        .current_dir(dir)
        .env("WID_ROOT", root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run wid");
    (out.status.code(), String::from_utf8_lossy(&out.stderr).into_owned())
}

/// The stderr of `wid ARGS` in `dir`, which must fail with 1.
fn stderr(dir: &Path, args: &str) -> String {
    let (status, err) = run(dir, args);
    assert_eq!(status, Some(1), "`wid {args}` exit status");
    err
}

#[test]
fn missing_directory() {
    let dir = scratch("missing");
    assert_eq!(
        stderr(&dir, "check gmae"),
        "\
error[E0206]: directory `gmae` does not exist
 --> command line:1:11
  |
1 | wid check gmae
  |           ^^^^ no directory with this path
help: a similar package directory exists: `game`
  | wid check game
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "run hello"),
        "\
error[E0206]: directory `hello` does not exist
 --> command line:1:9
  |
1 | wid run hello
  |         ^^^^^ no directory with this path
help: run the file `hello.wid` on its own with `-file`
  | wid run hello.wid -file
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "check nothere"),
        "\
error[E0206]: directory `nothere` does not exist
 --> command line:1:11
  |
1 | wid check nothere
  |           ^^^^^^^ no directory with this path
help: name a package directory, or a `.wid` file with `-file`
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn directory_without_wid_files() {
    let dir = scratch("empty");
    assert_eq!(
        stderr(&dir, "check empty"),
        "\
error[E0206]: `empty` contains no `.wid` files
 --> command line:1:11
  |
1 | wid check empty
  |           ^^^^^ this directory holds no package
  = note: it is empty
help: name a package directory, or a `.wid` file with `-file`
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "build docs"),
        "\
error[E0206]: `docs` contains no `.wid` files
 --> command line:1:11
  |
1 | wid build docs
  |           ^^^^ this directory holds no package
  = note: it holds `notes/` and `plan.txt`
help: name a package directory, or a `.wid` file with `-file`
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "check apps"),
        "\
error[E0206]: `apps` contains no `.wid` files
 --> command line:1:11
  |
1 | wid check apps
  |           ^^^^ this directory holds no package
help: the package in it is `apps/one`
  | wid check apps/one
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    // The default target, `.`, is shown as if it were written.
    assert_eq!(
        stderr(&dir.join("empty"), "check"),
        "\
error[E0206]: `.` contains no `.wid` files
 --> command line:1:11
  |
1 | wid check .
  |           ^ this directory holds no package
  = note: it is empty
help: name a package directory, or a `.wid` file with `-file`
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "build checks"),
        "\
error[E0206]: `checks` holds only tests
 --> command line:1:11
  |
1 | wid build checks
  |           ^^^^^^ every `.wid` file here ends in `_test.wid`
  = note: `_test.wid` files belong to the package's tests, which only `wid test` reads
help: run its tests with `wid test checks`
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn file_target() {
    let dir = scratch("file");
    assert_eq!(
        stderr(&dir, "check hello.wid"),
        "\
error[E0206]: `hello.wid` is a file; Wid builds packages (directories)
 --> command line:1:11
  |
1 | wid check hello.wid
  |           ^^^^^^^^^ a package is a directory of `.wid` files
help: check the file on its own with `-file`
  | wid check hello.wid -file
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "check README.md"),
        "\
error[E0206]: `README.md` is neither a `.wid` file nor a package directory
 --> command line:1:11
  |
1 | wid check README.md
  |           ^^^^^^^^^ a package is a directory of `.wid` files
  = note: the `.wid` files here are `hello.wid`
help: to check a single `.wid` file, name it with `-file`, like `wid check hello.wid -file`
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "build docs/plan.txt"),
        "\
error[E0206]: `docs/plan.txt` is neither a `.wid` file nor a package directory
 --> command line:1:11
  |
1 | wid build docs/plan.txt
  |           ^^^^^^^^^^^^^ a package is a directory of `.wid` files
help: to build a single `.wid` file, name it with `-file`, like `wid build docs/main.wid -file`
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flags_of_other_commands_are_usage_errors() {
    let dir = scratch("flags");
    for (args, message) in [
        (
            "explain -debug E0206",
            "`-debug` doesn't apply to `wid explain`; `wid build`, `wid run` and `wid test` take it",
        ),
        ("check -filter:x .", "`-filter` doesn't apply to `wid check`; only `wid test` takes it"),
        ("doc -out:x core:fmt", "`-out` doesn't apply to `wid doc`; `wid build`, `wid run` and `wid test` take it"),
        (
            "check main.wid -file -out:foo -keep-c -o:speed",
            "`-out` doesn't apply to `wid check`; `wid build`, `wid run` and `wid test` take it",
        ),
    ] {
        let (status, err) = run(&dir, args);
        assert_eq!(status, Some(2), "`wid {args}` exit status");
        assert_eq!(err, format!("error: {message}\nrun `wid help` for usage\n"), "`wid {args}`");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

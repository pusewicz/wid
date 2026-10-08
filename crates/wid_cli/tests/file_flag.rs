//! The errors for `-file` without a file, with a missing one or with a
//! directory, from `wid check`, `build` and `run`: they point into the
//! command line, like `wid doc`'s (whose cases are in `tests/doc`).

use std::path::{Path, PathBuf};
use std::process::Command;

/// A scratch directory holding `hello.wid`, `src/app.wid` (a package) and
/// an empty `none/`.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wid-file-flag-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).expect("create the scratch package");
    std::fs::create_dir_all(dir.join("none")).expect("create an empty directory");
    std::fs::write(dir.join("hello.wid"), "def main\n  puts 1\nend\n").expect("write hello.wid");
    std::fs::write(dir.join("src/app.wid"), "def main\n  puts 2\nend\n").expect("write src/app.wid");
    dir
}

/// Runs `wid ARGS` in `dir` and returns its stderr; it must fail with 1.
fn stderr(dir: &Path, args: &str) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let out = Command::new(env!("CARGO_BIN_EXE_wid"))
        .args(args.split_whitespace())
        .current_dir(dir)
        .env("WID_ROOT", root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run wid");
    assert_eq!(out.status.code(), Some(1), "`wid {args}` exit status");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn no_file_named() {
    let dir = scratch("none");
    assert_eq!(
        stderr(&dir.join("none"), "check -file"),
        "\
error[E0206]: `-file` needs a `.wid` file to read
 --> command line:1:11
  |
1 | wid check -file
  |           ^^^^^ no file is named
  = note: with `-file`, the package is a single `.wid` file instead of a directory, so the file must be named
help: name the file, like `wid check main.wid -file`
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "build -file"),
        "\
error[E0206]: `-file` needs a `.wid` file to read
 --> command line:1:11
  |
1 | wid build -file
  |           ^^^^^ no file is named
  = note: with `-file`, the package is a single `.wid` file instead of a directory, so the file must be named
help: build `hello.wid`, the only `.wid` file here
  | wid build hello.wid -file
help: or drop `-file` to build the package in `.`
  | wid build
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn missing_file() {
    let dir = scratch("missing");
    assert_eq!(
        stderr(&dir, "check helo.wid -file"),
        "\
error[E0206]: file `helo.wid` not found
 --> command line:1:11
  |
1 | wid check helo.wid -file
  |           ^^^^^^^^ no file with this path
help: a similar file exists: `hello.wid`
  | wid check hello.wid -file
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "run src/app -file"),
        "\
error[E0206]: file `src/app` not found
 --> command line:1:9
  |
1 | wid run src/app -file
  |         ^^^^^^^ no file with this path
help: a similar file exists: `src/app.wid`
  | wid run src/app.wid -file
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "check nothere.wid -file"),
        "\
error[E0206]: file `nothere.wid` not found
 --> command line:1:11
  |
1 | wid check nothere.wid -file
  |           ^^^^^^^^^^^ no file with this path
  = note: the `.wid` files here are `hello.wid`
help: name an existing `.wid` file
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn directory() {
    let dir = scratch("directory");
    assert_eq!(
        stderr(&dir, "check src -file"),
        "\
error[E0206]: `src` is a directory, not a file
 --> command line:1:11
  |
1 | wid check src -file
  |           ^^^ `-file` checks a single `.wid` file
help: drop `-file` to check the package in `src`
  | wid check src
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    assert_eq!(
        stderr(&dir, "build none -file"),
        "\
error[E0206]: `none` is a directory, not a file
 --> command line:1:11
  |
1 | wid build none -file
  |           ^^^^ `-file` builds a single `.wid` file
  = note: `none` holds no `.wid` files
help: name a `.wid` file instead
  = see `wid explain E0206`

error: could not compile due to 1 error
"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

//! The flags for building C, through the `wid` binary (#142): `-o:` levels,
//! `-collection:`, and `-sanitize:`, whose value is checked when the
//! command line is parsed (E0710) and which builds and runs a clean program
//! with each sanitizer the C compiler supports, skipping one it doesn't, as
//! `tests/vendor` skips missing libraries.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A scratch directory holding `hello.wid` and `app/main.wid`, which
/// imports `mylib:geo` from `libs/geo/`.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wid-build-flags-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["app", "libs/geo"] {
        std::fs::create_dir_all(dir.join(sub)).expect("create a scratch directory");
    }
    let hello =
        "def main\n  xs = [dynamic]Int.new\n  defer free(xs)\n  xs << 40\n  xs << 2\n  puts xs[0] + xs[1]\nend\n";
    std::fs::write(dir.join("hello.wid"), hello).expect("write hello.wid");
    std::fs::write(dir.join("libs/geo/geo.wid"), "def twice(x: Int) -> Int = x * 2\n").expect("write geo.wid");
    let app = "import \"mylib:geo\"\n\ndef main\n  puts geo.twice(21)\nend\n";
    std::fs::write(dir.join("app/main.wid"), app).expect("write app/main.wid");
    dir
}

/// Runs `wid ARGS` in `dir`.
fn wid(dir: &Path, args: &str) -> Output {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    Command::new(env!("CARGO_BIN_EXE_wid"))
        .args(args.split_whitespace())
        .current_dir(dir)
        .env("WID_ROOT", root)
        .env("NO_COLOR", "1")
        .output()
        .expect("run wid")
}

/// The stderr of `wid ARGS` in `dir`, which must be a usage error (2).
fn usage_error(dir: &Path, args: &str) -> String {
    let out = wid(dir, args);
    assert_eq!(out.status.code(), Some(2), "`wid {args}` exit status");
    String::from_utf8_lossy(&out.stderr).into_owned()
}

/// Checks that `wid ARGS` succeeds and prints `stdout`.
fn runs(dir: &Path, args: &str, stdout: &str) {
    let out = wid(dir, args);
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "`wid {args}` failed:\n{err}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), stdout, "`wid {args}`");
}

/// The C compilers to build with: `WID_TEST_CC`, or clang plus the newest
/// gcc.
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

/// Whether `cc` builds and runs a C program with `-fsanitize=NAME`: the
/// sanitizer's runtime library is installed.
fn sanitizer_works(dir: &Path, cc: &str, name: &str) -> bool {
    let c = dir.join("probe.c");
    let exe = dir.join(format!("probe-{name}"));
    std::fs::write(&c, "int main(void) { return 0; }\n").expect("write the probe");
    let built = Command::new(cc)
        .arg(format!("-fsanitize={name}"))
        .arg(&c)
        .arg("-o")
        .arg(&exe)
        .output()
        .is_ok_and(|o| o.status.success());
    built && Command::new(&exe).output().is_ok_and(|o| o.status.success())
}

#[test]
fn unknown_sanitizers_are_usage_errors() {
    let dir = scratch("unknown");
    assert_eq!(
        usage_error(&dir, "build hello.wid -file -sanitize:adress"),
        "\
error[E0710]: unknown sanitizer `adress`
 --> command line:1:37
  |
1 | wid build hello.wid -file -sanitize:adress
  |                                     ^^^^^^ not a sanitizer Wid supports
  = note: Wid supports `address` (AddressSanitizer) and `undefined` (UndefinedBehaviorSanitizer)
help: a similar sanitizer exists: `address`
  | wid build hello.wid -file -sanitize:address
  = see `wid explain E0710`

run `wid help build` for usage
"
    );
    assert_eq!(
        usage_error(&dir, "run hello.wid -file -sanitize:thread -sanitize:ubsan"),
        "\
error[E0710]: unknown sanitizer `thread`
 --> command line:1:35
  |
1 | wid run hello.wid -file -sanitize:thread -sanitize:ubsan
  |                                   ^^^^^^ not a sanitizer Wid supports
  = note: Wid supports `address` (AddressSanitizer) and `undefined` (UndefinedBehaviorSanitizer)
help: name one of them: `-sanitize:address` or `-sanitize:undefined`
  = see `wid explain E0710`

error[E0710]: unknown sanitizer `ubsan`
 --> command line:1:52
  |
1 | wid run hello.wid -file -sanitize:thread -sanitize:ubsan
  |                                                    ^^^^^ not a sanitizer Wid supports
  = note: Wid supports `address` (AddressSanitizer) and `undefined` (UndefinedBehaviorSanitizer)
help: Wid calls it `undefined`
  | wid run hello.wid -file -sanitize:thread -sanitize:undefined
  = see `wid explain E0710`

run `wid help run` for usage
"
    );
    assert_eq!(
        usage_error(&dir, "test . -sanitize:address,undefined"),
        "\
error[E0710]: `-sanitize:` takes one sanitizer, not a list
 --> command line:1:12
  |
1 | wid test . -sanitize:address,undefined
  |            ^^^^^^^^^^^^^^^^^^^^^^^^^^^ 2 sanitizers in one flag
  = note: each `-sanitize:` turns on one sanitizer; repeat the flag to combine them
help: give each sanitizer its own flag
  | wid test . -sanitize:address -sanitize:undefined
  = see `wid explain E0710`

run `wid help test` for usage
"
    );
    assert_eq!(
        usage_error(&dir, "build hello.wid -file -sanitize:"),
        "\
error[E0710]: `-sanitize:` names no sanitizer
 --> command line:1:37
  |
1 | wid build hello.wid -file -sanitize:
  |                                     ^ a sanitizer's name goes here
  = note: Wid supports `address` (AddressSanitizer) and `undefined` (UndefinedBehaviorSanitizer)
help: turn on AddressSanitizer
  | wid build hello.wid -file -sanitize:address
  = see `wid explain E0710`

run `wid help build` for usage
"
    );
    // Arguments after `--` belong to the program.
    let out = wid(&dir, "run hello.wid -file -sanitize:bogus -- -sanitize:bogus");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&out.stderr).matches("error[E0710]").count(), 1);
    // Nothing is built, so no build directory is left behind.
    assert!(!dir.join("hello").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_sanitizers_as_json() {
    let dir = scratch("json");
    let out = wid(&dir, "build hello.wid -file -sanitize:adress -json-errors");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(String::from_utf8_lossy(&out.stderr), "");
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("a JSON document");
    let diag = &doc["diagnostics"][0];
    assert_eq!(diag["code"], "E0710");
    assert_eq!(diag["file"], "command line");
    assert_eq!(diag["column"], 37);
    let edit = &diag["helps"][0]["edits"][0];
    assert_eq!(edit["replacement"], "address");
    assert_eq!(doc["errors"], 1);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn sanitizers_build_and_run_a_clean_program() {
    let dir = scratch("sanitize");
    let mut ran = 0;
    for cc in compilers() {
        for name in ["address", "undefined"] {
            if !sanitizer_works(&dir, &cc, name) {
                eprintln!("skipping -sanitize:{name} with {cc}: it can't build and run a C program with it");
                continue;
            }
            runs(&dir, &format!("run hello.wid -file -sanitize:{name} -cc:{cc}"), "42\n");
            runs(&dir, &format!("run hello.wid -file -sanitize:{name} -debug -cc:{cc}"), "42\n");
            ran += 1;
        }
        if sanitizer_works(&dir, &cc, "address") && sanitizer_works(&dir, &cc, "undefined") {
            runs(&dir, &format!("run hello.wid -file -sanitize:address -sanitize:undefined -cc:{cc}"), "42\n");
        }
    }
    eprintln!("ran {ran} sanitizer builds");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn optimization_levels() {
    let dir = scratch("levels");
    for cc in compilers() {
        for level in ["none", "minimal", "size", "speed", "aggressive"] {
            runs(&dir, &format!("run hello.wid -file -o:{level} -cc:{cc}"), "42\n");
        }
        runs(&dir, &format!("run hello.wid -file -debug -o:speed -cc:{cc}"), "42\n");
    }
    let out = wid(&dir, "build hello.wid -file -o:sped");
    assert_eq!(out.status.code(), Some(2));
    assert_eq!(
        String::from_utf8_lossy(&out.stderr),
        "error: unknown optimization level `sped`; use none, minimal, size, speed or aggressive\n\
         run `wid help` for usage\n"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn collections() {
    let dir = scratch("collections");
    let cc = compilers().remove(0);
    runs(&dir, &format!("run app -collection:mylib=libs -cc:{cc}"), "42\n");
    // The package keeps its collection path when it is the one queried.
    let out = wid(&dir, "query outline -in:mylib:geo -collection:mylib=libs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).expect("a JSON document");
    assert_eq!(doc["package"]["path"], "mylib:geo");
    assert_eq!(doc["results"][0]["package"], "mylib:geo");
    // Without the flag, the import names an unknown collection.
    let out = wid(&dir, "check app");
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown collection `mylib`"));
    let _ = std::fs::remove_dir_all(&dir);
}

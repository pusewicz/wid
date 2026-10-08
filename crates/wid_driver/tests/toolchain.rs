//! How a build reports the C compiler it runs, using fake compilers: shell
//! scripts that print what an old clang or gcc prints.
#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use wid_diagnostics::{RenderOptions, render_all};
use wid_driver::Options;

/// A fake compiler: `--version` prints `version` (or fails without one), and
/// anything else prints `stderr` and fails.
fn fake_cc(dir: &Path, name: &str, version: Option<&str>, stderr: &str) -> PathBuf {
    let path = dir.join(name);
    let version = match version {
        Some(v) => format!("cat <<'EOF'\n{v}\nEOF\nexit 0"),
        None => "exit 1".to_string(),
    };
    let script = format!(
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then\n{version}\nfi\ncat >&2 <<'EOF'\n{stderr}\nEOF\nexit 1\n"
    );
    std::fs::write(&path, script).expect("write the fake compiler");
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
    path
}

/// Builds `main.wid` in `dir` with `cc`, returning the rendered diagnostics
/// with the compiler's path shown as `CC`.
fn build_with(dir: &Path, cc: &Path) -> String {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root exists");
    let mut opts = Options::new(dir.join("main.wid"));
    opts.file_mode = true;
    opts.wid_root = Some(root);
    opts.out = Some(dir.join("main"));
    opts.cc = Some(cc.display().to_string());
    let built = wid_driver::build(&opts);
    assert!(built.exe.is_none(), "a fake compiler can't build");
    let rendered = render_all(&built.checked.diags, &built.checked.sources, RenderOptions { color: false });
    rendered.replace(&cc.display().to_string(), "CC")
}

// One test, so no other thread forks while the scripts are written.
#[test]
fn c_compilers_without_c23() {
    let dir = std::env::temp_dir().join(format!("wid-toolchain-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create a scratch directory");
    std::fs::write(dir.join("main.wid"), "def main\n  puts 1\nend\n").expect("write main.wid");

    // gcc 13 doesn't know `-std=c23`.
    let gcc13 = fake_cc(
        &dir,
        "gcc-13",
        Some("gcc-13 (GCC) 13.3.0\nCopyright (C) 2023 Free Software Foundation, Inc."),
        "gcc-13: error: unrecognized command-line option '-std=c23'; did you mean '-std=c2x'?",
    );
    assert_eq!(
        build_with(&dir, &gcc13),
        "error[E0702]: the C compiler `CC` doesn't support C23
  = note: gcc-13: error: unrecognized command-line option '-std=c23'; did you mean '-std=c2x'?
  = note: `CC --version` says: gcc-13 (GCC) 13.3.0
  = note: Wid compiles to C23, which needs clang 19 or newer, or gcc 15 or newer
help: install clang 19+ or gcc 15+, and choose it with `-cc:path` (like `-cc:gcc-15`) or the WID_CC variable
  = see `wid explain E0702`

error: could not compile due to 1 error
"
    );

    // clang 17 doesn't either, and this one can't say its version.
    let clang17 = fake_cc(
        &dir,
        "clang-17",
        None,
        "error: invalid value 'c23' in '-std=c23'\nnote: use 'c2x' for 'Working Draft for ISO C2x' standard",
    );
    let out = build_with(&dir, &clang17);
    assert!(out.contains("the C compiler `CC` doesn't support C23"), "{out}");
    assert!(!out.contains("--version"), "{out}");

    // gcc 13 in a UTF-8 locale, and a compiler that takes `-std=c23` but
    // lacks `<stdckdint.h>` or `#embed`.
    for (name, stderr) in [
        ("gcc-utf8", "gcc: error: unrecognized command-line option ‘-std=c23’; did you mean ‘-std=c2x’?"),
        (
            "no-ckdint",
            "In file included from program.c:1:\n./wid_runtime.h:16:10: fatal error: 'stdckdint.h' file not found\n   16 | #include <stdckdint.h>\n      |          ^~~~~~~~~~~~~\n1 error generated.",
        ),
        (
            "gcc-no-ckdint",
            "./wid_runtime.h:16:10: fatal error: stdckdint.h: No such file or directory\ncompilation terminated.",
        ),
        (
            "clang-no-embed",
            "program.c:17:2: error: invalid preprocessing directive\n   17 | #embed \"/tmp/data.txt\" limit(6)\n      |  ^\n1 error generated.",
        ),
        ("gcc-no-embed", "program.c:17:2: error: invalid preprocessing directive #embed\n   17 | #embed \"data.txt\""),
    ] {
        let cc = fake_cc(&dir, name, Some("cc 1.0"), stderr);
        let out = build_with(&dir, &cc);
        assert!(out.contains("doesn't support C23"), "{name}: {out}");
        assert!(out.contains("`CC --version` says: cc 1.0"), "{name}: {out}");
        assert!(!out.contains("bug in the Wid compiler"), "{name}: {out}");
    }

    // Any other error in the generated C is still a Wid bug.
    let broken = fake_cc(&dir, "broken", Some("cc 1.0"), "program.c:3:1: error: expected ';' after expression");
    let out = build_with(&dir, &broken);
    assert!(out.contains("rejected the generated C"), "{out}");
    assert!(out.contains("this is a bug in the Wid compiler"), "{out}");
    assert!(!out.contains("C23"), "{out}");
    if let Some(kept) = out.lines().find_map(|l| l.split_once("the generated C is at ").map(|(_, p)| PathBuf::from(p)))
        && let Some(build_dir) = kept.parent()
    {
        let _ = std::fs::remove_dir_all(build_dir);
    }

    let _ = std::fs::remove_dir_all(&dir);
}

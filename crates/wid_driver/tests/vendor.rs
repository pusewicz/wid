//! The `vendor:` packages and the programs that use system libraries.
//!
//! - Every `vendor/` package is checked with all of its declarations, not just
//!   the ones a program uses.
//! - `examples/NAME/` is built with clang and, when available, gcc, using
//!   `-std=c23 -Wall -Wextra -Wpedantic -Werror`. Examples open windows, so
//!   they are only built.
//! - `tests/vendor/NAME/` is built the same way and run; its stdout must
//!   match `tests/vendor/NAME.stdout`.
//!
//! A package or program whose library pkg-config doesn't know is skipped, as
//! is everything when libclang is missing. `WID_TEST_CC=clang,gcc-16` chooses
//! the compilers.

use std::path::{Path, PathBuf};
use std::process::Command;

use wid_diagnostics::{RenderOptions, render_all};
use wid_driver::Options;

/// The repository root.
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root exists")
}

/// The C compilers to build with: `WID_TEST_CC`, or clang plus the newest gcc.
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

/// The pkg-config packages a package or program needs: the `pkg_config:`
/// options of its `cimport`s and of the vendor packages it imports. Comments
/// are skipped, so documentation examples don't count.
fn pkg_config_needs(dir: &Path, root: &Path) -> Vec<String> {
    let mut needs = Vec::new();
    let mut seen = Vec::new();
    collect_needs(dir, root, &mut seen, &mut needs);
    needs.sort();
    needs.dedup();
    needs
}

/// Adds the pkg-config packages of the package in `dir` to `needs`, visiting
/// each package once.
fn collect_needs(dir: &Path, root: &Path, seen: &mut Vec<PathBuf>, needs: &mut Vec<String>) {
    if seen.iter().any(|d| d == dir) {
        return;
    }
    seen.push(dir.to_path_buf());
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "wid") {
            continue;
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        for line in text.lines().map(str::trim).filter(|l| !l.starts_with('#')) {
            if let Some(rest) = line.strip_prefix("import \"vendor:") {
                let name = rest.split('"').next().unwrap_or_default();
                collect_needs(&root.join("vendor").join(name), root, seen, needs);
            }
            for part in line.split("pkg_config: \"").skip(1) {
                needs.extend(part.split('"').next().map(str::to_string));
            }
        }
    }
}

/// Whether pkg-config knows every package in `needs`; reports a skip when not.
fn available(what: &str, needs: &[String]) -> bool {
    for package in needs {
        let found = Command::new("pkg-config").args(["--exists", package]).status().is_ok_and(|s| s.success());
        if !found {
            eprintln!("skipping {what}: pkg-config does not know {package}");
            return false;
        }
    }
    true
}

/// The directories directly inside `dir` holding `.wid` files, recursively,
/// in a stable order.
fn packages(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else { return out };
    let mut dirs: Vec<PathBuf> = entries.flatten().map(|e| e.path()).filter(|p| p.is_dir()).collect();
    dirs.sort();
    for d in dirs {
        let has_wid = std::fs::read_dir(&d)
            .map(|es| es.flatten().any(|e| e.path().extension().is_some_and(|x| x == "wid")))
            .unwrap_or(false);
        if has_wid {
            out.push(d.clone());
        }
        out.extend(packages(&d));
    }
    out
}

/// Checks every declaration of a vendor package through a program that
/// imports it.
fn check_vendor_package(root: &Path, package: &Path, failures: &mut Vec<String>) {
    let rel = package.strip_prefix(root.join("vendor")).expect("inside vendor/").to_string_lossy().replace('\\', "/");
    if !available(&format!("vendor:{rel}"), &pkg_config_needs(package, root)) {
        return;
    }
    let dir = std::env::temp_dir().join(format!("wid-vendor-{}-{}", rel.replace('/', "_"), std::process::id()));
    std::fs::create_dir_all(&dir).expect("create a temporary package");
    std::fs::write(dir.join("main.wid"), format!("import \"vendor:{rel}\", as: :lib\n\ndef main\nend\n"))
        .expect("write the package");
    let mut opts = Options::new(&dir);
    opts.check_all_packages = true;
    opts.wid_root = Some(root.to_path_buf());
    let checked = wid_driver::check(&opts);
    let _ = std::fs::remove_dir_all(&dir);
    if checked.diags.has_errors() {
        let rendered = render_all(&checked.diags, &checked.sources, RenderOptions { color: false });
        failures.push(format!("vendor:{rel} does not check:\n{rendered}"));
    }
}

/// Builds a program with every compiler and, when `expect` is given, runs it
/// and compares its stdout.
fn build_program(root: &Path, dir: &Path, expect: Option<&Path>, ccs: &[String], failures: &mut Vec<String>) {
    let name = dir.strip_prefix(root).unwrap_or(dir).display().to_string();
    if !available(&name, &pkg_config_needs(dir, root)) {
        return;
    }
    for cc in ccs {
        let mut opts = Options::new(dir);
        opts.wid_root = Some(root.to_path_buf());
        opts.cc = Some(cc.clone());
        opts.strict_c = true;
        let tag = format!("{name}-{cc}").replace(['/', '\\'], "_");
        let exe = std::env::temp_dir().join(format!("wid-vendor-{tag}-{}", std::process::id()));
        opts.out = Some(exe.clone());
        let built = wid_driver::build(&opts);
        if built.exe.is_none() {
            let rendered = render_all(&built.checked.diags, &built.checked.sources, RenderOptions { color: false });
            failures.push(format!("{name} [{cc}] does not build:\n{rendered}"));
            continue;
        }
        if let Some(expect) = expect {
            let output = Command::new(&exe).current_dir(root).output().expect("run the program");
            let stdout = String::from_utf8_lossy(&output.stdout);
            let want = std::fs::read_to_string(expect).unwrap_or_default();
            if !output.status.success() || stdout != want {
                failures.push(format!(
                    "{name} [{cc}] exited with {:?}\nexpected:\n{want}\nfound:\n{stdout}\nstderr:\n{}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stderr)
                ));
            }
        }
        let _ = std::fs::remove_file(&exe);
    }
}

/// Checks the vendor packages, then builds the examples and vendor programs.
/// libclang is loaded first and once, before anything else could read the
/// environment while loading sets a variable.
#[test]
fn vendor() {
    if let Err(e) = wid_cimport::libclang() {
        eprintln!("skipping the vendor tests: {e}");
        return;
    }
    let root = repo_root();
    let ccs = compilers();
    let mut failures = Vec::new();
    for package in packages(&root.join("vendor")) {
        check_vendor_package(&root, &package, &mut failures);
    }
    for example in packages(&root.join("examples")) {
        build_program(&root, &example, None, &ccs, &mut failures);
    }
    for program in packages(&root.join("tests/vendor")) {
        let expect = program.with_extension("stdout");
        build_program(&root, &program, Some(&expect), &ccs, &mut failures);
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

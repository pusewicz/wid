//! Imports real C libraries with `cimport` and checks every declaration of
//! the generated packages, not just the ones a program uses. Libraries that
//! aren't installed (or a missing libclang) skip their test.

use std::path::PathBuf;
use std::process::Command;

use wid_diagnostics::{RenderOptions, render_all};
use wid_driver::Options;

/// Checks a package whose only file is `source`, with every function of
/// every package checked, and fails with the rendered diagnostics.
fn check_all(name: &str, source: &str) {
    if wid_cimport::libclang().is_err() {
        eprintln!("skipping {name}: libclang not found");
        return;
    }
    let dir = std::env::temp_dir().join(format!("wid-cimport-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create a temporary package");
    std::fs::write(dir.join("main.wid"), source).expect("write the package");
    let mut opts = Options::new(&dir);
    opts.check_all_packages = true;
    opts.wid_root = Some(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."));
    let checked = wid_driver::check(&opts);
    let _ = std::fs::remove_dir_all(&dir);
    if checked.diags.has_errors() {
        panic!("{name}:\n{}", render_all(&checked.diags, &checked.sources, RenderOptions { color: false }));
    }
}

/// Whether pkg-config knows a package.
fn has_pkg_config(package: &str) -> bool {
    let found = Command::new("pkg-config").args(["--exists", package]).status().is_ok_and(|s| s.success());
    if !found {
        eprintln!("skipping: pkg-config does not know {package}");
    }
    found
}

/// Imports each library in turn: libclang is loaded once, before any other
/// thread could read the environment while it is.
#[test]
fn libraries() {
    raylib();
    raylib_with_mapped_vectors();
    sdl3();
    c_library();
}

/// raylib, as Homebrew or a distribution installs it.
fn raylib() {
    if has_pkg_config("raylib") {
        check_all("raylib", "cimport \"raylib.h\", as: :rl, pkg_config: \"raylib\"\n\ndef main\nend\n");
    }
}

/// raylib with its vectors mapped to Wid arrays.
fn raylib_with_mapped_vectors() {
    if has_pkg_config("raylib") {
        check_all(
            "raylib_mapped",
            "cimport \"raylib.h\", as: :rl, pkg_config: \"raylib\", types: {Vector2: [2]F32, Vector3: [3]F32, Vector4: [4]F32}\n\ndef main\nend\n",
        );
    }
}

/// SDL3 with its `SDL_` prefix stripped.
fn sdl3() {
    if has_pkg_config("sdl3") {
        check_all(
            "sdl3",
            "cimport \"SDL3/SDL.h\", as: :sdl, strip_prefix: \"SDL_\", pkg_config: \"sdl3\"\n\ndef main\nend\n",
        );
    }
}

/// Parts of the C library.
fn c_library() {
    check_all(
        "libc",
        "cimport \"stdio.h\", as: :c\ncimport \"stdlib.h\", as: :std\ncimport \"string.h\", as: :str\n\ndef main\nend\n",
    );
}

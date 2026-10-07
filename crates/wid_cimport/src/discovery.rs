//! Locating libclang and the platform SDK.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

/// File names libclang ships under on this platform.
#[cfg(target_os = "macos")]
const LIBRARY_NAMES: &[&str] = &["libclang.dylib"];
/// File names libclang ships under on this platform.
#[cfg(target_os = "windows")]
const LIBRARY_NAMES: &[&str] = &["libclang.dll", "clang.dll"];
/// File names libclang ships under on this platform.
#[cfg(not(any(target_os = "macos", target_os = "windows")))]
const LIBRARY_NAMES: &[&str] = &["libclang.so"];

/// Finds the libclang shared library.
///
/// The search order is:
///
/// 1. `LIBCLANG_PATH`, either the library itself or a directory holding it.
///    When it is set nothing else is searched, so a wrong value is reported
///    instead of silently replaced.
/// 2. `llvm-config --libdir`, using `LLVM_CONFIG_PATH` or `llvm-config` on `PATH`.
/// 3. The usual install locations for the platform, newest LLVM first.
///
/// Returns every path that was tried when none holds the library.
pub(crate) fn find_libclang() -> Result<PathBuf, Vec<PathBuf>> {
    search(std::env::var_os("LIBCLANG_PATH").map(PathBuf::from))
}

/// [`find_libclang`] with the value of `LIBCLANG_PATH` passed in.
fn search(explicit: Option<PathBuf>) -> Result<PathBuf, Vec<PathBuf>> {
    if let Some(explicit) = explicit {
        if explicit.is_file() {
            return Ok(explicit);
        }
        return library_in(&explicit).ok_or_else(|| vec![explicit]);
    }
    let mut searched = Vec::new();
    let directories = llvm_config_libdir().into_iter().chain(platform_directories());
    for directory in directories {
        if let Some(library) = library_in(&directory) {
            return Ok(library);
        }
        if !searched.contains(&directory) {
            searched.push(directory);
        }
    }
    Err(searched)
}

/// Returns the libclang library inside `directory`, if there is one.
fn library_in(directory: &Path) -> Option<PathBuf> {
    let exact = LIBRARY_NAMES.iter().map(|name| directory.join(name)).find(|path| path.is_file());
    if exact.is_some() || !cfg!(target_os = "linux") {
        return exact;
    }
    // Distributions often ship only versioned names such as
    // `libclang.so.18` or `libclang-18.so`.
    let mut versioned: Vec<PathBuf> = std::fs::read_dir(directory)
        .ok()?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path.file_name().and_then(OsStr::to_str).unwrap_or("");
            (name.starts_with("libclang.so.") || (name.starts_with("libclang-") && name.ends_with(".so")))
                && !name.contains("-cpp")
        })
        .collect();
    versioned.sort_by_key(|path| version_key(path));
    versioned.pop()
}

/// Asks `llvm-config` where LLVM's libraries live.
fn llvm_config_libdir() -> Option<PathBuf> {
    let program = std::env::var_os("LLVM_CONFIG_PATH").unwrap_or_else(|| "llvm-config".into());
    let output = Command::new(program).arg("--libdir").output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let line = text.lines().next()?.trim();
    (!line.is_empty()).then(|| PathBuf::from(line))
}

/// The directories libclang is usually installed in, best candidates first.
fn platform_directories() -> Vec<PathBuf> {
    let mut directories = Vec::new();
    if cfg!(target_os = "macos") {
        for prefix in ["/opt/homebrew/opt", "/usr/local/opt"] {
            directories.extend(versioned_children(Path::new(prefix), "llvm").into_iter().map(|dir| dir.join("lib")));
        }
        directories.push(PathBuf::from("/opt/local/libexec/llvm/lib"));
        if let Some(developer) = xcode_developer_dir() {
            directories.push(developer.join("Toolchains/XcodeDefault.xctoolchain/usr/lib"));
        }
        directories.push(PathBuf::from("/Library/Developer/CommandLineTools/usr/lib"));
    } else if cfg!(target_os = "windows") {
        for root in ["C:\\Program Files\\LLVM", "C:\\Program Files (x86)\\LLVM", "C:\\LLVM"] {
            directories.push(Path::new(root).join("bin"));
        }
    } else {
        for prefix in ["/usr/lib", "/usr/lib64", "/usr/local/lib"] {
            directories.extend(versioned_children(Path::new(prefix), "llvm").into_iter().map(|dir| dir.join("lib")));
        }
        directories.extend(
            ["/usr/lib/x86_64-linux-gnu", "/usr/lib/aarch64-linux-gnu", "/usr/lib64", "/usr/lib", "/usr/local/lib"]
                .into_iter()
                .map(PathBuf::from),
        );
    }
    directories
}

/// Children of `parent` named `stem` or `stem-N`/`stem@N`, highest version first
/// and the unversioned one ahead of all of them.
fn versioned_children(parent: &Path, stem: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(parent) else {
        return Vec::new();
    };
    let mut children: Vec<PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            let name = path.file_name().and_then(OsStr::to_str).unwrap_or("");
            name == stem
                || name.strip_prefix(stem).is_some_and(|rest| {
                    rest.starts_with(['-', '@']) && rest[1..].starts_with(|c: char| c.is_ascii_digit())
                })
        })
        .collect();
    children.sort_by_key(|path| std::cmp::Reverse(version_key(path)));
    children
}

/// The numbers in a path's file name, used to order versions.
///
/// An unversioned name sorts above every versioned one, since it is the
/// package manager's default.
fn version_key(path: &Path) -> (bool, Vec<u32>) {
    let name = path.file_name().and_then(OsStr::to_str).unwrap_or("");
    let numbers: Vec<u32> = name
        .split(|c: char| !c.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .filter_map(|part| part.parse().ok())
        .collect();
    (numbers.is_empty(), numbers)
}

/// The active Xcode developer directory.
fn xcode_developer_dir() -> Option<PathBuf> {
    let output = Command::new("xcode-select").arg("--print-path").output().ok()?;
    let text = String::from_utf8(output.stdout).ok()?;
    let line = text.lines().next()?.trim();
    (output.status.success() && !line.is_empty()).then(|| PathBuf::from(line))
}

/// The clang resource directory (builtin headers such as `<stdarg.h>`) that
/// belongs to the libclang at `library`.
///
/// libclang derives it from its own location, but some builds get that wrong
/// and look in a relative `lib/clang/N`, so it is passed explicitly. Looks in
/// `clang/<version>` beside the library, following symbolic links, and takes
/// the newest version that has the headers.
pub(crate) fn resource_dir(library: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(library).ok();
    let directories =
        [library.parent().map(Path::to_path_buf), real.as_deref().and_then(Path::parent).map(Path::to_path_buf)];
    directories.into_iter().flatten().find_map(|directory| {
        let mut versions: Vec<PathBuf> = std::fs::read_dir(directory.join("clang"))
            .ok()?
            .filter_map(|entry| entry.ok().map(|entry| entry.path()))
            .filter(|path| path.join("include").join("stddef.h").is_file())
            .collect();
        versions.sort_by_key(|path| version_key(path));
        versions.pop()
    })
}

/// The SDK to pass as `-isysroot` on macOS when the request has none.
///
/// libclang, unlike the `clang` driver, reads no configuration files, so it
/// cannot find `<stdio.h>` without one. Uses `SDKROOT` when set, otherwise
/// asks `xcrun`. The answer is computed once per process.
pub(crate) fn default_sysroot() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    static SYSROOT: OnceLock<Option<PathBuf>> = OnceLock::new();
    SYSROOT
        .get_or_init(|| {
            if let Some(root) = std::env::var_os("SDKROOT").filter(|root| !root.is_empty()) {
                return Some(PathBuf::from(root));
            }
            let output = Command::new("xcrun").args(["--show-sdk-path"]).output().ok()?;
            let text = String::from_utf8(output.stdout).ok()?;
            let line = text.lines().next()?.trim();
            (output.status.success() && !line.is_empty()).then(|| PathBuf::from(line))
        })
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_path_is_the_only_one_searched() {
        let missing = PathBuf::from("/nonexistent/wid-cimport/lib");
        assert_eq!(search(Some(missing.clone())), Err(vec![missing]));
        let empty = std::env::temp_dir();
        assert_eq!(search(Some(empty.clone())), Err(vec![empty]));
    }

    #[test]
    fn unversioned_directories_sort_first_then_newest() {
        let mut paths = vec![PathBuf::from("llvm@18"), PathBuf::from("llvm"), PathBuf::from("llvm@20")];
        paths.sort_by_key(|path| std::cmp::Reverse(version_key(path)));
        assert_eq!(paths, vec![PathBuf::from("llvm"), PathBuf::from("llvm@20"), PathBuf::from("llvm@18")]);
    }
}

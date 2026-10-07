//! Imports C headers with libclang.
//!
//! [`import`] parses a header set and returns a [`CModule`]: a faithful,
//! pure-data model of its functions, records, enums, typedefs, globals and
//! macros. Mapping them to Wid types is the type checker's job. The generated
//! C includes the original header, so this crate only has to describe what
//! exists, not reproduce layouts or macro expansions.
//!
//! libclang is loaded with `dlopen` on first use, so the compiler runs
//! without it until a program imports C.

mod discovery;
mod error;
mod ffi;
mod lower;
mod macros;
mod model;
mod source;

use std::path::{Path, PathBuf};

use clang_sys::*;

pub use error::{ImportError, ParseError};
pub use model::*;

use ffi::{Index, LoadError, ParseOptions, Severity};
use lower::Lowerer;
use source::Sources;

/// The in-memory file that includes the requested header. It is never on
/// disk; libclang only needs a name for it.
const WRAPPER: &str = "__wid_cimport__.c";

/// What to import and how to preprocess it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportRequest {
    /// The header to import.
    pub header: Header,
    /// Directories searched for `#include`, in order (`-I`).
    pub include_dirs: Vec<PathBuf>,
    /// Macros to define, as `NAME` or `NAME=value` (`-D`).
    pub defines: Vec<String>,
    /// Further arguments for clang, such as the output of
    /// `pkg-config --cflags`. They come last, so they override the defaults
    /// (`-std=c23`).
    pub clang_args: Vec<String>,
    /// The target triple, or `None` for the host.
    pub target: Option<String>,
    /// Functions to describe even when they are declared outside the
    /// imported headers, such as C library functions that a program declares
    /// by hand. They end up in [`CModule::probed`].
    pub probe_functions: Vec<String>,
}

impl ImportRequest {
    /// A request for `header` with no extra flags.
    pub fn new(header: Header) -> ImportRequest {
        ImportRequest {
            header,
            include_dirs: Vec::new(),
            defines: Vec::new(),
            clang_args: Vec::new(),
            target: None,
            probe_functions: Vec::new(),
        }
    }
}

/// How the header is named.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Header {
    /// A path to the header file.
    Path(PathBuf),
    /// A name looked up on the include path, as in `#include <SDL3/SDL.h>`.
    Include(String),
}

impl Header {
    /// The header as the user wrote it, for messages.
    fn display(&self) -> String {
        match self {
            Header::Path(path) => path.display().to_string(),
            Header::Include(name) => format!("<{name}>"),
        }
    }
}

/// The libclang in use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibclangInfo {
    /// The shared library.
    pub path: PathBuf,
    /// What `clang_getClangVersion` reports.
    pub version: String,
    /// The directory of clang's builtin headers passed as `-resource-dir`,
    /// when one was found beside the library.
    pub resource_dir: Option<PathBuf>,
}

/// Loads libclang if it is not loaded yet and describes it.
///
/// [`import`] does this on demand. A driver that imports from several
/// threads should call this once at startup, before spawning them: loading
/// briefly sets `LIBCLANG_PATH`, which is only sound while no other thread
/// reads the environment from C code.
pub fn libclang() -> Result<LibclangInfo, ImportError> {
    let info = ffi::ensure_loaded().map_err(load_error)?;
    Ok(LibclangInfo { path: info.path, version: info.version, resource_dir: info.resource_dir })
}

/// Converts a loader failure into the public error.
fn load_error(error: LoadError) -> ImportError {
    match error {
        LoadError::NotFound { searched } => ImportError::LibclangNotFound { searched },
        LoadError::OpenFailed { path, message } => ImportError::LibclangUnusable { path, reason: message },
        LoadError::Unsupported { path, version, missing } => {
            ImportError::LibclangTooOld { path, version, missing: missing.into_iter().map(String::from).collect() }
        }
    }
}

/// Imports the declarations of a C header and the headers beside it.
///
/// The result holds every declaration from the header and from headers in
/// its directory tree (so `<SDL3/SDL.h>` brings in `SDL3/SDL_*.h`), in the
/// order the preprocessor reached them.
pub fn import(request: &ImportRequest) -> Result<CModule, ImportError> {
    let libclang = libclang()?;
    let include = include_line(request)?;
    let args = clang_args(request, libclang.resource_dir.as_deref());
    let index = Index::new();
    let options = ParseOptions { detailed_preprocessing_record: true, skip_function_bodies: true };
    let tu = index.parse(WRAPPER, &include, &args, options).map_err(|code| ImportError::Clang { code })?;
    let cursors = tu.cursor().children();

    let mut inclusions = Vec::new();
    let mut main_header = None;
    for cursor in cursors.iter().filter(|cursor| cursor.kind() == CXCursor_InclusionDirective) {
        let spot = cursor.location().expansion();
        let included = cursor.included_file().map(|file| file.name());
        let (Some(includer), Some(included)) = (spot.file, included) else { continue };
        if includer == WRAPPER && main_header.is_none() {
            main_header = Some(included.clone());
        }
        inclusions.push((includer, spot.offset, included));
    }
    let not_found =
        || ImportError::HeaderNotFound { header: request.header.display(), include_dirs: request.include_dirs.clone() };
    let header = main_header.ok_or_else(not_found)?;

    let errors: Vec<ParseError> = tu
        .diagnostics()
        .into_iter()
        .filter(|diag| diag.severity >= Severity::Error)
        .map(|diag| ParseError {
            file: diag.location.file.map(PathBuf::from),
            line: diag.location.line,
            column: diag.location.column,
            message: diag.message,
        })
        .collect();
    if !errors.is_empty() {
        return Err(ImportError::Parse { errors });
    }

    let canonical = std::fs::canonicalize(&header).map_err(|_| not_found())?;
    let root = canonical.parent().map(Path::to_path_buf).unwrap_or_default();
    let flat = shared_include_dirs(&args).contains(&root);
    let mut lowerer = Lowerer::new(Sources::new(root, flat, WRAPPER.to_string(), inclusions), &request.probe_functions);
    lowerer.declarations(&cursors);
    let definitions = macros::definitions(&tu, &cursors, &mut lowerer);
    macros::lower(&index, WRAPPER, &include, &args, definitions, &mut lowerer)
        .map_err(|code| ImportError::Clang { code })?;

    let (triple, pointer_bits) = tu.target();
    let root = lowerer.sources.root().to_path_buf();
    let probed = std::mem::take(&mut lowerer.probed);
    Ok(CModule {
        header: PathBuf::from(header),
        root,
        target: Target { triple, pointer_bits },
        items: lowerer.finish(),
        probed,
    })
}

/// The `#include` line of the wrapper file.
fn include_line(request: &ImportRequest) -> Result<String, ImportError> {
    match &request.header {
        Header::Include(name) => Ok(format!("#include <{name}>\n")),
        Header::Path(path) => {
            if !path.is_file() {
                return Err(ImportError::HeaderNotFound {
                    header: request.header.display(),
                    include_dirs: request.include_dirs.clone(),
                });
            }
            let absolute = std::path::absolute(path).unwrap_or_else(|_| path.clone());
            let quoted = absolute.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
            Ok(format!("#include \"{quoted}\"\n"))
        }
    }
}

/// The clang command line for a request.
fn clang_args(request: &ImportRequest, resource_dir: Option<&Path>) -> Vec<String> {
    let mut args: Vec<String> = ["-x", "c", "-std=c23", "-ferror-limit=0"].into_iter().map(String::from).collect();
    let has_resource_dir = request.clang_args.iter().any(|arg| arg.starts_with("-resource-dir"));
    if let (Some(dir), false) = (resource_dir, has_resource_dir) {
        args.push("-resource-dir".to_string());
        args.push(dir.display().to_string());
    }
    if let Some(target) = &request.target {
        args.push(format!("--target={target}"));
    }
    let has_sysroot = request.clang_args.iter().any(|arg| arg.starts_with("-isysroot") || arg.starts_with("--sysroot"));
    let host_sdk = request.target.as_deref().is_none_or(|target| target.contains("apple"));
    if !has_sysroot
        && host_sdk
        && let Some(sysroot) = discovery::default_sysroot()
    {
        args.push("-isysroot".to_string());
        args.push(sysroot.display().to_string());
    }
    args.extend(request.include_dirs.iter().map(|dir| format!("-I{}", dir.display())));
    args.extend(request.defines.iter().map(|define| format!("-D{define}")));
    args.extend(request.clang_args.iter().cloned());
    args
}

/// Directories that hold the headers of many unrelated libraries, canonicalised:
/// the usual prefixes, the sysroot's `usr/include` and every `-isystem`
/// directory. A header directly inside one of them imports only the headers
/// at that directory's top level, not its subdirectories, so `stdio.h` brings
/// its private `_stdio.h` but not every header under `sys/`.
fn shared_include_dirs(args: &[String]) -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> =
        ["/usr/include", "/usr/local/include", "/opt/homebrew/include", "/opt/local/include"].map(PathBuf::from).into();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        for (flag, is_sysroot) in [("-isysroot", true), ("--sysroot=", true), ("--sysroot", true), ("-isystem", false)]
        {
            let Some(rest) = arg.strip_prefix(flag) else { continue };
            let value = if rest.is_empty() { iter.next().cloned() } else { Some(rest.to_string()) };
            if let Some(value) = value {
                let dir = PathBuf::from(value);
                dirs.push(if is_sysroot { dir.join("usr/include") } else { dir });
            }
            break;
        }
    }
    dirs.into_iter().filter_map(|dir| std::fs::canonicalize(dir).ok()).collect()
}

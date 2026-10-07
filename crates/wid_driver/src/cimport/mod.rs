//! `cimport`: reads a C header with libclang and adds its Wid view to the
//! program as a package of its own.
//!
//! The package is ordinary Wid source (see [`render`]) plus a
//! [`CBinding`] with what the source can't say: the `#include` line, the
//! macros and libraries, and the exact C types for casts. The generated C
//! includes the original header, so the C compiler checks every call.

mod render;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use wid_cimport::{Header, ImportError, ImportRequest};
use wid_diagnostics::{Applicability, Diagnostic, SourceMap, Span, codes};
use wid_sema::CBinding;
use wid_syntax::ast::{self, CimportValue, ExprKind, StrPart};

pub use render::{Naming, Rendered, render};

/// A `cimport` item's options, checked.
#[derive(Clone, Debug)]
pub struct Spec {
    /// The header as written.
    pub header: String,
    /// Where the header string is.
    pub header_span: Span,
    /// The namespace (`as:`).
    pub alias: String,
    /// Naming rules (`strip_prefix:`, `rename:`, `names:`, `types:` keys).
    pub naming: Naming,
    /// `define:` macros.
    pub defines: Vec<String>,
    /// `implement:` macro.
    pub implement: Option<String>,
    /// `link:` entries, resolved.
    pub link_libs: Vec<String>,
    /// Linker flags from `link:` entries that are paths or frameworks.
    pub link_flags: Vec<String>,
    /// `pkg_config:` package names.
    pub pkg_config: Vec<(String, Span)>,
    /// `include_dirs:`, made absolute.
    pub include_dirs: Vec<PathBuf>,
}

/// The option names `cimport` accepts.
const OPTIONS: &[&str] =
    &["as", "strip_prefix", "rename", "names", "types", "define", "implement", "link", "pkg_config", "include_dirs"];

/// Checks the options of a `cimport` item written in a package in `dir`.
pub fn spec(c: &ast::Cimport, dir: &Path, diags: &mut Vec<Diagnostic>) -> Option<Spec> {
    let mut spec = Spec {
        header: c.header.clone(),
        header_span: c.header_span,
        alias: default_alias(&c.header),
        naming: Naming::default(),
        defines: Vec::new(),
        implement: None,
        link_libs: Vec::new(),
        link_flags: Vec::new(),
        pkg_config: Vec::new(),
        include_dirs: Vec::new(),
    };
    let errors_before = diags.len();
    let mut seen = HashSet::new();
    for option in &c.options {
        let name = option.name.as_str();
        if !seen.insert(name.to_string()) {
            diags.push(
                Diagnostic::error(codes::CIMPORT_OPTION, format!("`{name}:` is given twice"))
                    .primary(option.span, "second time")
                    .help("merge the values into one option"),
            );
            continue;
        }
        let bad = |what: &str| {
            Diagnostic::error(codes::CIMPORT_OPTION, format!("`{name}:` takes {what}"))
                .primary(option_value_span(&option.value), "not accepted here")
        };
        match name {
            "as" => match &option.value {
                CimportValue::Expr(ast::Expr { kind: ExprKind::Symbol(sym), .. }) => spec.alias = sym.as_str().to_string(),
                _ => diags.push(bad("a symbol, like `as: :rl`")),
            },
            "strip_prefix" => match strings(&option.value) {
                Some(list) => spec.naming.strip_prefixes = list.into_iter().map(|(s, _)| s).collect(),
                None => diags.push(bad("a string or an array of strings, like `strip_prefix: \"SDL_\"`")),
            },
            "rename" => match &option.value {
                CimportValue::Expr(ast::Expr { kind: ExprKind::Symbol(sym), .. })
                    if matches!(sym.as_str(), "snake_case" | "keep") =>
                {
                    spec.naming.keep = sym.as_str() == "keep";
                }
                _ => diags.push(
                    bad("`:snake_case` (the default) or `:keep`")
                        .note("`:snake_case` turns `InitWindow` into `init_window`; `:keep` only lowercases the first letter of functions"),
                ),
            },
            "names" => match &option.value {
                CimportValue::Hash { entries, .. } => {
                    for entry in entries {
                        match &entry.value.kind {
                            ExprKind::Symbol(sym) => {
                                spec.naming.names.insert(entry.key.clone(), sym.as_str().to_string());
                            }
                            _ => diags.push(
                                Diagnostic::error(codes::CIMPORT_OPTION, "`names:` values are symbols")
                                    .primary(entry.value.span, "write the Wid name as a symbol, like `:get_fps`"),
                            ),
                        }
                    }
                }
                CimportValue::Expr(_) => diags.push(bad("a hash from C names to Wid names, like `names: {GetFPS: :fps}`")),
            },
            "types" => match &option.value {
                CimportValue::Hash { entries, .. } => {
                    spec.naming.mapped.extend(entries.iter().map(|e| e.key.clone()));
                }
                CimportValue::Expr(_) => diags.push(bad("a hash from C types to Wid types, like `types: {Vector2: Vec2}`")),
            },
            "define" => match strings(&option.value) {
                Some(list) => {
                    for (text, span) in list {
                        let name = text.split_once('=').map_or(text.as_str(), |(n, _)| n);
                        if is_c_identifier(name) {
                            spec.defines.push(text);
                        } else {
                            diags.push(
                                Diagnostic::error(codes::CIMPORT_OPTION, format!("`{name}` is not a C macro name"))
                                    .primary(span, "write `NAME` or `NAME=value`"),
                            );
                        }
                    }
                }
                None => diags.push(bad("a string or an array of strings, like `define: [\"NAME=1\"]`")),
            },
            "implement" => match strings(&option.value).as_deref() {
                Some([(text, span)]) => {
                    if is_c_identifier(text) {
                        spec.implement = Some(text.clone());
                    } else {
                        diags.push(
                            Diagnostic::error(codes::CIMPORT_OPTION, format!("`{text}` is not a C macro name"))
                                .primary(*span, "name the macro that switches on the implementation"),
                        );
                    }
                }
                _ => diags.push(bad("one string, like `implement: \"STB_IMAGE_IMPLEMENTATION\"`")),
            },
            "link" => match strings(&option.value) {
                Some(list) => {
                    for (text, _) in list {
                        if let Some(framework) = text.strip_prefix("framework:") {
                            spec.link_flags.push("-framework".to_string());
                            spec.link_flags.push(framework.to_string());
                        } else if text.contains('/') || has_library_extension(&text) {
                            spec.link_flags.push(dir.join(&text).display().to_string());
                        } else {
                            spec.link_libs.push(text);
                        }
                    }
                }
                None => diags.push(bad("a library name or an array of them, like `link: [\"m\", \"framework:Cocoa\"]`")),
            },
            "pkg_config" => match strings(&option.value) {
                Some(list) => spec.pkg_config = list,
                None => diags.push(bad("a pkg-config package name or an array of them, like `pkg_config: \"raylib\"`")),
            },
            "include_dirs" => match strings(&option.value) {
                Some(list) => spec.include_dirs = list.into_iter().map(|(d, _)| absolute(&dir.join(d))).collect(),
                None => diags.push(bad("an array of directories, like `include_dirs: [\"vendor/include\"]`")),
            },
            _ => {
                let mut diag = Diagnostic::error(codes::CIMPORT_OPTION, format!("`cimport` has no option `{name}:`"))
                    .primary(option.name.span, "unknown option");
                diag = match wid_diagnostics::did_you_mean(name, OPTIONS.iter().copied()) {
                    Some(best) => diag.suggest_replace(
                        format!("did you mean `{best}:`?"),
                        option.name.span,
                        best,
                        Applicability::MaybeIncorrect,
                    ),
                    None => diag.note(format!("options: {}", OPTIONS.join(", "))),
                };
                diags.push(diag);
            }
        }
    }
    (diags.len() == errors_before).then_some(spec)
}

/// The span of an option's value.
fn option_value_span(value: &CimportValue) -> Span {
    match value {
        CimportValue::Expr(e) => e.span,
        CimportValue::Hash { span, .. } => *span,
    }
}

/// A plain string literal's text.
fn string(e: &ast::Expr) -> Option<String> {
    match &e.kind {
        ExprKind::Str(parts) => {
            let mut out = String::new();
            for part in parts {
                match part {
                    StrPart::Text(t) => out.push_str(t),
                    StrPart::Interp(_) => return None,
                }
            }
            Some(out)
        }
        _ => None,
    }
}

/// A string or an array of strings, with their spans.
fn strings(value: &CimportValue) -> Option<Vec<(String, Span)>> {
    let CimportValue::Expr(e) = value else { return None };
    match &e.kind {
        ExprKind::Array(items) => items.iter().map(|i| string(i).map(|s| (s, i.span))).collect(),
        _ => string(e).map(|s| vec![(s, e.span)]),
    }
}

/// The name of the package a header becomes when it has no `as:` (its
/// declarations then join the importing package): the file name without the
/// extension, as an identifier.
fn default_alias(header: &str) -> String {
    let name = header.rsplit(['/', '\\']).next().unwrap_or(header);
    let stem = name.split('.').next().unwrap_or(name);
    let mut alias = stem.replace('-', "_").to_ascii_lowercase();
    if alias.is_empty() || alias.starts_with(|c: char| c.is_ascii_digit()) {
        alias.insert(0, 'c');
    }
    alias
}

/// Whether `name` is a valid C identifier.
fn is_c_identifier(name: &str) -> bool {
    !name.is_empty()
        && name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Whether a `link:` entry names a library file.
fn has_library_extension(text: &str) -> bool {
    [".a", ".so", ".dylib", ".lib", ".o", ".tbd"].iter().any(|ext| text.ends_with(ext))
}

/// Makes a path absolute without touching the file system.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The compiler and linker flags `pkg-config` gives for some packages.
#[derive(Debug, Default)]
struct PkgFlags {
    cflags: Vec<String>,
    libs: Vec<String>,
}

/// Runs `pkg-config` for each package, or explains why it failed.
fn pkg_config(packages: &[(String, Span)]) -> Result<PkgFlags, Diagnostic> {
    let mut flags = PkgFlags::default();
    for (package, span) in packages {
        for (flag, out) in [("--cflags", &mut flags.cflags), ("--libs", &mut flags.libs)] {
            let output = Command::new("pkg-config").arg(flag).arg(package).output().map_err(|e| {
                Diagnostic::error(codes::CIMPORT_FAILED, format!("cannot run `pkg-config` for `{package}`: {e}"))
                    .primary(*span, "needs pkg-config")
                    .help("install pkg-config (for example `brew install pkg-config`), or list the library with `link:` and its headers with `include_dirs:`")
            })?;
            if !output.status.success() {
                let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
                return Err(Diagnostic::error(codes::CIMPORT_FAILED, format!("pkg-config does not know `{package}`"))
                    .primary(*span, "not found by pkg-config")
                    .note(stderr)
                    .help(format!("install the library (for example `brew install {package}`), or add the directory of its `.pc` file to PKG_CONFIG_PATH")));
            }
            out.extend(String::from_utf8_lossy(&output.stdout).split_whitespace().map(str::to_string));
        }
    }
    Ok(flags)
}

/// What one `cimport` adds to the program.
pub struct Imported {
    /// The package source.
    pub rendered: Rendered,
    /// How the generated C includes the header.
    pub include: String,
    /// Flags for compiling the generated C.
    pub c_flags: Vec<String>,
    /// Flags for linking.
    pub link_flags: Vec<String>,
}

/// Imports the header a `cimport` names, reporting failures at `item_span`.
///
/// `dir` is the importing package's directory and `probes` the C names of
/// the program's `@[extern]` methods.
pub fn import(
    spec: &Spec,
    dir: &Path,
    probes: &[String],
    sources: &mut SourceMap,
) -> Result<Imported, Vec<Diagnostic>> {
    let local = dir.join(&spec.header);
    let (header, include) = if local.is_file() {
        let path = absolute(&local);
        let quoted = path.display().to_string().replace('\\', "\\\\").replace('"', "\\\"");
        (Header::Path(path), format!("\"{quoted}\""))
    } else if Path::new(&spec.header).is_absolute() {
        let quoted = spec.header.replace('\\', "\\\\").replace('"', "\\\"");
        (Header::Path(PathBuf::from(&spec.header)), format!("\"{quoted}\""))
    } else {
        (Header::Include(spec.header.clone()), format!("<{}>", spec.header))
    };
    let pkg = pkg_config(&spec.pkg_config).map_err(|d| vec![d])?;
    let mut request = ImportRequest::new(header);
    request.include_dirs = spec.include_dirs.clone();
    request.defines = spec.defines.clone();
    request.clang_args = pkg.cflags.clone();
    request.probe_functions = probes.to_vec();
    let module = wid_cimport::import(&request).map_err(|e| vec![import_error(e, spec, sources)])?;
    let rendered = render(&module, &spec.naming, &spec.header);
    let mut c_flags: Vec<String> = Vec::new();
    for d in &spec.include_dirs {
        c_flags.push("-isystem".to_string());
        c_flags.push(d.display().to_string());
    }
    let mut iter = pkg.cflags.iter();
    while let Some(flag) = iter.next() {
        if flag == "-I" {
            if let Some(dir) = iter.next() {
                c_flags.push("-isystem".to_string());
                c_flags.push(dir.clone());
            }
        } else if let Some(dir) = flag.strip_prefix("-I") {
            c_flags.push("-isystem".to_string());
            c_flags.push(dir.to_string());
        } else {
            c_flags.push(flag.clone());
        }
    }
    let mut link_flags = spec.link_flags.clone();
    link_flags.extend(pkg.libs);
    Ok(Imported { rendered, include, c_flags, link_flags })
}

/// Turns an importer failure into a diagnostic at the header string.
fn import_error(error: ImportError, spec: &Spec, sources: &mut SourceMap) -> Diagnostic {
    let at = spec.header_span;
    match error {
        ImportError::LibclangNotFound { searched } => {
            let mut diag = Diagnostic::error(codes::CIMPORT_FAILED, "`cimport` needs libclang, which was not found")
                .primary(at, "this import reads the header with libclang")
                .help("install LLVM (for example `brew install llvm` or `apt install libclang-dev`), or point LIBCLANG_PATH at the directory holding libclang");
            if !searched.is_empty() {
                let list: Vec<String> = searched.iter().map(|p| p.display().to_string()).collect();
                diag = diag.note(format!("searched: {}", list.join(", ")));
            }
            diag
        }
        ImportError::LibclangUnusable { path, reason } => {
            Diagnostic::error(codes::CIMPORT_FAILED, format!("libclang at {} cannot be loaded", path.display()))
                .primary(at, "this import reads the header with libclang")
                .note(reason)
                .help("point LIBCLANG_PATH at a working libclang, from LLVM 15 or newer")
        }
        ImportError::LibclangTooOld { path, version, missing } => {
            Diagnostic::error(codes::CIMPORT_FAILED, format!("libclang at {} is too old ({version})", path.display()))
                .primary(at, "this import reads the header with libclang")
                .note(format!("missing functions: {}", missing.join(", ")))
                .help("install a newer LLVM and point LIBCLANG_PATH at it")
        }
        ImportError::HeaderNotFound { header, include_dirs } => {
            let mut diag = Diagnostic::error(codes::CIMPORT_FAILED, format!("header `{}` not found", spec.header))
                .primary(at, "not next to the package's files or on the include path");
            if !include_dirs.is_empty() {
                let list: Vec<String> = include_dirs.iter().map(|p| p.display().to_string()).collect();
                diag = diag.note(format!("include_dirs: {}", list.join(", ")));
            }
            let _ = header;
            diag.help("write the path relative to the package directory, add its directory with `include_dirs:`, or name the library with `pkg_config:`")
        }
        ImportError::Parse { errors } => {
            let mut diag = Diagnostic::error(codes::CIMPORT_FAILED, format!("`{}` has C errors", spec.header)).primary(
                at,
                match errors.len() {
                    1 => "clang reports an error in this header".to_string(),
                    n => format!("clang reports {n} errors in this header"),
                },
            );
            let cwd = std::env::current_dir().unwrap_or_default();
            let mut files: HashMap<PathBuf, Option<wid_diagnostics::FileId>> = HashMap::new();
            for error in errors.iter().take(8) {
                let span = error.file.as_ref().and_then(|file| {
                    let id = *files.entry(file.clone()).or_insert_with(|| {
                        let text = std::fs::read_to_string(file).ok()?;
                        let shown = file.strip_prefix(&cwd).unwrap_or(file).display().to_string();
                        Some(sources.add(file.clone(), shown, text))
                    });
                    let id = id?;
                    let offset = sources.file(id).offset_of(error.line, error.column)?;
                    Some(Span::new(id, offset, offset))
                });
                diag = match span {
                    Some(span) => diag.secondary(span, error.message.clone()),
                    None => diag.note(error.message.clone()),
                };
            }
            if errors.len() > 8 {
                diag = diag.note(format!("and {} more", errors.len() - 8));
            }
            diag.help("fix the header, or set the macros it expects with `define:`")
        }
        ImportError::Clang { code } => Diagnostic::error(
            codes::CIMPORT_FAILED,
            format!("libclang failed to parse `{}` (error {code:?})", spec.header),
        )
        .primary(at, "this header")
        .help("check that the file is a C header, and that libclang works with `wid cimport --dump`"),
    }
}

/// Fills the parts of a binding that come from the import itself.
pub fn binding(imported: Imported, spec: &Spec, origin: (wid_sema::PackageId, usize, usize)) -> (String, CBinding) {
    let Imported { rendered, include, c_flags, link_flags } = imported;
    let mut binding = rendered.binding;
    binding.include = include;
    binding.defines = spec.defines.clone();
    binding.implement = spec.implement.clone();
    binding.link_libs = spec.link_libs.clone();
    binding.c_flags = c_flags;
    binding.link_flags = link_flags;
    binding.origin = origin;
    (rendered.source, binding)
}

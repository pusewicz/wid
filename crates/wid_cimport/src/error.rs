//! Why an import failed.

use std::fmt;
use std::path::PathBuf;

/// Why [`import`](crate::import) could not produce a module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportError {
    /// No libclang shared library was found.
    LibclangNotFound {
        /// Every path that was searched, in order.
        searched: Vec<PathBuf>,
    },
    /// A libclang shared library was found but could not be loaded.
    LibclangUnusable {
        /// The library.
        path: PathBuf,
        /// The loader's explanation.
        reason: String,
    },
    /// The libclang found is too old: it lacks functions the importer calls.
    LibclangTooOld {
        /// The library.
        path: PathBuf,
        /// What `clang_getClangVersion` reports.
        version: String,
        /// The missing functions.
        missing: Vec<String>,
    },
    /// The header does not exist or is not on the include path.
    HeaderNotFound {
        /// The header as requested.
        header: String,
        /// The include directories that were searched, besides the system ones.
        include_dirs: Vec<PathBuf>,
    },
    /// The headers have errors.
    Parse {
        /// Every error and fatal error libclang reported.
        errors: Vec<ParseError>,
    },
    /// libclang refused to parse at all (a crash or invalid arguments).
    Clang {
        /// The `CXErrorCode`.
        code: i32,
    },
}

/// An error libclang reported while parsing the headers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParseError {
    /// The file, or `None` for errors about the command line.
    pub file: Option<PathBuf>,
    /// The 1-based line, or 0 when there is no file.
    pub line: u32,
    /// The 1-based column, or 0 when there is no file.
    pub column: u32,
    /// The message, as clang words it.
    pub message: String,
}

impl fmt::Display for ParseError {
    /// Formats as `file:line:column: message`.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.file {
            Some(file) => write!(f, "{}:{}:{}: {}", file.display(), self.line, self.column, self.message),
            None => f.write_str(&self.message),
        }
    }
}

impl fmt::Display for ImportError {
    /// Formats a one-paragraph explanation.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ImportError::LibclangNotFound { searched } => {
                write!(f, "libclang was not found; set LIBCLANG_PATH to the library or the directory holding it")?;
                if !searched.is_empty() {
                    let list: Vec<String> = searched.iter().map(|path| path.display().to_string()).collect();
                    write!(f, " (searched {})", list.join(", "))?;
                }
                Ok(())
            }
            ImportError::LibclangUnusable { path, reason } => {
                write!(f, "libclang at {} could not be loaded: {reason}", path.display())
            }
            ImportError::LibclangTooOld { path, version, missing } => write!(
                f,
                "libclang at {} ({version}) is too old; it lacks {}. Install LLVM 11 or newer",
                path.display(),
                missing.join(", ")
            ),
            ImportError::HeaderNotFound { header, include_dirs } => {
                write!(f, "header `{header}` was not found")?;
                if !include_dirs.is_empty() {
                    let list: Vec<String> = include_dirs.iter().map(|path| path.display().to_string()).collect();
                    write!(f, " (include directories: {})", list.join(", "))?;
                }
                Ok(())
            }
            ImportError::Parse { errors } => {
                write!(f, "the C headers have {} error(s)", errors.len())?;
                for error in errors {
                    write!(f, "\n  {error}")?;
                }
                Ok(())
            }
            ImportError::Clang { code } => write!(f, "libclang failed to parse the headers (error code {code})"),
        }
    }
}

impl std::error::Error for ImportError {}

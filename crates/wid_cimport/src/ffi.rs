//! The only module that talks to libclang directly.
//!
//! Everything here wraps `clang-sys` calls in a safe API. Handles that libclang
//! owns (`CXIndex`, `CXTranslationUnit`, `CXString`, tokens, diagnostics and
//! evaluation results) are released on drop, and every cursor, type and
//! location borrows the translation unit it came from, so none of them can
//! outlive it.
#![allow(unsafe_code)]
// libclang constants keep their C names.
#![allow(non_upper_case_globals)]

use std::ffi::{CStr, CString, c_char, c_uint, c_void};
use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::ptr;
use std::sync::{Arc, OnceLock};

use clang_sys::*;

/// Reports whether `clang-sys` found a function in the loaded library.
type IsLoaded = fn() -> bool;

/// Every libclang function this crate calls, checked once after loading so a
/// library that lacks one fails with an error instead of a panic.
///
/// All of them exist in libclang 11 and later.
const REQUIRED: &[(&str, IsLoaded)] = &[
    ("clang_Cursor_Evaluate", clang_Cursor_Evaluate::is_loaded),
    ("clang_Cursor_getArgument", clang_Cursor_getArgument::is_loaded),
    ("clang_Cursor_getNumArguments", clang_Cursor_getNumArguments::is_loaded),
    ("clang_Cursor_getOffsetOfField", clang_Cursor_getOffsetOfField::is_loaded),
    ("clang_Cursor_getRawCommentText", clang_Cursor_getRawCommentText::is_loaded),
    ("clang_Cursor_getStorageClass", clang_Cursor_getStorageClass::is_loaded),
    ("clang_Cursor_isAnonymous", clang_Cursor_isAnonymous::is_loaded),
    ("clang_Cursor_isBitField", clang_Cursor_isBitField::is_loaded),
    ("clang_Cursor_isFunctionInlined", clang_Cursor_isFunctionInlined::is_loaded),
    ("clang_Cursor_isMacroBuiltin", clang_Cursor_isMacroBuiltin::is_loaded),
    ("clang_Cursor_isMacroFunctionLike", clang_Cursor_isMacroFunctionLike::is_loaded),
    ("clang_EvalResult_dispose", clang_EvalResult_dispose::is_loaded),
    ("clang_EvalResult_getAsDouble", clang_EvalResult_getAsDouble::is_loaded),
    ("clang_EvalResult_getAsLongLong", clang_EvalResult_getAsLongLong::is_loaded),
    ("clang_EvalResult_getAsStr", clang_EvalResult_getAsStr::is_loaded),
    ("clang_EvalResult_getAsUnsigned", clang_EvalResult_getAsUnsigned::is_loaded),
    ("clang_EvalResult_getKind", clang_EvalResult_getKind::is_loaded),
    ("clang_EvalResult_isUnsignedInt", clang_EvalResult_isUnsignedInt::is_loaded),
    ("clang_TargetInfo_dispose", clang_TargetInfo_dispose::is_loaded),
    ("clang_TargetInfo_getPointerWidth", clang_TargetInfo_getPointerWidth::is_loaded),
    ("clang_TargetInfo_getTriple", clang_TargetInfo_getTriple::is_loaded),
    ("clang_Type_getAlignOf", clang_Type_getAlignOf::is_loaded),
    ("clang_Type_getNamedType", clang_Type_getNamedType::is_loaded),
    ("clang_Type_getSizeOf", clang_Type_getSizeOf::is_loaded),
    ("clang_Type_getValueType", clang_Type_getValueType::is_loaded),
    ("clang_Type_visitFields", clang_Type_visitFields::is_loaded),
    ("clang_createIndex", clang_createIndex::is_loaded),
    ("clang_disposeDiagnostic", clang_disposeDiagnostic::is_loaded),
    ("clang_disposeIndex", clang_disposeIndex::is_loaded),
    ("clang_disposeString", clang_disposeString::is_loaded),
    ("clang_disposeTokens", clang_disposeTokens::is_loaded),
    ("clang_disposeTranslationUnit", clang_disposeTranslationUnit::is_loaded),
    ("clang_getArgType", clang_getArgType::is_loaded),
    ("clang_getArrayElementType", clang_getArrayElementType::is_loaded),
    ("clang_getArraySize", clang_getArraySize::is_loaded),
    ("clang_getCString", clang_getCString::is_loaded),
    ("clang_getCanonicalType", clang_getCanonicalType::is_loaded),
    ("clang_getClangVersion", clang_getClangVersion::is_loaded),
    ("clang_getCursorExtent", clang_getCursorExtent::is_loaded),
    ("clang_getCursorKind", clang_getCursorKind::is_loaded),
    ("clang_getCursorLocation", clang_getCursorLocation::is_loaded),
    ("clang_getCursorReferenced", clang_getCursorReferenced::is_loaded),
    ("clang_getCursorResultType", clang_getCursorResultType::is_loaded),
    ("clang_getCursorSpelling", clang_getCursorSpelling::is_loaded),
    ("clang_getCursorTLSKind", clang_getCursorTLSKind::is_loaded),
    ("clang_getCursorType", clang_getCursorType::is_loaded),
    ("clang_getCursorUSR", clang_getCursorUSR::is_loaded),
    ("clang_getDiagnostic", clang_getDiagnostic::is_loaded),
    ("clang_getDiagnosticLocation", clang_getDiagnosticLocation::is_loaded),
    ("clang_getDiagnosticSeverity", clang_getDiagnosticSeverity::is_loaded),
    ("clang_getDiagnosticSpelling", clang_getDiagnosticSpelling::is_loaded),
    ("clang_getEnumConstantDeclUnsignedValue", clang_getEnumConstantDeclUnsignedValue::is_loaded),
    ("clang_getEnumConstantDeclValue", clang_getEnumConstantDeclValue::is_loaded),
    ("clang_getEnumDeclIntegerType", clang_getEnumDeclIntegerType::is_loaded),
    ("clang_getExpansionLocation", clang_getExpansionLocation::is_loaded),
    ("clang_getFieldDeclBitWidth", clang_getFieldDeclBitWidth::is_loaded),
    ("clang_getFileName", clang_getFileName::is_loaded),
    ("clang_getIncludedFile", clang_getIncludedFile::is_loaded),
    ("clang_getNumArgTypes", clang_getNumArgTypes::is_loaded),
    ("clang_getNumDiagnostics", clang_getNumDiagnostics::is_loaded),
    ("clang_getPointeeType", clang_getPointeeType::is_loaded),
    ("clang_getRangeEnd", clang_getRangeEnd::is_loaded),
    ("clang_getRangeStart", clang_getRangeStart::is_loaded),
    ("clang_getResultType", clang_getResultType::is_loaded),
    ("clang_getTokenExtent", clang_getTokenExtent::is_loaded),
    ("clang_getTokenKind", clang_getTokenKind::is_loaded),
    ("clang_getTokenSpelling", clang_getTokenSpelling::is_loaded),
    ("clang_getTranslationUnitCursor", clang_getTranslationUnitCursor::is_loaded),
    ("clang_getTranslationUnitTargetInfo", clang_getTranslationUnitTargetInfo::is_loaded),
    ("clang_getTypeDeclaration", clang_getTypeDeclaration::is_loaded),
    ("clang_getTypeSpelling", clang_getTypeSpelling::is_loaded),
    ("clang_getTypedefDeclUnderlyingType", clang_getTypedefDeclUnderlyingType::is_loaded),
    ("clang_isConstQualifiedType", clang_isConstQualifiedType::is_loaded),
    ("clang_isCursorDefinition", clang_isCursorDefinition::is_loaded),
    ("clang_isFileMultipleIncludeGuarded", clang_isFileMultipleIncludeGuarded::is_loaded),
    ("clang_isFunctionTypeVariadic", clang_isFunctionTypeVariadic::is_loaded),
    ("clang_isRestrictQualifiedType", clang_isRestrictQualifiedType::is_loaded),
    ("clang_isVolatileQualifiedType", clang_isVolatileQualifiedType::is_loaded),
    ("clang_parseTranslationUnit2", clang_parseTranslationUnit2::is_loaded),
    ("clang_tokenize", clang_tokenize::is_loaded),
    ("clang_visitChildren", clang_visitChildren::is_loaded),
];

/// Why libclang could not be made available.
#[derive(Clone, Debug)]
pub(crate) enum LoadError {
    /// No candidate path held a libclang shared library.
    NotFound { searched: Vec<PathBuf> },
    /// A library was found but could not be opened.
    OpenFailed { path: PathBuf, message: String },
    /// The library lacks functions this crate calls.
    Unsupported { path: PathBuf, version: String, missing: Vec<&'static str> },
}

/// A successfully loaded libclang, shared by every thread.
struct Loaded {
    library: Arc<SharedLibrary>,
    version: String,
    resource_dir: Option<PathBuf>,
}

static LIBCLANG: OnceLock<Result<Loaded, LoadError>> = OnceLock::new();

/// Path and version of the libclang this process uses.
#[derive(Clone, Debug)]
pub(crate) struct LoadedInfo {
    pub path: PathBuf,
    pub version: String,
    pub resource_dir: Option<PathBuf>,
}

/// Loads libclang once per process and makes it usable on the calling thread.
///
/// `clang-sys` keeps the loaded library in a thread-local, so every thread
/// that calls into libclang must install the shared handle first; this does
/// that on each call.
pub(crate) fn ensure_loaded() -> Result<LoadedInfo, LoadError> {
    let loaded = LIBCLANG.get_or_init(load).as_ref().map_err(Clone::clone)?;
    if !clang_sys::is_loaded() {
        clang_sys::set_library(Some(Arc::clone(&loaded.library)));
    }
    Ok(LoadedInfo {
        path: loaded.library.path().to_path_buf(),
        version: loaded.version.clone(),
        resource_dir: loaded.resource_dir.clone(),
    })
}

/// Finds, opens and validates libclang.
fn load() -> Result<Loaded, LoadError> {
    let path = crate::discovery::find_libclang().map_err(|searched| LoadError::NotFound { searched })?;
    let library = open(&path)?;
    let previous = clang_sys::set_library(Some(Arc::clone(&library)));
    let missing: Vec<&'static str> =
        REQUIRED.iter().filter(|(_, is_loaded)| !is_loaded()).map(|(name, _)| *name).collect();
    let version = if clang_getClangVersion::is_loaded() {
        // SAFETY: the library is installed on this thread and exports the function.
        take_string(unsafe { clang_getClangVersion() })
    } else {
        String::from("unknown")
    };
    clang_sys::set_library(previous);
    if missing.is_empty() {
        let resource_dir = crate::discovery::resource_dir(&path);
        Ok(Loaded { library, version, resource_dir })
    } else {
        Err(LoadError::Unsupported { path, version, missing })
    }
}

/// Opens the libclang shared library at `path`.
///
/// `clang-sys` only loads from paths it discovers itself, and `LIBCLANG_PATH`
/// is the one way to direct it, so the variable is pointed at `path` for the
/// duration of the load and restored afterwards.
fn open(path: &Path) -> Result<Arc<SharedLibrary>, LoadError> {
    let previous = std::env::var_os("LIBCLANG_PATH");
    if previous.as_deref() != Some(path.as_os_str()) {
        // SAFETY: Rust's environment lock serialises this with every other
        // `std::env` access. Foreign code reading the environment concurrently
        // could still race, which is why the load happens once per process and
        // `preload` lets the driver do it before it starts worker threads.
        unsafe { std::env::set_var("LIBCLANG_PATH", path) };
    }
    let result = clang_sys::load_manually();
    match previous {
        // SAFETY: as above.
        Some(value) => unsafe { std::env::set_var("LIBCLANG_PATH", value) },
        // SAFETY: as above.
        None => unsafe { std::env::remove_var("LIBCLANG_PATH") },
    }
    result.map(Arc::new).map_err(|message| LoadError::OpenFailed { path: path.to_path_buf(), message })
}

/// Converts a libclang-owned string into a Rust string and frees it.
fn take_string(raw: CXString) -> String {
    // SAFETY: `raw` came from libclang and is disposed exactly once, after the
    // contents have been copied out.
    unsafe {
        let ptr = clang_getCString(raw);
        let text = if ptr.is_null() { String::new() } else { CStr::from_ptr(ptr).to_string_lossy().into_owned() };
        clang_disposeString(raw);
        text
    }
}

/// A libclang index, the context that owns translation units.
pub(crate) struct Index {
    raw: CXIndex,
}

impl Index {
    /// Creates an index that does not print diagnostics to stderr.
    pub fn new() -> Index {
        // SAFETY: plain constructor; the handle is released in `Drop`.
        let raw = unsafe { clang_createIndex(0, 0) };
        Index { raw }
    }

    /// Parses `contents` as an in-memory C file named `file_name`.
    ///
    /// Returns the libclang error code when parsing fails outright (as
    /// opposed to producing diagnostics).
    pub fn parse(
        &self,
        file_name: &str,
        contents: &str,
        args: &[String],
        options: ParseOptions,
    ) -> Result<TranslationUnit<'_>, i32> {
        let name = c_string(file_name);
        let text = c_string(contents);
        let args: Vec<CString> = args.iter().map(|arg| c_string(arg)).collect();
        let arg_ptrs: Vec<*const c_char> = args.iter().map(|arg| arg.as_ptr()).collect();
        let mut unsaved =
            CXUnsavedFile { Filename: name.as_ptr(), Contents: text.as_ptr(), Length: text.as_bytes().len() as _ };
        let mut flags = CXTranslationUnit_KeepGoing;
        if options.detailed_preprocessing_record {
            flags |= CXTranslationUnit_DetailedPreprocessingRecord;
        }
        if options.skip_function_bodies {
            flags |= CXTranslationUnit_SkipFunctionBodies;
        }
        let mut raw: CXTranslationUnit = ptr::null_mut();
        let arg_count = i32::try_from(arg_ptrs.len()).map_err(|_| CXError_InvalidArguments)?;
        // SAFETY: every pointer refers to a live `CString` or `Vec` that
        // outlives the call, and libclang copies unsaved buffers.
        let code = unsafe {
            clang_parseTranslationUnit2(
                self.raw,
                name.as_ptr(),
                arg_ptrs.as_ptr(),
                arg_count,
                &mut unsaved,
                1,
                flags,
                &mut raw,
            )
        };
        if code != CXError_Success || raw.is_null() {
            return Err(code);
        }
        Ok(TranslationUnit { raw, _index: PhantomData })
    }
}

impl Drop for Index {
    fn drop(&mut self) {
        // SAFETY: the index was created by `clang_createIndex` and every
        // translation unit borrowing it has already been dropped.
        unsafe { clang_disposeIndex(self.raw) }
    }
}

/// Builds a C string, dropping interior NUL bytes, which C cannot represent.
fn c_string(text: &str) -> CString {
    CString::new(text.replace('\0', "")).expect("invariant: NUL bytes were removed")
}

/// Options for [`Index::parse`].
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ParseOptions {
    /// Record macro definitions and inclusion directives as cursors.
    pub detailed_preprocessing_record: bool,
    /// Skip the bodies of function definitions.
    pub skip_function_bodies: bool,
}

/// A parsed translation unit.
pub(crate) struct TranslationUnit<'i> {
    raw: CXTranslationUnit,
    _index: PhantomData<&'i Index>,
}

impl Drop for TranslationUnit<'_> {
    fn drop(&mut self) {
        // SAFETY: the unit came from `clang_parseTranslationUnit2` and nothing
        // borrowing it outlives `self`.
        unsafe { clang_disposeTranslationUnit(self.raw) }
    }
}

impl TranslationUnit<'_> {
    /// The cursor for the whole translation unit.
    pub fn cursor(&self) -> Cursor<'_> {
        // SAFETY: `self.raw` is a live translation unit.
        Cursor::new(unsafe { clang_getTranslationUnitCursor(self.raw) })
    }

    /// Every diagnostic the parse produced.
    pub fn diagnostics(&self) -> Vec<Diagnostic> {
        // SAFETY: `self.raw` is a live translation unit.
        let count = unsafe { clang_getNumDiagnostics(self.raw) };
        (0..count)
            .map(|index| {
                // SAFETY: `index` is in range; the diagnostic is disposed below
                // after its data has been copied out.
                unsafe {
                    let diag = clang_getDiagnostic(self.raw, index);
                    let severity = match clang_getDiagnosticSeverity(diag) {
                        CXDiagnostic_Fatal => Severity::Fatal,
                        CXDiagnostic_Error => Severity::Error,
                        CXDiagnostic_Warning => Severity::Warning,
                        _ => Severity::Note,
                    };
                    let message = take_string(clang_getDiagnosticSpelling(diag));
                    let location = SourceLocation::new(clang_getDiagnosticLocation(diag)).expansion();
                    clang_disposeDiagnostic(diag);
                    Diagnostic { severity, message, location }
                }
            })
            .collect()
    }

    /// The tokens in `range`.
    pub fn tokenize(&self, range: SourceRange<'_>) -> Vec<Token> {
        let mut tokens: *mut CXToken = ptr::null_mut();
        let mut count: c_uint = 0;
        // SAFETY: `range` belongs to this unit; libclang allocates `tokens`,
        // which is read within bounds and then released with
        // `clang_disposeTokens`.
        unsafe {
            clang_tokenize(self.raw, range.raw, &mut tokens, &mut count);
            if tokens.is_null() {
                return Vec::new();
            }
            let slice = std::slice::from_raw_parts(tokens, count as usize);
            let out = slice
                .iter()
                .map(|&token| {
                    let kind = match clang_getTokenKind(token) {
                        CXToken_Punctuation => TokenKind::Punctuation,
                        CXToken_Keyword => TokenKind::Keyword,
                        CXToken_Identifier => TokenKind::Identifier,
                        CXToken_Literal => TokenKind::Literal,
                        _ => TokenKind::Comment,
                    };
                    let spelling = take_string(clang_getTokenSpelling(self.raw, token));
                    let extent = SourceRange::<'_> { raw: clang_getTokenExtent(self.raw, token), _tu: PhantomData };
                    Token {
                        kind,
                        spelling,
                        start: extent.start().expansion().offset,
                        end: extent.end().expansion().offset,
                    }
                })
                .collect();
            clang_disposeTokens(self.raw, tokens, count);
            out
        }
    }

    /// Whether libclang detected an include guard (or `#pragma once`) in `file`.
    pub fn is_include_guarded(&self, file: File<'_>) -> bool {
        // SAFETY: `file` belongs to this unit.
        unsafe { clang_isFileMultipleIncludeGuarded(self.raw, file.raw) != 0 }
    }

    /// The target triple and pointer width the unit was parsed for.
    pub fn target(&self) -> (String, u32) {
        // SAFETY: `self.raw` is live; the target info is disposed after use.
        unsafe {
            let info = clang_getTranslationUnitTargetInfo(self.raw);
            if info.is_null() {
                return (String::new(), 0);
            }
            let triple = take_string(clang_TargetInfo_getTriple(info));
            let width = clang_TargetInfo_getPointerWidth(info);
            clang_TargetInfo_dispose(info);
            (triple, u32::try_from(width).unwrap_or(0))
        }
    }
}

/// How serious a diagnostic is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Severity {
    Note,
    Warning,
    Error,
    Fatal,
}

/// A diagnostic copied out of libclang.
#[derive(Clone, Debug)]
pub(crate) struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    pub location: Spot,
}

/// The lexical class of a token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TokenKind {
    Punctuation,
    Keyword,
    Identifier,
    Literal,
    Comment,
}

/// A token copied out of libclang, with byte offsets into its file.
#[derive(Clone, Debug)]
pub(crate) struct Token {
    pub kind: TokenKind,
    pub spelling: String,
    pub start: u32,
    pub end: u32,
}

/// A file known to a translation unit.
#[derive(Clone, Copy)]
pub(crate) struct File<'tu> {
    raw: CXFile,
    _tu: PhantomData<&'tu ()>,
}

impl File<'_> {
    /// The file name as libclang resolved it (not canonicalised).
    pub fn name(&self) -> String {
        // SAFETY: `self.raw` is a live, non-null file handle.
        take_string(unsafe { clang_getFileName(self.raw) })
    }
}

/// A resolved position: file, 1-based line and column, and byte offset.
#[derive(Clone, Debug, Default)]
pub(crate) struct Spot {
    pub file: Option<String>,
    pub line: u32,
    pub column: u32,
    pub offset: u32,
}

/// A location inside a translation unit.
#[derive(Clone, Copy)]
pub(crate) struct SourceLocation<'tu> {
    raw: CXSourceLocation,
    _tu: PhantomData<&'tu ()>,
}

impl<'tu> SourceLocation<'tu> {
    /// Wraps a raw location.
    fn new(raw: CXSourceLocation) -> Self {
        SourceLocation { raw, _tu: PhantomData }
    }

    /// The file handle of the expansion location, if any.
    pub fn file(&self) -> Option<File<'tu>> {
        let mut file: CXFile = ptr::null_mut();
        // SAFETY: the out-pointers are valid; null ones are allowed.
        unsafe { clang_getExpansionLocation(self.raw, &mut file, ptr::null_mut(), ptr::null_mut(), ptr::null_mut()) };
        if file.is_null() { None } else { Some(File { raw: file, _tu: PhantomData }) }
    }

    /// Where the location ends up after macro expansion.
    pub fn expansion(&self) -> Spot {
        let mut file: CXFile = ptr::null_mut();
        let (mut line, mut column, mut offset) = (0, 0, 0);
        // SAFETY: the out-pointers are valid for the duration of the call.
        unsafe { clang_getExpansionLocation(self.raw, &mut file, &mut line, &mut column, &mut offset) };
        let file = if file.is_null() { None } else { Some(File { raw: file, _tu: PhantomData }.name()) };
        Spot { file, line, column, offset }
    }
}

/// A source range inside a translation unit.
#[derive(Clone, Copy)]
pub(crate) struct SourceRange<'tu> {
    raw: CXSourceRange,
    _tu: PhantomData<&'tu ()>,
}

impl<'tu> SourceRange<'tu> {
    /// The first position in the range.
    pub fn start(&self) -> SourceLocation<'tu> {
        // SAFETY: plain accessor on a value type.
        SourceLocation::new(unsafe { clang_getRangeStart(self.raw) })
    }

    /// The position just past the range.
    pub fn end(&self) -> SourceLocation<'tu> {
        // SAFETY: plain accessor on a value type.
        SourceLocation::new(unsafe { clang_getRangeEnd(self.raw) })
    }
}

/// The value of a constant expression, as `clang_Cursor_Evaluate` reports it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) enum EvalValue {
    Int(i128),
    Float(f64),
    Str(Vec<u8>),
}

/// A node of the AST.
#[derive(Clone, Copy)]
pub(crate) struct Cursor<'tu> {
    raw: CXCursor,
    _tu: PhantomData<&'tu ()>,
}

/// Collects children for [`Cursor::children`].
extern "C" fn collect_child(cursor: CXCursor, _parent: CXCursor, data: CXClientData) -> CXChildVisitResult {
    // SAFETY: `data` is the `Vec` that `Cursor::children` passed in, which is
    // alive and not otherwise borrowed during the visit.
    let out = unsafe { &mut *data.cast::<Vec<CXCursor>>() };
    out.push(cursor);
    CXChildVisit_Continue
}

/// Collects fields for [`Type::fields`].
extern "C" fn collect_field(cursor: CXCursor, data: CXClientData) -> CXVisitorResult {
    // SAFETY: `data` is the `Vec` that `Type::fields` passed in, which is alive
    // and not otherwise borrowed during the visit.
    let out = unsafe { &mut *data.cast::<Vec<CXCursor>>() };
    out.push(cursor);
    CXVisit_Continue
}

impl<'tu> Cursor<'tu> {
    /// Wraps a raw cursor.
    fn new(raw: CXCursor) -> Self {
        Cursor { raw, _tu: PhantomData }
    }

    /// The cursor kind.
    pub fn kind(&self) -> CXCursorKind {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_getCursorKind(self.raw) }
    }

    /// The name of the entity, or an empty string.
    pub fn spelling(&self) -> String {
        // SAFETY: plain accessor; the string is freed by `take_string`.
        take_string(unsafe { clang_getCursorSpelling(self.raw) })
    }

    /// The Unified Symbol Resolution string, stable across redeclarations.
    pub fn usr(&self) -> String {
        // SAFETY: plain accessor; the string is freed by `take_string`.
        take_string(unsafe { clang_getCursorUSR(self.raw) })
    }

    /// The location of the entity's name.
    pub fn location(&self) -> SourceLocation<'tu> {
        // SAFETY: plain accessor on a value type.
        SourceLocation::new(unsafe { clang_getCursorLocation(self.raw) })
    }

    /// The full source range of the entity.
    pub fn extent(&self) -> SourceRange<'tu> {
        // SAFETY: plain accessor on a value type.
        SourceRange { raw: unsafe { clang_getCursorExtent(self.raw) }, _tu: PhantomData }
    }

    /// The type of the entity.
    pub fn ty(&self) -> Type<'tu> {
        // SAFETY: plain accessor on a value type.
        Type::new(unsafe { clang_getCursorType(self.raw) })
    }

    /// Whether this cursor is the defining declaration.
    pub fn is_definition(&self) -> bool {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_isCursorDefinition(self.raw) != 0 }
    }

    /// The documentation comment attached to the declaration, verbatim.
    pub fn raw_comment(&self) -> Option<String> {
        // SAFETY: plain accessor; the string is freed by `take_string`.
        let text = take_string(unsafe { clang_Cursor_getRawCommentText(self.raw) });
        if text.is_empty() { None } else { Some(text) }
    }

    /// The storage class of a function or variable.
    pub fn storage_class(&self) -> CX_StorageClass {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_Cursor_getStorageClass(self.raw) }
    }

    /// Whether a function is declared `inline`.
    pub fn is_inlined(&self) -> bool {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_Cursor_isFunctionInlined(self.raw) != 0 }
    }

    /// Whether a variable is `thread_local`.
    pub fn is_thread_local(&self) -> bool {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_getCursorTLSKind(self.raw) != CXTLS_None }
    }

    /// The declaration a reference, such as a `DeclRefExpr`, names; a null
    /// cursor when there is none.
    pub fn referenced(&self) -> Cursor<'tu> {
        // SAFETY: plain accessor on a value type.
        Cursor::new(unsafe { clang_getCursorReferenced(self.raw) })
    }

    /// The return type of a function.
    pub fn result_type(&self) -> Type<'tu> {
        // SAFETY: plain accessor on a value type.
        Type::new(unsafe { clang_getCursorResultType(self.raw) })
    }

    /// The parameter declarations of a function.
    pub fn arguments(&self) -> Vec<Cursor<'tu>> {
        // SAFETY: plain accessors; `index` stays below the reported count.
        unsafe {
            let count = clang_Cursor_getNumArguments(self.raw);
            (0..u32::try_from(count).unwrap_or(0))
                .map(|index| Cursor::new(clang_Cursor_getArgument(self.raw, index)))
                .collect()
        }
    }

    /// The underlying type of a typedef.
    pub fn typedef_underlying(&self) -> Type<'tu> {
        // SAFETY: plain accessor on a value type.
        Type::new(unsafe { clang_getTypedefDeclUnderlyingType(self.raw) })
    }

    /// The integer type of an enum.
    pub fn enum_integer_type(&self) -> Type<'tu> {
        // SAFETY: plain accessor on a value type.
        Type::new(unsafe { clang_getEnumDeclIntegerType(self.raw) })
    }

    /// The value of an enum constant, read with the given signedness.
    pub fn enum_value(&self, signed: bool) -> i128 {
        // SAFETY: plain accessors on a value type.
        unsafe {
            if signed {
                i128::from(clang_getEnumConstantDeclValue(self.raw))
            } else {
                i128::from(clang_getEnumConstantDeclUnsignedValue(self.raw))
            }
        }
    }

    /// The width of a bit-field, or `None` for an ordinary field.
    pub fn bit_width(&self) -> Option<u32> {
        // SAFETY: plain accessors on a value type.
        unsafe {
            if clang_Cursor_isBitField(self.raw) == 0 {
                return None;
            }
            u32::try_from(clang_getFieldDeclBitWidth(self.raw)).ok()
        }
    }

    /// The offset of a field in bits, when the record is complete.
    pub fn field_offset_bits(&self) -> Option<u64> {
        // SAFETY: plain accessor on a value type.
        u64::try_from(unsafe { clang_Cursor_getOffsetOfField(self.raw) }).ok()
    }

    /// Whether a tag declaration has neither a name nor a typedef name.
    pub fn is_anonymous(&self) -> bool {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_Cursor_isAnonymous(self.raw) != 0 }
    }

    /// Whether a macro definition takes parameters.
    pub fn is_macro_function_like(&self) -> bool {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_Cursor_isMacroFunctionLike(self.raw) != 0 }
    }

    /// Whether a macro is predefined by the compiler.
    pub fn is_macro_builtin(&self) -> bool {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_Cursor_isMacroBuiltin(self.raw) != 0 }
    }

    /// The file an inclusion directive resolved to.
    pub fn included_file(&self) -> Option<File<'tu>> {
        // SAFETY: plain accessor on a value type.
        let raw = unsafe { clang_getIncludedFile(self.raw) };
        if raw.is_null() { None } else { Some(File { raw, _tu: PhantomData }) }
    }

    /// Evaluates the expression (or a variable's initializer) as a constant.
    pub fn evaluate(&self) -> Option<EvalValue> {
        // SAFETY: the result handle is checked for null, read with the
        // accessor matching its kind, and disposed exactly once.
        unsafe {
            let result = clang_Cursor_Evaluate(self.raw);
            if result.is_null() {
                return None;
            }
            let value = match clang_EvalResult_getKind(result) {
                CXEval_Int => Some(EvalValue::Int(if clang_EvalResult_isUnsignedInt(result) != 0 {
                    i128::from(clang_EvalResult_getAsUnsigned(result))
                } else {
                    i128::from(clang_EvalResult_getAsLongLong(result))
                })),
                CXEval_Float => Some(EvalValue::Float(clang_EvalResult_getAsDouble(result))),
                CXEval_StrLiteral => {
                    let ptr = clang_EvalResult_getAsStr(result);
                    (!ptr.is_null()).then(|| EvalValue::Str(CStr::from_ptr(ptr).to_bytes().to_vec()))
                }
                _ => None,
            };
            clang_EvalResult_dispose(result);
            value
        }
    }

    /// The direct children of this cursor, in source order.
    pub fn children(&self) -> Vec<Cursor<'tu>> {
        let mut out: Vec<CXCursor> = Vec::new();
        // SAFETY: `collect_child` only pushes into `out`, which outlives the
        // call and is not otherwise accessed until it returns.
        unsafe { clang_visitChildren(self.raw, collect_child, (&mut out as *mut Vec<CXCursor>).cast::<c_void>()) };
        out.into_iter().map(Cursor::new).collect()
    }
}

/// A C type.
#[derive(Clone, Copy)]
pub(crate) struct Type<'tu> {
    raw: CXType,
    _tu: PhantomData<&'tu ()>,
}

impl<'tu> Type<'tu> {
    /// Wraps a raw type.
    fn new(raw: CXType) -> Self {
        Type { raw, _tu: PhantomData }
    }

    /// The type kind.
    pub fn kind(&self) -> CXTypeKind {
        self.raw.kind
    }

    /// The type as clang would print it, qualifiers included.
    pub fn spelling(&self) -> String {
        // SAFETY: plain accessor; the string is freed by `take_string`.
        take_string(unsafe { clang_getTypeSpelling(self.raw) })
    }

    /// The type with all sugar (typedefs, elaboration) removed.
    pub fn canonical(&self) -> Type<'tu> {
        // SAFETY: plain accessor on a value type.
        Type::new(unsafe { clang_getCanonicalType(self.raw) })
    }

    /// The type an elaborated type (`struct Foo`) names.
    pub fn named(&self) -> Type<'tu> {
        // SAFETY: plain accessor on a value type.
        Type::new(unsafe { clang_Type_getNamedType(self.raw) })
    }

    /// The type a pointer points to.
    pub fn pointee(&self) -> Type<'tu> {
        // SAFETY: plain accessor on a value type.
        Type::new(unsafe { clang_getPointeeType(self.raw) })
    }

    /// The element type of an array.
    pub fn array_element(&self) -> Type<'tu> {
        // SAFETY: plain accessor on a value type.
        Type::new(unsafe { clang_getArrayElementType(self.raw) })
    }

    /// The length of a constant-size array.
    pub fn array_size(&self) -> Option<u64> {
        // SAFETY: plain accessor on a value type.
        u64::try_from(unsafe { clang_getArraySize(self.raw) }).ok()
    }

    /// The value type of an `_Atomic` type.
    pub fn atomic_value(&self) -> Type<'tu> {
        // SAFETY: plain accessor on a value type.
        Type::new(unsafe { clang_Type_getValueType(self.raw) })
    }

    /// The parameter types of a function type, or `None` for other types.
    pub fn arg_types(&self) -> Option<Vec<Type<'tu>>> {
        // SAFETY: plain accessors; `index` stays below the reported count.
        unsafe {
            let count = u32::try_from(clang_getNumArgTypes(self.raw)).ok()?;
            Some((0..count).map(|index| Type::new(clang_getArgType(self.raw, index))).collect())
        }
    }

    /// The return type of a function type.
    pub fn result(&self) -> Type<'tu> {
        // SAFETY: plain accessor on a value type.
        Type::new(unsafe { clang_getResultType(self.raw) })
    }

    /// Whether a function type takes `...`.
    pub fn is_variadic(&self) -> bool {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_isFunctionTypeVariadic(self.raw) != 0 }
    }

    /// Whether `const` is written on this type itself.
    pub fn is_const(&self) -> bool {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_isConstQualifiedType(self.raw) != 0 }
    }

    /// Whether `volatile` is written on this type itself.
    pub fn is_volatile(&self) -> bool {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_isVolatileQualifiedType(self.raw) != 0 }
    }

    /// Whether `restrict` is written on this type itself.
    pub fn is_restrict(&self) -> bool {
        // SAFETY: plain accessor on a value type.
        unsafe { clang_isRestrictQualifiedType(self.raw) != 0 }
    }

    /// `sizeof`, or `None` for incomplete and dependent types.
    pub fn size_of(&self) -> Option<u64> {
        // SAFETY: plain accessor on a value type.
        u64::try_from(unsafe { clang_Type_getSizeOf(self.raw) }).ok()
    }

    /// `alignof`, or `None` for incomplete and dependent types.
    pub fn align_of(&self) -> Option<u64> {
        // SAFETY: plain accessor on a value type.
        u64::try_from(unsafe { clang_Type_getAlignOf(self.raw) }).ok()
    }

    /// The declaration of a record, enum or typedef type.
    pub fn declaration(&self) -> Cursor<'tu> {
        // SAFETY: plain accessor on a value type.
        Cursor::new(unsafe { clang_getTypeDeclaration(self.raw) })
    }

    /// The field declarations of a record type, including the unnamed ones
    /// that hold anonymous members and padding bit-fields.
    pub fn fields(&self) -> Vec<Cursor<'tu>> {
        let mut out: Vec<CXCursor> = Vec::new();
        // SAFETY: `collect_field` only pushes into `out`, which outlives the
        // call and is not otherwise accessed until it returns.
        unsafe { clang_Type_visitFields(self.raw, collect_field, (&mut out as *mut Vec<CXCursor>).cast::<c_void>()) };
        out.into_iter().map(Cursor::new).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::REQUIRED;

    /// Every `clang_*` function called in this module must be listed in
    /// `REQUIRED`, or an old libclang would panic instead of erroring.
    #[test]
    fn every_called_function_is_required() {
        let source = include_str!("ffi.rs");
        let body = &source[source.find("/// Why libclang could not be made available.").unwrap_or(0)..];
        let mut missing = Vec::new();
        for (index, _) in body.match_indices("clang_") {
            let rest = &body[index..];
            let end = rest.find(|c: char| !(c.is_ascii_alphanumeric() || c == '_')).unwrap_or(rest.len());
            let name = &rest[..end];
            let called = rest[end..].starts_with('(') || rest[end..].starts_with("::is_loaded");
            if called && name != "clang_sys" && !REQUIRED.iter().any(|(required, _)| *required == name) {
                missing.push(name.to_string());
            }
        }
        missing.sort();
        missing.dedup();
        assert!(missing.is_empty(), "add to REQUIRED: {missing:?}");
    }
}

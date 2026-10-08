//! What `wid doc` and `wid query` share about their requests: the command
//! line as a source that errors point into (so fixes show the corrected
//! command), finding the package a request names, and the errors for a
//! symbol path that doesn't resolve (E0601–E0604) or a position where
//! `wid query type` finds nothing (E0605).

use std::path::{Path, PathBuf};

use wid_diagnostics::{Applicability, Code, Diagnostic, Edit, FileId, SourceMap, Span, and_list, codes, did_you_mean};
use wid_query::item::kind_words;
use wid_query::{Failure, PositionError};
use wid_sema::PackageId;
use wid_sema::index::{Index, PathErrorKind, Target};

use crate::{Options, find_wid_root};

/// The command a request belongs to, for its messages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Tool {
    /// `wid doc`.
    Doc,
    /// `wid query`.
    Query,
}

/// The command line as messages show it, with the span of each argument
/// and flag.
#[derive(Clone)]
pub(crate) struct CommandLine {
    pub(crate) text: String,
    args: Vec<(u32, u32)>,
    flags: Vec<(String, u32, u32)>,
}

impl CommandLine {
    /// A command line that starts with `command`: `wid doc`.
    pub(crate) fn new(command: &str) -> Self {
        CommandLine { text: command.to_string(), args: Vec::new(), flags: Vec::new() }
    }

    /// Appends an argument written after `prefix` (`-in:` for `-in:DIR`)
    /// and returns its number; its span leaves the prefix out.
    pub(crate) fn arg(&mut self, prefix: &str, arg: &str) -> usize {
        self.text.push(' ');
        self.text.push_str(prefix);
        let start = self.text.len() as u32;
        self.text.push_str(arg);
        self.args.push((start, self.text.len() as u32));
        self.args.len() - 1
    }

    /// Appends a flag, like `-file`.
    pub(crate) fn flag(&mut self, flag: &str) {
        self.text.push(' ');
        let start = self.text.len() as u32;
        self.text.push_str(flag);
        self.flags.push((flag.to_string(), start, self.text.len() as u32));
    }

    /// The span of a flag, or the empty span at the end when it wasn't
    /// added.
    fn flag_span(&self, file: FileId, flag: &str) -> Span {
        match self.flags.iter().find(|(f, _, _)| f == flag) {
            Some(&(_, start, end)) => Span::new(file, start, end),
            None => self.end(file),
        }
    }

    /// The edit that removes a flag and the space before it.
    fn drop_flag(&self, file: FileId, flag: &str) -> Edit {
        let span = self.flag_span(file, flag);
        Edit { span: Span::new(file, span.start.saturating_sub(1), span.end), replacement: String::new() }
    }

    /// Registers the command line as the source `command line`.
    pub(crate) fn add(&self, sources: &mut SourceMap) -> FileId {
        sources.add(PathBuf::from("<command line>"), "command line".to_string(), self.text.clone())
    }

    /// The span of argument `i`.
    pub(crate) fn span(&self, file: FileId, i: usize) -> Span {
        let (start, end) = self.args.get(i).copied().unwrap_or((0, 0));
        Span::new(file, start, end)
    }

    /// The empty span at the end, where a flag is added.
    pub(crate) fn end(&self, file: FileId) -> Span {
        let end = self.text.len() as u32;
        Span::new(file, end, end)
    }
}

/// A diagnostic waiting for the command line's file id.
pub(crate) type Pending = Box<dyn FnOnce(FileId) -> Diagnostic>;

/// Where a request looks for its package.
pub(crate) struct PackageArg<'a> {
    /// The package argument, as written, and its number on the command
    /// line; `None` for the package in `dir`.
    pub(crate) arg: Option<(&'a str, usize)>,
    /// For `wid doc`, a lone argument read as a symbol of the package in
    /// `.`, which the error for a missing package mentions.
    pub(crate) read_as_symbol: Option<&'a str>,
}

/// The directory (or file, with `-file`) a request names.
pub(crate) fn package_target(
    opts: &Options,
    dir: &Path,
    cmd: &CommandLine,
    tool: Tool,
    package: PackageArg,
) -> Result<PathBuf, Pending> {
    let verb = match tool {
        Tool::Doc => "document",
        Tool::Query => "read",
    };
    let file_request = |arg| FileRequest {
        cmd,
        arg,
        code: codes::DOC_UNKNOWN_PACKAGE,
        verb,
        prefix: if tool == Tool::Query { "-in:" } else { "" },
    };
    let Some((text, arg)) = package.arg else {
        if opts.file_mode {
            return file_target(dir, file_request(None));
        }
        if has_wid_files(dir) {
            return Ok(dir.to_path_buf());
        }
        let symbol = package.read_as_symbol.map(str::to_string);
        return Err(Box::new(move |file| no_package_here(file, tool, symbol.as_deref())));
    };
    let text = text.to_string();
    let span_at = cmd.args[arg];
    let span = move |file| Span::new(file, span_at.0, span_at.1);
    let end = cmd.text.len() as u32;
    // An existing path wins over a collection, for `C:\…` on Windows.
    if let Some((collection, rel)) = text.split_once(':').filter(|_| !dir.join(&text).exists()) {
        let root = find_wid_root(opts);
        let base = match collection {
            "core" | "vendor" => root.join(collection),
            other => match opts.collections.get(other) {
                Some(dir) => dir.clone(),
                None => {
                    let mut known = vec!["core".to_string(), "vendor".to_string()];
                    let mut extra: Vec<String> = opts.collections.keys().cloned().collect();
                    extra.sort();
                    known.extend(extra);
                    let collection = collection.to_string();
                    let best = did_you_mean(&collection, known.iter().map(String::as_str)).map(str::to_string);
                    return Err(Box::new(move |file| {
                        let span = Span::new(file, span_at.0, span_at.0 + collection.len() as u32);
                        let mut diag =
                            Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, format!("unknown collection `{collection}`"))
                                .primary(span, "no collection with this name")
                                .note(format!("the collections are {}", and_list(&quote_all(&known))));
                        diag = match best {
                            Some(best) => diag.suggest_replace(
                                format!("a similar collection exists: `{best}`"),
                                span,
                                best,
                                Applicability::MaybeIncorrect,
                            ),
                            None => diag.help(format!("define it with `-collection:{collection}=path/to/dir`")),
                        };
                        diag
                    }));
                }
            },
        };
        let dir = base.join(rel);
        if opts.file_mode {
            return file_target(&base, file_request(Some((&text, arg, clean(&dir)))));
        }
        if !rel.is_empty() && has_wid_files(&dir) {
            return Ok(dir);
        }
        let packages: Vec<String> =
            collection_packages(&base).into_iter().map(|p| format!("{collection}:{p}")).collect();
        let best = did_you_mean(&text, packages.iter().map(String::as_str)).map(str::to_string);
        return Err(Box::new(move |file| {
            let span = span(file);
            let mut diag = Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, format!("package `{text}` not found"))
                .primary(span, "no package with this path");
            if let Some(best) = best {
                diag = diag.suggest_replace(
                    format!("a similar package exists: `{best}`"),
                    span,
                    best,
                    Applicability::MaybeIncorrect,
                );
            }
            if !packages.is_empty() {
                diag = diag.note(format!("the packages there are {}", and_list(&quote_all(&packages))));
            }
            diag
        }));
    }
    let path = clean(&dir.join(&text));
    if opts.file_mode {
        return file_target(dir, file_request(Some((&text, arg, path))));
    }
    if path.is_file() {
        return Err(Box::new(move |file| {
            Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, format!("`{text}` is a file, not a package directory"))
                .primary(span(file), "a package is a directory of `.wid` files")
                .suggest(
                    format!("{verb} the file on its own with `-file`"),
                    vec![Edit { span: Span::new(file, end, end), replacement: " -file".to_string() }],
                    Applicability::MachineApplicable,
                )
        }));
    }
    if has_wid_files(&path) {
        return Ok(path);
    }
    let exists = path.is_dir();
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(dir).to_path_buf();
    let last = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let siblings: Vec<String> = std::fs::read_dir(&parent)
        .map(|rd| {
            let mut names: Vec<String> = rd
                .flatten()
                .filter(|e| has_wid_files(&e.path()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        })
        .unwrap_or_default();
    let best = match text.strip_suffix(last.as_str()) {
        Some(prefix) if !exists => {
            did_you_mean(&last, siblings.iter().map(String::as_str)).map(|best| format!("{prefix}{best}"))
        }
        _ => None,
    };
    Err(Box::new(move |file| {
        let span = span(file);
        let (message, label) = if exists {
            (format!("`{text}` has no `.wid` files"), "this directory holds no package")
        } else {
            (format!("package `{text}` not found"), "no directory with this path")
        };
        let mut diag = Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, message).primary(span, label);
        match best {
            Some(best) => {
                diag = diag.suggest_replace(
                    format!("a similar package exists: `{best}`"),
                    span,
                    best,
                    Applicability::MaybeIncorrect,
                )
            }
            None => {
                diag = diag
                    .help("name a package directory, a `.wid` file with `-file`, or a collection path like `core:fmt`")
            }
        }
        diag
    }))
}

/// The error for a request without a package outside any package.
fn no_package_here(file: FileId, tool: Tool, symbol: Option<&str>) -> Diagnostic {
    let (command, label) = match tool {
        Tool::Doc => ("wid doc", "`wid doc` documents the package in `.` when no package is named"),
        Tool::Query => ("wid query", "`wid query` reads the package in `.` unless `-in:` names one"),
    };
    let span = Span::new(file, 0, command.len() as u32);
    let mut diag = Diagnostic::error(codes::DOC_UNKNOWN_PACKAGE, "there is no package in the current directory")
        .primary(span, label);
    match (tool, symbol) {
        (Tool::Doc, Some(symbol)) => {
            diag = diag.note(format!(
                "`{symbol}` is not a directory, a file or a collection path (like `core:fmt`), so it was read as a symbol of the package in `.`"
            ));
            diag.help(format!("name the package first, like `wid doc path/to/package {symbol}`"))
        }
        (Tool::Doc, None) => {
            diag.help("run `wid doc` in a package directory, or name a package, like `wid doc core:fmt`")
        }
        (Tool::Query, _) => diag.help(
            "run `wid query` in a package directory, or name a package with `-in:`, like `wid query outline -in:core:fmt`",
        ),
    }
}

/// What the errors about `-file`'s argument need to know about the
/// request.
pub(crate) struct FileRequest<'a> {
    /// The command line, with `-file` added.
    pub(crate) cmd: &'a CommandLine,
    /// The file as written, its argument number and the path it names;
    /// `None` when the command line names none.
    pub(crate) arg: Option<(&'a str, usize, PathBuf)>,
    /// The code the errors have: E0206 for `build`, `run`, `check` and
    /// `test`, E0601 for `wid doc` and `wid query`.
    pub(crate) code: Code,
    /// What the command does with the file: `check`, `document`.
    pub(crate) verb: &'a str,
    /// What the command line writes before a file: `-in:` for `wid query`.
    pub(crate) prefix: &'a str,
}

/// The file `-file` names, or the error for no file (with the `.wid` files
/// in `dir`, where the command runs, as candidates), a missing one (with
/// those of its directory), or a directory (with a fix that drops `-file`).
pub(crate) fn file_target(dir: &Path, request: FileRequest) -> Result<PathBuf, Pending> {
    let FileRequest { cmd, arg, code, verb, prefix } = request;
    let (cmd, verb, prefix) = (cmd.clone(), verb.to_string(), prefix.to_string());
    let Some((text, arg, path)) = arg else {
        let files = wid_file_names(dir);
        let package = !files.is_empty();
        return Err(Box::new(move |file| {
            let flag = cmd.flag_span(file, "-file");
            let at = |name: &str| Edit {
                span: Span::new(file, flag.start, flag.start),
                replacement: format!("{prefix}{name} "),
            };
            let mut diag = Diagnostic::error(code, "`-file` needs a `.wid` file to read")
                .primary(flag, "no file is named")
                .note("with `-file`, the package is a single `.wid` file instead of a directory, so the file must be named");
            match files.as_slice() {
                [only] => {
                    diag = diag.suggest(
                        format!("{verb} `{only}`, the only `.wid` file here"),
                        vec![at(only)],
                        Applicability::MaybeIncorrect,
                    )
                }
                _ => {
                    let mut example = cmd.text.clone();
                    example.insert_str(flag.start as usize, &format!("{prefix}main.wid "));
                    let with = if prefix.is_empty() { String::new() } else { format!(" with `{prefix}`") };
                    diag = diag.help(format!("name the file{with}, like `{example}`"));
                    if !files.is_empty() {
                        diag = diag.note(format!("the `.wid` files here are {}", list_names(&files)));
                    }
                }
            }
            if package {
                diag = diag.suggest(
                    format!("or drop `-file` to {verb} the package in `.`"),
                    vec![cmd.drop_flag(file, "-file")],
                    Applicability::MaybeIncorrect,
                );
            }
            diag
        }));
    };
    if path.is_file() {
        return Ok(path);
    }
    let text = text.to_string();
    if path.is_dir() {
        let package = has_wid_files(&path);
        return Err(Box::new(move |file| {
            let diag = Diagnostic::error(code, format!("`{text}` is a directory, not a file"))
                .primary(cmd.span(file, arg), format!("`-file` {verb}s a single `.wid` file"));
            if package {
                diag.suggest(
                    format!("drop `-file` to {verb} the package in `{text}`"),
                    vec![cmd.drop_flag(file, "-file")],
                    Applicability::MachineApplicable,
                )
            } else {
                diag.note(format!("`{text}` holds no `.wid` files")).help("name a `.wid` file instead")
            }
        }));
    }
    let exists = path.exists();
    let last = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(dir).to_path_buf();
    let written_dir = text.strip_suffix(last.as_str()).unwrap_or_default().to_string();
    let files = wid_file_names(&parent);
    // `main` for `main.wid`, then a similar name.
    let with_extension = format!("{last}.wid");
    let best = files
        .iter()
        .find(|f| **f == with_extension)
        .map(String::as_str)
        .or_else(|| did_you_mean(&last, files.iter().map(String::as_str)))
        .map(|best| format!("{written_dir}{best}"));
    Err(Box::new(move |file| {
        let span = cmd.span(file, arg);
        let (message, label) = if exists {
            (format!("`{text}` is not a file"), format!("`-file` {verb}s a single `.wid` file"))
        } else {
            (format!("file `{text}` not found"), "no file with this path".to_string())
        };
        let mut diag = Diagnostic::error(code, message).primary(span, label);
        if let Some(best) = best {
            return diag.suggest_replace(
                format!("a similar file exists: `{best}`"),
                span,
                best,
                Applicability::MaybeIncorrect,
            );
        }
        if !files.is_empty() {
            let place = if written_dir.is_empty() { "here".to_string() } else { format!("in `{written_dir}`") };
            diag = diag.note(format!("the `.wid` files {place} are {}", list_names(&files)));
        }
        diag.help("name an existing `.wid` file")
    }))
}

/// The package directory `wid build`, `run`, `check` or `test` names
/// without `-file`: argument `arg` of `cmd`, written `text`, which is the
/// path `path`. The errors (E0206) point at it: a `.wid` file gets a fix
/// that adds `-file`, another file says it is neither, a missing directory
/// is matched against the package directories next to it, and one without
/// `.wid` files says what it holds.
pub(crate) fn dir_target(cmd: &CommandLine, arg: usize, text: &str, path: &Path) -> Result<PathBuf, Pending> {
    if has_wid_files(path) {
        return Ok(path.to_path_buf());
    }
    let (cmd, text) = (cmd.clone(), text.to_string());
    // `check` in `wid check nothere`.
    let verb = cmd.text.split_whitespace().nth(1).unwrap_or("build").to_string();
    let command = format!("wid {verb}");
    let code = codes::UNKNOWN_IMPORT;
    let parent = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new(".")).to_path_buf();
    if path.is_file() {
        let wid = path.extension().is_some_and(|e| e == "wid");
        let files = wid_file_names(&parent);
        return Err(Box::new(move |file| {
            let span = cmd.span(file, arg);
            if wid {
                let end = cmd.end(file);
                return Diagnostic::error(code, format!("`{text}` is a file; Wid builds packages (directories)"))
                    .primary(span, "a package is a directory of `.wid` files")
                    .suggest(
                        format!("{verb} the file on its own with `-file`"),
                        vec![Edit { span: end, replacement: " -file".to_string() }],
                        Applicability::MachineApplicable,
                    );
            }
            let mut diag =
                Diagnostic::error(code, format!("`{text}` is neither a `.wid` file nor a package directory"))
                    .primary(span, "a package is a directory of `.wid` files");
            let example = files.first().map_or("main.wid", String::as_str);
            let written_dir = text.rsplit_once('/').map_or(String::new(), |(d, _)| format!("{d}/"));
            diag = diag.help(format!(
                "to {verb} a single `.wid` file, name it with `-file`, like `{command} {written_dir}{example} -file`"
            ));
            if !files.is_empty() {
                let place = if written_dir.is_empty() { "here".to_string() } else { format!("in `{written_dir}`") };
                diag = diag.note(format!("the `.wid` files {place} are {}", list_names(&files)));
            }
            diag
        }));
    }
    if path.is_dir() {
        // What it holds: packages in its subdirectories, or other files.
        let mut packages = Vec::new();
        let mut entries = Vec::new();
        if let Ok(rd) = std::fs::read_dir(path) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if e.path().is_dir() {
                    if has_wid_files(&e.path()) {
                        packages.push(name.clone());
                    }
                    entries.push(format!("{name}/"));
                } else {
                    entries.push(name);
                }
            }
        }
        packages.sort();
        entries.sort();
        let base = if text.ends_with('/') { text.clone() } else { format!("{text}/") };
        let packages: Vec<String> = packages.into_iter().map(|p| format!("{base}{p}")).collect();
        return Err(Box::new(move |file| {
            let span = cmd.span(file, arg);
            let diag = Diagnostic::error(code, format!("`{text}` contains no `.wid` files"))
                .primary(span, "this directory holds no package");
            match (packages.as_slice(), entries.is_empty()) {
                ([only], _) => diag.suggest_replace(
                    format!("the package in it is `{only}`"),
                    span,
                    only.clone(),
                    Applicability::MaybeIncorrect,
                ),
                ([], true) => diag.note("it is empty").help("name a package directory, or a `.wid` file with `-file`"),
                ([], false) => diag
                    .note(format!("it holds {}", list_names(&entries)))
                    .help("name a package directory, or a `.wid` file with `-file`"),
                (_, _) => diag.note(format!("the packages in it are {}", list_names(&packages))),
            }
        }));
    }
    // A missing directory: a package directory next to it with a similar
    // name.
    let last = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let mut siblings: Vec<String> = std::fs::read_dir(&parent)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().is_dir() && has_wid_files(&e.path()))
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    siblings.sort();
    let written_dir = text.strip_suffix(last.as_str()).unwrap_or_default().to_string();
    let best = did_you_mean(&last, siblings.iter().map(String::as_str)).map(|b| format!("{written_dir}{b}"));
    let wid_file = parent.join(format!("{last}.wid")).is_file().then(|| format!("{text}.wid"));
    Err(Box::new(move |file| {
        let span = cmd.span(file, arg);
        let diag = Diagnostic::error(code, format!("directory `{text}` does not exist"))
            .primary(span, "no directory with this path");
        if let Some(best) = best {
            return diag.suggest_replace(
                format!("a similar package directory exists: `{best}`"),
                span,
                best,
                Applicability::MaybeIncorrect,
            );
        }
        if let Some(wid) = wid_file {
            let end = cmd.end(file);
            return diag.suggest(
                format!("{verb} the file `{wid}` on its own with `-file`"),
                vec![Edit { span, replacement: wid }, Edit { span: end, replacement: " -file".to_string() }],
                Applicability::MaybeIncorrect,
            );
        }
        diag.help("name a package directory, or a `.wid` file with `-file`")
    }))
}

/// The names of the `.wid` files directly in `dir`, sorted.
fn wid_file_names(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "wid") && e.path().is_file())
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// `dir/./x` without the `.`, so paths print as written.
fn clean(path: &Path) -> PathBuf {
    let out: PathBuf = path.components().filter(|c| !matches!(c, std::path::Component::CurDir)).collect();
    if out.as_os_str().is_empty() { PathBuf::from(".") } else { out }
}

/// Whether a directory holds `.wid` files.
pub(crate) fn has_wid_files(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok_and(|rd| rd.flatten().any(|e| e.path().extension().is_some_and(|x| x == "wid")))
}

/// The packages of a collection, as paths relative to it (`stb/image`),
/// sorted.
fn collection_packages(base: &Path) -> Vec<String> {
    fn walk(dir: &Path, rel: &str, depth: u32, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for entry in rd.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if rel.is_empty() { name } else { format!("{rel}/{name}") };
            if has_wid_files(&path) {
                out.push(rel.clone());
            }
            if depth < 3 {
                walk(&path, &rel, depth + 1, out);
            }
        }
    }
    let mut out = Vec::new();
    walk(base, "", 0, &mut out);
    out.sort();
    out
}

/// What the errors about a symbol path need to know about the request.
pub(crate) struct ErrorContext<'a> {
    pub(crate) index: &'a Index,
    pub(crate) cmd: &'a CommandLine,
    /// The command line's file.
    pub(crate) file: FileId,
    pub(crate) tool: Tool,
    /// The symbol path's argument number.
    pub(crate) arg: usize,
    /// For `wid query`, the query's argument number.
    pub(crate) query_arg: Option<usize>,
    pub(crate) symbol: &'a str,
    /// The only argument was read as a symbol of the package in `.`.
    pub(crate) read_as_symbol: bool,
    /// The package argument, as written.
    pub(crate) package_arg: Option<&'a str>,
}

impl ErrorContext<'_> {
    /// The span of segment `i` of the symbol path.
    fn segment(&self, i: usize) -> Span {
        let base = self.cmd.span(self.file, self.arg);
        let mut start = base.start;
        for (n, seg) in self.symbol.split('.').enumerate() {
            if n == i {
                return Span::new(self.file, start, start + seg.len() as u32);
            }
            start += seg.len() as u32 + 1;
        }
        base
    }

    /// The command that lists what the package declares, for hints.
    fn package_command(&self) -> String {
        match (self.tool, self.package_arg) {
            (Tool::Doc, Some(p)) => format!("wid doc {p}"),
            (Tool::Doc, None) => "wid doc".to_string(),
            (Tool::Query, Some(p)) => format!("wid query outline -in:{p}"),
            (Tool::Query, None) => "wid query outline".to_string(),
        }
    }

    /// The diagnostic for a failed request.
    pub(crate) fn report(&self, failure: Failure) -> Diagnostic {
        let error = match failure {
            Failure::Malformed => {
                let (message, label) = if self.symbol.is_empty() {
                    ("the symbol path is empty".to_string(), "a name goes here")
                } else {
                    (format!("`{}` is not a symbol path", self.symbol), "a name is missing around a `.`")
                };
                return Diagnostic::error(codes::DOC_UNKNOWN_SYMBOL, message)
                    .primary(self.cmd.span(self.file, self.arg), label)
                    .help("write a name, a type and its member, or an import name first: `Ball`, `Ball.update`, `rl.draw_circle_v`");
            }
            Failure::NotAType(target) => return self.not_a_type(&target),
            Failure::Position(error) => return self.position(error),
            Failure::Path(error) => error,
        };
        let span = self.segment(error.segment);
        let name = self.symbol.split('.').nth(error.segment).unwrap_or_default();
        match error.kind {
            PathErrorKind::UnknownName { package, via, candidates } => {
                self.unknown_name(span, name, package, via, &candidates)
            }
            PathErrorKind::NoMember { owner, candidates } => self.no_member(span, name, &owner, &candidates),
            PathErrorKind::NoMembers { target } => {
                let before = self.symbol.split('.').take(error.segment).collect::<Vec<_>>().join(".");
                let what = self.describe(&target);
                let dot = Span::new(self.file, span.start.saturating_sub(1), self.cmd.span(self.file, self.arg).end);
                let fix = match self.tool {
                    Tool::Doc => format!("document `{before}` itself"),
                    Tool::Query => format!("ask about `{before}` itself"),
                };
                Diagnostic::error(codes::DOC_NO_MEMBER, format!("`{before}` has no members"))
                    .primary(span, format!("`{before}` is {what}, so nothing can follow it"))
                    .note("only structs, enums, unions, modules and the type aliases of them have members")
                    .suggest(fix, vec![Edit { span: dot, replacement: String::new() }], Applicability::MaybeIncorrect)
            }
            PathErrorKind::Private { symbol } => {
                let s = self.index.symbol(symbol);
                let scope = if s.owner.is_some() { "its type" } else { "its package" };
                let mut diag =
                    Diagnostic::error(codes::DOC_PRIVATE, format!("`{}` is private", self.index.path_of(symbol)))
                        .primary(span, "private declarations are left out of the documentation");
                if s.span != Span::default() {
                    diag = diag.secondary(s.span, format!("declared `private` here, which hides it outside {scope}"));
                }
                diag.note("`wid doc` documents what code outside can use; `-private` shows the rest too").suggest(
                    "document private declarations too",
                    vec![Edit { span: self.cmd.end(self.file), replacement: " -private".to_string() }],
                    Applicability::MachineApplicable,
                )
            }
        }
    }

    fn unknown_name(
        &self,
        span: Span,
        name: &str,
        package: PackageId,
        via: Option<String>,
        candidates: &[String],
    ) -> Diagnostic {
        let pkg = self.index.package(package);
        let shown = match &via {
            Some(alias) => format!("package `{}` (imported as `{alias}`)", pkg.name),
            None if pkg.path == "." => "this package".to_string(),
            None => format!("package `{}`", pkg.path),
        };
        let mut diag = Diagnostic::error(codes::DOC_UNKNOWN_SYMBOL, format!("no symbol `{name}` in {shown}"))
            .primary(span, format!("not declared in {shown}"));
        if self.read_as_symbol {
            diag = diag.note(format!(
                "`{}` is not a directory, a file or a collection path (like `core:fmt`), so it was read as a symbol of the package in `.`",
                self.symbol
            ));
        }
        // A member of a type in the package, named without its type: an
        // exact match says more than a similar name.
        let owners = if via.is_none() { self.members_named(package, name) } else { Vec::new() };
        let mut suggested = true;
        match owners.as_slice() {
            [only] => {
                diag = diag.suggest_replace(
                    format!("`{only}` has that name; name its type first"),
                    span,
                    only.clone(),
                    Applicability::MaybeIncorrect,
                );
            }
            [] => match did_you_mean(name, candidates.iter().map(String::as_str)) {
                Some(best) => {
                    diag = diag.suggest_replace(
                        format!("a similar name exists: `{best}`"),
                        span,
                        best.to_string(),
                        Applicability::MaybeIncorrect,
                    )
                }
                None => suggested = false,
            },
            many => {
                diag = diag.note(format!("{} have that name", and_list(&quote_all(many))));
                match self.tool {
                    Tool::Doc => suggested = false,
                    Tool::Query => {
                        for path in many {
                            diag = diag.suggest_replace(
                                format!("`{path}` has that name; name its type first"),
                                span,
                                path.clone(),
                                Applicability::MaybeIncorrect,
                            );
                        }
                    }
                }
            }
        }
        if !suggested {
            diag = match (&via, self.tool) {
                (Some(alias), Tool::Doc) => {
                    diag.help(format!("`wid doc {alias}` lists what package `{}` declares", pkg.name))
                }
                (Some(_), Tool::Query) if !candidates.is_empty() => {
                    diag.note(format!("package `{}` declares {}", pkg.name, list_names(candidates)))
                }
                (Some(_), Tool::Query) => diag.note(format!("package `{}` declares no public names", pkg.name)),
                (None, _) => diag.help(format!("`{}` lists what the package declares", self.package_command())),
            };
        }
        diag
    }

    /// The paths of the members named `name` of the package's types, for
    /// a member named without its type: `Ball.update`. `wid doc` leaves out
    /// private ones.
    fn members_named(&self, package: PackageId, name: &str) -> Vec<String> {
        let shown = |id| self.tool == Tool::Query || self.index.is_public(id);
        self.index
            .package(package)
            .items
            .iter()
            .filter(|&&id| shown(id))
            .flat_map(|&id| {
                let s = self.index.symbol(id);
                let mut found: Vec<String> = s
                    .members
                    .iter()
                    .filter(|&&m| shown(m) && self.index.symbol(m).name == name)
                    .map(|&m| self.index.path_of(m))
                    .collect();
                found.extend(s.fields.iter().filter(|f| f.name == name).map(|_| format!("{}.{name}", s.name)));
                found
                    .extend(s.enum_members.iter().filter(|m| m.name == name).map(|m| format!("{}.{}", s.name, m.name)));
                found
            })
            .collect()
    }

    fn no_member(&self, span: Span, name: &str, owner: &Target, candidates: &[String]) -> Diagnostic {
        let (shown, decl) = match owner {
            Target::Symbol(id) => {
                let s = self.index.symbol(*id);
                (format!("{} `{}`", kind_words(s.kind), self.index.path_of(*id)), Some(s.span))
            }
            Target::Builtin(ty) => (format!("builtin type `{ty}`"), None),
            other => (self.describe(other), None),
        };
        let mut diag = Diagnostic::error(codes::DOC_NO_MEMBER, format!("{shown} has no member `{name}`"))
            .primary(span, format!("not a field, member or method of {shown}"));
        if let Some(decl) = decl.filter(|s| *s != Span::default()) {
            diag = diag.secondary(decl, format!("{shown} is declared here"));
        }
        if let Target::Builtin(ty) = owner {
            let tool = match self.tool {
                Tool::Doc => "`wid doc` lists",
                Tool::Query => "`wid query` knows",
            };
            diag = diag.note(format!(
                "for a builtin type, {tool} the methods that extensions (`extend {ty}`) add to it; its builtin methods are described in SPEC.md"
            ));
        }
        match did_you_mean(name, candidates.iter().map(String::as_str)) {
            Some(best) => {
                diag = diag.suggest_replace(
                    format!("a similar member exists: `{best}`"),
                    span,
                    best.to_string(),
                    Applicability::MaybeIncorrect,
                )
            }
            None if candidates.is_empty() => {}
            None => diag = diag.note(format!("its members are {}", list_names(candidates))),
        }
        diag
    }

    /// `wid query methods` of something that isn't a type.
    fn not_a_type(&self, target: &Target) -> Diagnostic {
        let what = self.describe(target);
        let symbol = self.symbol;
        let mut diag = Diagnostic::error(codes::DOC_NO_MEMBER, format!("`{symbol}` has no methods"))
            .primary(self.cmd.span(self.file, self.arg), format!("`{symbol}` is {what}, not a type"));
        if let Target::Symbol(id) = target {
            let s = self.index.symbol(*id);
            if s.span != Span::default() {
                diag = diag.secondary(s.span, format!("{} `{}` is declared here", kind_words(s.kind), symbol));
            }
        }
        diag = diag.note(
            "`wid query methods` lists the methods of a struct, an enum, a union, a module, a type alias of one, or a builtin type",
        );
        match self.query_arg {
            Some(arg) => diag.suggest(
                format!("ask for the declaration of `{symbol}` instead"),
                vec![Edit { span: self.cmd.span(self.file, arg), replacement: "def".to_string() }],
                Applicability::MachineApplicable,
            ),
            None => diag,
        }
    }

    /// The span of part `i` of a position argument: 0 the file, 1 the
    /// line, 2 the column.
    fn position_part(&self, i: usize) -> Span {
        let base = self.cmd.span(self.file, self.arg);
        let mut cuts = self.symbol.rmatch_indices(':').map(|(at, _)| base.start + at as u32);
        let (Some(second), Some(first)) = (cuts.next(), cuts.next()) else { return base };
        match i {
            0 => Span::new(self.file, base.start, first),
            1 => Span::new(self.file, first + 1, second),
            _ => Span::new(self.file, second + 1, base.end),
        }
    }

    /// `wid query type` at a position where nothing is (E0605).
    fn position(&self, error: PositionError) -> Diagnostic {
        let text = self.symbol;
        let arg = self.cmd.span(self.file, self.arg);
        let file = self.symbol.rsplitn(3, ':').nth(2).unwrap_or(text);
        let code = codes::QUERY_NO_POSITION;
        match error {
            PositionError::Malformed => {
                let mut diag = Diagnostic::error(code, format!("`{text}` is not a position"))
                    .primary(arg, "a position is `file:line:column`")
                    .help("write the file as diagnostics show it, then the line and the column, counted from 1: `main.wid:12:5`");
                let symbol_like = !text.is_empty()
                    && text.split('.').all(|s| {
                        s.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_')
                            && s.chars().all(|c| c.is_alphanumeric() || c == '_')
                    });
                if symbol_like && let Some(query) = self.query_arg {
                    diag = diag
                        .note("`wid query type` reads a position; a symbol's declaration is what `def` gives")
                        .suggest(
                            format!("ask for the declaration of `{text}` instead"),
                            vec![Edit { span: self.cmd.span(self.file, query), replacement: "def".to_string() }],
                            Applicability::MaybeIncorrect,
                        );
                }
                diag
            }
            PositionError::UnknownFile { candidates } => {
                let span = self.position_part(0);
                let mut diag = Diagnostic::error(code, format!("no file `{file}` in the program"))
                    .primary(span, "not a file this query read");
                let names = candidates.iter().map(|c| {
                    (Path::new(c).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default(), c)
                });
                let by_name: Vec<(String, &String)> = names.collect();
                let wanted = Path::new(file).file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
                let best =
                    did_you_mean(file, candidates.iter().map(String::as_str)).map(str::to_string).or_else(|| {
                        did_you_mean(&wanted, by_name.iter().map(|(n, _)| n.as_str()))
                            .and_then(|n| by_name.iter().find(|(m, _)| m == n).map(|(_, c)| c.to_string()))
                    });
                match best {
                    Some(best) => {
                        diag = diag.suggest_replace(
                            format!("a similar file exists: `{best}`"),
                            span,
                            best,
                            Applicability::MaybeIncorrect,
                        )
                    }
                    None if !candidates.is_empty() => {
                        diag = diag.note(format!("the package's files are {}", list_names(&candidates)))
                    }
                    None => {}
                }
                diag.help("name the file as diagnostics show it (relative to the current directory), and the package it belongs to with `-in:`")
            }
            PositionError::NoLine { lines } => {
                let s = if lines == 1 { "" } else { "s" };
                Diagnostic::error(code, format!("`{file}` has {lines} line{s}"))
                    .primary(self.position_part(1), "past the end of the file")
                    .help("lines are counted from 1, as in diagnostics")
            }
            PositionError::NoColumn { last } => {
                let line = self.symbol.rsplit(':').nth(1).unwrap_or_default();
                let chars = last - 1;
                let s = if chars == 1 { "" } else { "s" };
                Diagnostic::error(code, format!("line {line} of `{file}` has {chars} character{s}"))
                    .primary(self.position_part(2), "past the end of the line")
                    .help("columns are counted from 1, in characters, as in diagnostics")
            }
            PositionError::Nothing { nearest } => {
                let mut diag = Diagnostic::error(code, format!("nothing at `{text}` has a type"))
                    .primary(arg, "no expression, name, binding or written type is here");
                for (i, n) in nearest.iter().enumerate() {
                    diag = diag.secondary(n.span, if i == 0 { "the nearest code" } else { "nearby code" });
                }
                diag = diag.note(
                    "`wid query type` answers for the code the checker checked: expressions, names, bindings, parameters, written types and the names in declarations; blank space, comments and keywords have no type, and neither has code the checker never reaches, like a generic method that nothing instantiates",
                );
                if let Some(n) = nearest.first() {
                    diag = diag.suggest_replace(
                        format!("ask about the nearest code, `{}`", n.text),
                        arg,
                        format!("{file}:{}:{}", n.line, n.column),
                        Applicability::MaybeIncorrect,
                    );
                }
                diag
            }
        }
    }

    /// What a target is, in prose.
    fn describe(&self, target: &Target) -> String {
        match target {
            Target::Symbol(id) => self.index.symbol(*id).kind.a_describe().to_string(),
            Target::Field { .. } => "a field".to_string(),
            Target::EnumMember { .. } => "an enum member".to_string(),
            Target::Package(_) => "a package".to_string(),
            Target::Builtin(_) => "a builtin type".to_string(),
        }
    }
}

/// Names for a message, at most 30 of them: "`a`, `b` and `c`".
fn list_names(names: &[String]) -> String {
    let mut shown = quote_all(&names[..names.len().min(30)]);
    if names.len() > 30 {
        shown.push(format!("{} more", names.len() - 30));
    }
    and_list(&shown)
}

/// Each name in backticks.
fn quote_all(names: &[String]) -> Vec<String> {
    names.iter().map(|n| format!("`{n}`")).collect()
}

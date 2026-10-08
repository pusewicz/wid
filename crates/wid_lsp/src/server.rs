//! The server: one synchronous loop that reads a message, handles it and
//! replies.
//!
//! Open documents are kept as the client sends them and passed to the
//! driver as an [`Overlay`], so the checker reads unsaved text. A *root* is
//! a package being checked, the one an open document belongs to (see
//! [`RootKey::of`]); its last [`Analysis`] answers requests. A change marks
//! every root that read the file dirty. Dirty roots are checked when the
//! client goes quiet (at once after an open or a save, and
//! [`DEBOUNCE`] after the last change), or before a request about one of
//! their files is answered, so answers are always about the latest text.
//! Each check publishes the diagnostics of every file of the package, and
//! of the other files it has diagnostics for; a file several roots check
//! gets the diagnostics of all of them, without duplicates.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use lsp_types::{
    CodeAction, CodeActionKind, CodeActionOptions, CodeActionOrCommand, CodeActionParams, CodeActionProviderCapability,
    DidChangeTextDocumentParams, DidCloseTextDocumentParams, DidOpenTextDocumentParams, DidSaveTextDocumentParams,
    DocumentFormattingParams, DocumentSymbol, DocumentSymbolParams, DocumentSymbolResponse, GotoDefinitionParams,
    GotoDefinitionResponse, Hover, HoverContents, HoverParams, HoverProviderCapability, InitializeParams,
    InitializeResult, Location, MarkupContent, MarkupKind, MessageType, OneOf, Position, PositionEncodingKind,
    PublishDiagnosticsParams, Range, SaveOptions, ServerCapabilities, ServerInfo, ShowMessageParams, SymbolInformation,
    TextDocumentSyncCapability, TextDocumentSyncKind, TextDocumentSyncOptions, TextDocumentSyncSaveOptions, TextEdit,
    Uri, WorkspaceEdit,
};
use serde::de::DeserializeOwned;
use serde_json::Value;
use wid_diagnostics::FileId;
use wid_driver::fmt::Formatted;
use wid_driver::{Options, Overlay};
use wid_query::{Analysis, TypeItem};

use crate::Config;
use crate::convert::{Converter, Published};
use crate::features;
use crate::position::{Encoding, LineIndex};
use crate::transport::{self, Incoming, RpcError, code};
use crate::uri;

/// How long the server waits after a change for another one before it
/// checks.
pub(crate) const DEBOUNCE: Duration = Duration::from_millis(100);

/// Runs the server until the client sends `exit` or the messages end, and
/// returns the exit status: 0 when the client sent `shutdown` first, 1
/// otherwise.
pub(crate) fn serve(config: Config, messages: &Receiver<Incoming>, out: impl Write) -> i32 {
    Server::new(config, out).serve(messages)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Phase {
    /// Before `initialize`.
    Uninitialized,
    /// Between `initialize` and `shutdown`.
    Running,
    /// After `shutdown`: only `exit` is left.
    ShutDown,
}

/// An open document.
struct Document {
    /// The URI the client opened it with, which the server uses for it.
    uri: Uri,
    /// The file, for a `file:` URI. Other documents can be formatted but
    /// aren't checked.
    path: Option<PathBuf>,
    version: i32,
    text: Arc<str>,
}

/// Which package an open document is checked with.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
struct RootKey {
    /// The package directory, or the file with `file_mode`.
    target: PathBuf,
    /// Check `target` as a single file, as `wid check <file> -file`.
    file_mode: bool,
    /// Include the `_test.wid` files, as `wid test` does.
    testing: bool,
}

impl RootKey {
    /// A `.wid` file is checked with the package of its directory, as `wid
    /// check <dir>` loads it, and a `_test.wid` file with its `_test.wid`
    /// siblings too, as `wid test` loads it. Any other file is checked
    /// alone, as `wid check <file> -file`.
    fn of(path: &Path) -> RootKey {
        match path.parent() {
            Some(dir) if path.extension().is_some_and(|e| e == "wid") => RootKey {
                target: dir.to_path_buf(),
                file_mode: false,
                testing: path.file_stem().is_some_and(|s| s.to_string_lossy().ends_with("_test")),
            },
            _ => RootKey { target: path.to_path_buf(), file_mode: true, testing: false },
        }
    }

    /// Whether `path` is a file of the package itself.
    fn covers(&self, path: &Path) -> bool {
        if self.file_mode { path == self.target } else { path.parent() == Some(self.target.as_path()) }
    }
}

/// A package being checked.
struct Root {
    /// The last check; `None` before the first, or when it crashed.
    analysis: Option<Analysis>,
    /// Whether a file it reads changed since.
    dirty: bool,
    /// Every file on disk the last check read.
    files: HashSet<PathBuf>,
    /// The diagnostics of the last check by file, with an entry for every
    /// file of the package.
    diags: BTreeMap<PathBuf, Vec<Published>>,
}

impl Root {
    fn new() -> Root {
        Root { analysis: None, dirty: true, files: HashSet::new(), diags: BTreeMap::new() }
    }
}

struct Server<W: Write> {
    out: W,
    /// Writing to the client failed: the server stops.
    broken: bool,
    phase: Phase,
    encoding: Encoding,
    /// The client shows Markdown in hovers.
    markdown: bool,
    /// The client takes nested document symbols.
    hierarchical: bool,
    config: Config,
    /// The directory holding `core/`, `vendor/` and `runtime/`.
    wid_root: PathBuf,
    /// `docs/errors` there, when it exists.
    error_docs: Option<PathBuf>,
    /// Open documents, by URI.
    docs: HashMap<String, Document>,
    /// The open documents' text, by path.
    overlay: Overlay,
    roots: BTreeMap<RootKey, Root>,
    /// The diagnostics last published for each file.
    sent: HashMap<PathBuf, Vec<lsp_types::Diagnostic>>,
    /// Messages read but not handled yet.
    pending: VecDeque<Incoming>,
    /// When to check the dirty roots, unless a message comes first.
    deadline: Option<Instant>,
    /// Messages to show the user after the current reply.
    to_show: Vec<(MessageType, String)>,
}

/// Reads a request's or notification's parameters.
fn params<T: DeserializeOwned>(method: &str, params: Value) -> Result<T, RpcError> {
    serde_json::from_value(params)
        .map_err(|e| RpcError::new(code::INVALID_PARAMS, format!("invalid parameters for `{method}`: {e}")))
}

/// A result as JSON.
fn json(value: impl serde::Serialize) -> Result<Value, RpcError> {
    serde_json::to_value(value)
        .map_err(|e| RpcError::new(code::INTERNAL_ERROR, format!("cannot write the result: {e}")))
}

/// The URI of a file: the one an open document has, or a new one.
fn uri_of(docs: &HashMap<String, Document>, path: &Path) -> Option<Uri> {
    match docs.values().find(|d| d.path.as_deref() == Some(path)) {
        Some(doc) => Some(doc.uri.clone()),
        None => uri::from_path(path),
    }
}

/// `path` made absolute against `base` (or the current directory) and
/// cleaned, as the loader spells paths.
fn absolute(path: &Path, base: Option<&Path>) -> PathBuf {
    let joined = match base {
        Some(base) if path.is_relative() => base.join(path),
        _ => std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()),
    };
    uri::clean(&joined)
}

/// Whether two ranges share a position; their ends count.
fn overlaps(a: Range, b: Range) -> bool {
    a.start <= b.end && b.start <= a.end
}

impl<W: Write> Server<W> {
    fn new(mut config: Config, out: W) -> Self {
        let wid_root = absolute(&wid_driver::find_wid_root(&Options::new(".")), None);
        let error_docs = Some(wid_root.join("docs").join("errors")).filter(|d| d.is_dir());
        for path in config.collections.values_mut() {
            *path = absolute(path, None);
        }
        Server {
            out,
            broken: false,
            phase: Phase::Uninitialized,
            encoding: Encoding::Utf16,
            markdown: false,
            hierarchical: false,
            config,
            wid_root,
            error_docs,
            docs: HashMap::new(),
            overlay: Overlay::new(),
            roots: BTreeMap::new(),
            sent: HashMap::new(),
            pending: VecDeque::new(),
            deadline: None,
            to_show: Vec::new(),
        }
    }

    fn serve(&mut self, messages: &Receiver<Incoming>) -> i32 {
        loop {
            if self.broken {
                return 1;
            }
            while let Ok(message) = messages.try_recv() {
                self.accept(message);
            }
            if let Some(message) = self.pending.pop_front() {
                if let Some(status) = self.handle(message) {
                    return status;
                }
                continue;
            }
            let next = match self.deadline {
                None => messages.recv().map_err(|_| RecvTimeoutError::Disconnected),
                Some(at) => messages.recv_timeout(at.saturating_duration_since(Instant::now())),
            };
            match next {
                Ok(message) => self.accept(message),
                Err(RecvTimeoutError::Timeout) => {
                    self.deadline = None;
                    self.check_dirty();
                }
                // The client went away without `exit`.
                Err(RecvTimeoutError::Disconnected) => return if self.phase == Phase::ShutDown { 0 } else { 1 },
            }
        }
    }

    /// Queues a message. A cancellation answers the request it names, if
    /// that request is still waiting, and is otherwise dropped: the server
    /// answers one request at a time, so every other request is answered.
    fn accept(&mut self, message: Incoming) {
        if let Incoming::Notification { method, params } = &message
            && method == "$/cancelRequest"
        {
            let id = params.get("id").cloned().unwrap_or(Value::Null);
            let waiting =
                self.pending.iter().position(|m| matches!(m, Incoming::Request { id: other, .. } if *other == id));
            if let Some(i) = waiting {
                self.pending.remove(i);
                let error = RpcError::new(code::REQUEST_CANCELLED, "the request was cancelled");
                self.send(transport::error_response(&id, &error));
            }
            return;
        }
        self.pending.push_back(message);
    }

    /// Handles a message; `Some(status)` when the server must exit.
    fn handle(&mut self, message: Incoming) -> Option<i32> {
        match message {
            Incoming::Request { id, method, params } => self.request(id, &method, params),
            Incoming::Notification { method, params } => return self.notification(&method, params),
            Incoming::Response => {}
            Incoming::Invalid { id, error } => {
                crate::log(&error.message);
                self.send(transport::error_response(&id, &error));
            }
        }
        None
    }

    fn send(&mut self, body: String) {
        if !self.broken && transport::write_frame(&mut self.out, &body).is_err() {
            self.broken = true;
        }
    }

    fn notify(&mut self, method: &str, params: impl serde::Serialize) {
        match serde_json::to_value(params) {
            Ok(params) => self.send(transport::notification(method, params)),
            Err(e) => crate::log(&format!("cannot write `{method}`: {e}")),
        }
    }

    fn show(&mut self, typ: MessageType, message: String) {
        crate::log(&message);
        self.notify("window/showMessage", ShowMessageParams { typ, message });
    }

    fn request(&mut self, id: Value, method: &str, params: Value) {
        let refused = match self.phase {
            Phase::Uninitialized if method != "initialize" => Some(RpcError::new(
                code::SERVER_NOT_INITIALIZED,
                "the server isn't initialized: send `initialize` first",
            )),
            Phase::ShutDown => Some(RpcError::new(code::INVALID_REQUEST, "the server is shutting down: send `exit`")),
            _ => None,
        };
        let result = match refused {
            Some(error) => Err(error),
            None => catch_unwind(AssertUnwindSafe(|| self.dispatch(method, params))).unwrap_or_else(|_| {
                Err(RpcError::new(
                    code::INTERNAL_ERROR,
                    format!("`wid lsp` crashed answering `{method}`; this is a bug in Wid, please report it"),
                ))
            }),
        };
        match result {
            Ok(value) => self.send(transport::response(&id, value)),
            Err(error) => self.send(transport::error_response(&id, &error)),
        }
        for (typ, message) in std::mem::take(&mut self.to_show) {
            self.show(typ, message);
        }
    }

    fn dispatch(&mut self, method: &str, value: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => {
                if self.phase != Phase::Uninitialized {
                    return Err(RpcError::new(code::INVALID_REQUEST, "the server is already initialized"));
                }
                json(self.initialize(params(method, value)?))
            }
            "shutdown" => {
                self.phase = Phase::ShutDown;
                Ok(Value::Null)
            }
            "textDocument/hover" => json(self.hover(params(method, value)?)),
            "textDocument/definition" => json(self.definition(params(method, value)?)),
            "textDocument/formatting" => json(self.formatting(params(method, value)?)?),
            "textDocument/documentSymbol" => json(self.document_symbols(params(method, value)?)),
            "textDocument/codeAction" => json(self.code_actions(params(method, value)?)),
            _ => Err(RpcError::new(code::METHOD_NOT_FOUND, format!("`wid lsp` has no request `{method}`"))),
        }
    }

    /// Handles a notification; `Some(status)` for `exit`. Notifications it
    /// doesn't know, and every one but `exit` before `initialize`, are
    /// ignored.
    fn notification(&mut self, method: &str, value: Value) -> Option<i32> {
        if method == "exit" {
            return Some(if self.phase == Phase::ShutDown { 0 } else { 1 });
        }
        if self.phase == Phase::Uninitialized {
            return None;
        }
        let handled = match method {
            "textDocument/didOpen" => params(method, value).map(|p| self.did_open(p)),
            "textDocument/didChange" => params(method, value).map(|p| self.did_change(p)),
            "textDocument/didSave" => params(method, value).map(|p| self.did_save(p)),
            "textDocument/didClose" => params(method, value).map(|p| self.did_close(p)),
            _ => Ok(()),
        };
        if let Err(error) = handled {
            crate::log(&error.message);
        }
        None
    }

    fn initialize(&mut self, params: InitializeParams) -> InitializeResult {
        let caps = &params.capabilities;
        let encodings = caps.general.as_ref().and_then(|g| g.position_encodings.as_ref());
        let utf8 = encodings.is_some_and(|list| list.contains(&PositionEncodingKind::UTF8));
        self.encoding = if utf8 { Encoding::Utf8 } else { Encoding::Utf16 };
        let text = caps.text_document.as_ref();
        let formats = text.and_then(|t| t.hover.as_ref()).and_then(|h| h.content_format.as_ref());
        self.markdown = formats.is_some_and(|f| f.contains(&MarkupKind::Markdown));
        let symbols = text.and_then(|t| t.document_symbol.as_ref());
        self.hierarchical = symbols.and_then(|s| s.hierarchical_document_symbol_support).unwrap_or(false);
        let workspace = workspace_root(&params);
        for problem in self.read_options(params.initialization_options.as_ref(), workspace.as_deref()) {
            self.to_show.push((MessageType::WARNING, problem));
        }
        self.phase = Phase::Running;
        InitializeResult {
            capabilities: ServerCapabilities {
                position_encoding: Some(if utf8 { PositionEncodingKind::UTF8 } else { PositionEncodingKind::UTF16 }),
                text_document_sync: Some(TextDocumentSyncCapability::Options(TextDocumentSyncOptions {
                    open_close: Some(true),
                    change: Some(TextDocumentSyncKind::FULL),
                    will_save: None,
                    will_save_wait_until: None,
                    save: Some(TextDocumentSyncSaveOptions::SaveOptions(SaveOptions { include_text: Some(false) })),
                })),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                definition_provider: Some(OneOf::Left(true)),
                document_formatting_provider: Some(OneOf::Left(true)),
                document_symbol_provider: Some(OneOf::Left(true)),
                code_action_provider: Some(CodeActionProviderCapability::Options(CodeActionOptions {
                    code_action_kinds: Some(vec![CodeActionKind::QUICKFIX]),
                    work_done_progress_options: Default::default(),
                    resolve_provider: None,
                })),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo { name: "wid".into(), version: Some(env!("CARGO_PKG_VERSION").into()) }),
        }
    }

    /// Reads `initializationOptions`: `collections` (names to directories,
    /// relative ones against the workspace), `defines` (names to values)
    /// and `target` (`os_arch`), which add to and override the command
    /// line's. Returns what it couldn't read.
    fn read_options(&mut self, options: Option<&Value>, base: Option<&Path>) -> Vec<String> {
        let mut problems = Vec::new();
        let Some(options) = options.filter(|o| !o.is_null()) else { return problems };
        let Some(map) = options.as_object() else {
            problems
                .push("`initializationOptions` must be an object with `collections`, `defines` and `target`".into());
            return problems;
        };
        for (key, value) in map {
            match (key.as_str(), value) {
                ("collections", Value::Object(entries)) => {
                    for (name, path) in entries {
                        match path.as_str() {
                            Some(path) => {
                                self.config.collections.insert(name.clone(), absolute(Path::new(path), base));
                            }
                            None => problems.push(format!("the collection `{name}` needs a path, as a string")),
                        }
                    }
                }
                ("collections", _) => {
                    problems.push("`collections` must map names to paths, like {\"shared\": \"../shared\"}".into())
                }
                ("defines", Value::Object(entries)) => {
                    for (name, value) in entries {
                        let text = match value {
                            Value::String(s) => s.clone(),
                            Value::Bool(_) | Value::Number(_) => value.to_string(),
                            _ => {
                                problems.push(format!("the define `{name}` needs a string, number or boolean"));
                                continue;
                            }
                        };
                        self.config.defines.insert(name.clone(), text);
                    }
                }
                ("defines", _) => {
                    problems.push("`defines` must map names to values, like {\"LOG_LEVEL\": \"2\"}".into())
                }
                ("target", Value::String(text)) => match wid_sema::parse_target(text) {
                    Ok(target) => self.config.target = Some(target),
                    Err(e) => problems.push(format!("`target`: {e}")),
                },
                ("target", _) => problems.push("`target` must be a string like \"linux_amd64\"".into()),
                (other, _) => problems.push(format!(
                    "unknown initialization option `{other}`; the options are `collections`, `defines` and `target`"
                )),
            }
        }
        problems
    }

    fn did_open(&mut self, params: DidOpenTextDocumentParams) {
        let item = params.text_document;
        let path = uri::to_path(&item.uri);
        let text: Arc<str> = Arc::from(item.text);
        if let Some(path) = &path {
            self.overlay.insert(path, text.clone());
            self.roots.entry(RootKey::of(path)).or_insert_with(Root::new);
            self.mark_dirty(path);
            self.check_after(Duration::ZERO);
        }
        let key = item.uri.as_str().to_string();
        self.docs.insert(key, Document { uri: item.uri, path, version: item.version, text });
    }

    fn did_change(&mut self, params: DidChangeTextDocumentParams) {
        let encoding = self.encoding;
        let Some(doc) = self.docs.get_mut(params.text_document.uri.as_str()) else {
            crate::log(&format!("a change to `{}`, which isn't open", params.text_document.uri.as_str()));
            return;
        };
        let mut text = doc.text.to_string();
        for change in params.content_changes {
            match change.range {
                None => text = change.text,
                // Only full changes are asked for; a range is honored anyway.
                Some(range) => {
                    let lines = LineIndex::new(&text);
                    let start = lines.offset(range.start, encoding);
                    let end = lines.offset(range.end, encoding).max(start);
                    text.replace_range(start..end, &change.text);
                }
            }
        }
        doc.version = params.text_document.version;
        doc.text = Arc::from(text);
        if let Some(path) = doc.path.clone() {
            self.overlay.insert(&path, doc.text.clone());
            self.mark_dirty(&path);
            self.check_after(DEBOUNCE);
        }
    }

    fn did_save(&mut self, params: DidSaveTextDocumentParams) {
        let Some(doc) = self.docs.get_mut(params.text_document.uri.as_str()) else { return };
        if let Some(text) = params.text {
            doc.text = Arc::from(text);
        }
        // Other files may have changed on disk too: check again.
        if let Some(path) = doc.path.clone() {
            self.overlay.insert(&path, doc.text.clone());
            self.mark_dirty(&path);
            self.check_after(Duration::ZERO);
        }
    }

    fn did_close(&mut self, params: DidCloseTextDocumentParams) {
        let Some(doc) = self.docs.remove(params.text_document.uri.as_str()) else { return };
        let Some(path) = doc.path else { return };
        if !self.docs.values().any(|d| d.path.as_ref() == Some(&path)) {
            self.overlay.remove(&path);
        }
        self.mark_dirty(&path);
        // A package no open document belongs to is no longer checked, and
        // its diagnostics go.
        let open: HashSet<RootKey> = self.docs.values().filter_map(|d| d.path.as_deref()).map(RootKey::of).collect();
        let gone: Vec<RootKey> = self.roots.keys().filter(|k| !open.contains(*k)).cloned().collect();
        let mut paths = BTreeSet::new();
        for key in gone {
            if let Some(root) = self.roots.remove(&key) {
                paths.extend(root.diags.into_keys());
            }
        }
        self.publish(paths);
        self.check_after(Duration::ZERO);
    }

    /// Marks every root that reads `path`, or would, as dirty.
    fn mark_dirty(&mut self, path: &Path) {
        for (key, root) in &mut self.roots {
            if root.files.contains(path) || key.covers(path) {
                root.dirty = true;
            }
        }
    }

    /// Checks the dirty roots after `delay` without messages; a change
    /// postpones the check, an open or a save brings it forward.
    fn check_after(&mut self, delay: Duration) {
        let at = Instant::now() + delay;
        self.deadline = Some(match self.deadline {
            Some(earlier) if delay.is_zero() => earlier.min(at),
            _ => at,
        });
    }

    fn check_dirty(&mut self) {
        let dirty: Vec<RootKey> = self.roots.iter().filter(|(_, r)| r.dirty).map(|(k, _)| k.clone()).collect();
        for key in dirty {
            self.check(&key);
        }
    }

    fn options(&self, key: &RootKey) -> Options {
        let mut opts = Options::new(&key.target);
        opts.file_mode = key.file_mode;
        opts.command = "check".into();
        opts.testing = key.testing;
        opts.defines = self.config.defines.clone();
        opts.collections = self.config.collections.clone();
        if let Some((os, arch)) = &self.config.target {
            opts.target_os = os.clone();
            opts.target_arch = arch.clone();
        }
        opts.wid_root = Some(self.wid_root.clone());
        opts
    }

    /// Checks a root and publishes its diagnostics.
    fn check(&mut self, key: &RootKey) {
        let opts = self.options(key);
        let overlay = &self.overlay;
        let analysis = match catch_unwind(AssertUnwindSafe(|| wid_driver::analyze(&opts, overlay))) {
            Ok(analysis) => Some(analysis),
            Err(_) => {
                let shown = key.target.display();
                self.show(
                    MessageType::ERROR,
                    format!("checking `{shown}` crashed the compiler; this is a bug in Wid, please report it with the code that triggers it"),
                );
                None
            }
        };
        let mut diags: BTreeMap<PathBuf, Vec<Published>> = BTreeMap::new();
        let mut files = HashSet::new();
        if let Some(analysis) = &analysis {
            for file in analysis.sources.files() {
                if file.path.is_absolute() {
                    files.insert(file.path.clone());
                    if key.covers(&file.path) {
                        diags.entry(file.path.clone()).or_default();
                    }
                }
            }
            // What has no place in a file goes to the open documents of
            // the package, at their start.
            let open: Vec<PathBuf> =
                self.docs.values().filter_map(|d| d.path.clone()).filter(|p| RootKey::of(p) == *key).collect();
            let docs = &self.docs;
            let to_uri = |path: &Path| uri_of(docs, path);
            let mut converter = Converter::new(&analysis.sources, self.encoding, self.error_docs.as_deref(), &to_uri);
            for diag in analysis.diags.iter() {
                let (path, published) = converter.diagnostic(diag);
                let targets = match path {
                    Some(path) => vec![path],
                    None => open.clone(),
                };
                for path in targets {
                    let list = diags.entry(path).or_default();
                    if !list.contains(&published) {
                        list.push(published.clone());
                    }
                }
            }
        }
        let Some(root) = self.roots.get_mut(key) else { return };
        let mut paths: BTreeSet<PathBuf> = root.diags.keys().cloned().collect();
        paths.extend(diags.keys().cloned());
        root.diags = diags;
        root.files = files;
        root.analysis = analysis;
        root.dirty = false;
        self.publish(paths);
    }

    /// Publishes the diagnostics of `paths` that changed since they were
    /// last published: those of every root that has them, without
    /// duplicates. A file no root has any more is cleared.
    fn publish(&mut self, paths: BTreeSet<PathBuf>) {
        for path in paths {
            let mut list: Vec<lsp_types::Diagnostic> = Vec::new();
            let mut covered = false;
            for root in self.roots.values() {
                let Some(published) = root.diags.get(&path) else { continue };
                covered = true;
                for p in published {
                    if !list.contains(&p.diagnostic) {
                        list.push(p.diagnostic.clone());
                    }
                }
            }
            if !covered {
                if self.sent.remove(&path).is_some_and(|old| !old.is_empty()) {
                    self.send_diagnostics(&path, Vec::new());
                }
                continue;
            }
            if self.sent.get(&path) == Some(&list) {
                continue;
            }
            self.send_diagnostics(&path, list.clone());
            self.sent.insert(path, list);
        }
    }

    fn send_diagnostics(&mut self, path: &Path, diagnostics: Vec<lsp_types::Diagnostic>) {
        let Some(uri) = uri_of(&self.docs, path) else { return };
        let version = self.docs.values().find(|d| d.path.as_deref() == Some(path)).map(|d| d.version);
        self.notify("textDocument/publishDiagnostics", PublishDiagnosticsParams { uri, diagnostics, version });
    }

    /// The root of an open document, checked if it is dirty, and its path.
    fn fresh(&mut self, uri: &Uri) -> Option<(RootKey, PathBuf)> {
        let path = self.docs.get(uri.as_str())?.path.clone()?;
        let key = RootKey::of(&path);
        if self.roots.entry(key.clone()).or_insert_with(Root::new).dirty {
            self.check(&key);
        }
        Some((key, path))
    }

    /// The last check of a root, and the file `path` in it.
    fn analysis(&self, key: &RootKey, path: &Path) -> Option<(&Analysis, FileId)> {
        let analysis = self.roots.get(key)?.analysis.as_ref()?;
        Some((analysis, analysis.sources.find_by_path(path)?))
    }

    /// What is at a position: the innermost name or expression there. With
    /// the cursor just past a name (`twice|(x)`), where nothing names a
    /// declaration, it is that name.
    fn found_at(&self, analysis: &Analysis, file: FileId, position: Position) -> Option<TypeItem> {
        let text = &analysis.sources.file(file).text;
        let offset = LineIndex::new(text).offset(position, self.encoding);
        let here = wid_query::type_at_offset(analysis, file, offset as u32);
        if here.as_ref().is_some_and(|found| found.refers_to.is_some()) {
            return here;
        }
        let before = text[..offset].chars().next_back().filter(|c| c.is_alphanumeric() || matches!(c, '_' | '?' | '!'));
        let previous = before.and_then(|c| wid_query::type_at_offset(analysis, file, (offset - c.len_utf8()) as u32));
        match previous {
            Some(found) if found.refers_to.is_some() || here.is_none() => Some(found),
            _ => here,
        }
    }

    fn hover(&mut self, params: HoverParams) -> Option<Hover> {
        let at = params.text_document_position_params;
        let (key, path) = self.fresh(&at.text_document.uri)?;
        let (analysis, file) = self.analysis(&key, &path)?;
        let found = self.found_at(analysis, file, at.position)?;
        let value = features::hover_text(&found, self.markdown)?;
        let kind = if self.markdown { MarkupKind::Markdown } else { MarkupKind::PlainText };
        Some(Hover {
            contents: HoverContents::Markup(MarkupContent { kind, value }),
            range: features::name_range(analysis.sources.file(file), &found, self.encoding),
        })
    }

    fn definition(&mut self, params: GotoDefinitionParams) -> Option<GotoDefinitionResponse> {
        let at = params.text_document_position_params;
        let (key, path) = self.fresh(&at.text_document.uri)?;
        let (analysis, file) = self.analysis(&key, &path)?;
        let found = self.found_at(analysis, file, at.position)?;
        let (target, start, end) = features::declaration(analysis, found.refers_to.as_ref()?)?;
        let range = LineIndex::new(&target.text).range(start, end, self.encoding);
        Some(GotoDefinitionResponse::Scalar(Location { uri: uri_of(&self.docs, &target.path)?, range }))
    }

    /// One edit that replaces the whole document with its canonical text;
    /// none when it is canonical already, or doesn't parse (its errors are
    /// published).
    fn formatting(&mut self, params: DocumentFormattingParams) -> Result<Vec<TextEdit>, RpcError> {
        let Some(doc) = self.docs.get(params.text_document.uri.as_str()) else { return Ok(Vec::new()) };
        match wid_driver::fmt::format_text(FileId(0), &doc.text) {
            Formatted::Text(text) if *text == *doc.text => Ok(Vec::new()),
            Formatted::Text(new_text) => {
                let end = LineIndex::new(&doc.text).end(self.encoding);
                Ok(vec![TextEdit { range: Range { start: Position::default(), end }, new_text }])
            }
            Formatted::Errors(_) => Ok(Vec::new()),
            Formatted::Bug => Err(RpcError::new(
                code::REQUEST_FAILED,
                "formatting this file would change what it means, so it was left unchanged; this is a bug in `wid fmt`, please report it",
            )),
        }
    }

    fn document_symbols(&mut self, params: DocumentSymbolParams) -> Option<DocumentSymbolResponse> {
        let uri = params.text_document.uri;
        let (key, path) = self.fresh(&uri)?;
        let (analysis, file) = self.analysis(&key, &path)?;
        let symbols = features::document_symbols(analysis, file, self.encoding);
        if self.hierarchical {
            return Some(DocumentSymbolResponse::Nested(symbols));
        }
        let mut flat = Vec::new();
        flatten(&symbols, &uri, None, &mut flat);
        Some(DocumentSymbolResponse::Flat(flat))
    }

    /// The quick fixes of the diagnostics at a range: one per help with
    /// edits, preferred when the compiler knows the edits are right.
    // `WorkspaceEdit::changes` is keyed by `Uri`, whose hash is its text,
    // which never changes; the lint sees a cell in its parsed parts.
    #[allow(clippy::mutable_key_type)]
    fn code_actions(&mut self, params: CodeActionParams) -> Option<Vec<CodeActionOrCommand>> {
        let quickfix = CodeActionKind::QUICKFIX;
        if let Some(only) = &params.context.only
            && !only.iter().any(|kind| *kind == quickfix || kind.as_str().is_empty())
        {
            return Some(Vec::new());
        }
        let path = self.docs.get(params.text_document.uri.as_str())?.path.clone()?;
        self.fresh(&params.text_document.uri)?;
        let stale: Vec<RootKey> = self
            .roots
            .iter()
            .filter(|(key, root)| root.dirty && (root.files.contains(&path) || key.covers(&path)))
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale {
            self.check(&key);
        }
        let mut actions: Vec<CodeActionOrCommand> = Vec::new();
        for root in self.roots.values() {
            for published in root.diags.get(&path).into_iter().flatten() {
                if !overlaps(published.diagnostic.range, params.range) {
                    continue;
                }
                for fix in &published.fixes {
                    let mut changes: HashMap<Uri, Vec<TextEdit>> = HashMap::new();
                    for (file, edit) in &fix.edits {
                        let Some(uri) = uri_of(&self.docs, file) else { continue };
                        changes.entry(uri).or_default().push(edit.clone());
                    }
                    let action = CodeActionOrCommand::CodeAction(CodeAction {
                        title: fix.title.clone(),
                        kind: Some(quickfix.clone()),
                        diagnostics: Some(vec![published.diagnostic.clone()]),
                        edit: Some(WorkspaceEdit { changes: Some(changes), ..WorkspaceEdit::default() }),
                        is_preferred: Some(fix.preferred),
                        ..CodeAction::default()
                    });
                    if !actions.contains(&action) {
                        actions.push(action);
                    }
                }
            }
        }
        Some(actions)
    }
}

/// Document symbols as a flat list, for a client that takes no nesting.
#[allow(deprecated)] // `SymbolInformation::deprecated` must be written to build one.
fn flatten(symbols: &[DocumentSymbol], uri: &Uri, container: Option<&str>, out: &mut Vec<SymbolInformation>) {
    for s in symbols {
        out.push(SymbolInformation {
            name: s.name.clone(),
            kind: s.kind,
            tags: None,
            deprecated: None,
            location: Location { uri: uri.clone(), range: s.range },
            container_name: container.map(str::to_string),
        });
        flatten(s.children.as_deref().unwrap_or_default(), uri, Some(&s.name), out);
    }
}

/// The workspace's directory: its first folder, or the root the client
/// names.
fn workspace_root(params: &InitializeParams) -> Option<PathBuf> {
    if let Some(folder) = params.workspace_folders.as_ref().and_then(|f| f.first()) {
        return uri::to_path(&folder.uri);
    }
    #[allow(deprecated)] // Older clients only send `rootUri`.
    let root = params.root_uri.as_ref();
    root.and_then(uri::to_path)
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;
    use std::sync::mpsc::channel;

    use serde_json::{Value, json};

    use super::serve;
    use crate::Config;
    use crate::transport::{Incoming, code, read_frame};

    fn request(id: i64, method: &str, params: Value) -> Incoming {
        Incoming::Request { id: json!(id), method: method.into(), params }
    }

    fn notification(method: &str, params: Value) -> Incoming {
        Incoming::Notification { method: method.into(), params }
    }

    /// Serves `messages`, all sent before the server starts, and returns
    /// the exit status and what the server wrote.
    fn session(messages: Vec<Incoming>) -> (i32, Vec<Value>) {
        let (sender, receiver) = channel();
        for m in messages {
            sender.send(m).expect("the receiver is alive");
        }
        drop(sender);
        let mut out = Vec::new();
        let status = serve(Config::default(), &receiver, &mut out);
        let mut input = Cursor::new(out);
        let mut written = Vec::new();
        while let Ok(Some(body)) = read_frame(&mut input) {
            written.push(serde_json::from_str(&body).expect("the server writes JSON"));
        }
        (status, written)
    }

    fn initialize() -> Incoming {
        request(1, "initialize", json!({"capabilities": {}}))
    }

    #[test]
    fn a_waiting_request_can_be_cancelled() {
        let hover = json!({"textDocument": {"uri": "file:///nowhere/x.wid"}, "position": {"line": 0, "character": 0}});
        let (status, written) = session(vec![
            initialize(),
            request(2, "textDocument/hover", hover.clone()),
            notification("$/cancelRequest", json!({"id": 2})),
            request(3, "textDocument/hover", hover),
            notification("$/cancelRequest", json!({"id": 99})),
            request(4, "shutdown", Value::Null),
            notification("exit", Value::Null),
        ]);
        assert_eq!(status, 0);
        let ids: Vec<&Value> = written.iter().map(|m| &m["id"]).collect();
        assert_eq!(ids, [&json!(2), &json!(1), &json!(3), &json!(4)], "the cancellation answers request 2 at once");
        assert_eq!(written[0]["error"]["code"], code::REQUEST_CANCELLED);
        assert_eq!(written[2]["result"], Value::Null, "a hover of a document that isn't open");
    }

    #[test]
    fn the_lifecycle_is_enforced() {
        let (status, written) = session(vec![
            request(1, "textDocument/hover", json!({})),
            notification("textDocument/didOpen", json!({})),
            request(2, "initialize", json!({"capabilities": {}})),
            request(3, "initialize", json!({"capabilities": {}})),
            request(4, "textDocument/hover", json!({"bad": true})),
            request(5, "workspace/symbol", json!({"query": ""})),
            notification("$/unknown", json!({})),
            notification("textDocument/didOpen", json!({"nonsense": 1})),
            request(6, "shutdown", Value::Null),
            request(7, "textDocument/formatting", json!({})),
        ]);
        assert_eq!(status, 0, "the messages ended after `shutdown`");
        let codes: Vec<(i64, Option<i64>)> =
            written.iter().map(|m| (m["id"].as_i64().unwrap_or(-1), m["error"]["code"].as_i64())).collect();
        assert_eq!(
            codes,
            [
                (1, Some(code::SERVER_NOT_INITIALIZED)),
                (2, None),
                (3, Some(code::INVALID_REQUEST)),
                (4, Some(code::INVALID_PARAMS)),
                (5, Some(code::METHOD_NOT_FOUND)),
                (6, None),
                (7, Some(code::INVALID_REQUEST)),
            ]
        );
        let (status, _) = session(vec![initialize(), notification("exit", Value::Null)]);
        assert_eq!(status, 1, "`exit` without `shutdown`");
    }

    #[test]
    fn initialization_options_are_read_and_problems_shown() {
        let options = json!({
            "collections": {"shared": "lib/shared"},
            "defines": {"LEVEL": 2, "NAME": "x", "BAD": [1]},
            "target": "plan9_amd64",
            "colour": true,
        });
        let params = json!({
            "capabilities": {"general": {"positionEncodings": ["utf-8", "utf-16"]}},
            "initializationOptions": options,
            "workspaceFolders": [{"uri": "file:///work", "name": "work"}],
        });
        let (_, written) = session(vec![request(1, "initialize", params)]);
        assert_eq!(written[0]["result"]["capabilities"]["positionEncoding"], "utf-8");
        let shown: Vec<&str> = written[1..].iter().map(|m| m["params"]["message"].as_str().unwrap_or("")).collect();
        assert_eq!(shown.len(), 3, "{shown:?}");
        assert!(shown.iter().any(|m| m.contains("`BAD`")), "{shown:?}");
        assert!(shown.iter().any(|m| m.contains("unknown operating system `plan9`")), "{shown:?}");
        assert!(shown.iter().any(|m| m.contains("unknown initialization option `colour`")), "{shown:?}");
    }
}

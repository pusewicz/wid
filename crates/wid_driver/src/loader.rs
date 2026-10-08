//! Finding, reading and parsing packages and their imports.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use wid_diagnostics::{Applicability, Diagnostic, Diagnostics, SourceMap, Span, codes, did_you_mean};
use wid_sema::{CBinding, FileInput, PackageId, PackageInput, ProgramInput};
use wid_syntax::ast::ItemKind;

use crate::Options;
use crate::cimport;
use crate::cmdline::{CommandLine, FileRequest, dir_target, file_target};

/// Finds the directory holding `core/`, `vendor/` and `runtime/`.
pub fn find_wid_root(opts: &Options) -> PathBuf {
    if let Some(root) = &opts.wid_root {
        return root.clone();
    }
    if let Ok(root) = std::env::var("WID_ROOT") {
        return PathBuf::from(root);
    }
    if let Ok(exe) = std::env::current_exe() {
        let mut dir = exe.parent().map(Path::to_path_buf);
        while let Some(d) = dir {
            if d.join("core").is_dir() && d.join("runtime").join("wid_runtime.h").is_file() {
                return d;
            }
            dir = d.parent().map(Path::to_path_buf);
        }
    }
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

struct Loader<'o> {
    opts: &'o Options,
    root: PathBuf,
    sources: SourceMap,
    diags: Diagnostics,
    packages: Vec<PackageInput>,
    by_dir: HashMap<PathBuf, PackageId>,
    /// `cimport` items already imported, by package, file and item start.
    cimports_done: HashSet<(usize, usize, u32)>,
}

/// Reads and parses the target package and everything it imports.
pub fn load_program(opts: &Options) -> (SourceMap, Option<ProgramInput>, Diagnostics) {
    let mut loader = Loader {
        opts,
        root: find_wid_root(opts),
        sources: SourceMap::new(),
        diags: Diagnostics::new(),
        packages: Vec::new(),
        by_dir: HashMap::new(),
        cimports_done: HashSet::new(),
    };
    let target = &opts.target;
    let (dir, files) = if opts.file_mode {
        // The errors point into the command line `wid check main.wid -file`.
        let mut cmd = CommandLine::new(&format!("wid {}", opts.command));
        let text = target.to_string_lossy();
        let arg = (!text.is_empty()).then(|| (text.as_ref(), cmd.arg("", &text), target.clone()));
        cmd.flag("-file");
        let verb = match opts.command.as_str() {
            "doc" => "document",
            "query" => "read",
            other => other,
        };
        let request = FileRequest { cmd: &cmd, arg, code: codes::UNKNOWN_IMPORT, verb, prefix: "" };
        if let Err(pending) = file_target(Path::new("."), request) {
            let file = cmd.add(&mut loader.sources);
            loader.diags.push(pending(file));
            return (loader.sources, None, loader.diags);
        }
        let dir = target.parent().map(Path::to_path_buf).unwrap_or_else(|| PathBuf::from("."));
        (dir, vec![target.clone()])
    } else {
        // The errors point into the command line `wid check nothere`.
        let mut cmd = CommandLine::new(&format!("wid {}", opts.command));
        let text = target.to_string_lossy();
        let arg = cmd.arg("", &text);
        if let Err(pending) = dir_target(&cmd, arg, &text, target) {
            let file = cmd.add(&mut loader.sources);
            loader.diags.push(pending(file));
            return (loader.sources, None, loader.diags);
        }
        let files = loader.wid_files(target);
        if files.is_empty() {
            // Only `_test.wid` files, outside `wid test`.
            let file = cmd.add(&mut loader.sources);
            loader.diags.push(
                Diagnostic::error(codes::UNKNOWN_IMPORT, format!("`{text}` holds only tests"))
                    .primary(cmd.span(file, arg), "every `.wid` file here ends in `_test.wid`")
                    .note("`_test.wid` files belong to the package's tests, which only `wid test` reads")
                    .help(format!("run its tests with `wid test {text}`")),
            );
            return (loader.sources, None, loader.diags);
        }
        (target.clone(), files)
    };
    let name = if opts.file_mode {
        target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "main".into())
    } else {
        let canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        canon.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "main".into())
    };
    let path = loader.collection_path(&dir).unwrap_or_else(|| ".".into());
    loader.add_package(package_ident(&name), path, dir, files);
    let prelude_dir = loader.root.join("core").join("builtin");
    let prelude = if prelude_dir.is_dir() {
        let files = loader.wid_files(&prelude_dir);
        let canon = prelude_dir.canonicalize().unwrap_or_else(|_| prelude_dir.clone());
        match loader.by_dir.get(&canon) {
            // The prelude is the package itself (`wid doc core:builtin`),
            // which declares the target too.
            Some(&id) => {
                if !opts.file_mode {
                    loader.add_target_file(id);
                }
                Some(id)
            }
            None if !files.is_empty() => {
                let id = loader.add_package("builtin".into(), "core:builtin".into(), prelude_dir, files);
                loader.add_target_file(id);
                Some(id)
            }
            None => None,
        }
    } else {
        None
    };
    let mut next = 0;
    loop {
        while next < loader.packages.len() {
            loader.resolve_imports(PackageId(next as u32));
            next += 1;
        }
        if !loader.run_cimports() {
            break;
        }
    }
    loader.check_cycles();
    let input = ProgramInput { packages: loader.packages, options: opts.check_options(), prelude };
    (loader.sources, Some(input), loader.diags)
}

/// Returns true for C files Wid generated itself (kept with `-keep-c`), which
/// must not be compiled again as part of the package.
fn is_generated_c(path: &Path) -> bool {
    use std::io::Read;
    let mut head = [0u8; 64];
    let Ok(mut file) = std::fs::File::open(path) else { return false };
    let n = file.read(&mut head).unwrap_or(0);
    head[..n].starts_with(wid_codegen_c::GENERATED_HEADER.as_bytes())
}

/// Turns a directory name into a valid package identifier.
fn package_ident(name: &str) -> String {
    let mut s: String = name.chars().map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' }).collect();
    if s.is_empty() || s.starts_with(|c: char| c.is_ascii_digit()) {
        s.insert(0, '_');
    }
    s
}

impl Loader<'_> {
    /// The import path of a directory inside the `core` or `vendor`
    /// collection, like `core:strings`, so a collection package keeps its
    /// identity when it is the package being built or tested.
    fn collection_path(&self, dir: &Path) -> Option<String> {
        let canon = dir.canonicalize().ok()?;
        for collection in ["core", "vendor"] {
            let base = self.root.join(collection).canonicalize().ok()?;
            if let Ok(rel) = canon.strip_prefix(&base)
                && !rel.as_os_str().is_empty()
            {
                return Some(format!("{collection}:{}", rel.to_string_lossy().replace('\\', "/")));
            }
        }
        None
    }

    /// Adds the prelude's `OS` and `ARCH` constants for the target.
    fn add_target_file(&mut self, prelude: PackageId) {
        let text = format!(
            "# The operating system this program is compiled for, set with `-target:`.\nOS = Os.{}\n\
             # The CPU architecture this program is compiled for, set with `-target:`.\nARCH = Arch.{}\n",
            self.opts.target_os, self.opts.target_arch
        );
        let text: Arc<str> = Arc::from(text);
        let display = "core:builtin/target.wid".to_string();
        let file = self.sources.add(PathBuf::from(&display), display.clone(), text.clone());
        let (ast, diags) = wid_syntax::parse_file(file, &text);
        self.diags.extend(diags);
        self.packages[prelude.0 as usize].files.push(FileInput {
            ast,
            imports: HashMap::new(),
            text,
            display,
            deferred: HashMap::new(),
            struct_literals: Vec::new(),
        });
    }

    fn fatal(&mut self, message: String) {
        self.diags.push(Diagnostic::error(codes::UNKNOWN_IMPORT, message));
    }

    fn wid_files(&self, dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "wid"))
                    .filter(|p| {
                        self.opts.testing || !p.file_stem().is_some_and(|s| s.to_string_lossy().ends_with("_test"))
                    })
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    fn c_files(dir: &Path) -> Vec<PathBuf> {
        let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.flatten()
                    .map(|e| e.path())
                    .filter(|p| p.extension().is_some_and(|e| e == "c" || e == "cpp" || e == "cc"))
                    .filter(|p| !is_generated_c(p))
                    .map(|p| clean_path(&p))
                    .collect()
            })
            .unwrap_or_default();
        files.sort();
        files
    }

    fn add_package(&mut self, name: String, path: String, dir: PathBuf, files: Vec<PathBuf>) -> PackageId {
        let id = PackageId(self.packages.len() as u32);
        let canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        self.by_dir.insert(canon, id);
        let cwd = std::env::current_dir().unwrap_or_default();
        let mut inputs = Vec::new();
        for path in files {
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) => {
                    self.fatal(format!("cannot read `{}`: {e}", path.display()));
                    continue;
                }
            };
            let display = clean_path(path.strip_prefix(&cwd).unwrap_or(&path)).display().to_string();
            let text: Arc<str> = Arc::from(text);
            let file = self.sources.add(path.clone(), display.clone(), text.clone());
            let (ast, diags) = wid_syntax::parse_file(file, &text);
            // The checker reports struct literal errors, fitted to the type.
            let (struct_literals, rest): (Vec<_>, Vec<_>) =
                diags.into_vec().into_iter().partition(|d| d.code == codes::STRUCT_LITERAL);
            for d in rest {
                self.diags.push(d);
            }
            inputs.push(FileInput {
                ast,
                imports: HashMap::new(),
                text,
                display,
                deferred: HashMap::new(),
                struct_literals,
            });
        }
        let c_sources = if self.opts.file_mode && id.0 == 0 { Vec::new() } else { Self::c_files(&dir) };
        self.packages.push(PackageInput { name, path, dir, files: inputs, c_sources, cimport: None });
        id
    }

    fn resolve_imports(&mut self, pkg: PackageId) {
        let p = pkg.0 as usize;
        let dir = self.packages[p].dir.clone();
        let mut requests = Vec::new();
        for (f, file) in self.packages[p].files.iter().enumerate() {
            for (item, conditional) in file.ast.import_items() {
                if let ItemKind::Import(import) = &item.kind {
                    requests.push((f, item.span, import.path.clone(), import.path_span, conditional));
                }
            }
        }
        for (f, item_span, path, span, conditional) in requests {
            let i = item_span.start;
            let saved = conditional.then(|| std::mem::take(&mut self.diags));
            let target = self.resolve_import(&dir, &path, span);
            if let Some(saved) = saved {
                let deferred = std::mem::replace(&mut self.diags, saved).into_vec();
                if !deferred.is_empty() {
                    self.packages[p].files[f].deferred.entry(i).or_default().extend(deferred);
                }
            }
            let Some(target) = target else { continue };
            if target == pkg {
                let name = self.packages[p].name.clone();
                self.diags.push(
                    Diagnostic::error(codes::IMPORT_CYCLE, format!("package `{name}` imports itself"))
                        .primary(span, "this is the package being compiled")
                        .note("every file of a package already sees the declarations of the others")
                        .suggest_replace("remove the import", item_span, "", Applicability::MaybeIncorrect),
                );
                continue;
            }
            self.packages[p].files[f].imports.insert(i, target);
        }
    }

    fn resolve_import(&mut self, from: &Path, path: &str, span: Span) -> Option<PackageId> {
        let (base, rel) = match path.split_once(':') {
            Some(("core", rel)) => (self.root.join("core"), rel),
            Some(("vendor", rel)) => (self.root.join("vendor"), rel),
            Some((collection, rel)) => match self.opts.collections.get(collection) {
                Some(dir) => (dir.clone(), rel),
                None => {
                    let mut defined: Vec<String> = self.opts.collections.keys().cloned().collect();
                    defined.sort_unstable();
                    let mut known: Vec<String> = vec!["core".into(), "vendor".into()];
                    known.extend(defined);
                    self.diags.push(
                        Diagnostic::error(codes::UNKNOWN_IMPORT, format!("unknown collection `{collection}`"))
                            .primary(span, "no collection with this name")
                            .note(format!("known collections: {}", known.join(", ")))
                            .help(format!("define it with `-collection:{collection}=path/to/dir`")),
                    );
                    return None;
                }
            },
            None => (from.to_path_buf(), path),
        };
        let dir = clean_path(&base.join(rel));
        if !dir.is_dir() {
            let mut diag = Diagnostic::error(codes::UNKNOWN_IMPORT, format!("package `{path}` not found"))
                .primary(span, format!("no directory at `{}`", dir.display()));
            let mut siblings: Vec<String> = std::fs::read_dir(dir.parent().unwrap_or(&base))
                .map(|rd| {
                    rd.flatten()
                        .filter(|e| e.path().is_dir())
                        .map(|e| e.file_name().to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_default();
            // A directory lists its entries in no fixed order; ties in a
            // suggestion go to the first.
            siblings.sort_unstable();
            let last = rel.rsplit('/').next().unwrap_or(rel);
            if let Some(best) = did_you_mean(last, siblings.iter().map(String::as_str)) {
                let fixed = format!("\"{}\"", path.replacen(last, best, 1));
                diag =
                    diag.suggest_replace(format!("did you mean `{best}`?"), span, fixed, Applicability::MaybeIncorrect);
            }
            self.diags.push(diag);
            return None;
        }
        let canon = dir.canonicalize().unwrap_or_else(|_| dir.clone());
        if let Some(&id) = self.by_dir.get(&canon) {
            return Some(id);
        }
        let files = self.wid_files(&dir);
        if files.is_empty() {
            self.diags.push(
                Diagnostic::error(codes::UNKNOWN_IMPORT, format!("package `{path}` has no `.wid` files"))
                    .primary(span, format!("{} is empty", dir.display())),
            );
            return None;
        }
        let name = canon.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        Some(self.add_package(package_ident(&name), path.to_string(), dir, files))
    }

    /// Imports every `cimport` not imported yet, adding a package for each.
    /// Returns whether any package was added.
    fn run_cimports(&mut self) -> bool {
        let mut work = Vec::new();
        for (p, pkg) in self.packages.iter().enumerate() {
            if pkg.cimport.is_some() {
                continue;
            }
            for (f, file) in pkg.files.iter().enumerate() {
                for (item, conditional) in file.ast.import_items() {
                    let i = item.span.start;
                    if let ItemKind::Cimport(c) = &item.kind
                        && !self.cimports_done.contains(&(p, f, i))
                    {
                        work.push((p, f, i, c.clone(), conditional));
                    }
                }
            }
        }
        if work.is_empty() {
            return false;
        }
        let root = self.root.clone();
        self.resolve_import(&root, "core:c", Span::default());
        let probes = self.extern_names();
        let mut added = false;
        for (p, f, i, c, conditional) in work {
            self.cimports_done.insert((p, f, i));
            let dir = self.packages[p].dir.clone();
            let saved = conditional.then(|| std::mem::take(&mut self.diags));
            let mut diags = Vec::new();
            let spec = cimport::spec(&c, &dir, &mut diags);
            for d in diags {
                self.diags.push(d);
            }
            if let Some(spec) = spec {
                match cimport::import(&spec, &dir, &probes, &mut self.sources) {
                    Ok(imported) => {
                        let origin = (PackageId(p as u32), f, i as usize);
                        let (source, binding) = cimport::binding(imported, &spec, origin);
                        let id = self.add_cimport_package(&spec, dir, source, binding);
                        self.packages[p].files[f].imports.insert(i, id);
                        added = true;
                    }
                    Err(diags) => {
                        for d in diags {
                            self.diags.push(d);
                        }
                    }
                }
            }
            if let Some(saved) = saved {
                let deferred = std::mem::replace(&mut self.diags, saved).into_vec();
                if !deferred.is_empty() {
                    self.packages[p].files[f].deferred.entry(i).or_default().extend(deferred);
                }
            }
        }
        added
    }

    /// The C names of every `@[extern]` method loaded so far.
    fn extern_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        for pkg in &self.packages {
            for file in &pkg.files {
                for item in &file.ast.items {
                    let ItemKind::Def(f) = &item.kind else { continue };
                    let Some(attr) = item.attr("extern") else { continue };
                    let name = match attr.args.first().map(|a| &a.kind) {
                        Some(wid_syntax::ast::ExprKind::Str(parts)) => parts
                            .iter()
                            .map(|p| match p {
                                wid_syntax::ast::StrPart::Text(t) => t.clone(),
                                wid_syntax::ast::StrPart::Interp(_) => String::new(),
                            })
                            .collect(),
                        _ => f.name.as_str().to_string(),
                    };
                    names.push(name);
                }
            }
        }
        names.sort();
        names.dedup();
        names
    }

    /// Adds the package a `cimport` generates.
    fn add_cimport_package(
        &mut self,
        spec: &cimport::Spec,
        dir: PathBuf,
        source: String,
        binding: CBinding,
    ) -> PackageId {
        let id = PackageId(self.packages.len() as u32);
        let text: Arc<str> = Arc::from(source);
        let display = format!("cimport:{}", spec.header);
        let file = self.sources.add(PathBuf::from(&display), display.clone(), text.clone());
        let (ast, diags) = wid_syntax::parse_file(file, &text);
        self.diags.extend(diags);
        self.packages.push(PackageInput {
            name: spec.alias.clone(),
            path: display.clone(),
            dir,
            files: vec![FileInput {
                ast,
                imports: HashMap::new(),
                text,
                display,
                deferred: HashMap::new(),
                struct_literals: Vec::new(),
            }],
            c_sources: Vec::new(),
            cimport: Some(binding),
        });
        id
    }

    fn check_cycles(&mut self) {
        let n = self.packages.len();
        let edges: Vec<Vec<(PackageId, Span)>> = self
            .packages
            .iter()
            .map(|p| {
                p.files
                    .iter()
                    .flat_map(|f| {
                        f.imports.iter().map(move |(i, target)| {
                            let span = match f.ast.item_at(*i).map(|item| &item.kind) {
                                Some(ItemKind::Import(imp)) => imp.path_span,
                                _ => Span::default(),
                            };
                            (*target, span)
                        })
                    })
                    .collect()
            })
            .collect();
        let mut state = vec![0u8; n];
        let mut stack: Vec<(usize, Span)> = Vec::new();
        for start in 0..n {
            if state[start] == 0 {
                self.dfs(start, &edges, &mut state, &mut stack);
            }
        }
    }

    fn dfs(&mut self, node: usize, edges: &[Vec<(PackageId, Span)>], state: &mut [u8], stack: &mut Vec<(usize, Span)>) {
        state[node] = 1;
        for &(next, span) in &edges[node] {
            let next = next.0 as usize;
            if state[next] == 1 {
                let names: Vec<String> = stack
                    .iter()
                    .skip_while(|(p, _)| *p != next)
                    .map(|(p, _)| self.packages[*p].name.clone())
                    .chain([self.packages[node].name.clone(), self.packages[next].name.clone()])
                    .collect();
                self.diags.push(
                    Diagnostic::error(codes::IMPORT_CYCLE, "packages import each other in a cycle")
                        .primary(span, "this import closes the cycle")
                        .note(format!("cycle: {}", names.join(" -> ")))
                        .help("move the shared declarations into a third package that both can import"),
                );
            } else if state[next] == 0 {
                stack.push((node, span));
                self.dfs(next, edges, state, stack);
                stack.pop();
            }
        }
        state[node] = 2;
    }
}

/// Removes `.` components and folds `dir/..` pairs without touching the file
/// system, so paths print as `physics/body.wid` rather than `././physics/body.wid`.
pub(crate) fn clean_path(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir if matches!(out.components().next_back(), Some(Component::Normal(_))) => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    if out.as_os_str().is_empty() { PathBuf::from(".") } else { out }
}

//! `wid query`: answers about a package, as JSON (SPEC "Toolchain and
//! CLI"). The engine in `wid_query` answers; this module finds the package
//! the request names (for `type` without `-in:`, the one that holds the
//! position's file), loads and checks it with [`crate::analyze`], and
//! turns a failed request into a diagnostic that points into the command
//! line, like `wid doc`'s. It never generates code.

use std::path::{Path, PathBuf};

use wid_diagnostics::{Diagnostics, SourceMap};
pub use wid_query::{Answer, PackageInfo, Query};

use crate::Options;
use crate::cmdline::{CommandLine, ErrorContext, PackageArg, Tool, has_wid_files, package_target};
pub use crate::doc::Printed;

/// What `wid query` was asked.
#[derive(Clone, Debug)]
pub struct QueryRequest {
    /// The query and its argument.
    pub query: Query,
    /// The package (`-in:`): a directory, a file with `-file`, or a
    /// collection path such as `core:fmt`; `None` for the package in `.`.
    pub package: Option<String>,
    /// The directory relative paths are read from, and the package in `.`
    /// is: the current directory when empty.
    pub dir: PathBuf,
}

/// The result of `wid query`.
#[derive(Debug)]
pub struct QueryOutput {
    /// Every source file read, and the command line.
    pub sources: SourceMap,
    /// The package's diagnostics, and why the request failed if it did.
    pub diags: Diagnostics,
    /// The query.
    pub query: Query,
    /// The package asked about, once it is loaded.
    pub package: Option<PackageInfo>,
    /// The answer, unless the request failed.
    pub answer: Option<Answer>,
}

/// Runs `wid query`. `opts.target` is ignored: the package comes from the
/// request.
pub fn query(opts: &Options, request: &QueryRequest) -> QueryOutput {
    let mut cmd = CommandLine::new("wid query");
    let query_arg = cmd.arg("", request.query.name());
    let symbol_arg = request.query.symbol().map(|s| cmd.arg("", s));
    let package_arg = request.package.as_deref().map(|p| (p, cmd.arg("-in:", p)));
    if opts.file_mode {
        cmd.flag("-file");
    }
    let dir = if request.dir.as_os_str().is_empty() { Path::new(".") } else { request.dir.as_path() };
    let holder = match (&request.query, package_arg, symbol_arg) {
        (Query::Type(position), None, Some(arg)) => position_package(opts, dir, position).map(|p| (p, arg)),
        _ => None,
    };
    let package_arg = package_arg.or(holder.as_ref().map(|(p, arg)| (p.as_str(), *arg)));
    let failed = |sources: SourceMap, diags: Diagnostics| QueryOutput {
        sources,
        diags,
        query: request.query.clone(),
        package: None,
        answer: None,
    };
    let package = PackageArg { arg: package_arg, read_as_symbol: None };
    let target = match package_target(opts, dir, &cmd, Tool::Query, package) {
        Ok(target) => target,
        Err(diag) => {
            let mut sources = SourceMap::new();
            let file = cmd.add(&mut sources);
            let mut diags = Diagnostics::new();
            diags.push(diag(file));
            return failed(sources, diags);
        }
    };
    let mut opts = opts.clone();
    opts.target = target;
    opts.testing = false;
    let mut analysis = crate::analyze(&opts, &crate::Overlay::new());
    let file = cmd.add(&mut analysis.sources);
    let Some(root) = analysis.root() else {
        return failed(analysis.sources, analysis.diags);
    };
    let package = Some(analysis.items(true).package_info(root));
    let answer = match wid_query::answer(&analysis, root, &request.query) {
        Ok(answer) => Some(answer),
        Err(failure) => {
            let ctx = ErrorContext {
                index: &analysis.index,
                cmd: &cmd,
                file,
                tool: Tool::Query,
                arg: symbol_arg.unwrap_or(query_arg),
                query_arg: Some(query_arg),
                symbol: request.query.symbol().unwrap_or_default(),
                read_as_symbol: false,
                package_arg: package_arg.map(|(p, _)| p),
            };
            let diag = ctx.report(failure);
            analysis.diags.push(diag);
            None
        }
    };
    analysis.diags.sort();
    QueryOutput { sources: analysis.sources, diags: analysis.diags, query: request.query.clone(), package, answer }
}

/// For `type` without `-in:`, the package that holds the position's file:
/// its directory, or with `-file`, the file itself. `None` when that is
/// the package in `dir` or holds no package.
fn position_package(opts: &Options, dir: &Path, position: &str) -> Option<String> {
    let file = wid_query::Position::parse(position)?.file;
    if opts.file_mode {
        return dir.join(&file).is_file().then_some(file);
    }
    let parent = Path::new(&file).parent().filter(|p| !p.as_os_str().is_empty())?;
    has_wid_files(&dir.join(parent)).then(|| parent.to_string_lossy().into_owned())
}

/// Renders a result the way `wid query` prints it: the JSON document on
/// stdout, always, and the diagnostics as JSON on stderr when there are
/// any.
pub fn print(out: &QueryOutput) -> Printed {
    let document = wid_query::json::document(&out.query, out.package.as_ref(), out.answer.as_ref());
    let stdout = wid_query::json::render(&document) + "\n";
    let stderr = if out.diags.is_empty() {
        String::new()
    } else {
        wid_diagnostics::render_json(&out.diags, &out.sources) + "\n"
    };
    Printed { stdout, stderr, success: out.answer.is_some() && !out.diags.has_errors() }
}

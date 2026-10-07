//! The compiler driver: finds and parses packages, runs semantic analysis and
//! code generation, and invokes the host C compiler.

mod cimport;
mod loader;
mod test;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use wid_diagnostics::{Diagnostic, Diagnostics, SourceMap, codes};
use wid_sema::ir::Program;
use wid_sema::{CheckOptions, ProgramInput};

pub use loader::{find_wid_root, load_program};
pub use test::{TestResult, TestRun, TestStatus, render_test_report, test};

/// The runtime header, embedded so the compiler works from any directory.
pub const RUNTIME_HEADER: &str = include_str!("../../../runtime/wid_runtime.h");

/// Optimization levels, spelled as in Odin's `-o:` flag.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum OptLevel {
    /// `-O0`.
    None,
    /// `-O1`.
    #[default]
    Minimal,
    /// `-Os`.
    Size,
    /// `-O2`.
    Speed,
    /// `-O3`.
    Aggressive,
}

impl OptLevel {
    /// Parses an `-o:` value.
    pub fn parse(text: &str) -> Option<OptLevel> {
        Some(match text {
            "none" => OptLevel::None,
            "minimal" => OptLevel::Minimal,
            "size" => OptLevel::Size,
            "speed" => OptLevel::Speed,
            "aggressive" => OptLevel::Aggressive,
            _ => return None,
        })
    }

    fn flag(self) -> &'static str {
        match self {
            OptLevel::None => "-O0",
            OptLevel::Minimal => "-O1",
            OptLevel::Size => "-Os",
            OptLevel::Speed => "-O2",
            OptLevel::Aggressive => "-O3",
        }
    }
}

/// Everything that controls one compilation.
#[derive(Clone, Debug)]
pub struct Options {
    /// The package directory, or a single file with `file_mode`.
    pub target: PathBuf,
    /// Treat `target` as a single-file package.
    pub file_mode: bool,
    /// The output executable path.
    pub out: Option<PathBuf>,
    /// The C optimization level.
    pub opt: OptLevel,
    /// Debug build: debug info, overflow checks, `#line` directives.
    pub debug: bool,
    /// Keep the generated C next to the output.
    pub keep_c: bool,
    /// The C compiler to use.
    pub cc: Option<String>,
    /// `-define:NAME=value` constants.
    pub defines: HashMap<String, String>,
    /// The operating system to compile for (`-target:`), like `darwin`.
    pub target_os: String,
    /// The architecture to compile for (`-target:`), like `arm64`.
    pub target_arch: String,
    /// `-collection:name=path` roots.
    pub collections: HashMap<String, PathBuf>,
    /// Emit bounds checks.
    pub bounds_checks: bool,
    /// Compile the C with strict warnings as errors (used by the test suite).
    pub strict_c: bool,
    /// `-sanitize:` values.
    pub sanitize: Vec<String>,
    /// Build test procedures (`wid test`).
    pub testing: bool,
    /// Check every function of every package.
    pub check_all_packages: bool,
    /// Override for the Wid root directory.
    pub wid_root: Option<PathBuf>,
}

impl Options {
    /// Default options for compiling `target`.
    pub fn new(target: impl Into<PathBuf>) -> Self {
        Options {
            target: target.into(),
            file_mode: false,
            out: None,
            opt: OptLevel::Minimal,
            debug: false,
            keep_c: false,
            cc: None,
            defines: HashMap::new(),
            target_os: wid_sema::host_os(),
            target_arch: wid_sema::host_arch(),
            collections: HashMap::new(),
            bounds_checks: true,
            strict_c: false,
            sanitize: Vec::new(),
            testing: false,
            check_all_packages: false,
            wid_root: None,
        }
    }

    fn check_options(&self) -> CheckOptions {
        CheckOptions {
            bounds_checks: self.bounds_checks,
            overflow_checks: self.debug,
            defines: self.defines.clone(),
            os: self.target_os.clone(),
            arch: self.target_arch.clone(),
            debug: self.debug,
            testing: self.testing,
            check_all_packages: self.check_all_packages,
        }
    }
}

/// What `wid cimport --dump` imports.
#[derive(Clone, Debug, Default)]
pub struct DumpRequest {
    /// The header: a path, or a name on the include path.
    pub header: String,
    /// Prefixes to remove from every name.
    pub strip_prefixes: Vec<String>,
    /// Macros to define first, as `NAME` or `NAME=value`.
    pub defines: Vec<String>,
    /// Directories to search for headers.
    pub include_dirs: Vec<PathBuf>,
    /// pkg-config packages whose flags to use.
    pub pkg_config: Vec<String>,
}

/// Imports a header the way `cimport` does and returns the Wid source it
/// becomes, or the diagnostics explaining why it can't be imported.
pub fn cimport_dump(request: &DumpRequest) -> (SourceMap, Result<String, Diagnostics>) {
    let mut sources = SourceMap::new();
    let line = format!("wid cimport --dump {}", request.header);
    let start = (line.len() - request.header.len()) as u32;
    let file = sources.add(PathBuf::from("<command line>"), "command line".to_string(), line.clone());
    let header_span = wid_diagnostics::Span::new(file, start, line.len() as u32);
    let dir = std::env::current_dir().unwrap_or_default();
    let spec = cimport::Spec {
        header: request.header.clone(),
        header_span,
        alias: String::new(),
        naming: cimport::Naming { strip_prefixes: request.strip_prefixes.clone(), ..Default::default() },
        defines: request.defines.clone(),
        implement: None,
        link_libs: Vec::new(),
        link_flags: Vec::new(),
        pkg_config: request.pkg_config.iter().map(|p| (p.clone(), wid_diagnostics::Span::default())).collect(),
        include_dirs: request
            .include_dirs
            .iter()
            .map(|d| std::path::absolute(d).unwrap_or_else(|_| d.clone()))
            .collect(),
    };
    let result = match cimport::import(&spec, &dir, &[], &mut sources) {
        Ok(imported) => Ok(imported.rendered.source),
        Err(list) => {
            let mut diags = Diagnostics::new();
            for d in list {
                diags.push(d);
            }
            Err(diags)
        }
    };
    (sources, result)
}

/// The result of checking a program.
pub struct Checked {
    /// Every source file that was read.
    pub sources: SourceMap,
    /// All diagnostics, sorted.
    pub diags: Diagnostics,
    /// The parsed input, when loading succeeded.
    pub input: Option<ProgramInput>,
    /// The lowered program, when there were no errors.
    pub program: Option<Program>,
}

/// Loads, parses and checks a program.
pub fn check(opts: &Options) -> Checked {
    let (mut sources, input, mut diags) = load_program(opts);
    let Some(input) = input else {
        diags.sort();
        return Checked { sources, diags, input: None, program: None };
    };
    let (mut program, sema_diags) = wid_sema::check_program(&input);
    // Spans in code macros generated name these expansions.
    sources.set_expansions(std::mem::take(&mut program.expansions));
    diags.extend(sema_diags);
    diags.sort();
    let program = if diags.has_errors() { None } else { Some(program) };
    Checked { sources, diags, input: Some(input), program }
}

/// The result of building an executable.
pub struct Built {
    /// The checked program and its diagnostics.
    pub checked: Checked,
    /// The executable, when the build succeeded.
    pub exe: Option<PathBuf>,
    /// The generated C file, when it was kept.
    pub c_file: Option<PathBuf>,
}

/// Generates C for a checked program.
pub fn generate_c(program: &Program, sources: &SourceMap, opts: &Options) -> String {
    wid_codegen_c::generate(
        program,
        sources,
        &wid_codegen_c::Options { line_directives: opts.debug, runtime_header: "wid_runtime.h".into() },
    )
}

static BUILD_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Creates a fresh scratch directory for one build.
pub(crate) fn scratch_dir() -> std::io::Result<PathBuf> {
    let n = BUILD_COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("wid-build-{}-{n}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Returns the default output path for a target.
pub fn default_output(opts: &Options) -> PathBuf {
    let stem = if opts.file_mode {
        opts.target.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "main".into())
    } else {
        let canon = opts.target.canonicalize().unwrap_or_else(|_| opts.target.clone());
        canon.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "main".into())
    };
    let name = if cfg!(windows) { format!("{stem}.exe") } else { stem };
    PathBuf::from(name)
}

/// Checks, generates C and compiles an executable.
pub fn build(opts: &Options) -> Built {
    let mut checked = check(opts);
    let Some(program) = &checked.program else {
        return Built { checked, exe: None, c_file: None };
    };
    let (host_os, host_arch) = (wid_sema::host_os(), wid_sema::host_arch());
    if opts.target_os != host_os || opts.target_arch != host_arch {
        let target = format!("{}_{}", opts.target_os, opts.target_arch);
        checked.diags.push(
            Diagnostic::error(codes::CROSS_TARGET, format!("cannot build for `{target}` on `{host_os}_{host_arch}`"))
                .note("Wid builds with the host C compiler, which makes programs for the host only")
                .help(format!("check the code for that target with `wid check -target:{target}`, and build it on a {target} machine")),
        );
        return Built { checked, exe: None, c_file: None };
    }
    let c_source = generate_c(program, &checked.sources, opts);
    let out = opts.out.clone().unwrap_or_else(|| default_output(opts));
    match compile_c(&c_source, program, &out, opts) {
        Ok(c_file) => Built { checked, exe: Some(out), c_file },
        Err(diag) => {
            checked.diags.push(diag);
            Built { checked, exe: None, c_file: None }
        }
    }
}

/// Picks the C compiler: the option, `WID_CC`, `CC`, then `cc`.
pub fn c_compiler(opts: &Options) -> String {
    opts.cc
        .clone()
        .or_else(|| std::env::var("WID_CC").ok())
        .or_else(|| std::env::var("CC").ok())
        .unwrap_or_else(|| "cc".into())
}

/// Picks the C++ compiler for `.cpp` files: `WID_CXX`, `CXX`, then the C++
/// sibling of the C compiler (`clang` → `clang++`, `gcc-16` → `g++-16`).
pub fn cxx_compiler(opts: &Options) -> String {
    if let Ok(cxx) = std::env::var("WID_CXX").or_else(|_| std::env::var("CXX")) {
        return cxx;
    }
    let cc = c_compiler(opts);
    let (dir, file) = match cc.rfind('/') {
        Some(i) => cc.split_at(i + 1),
        None => ("", cc.as_str()),
    };
    let file = if let Some(rest) = file.strip_prefix("clang") {
        format!("clang++{rest}")
    } else if let Some(rest) = file.strip_prefix("gcc") {
        format!("g++{rest}")
    } else if file == "cc" {
        "c++".to_string()
    } else {
        file.to_string()
    };
    format!("{dir}{file}")
}

/// A failed compiler invocation.
struct StepFailure {
    diag: Diagnostic,
    /// Whether the generated C, rather than the user's code, was at fault.
    generated: bool,
}

/// The outcome of a compiler invocation.
type Step = Result<(), Box<StepFailure>>;

/// Compiles the generated C, the package's own C and C++ files, and links
/// them into `out`. Each source is compiled separately so an error names the
/// file it comes from. Returns where the generated C was kept, if anywhere.
fn compile_c(source: &str, program: &Program, out: &Path, opts: &Options) -> Result<Option<PathBuf>, Diagnostic> {
    let fail = |message: String| Diagnostic::error(codes::C_COMPILER_FAILED, message);
    let dir = scratch_dir().map_err(|e| fail(format!("cannot create a build directory: {e}")))?;
    let c_path = dir.join("program.c");
    std::fs::write(&c_path, source).map_err(|e| fail(format!("cannot write {}: {e}", c_path.display())))?;
    std::fs::write(dir.join("wid_runtime.h"), RUNTIME_HEADER)
        .map_err(|e| fail(format!("cannot write the runtime header: {e}")))?;
    let result = compile_and_link(&dir, &c_path, program, out, opts);
    let kept = if opts.keep_c {
        let target = out.with_extension("c");
        let _ = std::fs::copy(&c_path, &target);
        let _ = std::fs::write(target.with_file_name("wid_runtime.h"), RUNTIME_HEADER);
        Some(target)
    } else {
        None
    };
    match result {
        Ok(()) => {
            let _ = std::fs::remove_dir_all(&dir);
            Ok(kept)
        }
        Err(failure) if failure.generated => {
            let at = kept.unwrap_or(c_path);
            Err(failure.diag.note(format!("the generated C is at {}", at.display())))
        }
        Err(failure) => {
            let _ = std::fs::remove_dir_all(&dir);
            Err(failure.diag)
        }
    }
}

/// Runs the compile and link steps inside the scratch directory `dir`.
fn compile_and_link(dir: &Path, c_path: &Path, program: &Program, out: &Path, opts: &Options) -> Step {
    let cc = c_compiler(opts);
    let cxx = cxx_compiler(opts);
    let common = |cmd: &mut Command| {
        cmd.arg(opts.opt.flag());
        if opts.debug {
            cmd.arg("-g");
        }
        for s in &opts.sanitize {
            cmd.arg(format!("-fsanitize={s}"));
        }
        cmd.arg("-I").arg(dir);
        for flag in &program.c_flags {
            cmd.arg(flag);
        }
    };
    let mut objects = Vec::new();

    let object = dir.join("program.o");
    let mut cmd = Command::new(&cc);
    cmd.arg("-std=c23").arg("-fwrapv").arg("-fno-strict-aliasing");
    common(&mut cmd);
    if opts.strict_c {
        cmd.args(["-Wall", "-Wextra", "-Wpedantic", "-Werror"]);
    }
    cmd.arg("-c").arg(c_path).arg("-o").arg(&object);
    run_step(cmd, &cc, true, |stderr| {
        Diagnostic::error(codes::C_COMPILER_FAILED, format!("the C compiler `{cc}` rejected the generated C"))
            .note(stderr)
            .help("this is a bug in the Wid compiler; please report it with the program that triggers it")
    })?;
    objects.push(object);

    for (i, include) in program.c_includes.iter().enumerate() {
        let Some(unit) = wid_codegen_c::implementation_unit(include) else { continue };
        let path = dir.join(format!("implement_{i}.c"));
        std::fs::write(&path, unit).map_err(|e| {
            let diag = Diagnostic::error(codes::C_COMPILER_FAILED, format!("cannot write {}: {e}", path.display()));
            Box::new(StepFailure { diag, generated: false })
        })?;
        let object = dir.join(format!("implement_{i}.o"));
        let mut cmd = Command::new(&cc);
        cmd.arg("-std=c23");
        common(&mut cmd);
        cmd.arg("-c").arg(&path).arg("-o").arg(&object);
        let header = include.header.clone();
        let implement = include.implement.clone().unwrap_or_default();
        run_step(cmd, &cc, false, |stderr| {
            Diagnostic::error(codes::C_COMPILER_FAILED, format!("the implementation of {header} does not compile"))
                .note(stderr)
                .help(format!("`implement: \"{implement}\"` compiles the library's code from the header; check the macros it needs with `define:`"))
        })?;
        objects.push(object);
    }

    let mut uses_cxx = false;
    for (i, src) in program.c_sources.iter().enumerate() {
        let is_cxx = src.extension().is_some_and(|e| e != "c");
        uses_cxx |= is_cxx;
        let compiler = if is_cxx { &cxx } else { &cc };
        let stem = src.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        let object = dir.join(format!("{i}_{stem}.o"));
        let mut cmd = Command::new(compiler);
        cmd.arg(if is_cxx { "-std=c++20" } else { "-std=c23" });
        common(&mut cmd);
        cmd.arg("-c").arg(src).arg("-o").arg(&object);
        let shown = src.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        run_step(cmd, compiler, false, |stderr| {
            Diagnostic::error(codes::C_COMPILER_FAILED, format!("`{shown}` does not compile"))
                .note(stderr)
                .help(format!("fix the error in {}; it is compiled with every build of its package", src.display()))
        })?;
        objects.push(object);
    }

    let linker = if uses_cxx { &cxx } else { &cc };
    let mut cmd = Command::new(linker);
    for s in &opts.sanitize {
        cmd.arg(format!("-fsanitize={s}"));
    }
    cmd.arg("-o").arg(out).args(&objects);
    for flag in &program.link_flags {
        cmd.arg(flag);
    }
    for lib in &program.link_libs {
        cmd.arg(format!("-l{lib}"));
    }
    cmd.arg("-lm");
    run_step(cmd, linker, false, |stderr| {
        let mut diag = Diagnostic::error(codes::C_COMPILER_FAILED, format!("linking `{}` failed", out.display()))
            .note(stderr.clone());
        if stderr.contains("ndefined symbol") || stderr.contains("ndefined reference") {
            diag = diag.help(
                "a declared C function has no definition: add the library with `link:` on its `cimport`, or the `.c` file that defines it to the package",
            );
        }
        diag
    })
}

/// Runs one compiler command, turning a failure into a diagnostic built by
/// `on_error` from the compiler's output. `generated` marks the step that
/// compiles Wid's own output.
fn run_step(mut cmd: Command, program: &str, generated: bool, on_error: impl FnOnce(String) -> Diagnostic) -> Step {
    let output = cmd.output().map_err(|e| {
        let diag = Diagnostic::error(codes::C_COMPILER_FAILED, format!("cannot run the C compiler `{program}`: {e}"))
            .help("install clang (or gcc 15+), or choose a compiler with `-cc:path` or the WID_CC variable");
        Box::new(StepFailure { diag, generated: false })
    })?;
    if output.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&output.stderr).trim_end().to_string();
    if text.is_empty() {
        text = String::from_utf8_lossy(&output.stdout).trim_end().to_string();
    }
    Err(Box::new(StepFailure { diag: on_error(text), generated }))
}

//! Odin-style argument parsing: `wid <command> [target] [-flag[:value]] [-- args]`.

use std::collections::HashMap;
use std::path::PathBuf;

use wid_driver::OptLevel;

/// The subcommands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    Build,
    Run,
    Check,
    Test,
    Doc,
    Explain,
    Cimport,
    Version,
    Help,
}

/// Parsed command line.
#[derive(Clone, Debug)]
pub struct Parsed {
    pub command: Command,
    pub target: Option<PathBuf>,
    pub help_topic: Option<String>,
    pub file_mode: bool,
    pub out: Option<PathBuf>,
    pub opt: OptLevel,
    pub debug: bool,
    pub keep_c: bool,
    pub cc: Option<String>,
    pub defines: HashMap<String, String>,
    /// `-target:os_arch`.
    pub target_os_arch: Option<(String, String)>,
    pub collections: HashMap<String, PathBuf>,
    pub no_bounds_check: bool,
    pub sanitize: Vec<String>,
    pub json_errors: bool,
    pub filter: Option<String>,
    pub program_args: Vec<String>,
    /// `-define:` values as written, for `wid cimport`.
    pub raw_defines: Vec<String>,
    /// `wid cimport -strip-prefix:`.
    pub strip_prefixes: Vec<String>,
    /// `wid cimport -include-dir:`.
    pub include_dirs: Vec<PathBuf>,
    /// `wid cimport -pkg-config:`.
    pub pkg_config: Vec<String>,
    /// `wid doc`'s positional arguments: a package, a symbol, or both.
    pub doc_args: Vec<String>,
    /// `wid doc -json`.
    pub json: bool,
    /// `wid doc -private`.
    pub private: bool,
}

const COMMANDS: &[&str] = &["build", "run", "check", "test", "doc", "explain", "cimport", "version", "help"];

const FLAGS: &[&str] = &[
    "-file",
    "-out:",
    "-o:",
    "-debug",
    "-keep-c",
    "-cc:",
    "-define:",
    "-target:",
    "-collection:",
    "-no-bounds-check",
    "-sanitize:",
    "-json-errors",
    "-filter:",
    "-dump",
    "-strip-prefix:",
    "-include-dir:",
    "-pkg-config:",
    "-json",
    "-private",
];

/// Parses the arguments after the program name.
pub fn parse(argv: &[String]) -> Result<Parsed, String> {
    let mut parsed = Parsed {
        command: Command::Help,
        target: None,
        help_topic: None,
        file_mode: false,
        out: None,
        opt: OptLevel::Minimal,
        debug: false,
        keep_c: false,
        cc: None,
        defines: HashMap::new(),
        target_os_arch: None,
        collections: HashMap::new(),
        no_bounds_check: false,
        sanitize: Vec::new(),
        json_errors: false,
        filter: None,
        program_args: Vec::new(),
        raw_defines: Vec::new(),
        strip_prefixes: Vec::new(),
        include_dirs: Vec::new(),
        pkg_config: Vec::new(),
        doc_args: Vec::new(),
        json: false,
        private: false,
    };
    let Some(first) = argv.first() else { return Ok(parsed) };
    parsed.command = match first.as_str() {
        "build" => Command::Build,
        "run" => Command::Run,
        "check" => Command::Check,
        "test" => Command::Test,
        "doc" => Command::Doc,
        "explain" => Command::Explain,
        "cimport" => Command::Cimport,
        "version" | "-version" | "--version" => Command::Version,
        "help" | "-help" | "--help" | "-h" => Command::Help,
        other => {
            let hint = wid_diagnostics::did_you_mean(other, COMMANDS.iter().copied())
                .map(|c| format!("; did you mean `{c}`?"))
                .unwrap_or_default();
            return Err(format!("unknown command `{other}`{hint}"));
        }
    };
    let mut iter = argv[1..].iter();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            parsed.program_args.extend(iter.by_ref().cloned());
            break;
        }
        if parsed.command == Command::Help {
            parsed.help_topic = Some(arg.clone());
            continue;
        }
        if parsed.command == Command::Doc && (!arg.starts_with('-') || arg == "-") {
            if parsed.doc_args.len() == 2 {
                return Err(format!(
                    "unexpected argument `{arg}`; `wid doc` takes a package and a symbol, like `wid doc core:fmt int`"
                ));
            }
            parsed.doc_args.push(arg.clone());
            continue;
        }
        if !arg.starts_with('-') || arg == "-" {
            if parsed.target.is_some() {
                return Err(format!("unexpected argument `{arg}`; pass program arguments after `--`"));
            }
            parsed.target = Some(PathBuf::from(arg));
            continue;
        }
        let (name, value) = match arg.split_once(':') {
            Some((n, v)) => (n, Some(v)),
            None => (arg.as_str(), None),
        };
        let need = |what: &str| {
            value.map(str::to_string).ok_or_else(|| format!("`{name}` needs a value, like `{name}:{what}`"))
        };
        match name {
            "-file" => parsed.file_mode = true,
            "-out" => parsed.out = Some(PathBuf::from(need("path")?)),
            "-o" => {
                let v = need("speed")?;
                parsed.opt = OptLevel::parse(&v).ok_or_else(|| {
                    format!("unknown optimization level `{v}`; use none, minimal, size, speed or aggressive")
                })?;
            }
            "-debug" => parsed.debug = true,
            "-keep-c" => parsed.keep_c = true,
            "-cc" => parsed.cc = Some(need("clang")?),
            "-define" => {
                let v = need("NAME=value")?;
                parsed.raw_defines.push(v.clone());
                let (k, val) = v.split_once('=').unwrap_or((v.as_str(), "true"));
                parsed.defines.insert(k.to_string(), val.to_string());
            }
            "-target" => parsed.target_os_arch = Some(wid_sema::parse_target(&need("linux_amd64")?)?),
            "-collection" => {
                let v = need("name=path")?;
                let (k, p) =
                    v.split_once('=').ok_or_else(|| "write collections as `-collection:name=path`".to_string())?;
                parsed.collections.insert(k.to_string(), PathBuf::from(p));
            }
            "-no-bounds-check" => parsed.no_bounds_check = true,
            "-sanitize" => parsed.sanitize.push(need("address")?),
            "-json-errors" => parsed.json_errors = true,
            "-filter" => parsed.filter = Some(need("name")?),
            "-dump" | "--dump" if parsed.command == Command::Cimport => {}
            "-json" if parsed.command == Command::Doc => parsed.json = true,
            "-private" if parsed.command == Command::Doc => parsed.private = true,
            "-strip-prefix" => parsed.strip_prefixes.push(need("SDL_")?),
            "-include-dir" => parsed.include_dirs.push(PathBuf::from(need("path")?)),
            "-pkg-config" => parsed.pkg_config.push(need("raylib")?),
            _ => {
                let names: Vec<&str> = FLAGS.iter().map(|f| f.trim_end_matches(':')).collect();
                let hint = if name.starts_with("--") {
                    format!("; Wid flags use one dash, like `{}`", &name[1..])
                } else {
                    wid_diagnostics::did_you_mean(name, names.iter().copied())
                        .map(|f| format!("; did you mean `{f}`?"))
                        .unwrap_or_default()
                };
                return Err(format!("unknown flag `{name}`{hint}"));
            }
        }
    }
    Ok(parsed)
}

/// Returns the help text for a topic, or the general usage.
pub fn usage(topic: Option<&str>) -> String {
    match topic {
        Some("build") => "wid build [dir] [flags]\n\nCompiles the package in `dir` (default `.`) into an executable.\n".to_string() + FLAG_HELP,
        Some("run") => {
            "wid run [dir] [flags] [-- args]\n\nBuilds the package and runs it, passing everything after `--` to the program.\n"
                .to_string()
                + FLAG_HELP
        }
        Some("check") => "wid check [dir] [flags]\n\nType-checks the package without generating code.\n".to_string() + FLAG_HELP,
        Some("test") => "wid test [dir] [flags]\n\nBuilds the package with its `_test.wid` files and runs every `@[test]` method,\neach in its own process. `-filter:<text>` runs only tests whose name contains\nthe text. Exits with 1 when a test fails.\n".to_string() + FLAG_HELP,
        Some("doc") => DOC_HELP.to_string(),
        Some("explain") => "wid explain [CODE]\n\nPrints the long explanation of an error code, or lists all codes.\n".to_string(),
        Some("cimport") => "wid cimport --dump <header> [flags]\n\nPrints the Wid declarations `cimport` makes of a C header. A header that\nisn't a file is looked up on the include path, like `#include <name>`.\n\nFlags:\n  -strip-prefix:<prefix>  Remove a prefix from every name\n  -include-dir:<dir>      Search a directory for headers\n  -pkg-config:<name>      Use pkg-config's flags for a library\n  -define:NAME=value      Define a C macro first\n".to_string(),
        _ => format!(
            "wid {} - the Wid compiler\n\n\
             Usage: wid <command> [target] [flags]\n\n\
             Commands:\n  \
               build    Compile a package into an executable\n  \
               run      Build and run a package\n  \
               check    Type-check a package\n  \
               test     Run a package's tests\n  \
               doc      Show the documentation of a package or symbol\n  \
               explain  Explain an error code\n  \
               cimport  Print the Wid view of a C header\n  \
               version  Print the version\n  \
               help     Show help for a command\n\n{FLAG_HELP}",
            env!("CARGO_PKG_VERSION")
        ),
    }
}

const DOC_HELP: &str = "wid doc [package] [symbol] [flags]

Shows the documentation of a package, or of one of its symbols, from the doc
comments: the `# ` lines directly above a declaration. Without a symbol it
lists every public declaration of the package with the first paragraph of its
doc; with one it shows the whole doc, and for a type its fields, members and
methods, including those from `include`, `extend` and `using`.

The package is a directory (default `.`), a file with `-file`, or a collection
path like `core:fmt` or `vendor:raylib`. The symbol is a path: `Name`,
`Type.member`, or an import name first, `rl.draw_circle_v`. A single argument
is the package if it names a directory or file or contains `:`, and otherwise
a symbol of the package in `.`.

Examples:
  wid doc core:strings
  wid doc core:fmt int
  wid doc rl.draw_circle_v      (in a package that imports raylib as `rl`)
  wid doc . Ball.update -json

Flags:
  -json                  Print the documentation as JSON
  -private               Include private declarations
  -file                  Treat the package as a single file
  -json-errors           Print diagnostics as JSON (on stderr)
  -define:NAME=value     Set a value that `config(:NAME, default)` reads
  -target:<os_arch>      Document another target's code (sets OS and ARCH)
  -collection:name=path  Add an import collection
";

const FLAG_HELP: &str = "Flags:\n  \
    -file                  Treat the target as a single-file package\n  \
    -out:<path>            Output executable path\n  \
    -o:<level>             none, minimal (default), size, speed, aggressive\n  \
    -debug                 Debug info, overflow checks and #line directives\n  \
    -keep-c                Keep the generated C next to the executable\n  \
    -cc:<compiler>         C compiler to use (default: $WID_CC, $CC, cc)\n  \
    -define:NAME=value     Set a value that `config(:NAME, default)` reads\n  \
    -target:<os_arch>      Compile for another target, like linux_amd64 (sets OS and ARCH)\n  \
    -collection:name=path  Add an import collection\n  \
    -no-bounds-check       Disable bounds checks\n  \
    -sanitize:<name>       Enable a sanitizer, e.g. address\n  \
    -json-errors           Print diagnostics (and test results) as JSON\n  \
    -filter:<text>         `wid test`: run only tests whose name contains the text\n";

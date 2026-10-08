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
    Fmt,
    Query,
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
    /// `wid fmt -check`.
    pub check: bool,
    /// `wid query`'s positional arguments: the query and its argument.
    pub query_args: Vec<String>,
    /// `wid query -in:`: the package to query.
    pub query_in: Option<String>,
}

const COMMANDS: &[&str] =
    &["build", "run", "check", "test", "doc", "fmt", "query", "explain", "cimport", "version", "help"];

/// The flags `build` and `run` take, without their `:`; `test` takes
/// `-filter` too.
const BUILD_FLAGS: &[&str] = &[
    "-file",
    "-out",
    "-o",
    "-debug",
    "-keep-c",
    "-cc",
    "-define",
    "-target",
    "-collection",
    "-no-bounds-check",
    "-sanitize",
    "-json-errors",
];

/// The flags `wid check` takes: the ones that change what is checked.
/// Generating and compiling C (`-out`, `-o`, `-debug`, `-keep-c`, `-cc`,
/// `-no-bounds-check`, `-sanitize`) is for `build`, `run` and `test`.
const CHECK_FLAGS: &[&str] = &["-file", "-define", "-target", "-collection", "-json-errors"];

/// The flags `wid doc` takes.
const DOC_FLAGS: &[&str] = &["-json", "-private", "-file", "-json-errors", "-define", "-target", "-collection"];

/// The flags `wid fmt` takes.
const FMT_FLAGS: &[&str] = &["-check", "-file", "-json-errors"];

/// The flags `wid query` takes.
const QUERY_FLAGS: &[&str] = &["-in", "-file", "-define", "-target", "-collection"];

/// The flags `wid cimport` takes.
const CIMPORT_FLAGS: &[&str] = &["-dump", "-strip-prefix", "-include-dir", "-pkg-config", "-define", "-json-errors"];

/// The flags a command takes, for the hints about one it doesn't.
fn command_flags(command: Command) -> Vec<&'static str> {
    match command {
        Command::Build | Command::Run => BUILD_FLAGS.to_vec(),
        Command::Check => CHECK_FLAGS.to_vec(),
        Command::Test => BUILD_FLAGS.iter().copied().chain(["-filter"]).collect(),
        Command::Doc => DOC_FLAGS.to_vec(),
        Command::Fmt => FMT_FLAGS.to_vec(),
        Command::Query => QUERY_FLAGS.to_vec(),
        Command::Cimport => CIMPORT_FLAGS.to_vec(),
        Command::Explain | Command::Version | Command::Help => Vec::new(),
    }
}

/// Every command, in the order the help lists them.
const ALL_COMMANDS: [Command; 10] = [
    Command::Build,
    Command::Run,
    Command::Check,
    Command::Test,
    Command::Doc,
    Command::Query,
    Command::Explain,
    Command::Cimport,
    Command::Version,
    Command::Help,
];

/// The message for `flag`, which another command takes, given to
/// `command`: `wid explain -debug` says which commands take `-debug`.
fn misplaced_flag(command: Command, flag: &str) -> String {
    let takers: Vec<String> = ALL_COMMANDS
        .into_iter()
        .filter(|c| command_flags(*c).contains(&flag))
        .map(|c| format!("`wid {}`", command_name(c)))
        .collect();
    let takes = match takers.as_slice() {
        [one] => format!("only {one} takes it"),
        _ => format!("{} take it", wid_diagnostics::and_list(&takers)),
    };
    format!("`{flag}` doesn't apply to `wid {}`; {takes}", command_name(command))
}

/// The name a command is run with.
pub fn command_name(command: Command) -> &'static str {
    match command {
        Command::Build => "build",
        Command::Run => "run",
        Command::Check => "check",
        Command::Test => "test",
        Command::Doc => "doc",
        Command::Fmt => "fmt",
        Command::Query => "query",
        Command::Explain => "explain",
        Command::Cimport => "cimport",
        Command::Version => "version",
        Command::Help => "help",
    }
}

/// Flags that only one command takes.
const COMMAND_FLAGS: &[(&str, &str)] = &[
    ("-json", "doc"),
    ("-private", "doc"),
    ("-in", "query"),
    ("-dump", "cimport"),
    ("-filter", "test"),
    ("-check", "fmt"),
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
        check: false,
        query_args: Vec::new(),
        query_in: None,
    };
    let Some(first) = argv.first() else { return Ok(parsed) };
    parsed.command = match first.as_str() {
        "build" => Command::Build,
        "run" => Command::Run,
        "check" => Command::Check,
        "test" => Command::Test,
        "doc" => Command::Doc,
        "fmt" => Command::Fmt,
        "query" => Command::Query,
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
        if parsed.command == Command::Query && (!arg.starts_with('-') || arg == "-") {
            parsed.query_args.push(arg.clone());
            continue;
        }
        if !arg.starts_with('-') || arg == "-" {
            if parsed.target.is_some() && parsed.command == Command::Fmt {
                return Err(format!(
                    "unexpected argument `{arg}`; `wid fmt` formats one package directory or one file at a time"
                ));
            }
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
        // A flag of another command, like `wid explain -debug`, would be
        // ignored.
        if !command_flags(parsed.command).contains(&name)
            && ALL_COMMANDS.into_iter().any(|c| command_flags(c).contains(&name))
        {
            return Err(misplaced_flag(parsed.command, name));
        }
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
            "-check" if parsed.command == Command::Fmt => parsed.check = true,
            "-in" if parsed.command == Command::Query => parsed.query_in = Some(need("path/to/package")?),
            "-strip-prefix" => parsed.strip_prefixes.push(need("SDL_")?),
            "-include-dir" => parsed.include_dirs.push(PathBuf::from(need("path")?)),
            "-pkg-config" => parsed.pkg_config.push(need("raylib")?),
            _ => return Err(unknown_flag(parsed.command, name)),
        }
    }
    Ok(parsed)
}

/// The message for a flag `command` doesn't take. A double-dash flag
/// (`--debug`) is offered its one-dash form only when the command takes it.
fn unknown_flag(command: Command, name: &str) -> String {
    let accepted = command_flags(command);
    let one_dash = name.strip_prefix('-').filter(|rest| rest.starts_with('-') && !rest.starts_with("--"));
    if let Some(flag) = one_dash
        && accepted.contains(&flag)
    {
        return format!("unknown flag `{name}`; Wid flags use one dash, like `{flag}`");
    }
    let flag = one_dash.unwrap_or(name);
    if command == Command::Query && flag == "-json" {
        return format!("`wid query` always prints JSON; drop `{name}`");
    }
    if let Some((_, owner)) = COMMAND_FLAGS.iter().find(|(f, _)| *f == flag) {
        return if flag == name {
            format!("`{name}` only applies to `wid {owner}`")
        } else {
            format!("unknown flag `{name}`; `{flag}` only applies to `wid {owner}`")
        };
    }
    let hint = match wid_diagnostics::did_you_mean(flag, accepted.iter().copied()) {
        Some(similar) => format!("; did you mean `{similar}`?"),
        None if command == Command::Explain => {
            "; `wid explain` takes no flags: to list every error code, run `wid explain` with no code".to_string()
        }
        None if accepted.is_empty() => format!("; `wid {}` takes no flags", command_name(command)),
        None => String::new(),
    };
    format!("unknown flag `{name}`{hint}")
}

/// Returns the help text for a topic, or the general usage.
pub fn usage(topic: Option<&str>) -> String {
    match topic {
        Some("build") => "wid build [dir] [flags]\n\nCompiles the package in `dir` (default `.`) into an executable.\n".to_string() + BUILD_FLAG_HELP,
        Some("run") => {
            "wid run [dir] [flags] [-- args]\n\nBuilds the package and runs it, passing everything after `--` to the program.\n"
                .to_string()
                + BUILD_FLAG_HELP
        }
        Some("check") => "wid check [dir] [flags]\n\nType-checks the package without generating code, so it takes none of the flags\nfor building C (`-out:`, `-o:`, `-debug`, `-keep-c`, `-cc:`, `-no-bounds-check`,\n`-sanitize:`).\n".to_string() + CHECK_FLAG_HELP,
        Some("test") => "wid test [dir] [flags]\n\nBuilds the package with its `_test.wid` files and runs every `@[test]` method,\neach in its own process. `-filter:<text>` runs only tests whose name contains\nthe text. Exits with 1 when a test fails.\n".to_string() + BUILD_FLAG_HELP + "  -filter:<text>         Run only tests whose name contains the text\n",
        Some("doc") => DOC_HELP.to_string(),
        Some("fmt") => FMT_HELP.to_string(),
        Some("query") => QUERY_HELP.to_string(),
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
               fmt      Format a package's files in the canonical style\n  \
               query    Answer questions about a package, as JSON\n  \
               explain  Explain an error code\n  \
               cimport  Print the Wid view of a C header\n  \
               version  Print the version\n  \
               help     Show help for a command\n\n{BUILD_FLAG_HELP}  \
             -filter:<text>         `wid test`: run only tests whose name contains the text\n\n\
             `wid check` takes only `-file`, `-define:`, `-target:`, `-collection:` and\n\
             `-json-errors`; `wid help <command>` lists a command's flags.\n",
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

const FMT_HELP: &str = "wid fmt [dir|file] [flags]

Rewrites the `.wid` files of the package in `dir` (default `.`), its
`_test.wid` files included, or one `.wid` file, in Wid's canonical style, and
lists the files it changed. The style has no options; SPEC.md (\"Toolchain and
CLI\") describes it. A file that doesn't parse is reported and left as it is,
and the exit status is 1 then.

Formatting reads only syntax: it never loads imports, type-checks or generates
code.

Examples:
  wid fmt
  wid fmt game/player.wid
  wid fmt . -check

Flags:
  -check                 Change nothing: list the files that would change, and
                         exit with 1 when there are any
  -file                  Treat the target as a single file, whatever its name
  -json-errors           Print the changed files and the diagnostics as one
                         JSON document
";

const QUERY_HELP: &str = "wid query <query> [argument] [flags]

Answers a question about a package with one JSON document on stdout, for
editors, scripts and LLMs. Diagnostics go to stderr, as JSON too. A package
with errors is still answered from what the checker collected; the exit
status is 1 then, and when the request fails.

Queries:
  outline          Every declaration of the package, private ones too, with
                   fields, enum members and methods under their type
  def <symbol>     The declaration a symbol path names: `Name`,
                   `Type.member`, or an import name first, `rl.draw_circle_v`;
                   an overload set gives the set and each of its members
  methods <Type>   Every method callable on a type, grouped by where it comes
                   from: the type itself, `include`, `extend` and `using`
  refs <symbol>    Every use of a symbol: where, how (`read`, `write`, `call`,
                   `type`, `import`, `declaration`) and in which declaration
  calls <symbol>   The uses of a symbol that call it
  type <file:line:column>
                   What is at a position, and its type: the innermost
                   expression, name, binding or written type there

The package is the one in `.`, or the one `-in:` names: a directory, a `.wid`
file with `-file`, or a collection path like `core:fmt`; for `type`, the
package that holds the position's file. The JSON shapes are described in
SPEC.md (\"Toolchain and CLI\").

Examples:
  wid query outline
  wid query def Ball.update -in:game
  wid query methods String -in:core:strings
  wid query refs Ball.pos -in:game
  wid query type game/main.wid:12:5

Flags:
  -in:<package>          The package to query (default `.`)
  -file                  Treat the package as a single file
  -define:NAME=value     Set a value that `config(:NAME, default)` reads
  -target:<os_arch>      Query another target's code (sets OS and ARCH)
  -collection:name=path  Add an import collection
";

/// The flags `build`, `run` and `test` take (`test` takes `-filter:` too).
const BUILD_FLAG_HELP: &str = "Flags:\n  \
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
    -json-errors           Print diagnostics (and test results) as JSON\n";

/// The flags `wid check` takes.
const CHECK_FLAG_HELP: &str = "Flags:\n  \
    -file                  Treat the target as a single-file package\n  \
    -define:NAME=value     Set a value that `config(:NAME, default)` reads\n  \
    -target:<os_arch>      Check another target's code, like linux_amd64 (sets OS and ARCH)\n  \
    -collection:name=path  Add an import collection\n  \
    -json-errors           Print diagnostics as JSON\n";

#[cfg(test)]
mod tests {
    use super::{Command, command_flags, command_name, parse, usage};

    /// The error `wid ARGS` stops with.
    fn error(args: &str) -> String {
        let argv: Vec<String> = args.split_whitespace().map(str::to_string).collect();
        match parse(&argv) {
            Ok(parsed) => panic!("`wid {args}` parsed: {parsed:?}"),
            Err(message) => message,
        }
    }

    #[test]
    fn double_dash_offers_the_one_dash_flag_the_command_takes() {
        assert_eq!(error("build --debug"), "unknown flag `--debug`; Wid flags use one dash, like `-debug`");
        assert_eq!(error("doc --private"), "unknown flag `--private`; Wid flags use one dash, like `-private`");
        assert_eq!(error("test --filter:x"), "unknown flag `--filter`; Wid flags use one dash, like `-filter`");
    }

    #[test]
    fn double_dash_never_offers_a_flag_the_command_lacks() {
        let list = "`wid explain` takes no flags: to list every error code, run `wid explain` with no code";
        assert_eq!(error("explain --list"), format!("unknown flag `--list`; {list}"));
        assert_eq!(error("explain -list"), format!("unknown flag `-list`; {list}"));
        assert_eq!(error("build --private"), "unknown flag `--private`; `-private` only applies to `wid doc`");
        assert_eq!(error("query --json"), "`wid query` always prints JSON; drop `--json`");
        assert_eq!(error("check --filter:x"), "unknown flag `--filter`; `-filter` only applies to `wid test`");
        assert_eq!(error("build --verbose"), "unknown flag `--verbose`");
    }

    #[test]
    fn flags_of_other_commands_are_errors() {
        let build = "`wid build`, `wid run` and `wid test` take it";
        assert_eq!(error("explain -debug E0206"), format!("`-debug` doesn't apply to `wid explain`; {build}"));
        assert_eq!(error("check -filter:x ."), "`-filter` doesn't apply to `wid check`; only `wid test` takes it");
        assert_eq!(error("doc -out:x core:fmt"), format!("`-out` doesn't apply to `wid doc`; {build}"));
        assert_eq!(error("build -private"), "`-private` doesn't apply to `wid build`; only `wid doc` takes it");
        assert_eq!(error("query -dump"), "`-dump` doesn't apply to `wid query`; only `wid cimport` takes it");
        // `wid check` generates no C, so the flags for building it are errors.
        for flag in ["-out:x", "-o:speed", "-debug", "-keep-c", "-cc:clang", "-no-bounds-check", "-sanitize:address"] {
            let name = flag.split(':').next().unwrap_or(flag);
            assert_eq!(
                error(&format!("check main.wid -file {flag}")),
                format!("`{name}` doesn't apply to `wid check`; {build}")
            );
        }
        assert_eq!(
            error("version -json-errors"),
            "`-json-errors` doesn't apply to `wid version`; `wid build`, `wid run`, `wid check`, `wid test`, \
             `wid doc` and `wid cimport` take it"
        );
        assert_eq!(
            error("cimport -file x.h"),
            "`-file` doesn't apply to `wid cimport`; `wid build`, `wid run`, `wid check`, `wid test`, `wid doc` \
             and `wid query` take it"
        );
    }

    /// The flags a command's help lists, from the `Flags:` section of
    /// [`usage`].
    fn promised(command: &str) -> Vec<String> {
        let text = usage(Some(command));
        let flags = text.split_once("Flags:").map_or("", |(_, f)| f);
        flags
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .filter(|word| word.starts_with('-'))
            .map(|word| word.split(':').next().unwrap_or(word).to_string())
            .collect()
    }

    /// A value for a flag that needs one.
    fn sample(flag: &str) -> Option<&'static str> {
        Some(match flag {
            "-out" | "-include-dir" | "-in" => "x",
            "-o" => "speed",
            "-cc" => "clang",
            "-define" => "A=1",
            "-target" => "linux_amd64",
            "-collection" => "a=b",
            "-sanitize" => "address",
            "-filter" => "x",
            "-strip-prefix" => "SDL_",
            "-pkg-config" => "raylib",
            _ => return None,
        })
    }

    #[test]
    fn every_flag_the_help_promises_is_taken() {
        let mut checked = 0;
        for command in ["build", "run", "check", "test", "doc", "query", "cimport"] {
            let flags = promised(command);
            assert!(!flags.is_empty(), "`wid help {command}` lists no flags");
            for flag in flags {
                let arg = match sample(&flag) {
                    Some(v) => format!("{flag}:{v}"),
                    None => flag.clone(),
                };
                let argv = vec![command.to_string(), arg.clone()];
                if let Err(message) = parse(&argv) {
                    panic!("`wid {command} {arg}` failed: {message}");
                }
                checked += 1;
            }
        }
        assert!(checked > 30, "only {checked} flags checked");
        // The help lists every flag a command takes, and no other.
        for command in [Command::Build, Command::Run, Command::Check, Command::Test, Command::Doc, Command::Query] {
            let name = command_name(command);
            let mut listed = promised(name);
            let mut taken: Vec<String> = command_flags(command).into_iter().map(str::to_string).collect();
            listed.sort();
            taken.sort();
            assert_eq!(listed, taken, "`wid help {name}` and the flags `wid {name}` takes");
        }
        // The commands whose help lists no flags take none.
        for command in [Command::Explain, Command::Version, Command::Help] {
            assert!(command_flags(command).is_empty(), "`wid {}` takes flags", command_name(command));
        }
    }

    #[test]
    fn similar_flags_come_from_the_command() {
        assert_eq!(error("build -debgu"), "unknown flag `-debgu`; did you mean `-debug`?");
        assert_eq!(error("query -fiel"), "unknown flag `-fiel`; did you mean `-file`?");
        assert_eq!(error("version -x"), "unknown flag `-x`; `wid version` takes no flags");
    }
}

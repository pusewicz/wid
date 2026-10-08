//! What a `-debug` build gives a debugger (#141): a `#line` directive for
//! every statement and for an endless `def`'s expression, one back into the
//! C file after each function, so generated code and the C `main` don't
//! inherit the last `.wid` line, and `-o:none` unless `-o:` says otherwise.

use std::path::{Path, PathBuf};

use wid_driver::{OptLevel, Options};

const PROGRAM: &str = "\
struct V
  x: Int
  def self.zero -> V = V.new(0)
end

def add(a: Int, b: Int) -> Int = a + b

def main
  p add(1, 2)
  p V.zero.x
end
";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..").canonicalize().expect("repo root exists")
}

/// A scratch directory holding `line.wid`.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wid-debug-builds-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create a scratch directory");
    std::fs::write(dir.join("line.wid"), PROGRAM).expect("write line.wid");
    dir
}

fn options(dir: &Path) -> Options {
    let mut opts = Options::new(dir.join("line.wid"));
    opts.file_mode = true;
    opts.wid_root = Some(repo_root());
    opts.debug = true;
    opts
}

/// The lines of the C body of the function whose definition starts with
/// `head`, up to its closing `}`, and the line after that.
fn function<'a>(lines: &[&'a str], head: &str) -> (Vec<&'a str>, &'a str) {
    let start = lines.iter().position(|l| l.starts_with(head) && l.ends_with('{')).expect("the function is emitted");
    let end = start + lines[start..].iter().position(|l| *l == "}").expect("the function ends");
    (lines[start + 1..end].to_vec(), lines.get(end + 1).copied().unwrap_or_default())
}

#[test]
fn line_directives() {
    let dir = scratch("lines");
    let opts = options(&dir);
    let checked = wid_driver::check(&opts);
    let program = checked.program.as_ref().expect("the program checks");
    let c = wid_driver::generate_c(program, &checked.sources, &opts, Path::new("line.c"));
    let lines: Vec<&str> = c.lines().collect();
    let wid = format!("{:?}", dir.join("line.wid").display().to_string());

    // An endless `def`'s expression has its line, like a statement.
    let (add, after_add) = function(&lines, "static wid_Int line__add(");
    assert_eq!(add[0], format!("#line 6 {wid}"), "the body of `add`:\n{}", add.join("\n"));
    let (zero, after_zero) = function(&lines, "static line__V line__V__zero(");
    assert_eq!(zero[0], format!("#line 3 {wid}"), "the body of `V.zero`:\n{}", zero.join("\n"));
    let (main, after_main) = function(&lines, "static void line__main(");
    assert_eq!(main[0], format!("#line 9 {wid}"));

    // After each function, the C file's own lines, numbered as they are.
    for after in [after_add, after_zero, after_main] {
        assert!(after.starts_with("#line ") && after.ends_with(" \"line.c\""), "after a function: {after:?}");
    }
    let mut resets = 0;
    for (i, line) in lines.iter().enumerate() {
        if let Some(rest) = line.strip_prefix("#line ")
            && let Some(number) = rest.strip_suffix(" \"line.c\"")
        {
            assert_eq!(number.parse::<usize>().ok(), Some(i + 2), "`{line}` is on line {}", i + 1);
            resets += 1;
        }
    }
    assert_eq!(resets, 3, "one after each function:\n{c}");

    // The C `main` comes after the last reset, so it is the C file's.
    let main_at = lines.iter().position(|l| l.starts_with("int main(")).expect("a C main");
    let last_line = lines[..main_at].iter().rev().find(|l| l.starts_with("#line ")).expect("a #line before main");
    assert!(last_line.ends_with(" \"line.c\""), "the last #line before `main` is {last_line:?}");

    // A release build has none.
    let mut release = opts.clone();
    release.debug = false;
    let checked = wid_driver::check(&release);
    let program = checked.program.as_ref().expect("the program checks");
    let c = wid_driver::generate_c(program, &checked.sources, &release, Path::new("line.c"));
    assert!(!c.contains("#line"), "a release build has #line directives");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn debug_builds_at_o_none_unless_o_is_given() {
    let dir = scratch("levels");
    let mut opts = options(&dir);
    assert_eq!(opts.opt_level(), OptLevel::None);
    opts.opt = Some(OptLevel::Speed);
    assert_eq!(opts.opt_level(), OptLevel::Speed);
    opts.debug = false;
    assert_eq!(opts.opt_level(), OptLevel::Speed);
    opts.opt = None;
    assert_eq!(opts.opt_level(), OptLevel::Minimal);
    let _ = std::fs::remove_dir_all(&dir);
}

/// The flags a build gives the C compiler, recorded by a fake one that
/// succeeds without writing anything, so the build cleans up after itself.
#[cfg(unix)]
#[test]
fn the_c_compiler_gets_the_level() {
    use std::os::unix::fs::PermissionsExt;
    let dir = scratch("flags");
    let log = dir.join("args.log");
    let cc = dir.join("fake-cc");
    let script = format!("#!/bin/sh\necho \"$@\" >> '{}'\nexit 0\n", log.display());
    std::fs::write(&cc, script).expect("write the fake compiler");
    std::fs::set_permissions(&cc, std::fs::Permissions::from_mode(0o755)).expect("make it executable");
    let level = |debug: bool, opt: Option<OptLevel>| {
        let _ = std::fs::remove_file(&log);
        let mut opts = options(&dir);
        opts.debug = debug;
        opts.opt = opt;
        opts.cc = Some(cc.display().to_string());
        opts.out = Some(dir.join("line"));
        let built = wid_driver::build(&opts);
        assert!(!built.checked.diags.has_errors(), "the fake compiler succeeds");
        let args = std::fs::read_to_string(&log).expect("the compiler ran");
        let words: Vec<String> = args.split_whitespace().map(str::to_string).collect();
        words.into_iter().find(|w| w.starts_with("-O")).expect("an -O flag")
    };
    assert_eq!(level(true, None), "-O0");
    assert_eq!(level(true, Some(OptLevel::Speed)), "-O2");
    assert_eq!(level(false, None), "-O1");
    assert_eq!(level(false, Some(OptLevel::Aggressive)), "-O3");
    let _ = std::fs::remove_dir_all(&dir);
}

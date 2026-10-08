//! `wid fmt` on the file system: it rewrites the files of a package that
//! aren't canonical, `_test.wid` ones included, leaves a file that doesn't
//! parse byte for byte as it was, and with `-check` writes nothing.

use std::path::PathBuf;

use wid_driver::Options;

/// A fresh directory for one test.
fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("wid-fmt-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create a scratch directory");
    dir
}

const MESSY: &str = "def main\n    p   1\nend\n";
const CANONICAL: &str = "def main\n  p 1\nend\n";
const BROKEN: &str = "def broken(\n    p   1\n";

#[test]
fn rewrites_the_package_and_leaves_broken_files_alone() {
    let dir = scratch("rewrite");
    std::fs::write(dir.join("main.wid"), MESSY).expect("write main.wid");
    std::fs::write(dir.join("main_test.wid"), MESSY.replace("main", "helper")).expect("write main_test.wid");
    std::fs::write(dir.join("broken.wid"), BROKEN).expect("write broken.wid");
    std::fs::write(dir.join("notes.txt"), MESSY).expect("write notes.txt");
    let out = wid_driver::fmt::fmt(&Options::new(&dir), false);
    assert!(out.diags.has_errors(), "the broken file is reported");
    let changed: Vec<&str> = out.changed().map(|f| f.path.file_name().and_then(|n| n.to_str()).unwrap_or("")).collect();
    assert_eq!(changed, ["main.wid", "main_test.wid"]);
    let read = |name: &str| std::fs::read_to_string(dir.join(name)).expect("read back");
    assert_eq!(read("main.wid"), CANONICAL);
    assert_eq!(read("main_test.wid"), CANONICAL.replace("main", "helper"));
    assert_eq!(read("broken.wid"), BROKEN);
    assert_eq!(read("notes.txt"), MESSY);
    let printed = wid_driver::fmt::print(&out, false, false);
    assert!(!printed.success);
    assert!(printed.stderr.contains("could not format due to"), "{}", printed.stderr);
    // Formatting again changes nothing.
    std::fs::remove_file(dir.join("broken.wid")).expect("remove broken.wid");
    let again = wid_driver::fmt::fmt(&Options::new(&dir), false);
    assert_eq!(again.changed().count(), 0);
    assert!(wid_driver::fmt::print(&again, false, false).success);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn check_writes_nothing() {
    let dir = scratch("check");
    let file = dir.join("main.wid");
    std::fs::write(&file, MESSY).expect("write main.wid");
    let out = wid_driver::fmt::fmt(&Options::new(&file), true);
    let printed = wid_driver::fmt::print(&out, false, false);
    assert!(!printed.success, "a file that would change fails `-check`");
    assert!(printed.stdout.ends_with("main.wid\n"), "{}", printed.stdout);
    assert_eq!(std::fs::read_to_string(&file).expect("read back"), MESSY);
    let _ = std::fs::remove_dir_all(&dir);
}

//! The errors of `wid cimport --dump`: the header comes from the command
//! line, so they name its flags (`-include-dir:`, `-pkg-config:`,
//! `-define:`) and the current directory, where a `cimport` line's errors
//! name its options and the package directory (`tests/ui/cimport_errors`).
//! A missing libclang skips the test.

use std::path::PathBuf;

use wid_diagnostics::{RenderOptions, render_all_with};
use wid_driver::DumpRequest;

/// Dumps a header and returns the rendered errors, as `wid cimport` prints
/// them; the dump must fail.
fn dump_errors(request: &DumpRequest) -> String {
    let (sources, result) = wid_driver::cimport_dump(request);
    let Err(diags) = result else { panic!("`{}` imported", request.header) };
    render_all_with(&diags, &sources, RenderOptions { color: false }, "could not import the header due to")
}

/// Runs every case in turn: libclang is loaded once, before any other
/// thread could read the environment while it is.
#[test]
fn dump_errors_name_flags() {
    if wid_cimport::libclang().is_err() {
        eprintln!("skipping: libclang not found");
        return;
    }
    missing_header();
    missing_header_with_include_dirs();
    header_with_errors();
}

fn missing_header() {
    let request = DumpRequest { header: "missing.h".into(), ..DumpRequest::default() };
    assert_eq!(
        dump_errors(&request),
        "\
error[E0701]: header `missing.h` not found
 --> command line:1:20
  |
1 | wid cimport --dump missing.h
  |                    ^^^^^^^^^ not in the current directory or on the include path
help: write the path relative to the current directory, add its directory with `-include-dir:`, or name the library with `-pkg-config:`
  = see `wid explain E0701`

error: could not import the header due to 1 error
"
    );
}

fn missing_header_with_include_dirs() {
    let request = DumpRequest {
        header: "missing.h".into(),
        include_dirs: vec![PathBuf::from("nowhere")],
        ..DumpRequest::default()
    };
    let searched = std::path::absolute("nowhere").expect("an absolute path");
    assert_eq!(
        dump_errors(&request),
        format!(
            "\
error[E0701]: header `missing.h` not found
 --> command line:1:20
  |
1 | wid cimport --dump missing.h
  |                    ^^^^^^^^^ not in the current directory or on the include path
  = note: -include-dir: {}
help: write the path relative to the current directory, add its directory with `-include-dir:`, or name the library with `-pkg-config:`
  = see `wid explain E0701`

error: could not import the header due to 1 error
",
            searched.display()
        )
    );
}

fn header_with_errors() {
    let dir = std::env::temp_dir().join(format!("wid-cimport-dump-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("create a scratch directory");
    let header = dir.join("broken.h");
    std::fs::write(&header, "#ifndef NEEDS_CONFIG\n#error \"define NEEDS_CONFIG\"\n#endif\n")
        .expect("write the header");
    let request = DumpRequest { header: header.display().to_string(), ..DumpRequest::default() };
    let errors = dump_errors(&request);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(errors.contains("has C errors"), "{errors}");
    assert!(errors.contains("help: fix the header, or set the macros it expects with `-define:`\n"), "{errors}");
}

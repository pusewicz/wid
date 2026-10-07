//! Request options, failures and the properties the driver relies on:
//! purity and thread safety.

mod common;

use std::path::PathBuf;

use common::*;
use wid_cimport::*;

#[test]
fn libclang_is_reported() {
    let info = libclang().expect("libclang is available to the test suite");
    assert!(info.path.is_file(), "{}", info.path.display());
    assert!(info.version.contains("clang"), "{}", info.version);
    let resource_dir = info.resource_dir.expect("a resource directory beside libclang");
    assert!(resource_dir.join("include/stdarg.h").is_file());
}

#[test]
fn missing_header_path() {
    let request = ImportRequest::new(Header::Path(fixture("no_such_header.h")));
    let Err(ImportError::HeaderNotFound { header, .. }) = import(&request) else { panic!("expected HeaderNotFound") };
    assert!(header.ends_with("no_such_header.h"));
}

#[test]
fn missing_header_include() {
    let mut request = ImportRequest::new(Header::Include("no/such_header.h".into()));
    request.include_dirs.push(fixture(""));
    let error = import(&request).expect_err("the header does not exist");
    assert_eq!(
        error,
        ImportError::HeaderNotFound { header: "<no/such_header.h>".into(), include_dirs: vec![fixture("")] }
    );
    assert!(error.to_string().contains("<no/such_header.h>"));
}

#[test]
fn include_form_finds_headers_on_the_include_path() {
    let mut request = ImportRequest::new(Header::Include("kitchen/kitchen.h".into()));
    request.include_dirs.push(fixture(""));
    let by_include = import(&request).expect("found on the include path");
    let by_path = import_fixture("kitchen/kitchen.h");
    assert_eq!(by_include.root, by_path.root);
    let names = |module: &CModule| module.items.iter().map(|item| item.name().map(String::from)).collect::<Vec<_>>();
    assert_eq!(names(&by_include), names(&by_path));
}

#[test]
fn every_error_is_reported_with_its_position() {
    let request = ImportRequest::new(Header::Path(fixture("errors/broken.h")));
    let Err(ImportError::Parse { errors }) = import(&request) else { panic!("expected parse errors") };
    assert!(errors.len() >= 2, "{errors:?}");
    let lines: Vec<u32> = errors.iter().map(|error| error.line).collect();
    assert!(lines.contains(&2) && lines.contains(&3), "{errors:?}");
    for error in &errors {
        assert!(error.file.as_ref().is_some_and(|file| file.ends_with("errors/broken.h")));
        assert!(error.column > 0 && !error.message.is_empty());
    }
    let text = ImportError::Parse { errors }.to_string();
    assert!(text.contains("broken.h:2:"), "{text}");
}

#[test]
fn a_missing_nested_include_is_a_parse_error() {
    let request = ImportRequest::new(Header::Path(fixture("errors/missing_include.h")));
    let Err(ImportError::Parse { errors }) = import(&request) else { panic!("expected parse errors") };
    assert!(errors.iter().any(|error| error.message.contains("does_not_exist.h") && error.line == 1), "{errors:?}");
}

#[test]
fn defines_reach_the_preprocessor() {
    let without = import_fixture("defines.h");
    assert!(without.find("feature_on").is_none() && without.find("level_high").is_none());
    let mut request = ImportRequest::new(Header::Path(fixture("defines.h")));
    request.defines = vec!["WID_FEATURE".into(), "WID_LEVEL=3".into()];
    let with = import(&request).expect("imports");
    assert!(with.find("feature_on").is_some() && with.find("level_high").is_some() && with.find("always").is_some());
}

#[test]
fn clang_args_come_last() {
    let mut request = ImportRequest::new(Header::Path(fixture("defines.h")));
    request.clang_args = vec!["-DWID_LEVEL=9".into()];
    let module = import(&request).expect("imports");
    assert!(module.find("level_high").is_some());
}

#[test]
fn the_target_decides_type_sizes() {
    let import_for = |triple: &str| {
        let mut request = ImportRequest::new(Header::Path(fixture("target.h")));
        request.target = Some(triple.to_string());
        import(&request).unwrap_or_else(|error| panic!("{triple}: {error}"))
    };
    let windows = import_for("x86_64-pc-windows-msvc");
    assert!(windows.target.triple.starts_with("x86_64-pc-windows-msvc"));
    assert_eq!(function(&windows, "long_value").sig.ret, int(32, true, "long"));
    assert_eq!(function(&windows, "plain_char").sig.ret, CType::Char { signed: true });

    let linux = import_for("aarch64-unknown-linux-gnu");
    assert_eq!(function(&linux, "long_value").sig.ret, int(64, true, "long"));
    assert_eq!(function(&linux, "plain_char").sig.ret, CType::Char { signed: false });

    let wasm = import_for("wasm32-unknown-unknown");
    assert_eq!(wasm.target.pointer_bits, 32);
}

#[test]
fn imports_are_deterministic() {
    assert_eq!(import_fixture("kitchen/kitchen.h"), import_fixture("kitchen/kitchen.h"));
}

#[test]
fn imports_run_on_any_thread() {
    let handles: Vec<_> = (0..4).map(|_| std::thread::spawn(|| import_fixture("kitchen/kitchen.h"))).collect();
    let modules: Vec<CModule> = handles.into_iter().map(|handle| handle.join().expect("thread finished")).collect();
    assert!(modules.windows(2).all(|pair| pair[0] == pair[1]));
}

#[test]
fn errors_explain_themselves() {
    let not_found = ImportError::LibclangNotFound { searched: vec![PathBuf::from("/opt/llvm/lib")] };
    let text = not_found.to_string();
    assert!(text.contains("LIBCLANG_PATH") && text.contains("/opt/llvm/lib"), "{text}");
    let too_old = ImportError::LibclangTooOld {
        path: PathBuf::from("/usr/lib/libclang.so"),
        version: "clang version 9.0.0".into(),
        missing: vec!["clang_Type_getValueType".into()],
    };
    assert!(too_old.to_string().contains("clang_Type_getValueType"));
}

#[test]
fn a_header_in_a_shared_directory_imports_only_itself() {
    let mut request = ImportRequest::new(Header::Include("kitchen.h".into()));
    request.clang_args = vec!["-isystem".into(), fixture("kitchen").display().to_string()];
    let module = import(&request).expect("imports");
    assert!(module.find("add").is_some());
    assert!(module.find("part_count").is_none(), "detail/part.h is a neighbour in a shared directory");
}

#[test]
fn macro_probes_survive_pedantic_errors() {
    let mut request = ImportRequest::new(Header::Path(fixture("kitchen/kitchen.h")));
    request.clang_args = vec!["-pedantic-errors".into()];
    let module = import(&request).expect("imports");
    assert_eq!(macro_expr(&module, "KITCHEN_INT").1, Some(&MacroValue::Int(42)));
    assert_eq!(macro_expr(&module, "KITCHEN_VEC").0, &named(NamedKind::Typedef, "Vec2"));
}

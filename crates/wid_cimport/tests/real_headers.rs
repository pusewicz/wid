//! Imports real libraries when they are installed, and skips otherwise.
//!
//! `stb_image.h` is the copy vendored in `vendor/stb/image`. Set
//! `WID_RAYLIB_INCLUDE` or `WID_SDL3_INCLUDE`
//! to the include directory holding `raylib.h` or `SDL3/SDL.h` to test copies
//! Homebrew and the usual prefixes don't have.
//! Run with `--nocapture` to see what each import produced.

mod common;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use wid_cimport::*;

/// Finds `relative` under the directory in `env`, Homebrew's `formula`
/// prefix, or the usual include directories.
fn locate(env: Option<&str>, formula: &str, relative: &str) -> Option<PathBuf> {
    let mut roots: Vec<PathBuf> = env.and_then(std::env::var_os).map(PathBuf::from).into_iter().collect();
    if let Ok(output) = Command::new("brew").args(["--prefix", formula]).output()
        && output.status.success()
    {
        roots.push(PathBuf::from(String::from_utf8_lossy(&output.stdout).trim()).join("include"));
    }
    roots.extend(["/opt/homebrew/include", "/usr/local/include", "/usr/include"].map(PathBuf::from));
    roots.into_iter().map(|root| root.join(relative)).find(|path| path.is_file())
}

/// Imports `header` with `include_dir` on the include path and prints a
/// summary of the items by kind.
fn import_real(name: &str, header: Header, include_dir: Option<&Path>) -> CModule {
    let mut request = ImportRequest::new(header);
    request.include_dirs.extend(include_dir.map(Path::to_path_buf));
    let started = std::time::Instant::now();
    let module = import(&request).unwrap_or_else(|error| panic!("{name}: {error}"));
    let mut counts: BTreeMap<String, usize> = BTreeMap::new();
    for item in &module.items {
        let kind = match &item.kind {
            ItemKind::Function(_) => "function",
            ItemKind::Record(record) if record.is_opaque() => "opaque record",
            ItemKind::Record(_) => "record",
            ItemKind::Enum(_) => "enum",
            ItemKind::Typedef(_) => "typedef",
            ItemKind::Global(_) => "global",
            ItemKind::Macro(definition) => match &definition.kind {
                MacroKind::Expr { value: Some(_), .. } => "constant macro",
                MacroKind::Expr { value: None, .. } => "typed macro",
                MacroKind::Other => "other macro",
                MacroKind::FunctionLike { .. } => "function-like macro",
            },
        };
        *counts.entry(kind.to_string()).or_default() += 1;
    }
    eprintln!("{name}: {} items in {:.0?}: {counts:?}", module.items.len(), started.elapsed());
    assert_consistent(&module);
    module
}

#[test]
fn stb_image() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../vendor/stb/image/stb_image.h");
    assert!(path.is_file(), "the vendored stb_image.h is missing: {}", path.display());
    let module = import_real("stb_image", Header::Path(path), None);
    assert_eq!(typedef(&module, "stbi_uc").ty, int(8, false, "unsigned char"));
    assert_eq!(macro_expr(&module, "STBI_VERSION").1, Some(&MacroValue::Int(1)));

    let load = function(&module, "stbi_load");
    let names: Vec<_> = load.sig.params.iter().map(|param| param.name.as_deref().unwrap_or("")).collect();
    assert_eq!(names, ["filename", "x", "y", "channels_in_file", "desired_channels"]);
    assert_eq!(load.sig.ret, pointer(named(NamedKind::Typedef, "stbi_uc"), NONE));

    let callbacks = record(&module, "stbi_io_callbacks");
    assert!(["read", "skip", "eof"].iter().all(|name| matches!(field(callbacks, name).ty, CType::FnPtr(_))));
    let rgba = module
        .items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Enum(e) => e.constants.iter().find(|c| c.name == "STBI_rgb_alpha"),
            _ => None,
        })
        .next()
        .expect("STBI_rgb_alpha");
    assert_eq!(rgba.value, 4);
    assert!(module.items.iter().filter(|item| matches!(item.kind, ItemKind::Function(_))).count() >= 40);
}

#[test]
fn raylib() {
    let Some(path) = locate(Some("WID_RAYLIB_INCLUDE"), "raylib", "raylib.h") else {
        eprintln!("skipping: raylib.h not found (set WID_RAYLIB_INCLUDE)");
        return;
    };
    let module = import_real("raylib", Header::Path(path), None);
    let init = function(&module, "InitWindow");
    assert_eq!(init.sig.params.len(), 3);
    assert_eq!(init.sig.params[2].name.as_deref(), Some("title"));
    assert!(item(&module, "InitWindow").doc.as_deref().is_some_and(|doc| doc.contains("Initialize window")));

    let color = record(&module, "Color");
    assert_eq!(color.body.as_ref().map(|body| body.size), Some(4));
    assert_eq!(macro_expr(&module, "RED"), (&named(NamedKind::Typedef, "Color"), None), "struct-valued macro");
    assert!(matches!(macro_expr(&module, "RAYLIB_VERSION").1, Some(MacroValue::Str(_))));

    let keys = enumeration(&module, "KeyboardKey");
    assert_eq!(keys.constants.iter().find(|c| c.name == "KEY_A").map(|c| c.value), Some(65));

    let CType::FnPtr(trace) = &typedef(&module, "TraceLogCallback").ty else { panic!("callback typedef") };
    assert_eq!(trace.params[2].ty, CType::Opaque("va_list".into()));
    assert!(module.items.iter().filter(|item| matches!(item.kind, ItemKind::Function(_))).count() > 500);
}

#[test]
fn sdl3() {
    let Some(path) = locate(Some("WID_SDL3_INCLUDE"), "sdl3", "SDL3/SDL.h") else {
        eprintln!("skipping: SDL3/SDL.h not found (set WID_SDL3_INCLUDE)");
        return;
    };
    let include_dir = path.parent().and_then(Path::parent).map(Path::to_path_buf);
    let module = import_real("SDL3", Header::Include("SDL3/SDL.h".into()), include_dir.as_deref());
    assert!(module.root.ends_with("SDL3"), "SDL3/SDL_*.h belong to the import");

    let init = function(&module, "SDL_Init");
    assert_eq!(init.sig.params[0].ty, named(NamedKind::Typedef, "SDL_InitFlags"));
    assert_eq!(init.sig.ret, CType::Bool);
    assert!(item(&module, "SDL_Init").doc.as_deref().is_some_and(|doc| doc.starts_with("/**")));

    assert_eq!(macro_expr(&module, "SDL_INIT_VIDEO"), (&int(32, false, "unsigned int"), Some(&MacroValue::Int(0x20))));
    assert!(matches!(macro_expr(&module, "SDL_PROP_WINDOW_CREATE_TITLE_STRING").1, Some(MacroValue::Str(_))));

    let event = record(&module, "SDL_Event");
    assert_eq!((event.kind, event.body.as_ref().map(|body| body.size)), (RecordKind::Union, Some(128)));
    let window = record(&module, "SDL_Window");
    assert!(window.is_opaque());
    assert!(module.items.iter().filter(|item| matches!(item.kind, ItemKind::Function(_))).count() > 1000);
}

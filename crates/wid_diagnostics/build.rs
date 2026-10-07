//! Embeds `docs/errors/*.md` so `wid explain` works without the repository.

use std::fmt::Write as _;
use std::path::PathBuf;

fn main() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../docs/errors");
    println!("cargo:rerun-if-changed={}", dir.display());
    let mut entries: Vec<(String, PathBuf)> = Vec::new();
    if let Ok(read) = std::fs::read_dir(&dir) {
        for entry in read.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "md") {
                let code = path.file_stem().unwrap_or_default().to_string_lossy().into_owned();
                println!("cargo:rerun-if-changed={}", path.display());
                entries.push((code, path.canonicalize().unwrap_or(path)));
            }
        }
    }
    entries.sort();
    let mut out = String::from(
        "/// Long-form explanations keyed by error code.\npub static EXPLANATIONS: &[(&str, &str)] = &[\n",
    );
    for (code, path) in &entries {
        let _ = writeln!(out, "    ({code:?}, include_str!({:?})),", path.display().to_string());
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo")).join("explanations.rs");
    std::fs::write(dest, out).expect("write explanations.rs");
}

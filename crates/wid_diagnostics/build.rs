//! Embeds `docs/errors/*.md` so `wid explain` works without the repository.
//! Whole-line HTML comments (tooling markers such as `<!-- flags: … -->`) are
//! dropped from the embedded text.

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
        let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        let _ = writeln!(out, "    ({code:?}, {:?}),", strip_markers(&text));
    }
    out.push_str("];\n");
    let dest = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR is set by cargo")).join("explanations.rs");
    std::fs::write(dest, out).expect("write explanations.rs");
}

/// Removes whole-line HTML comments, and the blank line a removed comment
/// would otherwise leave doubled.
fn strip_markers(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut dropped = false;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim();
        if trimmed.starts_with("<!--") && trimmed.ends_with("-->") {
            dropped = true;
            continue;
        }
        if dropped && trimmed.is_empty() && (out.is_empty() || out.ends_with("\n\n")) {
            dropped = false;
            continue;
        }
        dropped = false;
        out.push_str(line);
    }
    out
}

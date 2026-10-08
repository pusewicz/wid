//! `wid fmt` over every `.wid` file of the repository: formatting keeps the
//! syntax tree and every comment, and formatting twice changes nothing.
//! Files that don't parse cleanly (the `tests/ui` cases with syntax errors)
//! are skipped: `wid fmt` leaves them untouched.
//!
//! `WID_FMT_DIFF=1` prints which files formatting would change, for a PR
//! that reformats the repository.

use std::path::{Path, PathBuf};

use wid_diagnostics::FileId;
use wid_syntax::{fmt, parse_file};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn wid_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            wid_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "wid") {
            out.push(path);
        }
    }
}

#[test]
fn formatting_keeps_the_tree_and_is_idempotent() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["core", "vendor", "examples", "tests"] {
        wid_files(&root.join(dir), &mut files);
    }
    files.sort();
    assert!(files.len() > 200, "found only {} .wid files", files.len());
    let mut failures = Vec::new();
    let (mut formatted, mut skipped, mut changed) = (0, 0, Vec::new());
    for path in &files {
        let name = path.strip_prefix(&root).unwrap_or(path).display().to_string();
        let text = std::fs::read_to_string(path).expect("read a .wid file");
        let (file, diags) = parse_file(FileId(0), &text);
        if !diags.is_empty() {
            skipped += 1;
            continue;
        }
        formatted += 1;
        let once = fmt::format(&text, &file);
        let (again, diags) = parse_file(FileId(0), &once);
        if !diags.is_empty() {
            failures.push(format!("{name}: the formatted file doesn't parse:\n{once}"));
            continue;
        }
        if !fmt::same_tree(&file, &again) {
            failures.push(format!("{name}: formatting changed the syntax tree:\n{once}"));
            continue;
        }
        if again.comments.len() != file.comments.len() {
            failures.push(format!("{name}: {} comments became {}", file.comments.len(), again.comments.len()));
        }
        let twice = fmt::format(&once, &again);
        if twice != once {
            failures.push(format!("{name}: formatting is not idempotent:\n--- once\n{once}\n--- twice\n{twice}"));
        }
        if once != text {
            changed.push(name);
        }
    }
    if std::env::var("WID_FMT_DIFF").is_ok() {
        for name in &changed {
            println!("would change: {name}");
        }
    }
    assert!(formatted > 150, "formatted only {formatted} files ({skipped} skipped)");
    assert!(failures.is_empty(), "{} of {formatted} files failed:\n\n{}", failures.len(), failures.join("\n\n"));
}

/// A small deterministic random number generator.
struct Lcg(u64);

impl Lcg {
    fn next(&mut self, n: u64) -> u64 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        (self.0 >> 33) % n
    }
}

/// Rewrites the whitespace between tokens and comments without changing
/// whether there is any: indentation, the width of spaces, blank lines.
///
/// An own-line comment before a closer keeps the author's choice between
/// the body's indentation and the closer's, by comparing their columns, so
/// comments and lines that start with a closer, `else`, `elsif` or `when`
/// move together, keeping the order of their columns; other lines move at
/// random.
fn perturb(text: &str, rng: &mut Lcg) -> String {
    use wid_syntax::token::{Keyword as K, TokenKind as T};
    let lexed = wid_syntax::lexer::lex(FileId(0), text);
    // Strings and splices are kept as written, so they are one span each.
    // The flag marks the spans whose column must keep its order.
    let mut spans: Vec<(usize, usize, bool)> = Vec::new();
    let mut depth = 0;
    for t in &lexed.tokens {
        let (start, end) = (t.span.start as usize, t.span.end as usize);
        let inside = depth > 0;
        match t.kind {
            T::StrBegin | T::SpliceBegin => depth += 1,
            T::StrEnd | T::SpliceEnd => depth -= 1,
            _ => {}
        }
        if inside {
            if let Some(last) = spans.last_mut() {
                last.1 = end;
            }
        } else if (t.kind != T::Newline || &text[start..end] == ";") && (start != end || t.kind == T::Eof) {
            let closer =
                matches!(t.kind, T::Kw(K::End | K::Else | K::Elsif | K::When) | T::RParen | T::RBracket | T::RBrace);
            spans.push((start, end, closer));
        }
    }
    spans.extend(lexed.comments.iter().map(|c| (c.span.start as usize, c.span.end as usize, true)));
    spans.sort();
    let (scale, shift) = (1 + rng.next(3) as usize, rng.next(4) as usize);
    let mut out = String::new();
    let mut at = 0;
    for (start, end, ordered) in spans {
        if start < at {
            continue;
        }
        let gap = &text[at..start];
        if gap.contains('\\') || !gap.chars().all(char::is_whitespace) {
            out.push_str(gap);
        } else if gap.contains('\n') {
            // One blank line is the author's choice; more are not.
            let breaks = gap.matches('\n').count();
            let breaks = if breaks >= 2 { breaks + rng.next(3) as usize } else { breaks };
            out.push_str(&"\n".repeat(breaks));
            let column = gap.len() - gap.rfind('\n').map_or(0, |i| i + 1);
            let indent = if ordered { column * scale + shift } else { rng.next(7) as usize };
            out.push_str(&" ".repeat(indent));
        } else if !gap.is_empty() {
            out.push_str(&" ".repeat(1 + rng.next(3) as usize));
        }
        out.push_str(&text[start..end]);
        at = end;
    }
    out.push_str(&text[at..]);
    out
}

#[test]
fn perturbed_whitespace_formats_the_same() {
    let root = repo_root();
    let mut files = Vec::new();
    for dir in ["core", "vendor", "examples", "tests"] {
        wid_files(&root.join(dir), &mut files);
    }
    files.sort();
    let mut rng = Lcg(7);
    let (mut checked, mut failures) = (0, Vec::new());
    for path in &files {
        let name = path.strip_prefix(&root).unwrap_or(path).display().to_string();
        let text = std::fs::read_to_string(path).expect("read a .wid file");
        let (file, diags) = parse_file(FileId(0), &text);
        if !diags.is_empty() {
            continue;
        }
        let canonical = fmt::format(&text, &file);
        let messy = perturb(&text, &mut rng);
        let (messy_file, diags) = parse_file(FileId(0), &messy);
        // Whitespace can matter (a named argument's value on the next line
        // must be indented deeper); skip the rare perturbation that changed
        // the parse.
        if !diags.is_empty() || !fmt::same_tree(&file, &messy_file) {
            continue;
        }
        checked += 1;
        let out = fmt::format(&messy, &messy_file);
        if out != canonical {
            failures.push(format!("{name}:\n--- canonical\n{canonical}\n--- from perturbed\n{out}"));
        }
    }
    assert!(checked > 150, "checked only {checked} files");
    assert!(failures.is_empty(), "{} files format differently:\n\n{}", failures.len(), failures.join("\n\n"));
}

//! Diagnostics infrastructure for the Wid compiler: source maps, spans, the
//! diagnostic model, the error-code registry and the renderers.

pub mod codes;
mod diagnostic;
mod render;
mod source;

pub use codes::{Code, CodeInfo};
pub use diagnostic::{Applicability, Diagnostic, Diagnostics, Edit, Help, Label, Severity};
pub use render::{RenderOptions, render, render_all, render_all_with, render_json, to_json};
pub use source::{Expansion, FileId, SourceFile, SourceMap, Span};

mod explanations {
    include!(concat!(env!("OUT_DIR"), "/explanations.rs"));
}

/// Returns the long-form explanation for `code`, if one exists.
pub fn explain(code: &str) -> Option<&'static str> {
    explanations::EXPLANATIONS.iter().find(|(c, _)| c.eq_ignore_ascii_case(code)).map(|(_, text)| *text)
}

/// Returns every code that has an explanation document.
pub fn explained_codes() -> impl Iterator<Item = &'static str> {
    explanations::EXPLANATIONS.iter().map(|(c, _)| *c)
}

/// Joins items into an English list for a message: `a`, `a and b`, or
/// `a, b and c`.
pub fn and_list<S: AsRef<str>>(items: &[S]) -> String {
    match items {
        [] => String::new(),
        [one] => one.as_ref().to_string(),
        [rest @ .., last] => {
            let rest: Vec<&str> = rest.iter().map(AsRef::as_ref).collect();
            format!("{} and {}", rest.join(", "), last.as_ref())
        }
    }
}

/// Computes the edit distance between two strings, for "did you mean" hints.
/// Insertions, deletions, substitutions and swaps of two neighbouring
/// characters each cost one, so `pirnt` is one edit from `print`.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut before = vec![0; b.len() + 1];
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                cur[j] = cur[j].min(before[j - 2] + 1);
            }
        }
        std::mem::swap(&mut before, &mut prev);
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Picks the closest candidate to `name`, if any is close enough to suggest.
///
/// Among equally close candidates the shorter one wins, then the one that
/// comes first in `candidates`. Callers pass candidates in an order that is
/// the same on every run (sorted, or in the order names resolve), never in a
/// hash map's iteration order, so the suggestion is too.
pub fn did_you_mean<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    // One edit turns a one- or two-letter name into an unrelated one, so
    // short names only match when they differ in case.
    let len = name.chars().count();
    let max = if len <= 2 { 0 } else { (len / 3).max(1) };
    candidates
        .into_iter()
        // A Wid name with `#{` (a splice's placeholder, or a name glued from
        // text and splices) can't be written, so it is never suggested.
        .filter(|c| *c != name && !c.contains("#{"))
        .map(|c| (edit_distance(&name.to_lowercase(), &c.to_lowercase()), c))
        .filter(|(d, _)| *d <= max)
        .min_by_key(|(d, c)| (*d, c.len()))
        .map(|(_, c)| c)
}

#[cfg(test)]
mod tests {
    use super::{and_list, did_you_mean, edit_distance};

    #[test]
    fn transpositions_cost_one_edit() {
        assert_eq!(edit_distance("pirnt", "print"), 1);
        assert_eq!(edit_distance("bonsu", "bonus"), 1);
        assert_eq!(edit_distance("nroth", "north"), 1);
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
    }

    #[test]
    fn suggests_the_closest_candidate() {
        assert_eq!(did_you_mean("tset", ["test", "export"]), Some("test"));
        assert_eq!(did_you_mean("xyz", ["test", "export"]), None);
        assert_eq!(did_you_mean("d", ["p", "e"]), None);
        assert_eq!(did_you_mean("X", ["x"]), Some("x"));
    }

    #[test]
    fn ties_go_to_the_shorter_then_the_first_candidate() {
        assert_eq!(did_you_mean("fooe", ["food", "fooa", "foob"]), Some("food"));
        assert_eq!(did_you_mean("fooe", ["fooa", "food", "foob"]), Some("fooa"));
        assert_eq!(did_you_mean("fooe", ["fooes", "fooa"]), Some("fooa"));
    }

    #[test]
    fn lists_read_as_english() {
        assert_eq!(and_list::<&str>(&[]), "");
        assert_eq!(and_list(&["`a`"]), "`a`");
        assert_eq!(and_list(&["`a`", "`b`"]), "`a` and `b`");
        assert_eq!(and_list(&["`a`", "`b`", "`d`"]), "`a`, `b` and `d`");
    }
}

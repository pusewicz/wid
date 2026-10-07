//! Keeps the editor grammars in `extras/` in sync with the lexer.

use std::collections::BTreeSet;

use wid_syntax::token::Keyword;

/// Returns the words `Keyword::from_ident` maps, read from its match arms so
/// that a newly added keyword can't be missed.
fn lexer_keywords() -> BTreeSet<&'static str> {
    include_str!("../src/token.rs")
        .lines()
        .filter_map(|line| line.trim().strip_prefix('"')?.split_once("\" => Keyword::"))
        .map(|(word, _)| word)
        .collect()
}

/// Returns the reserved words listed in `s:reserved` of the Vim syntax file.
fn vim_reserved_words() -> Vec<&'static str> {
    let text = include_str!("../../../extras/vim/syntax/wid.vim");
    let start = text.find("let s:reserved = [").expect("invariant: the Vim syntax file has `s:reserved`");
    let list = &text[start..];
    let list = &list[..list.find("\\ ]").expect("invariant: `s:reserved` is closed")];
    list.lines()
        .filter_map(|line| line.trim_start().strip_prefix("\\ ['wid"))
        .flat_map(|entry| {
            entry.split('\'').nth(2).expect("invariant: each entry lists its words second").split_whitespace()
        })
        .collect()
}

#[test]
fn lexer_keywords_are_parsed() {
    let keywords = lexer_keywords();
    assert!(keywords.len() > 30, "found only {keywords:?} in token.rs");
    for word in keywords {
        assert!(Keyword::from_ident(word).is_some(), "`{word}` is not a keyword");
    }
}

#[test]
fn vim_syntax_lists_every_keyword_once() {
    let words = vim_reserved_words();
    let unique: BTreeSet<&str> = words.iter().copied().collect();
    assert_eq!(unique.len(), words.len(), "a word appears twice in `s:reserved`: {words:?}");

    let keywords = lexer_keywords();
    let missing: Vec<_> = keywords.difference(&unique).collect();
    let extra: Vec<_> = unique.difference(&keywords).collect();
    assert!(
        missing.is_empty() && extra.is_empty(),
        "extras/vim/syntax/wid.vim is out of sync with the lexer: missing {missing:?}, not keywords {extra:?}"
    );
}

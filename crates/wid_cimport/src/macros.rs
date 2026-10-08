//! Macro definitions: classification, and the probe that types object-like
//! macros by expanding them in a second translation unit.
// libclang constants keep their C names.
#![allow(non_upper_case_globals)]

use std::collections::{HashMap, HashSet};

use clang_sys::*;

use crate::ffi::{Cursor, Diagnostic, EvalValue, Index, ParseOptions, Severity, Token, TokenKind, TranslationUnit};
use crate::lower::{Lowerer, is_identifier, name_params};
use crate::model::*;
use crate::source::{preceding_doc_comment, trailing_comment};

/// A macro definition found in the imported headers.
pub(crate) struct Definition {
    /// The macro name.
    pub name: String,
    /// The replacement list.
    pub body: Vec<Token>,
    /// Function-like parameters and variadic flag, or `None` for an
    /// object-like macro.
    pub params: Option<(Vec<String>, bool)>,
    /// Where the name is written.
    pub location: Location,
    /// The preprocessing order key.
    pub order: Vec<u32>,
    /// The documentation comment.
    pub doc: Option<String>,
}

/// Collects the macro definitions in the imported headers, skipping builtins,
/// include guards and object-like macros with an empty replacement list.
pub(crate) fn definitions(tu: &TranslationUnit<'_>, cursors: &[Cursor<'_>], lowerer: &mut Lowerer) -> Vec<Definition> {
    let mut guards: HashMap<String, Option<String>> = HashMap::new();
    let mut out = Vec::new();
    for &cursor in cursors {
        if cursor.kind() != CXCursor_MacroDefinition || cursor.is_macro_builtin() || !lowerer.is_owned(cursor) {
            continue;
        }
        let name = cursor.spelling();
        let tokens = tu.tokenize(cursor.extent());
        if tokens.first().is_none_or(|first| first.spelling != name) {
            continue;
        }
        let (params, body) = if cursor.is_macro_function_like() {
            let (params, variadic, rest) = parse_params(&tokens[1..]);
            (Some((params, variadic)), rest.to_vec())
        } else {
            (None, tokens[1..].to_vec())
        };
        let spot = cursor.location().expansion();
        let Some(file) = spot.file.clone() else { continue };
        if params.is_none() {
            if body.is_empty() {
                continue;
            }
            let guard = guards.entry(file.clone()).or_insert_with(|| {
                let guarded = cursor.location().file().is_some_and(|handle| tu.is_include_guarded(handle));
                guarded.then(|| lowerer.sources.text(&file).and_then(guard_macro)).flatten()
            });
            if guard.as_deref() == Some(name.as_str()) {
                continue;
            }
        }
        let doc = lowerer.sources.text(&file).and_then(|text| {
            let end = body.last().map_or(spot.offset, |token| token.end) as usize;
            preceding_doc_comment(text, spot.offset as usize).or_else(|| trailing_comment(text, end))
        });
        let order = lowerer.sources.order_key(&spot);
        out.push(Definition { name, body, params, location: Lowerer::location(cursor), order, doc });
    }
    out
}

/// Splits a function-like macro's tokens after the name into parameter
/// names, the variadic flag and the replacement list.
fn parse_params(tokens: &[Token]) -> (Vec<String>, bool, &[Token]) {
    let mut params = Vec::new();
    let mut variadic = false;
    let mut index = usize::from(tokens.first().is_some_and(|token| token.spelling == "("));
    while let Some(token) = tokens.get(index) {
        index += 1;
        match token.spelling.as_str() {
            ")" => break,
            "," => {}
            "..." => variadic = true,
            name => params.push(name.to_string()),
        }
    }
    (params, variadic, &tokens[index.min(tokens.len())..])
}

/// The include guard named by a file's leading `#ifndef NAME` or
/// `#if !defined(NAME)`, skipping comments and whitespace before it.
fn guard_macro(text: &[u8]) -> Option<String> {
    let mut at = 0;
    loop {
        while at < text.len() && text[at].is_ascii_whitespace() {
            at += 1;
        }
        if text[at..].starts_with(b"//") {
            at += text[at..].iter().position(|&byte| byte == b'\n')?;
        } else if text[at..].starts_with(b"/*") {
            at += text[at..].windows(2).position(|window| window == b"*/")? + 2;
        } else {
            break;
        }
    }
    let line_end = text[at..].iter().position(|&byte| byte == b'\n').map_or(text.len(), |len| at + len);
    let line = String::from_utf8_lossy(&text[at..line_end]);
    let directive = line.strip_prefix('#')?.trim_start();
    let name = if let Some(rest) = directive.strip_prefix("ifndef") {
        rest.trim()
    } else {
        let rest = directive.strip_prefix("if")?.trim_start().strip_prefix('!')?.trim_start();
        rest.strip_prefix("defined")?.trim_matches(|c: char| c == '(' || c == ')' || c.is_whitespace())
    };
    let name: String = name.chars().take_while(|&c| c == '_' || c.is_ascii_alphanumeric()).collect();
    is_identifier(&name).then_some(name)
}

/// Joins tokens with a single space wherever the source had whitespace.
fn body_text(tokens: &[Token]) -> String {
    let mut text = String::new();
    for (index, token) in tokens.iter().enumerate() {
        if index > 0 && tokens[index - 1].end < token.start {
            text.push(' ');
        }
        text.push_str(&token.spelling);
    }
    text
}

/// Whether the brackets in `tokens` nest properly.
fn balanced(tokens: &[Token]) -> bool {
    let mut stack = Vec::new();
    for token in tokens.iter().filter(|token| token.kind == TokenKind::Punctuation) {
        match token.spelling.as_str() {
            "(" | "[" | "{" => stack.push(token.spelling.as_str()),
            ")" if stack.pop() != Some("(") => return false,
            "]" if stack.pop() != Some("[") => return false,
            "}" if stack.pop() != Some("{") => return false,
            _ => {}
        }
    }
    stack.is_empty()
}

/// Turns the definitions into macro items, typing every object-like macro
/// that expands to an expression with one extra parse of `include` followed
/// by a probe function per macro.
pub(crate) fn lower(
    index: &Index,
    file_name: &str,
    include: &str,
    args: &[String],
    definitions: Vec<Definition>,
    lowerer: &mut Lowerer,
) -> Result<(), i32> {
    let mut seen = HashSet::new();
    let definitions: Vec<Definition> = {
        // The last definition of a name is the one in effect after the headers.
        let mut unique: Vec<Definition> =
            definitions.into_iter().rev().filter(|def| seen.insert(def.name.clone())).collect();
        unique.reverse();
        unique
    };
    let candidates: Vec<usize> = definitions
        .iter()
        .enumerate()
        .filter(|(_, def)| def.params.is_none() && balanced(&def.body))
        .map(|(position, _)| position)
        .collect();
    let probes = probe(index, file_name, include, args, &definitions, &candidates, lowerer)?;
    for (position, def) in definitions.into_iter().enumerate() {
        let kind = match &def.params {
            Some((params, variadic)) => MacroKind::FunctionLike { params: params.clone(), variadic: *variadic },
            None => match probes.get(&position) {
                Some((ty, value)) => {
                    let value = value.clone().map(|value| classify(value, ty, &def.body));
                    let value = value.or_else(|| decode_strings(&def.body).map(MacroValue::Str));
                    MacroKind::Expr { ty: ty.clone(), value }
                }
                None => MacroKind::Other,
            },
        };
        let item = Item {
            location: def.location,
            doc: def.doc,
            kind: ItemKind::Macro(Macro { name: def.name.clone(), body: body_text(&def.body), kind }),
        };
        lowerer.add_macro(def.order, item, &def.name);
    }
    Ok(())
}

/// Parses the probe unit and returns, for each candidate that expands to a
/// valid expression, its type and evaluated value.
fn probe(
    index: &Index,
    file_name: &str,
    include: &str,
    args: &[String],
    definitions: &[Definition],
    candidates: &[usize],
    lowerer: &mut Lowerer,
) -> Result<HashMap<usize, (CType, Option<EvalValue>)>, i32> {
    let mut results = HashMap::new();
    if candidates.is_empty() {
        return Ok(results);
    }
    // One function per line, so every diagnostic maps to one macro and a
    // broken expansion cannot swallow its neighbours. No parentheses around
    // the macro: libclang only evaluates a string literal that is the whole
    // initializer, and a top-level comma then fails the probe as it should.
    // `__extension__` keeps `-pedantic-errors` from rejecting `__auto_type`.
    let first_line = include.lines().count() as u32 + 1;
    let mut source = String::from(include);
    for (probe, &position) in candidates.iter().enumerate() {
        let name = &definitions[position].name;
        source.push_str(&format!(
            "void __wid_probe_{probe}(void) {{ __extension__ __auto_type __wid_m_{name} = {name}; }}\n"
        ));
    }
    let mut probe_args = args.to_vec();
    probe_args.push("-w".to_string());
    let tu = index.parse(file_name, &source, &probe_args, ParseOptions::default())?;
    let failed: HashSet<u32> = tu
        .diagnostics()
        .into_iter()
        .filter(|diag: &Diagnostic| {
            diag.severity >= Severity::Error && diag.location.file.as_deref() == Some(file_name)
        })
        .filter_map(|diag| diag.location.line.checked_sub(first_line))
        .collect();
    for function in tu.cursor().children() {
        if function.kind() != CXCursor_FunctionDecl {
            continue;
        }
        let Some(probe) = function.spelling().strip_prefix("__wid_probe_").and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        let Some(&position) = candidates.get(probe as usize) else { continue };
        if failed.contains(&probe) {
            continue;
        }
        let Some(var) = find_var(function) else { continue };
        let ty = var.ty();
        if matches!(ty.kind(), CXType_Invalid) || ty.canonical().kind() == CXType_Auto {
            continue;
        }
        let mut lowered = lowerer.ty(ty);
        // A function type carries no parameter names; a macro that names a
        // function (`#define ALIAS InitWindow`) takes them from its
        // declaration.
        if let Some(decl) = named_declaration(var) {
            name_params(&mut lowered, decl);
        }
        results.insert(position, (lowered, var.evaluate()));
    }
    Ok(results)
}

/// The function, or function-pointer variable, that a probe variable's
/// initializer names when it is just a name, through any implicit
/// conversions and parentheses.
fn named_declaration<'tu>(var: Cursor<'tu>) -> Option<Cursor<'tu>> {
    let mut expr = *var.children().last()?;
    loop {
        match expr.kind() {
            CXCursor_DeclRefExpr => {
                let decl = expr.referenced();
                return matches!(decl.kind(), CXCursor_FunctionDecl | CXCursor_VarDecl).then_some(decl);
            }
            CXCursor_UnexposedExpr | CXCursor_ParenExpr => match expr.children().as_slice() {
                [inner] => expr = *inner,
                _ => return None,
            },
            _ => return None,
        }
    }
}

/// The first variable declared under `cursor`.
fn find_var<'tu>(cursor: Cursor<'tu>) -> Option<Cursor<'tu>> {
    cursor
        .children()
        .into_iter()
        .find_map(|child| if child.kind() == CXCursor_VarDecl { Some(child) } else { find_var(child) })
}

/// Interprets an evaluated value in light of the macro's type and spelling.
fn classify(value: EvalValue, ty: &CType, body: &[Token]) -> MacroValue {
    match value {
        EvalValue::Int(value) if *ty == CType::Bool => MacroValue::Bool(value != 0),
        EvalValue::Int(value) if is_char_literal(body) => MacroValue::Char(value),
        EvalValue::Int(value) => MacroValue::Int(value),
        EvalValue::Float(value) => MacroValue::Float(value),
        EvalValue::Str(bytes) => MacroValue::Str(bytes),
    }
}

/// The tokens inside any number of enclosing parentheses.
fn unparenthesized(mut tokens: &[Token]) -> &[Token] {
    while tokens.len() >= 2
        && tokens[0].spelling == "("
        && tokens[tokens.len() - 1].spelling == ")"
        && balanced(&tokens[1..tokens.len() - 1])
    {
        tokens = &tokens[1..tokens.len() - 1];
    }
    tokens
}

/// Whether the body is a single character constant, such as `'a'`.
fn is_char_literal(body: &[Token]) -> bool {
    match unparenthesized(body) {
        [token] => {
            token.kind == TokenKind::Literal
                && token.spelling.trim_start_matches(['L', 'u', 'U', '8']).starts_with('\'')
        }
        _ => false,
    }
}

/// Decodes a body made only of narrow string literals, such as
/// `("a" "b")`, which libclang will not evaluate once parenthesised.
fn decode_strings(body: &[Token]) -> Option<Vec<u8>> {
    let tokens = unparenthesized(body);
    if tokens.is_empty() {
        return None;
    }
    let mut out = Vec::new();
    for token in tokens {
        let literal = token.spelling.strip_prefix("u8").unwrap_or(&token.spelling);
        let inner = literal.strip_prefix('"')?.strip_suffix('"')?;
        if token.kind != TokenKind::Literal {
            return None;
        }
        decode_escapes(inner, &mut out)?;
    }
    Some(out)
}

/// Appends the bytes of a string literal's contents, resolving escapes.
fn decode_escapes(text: &str, out: &mut Vec<u8>) -> Option<()> {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] != b'\\' {
            out.push(bytes[at]);
            at += 1;
            continue;
        }
        let escape = *bytes.get(at + 1)?;
        at += 2;
        let simple = match escape {
            b'n' => Some(b'\n'),
            b't' => Some(b'\t'),
            b'r' => Some(b'\r'),
            b'a' => Some(0x07),
            b'b' => Some(0x08),
            b'f' => Some(0x0c),
            b'v' => Some(0x0b),
            b'e' => Some(0x1b),
            b'\\' | b'\'' | b'"' | b'?' => Some(escape),
            _ => None,
        };
        if let Some(byte) = simple {
            out.push(byte);
            continue;
        }
        match escape {
            b'0'..=b'7' => {
                let start = at - 1;
                let len = bytes[start..].iter().take(3).take_while(|byte| (b'0'..=b'7').contains(byte)).count();
                out.push(u8::from_str_radix(std::str::from_utf8(&bytes[start..start + len]).ok()?, 8).ok()?);
                at = start + len;
            }
            b'x' => {
                let len = bytes[at..].iter().take_while(|byte| byte.is_ascii_hexdigit()).count();
                let value = u32::from_str_radix(std::str::from_utf8(&bytes[at..at + len]).ok()?, 16).ok()?;
                out.push(u8::try_from(value).ok()?);
                at += len;
            }
            b'u' | b'U' => {
                let len = if escape == b'u' { 4 } else { 8 };
                let digits = std::str::from_utf8(bytes.get(at..at + len)?).ok()?;
                let c = char::from_u32(u32::from_str_radix(digits, 16).ok()?)?;
                out.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
                at += len;
            }
            _ => return None,
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds tokens from spellings separated by single spaces.
    fn tokens(spellings: &[(&str, TokenKind)]) -> Vec<Token> {
        let mut offset = 0;
        spellings
            .iter()
            .map(|(spelling, kind)| {
                let token = Token {
                    kind: *kind,
                    spelling: spelling.to_string(),
                    start: offset,
                    end: offset + spelling.len() as u32,
                };
                offset = token.end + 1;
                token
            })
            .collect()
    }

    #[test]
    fn guards_are_found_after_comments() {
        assert_eq!(guard_macro(b"/* c */\n// d\n#ifndef FOO_H\n#define FOO_H\n").as_deref(), Some("FOO_H"));
        assert_eq!(guard_macro(b"#if !defined(BAR_H)\n").as_deref(), Some("BAR_H"));
        assert_eq!(guard_macro(b"#pragma once\n"), None);
    }

    #[test]
    fn parenthesized_strings_decode() {
        use TokenKind::*;
        let body =
            tokens(&[("(", Punctuation), ("\"a\\n\"", Literal), ("u8\"\\x41\\101\"", Literal), (")", Punctuation)]);
        assert_eq!(decode_strings(&body), Some(b"a\nAA".to_vec()));
        assert_eq!(decode_strings(&tokens(&[("L\"w\"", Literal)])), None);
    }

    #[test]
    fn brackets_must_nest() {
        use TokenKind::*;
        assert!(balanced(&tokens(&[("(", Punctuation), ("{", Punctuation), ("}", Punctuation), (")", Punctuation)])));
        assert!(!balanced(&tokens(&[(")", Punctuation), ("(", Punctuation)])));
        assert!(!balanced(&tokens(&[("{", Punctuation)])));
    }
}

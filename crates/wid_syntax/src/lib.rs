//! Lexing and parsing for the Wid language.

pub mod ast;
pub mod docs;
pub mod intern;
pub mod lexer;
pub mod parser;
pub mod print;
pub mod token;
pub mod visit;

pub use intern::Name;
pub use parser::{parse_expr_str, parse_file};

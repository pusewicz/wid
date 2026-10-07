//! Pretty-prints selected items of a header: `show <header> NAME…`.

use std::path::PathBuf;

use wid_cimport::{Header, ImportRequest, import};

/// Imports the header and prints the named items.
fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some((target, names)) = args.split_first() else {
        eprintln!("usage: show <header|<name>> NAME…");
        std::process::exit(2);
    };
    let header = match target.strip_prefix('<').and_then(|name| name.strip_suffix('>')) {
        Some(name) => Header::Include(name.to_string()),
        None => Header::Path(PathBuf::from(target)),
    };
    match import(&ImportRequest::new(header)) {
        Ok(module) => {
            for item in
                module.items.iter().filter(|item| item.name().is_some_and(|name| names.iter().any(|n| n == name)))
            {
                println!("{item:#?}");
            }
        }
        Err(error) => {
            eprintln!("error: {error}");
            std::process::exit(1);
        }
    }
}

//! `wid lsp`: the Wid language server, speaking LSP over stdin and stdout
//! (SPEC "Toolchain and CLI").
//!
//! It is a synchronous loop with no async runtime: a thread reads framed
//! JSON-RPC messages ([`transport`]), and the server handles them one at a
//! time ([`server`]). It checks packages with `wid_driver::analyze`, passing
//! the open documents as an overlay so unsaved text is what gets checked,
//! and answers from the same `wid_query` engine as `wid query`:
//!
//! - diagnostics for every file of the package an open document belongs
//!   to, on open, change (debounced) and save, with the compiler's fixes as
//!   quick fixes ([`convert`]);
//! - hover, go-to-definition and document symbols, from `type`, `def` and
//!   `outline` ([`features`]);
//! - formatting, with `wid_syntax::fmt`.
//!
//! It never generates C or runs a C compiler.

mod convert;
mod features;
mod position;
mod server;
mod transport;
mod uri;

use std::collections::HashMap;
use std::path::PathBuf;

/// What the command line sets: `wid lsp -collection:… -define:…
/// -target:…`. The client's `initializationOptions` add to it.
#[derive(Clone, Debug, Default)]
pub struct Config {
    /// `-collection:name=path` roots.
    pub collections: HashMap<String, PathBuf>,
    /// `-define:NAME=value` constants.
    pub defines: HashMap<String, String>,
    /// `-target:os_arch`.
    pub target: Option<(String, String)>,
}

/// Runs the server on stdin and stdout until the client sends `exit` or
/// closes stdin, and returns the exit status: 0 when the client sent
/// `shutdown` first, 1 otherwise. When stdout is closed, so that the server
/// can't answer, it stops quietly with status 1.
pub fn run(config: Config) -> i32 {
    let messages = match transport::spawn_reader(std::io::stdin()) {
        Ok(messages) => messages,
        Err(e) => {
            log(&format!("cannot start reading stdin: {e}"));
            return 1;
        }
    };
    let stdout = std::io::stdout();
    server::serve(config, &messages, stdout.lock())
}

/// Writes a line to stderr, which editors keep as the server's log. A
/// closed or failing stderr is ignored: the log is only for people.
pub(crate) fn log(message: &str) {
    // Under `cargo test`, `eprintln!` is captured with the test's output.
    #[cfg(test)]
    eprintln!("wid lsp: {message}");
    #[cfg(not(test))]
    {
        use std::io::Write;
        let _ = writeln!(std::io::stderr().lock(), "wid lsp: {message}");
    }
}

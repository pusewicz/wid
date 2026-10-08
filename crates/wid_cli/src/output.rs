//! Writing to stdout and stderr without panicking.
//!
//! `print!` and `eprint!` panic when a write fails, and a write to a pipe
//! whose reader has exited (`wid query outline | head -c 10`) fails with
//! `BrokenPipe`, because Rust ignores `SIGPIPE`. Every command writes
//! through [`out`] and [`err`] instead. Output to a closed pipe is dropped,
//! the command finishes, and `wid` exits with the status the command has
//! anyway (SPEC "Toolchain and CLI"). Any other failed write, like one to a
//! full disk, ends `wid` with status 1 and says so on stderr.

use std::io::{self, ErrorKind, Write};

/// Writes `text` to stdout.
pub fn out(text: &str) {
    write(&mut io::stdout().lock(), text, "stdout");
}

/// Writes `text` to stderr.
pub fn err(text: &str) {
    write(&mut io::stderr().lock(), text, "stderr");
}

fn write(stream: &mut impl Write, text: &str, name: &str) {
    match stream.write_all(text.as_bytes()).and_then(|()| stream.flush()) {
        Ok(()) => {}
        // The reader has gone, and won't read the rest either.
        Err(e) if e.kind() == ErrorKind::BrokenPipe => {}
        Err(e) => {
            let _ = writeln!(io::stderr(), "error: cannot write to {name}: {e}");
            std::process::exit(1);
        }
    }
}

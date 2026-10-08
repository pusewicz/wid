# Wid

Wid is a language that looks like Ruby, borrows its semantics from Odin and
compiles to C23. It is statically typed, manages memory manually without a GC,
and can import C headers directly. It is built for 2D games and anything else
that runs close to the metal.

```ruby
def main
  name = "world"
  puts "Hello, #{name}!"
end
```

```sh
wid run .
```

Status: design phase. See [SPEC.md](SPEC.md).

Editor support: [Vim](extras/vim), and the language server for Neovim and VS
Code ([docs/editors.md](docs/editors.md)).

## Building

Wid runs on Linux and macOS. You need:

- Rust 1.88 or newer
- a C23 compiler: clang 19 or newer, or gcc 15 or newer
- libclang (LLVM 11 or newer) for `cimport` and every `vendor:` package;
  `wid` finds it through `LIBCLANG_PATH`, `llvm-config` or the usual install
  locations
- pkg-config and the system library for `vendor:raylib` and `vendor:sdl3`

Build an optimised `wid` and put it on your `PATH`, or install it into
`~/.cargo/bin` with `cargo install --locked --path crates/wid_cli`:

```sh
cargo build --release                    # builds target/release/wid
export PATH="$PWD/target/release:$PATH"
```

`wid` reads the `core:` and `vendor:` collections from `WID_ROOT` if it is
set; otherwise from the nearest directory that holds `core/` and
`runtime/wid_runtime.h`, starting at the binary's own and going up; otherwise
from the checkout it was built from. So a `wid` built here works anywhere
while the checkout stays put. To use it without the checkout, copy `core/`,
`vendor/` and `runtime/` next to the binary or next to its `bin/` directory.

`wid` compiles with the C compiler that `-cc:` names, then `WID_CC`, `CC` or
`cc`. Save the example above as `hello.wid` and run it:

```sh
wid run hello.wid -file             # Hello, world!
wid run hello.wid -file -cc:gcc-15  # when `cc` is too old for C23 (E0702)
```

## Developing

`cargo build` makes a debug build at `target/debug/wid`. Every pull request
passes:

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo +1.88 check --workspace --all-targets --locked  # the minimum Rust version
cargo test
ruby scripts/errdocs_drift.rb  # after a diagnostic change; uses target/debug/wid
```

`cargo test` builds the language suite with `clang`, and with `gcc-16` or
`gcc-15` when one is installed. `WID_TEST_CC=clang,gcc-15` picks the
compilers instead, `WID_TEST_FILTER=name` runs only the cases whose name
contains `name`, and `WID_BLESS=1 cargo test -p wid_driver --test suite`
rewrites the expected output (review the diff). [CLAUDE.md](CLAUDE.md) has
the layout, the conventions and the bar for diagnostics.

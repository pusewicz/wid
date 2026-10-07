# CLAUDE.md

Wid is a Ruby-syntax, Odin-semantics systems language that compiles to C23. The
compiler is written in Rust. `SPEC.md` is the design. `docs/STATUS.md` tracks
what is implemented, which conventions are fixed and what comes next.

## Ground rules

- `SPEC.md` is the source of truth for the language. Any change to syntax or
  semantics updates `SPEC.md` in the same change. If the code and the spec
  disagree, stop and ask.
- A feature isn't done until its diagnostics are excellent. Programmer happiness
  and LLM-friendliness are requirements, not polish.
- Never introduce hidden allocations or hidden control flow, whether in the
  language or in the generated C.

## Layout

- `crates/wid_diagnostics`: spans, source map, error-code registry, human and
  JSON renderers
- `crates/wid_syntax`: lexer, error-recovering parser, AST
- `crates/wid_sema`: name resolution, type checking, flow typing and lowering to
  the typed IR; `src/interp/` runs that IR at compile time (`comptime`,
  constants), mirroring `runtime/wid_runtime.h`
- `crates/wid_codegen_c`: IR → C23 emitter
- `crates/wid_driver`: package loading, the stage pipeline, C compiler invocation,
  and the test suite (`crates/wid_driver/tests/suite.rs`)
- `crates/wid_cli`: the `wid` binary (Odin-style CLI)
- `crates/wid_cimport`: libclang header import, a pure-data model of C
  declarations (libclang is loaded lazily); `wid_driver/src/cimport/` renders
  it as a Wid package and `wid_sema/src/check/cimport.rs` maps it to C
- `crates/wid_lsp` (planned): LSP server, sharing the query engine with
  `wid query`
- `runtime/wid_runtime.h`: the C23 runtime, embedded into the compiler
- `core/`, `vendor/`: Wid collections. Each `core` package has `_test.wid`
  files that the suite runs with `wid test`. `core` code reaches the runtime
  through `@[extern("wid_…")]`; the runtime header declares those symbols, so
  the compiler emits no prototype for them. A `vendor` package is a `cimport`
  without `as:` (its names become the package's) with a short doc header;
  vendored C sources keep their license next to them.
- `examples/`: programs that need system libraries (built, not run, by
  `crates/wid_driver/tests/vendor.rs`)
- `docs/errors/EXXXX.md`: `wid explain` text, one file per code. Whole-line
  HTML comments are tooling markers (`<!-- flags: … -->`,
  `<!-- drift: skip REASON -->`) and are stripped from `wid explain`.
- `scripts/`: Ruby helpers. `errdocs_drift.rb` checks every docs/errors
  example against the compiler; `errdocs_update.rb` rewrites drifted output;
  `probe.rb` runs one-off programs through `wid check`.
- `tests/run/`: `.wid` programs with expected `.stdout` (and optional
  `.stderr`, `.exitcode`)
- `tests/ui/`: `.wid` files (or directory packages, for imports) with expected
  diagnostics in `NAME.stderr`
- `tests/test/`: packages run with `wid test`, with the expected report in
  `NAME.stdout`
- `tests/vendor/`: programs using pkg-config libraries (raylib, SDL3), run
  by `tests/vendor.rs` and skipped when the library is missing

## Commands

- Build: `cargo build`
- Test: `cargo test`
- Update expectations: `WID_BLESS=1 cargo test -p wid_driver --test suite`, then
  review the diff. Filter with `WID_TEST_FILTER=name`; pick compilers with
  `WID_TEST_CC=clang,gcc-16`.
- Lint: `cargo clippy --all-targets -- -D warnings`
- Format: `cargo fmt --check`
- Error docs: `cargo build && ruby scripts/errdocs_drift.rb` after any
  diagnostic change (E0902 fails until macros land).

## Workflow

- Work happens on branches in git worktrees, one task per branch, landing as
  stacked GitHub PRs. The orchestrating session follows
  `docs/ORCHESTRATOR.md`; a subagent works only in its own worktree, commits
  there with `git commit -m`, and never pushes unless told to.
- Every PR passes the full gate (fmt, clippy, `cargo test` with every
  available C compiler) and updates `docs/STATUS.md`.

## Diagnostics bar

Every new error needs all of the following:

- a stable code
- a labeled primary span
- a plain-English explanation of why it is wrong
- at least one concrete fix, machine-applicable when possible
- a `docs/errors/` entry
- a `tests/ui/` case

The test suite fails when a code that appears in `tests/ui` lacks docs. The
parser and checker recover and report every error, never just the first one.

## Generated C

- The output must compile warning-free with
  `-std=c23 -Wall -Wextra -Wpedantic -Werror` on clang ≥ 19 and gcc ≥ 15. The
  suite builds every run test with both when they are available. Programs are
  always compiled with `-fwrapv -fno-strict-aliasing`: integer arithmetic wraps
  and typed container headers are accessed through type-erased runtime views.
- A run or ui test can list flags (`-debug`, `-no-bounds-check`, `-o:speed`,
  `-define:NAME=value`, `-target:os_arch`) in `NAME.flags`.
- Emit `#line` directives in `-debug` builds and keep the output readable, with
  stable names and `wid_`-prefixed runtime symbols.
- Load the `c23` skill before writing runtime headers or emitter templates.

## Rust conventions

- Use the stable toolchain and edition 2024. Format with the repo's
  `rustfmt.toml`.
- User errors are diagnostics, never panics. Don't call `unwrap()` in compiler
  code; use `expect("invariant: …")` only for true invariants.
- Compiler stages are pure functions of their inputs, so `wid check`,
  `wid query` and the LSP can rerun them freely.

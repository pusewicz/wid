# Orchestrating the Wid build-out

This page is for the main Claude session that drives Wid's implementation. It
plans and dispatches the work, checks it and lands it as stacked pull requests.
Subagents doing a single task need only `CLAUDE.md`.

## Mission

The user's standing instruction is to implement `SPEC.md` fully, without asking
questions unless genuinely blocked. Two rules govern the work:

- Prefer quality, simplicity and long-term maintainability over development
  cost.
- Get every lint error, test failure, flaky test or other problem you see
  fixed, even when the current task didn't cause it. If the current task
  doesn't fix it, file a GitHub issue and dispatch an agent for it (see
  "Issues").

Read these before doing anything:

1. `SPEC.md`, the language.
2. `CLAUDE.md`, the repo conventions and the diagnostics bar.
3. `docs/STATUS.md`. Its "Next" section is the work queue, its "Known gaps"
   section lists what is incomplete, and its conventions section records
   decisions already made.

Ask the user only for decisions that are genuinely theirs. That covers the
items under `SPEC.md` → Open (map literal syntax, error payloads, threads and
so on) and anything that would change the language's character. For everything
else, decide, write the decision into `SPEC.md` and record it in the PR
description.

## Your role: a thin orchestrator

Keep this session's context small, because the build-out is long.

- **Delegate.** Every implementation task goes to a subagent. Don't write
  compiler code in this session.
- **Check.** Run the gate yourself in the agent's worktree before you open or
  update a PR. Agents' "all green" reports have been accurate so far, but
  confirm them anyway.
- **Review the diff for the hard rules.** Look for:
  - `unsafe` outside `crates/wid_cimport`;
  - `unwrap()` in compiler code;
  - hidden allocations or hidden control flow in the language or the generated
    C;
  - new external commands;
  - spec changes made without a matching `SPEC.md` edit.
- **Keep `docs/STATUS.md` current.** Each PR moves its items to "Done" and
  updates "Next" and "Known gaps".

## Worktrees and stacked PRs

- **One task per branch, one branch per PR.** Each task runs in a subagent with
  `isolation: "worktree"`. Name branches `wid/<slug>`, for example
  `wid/linux-ci` or `wid/macros`.
- **Stack dependent work.** A task that needs an unmerged change starts from
  that change's branch, and its PR targets that branch with `--base`. Put
  `Stacked on #N.` as the first line of the PR body. Independent tasks start
  from `main` and can run in parallel.
- **Restack with `git rebase --update-refs`.** It moves every branch in a stack
  at once. When a parent PR is squash-merged, run
  `git rebase --onto origin/main <parent-branch> <child>` and
  `gh pr edit <child> --base main`, then push with `--force-with-lease`. Only
  force-push branches the session created. Never push to `main`.
- **Keep PRs small enough to review.** Make each PR one feature or one fix,
  with tests, docs and spec changes together. Split big features the way
  comptime was split into the interpreter and macros. Each PR must pass the
  gate on its own.
- **Commits** use `git commit -m "…"`, never a heredoc. End the message with
  the attribution lines the session's system reminder gives.
- **PR descriptions** are short:
  - start with the reason and any context that isn't obvious;
  - include no list of changed files;
  - name any spec decisions;
  - end with the attribution the session's system reminder gives.

## Parallel work

Worktrees make parallel crate edits possible, but these files see the most
conflicts:

- `wid_sema/src/check/{expr,members,items,structs}.rs`
- `wid_codegen_c/src/lib.rs`
- `runtime/wid_runtime.h`
- `docs/STATUS.md`

Run tasks in parallel only when the areas they touch don't overlap, for
example the LSP crate, the cimgui vendor package and `wid fmt`. Stack tasks
that touch the checker one after another. You resolve the conflicts in
`STATUS.md` when you restack.

## Issues

Problems found along the way become GitHub issues, as `CLAUDE.md` describes.
Open issues are part of the work queue, alongside `docs/STATUS.md` → "Next".

- **File what you find.** Problems you find yourself while checking or
  reviewing get an issue too, written to the same bar.
- **Dispatch from issues.** Give each issue, or a group of closely related
  ones, to its own agent on a `wid/<slug>` branch. Paste the issue body into
  the prompt, since the agent should not have to fetch context.
- **Close through the PR.** Put `Fixes #N` in the PR body for each issue it
  resolves.

## Writing subagent prompts

Subagents and forks don't see advisor output or your reasoning, so put
everything into the prompt:

- the scope, with acceptance criteria (which programs must build and which
  diagnostics must appear);
- the design decisions already made, and the ones the agent must make and
  write into `SPEC.md`;
- known hazards: hot files, ordering problems, platform differences;
- the gate: `cargo fmt --check`,
  `cargo clippy --all-targets -- -D warnings`,
  `cargo +1.88 check --workspace --all-targets --locked` (the MSRV CI
  checks), and `cargo test` with every C compiler available;
- for anything that touches diagnostics, a run of `ruby scripts/errdocs_drift.rb`;
- the turn limit. Agents stop after about 200 turns, so say: "If you run low on
  turns, stop on a green build and list exactly what remains." Then resume the
  agent with `SendMessage`.
- the report format, a short final report of what landed, the spec
  decisions, the gaps and the gate result.

Agents should commit in their worktree. You push and open the PR.

## Checks that catch drift

- **The test suite.** `crates/wid_driver/tests/suite.rs` runs `tests/run`
  and `tests/ui`, plus a check that every code used in `tests/ui` has a
  `docs/errors` page.
- **The error-page drift check.** `scripts/errdocs_drift.rb` checks that every
  `docs/errors/EXXXX.md` example still produces the output it shows, and that
  its fixed program passes `check`. Pages it can't reproduce carry
  `<!-- drift: skip REASON -->`. After an intended change, run
  `scripts/errdocs_update.rb`, review the diff, then rerun the drift check.
- **Probing.** `scripts/probe.rb` runs one-off programs through the compiler.
  Use it to look for bad diagnostics. A dedicated "bug hunt" agent that only
  probes and files GitHub issues found 46 real bugs last time, and is worth
  repeating after big features.

## Environment

Wid was developed on macOS (Apple Silicon) with Homebrew LLVM 23, Apple clang
21, gcc-16, and raylib 6 and SDL3 from Homebrew. CI (`.github/workflows/ci.yml`)
also builds and tests it on Ubuntu 26.04 with clang-22 and gcc-15.

It needs:

- stable Rust, 1.88 or later (edition 2024);
- clang ≥ 19 and gcc ≥ 15, because C23 `#embed` and `<stdckdint.h>` are
  required;
- libclang, set through `LIBCLANG_PATH` if discovery fails;
- pkg-config;
- Ruby, for the scripts.

raylib and SDL3 development packages are optional. The vendor tests skip when
pkg-config can't find them. Pick compilers with `WID_TEST_CC=clang,gcc-15`.

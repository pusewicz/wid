# Implementation status

The source of truth for the language is `SPEC.md`. This file tracks what the
compiler implements and what is next.

## Pipeline

`wid_syntax` (lex, parse) → `wid_sema` (resolve, check, lower to typed IR) →
`wid_codegen_c` (IR → C23) → host C compiler. `wid_driver` loads packages and
runs the stages; `wid_cli` is the `wid` binary.

## Conventions fixed so far

- Every Wid-ABI function takes `wid_Context *ctx` as its first C parameter.
  `@[c]` functions use the plain C ABI and get a default context.
- The C `main` builds the default context and calls the Wid `main`.
- Mangling: `<pkg>__<Name>` for package items, `<pkg>__<Type>__<method>`
  for methods, with a `__<n>` suffix for generic or inlined instances.
- Fixed arrays are wrapped in C structs so they copy and return by value.
- Sema lowers `defer`, block inlining (`yield`), `if`/`case` used as values,
  and short-circuiting into statements plus temporaries. Codegen just prints.
- Lowering state: a `Dest` (discard, assign to a local, return) says where a
  statement list's value goes; an exit stack (`Scope` with pending defers,
  `Loop`, `Function`, `Defer`) drives `return`/`break`/`next` and defer
  emission. Deferred blocks are lowered once and relabeled when copied to
  another exit. A returned value is spilled before defers run unless it is a
  constant.
- All loop control is `goto` to labels; codegen only prints labels that are
  targeted.
- Evaluation order is left to right: when a later operand or argument has
  side effects or needs statements, earlier impure ones are spilled first.
- Integer arithmetic wraps (`-fwrapv`); `-debug` builds trap on overflow via
  `ckd_*`. Division always checks for zero. `%` truncates toward zero, as in C
  and Odin.
- `E0001` is reserved and has no uses. Macros, which belong to a later phase,
  report their own code (E0902) with a workaround until they land.
- Copies of a deferred block share `LocalId`s with the original; each copy is
  emitted as its own C scope, so a local is declared once per copy.
- `private` is enforced for package members (`pkg.name`) and for methods
  (callable only when the frame's `self` type is the receiver type or the
  method's owner).
- Overload resolution works on lowered argument values
  (`call_with_values`), so compound assignments and `[]=` reuse it without
  re-evaluating operands. Module and generic-struct members of a set are
  instantiated with the receiver's bindings plus `Self`.
- Codegen marks parameters the body never reads `[[maybe_unused]]` in
  definitions (Wid allows unused parameters).
- `@[extern("sym")]` emits `extern R wid_extern_sym(params)
  __asm__(WID_SYMBOL("sym"));`: its own name bound to the C symbol, so it never
  conflicts with a header's declaration of `sym`. `wid_` symbols get no
  prototype (the runtime header declares them). A function an included header
  declares (a `cimport` package's, or an `@[extern]` whose symbol a `cimport`
  found, `CBinding::externs`) gets no prototype either; its calls carry an
  `ir::CCall` that casts pointer arguments to the header's C types and the
  result back.
- `cimport` runs in the driver (`crates/wid_driver/src/cimport/`): it checks
  the options, runs pkg-config and `wid_cimport::import`, renders the header as
  Wid source (`render.rs`, also `wid cimport --dump`) and adds it as a package
  whose `PackageInput::cimport` holds a `CBinding` (include line, flags, C
  spellings of every parameter, field and value, skipped declarations). Sema
  (`check/cimport.rs`) resolves `types:` in the importing file, computes
  `CConv`s (pointer casts, array views, `memcpy` for mapped records) and
  explains skipped names. Codegen (`cconv.rs`) applies them; `implement:`
  units are compiled separately by the driver, without strict warnings.
- A `cimport` without `as:` is merged by `Checker::merge_cimport` after
  collection: its package's scope entries are copied into the importing
  package's scope (`merged_cimports`), so lookups need no special case. A
  failed merged `cimport` (`failed_merges`) silences undefined-name errors in
  that package.
- The driver loads libclang lazily; the test suite loads it before starting
  workers (loading sets `LIBCLANG_PATH` briefly) and skips `cimport` and
  `vendor_` cases when it is missing.
- `crates/wid_driver/tests/vendor.rs` checks every `vendor/` package with all
  declarations, builds `examples/*` (build only: they open windows) and
  builds and runs `tests/vendor/*` against `.stdout`, with both compilers and
  strict flags; anything whose pkg-config library is missing is skipped.
  Vendored libraries with no system dependency (stb, miniaudio) have
  ordinary `tests/run/vendor_*` cases.
- Panics print `panic: message` and the location to stderr and exit with
  status 101; `-debug` builds `abort()` instead so debuggers stop there.
- Test builds (`wid test`) set `CheckOptions::testing`: `_test.wid` files are
  loaded, `main` is optional, sema records the `@[test]` methods and
  `core:testing.run_test` in `Program::tests`/`test_runner`, and codegen emits
  a `main` that runs one test by index (`--list` prints names). Only functions
  reachable from the tests are emitted. The driver runs each test in its own
  process and parses the `\nwid-test <kind> <file:line:col> <len>\n<message>`
  records `core:testing` writes to stderr, plus `panic:` reports.
- A root package inside the `core`/`vendor` collection keeps its collection
  path (`core:testing`), so `wid test core/x` behaves like an import.
- The loader skips `.c` files that start with the generated-C header, so a
  `-keep-c` output inside a package is not compiled back in.
- `numeric_array_elem` covers matrices too, so element-wise paths (scalar
  scaling, unary `-`, `+=`) are shared; `*` between shaped operands with a
  matrix is the product (`matrix_product`), lowered to a codegen helper.
- `^T?` parses as an optional pointer (`TypeKind::Optional(Pointer)`); the
  `?` binds after `^T` and `[^]T`. Types display a pointer to an optional as
  `^(T?)`.
- Layout (size, align, field offsets) is computed in sema for 64-bit targets.
- Compile-time code (`check/comptime.rs`) is lowered into a function of its
  own (a fresh `Body`, the outer frame's generic bindings, outer locals made
  uncapturable) and run by `crate::interp` over the IR. The interpreter keeps
  every value as bytes in its C layout in three regions (static, stack, heap;
  the region is in the address's top bits), so pointers, containers and
  allocators behave as in the generated program; builtins and the runtime's
  container, map and writer functions are ported from `wid_runtime.h`, as is
  float formatting. Before running, only the queued functions the code can
  reach are lowered (`lower_needed`), so a constant's evaluation never lowers
  code that reads the constant. The result crosses back through
  `interp::to_ir`: scalars and strings inline, other values as `ConstGlobal`s
  (read-only `static const` objects, `wid_const_N`), slices as a writable
  static array plus a `SliceOf` it. Codegen emits only the globals reachable
  functions use (`statics.rs`), in id order, and treats procs named in their
  initializers as reachable.
- Constants: `fold_const` folds untyped literal arithmetic; `eval_const`
  falls back to the interpreter for other shapes (`needs_interpreter`); a
  constant initializer without `comptime` sets `const_init`, which makes
  `call_fn` report E0327 with a fix that adds `comptime`. Parameter defaults
  only fold (or run an explicit `comptime`); others are lowered at the call.
- Declaration-level `comptime if`s are collected as `pending_ifs` and resolved
  after every unconditional declaration and `cimport` merge, in source order;
  a chosen branch's items go through the same collection. The loader resolves
  `import`s and `cimport`s inside every branch, keyed by the item's start
  offset (`FileInput::imports`), and holds back their errors in
  `FileInput::deferred` until sema chooses the branch.
- `OS` and `ARCH` come from a prelude file the loader generates
  (`core:builtin/target.wid`) for `-target:`; `Os`, `Arch`, `FieldInfo` and
  `MethodInfo` are declared in `core/builtin/comptime.wid`. `Type` values are
  `TyId`s held in 8 bytes (`TyKind::Type`).
- After lowering, functions that use `Type` values or reflection builtins,
  or call such functions, are marked `comptime_only` and never emitted;
  `check_comptime_only` reports (E0906) where code reachable from `main`,
  tests and exported functions calls one.
- `type_info(x)` lowers to `Builtin::TypeInfo` whose one argument is a `Zero`
  of the described type, never evaluated (`check/type_info.rs` checks `x` in
  a block it then drops). `wid_sema::type_info::describe` says what a table
  holds, and `type_info::check` rejects types whose tables would reach a
  `Type` (E0906) or `Never` (E0323). Codegen (`type_info.rs`) emits one
  `static const` object, `wid_typeinfo`, with arrays `types`, `fields`,
  `members` and `variants`, after the statics; `type_info(T)` is
  `((builtin__TypeInfo *)&wid_typeinfo.types[i])`. Entries are numbered in
  the order emitted functions describe types, then the types they point at.
  Keeping everything in one object lets entries point at each other without
  forward declarations (a `static const` object can't be declared before its
  definition without a tentative definition), and every address is cast to
  the non-`const` Wid type. Sizes, alignments and offsets are `sizeof`,
  `alignof` and `offsetof`. The interpreter builds the same tables in its
  static region from `describe`, once per type per evaluation
  (`Interp::type_infos`), with Wid's layouts.
- In an `enum` body, `struct`, `enum` or `union` followed by a newline, `=` or
  `,` is a member (`is_keyword_member` in the parser), for `TypeKind`. Vim
  matches them as `widEnumMember`, which opens no block.

## Done

- Diagnostics: spans, source map, codes registry, human and JSON renderers,
  `did_you_mean`.
- Lexer and parser for the full grammar in `SPEC.md`, including error
  recovery and the missing-`end` heuristic.
- Sema, codegen, runtime, driver and CLI (`build`, `run`, `check`,
  `explain`, `version`) for: functions with named and default arguments,
  locals, primitive types, constants, arithmetic, comparisons, `if`/`unless`,
  `while`/`until`, `loop`, range `for`, `break`/`next`, `defer`, `puts`,
  `print`, `p`, `panic`, `assert`.
- Structs (fields, defaults, `T.new`, `@field`, `self`, methods, `def self.`,
  implicit-self calls, operator methods, memberwise `==`, printing), recursive
  struct detection, enums (backing types, explicit values, symbols, methods,
  printing, `to_i`, `to(T)`), `case`/`when` with values, ranges and enum
  exhaustiveness.
- Optionals (`T?`, `nil`, wrapping, `||` defaults, `&.`, `nil?`, `if v = …`),
  flow-sensitive narrowing, `guard` (optionals, `(…, Error)` results, bare
  `Error`, `Bool`), multiple return values, destructuring and swapping
  assignment, `_` discards, the builtin `Error` set, ignored-error checks.
- Tagged unions (wrapping, nil state, `case` on variant types with narrowing
  and exhaustiveness, printing).
- Containers: fixed arrays (literals, indexing, slicing, element-wise math,
  swizzles), slices, dynamic arrays (`<<`, `push`, `pop`, `insert`,
  `delete_at`, `reserve`, `clear`), maps (`m[k]`, `m[k] = v`, `m[k] += 1`,
  `has_key?`, `delete`), iteration (`for x`, `for &x`, `for x, i`,
  `for k, v in m`, runes in strings), bounds checks and
  `@[no_bounds_check]`.
- Strings: interpolation, `to_s`, `inspect`, byte indexing and slicing,
  comparisons, `include?`, `index`, `starts_with?`, `ends_with?`, `to_cstr`.
- Memory: `alloc(T)`, `alloc([]T, n)`, `free`, `free_all`, `size_of`,
  `align_of`, `T.size`; `context` as a real struct, with assignments to its
  fields scoped to the block (`Stmt::WithContext`).
- Blocks: `&blk: block(T) -> R` parameters, `yield`, inlining at each call
  (receiver and arguments evaluated once, `return`/`break`/`next` with values,
  defers across the boundary, by-reference `|&x|`, recursion detection), and
  checking of block methods that are never called.
- Procs: `->(x: T) -> R { … }`, `method(:name)`, calls through locals, fields
  and `.call`, capture detection.
- `**` (integers and floats) and `<=>`.
- Generics: `$T` functions with inference (slice-aware), generic structs with
  type and value parameters, methods of generic structs, template semantics
  (instances keyed by declaration and arguments).
- `extend` (program-wide, with `Self`, `$T` patterns, ambiguity errors,
  slice fallback for arrays) and the `core:builtin` prelude.
- `overload` sets (package level and in types, including operators and
  generic structs; most-exact-match resolution with literal conversion;
  declaration checks), package-level operator functions with literal operand
  typing, unary `def -`/`def ~`, `[]`/`[]=` methods, and `+=`-style forms
  through operator methods.
- `using` fields (by value or pointer; field and method promotion, also
  through `@field`, implicit-self calls and nested `using`; ambiguity errors),
  `module`/`include` (in structs, enums, modules and `extend`; `Self`-generic
  module methods, transitive includes, static module methods), and `private`
  methods.
- `@[export("name")]`, `@[c]` and `@[extern("name")]`, plus `.c` and `.cpp`
  files in a package, each compiled separately (C++ as C++20 with the
  matching C++ compiler) and linked with it; failures name the file or the
  link step. Attributes are validated once per declaration;
  `@[no_bounds_check]` works on methods (lexically) and statements.
- Named `distinct` types (`Meters = distinct F64`).
- Diagnostics pass over the errdocs bug log: constant cycles, generic argument
  kinds, blocks passed to built-ins, private package types, quoted symbols
  (E0110), recovery from unclosed strings and interpolations, deferred
  resolution of types named behind pointers (no false E0312), and fewer
  cascades (failed imports, multi-assignments, overload duplicates,
  unreachable tails, block-method cycles reported once).
- `&&=`/`||=` (locals, fields, optionals, map entries), `while v = maybe`,
  `-> Never`, `caller_location`, `Location`/`AllocMode` builtins and typed
  `Allocator`/`Logger` procs (allocators written in Wid), pointer and byte-view
  conversions with `.to(T)`, implicit pointer → `RawPtr`, `[^]T` arithmetic,
  `.concat`/`.resize` on dynamic arrays, generic unions, generic methods as
  procs (instantiated from the expected proc type), and `matrix[R, C]T`.
- Core library: `core:mem` (heap/temp, `Arena`, `Pool`, `Tracker`, `copy`,
  `set`, `zero`, `compare`), `core:fmt` (`int`, `uint`, `float`, `write_*`
  into `strings.Builder`, `bprint`, `aprint`), `core:strings` (`Builder` and
  `String`/`[]String` extensions: `strip`, `split`, `each_line`, `join`,
  `replace`, `upcase`, `ljust`, `parse_int`, …), `core:os` (args, env, files,
  stdin lines, `exit`), `core:math` (constants, libm overloads for `F32`/`F64`,
  `lerp`/`remap`/`smoothstep`, `Vec2`/`Vec3` extensions, `Rect`), `core:c`
  (C type names, limits) and `core:testing`. `-debug` builds track the heap
  and report leaks at exit.
- `wid test` (`-filter:`, `-json-errors`), with per-test processes, leak
  checks and failures as E0802 diagnostics.
- `cimport` through libclang (`crates/wid_cimport`): functions, structs and
  unions (with C layouts, opaque structs, snake_case fields), enums,
  typedefs, constant and value macros, function-alias macros, const globals,
  callbacks as `@[c] proc` types, variadic functions, `as:`, `strip_prefix:`,
  `rename:`, `names:`, `types:` (checked layouts, `memcpy` at the boundary),
  `define:`, `implement:` (own translation unit), `link:`, `pkg_config:`,
  `include_dirs:`, `wid cimport --dump`, and errors E0701 and E0703–E0708.
  raylib 6, SDL3 and parts of libc import and type-check completely
  (`crates/wid_driver/tests/cimport_libraries.rs`).
- `cimport` without `as:` merges into the package namespace (binding
  packages); `vendor:raylib` and `vendor:sdl3` (pkg-config), vendored
  `vendor:stb/{image,image_write,truetype,rect_pack}` and `vendor:miniaudio`.
  The SPEC's Taste sample builds verbatim as `examples/taste`.
- `.to([^]T)` on arrays, slices and dynamic arrays; any pointer converts to
  `RawPtr?`; `Context` and the other predeclared library types can be
  redeclared by a package, while primitive type names are reserved.
- Compile time: `comptime` expressions and `comptime do … end`, constants
  computed by the interpreter (struct literals, `T.size`, indexing, and method
  calls under `comptime`), `comptime if` in declarations (including guarded
  `import`/`cimport`) and in method bodies (including `T == F32` in generic
  methods), `OS`/`ARCH` and `-target:` (`wid check` for any target; builds for
  the host only, E0709), `config(:name, default)` reading `-define:`,
  reflection (`T.fields`, `T.methods`, `T.name`, `Type` values with `.name`,
  `.size`, `.align`, `.fields` and `==`), `embed("file")` through `#embed`, and
  errors E0901 and E0903–E0909.
- `type_info(T)` and `type_info(x)`: the prelude's `TypeKind`, `TypeInfo`,
  `TypeInfoField` and `TypeInfoMember` (`core/builtin/type_info.wid`), static
  tables for every kind of type (including generic instances, `distinct`
  types, `Error`, cimported and opaque C structs, and recursive types), the
  same tables in `comptime` code, E0906 for `Type` and records that hold one,
  E0323 for `Never`, and E0906 help that points at `type_info(T)`.
- Macro syntax: `quote` bodies holding statements and declarations, with
  splices in every expression, type, declaration and name position (`#{x}`,
  `@#{f}`, `:#{s}`), splices outside a `quote` (E0111), variadic
  `*names: T` macro parameters (E0112), and package-qualified
  declaration-level macro calls. Expansion is next (item 1).
- Test suite: `tests/run` (clang and gcc-16, strict flags), `tests/ui`,
  `tests/test` (`wid test` reports) and every `core/` package's `_test.wid`
  files.
- Linux and CI (`.github/workflows/ci.yml`, cached with sccache and
  rust-cache): `cargo fmt --check`; clippy and the full `cargo test` on
  Ubuntu 26.04 (clang-22, gcc-15, libclang 22, SDL3) and macOS 26 (Apple
  clang, gcc-15, SDL3, raylib), failing when libclang or a vendor library is
  missing instead of skipping; clippy and unit tests on Windows; `cargo check`
  at the MSRV (1.88). Ubuntu doesn't package raylib, so `vendor:raylib` and
  `examples/taste` are covered on macOS only. `cimport` keeps doc comments
  from system headers, which is where Linux installs libraries.

## Next

Everything before macros is done (see "Done"). This is the work queue for
the orchestrator (`docs/ORCHESTRATOR.md`). Each item is one PR unless it says
otherwise. Items 2–5 depend only on `main` and can run in parallel with the
macro stack.

1. **Macros and `type_info`** (`wid/macros-*`, about three stacked PRs):
   - lexer, parser and AST for `quote` and splices (**landed**);
   - expansion, hygiene and the `attr_*` macros;
   - `type_info` (landed separately; see "Done").

   The design below is settled and written into SPEC.md ("Compile-time").
   Only the syntax has landed: today a `quote` and a declaration-level
   macro call (`attr_reader :hp`, `lib.attr_reader :hp`) in a type body
   are E0902 ("macros cannot run yet", `tests/ui/not_yet_available`), a
   package-level macro call is E0108, and `macro def` bodies are parsed but
   never checked (`check/mod.rs` skips `is_macro`).
   - **Syntax (landed):**
     - Lexer: `#{` outside a string emits `SpliceBegin`, its `}` emits
       `SpliceEnd` (an `Interp` with `quote: None`; newlines inside are
       suppressed), and `@#{`/`:#{` emit `AtSplice`/`ColonSplice` first. A
       splice with no `}` anywhere ends at a zero-width `SpliceEnd` after the
       last code on its line, which the parser reports (E0102 inside a
       `quote`; outside one only E0111).
     - `ExprKind::Quote(Box<QuoteExpr { body: Vec<Stmt>, splices: Vec<Expr> }>)`
       numbers its splices in source order, one list per `quote` (a `quote`
       inside a splice has its own). A body line that can only start a
       declaration (`def`, `struct`, `include`, `NAME = v`, … with
       attributes and `private`) is a `StmtKind::Item`, also in the branches
       of a top-level `comptime if`; every other line is a statement. So
       PR 2 uses the body as is in a method, and in a declaration context
       takes each `StmtKind::Item`'s item and turns a call or name statement
       (`attr_reader :hp`) into an `ItemKind::MacroCall`.
     - A splice is `ExprKind::Splice(i)` in an expression,
       `TypeKind::Splice(i)` in a type, and `ItemKind::Splice(i)` alone on a
       line in a type body (in an enum body it may be `Symbol`s, i.e.
       members). In every name position (method, field, parameter, local,
       block-parameter, loop-variable, type and enum-member names, `x.#{m}`,
       `@#{f}`, `:#{s}`, named arguments, `#{name}(args)`) it is an
       `Ident`, `IVar` or `Symbol` named `#{i}` (`ast::splice_name`,
       `Ident::splice_index`), spanning the whole `#{…}`. A splice in a
       generic argument list is a `GenericArg::Expr`.
     - A splice outside a `quote` is E0111, with a fix that adds a space
       after `#` (when it reads as a comment, the rest of its line is
       skipped like that comment) or else one that removes `#{` and `}`; a
       splice inside a splice is E0111 too.
     - `*names: T` sets `Param::splat`. E0112 rejects it outside a
       `macro def` (recovering as `names: []T`, whose calls take any
       arguments without more errors), before another parameter, or with a
       default.
     - A package-qualified call (`lib.name args`, `lib.name(…)`, `lib.name`)
       parses as `ItemKind::MacroCall` at package level and in type bodies.
   - **Expansion** (new `check/macros.rs`):
     - Each argument is converted to a value: `Code` arguments become
       fragment handles over the call-site AST, `Symbol` arguments become
       `Name` indices (add `Name::index`/`Name::from_index`), `Type`
       arguments become `TyId`s, and other argument types run through
       `comptime_value`.
     - A wrapper function calls the macro's `FnId`. It goes through
       `lower_needed`, then the interpreter.
     - A new `Builtin::Quote { template }` records
       `Fragment { template, values: Vec<SpliceValue> }` in an interpreter
       table and returns its index as the `Code` value. Splice values are
       read from interpreter memory: `Code`, `[]Code`, `Symbol`, `Type`,
       and scalars.
     - The template's AST is cloned, its own binders are renamed for
       hygiene, then splices are substituted. This needs a mutable AST
       visitor in `wid_syntax`.
     - Generated AST must live for `'a`. Use a `typed-arena` arena created
       in `check_program` (`check/mod.rs:246`).
     - A `*names: T` parameter collects the remaining positional arguments,
       each converted by `T`'s rule, into a `[]T`.
     - Macro calls resolve like other calls, so `lib.name` reaches a
       package's macro and `private macro def` is enforced. Names in the
       quote's own code that aren't quote-bound locals or members of
       `self` resolve in the macro's package (its `DeclLoc`); splice names
       and `Self` resolve at the call site.
     - Errors: each expansion gets a virtual `FileId` that aliases the
       template's file and records the call span and the macro's name;
       nested expansions form a chain, and a diagnostic in generated code
       lists the chain innermost first, in the human and JSON renderers.
       `#line` in `-debug` builds points at the template's real file.
     - Budgets: each macro run has the `comptime` limits; expansions nest
       at most 64 deep and a build runs at most 65,536 (E0903).
   - **Where expansions are checked:**
     - Statement-level expansions lower in place with `lower_stmts`.
     - Expression-level expansions lower like `comptime` statements, into a
       result local.
     - Declaration-level calls are queued with the pending `comptime if`
       items and expanded in source order. The generated items go through
       `collect_item`.
   - **New comptime-only types:** `Code` and first-class `Symbol` values,
     plus `sym.to_s` and `str.to_sym`. Extend `check_comptime_only` (E0906)
     to cover them.
   - **`core:builtin`:** `attr_reader`, `attr_writer` and `attr_accessor`
     macros. They look up the field type through `Self.fields`.
   - **`type_info(x)` / `type_info(T)`:** landed separately, ahead of the
     macros (see "Done" and SPEC "Compile-time").
   - **Settled (in SPEC.md, "Compile-time"):** errors in generated code
     point at the `quote` line and every macro call that led to it,
     innermost first; variadic `*names: T` parameters (macros only, last,
     no default); symbol splices `:#{name}`; qualified calls `pkg.name`
     and `private macro def`; definition-site resolution of the quote's own
     names; budgets (64 deep, 65,536 per build, E0903). The implementation
     notes are under "Expansion" above.
2. **`wid doc`** (`wid/doc`). Documentation for packages, types and
   methods, generated from doc comments, including cimported C symbols
   (SPEC "C and C++ interop", "Toolchain and CLI"). It prints text by
   default and JSON with `-json`, and resolves `wid doc rl.draw_circle_v`
   style queries.
3. **`wid query`** (`wid/query`). The introspection engine in SPEC "Built
   for humans and LLMs": symbols, types, definitions, references and call
   sites, as stable JSON. Factor it as a reusable engine, because the LSP
   shares it, and keep the compiler stages pure so queries can rerun them.
4. **`wid fmt`** (`wid/fmt`). A canonical formatter. It must be
   idempotent, and parse → format → parse must give the same AST for every
   file in `tests/`, `core/`, `vendor/` and `examples/`. It keeps comments
   and supports `-check`.
5. **`wid lsp`** (`crates/wid_lsp`, `wid/lsp`, stacked on 3 and 4).
   Diagnostics, hover, go-to-definition, completion, formatting and rename,
   all on top of the query engine.
6. `vendor:cimgui`: vendor cimgui with the Dear ImGui sources, compiled as
   C++ package files (the driver already builds `.cpp` files and links with
   the C++ compiler), plus a raylib or SDL3 backend. Needs a decision on
   shipping the C++ sources versus requiring a system cimgui.
7. **Cross-target builds** (`wid/targets`). Lift E0709. Make
   `-target:os_arch` build through clang `--target` with a sysroot. Port
   `core:os`/`core:c` to Windows (LLP64) and make C type sizes
   target-driven.
8. **SPEC conformance audit** (one agent, a report and no code). List every
   SPEC.md claim that is unimplemented or behaves differently: CLI flags such
   as `-vet`, `-sanitize:address`, the `-o:` levels and `-collection:`;
   `#line` in `-debug`; the prelude list; and so on. Queue each item here.
9. **Known gaps** below: one small PR each, in any order.
10. **Bug hunt after every large feature.** One agent probes with
    `scripts/probe.rb` and `scripts/errdocs_drift.rb` and logs bad
    diagnostics and crashes. Another agent fixes them. The first hunt found
    46 real bugs.

Items under SPEC.md → Open (map literal syntax, error payloads, threads,
hot reload, a package manager, `#soa`, a REPL) need the user's decisions
before anyone starts them.

## Known gaps

- An untyped array literal does not take its type from the other operand of
  a binary operator or from an overload set's parameters (`xf * [1.0, 0.0]`
  needs a typed `Vec2`); `[1.0, 1.0] * m` likewise needs a typed vector.
- `core:c` sizes assume 64-bit Unix (LP64); `core:os` uses POSIX `stat` and
  `mkdir`. Windows support comes with `-target:`; until then Windows CI runs
  only clippy and the unit tests.
- The tracking allocator and the temp arena are not thread-safe.
- `cimport`: `types:` mappings don't apply inside callback signatures (they
  keep the C struct); taking `method(:f)` of an imported function whose
  signature needs `memcpy` conversions casts the C function pointer instead of
  wrapping it. Anonymous struct and union types of named fields, `long double`,
  `_BitInt`, vectors and `va_list` by value are skipped. Plain `//` comments
  above declarations are not kept as docs.
- Two `cimport`s whose headers declare the same C struct give two distinct
  Wid types (one per cimport package), so values don't pass between them;
  import related headers (raylib.h, raymath.h, rlgl.h) through one header or
  one package.
- Compile time: `T.methods` works on types written in the source only (a
  `Type` value has no `.methods`), and lists methods that take no `$T`
  parameters. A struct's fields can't be inside `comptime if`. Writes through
  a pointer to a comptime-computed constant aren't prevented (constants are
  copied when their address is taken, but a slice into a constant's static
  data is writable). The interpreter runs about 10 million steps a second, so
  very large tables are slow to build.
- `type_info`: `T?` and `proc(…)` types can't be written as arguments
  (`type_info(Int?)`, like `size_of(Int?)`, doesn't parse); name them with a
  constant first (`MaybeInt = Int?`). At compile time the tables use Wid's
  layouts, which differ from C's for a cimported C union (whose fields all
  start at 0 in C), and `Error`'s members are the error symbols seen so far.
  Proc tables don't say whether a proc is `@[c]`. Nothing stops a program
  from writing through a `^TypeInfo` (the run-time tables are `const`, so it
  faults; at compile time it succeeds).
- `vendor:miniaudio` built with GCC on macOS has no CoreAudio backend: GCC
  can't parse the block syntax in Apple's headers (`miniaudio.c` sets
  `MA_NO_COREAUDIO` there).

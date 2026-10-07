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
- Test suite: `tests/run` (clang and gcc-16, strict flags), `tests/ui`,
  `tests/test` (`wid test` reports) and every `core/` package's `_test.wid`
  files.

## Next

1. Grow features in this order: variables and control flow → structs and
   methods → enums, unions, optionals and flow typing → multi-return, `guard`
   and `Error` → arrays, slices, dynamic arrays, maps, strings, context,
   allocators and `defer` → blocks, `yield` and procs → generics, `overload`
   and operators → modules, `using`, `extend`, packages and imports → core
   library and `wid test` → `cimport` → `vendor:` packages and the Taste
   sample → comptime (all done) → macros and `type_info` → `doc`, `query`,
   `fmt` and `lsp`.
2. `vendor:cimgui`: vendor cimgui with the Dear ImGui sources, compiled as
   C++ package files (the driver already builds `.cpp` files and links with
   the C++ compiler), plus a raylib or SDL3 backend. Needs a decision on
   shipping the C++ sources versus requiring a system cimgui.

## Known gaps

- An untyped array literal does not take its type from the other operand of
  a binary operator or from an overload set's parameters (`xf * [1.0, 0.0]`
  needs a typed `Vec2`); `[1.0, 1.0] * m` likewise needs a typed vector.
- `core:c` sizes assume 64-bit Unix (LP64); `core:os` uses POSIX `stat` and
  `mkdir`. Windows support comes with `-target:`.
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
- `vendor:miniaudio` built with GCC on macOS has no CoreAudio backend: GCC
  can't parse the block syntax in Apple's headers (`miniaudio.c` sets
  `MA_NO_COREAUDIO` there).

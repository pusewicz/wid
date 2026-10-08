# Implementation status

The source of truth for the language is `SPEC.md`. This file tracks what the
compiler implements and what is next.

## Pipeline

`wid_syntax` (lex, parse) → `wid_sema` (resolve, check, lower to typed IR) →
`wid_codegen_c` (IR → C23) → host C compiler. `wid_driver` loads packages and
runs the stages; `wid_cli` is the `wid` binary. `wid_query` answers questions
about a checked package (`wid query`, and the LSP later) without generating
code; `wid_driver::analyze` loads and checks for it and for `wid doc`.

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
- `E0001` and `E0902` are reserved and have no uses: their pages say so and
  start with `<!-- drift: skip … -->`. Any feature the compiler doesn't
  implement yet gets its own code with a workaround.
- Copies of a deferred block share `LocalId`s with the original; each copy is
  emitted as its own C scope, so a local is declared once per copy.
- `private` is enforced for package members (`pkg.name`) and for methods
  (callable only when the frame's `self` type is the receiver type or the
  method's owner). Fields are always public: `private` on a field in a
  struct body (a `using` one, or one inside a `quote`'s struct, too) is
  E0105 from the parser (`Parser::private_field`), with a fix that removes
  it; the field is kept, and the item's `private` flag cleared. The parser
  reports `private` the same way wherever else it hides nothing: before an
  `import`, `cimport`, `include`, `extend`, `comptime if` or a splice
  standing alone (`nameless_item`), an enum member, or a statement
  (`Parser::private_statement`); what follows is parsed as if it weren't
  there.
- Overload resolution works on lowered argument values
  (`call_with_values`), so compound assignments and `[]=` reuse it without
  re-evaluating operands. Module and generic-struct members of a set are
  instantiated with the receiver's bindings plus `Self`.
- Codegen marks parameters and locals the body never reads
  `[[maybe_unused]]` (Wid allows unused parameters and `_` names). An
  assignment to a field or an element of a local stored in place
  (`ir::Expr::written_local`) reads only its indexes, as in sema, since gcc
  reports such a local as "set but not used".
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
  Sizes saturate instead of overflowing, and no type is over
  `types::MAX_TYPE_SIZE` (`2^61 - 1` bytes, E0329) once checking succeeds,
  so codegen never sees a saturated size.
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
- Declaration-level `comptime if`s and macro calls are queued in
  `pending_decls` (`comptime::Pending`) and resolved by `resolve_pending`
  after every unconditional declaration and `cimport` merge, in source order,
  in rounds: what a round collects may queue more for the next. A chosen
  branch's items go through the same collection. The loader resolves
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
  a block and scope of its own, `lower_operand`, then drops the block). `wid_sema::type_info::describe` says what a table
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
- Macro expansion (`check/macros.rs`, whose module docs describe the core):
  - A call resolves like any call; `call`, `ident` and `package_member`
    hand a macro to `Checker::call_macro` with the expected type, and
    `call_fn` does for any other path. A package-level `overload` set with
    macro members goes through `call_package_set` (from `call` and
    `package_member`), whose `choose_with_macros` picks the member before
    any argument is lowered (`Fit::Code` for a macro's `Code` parameter,
    scored below typed fits; a symbol literal for `Symbol`, `named_type`
    for `Type`; otherwise the argument's type, from a `Probe` lowered once
    in a dropped block when a member needs it). A macro chosen goes to
    `call_macro`, shown by its own name (`chosen_macro_call`); a `def` gets
    its arguments lowered and `call_member`. Among declarations,
    `resolve_item_macro` accepts such a set and `chosen_macro` picks the
    member inside the expansion frame. `expand` converts the arguments
    (`Code` → an *argument fragment*: the call-site syntax, numbered from
    one; `Symbol` → `Name::index`; `Type` → `TyId`; others →
    `comptime_value`; a `*names: T` → a static `[]T`, `fn_sig` typing the
    splat as `[]T` for macros), runs a wrapper that calls the macro through
    `lower_needed` and `interpret` (shared with `comptime`), builds the
    returned `Code` value into statements, and allocates them in
    `Checker::generated` (a `typed_arena::Arena<Vec<ast::Stmt>>` created in
    `check_program`, so generated syntax lives for `'a`).
    `lower_generated` lowers them in place: all but the last with
    `lower_stmts`, the last as the value (with the expected type), then
    re-emits the caller's `Line`. A call that is a statement of its own
    (the statement being lowered, `MacroState::line`, or the whole last
    line of the code of such a call, `MacroState::discarded`) lowers a
    last `if`, `case` or `comptime if` as a statement. Where the value is
    used, `MacroState::value_tails` holds the spans that end that line's
    branches, and `call_value_note` (run by `splice_context`) adds a note
    to an E0323 or E0301 there saying the line gives the call's value.
  - `Code` values are numbers: 0 is no code, `1..=n` the argument
    fragments, then the fragments the macro's `quote`s recorded, in order.
    `Builtin::Quote { template }` (lowered by `lower_quote`; `template`
    indexes `MacroState::templates`, keyed by the quote's span) evaluates
    its splices and records an `interp::Fragment` of `SpliceValue`s read
    from interpreter memory (`interp/quote.rs`). `Symbol` values are
    `Name::index`es (`TyKind::Symbol`, 8 bytes); `str.to_sym` is
    `Builtin::ToSymbol`, `sym.to_s` is ordinary printing.
  - `Expander` builds a fragment: it clones the template, moves its spans
    into the expansion's virtual file (`Respan`), and `Splicer` (a
    `wid_syntax::visit::VisitMut`) replaces each splice by its value:
    sequences in `visit_stmts`, `visit_items`, `visit_args` and
    `visit_exprs`; names in `visit_ident` and the `IVar`/`Symbol`/`Ident`
    placeholders; types in `visit_type` (a `Type` becomes
    `ast::TypeKind::Spliced(TyId)`, resolved by `resolve_type`, also inside
    `ExprKind::Type` where a value goes). A nested `quote` is left alone. A
    name from a `Symbol` gets the span of the matching symbol argument
    (past its colon), or of the call. In an enum's body, a splice alone on
    a line of `Symbol`s or of code gives members, inserted after the
    written members whose spans come before it (`take_member_splices`
    counts them before the walk splices their names, and
    `insert_member_splices` inserts after it): a code line that is a name
    alone or `name = value` is a member (`enum_member_line`), and the
    others go through `lines_to_items` (`enum_lines`). Splices that don't fit are
    E0911 at the call, and the code is then not lowered. A name from a
    `Symbol` is checked for its place (`NamePlace`: `item_names`,
    `expr_names` and `visit_stmt` resolve the names a declaration,
    expression or statement declares or uses before the walk, and other
    placeholders count as identifiers) against
    `wid_syntax::lexer::name_shape`, the lexer's reading of the text;
    `check_name` reports each bad name once per expansion
    (`Expander::bad_names`), with a fix at the call for a symbol
    argument. A spliced assignment target's name is checked
    (`NamePlace::Target`) before `splice_target`'s rule.
  - Virtual files: `FileId::expansion(i)` (ids from `1 << 31`) is
    `MacroState::files[i]`, one per (expansion, template file); it records
    the template file, the expansion (call span, name as called, depth)
    and the `DeclLoc` its own names resolve in. Their text is registered
    in `source_texts`/`file_positions`. `ir::Program::expansions` carries
    them to the driver, which calls `SourceMap::set_expansions`;
    `SourceMap::file` resolves an expansion id to the template's real file,
    so `#line`, panic locations and every renderer work unchanged, and
    `expansion_chain` gives the calls behind a span, which both renderers
    print (`expansions` in JSON) for `Diagnostic::chain_span`: the first
    label marked `Label::splice` (`Diagnostic::splice`), else the primary
    span.
  - Spliced code keeps its spans, so its errors would show no expansion.
    `Expander` records a `Splice` (code span, the splice's virtual span,
    the name for a name, and whether the name was computed and so has the
    call's span) for each `Code` argument it inserts (`code(value, at)`)
    and each name (`name_span`, `literal_span`), committed to
    `MacroState::splices` (by file) when the build succeeds.
    `Checker::report` runs `splice_context`: for a primary span inside
    spliced code (equal to it, for a computed name), the innermost splice
    gets a splice label (`` `label` is spliced here by `wrap` ``), so the
    chain is listed. Of code spliced more than once and of names computed
    for one call (which share the call's span), it picks the name the
    message mentions, else the splice in the statement (`MacroState::line`)
    or method being checked, else the latest when they agree; otherwise
    nothing is added. For a computed name the primary label at the call
    names it (`` `label`, spliced by `wrap`, has type … ``) and edits at
    the call are dropped (#30).
  - Sites and hygiene: `Frame::site` is the virtual file of the code being
    lowered, set by `expr` and `lower_stmt` from each node's span
    (`enter_site`/`leave_site`). `loc()` is the site's `DeclLoc`, else the
    frame's; `loc_at(span)` does the same for one name's span (used by
    `call`, `ident`, `classify_receiver`, `check_visible`), and
    `resolve_path_type` and array lengths use a virtual span's `DeclLoc`.
    Each `Var` has a `mark` (the expansion of the span that declared it);
    `find_var` matches the site's mark and `find_var_at(name, span)` the
    span's, so the quote's own locals and the caller's never see each
    other, while spliced code and names (call-site spans) do. Lambdas
    inherit the site; like block parameters, their parameters are declared
    at their names' spans, so a spliced name gets the caller's mark.
    Parameters of a generated method (`declare_param`,
    and inlined block methods) are `open`: also visible to code with the
    mark of the expansion's call site.
  - Operands that don't run once with their statement (`macros::Operand`:
    the right side of `&&`/`||` in `logical` and `or_default`, the value of
    `||=`/`&&=` in `lower_logical_assign` and `map_logical_assign`, the
    call of `&.` with its arguments in `safe_member`, every `when` pattern
    but a `case`'s first in `lower_when_chain` (a pattern that needs
    statements is tested only when the earlier ones of its `when` didn't
    match), `type_info`'s operand, and `while`/`until` conditions) are
    lowered by `lower_operand`, in a block and a `Scope` of their own whose
    `operand` is set; the caller places the statements right before the
    value's use (a value read after a context-shadowing block is first
    stored in a temporary declared before it, and a call without a value
    runs inside it). Popping the scope moves its variables into the
    enclosing scope's `scoped_out`, so `ident` reports a later use with
    `report_scoped_out` (E0201 naming the macro and the operand, hygiene
    marks respected). `lower_defer` asks `defer_in_operand`, which reports
    a `defer` whose scope is an operand's (E0406, at the call that
    generated it) and keeps it out of the exit stack; its body is still
    checked. For `while v = …` the operand scope wraps `lower_if_bind`, so
    the body sees the names.
  - Budgets: depth comes from the call span's virtual file (parent depth +
    1, at most 64); at most 65,536 expansions per build; each E0903 is
    reported once (per macro for depth). A macro whose reachable functions
    had errors (`MacroState::failed`, filled by `lower_pending`) doesn't
    run. `check_macro` validates a `macro def` once (`-> Code`, no `$T`, no
    block); `check_all_roots` checks root-package macro bodies like other
    methods. `MacroState::in_macro` (set by `lower_function`) allows
    `quote`.
  - `check_comptime_only` covers `Code` and `Symbol` (`Only`), pointing at
    the first line that uses one; codegen maps both to `wid_TypeId` but
    never emits them.
  - `Self` in a macro's own code (`macro_self_use`, a `FindSelf` walk that
    skips `quote` bodies but not their splices, cached in
    `MacroState::self_uses`): `macro_instance` runs such a macro as the
    instance `fn_instance_with(decl, [Self = T])` for the frame's
    `self_ty`, and reports E0209 when there is none or it has placeholders
    (`Param(Self)` in a module or `extend`, a generic struct's template).
    `check_all_roots` doesn't check such a macro's body on its own; each
    instance is checked when it runs, with the call on `instance_stack`
    (whose fourth element, the `Self` shown, words the note for a macro).
  - Declaration-level calls (`check/decl_macros.rs`, whose module docs
    describe the flow): `collect_item` queues a `PendingMacro` (rejecting
    attributes, E0328); `expand_item_macro` resolves the name where it is
    written (`names_at`: a virtual span's `DeclLoc`, else the declaring
    file), pushes a frame of its own (its `self_ty` the owner's type, made
    only when the macro or an argument uses `Self`, so a struct's fields
    aren't resolved early otherwise), runs `expand_code`, turns the
    statements into items with `lines_to_items` (shared with
    `Splicer::push_items`), allocates them in `generated` and collects
    them with `collect_item` under the call's `DeclLoc` and owner. Fields
    for a struct owner are E0913 there; a generated `import`/`cimport` is
    E0913 wherever it lands (`generated_import`, called from
    `collect_items` and `collect_item`, since the loader keyed nothing by
    a virtual offset), and marks its name failed in the macro's file so
    uses don't cascade. `private` on the call is copied to every item.
  - Generated members join their owner like written ones: `members` for
    lookup, `include_items` (every `include` collected, in order; resolved
    lazily by `includes_of`, and one collected after that is resolved on
    the spot by `resolve_include`, by its own span's `DeclLoc`), and
    `check_member_after_fields` reports a member named like a field when
    the struct's fields were already resolved (a macro reading
    `Self.fields` resolves them first). `fold_const` and `undefined` use a
    virtual span's `DeclLoc`, so a generated constant folds the macro
    package's constants and failed imports are found where the name
    resolves.
  - Failed expansions don't cascade. A call in a method body that fails
    (`call_macro` gets no code) runs `failed_expansion`: the variables
    visible at the call are marked read, and the innermost scope gets
    `Scope::failed_macro`, so `declared_by_failed_macro` skips E0201 for
    a local (`ident`, `place`) while that scope is open. A call among
    declarations that fails or names no macro runs
    `failed_among_declarations`: at package level the package joins
    `MacroState::failed_packages`, which `pkg_incomplete` treats like a
    failed `cimport` merge (undefined names and types). While it isn't
    empty, `members_incomplete` is true for every type, whichever package
    looks the member up: the call may have generated an `extend` of any
    type, and extensions apply program-wide (#29). In a body the owner
    joins `failed_owners`, so `members_incomplete` skips E0204
    on its type (also through an included module or an `extend`) and
    `declared_by_failed_macro` skips E0201 for implicit-self calls and
    constants in its methods. An undefined macro among declarations is
    still reported after an earlier failure. A field that E0913 rejects
    joins `MacroState::rejected_fields` instead: the call's other
    declarations are known, so only that name is skipped (`field_rejected`,
    in `no_member` for `x.f` and `@f`, and in `struct_new`). An unknown
    call in a method (`undefined_call`) runs `failed_expansion` when
    `did_you_mean` picks a macro (the E0201 then offers the macro and labels
    its declaration) or when, with no close name, it is the whole statement
    (`MacroState::line`) and has a symbol argument.
  - `enum_type` reports a member without a value whose name is a macro
    visible in the enum's body (`member_names_macro`, E0914, fixed by
    adding `()`), keeps the member, and adds the enum to `failed_owners`
    so the methods the macro would generate aren't reported missing.
- The symbol index (`wid_sema::index`, for `wid doc` now and `wid query`
  and the LSP later): `check_program_indexed` runs the checker like
  `check_program` and then `Checker::build_index` (`check/index.rs`), which
  only reads the checker's tables (`decls`, `pkg_scopes`, `file_imports`,
  `include_items`, `merged_cimports`) and reports nothing. Every `DeclId` is
  the `SymbolId` of the same number. Names in `include`s, `using` field
  types, `extend` targets, union variants and type aliases resolve with the
  checker's pure lookups from the file where they are written (a virtual
  file's `DeclLoc` for generated code), following type aliases. Symbols hold
  owned data: the declaration line from `wid_syntax::print::Printer` (which
  reads literals from `source_texts` and names spliced types with
  `TypeTable::display`), the doc, fields and enum members (whose docs come
  from `wid_syntax::docs::DocComments`, since the AST keeps none for them),
  and for a `cimport` package's declarations the C name (the `extern`
  attribute) and `CBinding::locations`. A package's `items` are sorted by
  where they stand: generated ones at their outermost macro call, merged
  `cimport` ones at the `cimport`. `Index::resolve` resolves symbol paths
  and `member_groups` lists a type's members by origin, in lookup order.
- `wid doc` (`wid_driver::doc`) reads its arguments against
  `DocRequest::dir`, so the suite runs it without changing directory, then
  loads and checks with `CheckOptions::library` (no `def main` needed) and
  builds a `Page` of `Entry`s that `render_text` and `render_json` print;
  `doc::print` is what the CLI and the suite share. Errors about the
  request point into a "command line" source holding `wid doc ARGS`, like
  `wid cimport --dump`'s, so fixes show the corrected command.
- `tests/doc/NAME.args` cases run `wid doc` from `tests/doc/` (or
  `tests/doc/DIR` with a leading `-in:DIR`) against `NAME.stdout` and
  `NAME.stderr`; the directories there are their packages. Codes in their
  `.stderr` count as covered for the docs check. `docs/errors` pages about
  another command say so with `<!-- command: ARGS -->` and
  `<!-- fix-command: ARGS -->` (E0601–E0605), which both errdocs scripts
  follow.
- The query engine (`crates/wid_query`, for `wid query` now and the LSP
  later) depends on `wid_sema`, `wid_syntax` and `wid_diagnostics` only;
  `wid_driver` depends on it, so the suite in `wid_driver` runs it and the
  graph stays acyclic. `Analysis::check` (pure: a `ProgramInput` in, the
  sources, sorted diagnostics, `Index` and `Extents` out) is what
  `wid_driver::analyze` calls after `load_program`, as a library and
  without codegen; `wid doc` loads through it too. `Extents` finds a
  declaration's whole span (attributes and `private` to its last token)
  by walking the parsed files for the smallest item around the name the
  index holds, so the checker isn't touched: for generated code, the
  `quote`'s item (the span keeps the expansion's file id) or, for a name
  from a macro argument, the macro call; for an enum member, its name and
  value. `wid_query::item` holds the model both commands print (`Item`,
  formerly `doc::Entry`, built by `ItemBuilder`, formerly `PageBuilder`)
  and `item_json`, whose `Style::Query` adds `span` and the ends of
  locations; `wid_query::json` builds the query document. Queries return
  `Failure` (malformed path, `PathError`, not a type), and
  `wid_driver::cmdline` (shared by `doc` and `query`: the command line as
  a source, `package_target`, `ErrorContext`) turns it into E0601–E0603,
  worded per `Tool`. `Query::parse` gives usage errors, which the CLI
  prints with status 2.
- What names and expressions resolve to (`wid_sema::uses`, for `wid query
  refs`, `calls` and `type`): `Checker::recorder` is `Some` only in
  `check_program_indexed`, and the `note_*` methods (`check/record.rs`)
  return at once when it is `None`, so `check_program` allocates nothing
  and pays one branch per call. They are called where a name resolves:
  `Checker::expr` for every expression's type; `ident` (locals),
  `const_ref_decl` and `fold_const` (constants, at a qualified one's last
  name), `call_fn`, `call_with_values` and `call_package_set` (the set)
  and `call_member` (the member chosen; `chosen_macro` for a set called
  among declarations), `call_macro` and a declaration-level macro call,
  package operator calls (at the operation), `value_member` and
  `ivar_owner` (fields, at the declaring struct, so promotion needs
  nothing more), `struct_new` (named
  arguments, writes), `enum_member`, `package_member` and type paths (the
  import name), `resolve_type_inner` (every written type, wrapped around
  `resolve_type_here`), `resolve_path_type`, `decl_as_type` and
  `generic_instance` (types; `decl_as_type` at a declaration's own name is
  dropped), `resolve_include`, `overload_members` and `method_ref` (reads),
  `declare_var`/`declare_param` and a proc's parameters (bindings), and
  `lower_assign` (`note_write`: a read at an assignment target's name is a
  write). Refs are deduplicated by span, target and kind. A span's type is
  kept per lowering (the first frame's declaration and bindings plus the
  innermost generic instance): the last type within one lowering wins (an
  untyped literal lowered again with a parameter's type), the first
  lowering's is the type, and the others' go to `instances`.
  `Checker::build_uses` adds every declaration's own name (symbols,
  fields, enum members, import names) with its type: a method's proc type
  from `sigs`, a constant's, the declared type, a field's from its written
  type; and every signature's parameters. Fields and enum members are
  numbered as the index numbers them (the fields written in the body).
- `wid_query` reads `Uses` from `Analysis::uses`. `refs` maps a span in a
  virtual file to its outermost macro call (`SourceMap::expansion_chain`)
  with `via_macro` the expansion's name, leaves out uses in `cimport:`
  files, and finds `context` as the smallest symbol extent around a use
  that isn't the symbol's own name. `type_at` takes the smallest typed span
  around the position and the smallest use inside it (a declaration,
  then a read or write, a type, an import, a call; a set's chosen member
  before the set), and a name inside a larger expression gets its own
  declared type unless it names the call or the generic type that starts
  a written type. `find_file` matches the position's file by display, by
  path, or by the end of a path; the driver picks the package that holds
  the file when there's no `-in:`. Position failures are
  `Failure::Position`, E0605 in `cmdline.rs`.
- `tests/query/NAME.args` cases run `wid query` from the repository root
  (packages are `-in:tests/doc/shapes` and the like) against `NAME.stdout`
  and `NAME.stderr`; a usage error expects the CLI's two lines on stderr.
  `tests/query/cmerged` is a package with a `cimport` without `as:`, and
  `tests/query/game` (with `geo`) the program `refs`, `calls` and `type`
  cases read.
- Every `core` package opens the file named after it (`core/mem/mem.wid`,
  `core/builtin/builtin.wid`) with its package doc, and every public
  declaration in `core` has a `# ` doc comment directly above it: types,
  fields, enum members, methods, constants, overload sets and extensions.
  Other file headers describe their file. `wid_driver/tests/core_docs.rs`
  checks both.

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
- Types written in place as arguments: `size_of`, `align_of` and `type_info`
  take any type (`Int?`, `proc(Int) -> Int`, `@[c] proc(I32)`, `(A, B)`,
  generic instances, also from other packages, `C.int?`); in any call, a type
  ending in its own `?` or a `proc(…) -> R` type parses as a type
  (`alloc(Int?)`, `Pool(Int?, 2)`). The parser decides
  (`Parser::parse_type_arg`); expression-shaped names are resolved by
  `Checker::type_arg` and `named_type`. A type's `?` after a space
  (`size_of(Int ?)`) is E0105 with a fix, a variable given as a type
  (`size_of(count)`) is E0322 with a fix that writes its type, and an
  optional proc displays as `(proc(Int) -> Int)?`.
- A `$T` written as a parameter of its own (`$T, xs: []T`, or Odin's
  `$T: typeid`) is one E0105 at `$T`, with a fix that introduces it where a
  parameter's type first uses `T` (`xs: []$T`); the parameter list recovers
  as that fix, so the rest of the file is checked normally.
- Parenthesized types as constant values: `MaybeCb = (proc(Int) -> Int)?`,
  `Pair = (Int, String)` (`Parser::try_type_alias`). A value in parentheses
  that parses as an expression (`(1 + 2) * 3`, `(Vec2)`) stays one.
- Types written in place in calls without parentheses: their arguments end
  with the statement or at an `if`/`unless` modifier (`n = size_of Int?`).
  Nested (`puts size_of Int?`), that is one E0109 whose fix puts the `)`
  before the line end; after an argument that failed to parse, the fix is
  only `MaybeIncorrect`.
- `private` on a struct field is E0105 (fields are always public), once per
  field, with a machine-applicable fix that removes it (#9).
- `private` where it hides nothing is one E0105 with a machine-applicable
  fix that removes it (#28): before an `import`, `cimport`, `include`,
  `extend`, `comptime if` or a splice among declarations (which ignored it),
  before an enum member (which became an undefined macro call), and before
  a statement in a method or a `quote` (which cascaded; the line is now
  parsed as if `private` weren't there, so `private x = 1` declares `x`).
  Keywords in parser messages are in backticks (`found `end``).
- An unclosed `(` in an expression (`X = (1 + 2`, `E = ([]`) is one E0105
  at the end of its line, with a machine-applicable fix that adds the `)`,
  when the expression is complete and the next line isn't its `)`; the
  next line is parsed on its own, so a `def main` after it is no longer
  lost (#33). A `(` that ends its line before a declaration or an `end`
  (`X = (`) is one "expected an expression" there.
- A `?` right after a type's name or its closing `)` (`t = Int?`,
  `rl.Color?`, `Pool(Ball, 64)?`, `(proc(Int) -> Int)?`) ends the type,
  written in place in any expression, unless a conditional's `:` follows
  (on the line, or spaced on the next one, as in `f(FLAG?` / `1 : 2)`):
  `t = Int?` is one E0323 with the `nil` help instead of a conditional that
  ran past the line end (#34). `names_type` is shared with the
  `size_of(Int ?)` check.
- #33's rule covers argument lists, arrays, indexes, `{ }` blocks and
  block parameters (#40): a list whose line ends after a complete item
  with something other than `,` or its closer on the next line is one
  E0105 at the end of the line, with a machine-applicable fix that closes
  it (`X = max(1, 2`, `X = [1, 2`, `xs[1`, `xs.each { |x| p x` before
  `end`), and the next line is parsed on its own. A next line indented
  under the list that starts a value is its next item with the `,`
  missing (a fix to review). A declaration or `end` starting the line
  after an operator, `=`, `,`, `(`, `[`, `{` or `|` (where the lexer
  drops the line end), or a named argument's `:`, is never read as its
  operand: `X = 1 +` before `def main` is one "expected an expression,
  found end of line". Line ends
  put back this way are taken back when a speculative type parse rewinds,
  and a constant whose value holds a parse error no longer adds E0327.
- #40's rule covers parameter lists and types too (#52), through the
  same `List`, `list_left_open` and `list_step` (with `Items` telling
  arguments, unnamed items and parameters apart): `def foo(a: Int,`
  before `def main` is one E0105 with a fix that adds the `)`, and the
  method ends with that line (before a statement, the lines after it are
  its body; an indented `b: Int` is the next parameter, `,` missing);
  procs' parameters and proc types alike. A type's arguments
  (`Pool(Int, 4`), a tuple, an array length, `[^`, a map key and a
  matrix's size left open before the next field are one error with a
  fix (`close_type_bracket`, `parse_type_list`), and the field is still
  declared; after a `,`, a `name:` line ends a list whose items are never
  named. A named argument's value on a line indented no deeper than the
  call's (`x = add(a:` before `p x`) is missing. `recover_line` stops at
  a declaration, or an `end` no skipped `do` opened, starting a later
  line, so `X = Foo{` before `def main` is one error, and a constant
  whose line goes on after its value isn't checked on its own (no
  E0327).
- A C type of `core:c` written as a value (`t = C.int?`, `u = C.size_t`)
  is the type written in place, like `t = Int?`: one E0323 with the help
  that fits it (`nil` for the optional), instead of an undefined member
  `int?` or `int` (#41). The checker recognizes the C type name, with the
  `?` the lexer glues to a lowercase name taken off, where the member
  lookup fails; predicates like `xs.empty?` are untouched.
- `private` before a call or a name alone on a line of a `quote`
  (`private helpers :foo`) is a private declaration-level macro call, as
  written at package level: the parser makes the line that item, so what
  the call generates is private (#42; it was E0105 "`private` applies
  only to declarations"). Before an assignment or a local it is still that
  E0105. Expanded in a method, the line is a nested declaration. The
  speculative parses share a `Mark` that also takes back put-back line
  ends.
- A splice glued to text in a name inside a `quote` (`def bump_#{name}`,
  `@#{name}_count`, `:a_#{f}`, `struct #{name}Box`, in any name position)
  is one E0111 "a splice can't be part of a name" instead of five
  cascading errors (#45). Its fix, to review, builds the name before the
  `quote` (`bump_name = "bump_#{name}".to_sym`, once for each name) and
  splices it (`def #{bump_name}`); in a `quote` inside a splice it is a
  help. The parser reads the glued name as a splice of the
  `"bump_#{name}".to_sym` it meant, so the declaration parses and code
  using the generated name adds no errors.
- Outside a `quote` too, a splice glued to a name (`def bump_#{name}`,
  `#{prefix}_LIMIT = 3`, `hp_#{stat}: Int`) is one E0111 "`#{` starts a
  splice outside a `quote`", noting that it can't be part of a name
  either, with the help that builds the name in a macro (#61). It was
  four errors, with a fix that read it as a comment (`def bump_# {name}`)
  and lost `def main`. `glued_len` works outside a `quote` (and, since
  #87, inside a splice's expression), `stray_is_comment` never takes a
  splice glued to a name for a comment, and the line is read as the
  declaration it was written in, with the name as written, which no code
  can refer to. The
  E0111 title is now "misplaced splice" (`codes::MISPLACED_SPLICE`), as it
  also covers a splice inside a splice and one glued to a name.
- Any type written where a `Type` is expected, or in `comptime` code, is a
  `Type` value: constructors (`name_of([]Int)`, `name_of(Int?)`,
  `name_of(^Node)`, `name_of((proc(Int) -> Int)?)`), generic instances and
  package types (`name_of(Pool(Ball, 64))`, `name_of(C.int)`). Elsewhere,
  E0323's help fits the type: `.new` only for `[dynamic]T` and `map[K]V`,
  `nil` for an optional, a proc literal for a proc type, `&x` for a pointer
  and `{}` otherwise.
- A member a `Type` value doesn't have (E0204, `no_type_value_member`) lists
  what it answers (`.name`, `.size`, `.align`, `.fields`) and suggests the
  closest; for `.methods` it offers a `[]MethodInfo` parameter given
  `T.methods` with the type written by name, or `Self.methods` in a macro
  called in the type's body (#31).
- Named constants as generic value arguments: `Pool(Ball, MAX)`,
  `Pool(Ball, MAX * 2)`, `Pool(Ball, (N))` and `comptime` results work like
  the literal, in types and in calls (`Checker::value_generic_arg`). A
  non-integer constant is E0315 saying the parameter takes an `Int` (with a
  `.to(Int)` fix for a float), a variable is E0315 with its declaration, an
  unknown name is E0201, and a reported argument no longer cascades into
  E0327 at the instance's `[N]T` fields.
- An `import` inside a `struct`, `enum`, `module` or `extend` body is E0105,
  like a `cimport` there, instead of being ignored. Both get a
  machine-applicable fix that moves the line after the file's last import
  (or above its first declaration), and the names they would bind aren't
  reported again. Suggestions whose edits are far apart show each place,
  with `...` between them.
- `[$N]T` in a method's signature is one E0105 at `$N` (a method can't take
  a value parameter), whose fix takes a slice and reads `N` as `xs.size`;
  the parameter recovers as that slice and the uses of `N` aren't reported
  again. Elsewhere, like in a struct field, `[$N]T` is one E0105, and an
  array length that failed to parse is no longer reported again as E0327.
- A type name in parentheses is a type alias: `X = (Int)`, `P = (Vec2)`
  (the checker looks through `Paren` when it classifies a constant, so
  `X.new`, `X.size` and `name_of(X)` work too), while `Y = (NINE)` stays a
  value. A type alias constant passed where a `Type` is expected is that
  type (it was `{unknown}`), and a value constant's methods (`X.abs` for
  `X = NINE`) no longer read it as a type. A constant whose value starts
  with `(` and a type-only token (`(proc`, `(^`, `([]`, `(distinct`, …)
  commits to the type parse, and an unclosed `(` at the end of a line is
  one E0105 there with a fix that adds the `)`.
- A generic struct or union of another package works as a receiver:
  `geo.Box(Int).new`, `geo.Box(Int).size`, its `def self.` methods and
  `geo.Outcome(Int).align` (it was E0323, a call of the member `Box`). A
  receiver call goes through the same `generic_instance` as `type_info`
  and `size_of`, so an unqualified generic union (`Outcome(Int).size`)
  works too, a private one is E0205, and `geo.Box.new` or `Outcome.size`
  without arguments is E0315 at the use (`geo.Box.new` compiled, and
  `Outcome.size` was reported at the declaration). `x = geo.Box(Int)`
  suggests `geo.Box(Int).new` (#27).
- A generic struct's value parameter is a constant in its methods and in
  an `extend` of it: `def cap -> Int = N` in `Pool(Int, 4)` is `4` (it was
  E0201, undefined constant `N`). A constant name looks in the frame's
  generic bindings first and reads a `$N` there as an untyped integer
  constant, so it fits `U8` or `F64` where those are expected; constant
  folding (`fold_const_in`, `eval_const_in`) takes the bindings of a type
  context, so `buf: [N + 1]U8`, `Pool(T, N * 2).new`, `matrix[N, N]F32`
  and `comptime N * 2` work in a body, and `N.times` and `type_info(N)`
  read `N` as a value instead of a type (#35).
- Method signatures read a value parameter per instance: in `Pool(Int, 2)`,
  `def bigger -> Pool(T, N + 1)` is `Pool(Int, 3)` (it read a package
  constant `N` when there was one, else was E0315) and `def all -> [N]T` is
  `[2]Int` (it was `[0]Int`). A type that reads a placeholder value
  parameter (`[N]T`, `[N + 1]T`, `matrix[N, N]F32`, `Pool(T, N + 1)`)
  marks the signature `per_instance`, and `fn_sig_inst` resolves such a
  signature again with the instance's bindings, as does `call_fn` for its
  first coercion; the template keeps no error for it, so `buf: [N + 1]T`
  fields work too. `comptime` code in a type (`[comptime N * 2]T`) reads
  the bindings of the type's context instead of the code being lowered.
  A value parameter where a type is expected (`y: N`, `size_of(N)`,
  `-> N`, a field `slot: N`) is E0322 with a fix that writes its declared
  type, and a value parameter must be declared `$N: Int`: another type is
  E0315 at the declaration with a fix (#37).
- Diagnostics around type names: a type parameter used as a value
  (`def f -> Int = T` in `struct S($T)`) is E0323 "`T` is a type, not a
  value" pointing at `$T`, with a `size_of(T)` fix where an integer is
  expected (it was E0201, undefined constant `T`); calling a generic struct
  (`Local(Int)`) suggests `Local(Int).new` (it suggested `Local.new(Int)`);
  kinds take the article they sound with ("a union", not "an union"); and a
  name an imported package lacks is "`geo` has no member `Missing`" with a
  did-you-mean over the package's public names, or a list of them when
  there are few (it read "undefined member of `geo` `Missing`") (#38).
- Macro syntax: `quote` bodies holding statements and declarations, with
  splices in every expression, type, declaration and name position (`#{x}`,
  `@#{f}`, `:#{s}`), splices outside a `quote` (E0111), variadic
  `*names: T` macro parameters (E0112), and package-qualified
  declaration-level macro calls.
- Macro expansion in expressions and statements: `macro def` bodies checked
  and run at each call (`Code`, `Symbol`, `Type`, computed and `*names: T`
  parameters), `quote` with every kind of splice, `Code` and `Symbol`
  values (`sym.to_s`, `str.to_sym`, `==`), qualified and private macros,
  definition-site name resolution, hygiene, nested expansion, budgets,
  errors in generated code with their call chain (human and JSON), `#line`
  and panic locations in the `quote`, and errors E0910–E0912.
- Macro calls among declarations: at package level and in `struct`, `enum`,
  `module` and `extend` bodies, qualified ones included, expanded in source
  order with the pending `comptime if`s; generated methods (`def`,
  `def self.`), constants, types, `overload` sets, `include`s, `comptime if`s,
  `macro def`s and further macro calls (nested, within the budgets), all
  visible to code written before the call; `private` calls; a macro's
  `Self` (the type whose body or method holds the call, so `Self.fields`
  drives generated methods); errors in generated declarations and in their
  bodies with the call chain; E0913 for generated fields and imports, E0108
  for generated statements, E0209 for `Self` without one type. E0902 is
  retired. An `include` in a type-level `comptime if` is no longer ignored.
- Fewer cascades after errors: a local whose value or type is an error (a
  parse error, a type used as a value, an unknown type) is not reported as
  unused (#6); a macro call that fails to expand or names no macro hides
  the undefined names, missing members and unread variables that its code
  might have declared or read (#8); an enum member written as a name alone
  that a macro also has is E0914, with a fix that calls the macro (#16);
  uses of a field that E0913 rejected aren't reported missing, and a call
  in a method whose name is close to a macro's (E0201, whose help names the
  macro) or that stands alone with a symbol argument counts as a failed
  expansion (#25); after a macro call at package level fails (a splice that
  doesn't fit, or an error in the macro's own code), no missing member of
  any type is reported, since it may have generated an `extend` (#29).
- A local whose value holds a parse error anywhere (`x = add(a:`,
  `z = [1, add(2 - )]`, either name of `a, b = pair(1 * )`) is not reported
  as unused either, though the value's type is known (#58).
  `holds_parse_error` walks the whole expression with the new read-only
  `wid_syntax::visit::Visit` (generated with `VisitMut` from one macro), so
  constants, array lengths and type arguments see errors at any depth too.
- A constant's value that doesn't fold and needs no interpreter (a name, or
  arithmetic on names) is checked as code instead of being E0327 "constant
  value is not known at compile time" (#59): `X = Foo` with `Foo`
  undefined is E0201 "undefined constant `Foo`" with its did-you-mean, as
  in a method; `X = width` is E0327 with the `comptime` fix; and values
  that failed before evaluate (`START = ORIGIN` for a struct constant,
  `FACING: Dir = :north`, `X = 1 / 0` is E0901). The E0327 that remains
  says the compiler can't compute the value, with a note on what a
  constant's value may be.
- Struct literal syntax from Odin, Go, Rust or Zig, a constant directly
  followed by `{` (`Foo{a: 1}`, `Foo{1, 2}`, `geo.Vec2{x: 1.0}`,
  `Pool(Int, 4){}`), is one new E0113 "Wid has no struct literal syntax"
  at `Foo{`, with a machine-applicable fix that writes `Foo.new(a: 1)`
  (Odin's `a = 1` becomes `a: 1`; braces over several lines become the
  call's parentheses) (#60). It was E0323 and E0105 in a method, and E0105
  alone at the top level. `Parser::parse_struct_literal` reads the braces as
  the arguments of that `new` call, so the value has the struct's type and
  adds no more errors; left open before the next line, it is this error
  alone, whose fix also closes the call. A `{ |x|` after a constant is still
  a block.
- Errors in code a macro spliced in from its call site (a `Code` argument,
  a name from a `Symbol`) point at the splice in the `quote` and list the
  calls, in both renderers; a name the macro computed is named in the
  label at the call instead of the call getting a type (#30).
- A bare read of a field of `self` in an instance method (`hp` for `@hp`),
  its own or promoted by `using`, is E0201 with a note naming the struct
  that declares the field and a machine-applicable fix that writes `@hp`
  (#44); a promoted field read that way was E0305 with a help about
  arguments. `Checker::self_member` searches like `self.name`, so methods,
  implicit-self calls and promoted proc fields work as before.
- A field of `self` called like a method (`hp()`), its own or promoted, is
  E0201 with the same note and a fix that writes `@hp` in one step; a field
  holding a proc gets `@on_hit.call(…)`, and arguments or a block get a
  help instead (#48). A bare name close to a field's suggests `@hp`
  (`Checker::self_field_names`, which reaches promoted fields too). A name
  that several `using` fields promote, written bare, called or as `@id`, is
  E0204 alone, with a fix that writes `@body.id` rather than `body.id`.
- No message puts "a" or "an" before a type name, since no rule follows how
  every name is read (`User`, `UInt`, `F32`, a user's `Hour`): messages
  name the type without one, as in "this field's type is `User`", "a value
  of type `U8` cannot be called" and "`RATE` has type `F64`" (#50; it read
  "an `User`" and "a `F64`"). `wid_diagnostics::a_or_an` is gone; kind
  names keep their fixed articles (`DeclKind::a_describe`).
- Messages that list names join them with `wid_diagnostics::and_list`:
  "`a`, `b` and `d` all provide `hp`", keeping "both `a` and `b`" for two
  (#54; it read "both `a` and `b` and `d`"). The same goes for tied overload
  members, `a, b, c += 1` and C names that become the same Wid name.
- Inside a method of a type, a misspelled name or call close to a method
  that a call without a receiver reaches (its own, mixed in with `include`,
  added by `extend`, or promoted by `using`; only type-level ones in a
  `def self.`) suggests it: `heall(1)` gets `heal(1)` (#54). Like every
  "did you mean", the fix is `MaybeIncorrect`, a guess to review. Ties go
  to a variable, then a method, then a package name, then a field
  (`Checker::self_names`, `SelfNames`).
- `@name(args)` calls the proc a field of `self` holds, its own or promoted
  by `using`, like `self.name(args)` (#55; `@cb()` was E0301 and E0105).
  The parser reads it as `Callee::IVar` when the `(` follows the name with
  no space, with a trailing block the way `self.name(args)` takes one;
  `Checker::ivar_call` lowers it. `@hp()` on another field is E0305 with a
  fix that drops `()`, as for `self.hp()`; `@heal(1)` on a method is E0204
  with the fix `heal(1)`; an ambiguous `@id()` gets `@body.id`. An own proc
  field called by bare name (`on_hit(3)`) gets the fix `@on_hit(3)`
  instead of `@on_hit.call(3)`, and a call close to a proc field's name
  suggests `@on_hit`.
- "Did you mean" picks the same name on every run (#64; `fooe` among
  `fooa`…`food` suggested a different one each run). `did_you_mean` gives
  ties to the shorter candidate, then the first, and every caller now passes
  candidates in a fixed order: package names (`Checker::package_names`), a
  type's methods, an overload set's scope, a file's imports and a header's
  records sorted, as are the files and directories a missing `embed` file
  or import path is compared with; "known collections" lists the
  `-collection:` names sorted. `tests/ui/suggestion_ties.wid` covers ties.
- `@mend` or `@mend(1)` on a method mixed in with `include`, added by
  `extend` or promoted by `using` gets the message an own method gets (#65;
  it was "`P` has no field `@mend`" with no fix, or "`@push` names a method
  of `Body`" for a promoted one): "`@mend` reads a field, but `mend` is a
  method", a note saying where the method comes from and the
  machine-applicable fix `mend(1)`, or `self.mend(1)` when a variable
  `mend` is in scope. `Checker::self_method_origin` searches as a call
  without a receiver does. A missing field reads "`P` has no field
  `on_hitt`", without the `@`.
- A macro can generate `@#{name}(args)`, which calls the proc the field
  holds as `@name(args)` does (#67; it was E0105 and E0301). The parser
  reads it as `Callee::IVar` when `(` follows the splice's `}` with no
  space, and the splicer substitutes the name like any call's. Its errors
  point at the name the macro call gave, with the splice and the call, as
  for `#{name}(args)`; fixes in the `quote` keep the splice and are
  `MaybeIncorrect`: `@#{name}()` on a field drops `()`, on a method it
  becomes `#{name}(1)`, an ambiguous name `@body.#{name}`, and a misspelled
  field is fixed at the call (`:hp`). `Checker::splice_site` and
  `name_end` find the splice, which also fixes the `()` fix for a field
  called as `self.#{name}()` (it edited unrelated text).
- `@#{name}` read or assigned without arguments reports a missing field
  at the name the macro call gave, with the splice and the call, like
  `@#{name}(…)` (#95; it pointed at the `quote` with no splice label, and
  its fix replaced the splice with a literal `@hp`). A misspelled field is
  fixed at the call (`:hp`); a method's fix keeps the splice (`#{name}`,
  `MaybeIncorrect`). The splicer records the name's span for each
  `@#{name}` (`MacroState::ivar_names`), which `Checker::ivar` looks up
  (`spliced_ivar`). Of several splices of one name, the one picked when
  neither the statement nor the method being checked holds one is the
  first in the latest expansion (`Checker::pick_splice`).
- A field's name spliced as a bare name (`#{f}`, `#{f}()`, `#{f}(3)` for
  a proc field) is fixed at the splice in the `quote` (`@#{f}`,
  `MaybeIncorrect`), not at the call's argument (#79; the fix was
  `gen :@hp`, which doesn't parse). `Checker::name_splice_site` finds the
  splice as `splice_context` does.
- E0201 explains names that macros keep apart (#79; it was a bare
  "undefined name"): generated code naming a caller's variable says
  hygiene hides it, with a label on the variable and a help to pass its
  name as a `Symbol` (`#{name} += 1`); a `quote` naming its macro's
  parameter says so, with the fix `#{e}`; and code naming a variable a
  macro's code declared points at it as private to the expansion (no
  E0203 for it any more). `Checker::explain_macro_name`, called by
  `undefined_near`, finds the macro through `Expansion::decl` and the
  hidden variable by its mark (`hidden_var`).
- The human renderer shortens a list of more than six macro calls behind
  an error to the first three and the outermost, with a line like "...
  60 more expansions of `ping` and `pong`" (#79; macros calling each
  other to the nesting limit printed all 64 calls, 266 lines). JSON keeps
  every call. `render::shorten_frames`.
- Macro errors are worded for macros (#79): a variable given to a value
  parameter (`rep(k)` with `n: Int`) is E0327 "the macro `rep` runs while
  compiling, so its `Int` parameter `n` needs a constant", with a help to
  take `Code` (`MacroState::value_arg`, read by `report_capture`); a
  spliced type used as a value names the type (`` `Int` is a type ``, not
  `` `#{t}` ``); and `3.twice` with `twice` a macro is E0204 with a label
  on the macro and the fix `twice(3)` (`Checker::macro_as_method`).
- A macro whose body failed to parse (an empty splice `#{}`, say) doesn't
  run, so its calls report nothing more (#79; it ran and failed with
  E0901 "a value of type `{unknown}` can't be spliced", reported before
  the parse error). `check_macro` asks `runtime::body_holds_parse_error`
  and notes it in `MacroState::unparsed`, which `expand_code` checks;
  the call counts as a failed expansion, so what it would have declared
  isn't reported missing.
- A name that two calls of a macro declare (E0202, E0317) shows the line
  of the `quote` once, labelled for each expansion, with both calls
  (`first expansion here`, `second expansion here`) in the human and JSON
  output, and a help to call the macro once or splice the name (#79; the
  "first definition" label sat on the same `quote` span as the second,
  and only the second call appeared). `Checker::repeated_expansion`
  rewrites the labels in `splice_context`; the human renderer no longer
  lists a call that a secondary label already shows.
- A negative argument for a generic struct's value parameter that is a
  field's array length (`Grid(-1)` with `cells: [N]U8`) is E0301 at the
  argument, with a label on the array and a note naming the field (#79;
  it pointed at `[N]U8` with nothing at the call, plus E0203 on the
  variable holding the value). `check_generic_args` reports it
  (`sized_field`) and fails, so the instance is unknown; a value argument
  that was reported (`Grid(1.5)`) fails it too.
- A splice belongs to the innermost `quote` (SPEC "Compile-time", decided
  in #79): in a `macro def` a macro generates, the inner `quote`'s `#{n}`
  reads the inner macro's names. Naming the outer macro's parameter there
  is E0201 saying so, with a label on the parameter and a help to
  generate a constant (`N_VALUE = #{n}`) or splice the value into the
  inner macro's own code (it was a bare "undefined name"). `lower_quote`
  sets `MacroState::splicing` while it lowers splices, and
  `explain_macro_name` checks that the macro being lowered came from the
  name's expansion (`generated_macro`). `tests/run/macro_nested_quote`
  covers both ways to pass the value.
- A `def` that returns `Code` is one E0910 at the `def`, "`make_const`
  returns `Code`, but it is a `def`, not a `macro def`", with labels on
  its `quote` and on a call among declarations and the machine-applicable
  fix `macro def` (no fix in a type's body) (#102; a call among
  declarations gave E0910 at the `quote`, E0108 with the help "move it
  into `def main`" and E0201 for each name it would have declared).
  `Checker::returns_code` and `not_a_macro_def` (once per `def`,
  `MacroState::not_macros`) serve `resolve_item_macro`, `chosen_macro`
  and `lower_quote`; the call among declarations runs
  `failed_among_declarations`.
- A proc parameter whose name a macro splices (`->(#{v}: Int) -> Int {
  #{v} * 2 }`), in a method body or a generated `def`, is the caller's
  name, as a block parameter's is: the proc's body and code spliced from
  the call site see it (#73; it was E0201). `lower_lambda` declares each
  parameter at its name's span instead of the whole parameter's.
- A macro call that is a statement of its own runs its last line as a
  statement too: an `if`, `case` or `comptime if` whose branches have no
  value works there, in a method body, a branch or a block, also through
  nested calls (#72; it was E0323 on each branch). Where the call's value
  is used, E0323 (or E0301) on a branch says that the last line gives the
  call's value, and the help calls the macro as a statement.
- A macro call in an operand that may not run (the right side of `&&` or
  `||`, the value of `||=` or `&&=`, the arguments of a `&.` call, a `when`
  pattern after the first), never runs (`type_info`'s operand) or
  runs on each test (a `while` or `until` condition) runs its code with the
  operand (#70). The names it declares are the operand's: a use after it
  is E0201 with a note naming the macro and the operand, where the C
  compiler rejected an undeclared variable. A `defer` there is the new
  E0406 at the call, where it ran at the end of the block even when the
  operand didn't. A `when` pattern that needs statements (like a macro
  call's) is tested only when the earlier patterns of its `when` didn't
  match; they all ran before.
- Macros in an `overload` set expand when a call chooses them, in an
  expression (with the expected type), as a statement or among
  declarations (#74; the chosen macro was called as a run-time function,
  E0906, and a `Code` member never fit). A `Code` parameter takes any
  argument and ranks below a typed one; a macro with a `*` parameter can't
  be a member (E0316); a `def` chosen among declarations is E0108.
- A `macro def` named like an operator (`+`, `[]`, unary `-`, …) is E0915
  at the declaration, with a fix that names it (`add`) and a help to define
  the operator with `def` (#74; it was accepted, and `a + b` then called it
  at run time, E0906). Package operators skip macros, so a use is E0307
  like any missing operator. An operator's `overload` set can't list a
  macro either (E0915).
- A name spliced from a `Symbol` must be one the lexer reads in its place
  (#77; `"Odd-Name".to_sym` named a struct, `"x-y"` a field and `"end"` a
  local): a capitalized identifier for a type or a constant, an identifier
  for a method (maybe ending in `?` or `!`, or an operator), field,
  parameter, variable or enum member, never a reserved word. Otherwise it
  is E0911 at the call, naming the name, the place and its rule, with a
  name that would fit (a fix at the call for a symbol argument). An empty
  name is "an empty name" (it was E0203 on a variable named ``).
- Code spliced alone on a line in a generated enum's body gives members:
  each line that is a name alone (`#{m}`) or `name = value`, in the place
  of the splice among the written members, so `[]Code` fragments build an
  enum (#76; the names were read as undefined macro calls, E0201, and
  `name = value` as a statement, E0911). A spliced `Symbol` member keeps
  its place too (it went last). A line that is neither a member nor a
  declaration is one E0911 that quotes it, and E0204 leaves out an empty
  "members:" note.
- An operator with nothing after it before a closer (`[1, 2 +]`, `(1 +)`,
  `f(1, 2 *)`, `{ |x| x > }`, `[1, -]`) is one E0105 "expected an
  expression after `+`, found `]`" (`Parser::missing_operand`), with a fix
  to review that removes a binary operator; the closer is no longer
  consumed, so it still closes its bracket (#85; `[1, 2 +]` also got
  "expected `]`" with a fix that added a second `]`).
- `Foo { a: 1 }` and `geo.Vec2 { x: 1.0 }`, with a space before the `{` as
  Rust and Go write it, are #60's single E0113 with the fix `Foo.new(a: 1)`
  (#86; it was E0323 and E0105). A constant never takes a block, so
  `struct_literal_ahead` accepts a spaced `{` on the constant's line, but
  not after a generic instance (`Pool(Int, 4) { … }` is a call's block) or
  before a block's `|x|`. The fix takes the spaces inside the braces with
  them (`Foo { }` becomes `Foo.new()`).
- A field name glued to a splice and called like `@name(args)` in a
  `quote` (`@on_#{event}(n)`) is #45's one E0111, read as a call of the
  proc the field holds (`Callee::IVar`) like `@#{name}(args)`; the `(n)`
  was "expected end of line" and the field's proc type an E0301 (#95,
  second part).
- Inside a splice's expression, a name glued to a splice
  (`#{foo_#{name}}`) is one E0111 "a splice inside a splice", whose fix to
  review builds the name before the `quote` (`foo_name =
  "foo_#{name}".to_sym`) and splices it, `#{foo_name}` (#87; it was E0201
  "undefined name `foo_`" and "expected `}` to close the splice").
  `glued_len` now works inside a splice's expression too, and
  `Parser::glued_in_splice` reads the name as that `to_sym` call, so the
  code it lands in adds no errors. In a name position there (a method
  name after `.`), the help explains it without edits.
- `quote` lines (#75): `#{name} = distinct F64`, `#{name} = proc(Int) ->
  Int` and `#{name} = @[c] proc(…)` are constant declarations
  (`Parser::quote_item_ahead`, `type_only_value_at`), as `NAME = v` is;
  `#{name} = 5` stays an assignment, which among declarations is a
  constant (they were E0201 "undefined method `distinct`"). `#{t}?` is
  the optional type in any expression, as `Int?` is (`names_type` takes a
  splice; it read as a conditional). A `using` line is a field, parsed as
  in a struct, so a fragment spliced into a generated struct may hold
  one (it was E0105 and up to 12 errors); generated in the struct whose
  body holds the call, it is E0913 alone, since the struct's missing
  members may be ones it would have promoted. A splice where the parser
  takes none is one error: `recover_line` skips a splice whole (its `}`
  ended the `quote` and cascaded into up to 11 errors), as do the
  attribute list and generic parameters; `overload #{name}` gets the fix
  `:#{name}` and reads as it, and `import #{path}` notes that generated
  code can't import.
- Ruby habits are one error each, with a fix, and the rest of the file is
  checked (#80): `quote { 1 }` is E0105 with the fix `quote do 1 end` and
  reads as that `quote` (`Parser::braced_quote`; it lost `def main`);
  `macro twice(…)` is E0105 with the fix `macro def twice`, parsed as if
  `def` were there; a macro's string where `Code` is expected (E0301)
  gets a help, with the string written out as a `quote` when it is a
  literal (`Checker::string_as_code`); a block passed to a macro that
  takes `Code` is E0319 alone (the `Code` argument isn't reported
  missing); `guard x = v else return 0` is E0105 with the multi-line fix,
  its branch read as the line's statement (`Parser::one_line_guard`; it
  swallowed the method's `end`); a keyword followed directly by `:`
  names an argument (`Node.new(v: 1, next: &n)`, also starting a call
  without parentheses; `Parser::keyword_label`), since a field may be
  named `next`; and `Self` where a value goes in a method (`"#{Self}"`)
  is E0323 "`Self` is a type, not a value" with `type_info(Self).name`
  and, in generated code, computing the name in the macro (it was
  "undefined constant `Self`").
- A C compiler without C23 (one that rejects `-std=c23`, like gcc 13 or
  clang 17, or lacks `<stdckdint.h>` or `#embed`) is E0702 "the C compiler
  `cc` doesn't support C23", with the first line of its `--version`, the
  versions Wid needs and a help to choose another with `-cc:` or `WID_CC`,
  instead of a Wid bug (#46). `run_step` reads the compiler's output on any
  `-std=c23` step; working compilers are never run an extra time.
  `crates/wid_driver/tests/toolchain.rs` builds with fake compilers.
- A type over `2^61 - 1` bytes, or an array with more elements, is E0329
  where it is written, with the size it would take and the limit (#68): the
  layout panicked on overflow (`[1 << 61]I64`), and types that fit in a
  `u64` but not in C failed in the C compiler as E0702. Layout arithmetic
  saturates (`TypeTable::wide_layout` works in `u128`), and
  `TypeTable::oversize` measures a type's layout, which is C's (#100). The
  checker reports array, optional and tuple types as they are resolved (an
  array of a struct still being resolved, behind a pointer, once it is), the
  field or union variant that takes its type over (it becomes unknown),
  array literals, and generic calls whose instance would return one.
- A splice used as an assignment target (`=`, `+=`, `||=`, `a, #{n} = …`)
  that gives something that can't be assigned to (a number, string, `Bool`
  or `Type`, a capitalized `Symbol`, or `Code` that is a call or a literal)
  is E0911 at the call, marking the splice and naming what it holds (#69);
  it checked and then failed in the C compiler (`((void)0) = …`), or built
  for `||=`. `Splicer::splice_target` substitutes targets before the rest of
  the statement and applies the parser's E0107 shape rule to the result; a
  constant's name passes there (among declarations it declares the
  constant), and `Checker::place` reports one spliced into a method
  (`Checker::spliced_by`).
- A local that is only written through a field or an element (`a[0] = 1`,
  `m.hp = 9`, `a[0], a[1] = …`, a loop variable's field) is E0203, with a
  label on the first such write (#71); it counted as read, and gcc-15
  rejected the C ("set but not used"). `Checker::write_place` lowers `=`
  targets and leaves the variable unread when the place is in it
  (`ir::Expr::written_local`) and nothing else in the target names it;
  codegen counts reads the same way, so parameters and `_` locals written
  that way get `[[maybe_unused]]`. `tests/run/write_only_locals` covers the
  shapes that stay valid under gcc's strict flags. A by-value `for` binding
  over a place that is only written that way (`for s in ships` with
  `s.hp = 1`) is told the write changes a copy, with a `MaybeIncorrect` fix
  that binds by reference (`for &s in ships`, `Checker::loop_copies`)
  instead of the `_s` one.
- Writing into a `type_info` table is E0309 "`type_info` tables are
  read-only" (#81); it built and silently wrote to the `static const`
  tables (STATUS said it faulted). `Checker::place`, the assignments to
  `for &x` variables, `&` and `for &v` check `Checker::in_type_table`: the
  place is reached through a pointer to, or a slice of, a `TypeInfo`,
  `TypeInfoField` or `TypeInfoMember`, or through a pointer or slice read
  out of one. `&` of a whole record stays allowed, since writes through it
  are caught.
- Assigning to a name that a macro declared only in an operand
  (`zz += 1` or `zz ||= 1` after `false && decl(:zz)`) gets the same E0201
  note and help as reading it (#105): `Checker::place` asks
  `report_scoped_out` before the bare "undefined variable".
- A type alias whose value is undefined (`X = Foo`) is reported once, as
  E0201 where it is declared (#89); it was also E0314 where `X` was first
  used as a type. `decl_as_type` resolves a constant whose value isn't a
  type first, and a constant that failed is the unknown type, silently.
- A `comptime if` used as a value without an `else` is E0324 with the
  `if`'s fix, whether or not a branch is chosen, `elsif` chains included
  (#97); it emitted `void` variables the C compiler rejected (E0702).
  `comptime do … end` whose last statement is a `comptime if` without
  `else` discards it, like an `if`.
- A macro without arguments called without `()` expands in an array
  length (`[five]Int`, `[five + 1]Int`) and an enum member's value
  (`a = five`) as in a constant's (#78); those were E0327. The constant
  evaluator's `needs_interpreter` counts a bare name that names a macro as
  a call. A bare method name stays E0327 with the `comptime` fix.
- A constant shift whose value doesn't fit is E0311 whatever the amount
  (#84): `1 << 200` folded to 0, and `1 << 127` wrapped to a negative
  number. The message names the value as a power of two ("`1 << 200` is
  2^200, which doesn't fit in `Int`"), with the largest shift that fits
  and, when one exists, a type that holds it. A shift past the 128 bits
  constants fold in is reported where it is folded
  (`Checker::fold_const_for`, which knows the type the value is for) and
  folds to 0 so nothing reports it again; one that fits in them is
  checked where the value gets its type (`Checker::shift_overflows`).
  `a >> b` by 128 or more folds to 0 or -1.
- Types that would be empty are laid out as the generated C lays them out
  (#100): a struct without fields takes a byte (the emitter's `char
  unused_`), `[0]T` one element (`data[1]`; its length stays 0) and a union
  without variants a byte after its tag. `size_of`, `align_of`, field
  offsets and compile-time `type_info` said 0 bytes where the run-time
  tables, which C computes, said 1 or 8, and code that allocated by
  `size_of(Pair)` got too few bytes. `TypeTable::layout` itself matches C
  now (`aggregate_wide` gives an empty aggregate a byte), so the separate
  `c_layout` measure is gone; the interpreter compares `[0]T` arrays by
  their length, as the C does. `tests/run/zero_size_layout` prints the
  layouts next to the run-time tables.
- Test suite: `tests/run` (clang and gcc-16, or gcc-15 when gcc-16 is
  missing, strict flags), `tests/ui`
  (human output, or the JSON document with `-json-errors` in `NAME.flags`),
  `tests/test` (`wid test` reports), `tests/doc` (`wid doc` pages and
  errors), `tests/query` (`wid query` documents and errors) and every
  `core/` package's `_test.wid` files.
- `wid doc [package] [symbol]` (SPEC "Toolchain and CLI"): package
  overviews (the package doc, then every public declaration by section with
  the first paragraph of its doc), and pages for types (fields, promoted
  fields, enum members, union variants, and methods from the type itself,
  `include`d modules, `extend` blocks and `using` promotion, each group
  marked), methods, macros, constants, type aliases, overload sets, fields,
  enum members, builtin types (the methods extensions add) and `cimport`ed
  C declarations (C name, `file:line`, C doc). Symbol paths go through
  import names (`rl.draw_circle_v`), and one argument is the package or a
  symbol of `.`. `-json` (a stable shape `wid query` will reuse),
  `-private`, `-file`; `wid help doc`. Errors E0601 (package), E0602
  (symbol), E0603 (member) and E0604 (private) point at the argument in the
  command line, with did-you-mean fixes, a fix that names a member's type
  first, `-file` and `-private` fixes, and notes on how one argument was
  read. A package with errors is reported and still documented. Doxygen's
  `///<` and `/**<` markers no longer show in imported C docs.
- `wid query`, part 1 (SPEC "Toolchain and CLI"): the `wid_query` engine
  and `wid query outline`, `def <symbol>` and `methods <Type>`, with
  `-in:` (a directory, a file with `-file`, or a collection path). Always
  one JSON document on stdout (`query`, `symbol`, `package`, `results`),
  in `wid doc -json`'s item shape plus `span` (the whole declaration) and
  location ends; the outline nests fields, enum members and methods under
  their type in `children` and includes private declarations. `def` of an
  overload set gives the set and its members, of an import name a
  `package` item. `methods` groups by origin. Diagnostics go to stderr as
  JSON: E0601–E0603 with `wid doc`'s wording and fixes, one fix per type
  for a member named without its type, E0603 with a `def` fix for
  `methods` of a non-type; a package with errors is still answered.
  Unknown queries, `refs`/`type` and missing arguments are usage errors.
  `wid help query`. `wid doc`'s errors now prefer an exact member match
  (`Player.heal` for `heal`) over a similar package-level name, and a
  flag of another command says which command takes it.
- `wid query`, part 2 (SPEC "Toolchain and CLI"): `refs <symbol>`,
  `calls <symbol>` and `type <file:line:column>`, read from what the
  checker records while it checks for a query (`wid_sema::uses`, see
  "Conventions fixed so far"). `refs` lists every use with `location`,
  `kind` (`declaration`, `read`, `write`, `call`, `type`, `import`),
  `context` and, for code a macro generated, `via_macro` at the macro
  call, sorted by file, line and column; generic instances and inlined
  calls count once, `method(:f)` and `overload` lists read, promoted
  fields are their declaring struct's. `calls` keeps the `call` uses.
  `type` gives the innermost expression, name, binding, parameter,
  written type or declaration name at a position, with `location`, `span`,
  `type` (and `instances` for generic code), `kind` and `refers_to` (a
  `def` item, or a `local`/`parameter` item); without `-in:` it reads the
  package holding the file. E0605 reports a malformed position, an
  unknown file (did-you-mean), a line or column past the end, and a
  position with nothing recorded, pointing at the nearest code with a fix
  that asks about it; a symbol path given to `type` gets a fix to `def`.
- Linux and CI (`.github/workflows/ci.yml`, cached with sccache and
  rust-cache): `cargo fmt --check`; clippy and the full `cargo test` on
  Ubuntu 26.04 (clang-22, gcc-15, libclang 22, SDL3) and macOS 26 (Apple
  clang, gcc-15, SDL3, raylib), failing when libclang or a vendor library is
  missing instead of skipping; clippy and unit tests on Windows; `cargo check`
  at the MSRV (1.88). Ubuntu doesn't package raylib, so `vendor:raylib` and
  `examples/taste` are covered on macOS only. `cimport` keeps doc comments
  from system headers, which is where Linux installs libraries.
- Unknown-flag hints come from the flags the command takes: a double-dash
  flag is offered its one-dash form only when the command takes it,
  another command's flag says which command takes it, and `wid explain`
  (which takes none) says to run it with no code to list every code.
- The summary line after the diagnostics is worded per command
  (`render_all_with`): `could not compile` stays with `build`, `run`,
  `check` and `test`; `cimport` could not import the header, and `wid doc`
  could not write the documentation, or warns that the page it printed may
  be incomplete. The errdocs scripts accept every wording. `wid query`
  prints its diagnostics as JSON only (`refs`, `calls` and `type`
  included), so it has no summary line.
- `-file` errors point into the command line (`wid check main.wid -file`)
  for every command: no file named (E0206, or E0601 for `wid doc -file`,
  as for `wid query`) offers the only `.wid` file of the directory and
  dropping `-file`; a missing file offers a similar `.wid` file (`main` for
  `main.wid` too) or lists those there; a directory offers dropping
  `-file`. `Options::command` names the command for the message.
- `core` docs: `core:builtin` has a package doc describing the prelude
  (`builtin.wid`), `Arena`, `Pool`, `Tracker` and `Builder` are described
  above their declarations instead of in file headers, and every public
  `core` declaration, field and enum member has a doc comment, checked by
  `wid_driver/tests/core_docs.rs`. `OS` and `ARCH` have a doc each, and the
  prelude declares them when it is the package itself
  (`wid doc core:builtin OS`).

## Next

Everything before macros is done (see "Done"). This is the work queue for
the orchestrator (`docs/ORCHESTRATOR.md`), together with the open GitHub
issues. Each item is one PR unless it says otherwise. Items 2–4 depend only on `main` and can run in parallel with the
macro stack.

1. **Macros and `type_info`** (`wid/macros-*`, about three stacked PRs):
   - lexer, parser and AST for `quote` and splices (**landed**);
   - expansion, hygiene and errors in expressions and statements
     (**landed**; see "Conventions fixed so far");
   - declaration-level expansion (**landed**; see "Conventions fixed so
     far" and "Done");
   - `type_info` (landed separately; see "Done").

   **No accessor macros (decided):** Wid has no `attr_reader`,
   `attr_writer` or `attr_accessor`. Fields are always public and code
   reads and writes them directly (SPEC "Data and behavior"), and a method
   can't share a field's name (E0202). `attr_reader :hp` stays an undefined
   macro (E0201) with a note saying so (`undefined_macro` in
   `check/decl_macros.rs`, `tests/ui/accessor_macros`); a macro of the
   program's own with that name works like any other
   (`tests/run/macro_ruby_names`).

   The design below is settled and written into SPEC.md ("Compile-time"),
   and all of it has landed; the notes stay as a map of the implementation:
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
       (`counter :kills`) into an `ItemKind::MacroCall`.
     - A splice is `ExprKind::Splice(i)` in an expression,
       `TypeKind::Splice(i)` in a type, and `ItemKind::Splice(i)` alone on a
       line in a type body (in an enum body it may be `Symbol`s, i.e.
       members). In every name position (method, field, parameter, local,
       block-parameter, loop-variable, type and enum-member names, `x.#{m}`,
       `@#{f}`, `:#{s}`, named arguments, `#{name}(args)`,
       `@#{f}(args)`) it is an
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
   - **`type_info(x)` / `type_info(T)`:** landed separately, ahead of the
     macros (see "Done" and SPEC "Compile-time").
   - **Settled (in SPEC.md, "Compile-time"):** errors in generated code
     point at the `quote` line and every macro call that led to it,
     innermost first; variadic `*names: T` parameters (macros only, last,
     no default); symbol splices `:#{name}`; qualified calls `pkg.name`
     and `private macro def`; definition-site resolution of the quote's own
     names; budgets (64 deep, 65,536 per build, E0903). The implementation
     notes are under "Expansion" above.
2. **`wid query`** (`wid/query`, two stacked PRs): **landed** (see "Done"
   and "Conventions fixed so far"; its limits are under "Known gaps").
3. **`wid fmt`** (`wid/fmt`). A canonical formatter. It must be
   idempotent, and parse → format → parse must give the same AST for every
   file in `tests/`, `core/`, `vendor/` and `examples/`. It keeps comments
   and supports `-check`. Start from `wid_syntax::print`, which renders
   types, expressions and declaration lines.
4. **`wid lsp`** (`crates/wid_lsp`, `wid/lsp`, stacked on 2 and 3).
   Diagnostics, hover, go-to-definition, completion, formatting and rename,
   all on top of the query engine.
5. `vendor:cimgui`: vendor cimgui with the Dear ImGui sources, compiled as
   C++ package files (the driver already builds `.cpp` files and links with
   the C++ compiler), plus a raylib or SDL3 backend. Needs a decision on
   shipping the C++ sources versus requiring a system cimgui.
6. **Cross-target builds** (`wid/targets`). Lift E0709. Make
   `-target:os_arch` build through clang `--target` with a sysroot. Port
   `core:os`/`core:c` to Windows (LLP64) and make C type sizes
   target-driven.
7. **SPEC conformance audit** (one agent, a report and no code). List every
   SPEC.md claim that is unimplemented or behaves differently: CLI flags such
   as `-vet`, `-sanitize:address`, the `-o:` levels and `-collection:`;
   `#line` in `-debug`; the prelude list; and so on. Queue each item here.
8. **Known gaps** below: one small PR each, in any order.
9. **Bug hunt after every large feature.** One agent probes with
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
- `type_info`: at compile time the tables use Wid's layouts, which differ
  from C's for a cimported C union (whose fields all start at 0 in C), and
  `Error`'s members are the error symbols seen so far. Proc tables don't say
  whether a proc is `@[c]`. Writes into the tables are E0309, except through
  a `[]^TypeInfo` copied out of a table (`vs = t.variants`, or one passed to
  a method) or a pointer converted with `.to`: those are undefined
  behaviour at run time (the tables are `static const` and the C casts
  `const` away; the write may be ignored or fault) and succeed at compile
  time.
- Macros: a `quote` inside a splice must fit on one line, because newlines
  are suppressed inside splices (`#{if a then quote do x end else quote do
  end end}` works; a multi-line `quote` there doesn't). Code spliced from
  the call site into a `comptime` inside a `quote` resolves names where the
  macro is defined, not at the call site. A name spliced from a computed
  `Symbol` (not a symbol argument) points at the whole macro call, which
  is where a "did you mean" fix would apply. "Did you mean" suggestions in
  generated code can name the caller's locals, which the code can't see.
  E0304 tells a symbol literal from a `Symbol` value by its source text. A
  `Code` parameter's default can't be a `quote`. A macro run is repeated
  for each generic instance that contains the call. Choosing among an
  `overload` set's members lowers an argument that some member needs a
  value for, so such an argument must check where the call is, even when
  the macro chosen takes it as `Code`.
- Macros among declarations: calls expand strictly in source order, once
  each, so a call can't use a macro or a declaration that a later call
  generates (it is undefined), and a macro runs, with the helpers it calls
  lowered, before the declarations later calls generate exist. In an
  `enum` body a macro without arguments needs `()`, since a name alone is
  a member (one that a macro also has is E0914). A generic struct's body can't call a macro that uses `Self`
  (E0209): its `Self.fields` would hold placeholder types and no layout.
  `Self.methods` in a type-body macro lists the methods collected so far.
  Generated declarations whose names come from computed symbols point at
  the whole call in messages (E0202, E0317).
- `wid doc`: the AST drops the parameter names of `proc` types, so a C
  callback shows as `@[c] proc(RawPtr?, C.int)` (`wid fmt` needs them
  too). Expressions that hold statements (`if`, `case`, blocks,
  `comptime do`) print as `if … end` in declaration lines. Only named
  builtin types can be asked for (`String`, `Int`); extensions of patterns
  (`[]$T`, `[2]F32`) show in the overview only, and a type alias of a
  builtin type (`Vec2 = [2]F32`) lists no methods. Extensions in packages
  the documented package doesn't load are not listed. `cimport`
  declarations have no Wid location (their source is generated), and the C
  enum a constant came from isn't named.
- `wid query`: it shares `wid doc`'s limits on builtin types and
  patterns: `methods` lists only what extensions add, and none for an
  alias of a builtin type (`Vec2 = [2]F32`). `refs`, `calls` and `type`
  see only code the checker lowers: a generic method that nothing
  instantiates (an `extend` or `module` method is generic over `Self`, so
  `wid query type` in `core:strings` finds little), a field default that
  no `T.new` uses, and methods of other packages that the package doesn't
  reach have no uses or types.
  A constant's folded initializer (`MAX = 3`) records the constants it
  reads but not its own types. A builtin type's uses are only where it is
  written as a type. An operator method's use is at the whole operation
  (`a + b`) or, for `+=`, at its target, since the parser keeps no span
  for the operator. `refs` has no way to name a local (`type` covers
  them), and `wid query calls` has no reverse view (what a method calls).
  The LSP will want positions in UTF-16 (`SourceFile::offset_of_utf16`);
  `type` takes characters. A declaration a macro generates under a name from its
  arguments (`counter :kills`) has the macro call as its `span`. A `using`
  origin has no location. The fixes in its diagnostics edit the command
  line as `wid query` rebuilds it (query, argument, `-in:`, `-file`), not
  the order the flags were typed in.
- `vendor:miniaudio` built with GCC on macOS has no CoreAudio backend: GCC
  can't parse the block syntax in Apple's headers (`miniaudio.c` sets
  `MA_NO_COREAUDIO` there).

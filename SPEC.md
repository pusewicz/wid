# Wid — Language Spec (draft 0.1)

Wid reads like Ruby but is statically and strongly typed, uses manual memory
management, and compiles to C23. Its semantics, memory model and CLI come from
Odin. It is built for 2D games but works as a general-purpose systems language,
and it is meant to be easy for both people and LLMs to read, write and debug.
It aims for programmer happiness: one obvious way to do things, no hidden costs,
and error messages that teach.

## Taste

```ruby
import "vendor:raylib", as: :rl

Vec2 = [2]F32

struct Ball
  pos: Vec2
  vel: Vec2 = [120.0, 80.0]
  radius: F32 = 8.0

  def update(dt: F32)
    @pos += @vel * dt
    @vel.y = -@vel.y unless @pos.y.between?(0.0, 450.0)
  end
end

def main
  rl.init_window(800, 450, "bounce")
  defer rl.close_window

  balls = [dynamic]Ball.new
  defer free(balls)
  balls << Ball.new(pos: [400.0, 225.0])

  until rl.window_should_close
    balls.each { |&b| b.update(rl.get_frame_time) }
    rl.begin_drawing
    rl.clear_background(rl.BLACK)
    for b in balls
      rl.draw_circle_v(b.pos, b.radius, rl.RED)
    end
    rl.end_drawing
    free_all(context.temp_allocator)
  end
end
```

## Syntax

- Wid keeps Ruby's surface: `def … end`, endless `def f = expr`,
  `if/unless/elsif`, postfix `if`/`unless`, `while/until/loop`, `case/when`,
  implicit return, `#{}` interpolation, ranges `0..n`/`0...n`, `# ` comments
  and no semicolons (outside a string, `#{` starts a macro splice). `?`
  methods must return `Bool`. Source files use the `.wid` extension.
- **Blocks** can be written `do |x| … end` or `{ |x| … }`. A `{` right after a
  call opens a block; anywhere else, `{}` is the zero-value literal. A type
  never takes a block: `Vec2{…}` and `Vec2 {…}` are struct literal syntax,
  which Wid doesn't have (E0113; see structs).
- **Declarations.** The first assignment declares a variable: `x = 1`,
  `speed: F32 = 120.0`, or `grid: [4][4]U8`. Variables are zero-initialized,
  and `= ---` opts out. As in Ruby, a name that starts with an uppercase letter
  is a compile-time constant: `MAX = 256`, `Vec2 = [2]F32`. There are no
  package-level variables, and a constant always has a value: `= ---` on one
  is an error (E0323) whose fixes write a value or `{}`, except on an
  `@[extern]` constant, whose value C defines. Reading an
  undeclared name is an error with a "did you mean", and so is a local
  variable that is assigned but never read (prefix it with `_` to keep it).
  Writing a field or an element of a variable (`ship.hp = 9`,
  `cells[0] = 1`) doesn't read it; writing through a pointer, a slice or a
  dynamic array reads the variable that holds it, and so does a compound
  assignment like `cells[0] += 1`. Unused parameters are allowed.
- **Calls.** Parentheses are optional for zero-argument calls and for the
  outermost call of a statement (`puts "hi"`). Any parameter can be passed by
  name. Defaults are written `hp: Int = 100`. A field may be named by most
  keywords (`next: ^Node?`), and, as in Ruby, a keyword followed directly
  by `:` in an argument list names an argument: `Node.new(next: n)`.
- **Symbols.** `:north` is a compile-time name. Where an enum is expected it
  selects a member, like Odin's `.North`; anywhere else it is a plain
  identifier (in macros and `cimport` options, for example).
- **Attributes.** Written `@[export("on_audio"), c]`. Methods take `c`,
  `export`, `extern`, `no_bounds_check` and `test`; statements take
  `no_bounds_check`. C declarations take `extern`: a struct defined by C
  (`@[extern("struct Foo"), size(8), align(4)]`, or `opaque` when only C
  knows its fields), a field with a different C name, and a constant C reads
  by name (`@[extern("RED")] RED: Color = ---`). Other declarations take
  none. Inside a method, `@name` means `self.name`, and `@name(args)` calls
  the proc that field holds; methods are called by name.
- **Smaller syntax rules:**
  - `def self.name` declares a type-level function (`Vec2.zero`).
  - `def -` with no parameters is unary minus.
  - `private def …` hides a declaration outside its package, or outside its
    type for methods: a private method can only be called from methods of the
    same type, including ones mixed in with `include` or added by `extend`.
    Fields are always public, so `private` can't be written on a field,
    `using` ones and those in a `quote` included (E0105). Nor on an enum
    member, which is public too, or where nothing is declared by name: an
    `import` or `cimport` (`as:` keeps its names in the file), an
    `include`, an `extend`, a `comptime if` (write it on the declarations
    in the branches), a splice standing alone among declarations, or a
    statement (E0105).
  - `loop do … end` loops forever.
  - `for x in xs`, `for &x in xs` and `for x, i in xs` iterate.
  - `^` only builds pointer types (`^T`) and dereferences (`p^`). Bitwise xor
    is `~`, as in Odin.
  - `&x` takes an address.
  - `{…}` literals must be empty.
  - Enum members are lowercase. Inside an `enum`, `struct`, `enum` or
    `union` standing alone (followed by a newline, `,` or `= value`) names a
    member, as in the prelude's `TypeKind`; `TypeKind.struct` and `:struct`
    select it.
  - `map`, `proc`, `block`, `distinct`, `matrix` and `dynamic` are keywords only
    where a type is expected.
  - Parentheses group a type, as in `(proc(Int) -> Int)?`, an optional proc
    (`proc(Int) -> Int?` returns an optional). A constant whose value is a
    name in parentheses is whatever the name is: `X = (Int)` is a type
    alias like `X = Int`, and `Y = (NINE)` a value. One whose value starts
    with `(` and something only a type starts with (`proc`, `distinct`,
    `^`, `[]`, `@[`, …) is a type, and is reported as one when malformed.
  - A line that ends with an operator or `,`, or a next line that starts with
    `.method`, continues the statement. A declaration or an `end` starting
    the next line never does: `X = 1 +` followed by `def main` is missing
    its operand. Inside `( )` and `[ ]`, a line end after a complete
    expression may only lead to a `,` or the closer: anything else on the
    next line means the bracket was left open at the end of the line (a
    value indented under the list's first line is read as its next item,
    with the `,` missing), as does a declaration or an `end` inside a
    `{ }` block. The same holds for a method's or proc's parameters and a
    type's arguments, array length and map key. After a `,`, a line that
    can't start an item also means the list was left open: in a parameter
    list, one that doesn't start with a parameter; in a list whose items
    are never named (an array, an index, a type's arguments), a `name:`
    declaration. A method whose parameter list was left open before a
    declaration ends with that line. A named argument's value may go on
    the next line only indented deeper than the call's own line.
  - `x ? a : b` needs spaces around `?`, because `x?` is a predicate name.
    So a `?` written right after a type's name, its closing `)` or a
    splice (`Int?`, `rl.Color?`, `Pool(Ball, 64)?`,
    `(proc(Int) -> Int)?`, `#{t}?` in a `quote`) ends that type, written
    in place, unless a conditional's `:` follows it:
    `t = Int?` is the type `Int?` (a value goes there, E0323). In `C.int?`
    the `?` is read as part of the name, as in a predicate's, but `core:c`
    has no member `int?`: `C.int?` is the optional C type wherever it is
    written, like `C.int`.
  - A call argument is a type when no expression reads the same way and
    `,` or `)` follows it (in a call without parentheses, also the end of
    the statement or an `if`/`unless` modifier, as in `n = size_of Int?`):
    a type that ends in its own `?` (`Int?`, `rl.Color?`,
    `Pool(Ball, 64)?`, `(proc(Int) -> Int)?`, unlike the name `empty?`), or
    a `proc(…) -> R` or
    `@[c] proc(…)` type. `size_of(T)` and `align_of(T)`
    (a type's size and alignment in bytes) and `type_info(T)` expect a
    type, so there `proc`, `block` and `distinct` start one too (unless a
    local has that name), as do `$T` and a tuple type `(A, B)`: any type
    can be written in place, like `size_of(Int?)` or `align_of(proc(Int))`.
    Other types already read as expressions (`Vec2`, `[]Int`, `C.int`,
    `Pool(Ball, 64)`).

## Types

- **Primitives:** `Int`/`UInt` (pointer-sized), `I8…I64`, `U8…U64`, `F32`,
  `F64`, `Bool`, `Rune`, `String` (an immutable UTF-8 ptr+len view), `CString`,
  `RawPtr`, `TypeId` and `Any`. These names, `Error` and `Never` can't be
  declared again. `Context`, `Allocator`, `AllocMode`, `Location` and
  `Logger` are predeclared library types: a package may declare its own type
  under one of these names (a C library's `stbrp_context` becomes `Context`),
  and inside that package the name means its own type.
- **Constructors (Odin):** `^T`, `[^]T`, `[N]T`, `[]T`, `[dynamic]T`,
  `map[K]V`, `proc(A) -> R`, `distinct T` and `matrix[R, C]T`. Small numeric
  arrays support element-wise math and swizzles (`v.xy`, `c.rgb`).
- **Size limit:** a type can take at most `2^61 - 1` bytes, and an array can
  have at most `2^61 - 1` elements. Clang rejects larger arrays, so every
  type within the limit compiles with every supported C compiler. A larger
  type is an error (E0329) where it is written: an array, optional or tuple
  type, the field or union variant that takes its struct or union over the
  limit, an array literal, or a call of a generic method whose instance
  would return one.
- **Layout** is the generated C's: `size_of`, `align_of`, field offsets,
  `T.size` and `T.fields` at compile time and `type_info` at run time all
  report the layout the program uses. C has no empty types, so a struct
  without fields takes 1 byte, `[0]T` takes the space of one element
  (`size_of([0]Int)` is 8) while its length stays 0 for everything else,
  and a union without variants is laid out as its C counterpart, a tag and
  one byte.
- **Distinct types** are declared as constants, `Meters = distinct F64`, and
  each declaration is a new type with the base type's representation and
  operators. Untyped literals convert to it; typed values convert with `.to`
  in either direction (`f.to(Meters)`, `m.to(F64)`), never implicitly.
- **Optionals:** `T?`, whose empty value is `nil`. A `T` converts to `T?`
  implicitly; the reverse needs an unwrap. Pointers can't be nil
  unless written `^T?` (the `?` after `^T` or `[^]T` makes the pointer
  nil-able; `^(T?)` points at an optional). Use `x || default` to unwrap with a fallback, `x&.f` to
  chain, `if v = maybe … end` to bind, and `while v = maybe … end` to loop
  until it is nil. `a ||= b` assigns when `a` is nil or false (on a map entry:
  when the key is missing) and `a &&= b` when it holds a value or is true;
  `b` is only evaluated then. Checks narrow a local's type for the
  code they protect: `if x`, `unless x.nil?`, `x != nil`, `return … if x.nil?`
  and `guard`. Assigning to the local ends the narrowing, and loops forget it
  for locals they assign. Fields don't narrow; copy them into a local. Only
  `nil` and `false` are falsy; a condition must be a `Bool` or a nil-able
  value.
- **Conversions** are always explicit: `n.to(F32)`, `to_i`, `to_f`, `to_s`.
  `.to(T)` also converts between pointer types (`^T`, `[^]T`, `RawPtr`,
  `CString`), between a pointer and its address (`Int`/`UInt`), and between the
  byte views `String`, `[]U8` (also from `[N]U8` and `[dynamic]U8`) and
  `CString` (to `String` only). These conversions never copy. A conversion
  from a nil-able pointer must target a nil-able one (`raw.to(^Node?)`), so
  nil stays visible. On an array, slice or dynamic array of `T`,
  `.to([^]T)` is a pointer to its first element, for C calls; it doesn't copy
  and is valid while the container is. The one implicit conversion is any
  pointer to `RawPtr` or `RawPtr?`.
- **`[^]T` arithmetic:** `p + n` and `p - n` move by whole elements, and
  `p - q` counts the elements between two of them.
- **Integer operations** wrap on overflow; `-debug` builds panic on `+`, `-`
  and `*` that overflow. Division and remainder by zero always panic,
  `MIN / -1` is `MIN` and `MIN % -1` is 0, and `%` truncates toward zero.
  A shift by the type's width or more shifts every bit out: the result is 0,
  or -1 for `>>` of a negative value. A negative shift amount is an error
  when it is a constant (E0311, whose fix shifts the other way: `x >> 1`
  for `x << -1`) and panics in `-debug` builds and at compile time.
  Otherwise the amount is read as unsigned, so a negative one is past the
  width and shifts every bit out.
- **`Never`** is the return type of methods that never return, like
  `os.exit`. Their body must end in `panic`, an endless loop or another
  `-> Never` call, and a call to one ends the code path, so it satisfies
  `guard … else`.
- **`caller_location`** is a `Location` (`file`, `line`, `column`, `proc`).
  As a parameter default, `loc: Location = caller_location`, it is the
  location of the call, which is how `t.expect` and allocators report where
  something happened. `AllocMode` and `Location` are builtin types, so
  allocators can be written in Wid: an `Allocator` is a C-ABI
  `proc(RawPtr, AllocMode, Int, Int, RawPtr, Int, Location) -> RawPtr` and a
  data pointer.
- **Generics:** a `$T` in a parameter introduces a type parameter
  (`def max(a: $T, b: T) -> T`). Generic structs are written
  `struct Pool($T, $N: Int)` and used as `Pool(Ball, 64)`, or as
  `geo.Pool(Ball, 64)` from another package. An instance is a type like any
  other: `Pool(Ball, 64).new`, `.size` and `def self.` methods work on it,
  while `Pool.new` without arguments is an error. A value
  parameter is declared `$N: Int` (any other type is an error at the
  declaration) and takes any constant integer: a literal, a named
  constant (`Pool(Ball, MAX)`), constant arithmetic (`Pool(Ball, MAX * 2)`)
  or a `comptime` result; a name there is a constant unless it names a type.
  A negative argument for a parameter that is a field's array length
  (`cells: [N]U8`) is E0301 at the argument. Only generic structs take
  value parameters: a method's `xs: [$N]Int` is
  E0105, and the method takes a slice, `xs: []Int`, with `xs.size` as its
  length, instead.
  - In the struct's fields and methods and in an `extend` of it, a value
    parameter is a constant of the instance: in `Pool(Ball, 64)`, `N` is
    `64` wherever a constant integer fits (`def cap -> Int = N`,
    `buf: [N + 1]U8`, `comptime N * 2`, `Pool(T, N * 2).new`, `N.times`),
    in method signatures too (`def all -> [N]T`,
    `def bigger -> Pool(T, N + 1)`), where it hides a package constant of
    the same name. Like a constant declared without a type (`MAX = 64`), it
    takes its type from where it is used and is an `Int` otherwise. It
    can't be assigned, and it is not a type (`x: N` is an error). Methods of
    an included module don't see the struct's generic parameters.
  - Type arguments are inferred from the arguments. A `[N]T` or `[dynamic]T`
    argument matches a `[]$T` parameter.
  - A type parameter is a type, not a value: in generic code, `T` alone
    where a value is expected is an error, even when a package constant is
    named `T`. `size_of(T)` and `type_info(T)` are values about it, and a
    `t: Type` parameter takes it.
  - Generic code is checked once per set of type arguments, like a template:
    using `<` on a `T` is fine as long as every `T` it is used with has `<`.
    Generic methods that are never called are not checked.
- A minus sign written directly before a number literal belongs to it, so
  `-7.abs` is `7`, as in Ruby.
- **Literals:** `{}` is the zero value of the expected type, and `[1, 2, 3]` is
  a fixed array (or a slice of a temporary array where `[]T` is expected).
- **Collections:**
  - `xs[i]` is bounds-checked. `xs[a...b]`, `xs[a..b]`, `xs[a..]` and
    `xs[..b]` make slices.
  - Arrays and dynamic arrays convert to `[]T` where a slice is expected.
  - Arrays, slices and dynamic arrays share `.size`, `.empty?`, `.first` and
    `.last`. The last two return `T?`.
  - Dynamic arrays also have `<<` and `.push`, `.pop` (returns `T?`),
    `.insert(i, x)`, `.delete_at(i)`, `.reserve(n)`, `.clear` and
    `.capacity`.
  - `[dynamic]T.new` and `map[K]V.new` start empty and grow with the
    allocator that was `context.allocator` at creation, or with `allocator:`.
    A zero-valued container adopts `context.allocator` when it first grows.
  - As in Odin, growing a dynamic array or map while iterating it, or while
    holding a pointer into it, invalidates the iteration and the pointer.
  - Map keys are numbers, runes, bools, enums, pointers or strings. `m[k]`
    returns `V?`, `m[k] = v` stores, and `m[k] += 1` starts from zero for a
    missing key. Maps also have `.has_key?`, `.delete` and `.size`.
- **Strings** are byte views:
  - `.size` counts bytes, `s[i]` is a `U8`, and `s[a...b]` slices bytes.
  - `for r in s` iterates runes, and `for r, i in s` adds the byte offset.
  - Strings compare with `==` and `<`. They also have `.include?`,
    `.index` (returns `Int?`), `.starts_with?`, `.ends_with?`, `.empty?` and
    `.to_cstr`.
- **Fixed numeric arrays** support element-wise `+ - * / %` with each other or
  with a scalar, unary `-`, and swizzles (`v.x`, `v.yx`, `c.rgb`).
- **Matrices:** `matrix[R, C]T` holds numbers in column-major order (as in
  Odin and GLSL), with 1 to 16 rows and columns. A literal lists the elements
  row by row: `m: matrix[2, 2]F32 = [1.0, 2.0, 3.0, 4.0]`. `m[row, col]`
  reads and writes an element. `+` and `-` work element-wise, `*` and `/` by a
  scalar scale every element, and `*` between a matrix and a matrix or vector
  is the matrix product: `matrix[R, K] * matrix[K, C]`, `matrix[R, C] * [C]T`
  (a column) and `[R]T * matrix[R, C]` (a row). They also have `==`,
  `.transpose`, `.row(i)`, `.column(j)` and `matrix[N, N]T.identity`.
- **Dynamic arrays** also have `.concat(xs)`, which appends a slice, and
  `.resize(n)`, which zero-fills new elements.

## Data and behavior

- A `struct` has fields and methods, and its values are copied. Methods get
  `self` as `^Self`, and the call site takes the address for you. `T.new(…)`
  builds a value and **never allocates**. It takes fields by position (in
  declaration order) or by name; missing fields use their declared default or
  zero. A default is checked against its field's type whether or not a `new`
  takes it, and runs at each `new` that does. `new` is reserved for this, so
  name custom constructors otherwise (`def self.create`). There is no struct
  literal syntax: a type followed
  directly by `{` (`Vec2{x: 1.0}`, `geo.Vec2{1.0, 2.0}`, `Pool(Int, 4){}`,
  as in Odin, Go, Rust or Zig) is E0113, with a fix that writes the `new`
  call, and is read as that call. On a type without `new`, the fix fits the
  type: a number is written as itself (`w = Int{1}` becomes `w: Int = 1`,
  another use `1.to(Int)`), and an enum, union, distinct or other type gets
  a help on how to build its value; the value has the type and adds no
  other error. A constant never takes a block, so a `{`
  after a space on the constant's line is the same mistake (`Foo { a: 1 }`,
  `geo.Vec2 { … }`), unless it opens a block's `|x|`; after a generic
  instance, a spaced `{` (`Pool(Int, 4) { … }`) is a call's block. `==`
  compares structs field by field when every field is comparable; define
  `==` to customize it.
- **Fields are always public**, as in Odin: code reads and writes them
  directly (`hero.hp`, `hero.hp = 3`, `@hp` inside a method). There are no
  getter or setter methods and no accessor macros like Ruby's `attr_reader`,
  and a method can't share a field's name (E0202).
- **No implicit overloading.** Two defs can't share a name. To overload, you
  declare an explicit set, like an Odin proc group:
  `overload :clamp, :clamp_f32, :clamp_int`. A call picks the member with the
  most exact parameter matches. Typed values never convert between number
  types; an untyped literal matches its default type exactly and converts to
  other number types, so `clamp(5)` picks `clamp_int` and `clamp(0.5)` picks
  `clamp_f32`. If none fits, or several fit equally well, the error lists
  every member, and offers `.to(T)` only where that conversion exists and
  makes a member fit. Members can't take blocks or `$` type parameters, and no two
  may take the same parameter types. A member may be a macro, which expands
  when a call chooses it (see Compile-time).
- **Operators are methods**, because math-heavy game code needs them. You can
  define `+ - * / %`, unary `-`, `==`, `<=>` (which gives you `<`, `<=`, `>`
  and `>=`), `[]` and `[]=`, and `+=`-style forms are derived automatically.
  When a type needs more than one right-hand type, use an explicit set, as
  with any other overload:

  ```ruby
  struct Transform
    origin: Vec2
    basis: matrix[2, 2]F32

    def apply(p: Vec2) -> Vec2 = @basis * p + @origin
    def compose(t: Transform) -> Transform =
      Transform.new(apply(t.origin), @basis * t.basis)
    overload :*, :apply, :compose      # xf * point, xf * xf
  end
  ```

  If the left operand isn't the struct (as in `F32 * Transform`), define the
  operator at package level: `def *(s: F32, t: Transform) -> Transform`, or
  name several such functions and group them with `overload :*, …`. An
  untyped literal operand takes its type from the operator's other parameter,
  so `2.0 * xf` works.
- There is no inheritance. `using base: Entity` (or `using base: ^Entity`)
  promotes another struct's fields and methods into this one, so `player.hp`
  and `@hp` reach `player.base.hp`. Its methods include those its modules
  mix in and those `extend` blocks add, which are methods of the type too:
  `heal(1)` in a method of the struct and `player.heal(1)` reach
  `player.base.heal(1)`. Promotion is transitive. The struct's own members
  win over promoted ones, and a name that two `using` fields provide is an
  error until the access names the field.
- `module Name … end` holds methods and constants (no fields). `include Name`
  in a struct, enum, module or `extend` mixes its methods in at compile time.
  Module methods are generic over `Self`, checked for each type that includes
  them, and `@field` reads that type's fields. A module that nothing includes
  is never checked.
- `extend T1, T2 … end` adds methods to existing types. That includes builtin
  types and patterns such as `[]$T`. Extensions apply program-wide, and two
  extensions that define the same method for a type are an error. The
  methods of an `extend` of concrete types (`extend P`, `extend Int, F64`)
  are checked for each type it lists, whether or not anything calls them,
  like a struct's own; those of an `extend` over a pattern are generic,
  checked for each type a call uses them with. A method
  call looks for a field, then the type's own methods, then included modules,
  then members promoted by `using`, then builtin methods (`size`, `push`,
  `to_s`, …), then extensions. For arrays and dynamic arrays it also looks
  for extensions of `[]T`.
- **The prelude** (`core:builtin`, visible everywhere without an import)
  extends:
  - slices with `each`, `each_with_index`, `reverse_each`, `count`, `any?`,
    `all?`, `none?`, `find`, `find_index`, `include?`, `index`, `sum`, `min`,
    `max`, `reverse!`, `sort!` and `sort_by!`;
  - numbers with `between?`, `clamp`, `min`, `max`, `zero?`, `abs`, `even?`,
  `odd?`, `times` and `upto`. Inside a method, `self` is the receiver itself (passed by
  pointer, so changes are visible to the caller), and a bare method name calls
  it on `self`.
- `enum Dir : U8 … end` declares an enum (backed by `Int` when no type is
  given). Members count up from 0 unless written `name = value`; `Dir.north`
  and `:north` both name one, and `.to_i` gives its value. `union Shape =
  Circle | Rect` declares a tagged union, matched with
  `case s when Circle then s.radius`. `s` is narrowed in each branch.
- `case` over an enum or union must handle every member unless it has an
  `else`; `when` takes several values, and ranges such as `1..9`. Patterns
  are tested in order, and testing stops at the first that matches. A
  `case` used as a value needs an `else` unless it is exhaustive. An exhaustive
  `case` without `else` panics if the value is a nil union or not a valid
  enum member.
- Polymorphism is explicit: use tagged unions, or structs of `proc` fields.
  There are no hidden vtables.
- **Blocks.** A method declares `&blk: block(T) -> R` and calls the block with
  `yield`. Blocks are **inlined** into the caller, so they capture locals freely
  and allocate nothing; `break`, `next` and `return` work as in Ruby. A block
  can't be stored. `|&x|` and `for &x in xs` bind by reference; otherwise
  bindings are by value.
  - `block` alone takes and returns nothing.
  - `next v` gives the block's value and `break v` gives the method's.
  - A method that takes a block cannot call itself, because it is inlined.
  - Passing a block to a method without one, or calling a block method
    without one, is an error.
- **Procs.** Storable callbacks are non-capturing procs, written
  `->(x: Int) -> Int { x * 2 }` or `method(:on_hit)` (a package or
  type-level method). A proc is called like a method, `f(x)` or `f.call(x)`,
  including through a field (`button.on_click(e)`). Procs are nil-able. If a
  proc captures a local, the error suggests a parameter or a block instead.

## Errors

```ruby
def load_level(path: String) -> (Level, Error)
  guard data = os.read_file(path) else |err|
    return {}, err
  end

  guard spawn = data.find_spawn else   # Vec2?
    return {}, :no_spawn
  end

  lives = data.parse_int("lives") || 3
  return Level.new(data, spawn, lives), nil
end
```

- Wid has no exceptions. A fallible function returns multiple values, with the
  error last.
- `Error` is a built-in enum made of every error symbol the program uses, like
  Zig's `anyerror`: writing `:name` where an `Error` is expected (returning it,
  comparing with it, matching it in `case`) adds it to the set. A union that
  can be nil also works as the error value. `nil` means success.
- `guard a, b = f() else |err| … end` binds the non-error values, or unwraps a
  `T?`, for the rest of the scope. The `else` branch must leave the scope with
  `return`, `break`, `next` or `panic`. Like any block, it starts on the line
  after `else` (or `|err|`) and ends with `end`: a branch written on the
  guard's own line (`guard x = v else return 0`) is E0105, whose fix moves it
  to its own line. There are no shorthand propagation operators such as
  `or_return` or `?`. `guard cond else … end` also works with a plain `Bool`
  condition.
- By convention a failing function returns `{}, err` if its type is
  `(T, Error)` and `nil` if its type is `T?`. Silently dropping an `Error` is a
  compile error; discard one explicitly with `_`.
- Use `assert` and `panic` for bugs and `unreachable` for impossible paths.

## Memory

- Memory is managed manually, as in Odin. Every Wid procedure gets an implicit
  `context` carrying `allocator`, `temp_allocator`, `logger` and `user_data`.
  Setting `context.allocator = arena` lasts until the end of the scope.
- **Allocation is always visible.** You allocate with `alloc(T)` (returns
  `^T`) or `alloc([]T, n)`, and release with `free(x)`; each takes an optional
  `allocator:`. A container grows using the allocator it was created with.
  String interpolation and `to_s` use `context.temp_allocator`; reset it once
  per frame with `free_all`. `.new`, `{}` and array literals never allocate.
- Cleanup uses `defer`, which runs at scope exit. Wid has no `ensure`.
- `core:mem` ships the heap and temp allocators, a growing `Arena`, a
  fixed-chunk `Pool` and a `Tracker`, plus `copy`, `set`, `zero` and
  `compare`. In `-debug` builds the heap is wrapped in a tracking allocator
  that reports leaks with their allocation sites when `main` returns, and a
  double free (or a free with the wrong allocator) panics with both sites.
- Functions in `core` that build new strings or slices (`s.split`,
  `s.upcase`, `fmt.int`, …) take `allocator:`, which defaults to
  `context.temp_allocator` like interpolation does; pass another allocator to
  keep the result. Functions that read data (`os.read_file`, `os.read_line`)
  default to `context.allocator`. Views (`s.strip`, `os.args`) never allocate.
- Checks are on by default: bounds, nil, and integer overflow (in `-debug`).
  Turn them off for a whole build (`-no-bounds-check`), or for one method or
  statement (`@[no_bounds_check]`). The attribute is lexical: it covers the
  code written inside, not the methods that code calls.

## Compile-time

```ruby
SIN = comptime build_table(256)       # [256]F32, computed while compiling
LEVEL = config(:level, 1)             # -define:level=3
FONT = embed("assets/font.ttf")       # []U8, through C23 #embed

comptime if OS == :windows
  def path_sep = "\\"
else
  def path_sep = "/"
end
```

- `comptime expr` and `comptime do … end` run Wid code inside the compiler,
  and the result becomes a constant of the program. The code is checked like
  any other and run by an interpreter over the same IR, so it computes exactly
  what the built program would. It always runs with the checks of a `-debug`
  build: bounds, nil and integer overflow.
- **Constants** are evaluated while compiling. Literal arithmetic stays
  untyped (`SIZE = 4 * 64`) and exact, past every integer type in between
  (`(1 << 100) >> 90` is `1024`); a value that doesn't fit its type is
  E0311, which names a shift's value as a power of two (`1 << 200` is
  2^200), whatever the shift amount. Folding is exact in 128 bits; an
  operation past them (`2 ** 200`, `-(-2^127)`) is E0311 where it happens,
  unless the value is for a float type, which computes it in floating
  point. Struct literals, `T.size`, other constants of any type, enum
  members, indexing other constants and the like evaluate too
  (`ORIGIN = Vec2.new(x: 0.0, y: 0.0)`, `START = ORIGIN`,
  `FACING: Dir = :north`). Calling a method needs `comptime`, so every place
  where code runs at compile time says so (E0327 suggests adding it). A
  macro expands without it, and one without arguments may omit `()` there
  as anywhere (`X = five`, `[five]Int`, an enum member's `a = five`). The
  value's names resolve as in a method: an undefined one is E0201 with a
  did-you-mean.
- **What is checked.** Every concrete declaration is checked whether or not
  anything uses it: package-level methods, the methods of a struct or enum
  that isn't generic, those of an `extend` of concrete types (for each type
  it lists), field defaults and constants. Generic code is checked per use,
  like a template: methods with `$T` parameters, the methods of a generic
  struct, of an `extend` over a pattern (`extend []$T`) and of a module
  (for each type that includes it). Generic code that nothing uses, like a
  module nothing includes, is never checked; a future `-vet` could check
  it.
- `comptime` code can use constants, literals and any Wid method, but not the
  variables around it, which have no value yet. It can allocate:
  `context.allocator` is a compile-time heap. `puts` and `p` show their output
  as a warning (E0909), for debugging. C can't run in the compiler (E0904),
  except the pure math functions (`sqrt`, `sin`, `pow`, …) and `core`'s memory
  helpers, which the compiler implements itself. `@[c]` defs are Wid code and
  do run. One evaluation may run 20 million steps, nest 256 calls and use 256
  MiB (E0903).
- **What crosses to run time:** numbers, `Bool`, strings, enums, procs that
  name Wid methods, and structs, fixed arrays, tuples, optionals and unions of
  these. A slice crosses as a copy of its elements in static storage. Pointers,
  dynamic arrays, maps, allocators and `Type` values point at memory that only
  exists in the compiler, so they can't (E0905): return a fixed array or a
  slice instead. Large values live in one read-only static object that every
  use shares.
- **`comptime if cond … elsif … else … end`** chooses code, at declaration
  level and in method bodies. Only the chosen branch is checked and compiled;
  the others are only parsed, so a branch may `cimport` a header or call an API
  that exists on one platform only. Declaration-level conditions run once every
  declaration outside them is known, in source order. A struct's fields can't
  be conditional. Used as a value (`n = comptime if … end`), it needs an
  `else`, like `if`, even when a branch is chosen: on another target none
  may be (E0324).
- `OS` and `ARCH` hold the target, as members of the prelude enums `Os`
  (`:darwin`, `:linux`, `:windows`, `:freebsd`, …) and `Arch` (`:arm64`,
  `:amd64`, `:wasm32`, …). `-target:os_arch` sets them (default: the host).
  `wid check -target:linux_amd64` checks another target's code; building for
  another target isn't supported yet (E0709).
- `config(:name, default)` reads `-define:name=value`, parsed as the default's
  type (`Bool`, an integer, a float or `String`); without the flag it is the
  default.
- A `macro def` runs at compile time. It receives types, AST and symbols, and
  returns code built with `quote do … end`, using `#{}` to splice values in.
  - **Parameters:**
    - A `Code` parameter receives the argument's code, unevaluated. The
      code runs where the `quote` splices it, as often as it is spliced.
    - A `Symbol` parameter receives a symbol literal (`:hp`) as a name.
    - A `Type` parameter receives a type.
    - Any other parameter type receives a value computed like `comptime`.
    - An argument of the wrong kind for its parameter, like an expression
      for a `Symbol` or a value for a `Type`, is E0912.
    - The last parameter may be written `*names: T`. It collects the
      remaining positional arguments, zero or more, into a `[]T`, each
      converted by `T`'s rule, so `macro def flags(*names: Symbol) -> Code`
      takes `flags :READ, :WRITE, :EXEC` and can generate a constant for
      each name. It can't have a default. Only macros take one; other
      methods take a `[]T` and an array literal (E0112).
    - A macro always returns `Code` and says so (`-> Code`, E0310). It
      takes no `$T` parameters (E0315, take a `Type`) and no block (E0319,
      take a `Code`).
  - **`Code` and `Symbol`** values exist only while compiling, like `Type`;
    a method the program runs can't use them (E0906). A macro body can keep
    them in variables and collections (`[dynamic]Code`, `[]Symbol`),
    compare symbols with `==`, and convert with `sym.to_s` and
    `str.to_sym`. In compile-time code a symbol literal where no enum is
    expected is a `Symbol`. A zero `Code` (`{}`) is no code.
  - **Quotes:** the code in a `quote` may be statements or declarations
    (`def`, `struct`, constants, `using` fields, other macro calls, …);
    which it must be depends on where the macro is called. A line
    `#{name} = v` reads as an assignment, which among declarations is a
    constant; when `v` can only be a type (`distinct F64`,
    `proc(Int) -> Int`, `@[c] proc(I32)`), the line is that constant's
    declaration wherever it is, as `NAME = v` is. `quote` works only in a
    `macro def`, the procs inside it included (E0910). A `def` that
    returns `Code` was meant to be a macro: it is E0910 at the `def`, with
    the fix `macro def`, and a call of it among declarations counts as a
    failed expansion, so the names it would have declared aren't reported
    missing.
  - **Nested quotes:** a splice belongs to the innermost `quote` around
    it. In a `macro def` that a `quote` generates, the inner macro's
    `quote` is left as written when the outer macro expands; its splices
    run when the inner macro runs and read the inner macro's names, so
    they can't read the outer macro's parameters (E0201, which says so).
    To pass an outer value on, generate a constant in the outer `quote`
    (`N_VALUE = #{n}`) and use it, or splice the value into the inner
    macro's own code, outside its `quote` (`v = #{n}`, then `#{v}`).
  - **Splices:** a spliced value is inserted according to its type, and must
    fit where the splice is (E0911, reported at the call):
    - `Code` inserts that code: one expression where a value goes, and any
      statements or declarations when the splice stands alone on a line.
      Code of several statements can't stand where a value goes.
    - `[]Code` (or `[dynamic]Code`, `[N]Code`) inserts a sequence: of
      statements or declarations when the splice stands alone on a line,
      and of elements when it is one of a comma-separated list (call
      arguments, array elements, returned values, …), one per element.
      Nowhere else.
    - `Symbol` inserts a name, usable as an identifier, a method name
      (`def #{name}`, `x.#{name}`), a field (`@#{name}`, or
      `@#{name}(args)` to call the proc it holds) or a parameter name. In
      an expression it is that identifier (a constant's, if capitalized),
      and where a type goes, the type of that name.
      `:#{name}` inserts a symbol literal; its value must be a `Symbol`.
      In a list, a `[]Symbol` inserts one identifier (or, written
      `:#{names}`, one symbol literal) per name. A `Symbol` may hold any
      text (`"a b".to_sym`), but a name spliced from one must be what the
      lexer reads in its place (E0911): a type's or a constant's starts
      with a capital letter; a method's, field's, parameter's,
      variable's or enum member's with a lowercase letter or `_`; both go
      on with letters, digits and `_`, a method's may end in `?` or `!`
      or be an operator, and none is a reserved word. A symbol literal
      (`:#{name}`) takes any `Symbol`.
    - `Type` inserts the type where a type goes, and the type as a value
      elsewhere, so `#{t}.new(…)`, `x.to(#{t})` and `size_of(#{t})` work.
    - Numbers, `Bool`s and strings insert literals; a negative number is
      parenthesized, and a float that is not finite can't be spliced.
    - A splice that is an assignment target (`#{x} = v`, `#{x} += v`,
      `#{x} ||= v`, `a, #{x} = …`) must insert something that can be
      assigned to: a variable's name from a `Symbol`, or `Code` that is a
      variable, a field, an element or a dereference. Among declarations a
      capitalized name declares a constant instead (`#{name} = v`). A
      number, string, `Bool`, `Type`, call or other value there, or a
      capitalized name in a method, is E0911.
    - In an `enum`'s body, a splice alone on a line gives members, in its
      place among the written ones: a `Symbol` (or `[]Symbol`) one per
      name, and code (`Code` or `[]Code`) one per line that is a name
      alone or `name = value`, so a macro can build the members from
      fragments (`quote do #{m} = #{v} end`). The code's other lines are
      declarations, and a line that is neither is E0911.
    - A `quote` splices only these types; splicing another is E0911 in the
      macro.
  - **Lexing:** `#{` outside a string literal always starts a splice, and a
    splice is only valid inside `quote` (E0111). Comments therefore start
    with `# `. A splice is one expression and may span lines. Inside a
    string literal in a `quote`, `#{}` is ordinary run-time interpolation
    of the generated code. A splice inserts a whole name, so it can't be
    glued to text, as in Ruby's `define_method("bump_#{name}")`:
    `def bump_#{name}` or `@#{name}_count` is E0111. The macro builds the
    name first (`fname = "bump_#{name}".to_sym`) and splices that
    (`def #{fname}`). Outside a `quote`, a splice glued to a name is never
    read as a comment: it is one E0111, and the line is read as the
    declaration it was written in. The name names nothing: a field, call,
    method, symbol, local or named argument written with it adds no other
    error, and no suggestion offers it. Inside a splice's expression, which is
    macro code, a name glued to a splice (`#{foo_#{name}}`) is E0111 too,
    and is read as the `"foo_#{name}".to_sym` it means.
  - **Calls:** macros are package members like any def, declared at the top
    level of a file (E0105 inside a type). `pkg.name(…)`, and
    `pkg.name args` as a statement or declaration, works wherever an
    unqualified call does, including at declaration level.
    `private macro def` hides a macro outside its package (E0205). A
    macro is not a method the program can call, so it can't be a proc
    (`method(:name)`, E0323), and it takes no block.
  - **In an `overload` set,** a macro member expands when a call chooses
    it, in an expression (with the call's expected type), as a statement
    or among declarations, where the chosen member must be a macro
    (E0108). The call picks the member by parameter types as usual,
    before any argument is lowered: a `Code` parameter takes any argument
    but ranks below every typed parameter, exact or converting; a
    `Symbol` parameter takes a symbol literal and a `Type` parameter a
    type, both exactly; other parameters take values by type, and an
    argument is checked for its type only when a member needs it. A
    macro that collects arguments with `*` can't be a member (E0316).
  - **Operators** are methods of types, which the program runs, so a
    `macro def` can't be named like one (`+`, `==`, `[]`, unary `-`, …),
    and an operator's `overload` set can't list a macro (E0915). Define
    the operator with `def`, whose body may call a macro, or give the
    macro a name.
  - **Where a macro call expands:** in an expression, as a statement, as a
    declaration in a `struct`, `enum`, `module` or `extend` body, or at
    package level. Declaration-level calls expand once every declaration
    outside them is known, in source order, like `comptime if`.
  - **Among declarations,** a call must name a macro (or an `overload`
    set that chooses one): calling a method there is a statement outside
    a method (E0108), and an unknown name is an undefined macro (E0201).
    In an `enum` body a name alone on a line is
    a member, so a macro without arguments is called `name()` there. A
    member written as a name alone that a macro visible there also has is
    E0914, whose fix adds the `()`; a member with a value (`name = 1`) is
    never a call.
    - The generated declarations take the call's place and are collected
      like written ones, before any method body is checked, so code
      anywhere in the package can use them. Calls written after the call,
      and the macros they run, see them too; calls before it don't. The
      calls among generated declarations expand after every call written
      outside them.
    - Each line of the generated code must be a declaration: `def`,
      `macro def`, `struct`, `enum`, `union`, `module`, `extend`,
      `overload`, `include`, a constant (`NAME = v` or `NAME: T = v`, also
      with a spliced name, `#{name} = v`), a field (`name: T` or
      `using name: T`), a `comptime if` whose branches follow the same
      rule, or a macro call (a call or a name alone on a line, maybe after
      `private`), which expands in turn and counts against the budgets.
      Any other statement is E0108. Code spliced among declarations inside
      a `quote`, like `#{fields}` in a `struct` body, follows the same rule
      (E0911 for a statement).
    - What a call may generate follows what may be written where it is
      (types only at package level, fields only in a struct, and so on),
      with two exceptions (E0913). A macro can't add fields to the struct
      whose body holds the call, because its layout is fixed before its
      macros run (an enum's members are fixed too, and a name alone on a
      generated line is a macro call). And generated code can't `import`
      or `cimport`, because packages are loaded before any macro runs; the
      quote's own code uses the imports of the macro's file instead.
    - `private` before a call (`private helpers :hp`) makes every
      declaration it generates private, whether the call is written among
      declarations or is a line of a `quote` (where `private` before a
      call or a name alone makes the line such a call). A call takes no
      attributes (E0328).
    - In a generic struct's body a macro runs once, for the declaration,
      not once per instance. The methods it generates are checked for each
      instance, like written ones, and `Self` in them is the instance.
  - **`Self` in a macro:** the macro's own code (outside its `quote`s,
    their splices included) may use `Self`, the type whose body or method
    holds the call, so a macro called in a `struct` body can read
    `Self.fields`. Such a macro runs, and is checked, for each `Self` it is
    called with, like a generic method. Where a call has no `Self` (at
    package level, or in a method that isn't a type's) or where `Self`
    stands for many types (in a `module` or `extend` body, or in a generic
    struct's body), calling it, or passing `Self` as an argument, is E0209.
  - **In an expression or as a statement,** the generated statements run
    in place of the call, in the caller's block, and the value of the last
    one is the call's value. So the variables a spliced name declares stay
    visible after the call, a generated `defer` runs when the caller's
    block ends, and `return`, `break` and `next` act on the caller.
    - When the call is a statement of its own, its last line is a
      statement too, as if the caller had written it: an `if`, `case` or
      `comptime if` there needs no value in its branches. Where the call's
      value is used (`x = say("hi")`), the last line gives it, so each such
      branch must end with one (E0323 otherwise).
    - In an operand that doesn't run once with the statement around it,
      the generated statements run with the operand. These operands are:
      the right side of `&&` or `||`, the value of `||=` or `&&=`, the
      arguments of a `&.` call and every `when` pattern but a `case`'s
      first, which may not run; `type_info`'s operand, which never runs
      (it is only checked); and a `while` or `until` condition, which runs
      on each test of the loop. The names they declare are visible only in
      that operand, as in a branch of a ternary or a postfix `if`; using
      one after it is E0201, with a note that the expansion in the operand
      declared it. In `while v = …`, the loop's body sees them too, as it
      sees `v`.
    - A `defer` that such an operand's code generates is an error at the
      call (E0406): a `defer` runs when its block ends, and no block ends
      when the operand's code does. Call the macro in a branch of an `if`,
      or as a statement of its own, instead.
  - **Names** in the quote's own code that aren't locals bound in the
    quote or members of `self` (reached with `@name` or an implicit-self
    call), that is methods, types, constants and packages, resolve where
    the macro is defined, with that package's imports and privacy. So a
    library macro can call its own helpers without the caller importing
    them. Names that come from splices, and `Self`, resolve at the call
    site. A name spliced from a `Symbol` argument resolves where that
    argument was written, even after passing through further macros
    (`inner :#{name}`).
  - **Hygiene:** each expansion keeps the locals that the quote's own code
    binds (assignment targets, `for` variables, block and proc parameters,
    and `guard` and `if v =` bindings) apart from the caller's, as if it
    renamed them: code spliced in from the call site can't see them, and
    the quote's own code can't see the caller's locals. Messages still show
    the names as written. The parameters of a generated `def` are part of
    its interface (named arguments use them), so code spliced into its
    body from the call site sees them too. Names that come from splices
    keep their spelling and belong to the caller, so a macro can
    deliberately bind a caller's name. A use that hygiene hides is E0201,
    which names the variable it can't see and suggests passing its name as
    a `Symbol`; so is a `quote` naming one of its macro's parameters
    without a splice, with the fix `#{name}`.
  - **Errors** in generated code point at the line inside the `quote` and
    at each macro call that led to it, innermost first, in the human and
    JSON output alike: the human output adds a `:::` snippet per call
    (`` `m` expands here ``, with nested calls of a macro from one place
    counted; more than six are shortened to the first three and the
    outermost, with a line counting the rest and naming their macros),
    and each JSON diagnostic has an `expansions` list of every call (the
    `macro` name and the call's position). Code spliced from the call site
    keeps its own position, and an error in it also points at the splice
    in the `quote` where it landed and at the calls behind that. A name the
    macro computed (`str.to_sym`) has the call's position, and the error
    says which name it is. Panic locations and `-debug` `#line`
    directives in generated code name the `quote`'s file and line. Two
    calls whose code declares the same name (E0202, E0317) show that line
    of the `quote` once, with both calls. A macro that fails while it runs
    is E0901, at the call. A macro whose code failed to parse doesn't run,
    and its calls report nothing more.
  - **Budgets:** each macro run has the `comptime` limits. Expansions may
    nest at most 64 deep (a macro whose code calls a macro), and one build
    runs at most 65,536 expansions. Exceeding either is E0903.
- **Reflection** at compile time: `T.fields` is a `[]FieldInfo` (`name`,
  `type`, `offset`), `T.methods` a `[]MethodInfo` (`name`, `params`, `ret`,
  `static`), and `T.name`, `T.size` and `T.align` describe the type. A type
  written where a `Type` is expected, or used as a value in `comptime` code, is
  a `Type` value, whatever its form (`name_of([]Int)`, `name_of(Int?)`,
  `name_of(Pool(Ball, 64))`, `name_of(C.int)`); it answers `.name`, `.size`,
  `.align` and `.fields` and compares with `==`. Elsewhere a type is not a
  value (E0323). `Type` values exist only while compiling: a method the
  built program runs can't use them (E0906). `p x` pretty-prints any value.
- **`type_info(T)`** and **`type_info(x)`** describe a type at run time. `T`
  is any type, written in place (`type_info(Int?)`,
  `type_info(proc(Int) -> Int)`). For an expression only its static type
  counts: `x` is checked but not evaluated. Both return a `^TypeInfo` that
  points at a read-only static table, and the same type always gives the
  same pointer, so `type_info(a) == type_info(b)` compares types. The
  prelude declares the records:
  - `TypeInfo` has `name` (as Wid displays the type: `"[]Vec2"`,
    `"Pool(Ball, 64)"`), `kind` (a `TypeKind`: `:int`, `:uint`, `:float`,
    `:bool`, `:rune`, `:string`, `:cstring`, `:rawptr`, `:typeid`, `:any`,
    `:pointer`, `:multi_pointer`, `:array`, `:slice`, `:dynamic_array`,
    `:map`, `:matrix`, `:optional`, `:proc`, `:struct`, `:enum` or
    `:union`), `size`, `align`, `elem`, `key`, `count`, `columns`, `fields`,
    `members` and `variants`.
  - `elem` is the pointee, element, map value, optional payload (`^T` for
    `^T?`), enum backing type or proc return type, and `nil` when there is
    none; `key` is a map's key type. `count` is `N` of `[N]T` and the rows
    of a matrix, `columns` its columns.
  - `fields` lists struct fields in declaration order, including `using`
    ones, as `TypeInfoField`s (`name`, `type`, `offset`). For a proc they
    are its parameters, with empty names and offset 0, since a proc type
    doesn't keep parameter names. A multiple-value type such as
    `(Int, Error)` is a struct with fields `0`, `1`, ….
  - `members` lists enum members as `TypeInfoMember`s (`name`, `value: I64`;
    a `U64`-backed member gives its bit pattern), and `variants` a union's
    variant types, both in declaration order.
  - A `distinct` type reports its base type's kind, layout and details under
    its own name. `Error` is a `U32`-backed enum whose members are the error
    symbols the program uses. An `opaque` C struct has size 0, alignment 0
    and no fields. Sizes, alignments and offsets are C's, so they are exact
    for structs C lays out (a cimported C union's fields all have offset 0).
  - `Type`, and records that hold one such as `FieldInfo`, exist only while
    compiling, so `type_info` can't describe them (E0906); `Never` has no
    values to describe (E0323).
  - The tables are static data: only types the program passes to
    `type_info`, and the types those point at, are emitted, and nothing is
    allocated.
  - The tables are read-only. An assignment (`=`, compound or `||=`) to a
    `TypeInfo`, `TypeInfoField` or `TypeInfoMember` reached through a
    pointer or a slice, or to anything reached through a pointer or slice
    read out of one, is E0309, and so are `&` of a value inside a table
    other than a whole record and `for &v` over a table's list. A copy in a
    variable can be changed (`info = t^`), and a variable can point at
    another table. This is a rule for these records, not a read-only
    pointer type: a `[]^TypeInfo` copied out of a table
    (`vs = t.variants`, then `vs[0] = …`) and pointers converted with `.to`
    can still be written through, which is undefined behaviour.
  - `type_info` works in `comptime` code too. Its result is a pointer, so it
    can't cross to run time (E0905), but what is read from it can:
    `comptime type_info(Ball).size`.
- `embed("font.ttf")` reads a file, relative to the package directory, when the
  program is compiled, and returns its bytes as a `[]U8` stored with C23
  `#embed`. Embedded files are inputs of the build.

## C and C++ interop

```ruby
cimport "vendor/stb_image.h", as: :stbi, strip_prefix: "stbi_",
  implement: "STB_IMAGE_IMPLEMENTATION"

def load_rgba(path: CString) -> ([^]U8, Int, Int)?
  w, h, n: C.int
  px = stbi.load(path, &w, &h, &n, 4)
  return nil if px.nil?
  return px, w.to_i, h.to_i
end
```

- `cimport` uses libclang to turn the functions, structs, unions, enums,
  typedefs, constants and simple macros of a header into a package of Wid
  declarations. `wid cimport --dump <header>` prints that package.
- **Namespaces.** With `as: :stbi`, the declarations live under `stbi.` in
  the file that imports them, like an `import`, and other packages don't see
  them. Without `as:`, they join the package's own namespace, exactly as if
  the package declared them: its code calls them unqualified and its
  importers reach them like any other declaration. That is how a binding
  package is written: `vendor/raylib/raylib.wid` is one `cimport` without
  `as:`, so `import "vendor:raylib", as: :rl` gives `rl.init_window` and
  `rl.Color`, and Wid helpers can sit next to it. A name the package declares
  itself, or that two such `cimport`s add, is an error (E0202) that suggests
  `names:`. `cimport` also brings `C`
  into scope, which holds `C.int`, `C.size_t` and the other C types, sized for
  the target (`long` is 64 bits on 64-bit Unix). Without `cimport`, write
  `import "core:c", as: :C` for the same names.
- **Headers.** A path is looked up next to the package's files first, then on
  the include path, like `#include <name>`. The import covers the header and
  the headers in its directory tree (so `SDL3/SDL.h` brings `SDL3/SDL_*.h`);
  a header directly in a shared directory such as `/usr/include` brings the
  headers at that directory's top level. Names C reserves (`__x`, `_X`) are
  left out.
- **Names.** `strip_prefix:` (a string or an array, matched ignoring case)
  goes first. Then functions, parameters and fields become `snake_case`
  (`InitWindow` → `init_window`, `vertexCount` → `vertex_count`), types
  `PascalCase` (`io_callbacks` → `IoCallbacks`), and constants keep their
  spelling, or become `SCREAMING_CASE` if they start with a lowercase letter.
  `rename: :keep` keeps C names, only lowercasing the first letter of
  functions and fields. `names: {CName: :wid_name}` names single
  declarations. Names that two declarations would share go to neither, and
  using one is an error (E0704) that suggests `names:`.
- **Types.** `char *` (const or not) is `CString?`, `unsigned char *` (and
  `uint8_t *`) is `[^]U8?`, `void *` is `RawPtr?` and every other pointer is
  `^T?`. C arrays in structs are `[N]T`, and function pointers are C-ABI procs,
  `@[c] proc(x: C.int) -> C.int`, which take `method(:name)` of an `@[c]` def
  and can be called with `.call`. An enum is its integer type
  (`KeyboardKey = C.int`) and its constants are untyped integer constants, so
  they fit any integer parameter. Typedefs are aliases. A struct whose fields
  C hides is `opaque` and is only used through `^T`. Types from outside the
  import that are only pointed at (`FILE`) become opaque structs.
- **`types: {Vector2: Vec2}`** lets a Wid type stand in for a C struct of the
  same size and alignment (checked, E0706): every function, field and constant
  that uses the struct uses the Wid type, and values are copied across with
  `memcpy`.
- **Constants and macros.** Macros that evaluate to numbers, strings or bools
  become constants. Other value macros, like raylib's `RED`, and `const`
  globals are read by name in C (`@[extern("RED")] RED: Color = ---`). A macro
  that names a function (`#define GetMouseRay GetScreenToWorldRay`) is a
  function. Function-like macros, mutable globals and bit-fields are not
  imported, and using one is an error (E0705) that explains why.
- **Calls.** String literals convert to `CString`; a runtime `String` needs
  `.to_cstr`. A C function ending in `...` takes numbers, pointers, C strings
  and C structs there; untyped literals become `C.int` and `C.double`, as in C.
  Only `@[extern]` methods may end their parameters with `...`.
- The generated C `#include`s the original header unchanged, so the C compiler
  is responsible for getting the ABI, layout and macros right, and checks every
  call; where Wid's C spelling of a type differs (`char *` against
  `const char *`), the call casts. This is the main reason Wid compiles to C23.
  An `@[extern]` method is declared under a name of its own, bound to the C
  symbol, so it never conflicts with a header that declares the same function.
- `define: ["NAME=value"]` sets macros before every inclusion. `implement:`
  defines a header-only library's implementation macro in exactly one
  translation unit of its own. `link:` names libraries (`"m"`), library files
  and macOS frameworks (`"framework:Cocoa"`). `pkg_config:` takes compiler and
  linker flags from pkg-config, and `include_dirs:` adds header directories.
- `wid build` compiles and links any `.c` and `.cpp` files in a package (C++
  as C++20, with the C++ compiler matching the C one). C++ is
  reached through C APIs (`extern "C"`, e.g. cimgui). Importing C++ classes or
  templates directly is out of scope.
- **Calling Wid from C.** `@[c]` gives a def the C ABI and `@[export("name")]`
  gives it a stable symbol. A C-ABI def starts with `Context.default`.
- `wid doc` works on C symbols too: it shows the declaration `cimport`
  renders, the C name, where the header declares it, and the C doc comment
  (`///`, `/** */`, or a comment trailing the declaration).

## Toolchain and CLI

- The compiler is written in Rust. It emits C23 and calls the host C compiler
  (clang ≥ 19 or gcc ≥ 15); `cimport` also needs libclang. The output has
  `#line` directives, so lldb, gdb and sanitizers point at `.wid` sources.
  `-keep-c` keeps the generated C.
- **Tests.** `wid test <dir>` builds the package together with its
  `_test.wid` files (which other builds skip) and runs every `@[test]` method:
  a package-level `def name(t: ^testing.T)` from `core:testing`.
  `t.expect(cond)`, `t.expect_eq(got, want)`, `t.expect_nil(x)` and
  `t.fail(message)` record a failure at the caller and the test continues;
  `t.log` adds to the test's output. Each test runs in its own process on a
  fresh tracking allocator: a panic fails only that test, and every block it
  leaks from `context.allocator` fails it with the allocation site. Failures
  are diagnostics (E0802) pointing at the expectation, the panic or the
  allocation. `-filter:<text>` runs the tests whose name contains the text,
  and `-json-errors` prints the results as JSON. The exit status is 1 when a
  test fails.
- A package is a directory. Imports look like `import "core:fmt"`,
  `import "vendor:raylib"` and `import "./physics"`. An `import` or
  `cimport` is written at the top level of a file (or in a top-level
  `comptime if`), never in a `struct`, `enum`, `module` or `extend` body
  (E0105). Wid ships the `core:` and
  `vendor:` collections. `vendor:` holds `raylib` and `sdl3` (the system
  libraries, through pkg-config) and the vendored `stb/image`,
  `stb/image_write`, `stb/truetype`, `stb/rect_pack` and `miniaudio`. `core:` holds
  `builtin` (the prelude), `mem`, `fmt`, `strings`, `os`, `math`, `c` and
  `testing`. Each opens the file named after it with its package doc, and
  documents every public declaration, so `wid doc core:<pkg>` has a summary
  for each.
- The CLI follows Odin. Commands: `wid run <dir> [-- args]`, `build`, `check`,
  `test`, `doc`, `fmt`, `explain`, `cimport`, `query`, `lsp` and `version`. Flags:
  `-out:`, `-o:none|minimal|size|speed|aggressive`, `-debug`, `-vet`,
  `-define:NAME=val`, `-collection:name=path`, `-target:os_arch`, `-file`,
  `-sanitize:address`, `-filter:` (for `test`), `-json` and `-private` (for
  `doc`), `-in:` (for `query`), `-check` (for `fmt`) and `-json-errors`.
  `check` generates no C, so it takes only the flags that change what is
  checked: `-file`, `-define:`, `-target:`, `-collection:` and
  `-json-errors`; the flags for building C (`-out:`, `-o:`, `-debug`,
  `-keep-c`, `-cc:`, `-no-bounds-check`, `-sanitize:`) belong to `build`,
  `run` and `test`. Compile-time code runs with `-debug`'s checks whatever
  the flags. A flag another command takes is an error that names the
  commands taking it (`wid explain -debug`: "`-debug` doesn't apply to
  `wid explain`"), never silently ignored; like every usage error, it exits
  with status 2.
- Without `-file`, the target of `build`, `run`, `check` and `test` is a
  package directory. One that isn't is an error (E0206) that points into the
  command line: a missing directory, with a similar package directory next
  to it (or its `.wid` file) as the fix; a directory without `.wid` files,
  with what it holds (the package in a subdirectory as the fix); a `.wid`
  file, with adding `-file` as the fix; or another file, which is neither.
- `-file` makes the target a single `.wid` file instead of a package
  directory, so it needs one: `wid check main.wid -file` (for `doc` and
  `query`, a collection path can name it: `core:fmt/fmt.wid`). Naming no
  file, a missing file or a directory is an error (E0206; E0601 for `doc`
  and `query`) that points into the command line, with the only `.wid`
  file of the directory or a similar one as a fix, and dropping `-file` as
  the fix for a package directory.
- **Docs.** `wid doc [package] [symbol]` shows documentation made from doc
  comments: the `# ` comment lines directly above a declaration (above its
  attributes too), with no blank line between; an enum member's sit above
  it the same way. A package's doc is the comment block that opens one of
  its files, when a blank line, the end of the file, or an `import` or
  `cimport` follows it (so a `vendor` package's header counts); the file
  named after the package (`strings.wid` in `core/strings`) wins, then the
  first file that has one.
  - `package` is a directory (default `.`), a single file with `-file`, or
    a collection path such as `core:fmt` or `vendor:raylib`. `symbol` is a
    path: `Name`, `Type.member`, `alias.Name` or `alias.Type.member`, where
    `alias` is an `import` or `cimport … as:` name of the package's files
    (`wid doc . rl.draw_circle_v`); an alias alone documents its package. A
    first name the package doesn't declare is looked up in the prelude and
    among the builtin types: `wid doc core:strings String` lists the
    methods the program's extensions add to `String`.
  - With one argument, it is the package if it names an existing directory
    or file or contains `:`, and otherwise a symbol of the package in `.`:
    `wid doc rl.draw_circle_v` works in a package directory. When neither
    reading works, the error says what was tried both ways.
  - Without a symbol, the page shows the package doc, then every public
    declaration with its declaration line and the first paragraph of its
    doc, in sections: `CONSTANTS`, `TYPES` (structs, enums, unions and type
    aliases, with their fields, members and own methods), `MODULES`,
    `METHODS` (package-level methods and overload sets), `MACROS` and
    `EXTENSIONS`. Declarations a `cimport` without `as:` adds are the
    package's own.
  - A symbol's page shows its attributes, its declaration line, its whole
    doc and where it is declared. A type's page adds its fields (with those
    `using` promotes), enum members or union variants, and its methods
    grouped by where they come from: its own, `include`d modules, `extend`
    blocks and `using` promotion. Declaration lines read like the source:
    generics show their parameters (`struct Pool($T, $N: Int)`), and a
    macro's starts with `macro def`. A declaration of a `cimport` package
    shows the C name and where the header declares it instead of a Wid
    file.
  - Private declarations are left out; naming one is an error (E0604) that
    suggests `-private`, which documents them too, marked `private`. An
    unknown package is E0601, an unknown symbol E0602 and a missing member
    E0603.
  - `wid doc` loads and checks the package, and never generates code.
    Errors in the package are reported as usual, and the page still shows
    every declaration the checker collected (the chosen `comptime if`
    branches and what macros generated included); the exit status is then
    1.
  - The page goes to stdout and diagnostics to stderr. `-json` prints the
    page as one JSON document with `package` (`name`, `path`, `doc`),
    `symbol` (the path asked for, or `null`) and `items`: every declaration
    of an overview, or the one symbol. Each item has `kind`, `name`, `path`,
    `package`, `signature`, `attributes`, `doc`, `private` and `location`
    (`file`, `line`, `column`; `null` for C), and, where they apply,
    `owner`, `static`, `c` (`name`, `header`, `declared_at`), `fields`,
    `members`, `variants`, `targets`, `aliases` and `methods`. Fields and
    methods listed on a type have an `origin` (`kind`: `own`, `include`,
    `extend` or `using`; `via`, as written; `location`). `-json-errors`
    prints the diagnostics as JSON, on stderr. `wid query` reuses this
    shape.
- **Formatting.** `wid fmt [dir|file] [-check]` rewrites the `.wid` files
  of the package in `dir` (default `.`, its `_test.wid` files included),
  or one `.wid` file (any file with `-file`), in Wid's canonical style, and
  lists the files it changed, one per line. The style has no options.
  - Only the whitespace between tokens changes, plus a list's trailing
    `,`. Every token keeps its text: strings and their interpolations,
    numbers, symbols and splices (`#{…}`) are never changed. The author's
    line breaks stay: no line is split or joined, and nothing is wrapped
    to a width.
  - Indentation is two spaces per block. `end` and closing brackets line
    up with the line that opened them, `else` and `elsif` with their `if`,
    `when` and `else` with their `case`; the branches of `x = case y` go
    one step in. A line that continues a statement (after an operator or
    `,`, or starting with `.method`) goes one step past the statement's
    first line, and the lines of a bracketed list one step past the line
    that opens it. In a block's header, where the body follows, a
    continued line and a list that closes on its last item's line
    (`def f(a: Int,` then `b: Int)`) go two steps. A `\` that ends a line
    stays.
  - One space around binary operators, assignments, the `=` of a default,
    a constant or `def f = expr`, `->`, a union's `|`, a ternary's `?` and
    `:`, and the `:` of `enum Dir : U8`; after `,` and after a keyword
    (`return (x)`, but `yield(x)` and `yield (x)` differ and stay); after
    a name's `:` (`x: Int`, `name: value`), with none before it. None
    inside `( )`, `[ ]` and `@[ ]`, around `.`, `&.`, `..` and `...`,
    after a unary operator or a prefix `&`, `*` or `^` (`-x`, `&x`, `*xs`,
    `^T`), before a call's or a parameter list's `(`, a type's `?` or a
    dereference's `^`, or between a type's `]` and its element (`[]Int`,
    `[4]F32`). A `{ }` block has a space inside its braces,
    `{ |x| x * 2 }` (an empty one is `{}`), and block parameters are
    written `|a, &b|`. Where a space decides how the line parses
    (`foo -1`, `foo [1]`, `foo (x)`), it stays as written.
  - A call, array or parameter list whose closer is on a line of its own
    ends with `,` (except after `...`); one that closes on its last item's
    line has no trailing `,`.
  - Blocks keep the form they were written in, `do |x| … end` or
    `{ |x| … }`, and so do one-liners: `def f = expr`, `x if c` and
    `if c then a else b end`.
  - At most one blank line in a row, and none at the start or end of the
    file or of a block (after a line that opens one, next to `else`,
    `elsif` and `when`, or before a closer). A declaration that spans
    several lines has one blank line before it (above its doc comment) and
    after it, at the top of a file and in a type's body; between one-line
    declarations the author's choice stays.
  - Every comment is kept where it is. A trailing comment is one space
    after the code at least; trailing comments on consecutive lines with
    the same indentation line up one space after the longest code. An
    own-line comment takes the indentation of the line after it, except
    that before `end`, `else`, `elsif`, `when` or a closing bracket it
    takes the body's when it was written deeper than that line. A comment
    starts with `# ` (a space is added after `#`), except a `#!` line that
    opens the file. Doc comments stay directly above their declarations.
  - Lines end with `\n`, the file with exactly one. Indentation and the
    space between tokens are spaces, and trailing blanks go (strings keep
    theirs).
  - Only a file that parses without errors is formatted. Otherwise its
    errors are reported, as `wid check` reports them, and the file is left
    as it is; the exit status is 1. A file is rewritten only when the
    result parses to the same syntax tree with the same comments, and
    formatting it again changes nothing; a file that can't be formatted so
    is left as it is and reported as a bug in `wid fmt`.
  - `-check` changes nothing: it lists the files that would change and
    exits with 1 when there are any. `-json-errors` prints one JSON
    document on stdout instead: `changed` (the files), `errors`,
    `warnings` and `diagnostics`.
  - Formatting reads only syntax: it never loads imports, type-checks or
    generates code. The formatter is `wid_syntax::fmt::format`, a pure
    function of a file's text and its syntax tree, which the LSP's
    formatting request calls too.
- **Query.** `wid query <query> [argument] [flags]` answers a question
  about a package with JSON, for editors, scripts and LLMs.
  - The engine is the `wid_query` crate, which `wid lsp` shares: a pure
    library that takes a loaded and checked program (the checker's symbol
    index, and where each declaration starts and ends) and returns plain
    data. It never prints or exits. The driver loads and checks the package
    with one function that a long-lived caller reruns when files change,
    and the CLI prints the answer. A query never generates code or runs the
    C compiler.
  - The package is the one in `.`, or the one `-in:` names: a directory, a
    `.wid` file with `-file`, or a collection path (`-in:core:fmt`).
    `-collection:`, `-define:` and `-target:` work as for the other
    commands.
  - `outline` lists every declaration of the package in source order,
    private ones too, marked `private: true`: a query tool sees everything.
    Each item has `kind`, `name`, `path`, `package`, `signature`,
    `attributes`, `private`, `summary` (the first paragraph of its doc),
    `location` and `span`, and where they apply `static`, `c` and
    `children`: a struct's fields, an enum's members, then the methods,
    constants and overload sets written in a type, module or extension.
  - `def <symbol>` gives the declaration a symbol path names, private ones
    too. The path is `wid doc`'s (`Name`, `Type.member`, `alias.Name`,
    `alias.Type.member`) and resolves the same way. Each result is a
    `wid doc -json` item with its `span` added. An overload set gives the
    set and then each of its members, an import name a `package` item (its
    `signature` is the `import` or `cimport` that binds the name), and a
    builtin type the methods that extensions add to it.
  - `methods <Type>` lists every method and overload set callable on a
    struct, enum, union or module, a type alias of one, or a builtin type,
    private ones too. They are grouped by origin in lookup order (the type
    itself, `include`, `extend`, `using`), and each group is
    `{"origin": …, "methods": […]}`, with `origin` as `wid doc` writes it.
    The builtin methods of builtin types are described here, not listed.
  - `refs <symbol>` lists every use of what a symbol path names (read as
    for `def`) in the package and the packages it loads, private ones too,
    sorted by file, line and column. Each use is `{"location", "kind",
    "context"}`. `kind` is `declaration` (the name where it is declared;
    for an import name, the `import` or `cimport` that binds it), `read`,
    `write` (the target of `=`, `+=` or `||=`, or a named argument of
    `T.new`), `call` (a method, macro or operator method called; calling
    an overload set calls the set and the member it chose), `type` (written
    in a type, or as `T.new`, `T.method`, `include` or `extend` names it) or
    `import` (an import name before a dot: `geo` in `geo.Vec`). `context`
    is the path of the declaration the use is in (`Player.heal`), or `null`
    at the top level of a file. A use in code a macro generated is
    reported at the macro call that generated it (the outermost, which is
    in a file) with `"via_macro"`: the macro as the call names it. Code
    checked more than once, like a generic method for each instance or a
    block method at each call, counts once. `method(:f)` and an `overload`
    list read `f`. A field `using` promotes is its declaring struct's, so
    `Player.hp` and `Entity.hp` have the same uses; a builtin type's uses
    are the places it is written as a type.
  - `calls <symbol>` is `refs` keeping only the `call` uses: the call
    sites.
  - `type <file:line:column>` says what is at a position: the innermost
    expression, name, binding, parameter, written type or declaration name
    there. Lines and columns count from 1, as in diagnostics, and the file
    is named as diagnostics show it (relative to the current directory).
    Without `-in:`, the package is the one that holds the file. The result
    is `{"location", "span", "type", "kind"}`: `location` is the name or
    code at the position, `span` what the type is of (the whole call, for a
    method's name in one), `type` the type as Wid displays it (`^Ball?`, or
    `proc(Int) -> Int` for a method's name; `null` for a package, module,
    macro or overload set), and `kind` one of `expression`, `call`,
    `local`, `parameter`, `field`, `type`, `declaration` (the name in a
    method's, constant's or type's own declaration) or `package` (an import
    name). When the position names a declaration, field, enum member,
    package or variable, `refers_to` is its `def` item (a `local` or
    `parameter` item for a variable). In code checked for several generic
    instances, `type` is the first instance's and `instances` lists each
    instance's type. A position where nothing is, like a comment or blank
    space, is E0605, which points at the code nearest to it and suggests
    asking about the nearest; so is a malformed position, a file the
    program doesn't hold, or a line or column past the end.
  - `refs`, `calls` and `type` read what the checker records as it
    resolves names and lowers code, only when it checks for a query, so
    they cover the code it checks: the package's methods and the code of
    other packages that the package reaches. A generic method that no call
    instantiates (methods of `extend` blocks and modules are generic over
    `Self`), and a field default that no `T.new` uses, have nothing
    recorded.
  - stdout always holds one JSON document with `query` (`outline`, `def`,
    `methods`, `refs`, `calls`, `type`), `symbol` (the argument: a symbol
    path or a position, or `null`), `package` (`name`, `path`, `doc`, or
    `null` when it can't be loaded) and `results`, which is empty when the
    request fails. In an item, a `location` is where a declaration's name
    is and a `span` the whole declaration, from its attributes (or
    `private`) to its last token; both are `null` for C declarations and
    packages. Every location and span has `file`, `line`, `column`,
    `end_line` and `end_column`, 1-based with an exclusive end, as in
    diagnostics. Keys are sorted, and lists keep source or lookup order (or
    file order, for uses), so the same program always gives the same
    document.
  - Diagnostics go to stderr as one JSON document, the one `-json-errors`
    prints, whenever there are any. A package with errors is still
    answered from what the checker collected, as with `wid doc`. An unknown
    package, symbol or member is E0601, E0602 or E0603 as for `wid doc`,
    pointing into the command line `wid query …`, with did-you-mean fixes.
    A member named without its type gets one fix for each type that has
    it, and `methods` of something that isn't a type (E0603) gets a fix
    that asks for its `def`; a position where `type` finds nothing is
    E0605. The exit status is 1 when the request fails or the package has
    errors. A malformed command line, like an unknown query or a missing
    argument, is a usage error (status 2), as for every command.

## Built for humans and LLMs

```
error[E0207]: `spawn` may be nil here
  --> game/level.wid:14:18
   |
14 |   player.pos = spawn
   |                ^^^^^ `data.find_spawn` returns `Vec2?`
help: unwrap it and handle the nil case
   |   guard spawn = data.find_spawn else
   |     return {}, :no_spawn
   |   end
   = see `wid explain E0207`
```

- Every diagnostic has a stable code, a labeled span, a plain explanation of the
  problem and at least one concrete fix. Fixes are machine-applicable where
  possible, and "did you mean" candidates are included. The compiler recovers
  and reports every error, not just the first one.
- The same program gets the same diagnostics on every run. Among equally
  close "did you mean" candidates the shorter one wins, then the one the name
  would resolve to first (a variable, a method of `self`, a package name, a
  field), then the first alphabetically.
- The diagnostics end with a summary line that says what failed, in the
  command's words: `error: could not compile due to 2 errors` for `build`,
  `run`, `check` and `test`, `could not import the header due to …` for
  `cimport`, and for `wid doc` `could not write the documentation due to …`,
  or `the documentation may be incomplete due to …` when it still prints a
  page. With only warnings it is `warning: 2 warnings emitted`. `wid query`
  prints its diagnostics as JSON, with no summary line.
- `-json-errors` produces the same diagnostics with structured fix-its.
  `wid explain <code>` gives the long-form explanation with examples.
- `wid query` answers questions about a package in JSON: `outline`,
  `def <sym>`, `refs <sym>`, `calls <sym>`, `type <file:line:col>` and
  `methods <Type>`. It shares an engine with `wid lsp` (see "Toolchain and
  CLI").
- `wid fmt` is canonical and has no configuration (see "Toolchain and
  CLI"). `p`/`inspect` works on every
  type, and the tracking allocator reports leaks.

## Open

Map literal syntax, error payloads, threads/async, hot reloading, a package
manager, `#soa` layouts, and a comptime-backed REPL.

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

Editor support: [Vim](extras/vim).

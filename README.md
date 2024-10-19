# Wid: A Programming Language

Wid <sup>(pronunciation: /[vit](https://en.wikipedia.org/wiki/Help:IPA/Polish)/, from archaic Polish term [wid](https://en.wiktionary.org/wiki/wid#Noun) meaning a phantom)</sup> will be a statically typed programming language written in [Ruby](https://en.wikipedia.org/wiki/Ruby_(programming_language)) that compiles down to [C](https://en.wikipedia.org/wiki/C_(programming_language)).

## Features

  - [ ] Ruby Syntax
  - [ ] Static Typing

## Getting Started

Given an example `hello_world.wid`:

```ruby
msg = "Hello, World!"
puts(msg)
```

And compiling it with `wid hello_world.wid`:

```
wid hello_world.wid
```

We end up with `hello_world.c`:

```c
#include <Wid.h>
#include <stdio.h>

int main() {
  char *msg = "Hello, World!";
  puts(msg);

  return 0;
}
```

And a binary `hello_world`:

```
$ ./hello_world
Hello, World!
```

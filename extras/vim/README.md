# Wid for Vim

Syntax highlighting, indentation, comment settings and matchit support for
`.wid` files in Vim and Neovim.

## Install

Add this directory to the runtime path. With vim-plug:

```vim
Plug '~/src/wid', { 'rtp': 'extras/vim' }
```

With a native package (shown for fish; use `~/.config/nvim` for Neovim):

```fish
mkdir -p ~/.vim/pack/wid/start
ln -s ~/src/wid/extras/vim ~/.vim/pack/wid/start/wid
```

Syntax highlighting and filetype plugins need to be on (`syntax on` and
`filetype plugin indent on`). The indent rules read the syntax groups, so they
need highlighting too.

## Options

- `let g:wid_highlight_operators = 1` highlights operators.
- `let g:wid_recommended_style = 0` keeps your own `shiftwidth`, `softtabstop`
  and `expandtab` instead of the two spaces `wid fmt` uses.

## Maintenance

The reserved words in `syntax/wid.vim` come from the lexer, and
`crates/wid_syntax/tests/editor_syntax.rs` fails when they drift apart. Other
rules mirror `crates/wid_syntax/src/lexer.rs` and `SPEC.md`; update them along
with the language.

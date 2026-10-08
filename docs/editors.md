# Editor setup

`wid lsp` is Wid's language server. An editor starts it and speaks the
Language Server Protocol with it over stdin and stdout. It checks the package
each open `.wid` file belongs to as you type, and offers diagnostics with quick
fixes, hover, go to definition, document symbols and formatting. SPEC.md
("Toolchain and CLI", "LSP") describes what it does.

The `wid` binary must be on your `PATH` and find its `core/` directory: it
looks next to the binary and in the directories above it, or where `WID_ROOT`
points.

## Options

The server takes `-collection:name=path`, `-define:NAME=value` and
`-target:os_arch` on its command line, like `wid check`. The client's
`initializationOptions` add to them:

```json
{
  "collections": { "shared": "../shared" },
  "defines": { "LOG_LEVEL": "2" },
  "target": "linux_amd64"
}
```

A relative collection path is read against the workspace folder.

## Neovim

Neovim 0.11 and later configure servers with `vim.lsp.config`. The Vim plugin
in `extras/vim` sets the `wid` filetype; without it, add
`vim.filetype.add({ extension = { wid = "wid" } })`.

```lua
vim.lsp.config("wid", {
  cmd = { "wid", "lsp" },
  filetypes = { "wid" },
  root_markers = { ".git" },
  -- init_options = { collections = { shared = "../shared" } },
})
vim.lsp.enable("wid")
```

With nvim-lspconfig on an older Neovim, register the server first:

```lua
local configs = require("lspconfig.configs")
if not configs.wid then
  configs.wid = {
    default_config = {
      cmd = { "wid", "lsp" },
      filetypes = { "wid" },
      root_dir = require("lspconfig.util").root_pattern(".git"),
      single_file_support = true,
    },
  }
end
require("lspconfig").wid.setup({})
```

## Vim

[yegappan/lsp](https://github.com/yegappan/lsp) is an LSP client for Vim 9.
The Vim plugin in `extras/vim` sets the `wid` filetype; without it, add
`autocmd BufRead,BufNewFile *.wid setfiletype wid`. Register the server as the
client's [configs](https://github.com/yegappan/lsp/blob/main/doc/configs.md)
do for clangd:

```vim
call LspAddServer([#{
      \   name: 'wid',
      \   filetype: ['wid'],
      \   path: 'wid',
      \   args: ['lsp'],
      \   rootSearch: ['.git/'],
      \ }])
```

`path` is looked up in `$PATH`; give the full path to `wid` if it isn't on
it. A name in `rootSearch` that ends in `/` is a directory. Pass options with
`initializationOptions: #{collections: #{shared: '../shared'}}`. When a
plugin manager loads yegappan/lsp after your vimrc, wrap the call in
`autocmd User LspSetup …` as the client's `:help lsp` shows.

## VS Code

VS Code needs an extension to start a language server. A generic LSP client
extension works when it is set to run `wid lsp` for the `wid` language on
`.wid` files. A minimal extension of your own uses `vscode-languageclient`:
its `package.json` contributes the language (`"languages": [{ "id": "wid",
"extensions": [".wid"] }]`) and activates on it (`"onLanguage:wid"`), and its
`extension.js` starts the server:

```js
const { LanguageClient } = require("vscode-languageclient/node");

let client;

exports.activate = () => {
  client = new LanguageClient(
    "wid",
    "Wid",
    { command: "wid", args: ["lsp"] },
    {
      documentSelector: [{ scheme: "file", language: "wid" }],
      // initializationOptions: { collections: { shared: "../shared" } },
    },
  );
  return client.start();
};

exports.deactivate = () => client?.stop();
```

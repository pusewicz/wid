" Vim filetype plugin
" Language: Wid
"
" Set g:wid_recommended_style to 0 to keep your own indentation settings
" instead of the two spaces `wid fmt` uses.

if exists('b:did_ftplugin')
  finish
endif
let b:did_ftplugin = 1

let s:cpo_save = &cpo
set cpo&vim

" A comment leader is `#` and a blank: a line starting with `#{` is a splice.
setlocal comments=b:# commentstring=#\ %s
setlocal formatoptions-=t formatoptions+=croql
setlocal suffixesadd=.wid
setlocal include=
let &l:define = '^\s*\%(private\s\+\)\=\%(macro\s\+\)\=\%(def\s\+\%(self\.\)\=\|\%(struct\|enum\|union\|module\)\s\+\)'
let b:undo_ftplugin = 'setlocal comments< commentstring< formatoptions< suffixesadd< include< define<'

if get(g:, 'wid_recommended_style', 1)
  setlocal expandtab shiftwidth=2 softtabstop=2
  let b:undo_ftplugin .= ' | setlocal expandtab< shiftwidth< softtabstop<'
endif

" matchit: `%` jumps between a block's opening keyword, its `else`/`elsif`/
" `when` and its `end`. Modifiers (`x if y`), endless defs, method names
" (`x.end`), symbols, strings and comments are skipped.
let b:match_ignorecase = 0
let b:match_words =
      \ '\<\%(def\|struct\|enum\|module\|extend\|if\|unless\|while\|until\|for\|case\|do\|guard\)\>[?!]\@!'
      \ . ':\<\%(else\|elsif\|when\)\>[?!]\@!'
      \ . ':\<end\>[?!]\@!'
let b:match_skip = 's:\<wid\%(Comment\|Todo\|String\|RawString\|StringDelimiter\|Escape\|RawEscape\|'
      \ . 'EscapeError\|InterpolationDelimiter\|Symbol\|ConditionalModifier\|RepeatModifier\|Member\|'
      \ . 'EndlessDefine\|Attribute\|AttributeDelimiter\|EnumMember\)$'
let b:undo_ftplugin .= ' | unlet! b:match_ignorecase b:match_words b:match_skip'

let &cpo = s:cpo_save
unlet s:cpo_save

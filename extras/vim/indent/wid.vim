" Vim indent file
" Language: Wid
"
" Needs syntax highlighting: the syntax groups tell code from strings,
" comments and symbols, and `x if y` modifiers and endless defs from the
" keywords that open a block.

if exists('b:did_indent')
  finish
endif
let b:did_indent = 1

setlocal autoindent nolisp nosmartindent
setlocal indentexpr=GetWidIndent(v:lnum)
setlocal indentkeys=0{,0},0),0],!^F,o,O,=end,=else,=elsif,=when,0.
let b:undo_indent = 'setlocal autoindent< indentexpr< indentkeys< lisp< smartindent<'

if exists('*GetWidIndent')
  finish
endif

let s:cpo_save = &cpo
set cpo&vim

" Text in these groups never opens, closes or continues a statement.
let s:not_code = '^wid\%(Comment\|Todo\|String\|RawString\|StringDelimiter\|Escape\|RawEscape\|EscapeError\|'
      \ . 'InterpolationDelimiter\|Symbol\|ConditionalModifier\|RepeatModifier\|Member\|EndlessDefine\|'
      \ . 'Attribute\|AttributeDelimiter\|EnumMember\)$'
let s:in_string = '^wid\%(String\|RawString\|Escape\|RawEscape\|EscapeError\)$'
let s:comment = '^wid\%(Comment\|Todo\)$'
let s:skip = "synIDattr(synID(line('.'), col('.'), 1), 'name') =~# '" . s:not_code . "'"

let s:open = '\<\%(def\|struct\|enum\|module\|extend\|if\|unless\|while\|until\|for\|case\|do\|guard\)\>[?!]\@!\|[[({]'
let s:close = '\<end\>[?!]\@!\|[])}]'
let s:token = s:open . '\|' . s:close
let s:leading_close = '^\s*\%(\<end\>[?!]\@!\|[])}]\)'
let s:leading_branch = '^\s*\<\%(else\|elsif\|when\)\>[?!]\@!'
" `.method` (or `&.method`) at the start of a line continues the line above.
let s:leading_call = '^\s*&\=\.\.\@!'

" Returns the syntax group name at line {lnum}, byte column {col}.
function! s:Group(lnum, col) abort
  return synIDattr(synID(a:lnum, a:col, 1), 'name')
endfunction

" Returns true when the character at {lnum}, {col} is code: not in a string,
" comment or symbol, and not a modifier `if` or an endless `def`.
function! s:IsCode(lnum, col) abort
  return s:Group(a:lnum, a:col) !~# s:not_code
endfunction

" Returns the nearest line above {lnum} that holds code, skipping blank and
" comment-only lines, or 0.
function! s:PrevCodeLine(lnum) abort
  let lnum = prevnonblank(a:lnum - 1)
  while lnum > 0 && s:Group(lnum, match(getline(lnum), '\S') + 1) =~# s:comment
    let lnum = prevnonblank(lnum - 1)
  endwhile
  return lnum
endfunction

" Returns the code of line {lnum} without its comment and trailing blanks.
function! s:Code(lnum) abort
  let line = getline(a:lnum)
  let idx = stridx(line, '#')
  while idx >= 0
    if s:Group(a:lnum, idx + 1) =~# s:comment
      let line = strpart(line, 0, idx)
      break
    endif
    let idx = stridx(line, '#', idx + 1)
  endwhile
  return substitute(line, '\s\+$', '', '')
endfunction

" Scans line {lnum} for block keywords and brackets. Returns [unclosed
" openers, closers of earlier lines' openers, byte column of the last such
" closer]. With {branch} set, a leading `else`, `elsif` or `when` counts as
" an opener: it ends one branch and opens the next.
function! s:Balance(lnum, branch) abort
  let line = getline(a:lnum)
  let opens = a:branch && line =~# s:leading_branch && s:IsCode(a:lnum, match(line, '\S') + 1) ? 1 : 0
  let closes = 0
  let close_col = 0
  let start = 0
  while 1
    let [tok, idx, end] = matchstrpos(line, s:token, start)
    if idx < 0
      break
    endif
    let start = end
    if !s:IsCode(a:lnum, idx + 1)
      continue
    endif
    if tok =~# '^\%(end\|[])}]\)$'
      if opens > 0
        let opens -= 1
      else
        let closes += 1
        let close_col = idx + 1
      endif
    else
      let opens += 1
    endif
  endwhile
  return [opens, closes, close_col]
endfunction

" Returns the line of the opener that the code at {lnum}, {col} closes, or 0.
function! s:OpenerOf(lnum, col) abort
  call cursor(a:lnum, a:col)
  return searchpair(s:open, '', s:close, 'bW', s:skip)
endfunction

" Follows the closers on line {lnum} back to the line of the outermost opener
" they close, so `)` or `end` on its own line is measured from the line that
" opened it. Returns {lnum} when it closes nothing from an earlier line.
function! s:OpenerLine(lnum) abort
  let lnum = a:lnum
  while 1
    let [_, closes, close_col] = s:Balance(lnum, 0)
    if closes == 0
      return lnum
    endif
    let open = s:OpenerOf(lnum, close_col)
    if open <= 0 || open >= lnum
      return lnum
    endif
    let lnum = open
  endwhile
endfunction

" Returns how many blocks or brackets lines {first} to {last} leave open,
" counting a branch that {first} starts as open.
function! s:Open(first, last) abort
  let depth = 0
  for lnum in range(a:first, a:last)
    let [opens, closes, _] = s:Balance(lnum, lnum == a:first)
    let depth += opens - closes
  endfor
  return depth
endfunction

" Returns true when line {lnum} ends where the statement can't: after an
" operator, a comma or a `\`, or inside a string that goes on.
function! s:Continues(lnum) abort
  let code = s:Code(a:lnum)
  if code ==# ''
    return 0
  endif
  let group = s:Group(a:lnum, strlen(code))
  if group =~# s:in_string
    return 1
  endif
  if group =~# s:not_code || group =~# '^wid\%(Function\|OperatorMethod\|Uninitialized\)$'
    return 0
  endif
  " Block parameters end in `|` but open a block rather than continue.
  if code =~# '\%(\<do\|{\)\s*|[^|]*|$'
    return 0
  endif
  return code =~# '\%([-+*/%=<>&|~,\\]\|\%(^\|[^.]\)\.\)$'
endfunction

" Returns true when line {lnum} is directly inside a bracket rather than a
" block, where a trailing comma separates items instead of continuing a line.
function! s:InBracket(lnum) abort
  call cursor(a:lnum, 1)
  let [lnum, col] = searchpairpos(s:open, '', s:close, 'bW', s:skip)
  return lnum > 0 && getline(lnum)[col - 1] =~# '[[({]'
endfunction

" Returns the first line of the statement that line {lnum} belongs to: closers
" lead back to their openers and continued lines to the line they continue.
function! s:StatementStart(lnum) abort
  let lnum = s:OpenerLine(a:lnum)
  while 1
    let prev = s:PrevCodeLine(lnum)
    if prev == 0
      return lnum
    endif
    let prev_start = s:OpenerLine(prev)
    if s:Open(prev_start, prev) > 0
      return lnum
    endif
    if getline(lnum) !~# s:leading_call && !s:Continues(prev)
      return lnum
    endif
    let lnum = prev_start
  endwhile
endfunction

" Returns true when line {lnum} starts inside a string that began above it.
function! s:InsideString(lnum) abort
  if a:lnum <= 1
    return 0
  endif
  let prev = getline(a:lnum - 1)
  if prev ==# ''
    return s:InsideString(a:lnum - 1)
  endif
  return s:Group(a:lnum - 1, strlen(prev)) =~# s:in_string
endfunction

" Returns the indent of line {lnum}.
function! s:Indent(lnum) abort
  if s:InsideString(a:lnum)
    return -1
  endif
  let line = getline(a:lnum)
  let first = match(line, '\S') + 1

  " `end` and closing brackets line up with the line that opened them.
  if line =~# s:leading_close && s:IsCode(a:lnum, first)
    let open = s:OpenerOf(a:lnum, first)
    return open > 0 ? indent(s:OpenerLine(open)) : -1
  endif

  " `else`, `elsif` and `when` line up with their `if` or `case`, except that
  " the branches of `x = case y` are indented.
  if line =~# s:leading_branch && s:IsCode(a:lnum, first)
    call cursor(a:lnum, first)
    let [open, col] = searchpairpos(s:open, '', s:close, 'bW', s:skip)
    if open <= 0
      return -1
    endif
    let open_line = getline(open)
    let ind = indent(s:OpenerLine(open))
    if open_line[col - 1 :] =~# '^case\>' && col - 1 > match(open_line, '\S')
      let ind += shiftwidth()
    endif
    return ind
  endif

  let prev = s:PrevCodeLine(a:lnum)
  if prev == 0
    return 0
  endif
  let prev_start = s:OpenerLine(prev)
  if s:Open(prev_start, prev) > 0
    return indent(prev_start) + shiftwidth()
  endif
  let start = s:StatementStart(prev_start)
  if line =~# s:leading_call
    return indent(start) + shiftwidth()
  endif
  if s:Continues(prev)
    if s:Code(prev) =~# ',$' && s:InBracket(prev_start)
      return indent(prev)
    endif
    return indent(start) + shiftwidth()
  endif
  return indent(start)
endfunction

" Returns the indent of line {lnum} for 'indentexpr', or -1 to keep it.
function! GetWidIndent(lnum) abort
  let view = winsaveview()
  try
    return s:Indent(a:lnum)
  finally
    call winrestview(view)
  endtry
endfunction

let &cpo = s:cpo_save
unlet s:cpo_save

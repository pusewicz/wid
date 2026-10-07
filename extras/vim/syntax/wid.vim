" Vim syntax file
" Language:  Wid
" Filenames: *.wid
"
" The rules follow the lexer (crates/wid_syntax/src/lexer.rs). Set
" g:wid_highlight_operators to highlight operators as well.

if exists('b:current_syntax')
  finish
endif

let s:cpo_save = &cpo
set cpo&vim

syn case match
syn sync minlines=500

" Operators come first so that every more specific match starting at the same
" column (symbols, `---`, ranges in literals) wins over them.
if get(g:, 'wid_highlight_operators', 0)
  syn match widOperator /[-+*\/%=<>~^&|]\|\w\@1<!!\|!=\|\.\.\.\=\|\s\zs?\ze\s/
endif

syn match widNumber /\<\d[0-9_]*\>/
syn match widNumber /\<0[xX][[:xdigit:]_]\+\>/
syn match widNumber /\<0[bB][01_]\+\>/
syn match widNumber /\<0[oO][0-7_]\+\>/
" A float needs a digit after the dot, so `1.to_f` and `0..9` stay integers.
syn match widFloat /\<\d[0-9_]*\%(\.\d[0-9_]*\%([eE][-+]\=\d\+\)\=\|[eE][-+]\=\d\+\)\>/

" An uppercase name is a constant to the compiler; by convention PascalCase
" names a type and SCREAMING_CASE a value.
syn match widType /\<\u\w*\>/
syn match widConstant /\<\u[A-Z0-9_]\+\>/
syn keyword widBuiltinType Int UInt I8 I16 I32 I64 U8 U16 U32 U64 F32 F64 Bool Rune
syn keyword widBuiltinType String CString RawPtr TypeId Any Error Never Type Code Symbol Self
syn keyword widBuiltinType Context Allocator AllocMode Location Logger Os Arch FieldInfo MethodInfo
syn keyword widBuiltinType TypeInfo TypeKind TypeInfoField TypeInfoMember

" The lexer's reserved words (`Keyword` in crates/wid_syntax/src/token.rs) by
" highlight group, with extra arguments for the match. A reserved word followed
" by `?` or `!` is a method name (`nil?`). The wid_syntax test editor_syntax.rs
" keeps this list in sync with the lexer.
let s:reserved = [
      \ ['widConditional', 'if unless elsif else then case when guard'],
      \ ['widRepeat', 'while until for in loop'],
      \ ['widControl', 'return break next yield defer'],
      \ ['widKeyword', 'do end using overload private'],
      \ ['widStructure', 'struct enum union module extend'],
      \ ['widInclude', 'import cimport include'],
      \ ['widComptime', 'comptime quote'],
      \ ['widMacro', 'macro'],
      \ ['widDefine', 'def', 'nextgroup=widDefSelf,widFunction,widOperatorMethod skipwhite'],
      \ ['widBoolean', 'true false'],
      \ ['widNil', 'nil'],
      \ ['widPseudoVariable', 'self'],
      \ ]
for s:entry in s:reserved
  exe 'syn match' s:entry[0] '/\<\%(' . join(split(s:entry[1]), '\|') . '\)\>[?!]\@!/' get(s:entry, 2, '')
endfor
unlet s:entry s:reserved
syn match widPseudoVariable /\<\%(context\|caller_location\)\>[?!]\@!/
" Inside an enum, `struct`, `enum` or `union` standing alone names a member
" (the prelude's `TypeKind`) and opens no block. It stays unhighlighted, like
" other members.
syn match widEnumMember /\<\%(struct\|enum\|union\)\>\ze\s*\%(=[=~>]\@!\|,\|#\|$\)/

" `x if cond`: after the end of an expression, `if`, `unless`, `while` and
" `until` are modifiers and open no block. The indent and matchit rules rely on
" these groups.
let s:after_expr = '\%(\%(\w\|[)\]}"''?!]\)\s\+\)\@<=\%(\<\%(comptime\|then\|else\)\s\+\|@\[[^]]*\]\s*\)\@<!'
exe 'syn match widConditionalModifier /' . s:after_expr . '\<\%(if\|unless\)\>[?!]\@!/'
exe 'syn match widRepeatModifier /' . s:after_expr . '\<\%(while\|until\)\>[?!]\@!/'
unlet s:after_expr

" `def f(x: Int) -> Int = x * 2` has no `end`. In a `quote`, the name may be
" a splice, `def #{name} = @#{name}`.
let s:method_name = '\%(self\.\)\=\%(#{[^}]*}\|\h\w*[?!]\=\|\[\]=\=\|<=>\|\*\*\|[=!<>]=\|<<\|>>\|[-+*\/%<>!~&|]\)'
let s:params = '\%(([^()]*\%(([^()]*)[^()]*\)*)\)\='
let s:return_type = '\%(\s*->[^=#]\{-1,}\)\='
exe 'syn match widEndlessDefine /\<def\>\ze\s\+' . s:method_name . s:params . s:return_type
      \ . '\s\+=[=~>]\@!/ nextgroup=widDefSelf,widFunction,widOperatorMethod skipwhite'
unlet s:method_name s:params s:return_type
syn match widFunction /\h\w*[?!]\=/ contained
syn match widOperatorMethod /\[\]=\=\|<=>\|\*\*\|[=!<>]=\|<<\|>>\|[-+*\/%<>!~&|]/ contained
syn match widDefSelf /\<self\./ contained nextgroup=widFunction,widOperatorMethod

" Built-in procedures, unless the name is being assigned or passed by name.
syn match widBuiltin /\<\%(puts\|print\|method\|alloc\|embed\|config\|free\|free_all\|size_of\|align_of\|type_info\|panic\|unreachable\|assert\)\>[?!]\@!\%(\s*\%([-+*\/%&|~<>]\{,2}\)=\%([^=]\|$\)\|:\)\@!/
" `p` is also a common local name, so it only counts when it is called.
syn match widBuiltin /\<p\>\%((\|\s\+\%(\<in\>\|[-+*\/%=<>!&|^~.?,)\]}#]\)\@!\S\)\@=/

" Only where a type is expected.
syn match widTypeKeyword /\<\%(map\|matrix\)\ze\[/
syn match widTypeKeyword /\[\zsdynamic\ze\]/
syn match widTypeKeyword /\<proc\ze\s*(/
syn match widTypeKeyword /\%(:\s*\)\@<=block\>\|\<block\ze\s*(/
syn match widTypeKeyword /\<distinct\ze\s\+\S/

" A method call or field access; keeps `x.end` or `x.p` from highlighting as
" a keyword or built-in.
syn match widMember /\.\@1<!\.\%(\l\|_\)\w*\%([?!]=\@!\)\=/

" `:north` and `:+`, but not the colon of `name: value`.
let s:unattached = '\%(\w\|[)\]"''?!]\)\@1<!'
exe 'syn match widSymbol /' . s:unattached . ':\h\w*\%([?!]=\@!\|=[=>~]\@!\)\=/'
exe 'syn match widSymbol /' . s:unattached . ':\%(\[\]=\=\|<=>\|\*\*\|[=!<>]=\|<<\|>>\|[-+*\/%<>!]\)/'
unlet s:unattached

syn match widInstanceVariable /@\h\w*/
syn match widTypeParam /\$\h\w*/
syn match widUninitialized /---/

syn region widAttribute matchgroup=widAttributeDelimiter start=/@\[/ end=/\]/ oneline
      \ contains=widString,widRawString,widNumber,widFloat,widSymbol,widType,widConstant,widBuiltinType

syn match widEscapeError /\\./ contained
syn match widEscape /\\[ntr0eab\\"'#]/ contained
syn match widEscape /\\x[0-7][[:xdigit:]]/ contained
syn match widEscape /\\u{[[:xdigit:]]\{1,6}}/ contained
syn match widEscape /\\$/ contained
syn match widRawEscape /\\[\\']/ contained

syn cluster widExpr contains=widOperator,widNumber,widFloat,widType,widConstant,widBuiltinType,
      \widConditional,widConditionalModifier,widRepeat,widRepeatModifier,widControl,widKeyword,
      \widStructure,widInclude,widComptime,widBoolean,widNil,widPseudoVariable,widBuiltin,
      \widTypeKeyword,widMember,widSymbol,widInstanceVariable,widTypeParam,widUninitialized,
      \widString,widRawString,widNestedBraces,widSplice
syn region widInterpolation matchgroup=widInterpolationDelimiter start=/#{/ end=/}/ contained oneline
      \ contains=@widExpr
syn region widNestedBraces start=/{/ end=/}/ contained oneline transparent contains=@widExpr

syn region widString matchgroup=widStringDelimiter start=/"/ skip=/\\\\\|\\"/ end=/"/
      \ contains=widEscapeError,widEscape,widInterpolation
syn region widRawString matchgroup=widStringDelimiter start=/'/ skip=/\\\\\|\\'/ end=/'/
      \ contains=widRawEscape

" Outside a string, `#{…}` is a macro splice (valid inside `quote`), with
" `@#{name}` for a field and `:#{name}` for a symbol. It is code, may span
" lines, and its delimiters highlight like `quote`.
syn region widSplice matchgroup=widSpliceDelimiter
      \ start=/\%(@\|\%(\w\|[)\]}"'?!]\)\@1<!:\)\=#{/ end=/}/ contains=@widExpr

syn keyword widTodo TODO FIXME XXX NOTE contained
" A comment is `#` and anything but `{`, to the end of the line.
syn match widComment /#{\@!.*$/ contains=widTodo,@Spell

hi def link widOperator Operator
hi def link widNumber Number
hi def link widFloat Float
hi def link widType Type
hi def link widConstant Constant
hi def link widBuiltinType Type
hi def link widConditional Conditional
hi def link widConditionalModifier widConditional
hi def link widRepeat Repeat
hi def link widRepeatModifier widRepeat
hi def link widControl Statement
hi def link widKeyword Keyword
hi def link widStructure Structure
hi def link widInclude Include
hi def link widComptime PreProc
hi def link widMacro Define
hi def link widDefine Define
hi def link widEndlessDefine widDefine
hi def link widFunction Function
hi def link widOperatorMethod widFunction
hi def link widDefSelf widPseudoVariable
hi def link widBoolean Boolean
hi def link widNil Constant
hi def link widPseudoVariable Constant
hi def link widBuiltin Function
hi def link widTypeKeyword Type
hi def link widSymbol Constant
hi def link widInstanceVariable Identifier
hi def link widTypeParam Type
hi def link widUninitialized Special
hi def link widAttribute PreProc
hi def link widAttributeDelimiter PreProc
hi def link widString String
hi def link widRawString String
hi def link widStringDelimiter String
hi def link widEscape SpecialChar
hi def link widRawEscape SpecialChar
hi def link widEscapeError Error
hi def link widInterpolationDelimiter Delimiter
hi def link widSpliceDelimiter widComptime
hi def link widTodo Todo
hi def link widComment Comment

let b:current_syntax = 'wid'

let &cpo = s:cpo_save
unlet s:cpo_save

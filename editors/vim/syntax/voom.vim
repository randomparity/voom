" Vim syntax file
" Language: VOOM DSL (.voom)
" Maintainer: VOOM contributors

if exists('b:current_syntax')
  finish
endif

" Comments
syn match voomComment "//.*$" contains=voomTodo
syn keyword voomTodo TODO FIXME XXX NOTE contained

" Strings (double-quoted with escapes)
syn region voomString start=/"/ skip=/\\"/ end=/"/ contains=voomEscape,voomInterpolation
syn match voomEscape /\\[\\"]/ contained
syn match voomInterpolation /{[^}]*}/ contained

" Numbers
syn match voomNumber /\<\d\+\(\.\d\+\)\?\>/
syn match voomNumberSuffix /\<\d\+[a-z]\+\>/ contains=voomNumber

" Booleans
syn keyword voomBoolean true false

" Top-level structure keywords
syn keyword voomStructure policy config phase

" Config keywords
syn keyword voomConfigKey languages on_error commentary_patterns

" Phase control
syn keyword voomPhaseControl depends_on run_if skip_when
syn keyword voomTrigger modified completed

" Track targets
syn keyword voomTrackTarget audio subtitle subtitles video attachments track

" Operations
syn keyword voomOperation container keep remove order defaults actions
syn keyword voomOperation transcode to synthesize
syn keyword voomOperation clear_tags set_tag delete_tag

" Action keywords (in when/rules blocks)
syn keyword voomAction skip warn fail
syn keyword voomAction set_default set_forced set_language

" Rules keywords
syn keyword voomRules rules rule
syn keyword voomRulesMode first all

" Condition keywords
syn keyword voomCondition exists count
syn keyword voomCondition audio_is_multi_language is_dubbed is_original

" Logical operators
syn keyword voomLogical and or not

" Filter keywords
syn keyword voomFilter where in
syn match voomFilter /\<contains\>/
syn match voomFilter /\<matches\>/
syn keyword voomFilter commentary forced default font

" Synthesize settings
syn keyword voomSynthKey codec channels source prefer bitrate
syn keyword voomSynthKey skip_if_exists create_if title language
syn keyword voomSynthKey inherit position after_source last

" Transcode settings
syn keyword voomTranscodeKey crf preset max_resolution scale_algorithm
syn keyword voomTranscodeKey hw hw_fallback hdr_mode tune pixel_format
syn keyword voomTranscodeKey preserve

" Actions block settings
syn keyword voomActionsKey clear_all_default clear_all_forced clear_all_titles

" When/else
syn keyword voomConditional when else

" Comparison operators
syn match voomOperator /[!=<>]=\?/
syn match voomOperator /==/

" Field access (dotted paths like plugin.radarr.title)
syn match voomFieldAccess /\<[a-zA-Z_][a-zA-Z0-9_-]*\(\.[a-zA-Z_][a-zA-Z0-9_-]*\)\+/

" Delimiters
syn match voomDelimiter /[{}()\[\]:,]/

" Highlighting links
hi def link voomComment Comment
hi def link voomTodo Todo
hi def link voomString String
hi def link voomEscape SpecialChar
hi def link voomInterpolation Special
hi def link voomNumber Number
hi def link voomNumberSuffix Number
hi def link voomBoolean Boolean
hi def link voomStructure Structure
hi def link voomConfigKey Keyword
hi def link voomPhaseControl Keyword
hi def link voomTrigger Constant
hi def link voomTrackTarget Type
hi def link voomOperation Statement
hi def link voomAction Statement
hi def link voomRules Keyword
hi def link voomRulesMode Constant
hi def link voomCondition Function
hi def link voomLogical Operator
hi def link voomFilter Keyword
hi def link voomSynthKey Identifier
hi def link voomTranscodeKey Identifier
hi def link voomActionsKey Identifier
hi def link voomConditional Conditional
hi def link voomOperator Operator
hi def link voomFieldAccess Special
hi def link voomDelimiter Delimiter

let b:current_syntax = 'voom'

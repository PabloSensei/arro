; Keywords, declarations, strings and comments of a C source file.

[
  "auto" "break" "case" "const" "continue" "default" "do" "else" "enum"
  "extern" "for" "goto" "if" "inline" "register" "restrict" "return"
  "sizeof" "static" "struct" "switch" "typedef" "union" "volatile" "while"
  "_Atomic" "_Generic" "_Noreturn"
] @keyword

(preproc_directive) @keyword
[
  "#define"
  "#elif"
  "#elifdef"
  "#elifndef"
  "#else"
  "#endif"
  "#if"
  "#ifdef"
  "#ifndef"
  "#include"
] @keyword

[
  (primitive_type)
  (sized_type_specifier)
  (type_identifier)
] @keyword

[
  (char_literal)
  (string_literal)
  (system_lib_string)
] @string

(number_literal) @number
(null) @constant

(comment) @comment

(field_identifier) @field

(call_expression
  function: (identifier) @function)
(call_expression
  function: (field_expression
    field: (field_identifier) @function))
(function_declarator
  declarator: (identifier) @function)
(preproc_function_def
  name: (identifier) @function)

; Keywords, declarations, strings and comments of a C++ source file.

[
  "alignas" "alignof" "and" "and_eq" "asm" "bitand" "bitor" "catch"
  "class" "compl" "concept" "consteval" "constexpr" "constinit" "co_await"
  "co_return" "co_yield" "decltype" "delete" "explicit" "friend"
  "mutable" "namespace" "new" "noexcept" "not" "not_eq" "operator" "or"
  "or_eq" "private" "protected" "public" "requires" "static_assert"
  "template" "thread_local" "throw" "try" "typename" "using" "virtual"
  "xor" "xor_eq"
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
  (raw_string_literal)
  (string_literal)
  (system_lib_string)
] @string

(number_literal) @number

[
  (false)
  (null)
  (true)
] @constant
(null "nullptr" @constant)

(comment) @comment

(field_identifier) @field

(call_expression
  function: (identifier) @function)
(call_expression
  function: (field_expression
    field: (field_identifier) @function))
(call_expression
  function: (qualified_identifier
    name: (identifier) @function))
(function_declarator
  declarator: (identifier) @function)
(function_declarator
  declarator: (field_identifier) @function)
(function_declarator
  declarator: (qualified_identifier
    name: (identifier) @function))
(preproc_function_def
  name: (identifier) @function)

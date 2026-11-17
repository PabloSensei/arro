; Keywords, declarations, strings and comments of a Go source file.

[
  "break" "case" "chan" "const" "continue" "default" "defer" "else"
  "fallthrough" "for" "func" "go" "goto" "if" "import" "interface"
  "map" "package" "range" "return" "select" "struct" "switch" "type"
  "var"
] @keyword

(type_identifier) @keyword

[
  (interpreted_string_literal)
  (raw_string_literal)
  (rune_literal)
] @string

[
  (float_literal)
  (imaginary_literal)
  (int_literal)
] @number

[
  (false)
  (iota)
  (nil)
  (true)
] @constant

(comment) @comment

(field_identifier) @field

(function_declaration
  name: (identifier) @function)
(method_declaration
  name: (field_identifier) @function)
(call_expression
  function: (identifier) @function)
(call_expression
  function: (selector_expression
    field: (field_identifier) @function))

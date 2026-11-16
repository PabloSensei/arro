; Keywords, declarations, strings and comments of a Lua source file.

[
  "and" "do" "else" "elseif" "end" "for" "function" "goto"
  "if" "in" "local" "not" "or" "repeat" "return" "then" "until"
  "while"
] @keyword

(break_statement) @keyword

[
  (false)
  (nil)
  (true)
] @constant

(number) @number
(string) @string
(comment) @comment

(function_declaration
  name: (identifier) @function)
(function_declaration
  name: (dot_index_expression
    field: (identifier) @function))
(function_call
  name: (identifier) @function)
(function_call
  name: (dot_index_expression
    field: (identifier) @function))

(field
  name: (identifier) @field)
(dot_index_expression
  field: (identifier) @field)

((identifier) @constant
 (#match? @constant "^[A-Z][A-Z_0-9]*$"))

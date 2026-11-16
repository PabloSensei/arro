; Keywords, declarations, strings and comments of a PowerShell script.

[
  "param" "dynamicparam" "begin" "process" "end" "if" "elseif" "else"
  "switch" "foreach" "for" "while" "do" "until" "function" "filter"
  "workflow" "break" "continue" "throw" "return" "exit" "trap" "try"
  "catch" "finally" "data" "inlinescript" "parallel" "sequence"
] @keyword

(type_spec) @keyword

(string_literal) @string

[
  (integer_literal)
  (real_literal)
] @number

(comment) @comment

(command
  command_name: (command_name) @function)
(function_statement
  (function_name) @function)
(invokation_expression
  (member_name) @function)

(member_access
  (member_name) @field)

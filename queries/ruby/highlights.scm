; Keywords, declarations, strings and comments of a Ruby source file.

[
  "alias" "and" "begin" "break" "case" "class" "def" "do" "else"
  "elsif" "end" "ensure" "for" "if" "in" "module" "next" "or" "rescue"
  "retry" "return" "then" "unless" "until" "when" "while" "yield"
] @keyword

((identifier) @keyword
 (#match? @keyword "^(private|protected|public)$"))

(constant) @keyword

[
  (heredoc_beginning)
  (heredoc_body)
  (subshell)
  (string)
] @string

[
  (float)
  (integer)
] @number

[
  (false)
  (nil)
  (true)
] @constant

(comment) @comment

[
  (class_variable)
  (instance_variable)
] @field

(method
  name: [(identifier) (constant)] @function)
(singleton_method
  name: [(identifier) (constant)] @function)
(call
  method: [(identifier) (constant)] @function)

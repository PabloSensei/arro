; Keywords, declarations, strings and comments of a Python source file.

[
  "as" "assert" "async" "await" "break" "class" "continue" "def" "del"
  "elif" "else" "except" "exec" "finally" "for" "from" "global" "if"
  "import" "lambda" "nonlocal" "pass" "print" "raise" "return" "try"
  "while" "with" "yield" "match" "case"
] @keyword

[
  (none)
  (true)
  (false)
] @constant

[
  (integer)
  (float)
] @number

(comment) @comment
(string) @string

(decorator
  (identifier) @annotation)
(decorator
  (attribute
    attribute: (identifier) @annotation))

(function_definition
  name: (identifier) @function)
(call
  function: (identifier) @function)
(call
  function: (attribute
    attribute: (identifier) @function))

(attribute
  attribute: (identifier) @field)
(keyword_argument
  name: (identifier) @field)
(assignment
  left: (identifier) @field)

(class_definition
  name: (identifier) @keyword)
(type
  (identifier) @keyword)

((identifier) @constant
 (#match? @constant "^[A-Z][A-Z_]*$"))

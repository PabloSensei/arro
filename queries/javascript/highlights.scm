; Keywords, declarations, strings and comments of a JavaScript file.

[
  "as" "async" "await" "break" "case" "catch" "class" "const" "continue"
  "debugger" "default" "delete" "do" "else" "export" "extends" "finally"
  "for" "from" "function" "get" "if" "import" "in" "instanceof" "let" "meta"
  "new" "of" "return" "set" "static" "switch" "target" "throw" "try" "typeof"
  "using" "var" "void" "while" "with" "yield"
] @keyword

[
  (true)
  (false)
  (null)
  (undefined)
] @constant

[
  (this)
  (super)
] @keyword

(comment) @comment

[
  (string)
  (template_string)
  (regex)
] @string

(number) @number

(function_declaration
  name: (identifier) @function)
(function_expression
  name: (identifier) @function)
(generator_function_declaration
  name: (identifier) @function)
(method_definition
  name: (property_identifier) @function)
(call_expression
  function: (identifier) @function)

(property_identifier) @field
(shorthand_property_identifier) @field

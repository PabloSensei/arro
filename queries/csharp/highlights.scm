; Keywords, declarations, strings and comments of a C# source file.

[
  (modifier)
  "this"
  (implicit_type)
] @keyword

[
  "add" "alias" "as" "base" "break" "case" "catch" "checked" "class"
  "continue" "default" "delegate" "do" "else" "enum" "event" "explicit"
  "extern" "finally" "for" "foreach" "global" "goto" "if" "implicit"
  "interface" "is" "lock" "namespace" "notnull" "operator" "params"
  "record" "remove" "return" "sizeof" "stackalloc" "struct" "switch"
  "throw" "try" "typeof" "unchecked" "using" "while" "new" "await" "in"
  "yield" "get" "set" "when" "out" "ref" "from" "where" "select" "init"
  "with" "let"
] @keyword

(predefined_type) @keyword

[
  (class_declaration name: (identifier) @keyword)
  (struct_declaration name: (identifier) @keyword)
  (interface_declaration name: (identifier) @keyword)
  (record_declaration name: (identifier) @keyword)
  (enum_declaration name: (identifier) @keyword)
]

[
  (character_literal)
  (interpolated_string_expression)
  (raw_string_literal)
  (string_literal)
  (verbatim_string_literal)
] @string

[
  (integer_literal)
  (real_literal)
] @number

[
  (boolean_literal)
  (null_literal)
] @constant

(comment) @comment

(attribute
  name: (identifier) @annotation)

(method_declaration
  name: (identifier) @function)
(local_function_statement
  name: (identifier) @function)
(invocation_expression
  (member_access_expression
    name: (identifier) @function))
(invocation_expression
  (identifier) @function)

(property_declaration
  name: (identifier) @field)
(enum_member_declaration
  (identifier) @field)
(member_access_expression
  name: (identifier) @field)

; Keywords, declarations, strings and comments of a Scala source file.

[
  "abstract" "case" "catch" "class" "def" "do" "else" "enum" "export"
  "extends" "final" "finally" "for" "forSome" "given" "if" "implicit"
  "import" "lazy" "match" "new" "object" "override"
  "package" "private" "protected" "return" "sealed" "then" "throw"
  "trait" "try" "type" "using" "val" "var" "while"
  "with" "yield"
] @keyword

[
  (opaque_modifier)
  (open_modifier)
  (transparent_modifier)
] @keyword

(end_marker) @keyword
(extension_definition "extension" @keyword)

[
  (type_identifier)
] @keyword

(class_definition
  name: (identifier) @keyword)
(enum_definition
  name: (identifier) @keyword)
(object_definition
  name: (identifier) @keyword)
(trait_definition
  name: (identifier) @keyword)
(type_definition
  name: (type_identifier) @keyword)

[
  (boolean_literal)
  (null_literal)
] @constant

[
  (integer_literal)
  (floating_point_literal)
] @number

[
  (character_literal)
  (interpolated_string_expression)
  (string)
] @string

(comment) @comment
(block_comment) @comment

(annotation) @annotation

(function_definition
  name: (identifier) @function)
(function_declaration
  name: (identifier) @function)
(call_expression
  function: (identifier) @function)
(call_expression
  function: (field_expression
    field: (identifier) @function))

(field_expression
  field: (identifier) @field)
(val_definition
  pattern: (identifier) @field)
(var_definition
  pattern: (identifier) @field)

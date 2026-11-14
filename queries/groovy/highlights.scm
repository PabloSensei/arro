; Keywords, declarations, strings and comments of a Groovy source file.

[
  "abstract" "as" "assert" "break" "byte" "case" "catch" "char" "class"
  "continue" "def" "default" "do" "double" "else" "enum" "exports" "extends"
  "final" "finally" "float" "for" "if" "implements" "import" "in" "instanceof"
  "int" "interface" "long" "module" "native" "new" "open" "opens" "package"
  "permits" "private" "protected" "provides" "public" "record" "requires"
  "return" "sealed" "short" "static" "strictfp" "switch" "synchronized" "throw"
  "throws" "to" "transient" "transitive" "try" "uses" "volatile" "when" "while"
  "with" "yield"
] @keyword

[
  (boolean_type)
  (integral_type)
  (floating_point_type)
  (void_type)
  (this)
  (super)
  (true)
  (false)
  (null_literal)
] @keyword

((type_identifier) @keyword
 (#eq? @keyword "def"))

[
  (character_literal)
  (string_literal)
  (multiline_string_fragment)
  (string_fragment)
] @string

[
  (decimal_integer_literal)
  (hex_integer_literal)
  (octal_integer_literal)
  (binary_integer_literal)
  (decimal_floating_point_literal)
  (hex_floating_point_literal)
] @number

(line_comment) @comment
(block_comment) @comment
(shebang) @comment

(annotation
  "@" @annotation
  name: (_) @annotation)
(marker_annotation
  "@" @annotation
  name: (_) @annotation)

(method_declaration
  name: (identifier) @function)
(function_definition
  name: (identifier) @function)
(constructor_declaration
  name: (identifier) @function)
(compact_constructor_declaration
  name: (identifier) @function)

(field_declaration
  declarator: (variable_declarator
    name: (identifier) @field))
(field_access
  field: (identifier) @field)

(enum_constant
  name: (identifier) @constant)
((identifier) @constant
 (#match? @constant "^[A-Z][A-Z0-9_]+$"))

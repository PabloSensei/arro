; Colours follow what the IDE shows without semantic information: classes and locals
; stay plain, declarations of methods and fields are coloured.

[
  "abstract" "assert" "break" "case" "catch" "class" "continue" "default" "do"
  "else" "enum" "exports" "extends" "final" "finally" "for" "if" "implements"
  "import" "instanceof" "interface" "module" "native" "new" "non-sealed" "open"
  "opens" "package" "permits" "private" "protected" "provides" "public" "record"
  "requires" "return" "sealed" "static" "strictfp" "switch" "synchronized"
  "throw" "throws" "to" "transient" "transitive" "try" "uses" "volatile" "when"
  "while" "with" "yield"
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
 (#eq? @keyword "var"))

[
  (string_literal)
  (character_literal)
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
((block_comment) @comment.doc
 (#match? @comment.doc "^/[*][*]"))

(annotation
  "@" @annotation
  name: (_) @annotation)
(marker_annotation
  "@" @annotation
  name: (_) @annotation)

(method_declaration
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

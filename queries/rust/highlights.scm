; Keywords, declarations, strings and comments of a Rust source file.

[
  "as" "async" "await" "break" "const" "continue" "default"
  "dyn" "else" "enum" "extern" "fn" "for" "gen" "if" "impl" "in" "let"
  "loop" "macro_rules!" "match" "mod" "move" "pub" "raw" "ref"
  "return" "static" "struct" "trait" "type" "union"
  "unsafe" "use" "where" "while" "yield"
] @keyword

[
  (crate)
  (mutable_specifier)
  (self)
  (super)
] @keyword

[
  (primitive_type)
  (type_identifier)
] @keyword

[
  (char_literal)
  (raw_string_literal)
  (string_literal)
] @string

[
  (float_literal)
  (integer_literal)
] @number

(boolean_literal) @constant

(line_comment) @comment
(block_comment) @comment

(attribute_item) @annotation
(inner_attribute_item) @annotation

(function_item
  (identifier) @function)
(function_signature_item
  (identifier) @function)
(call_expression
  function: (identifier) @function)
(call_expression
  function: (field_expression
    field: (field_identifier) @function))
(call_expression
  function: (scoped_identifier
    name: (identifier) @function))
(macro_invocation
  macro: (identifier) @function)

(field_identifier) @field
(parameter
  (identifier) @field)

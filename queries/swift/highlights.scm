; Keywords, declarations, strings and comments of a Swift source file.

[
  "any" "as" "associatedtype" "async" "await" "break" "case"
  "class" "continue" "convenience" "deinit" "didSet"
  "do" "enum" "extension" "fallthrough" "false" "fileprivate"
  "for" "func" "get" "guard" "if" "import" "in" "indirect" "init"
  "inout" "internal" "is" "let" "nonisolated" "open" "operator"
  "override" "private" "protocol" "public" "repeat" "required"
  "return" "self" "set" "some" "static" "struct"
  "subscript" "super" "switch" "true" "try"
  "typealias" "unowned" "unsafe" "var" "weak" "while"
  "willSet"
] @keyword

[
  (catch_keyword)
  (default_keyword)
  (else)
  (throw_keyword)
  (throws)
  (where_keyword)
] @keyword

(type_identifier) @keyword

[
  (integer_literal)
  (hex_literal)
  (oct_literal)
  (bin_literal)
  (real_literal)
] @number

[
  (boolean_literal)
  (nil_literal)
] @constant

[
  (line_str_text)
  (multi_line_str_text)
  (raw_str_part)
] @string

[
  (comment)
  (multiline_comment)
] @comment

(attribute
  "@" @annotation
  (user_type
    (type_identifier) @annotation))

(function_declaration
  (simple_identifier) @function)
(protocol_function_declaration
  name: (simple_identifier) @function)
(call_expression
  (simple_identifier) @function)
(call_expression
  (navigation_expression
    (navigation_suffix
      (simple_identifier) @function)))

(navigation_expression
  (navigation_suffix
    (simple_identifier) @field))
(class_body
  (property_declaration
    (pattern
      (simple_identifier) @field)))

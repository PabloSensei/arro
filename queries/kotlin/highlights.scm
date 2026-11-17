; Keywords, declarations, strings and comments of a Kotlin source file.

[
  "abstract" "actual" "annotation" "as" "by" "catch" "class" "companion"
  "const" "constructor" "crossinline" "data" "delegate" "do" "dynamic" "else"
  "enum" "expect" "external" "field" "file" "final" "finally" "for" "fun"
  "get" "if" "import" "in" "infix" "init" "inline" "inner" "interface"
  "internal" "is" "lateinit" "noinline" "object" "open" "operator" "out"
  "override" "package" "param" "private" "property" "protected" "public"
  "receiver" "return" "sealed" "set" "setparam" "suspend" "tailrec" "throw"
  "try" "typealias" "val" "value" "var" "vararg" "when" "where" "while"
] @keyword

[
  (this_expression)
  (super_expression)
] @keyword

((identifier) @constant
 (#any-of? @constant "true" "false" "null"))

[
  (string_literal)
  (multiline_string_literal)
  (character_literal)
] @string

[
  (number_literal)
  (float_literal)
] @number

(line_comment) @comment
(block_comment) @comment
(shebang) @comment

(annotation
  (user_type
    (identifier) @annotation))
(annotation
  (constructor_invocation
    (user_type
      (identifier) @annotation)))

(function_declaration
  name: (identifier) @function)
(call_expression
  (identifier) @function)

(variable_declaration
  (identifier) @field)
(navigation_expression
  (identifier) @field)

(enum_entry
  (identifier) @constant)

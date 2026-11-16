; Keywords, declarations, strings and comments of a PHP source file.

[
  "and" "as" "break" "case" "catch" "class" "clone" "const" "continue"
  "declare" "default" "do" "echo" "else" "elseif" "enddeclare" "endfor"
  "endforeach" "endif" "endswitch" "endwhile" "enum" "exit" "extends"
  "finally" "fn" "for" "foreach" "function" "global" "goto" "if"
  "implements" "include" "include_once" "instanceof" "insteadof"
  "interface" "match" "namespace" "new" "or" "print" "require"
  "require_once" "return" "switch" "throw" "trait" "try" "use" "while"
  "xor" "yield" "yield from"
  (abstract_modifier)
  (final_modifier)
  (readonly_modifier)
  (static_modifier)
  (visibility_modifier)
] @keyword

[
  (cast_type)
  (named_type)
  (primitive_type)
] @keyword

[
  (class_declaration name: (name) @keyword)
  (interface_declaration name: (name) @keyword)
  (trait_declaration name: (name) @keyword)
]

[
  (encapsed_string)
  (heredoc)
  (heredoc_body)
  (nowdoc_body)
  (string)
  (string_content)
] @string

[
  (float)
  (integer)
] @number

[
  (boolean)
  (null)
] @constant

(comment) @comment

(function_definition
  name: (name) @function)
(function_call_expression
  function: (name) @function)
(function_call_expression
  function: (qualified_name
    (name) @function))
(method_declaration
  name: (name) @function)
(member_call_expression
  name: (name) @function)
(scoped_call_expression
  name: (name) @function)

(property_element
  (variable_name) @field)
(member_access_expression
  name: (name) @field)
(member_access_expression
  name: (variable_name
    (name)) @field)

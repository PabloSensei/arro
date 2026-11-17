; Keywords, declarations, types and comments of a TypeScript file.

[
  "abstract" "accessor" "any" "as" "assert" "asserts" "async" "await"
  "boolean" "break" "case" "catch" "class" "const" "continue" "debugger"
  "declare" "default" "delete" "do" "else" "enum" "export" "extends" "finally"
  "for" "from" "function" "get" "global" "if" "implements" "import" "in"
  "infer" "instanceof" "interface" "is" "keyof" "let" "meta" "module"
  "namespace" "never" "new" "number" "object" "of" "override" "private"
  "protected" "public" "readonly" "require" "return" "satisfies" "set"
  "static" "string" "switch" "symbol" "target" "throw" "try" "type" "typeof"
  "unknown" "using" "var" "void" "while" "with" "yield"
] @keyword

(type_identifier) @keyword
(predefined_type) @keyword

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
(interface_declaration
  name: (type_identifier) @function)
(type_alias_declaration
  name: (type_identifier) @function)
(enum_declaration
  name: (identifier) @function)

(property_identifier) @field
(shorthand_property_identifier) @field
(property_signature
  name: (property_identifier) @field)
(public_field_definition
  name: (property_identifier) @field)

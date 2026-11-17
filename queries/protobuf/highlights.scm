; Keywords, declarations, strings and comments of a Protobuf schema.

[
  "enum" "extend" "extensions" "group" "import" "map" "message" "oneof"
  "option" "package" "public" "reserved" "returns" "rpc" "service"
  "syntax" "to" "weak"
] @keyword

[
  (enum_name)
  (message_name)
  (message_or_enum_type)
  (service_name)
] @keyword

[
  (float_lit)
  (int_lit)
] @number

[
  (false)
  (true)
] @constant

[
  (reserved_identifier)
  (string)
] @string

(comment) @comment

(rpc_name) @function

(field
  (identifier) @field)
(map_field
  (identifier) @field)
(enum_field
  (identifier) @field)
(oneof_field
  (identifier) @field)

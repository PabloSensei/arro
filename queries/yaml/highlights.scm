; Keys, scalars and comments of a YAML document.

[
  (double_quote_scalar)
  (single_quote_scalar)
  (block_scalar)
  (string_scalar)
] @string

[
  (integer_scalar)
  (float_scalar)
] @number

[
  (boolean_scalar)
  (null_scalar)
] @constant

(comment) @comment

; The key of a mapping has the shape of a string, so it comes after the scalar patterns.
(block_mapping_pair
  key: (flow_node
    (plain_scalar
      (string_scalar) @field)))

(block_mapping_pair
  key: (flow_node
    [
      (double_quote_scalar)
      (single_quote_scalar)
    ] @field))

(flow_pair
  key: (flow_node
    (plain_scalar
      (string_scalar) @field)))

; Keys, strings, numbers and the constants of a JSON document.

(string) @string

(number) @number

[
  (null)
  (true)
  (false)
] @constant

(comment) @comment

; The key has the shape of a string, so the field comes after the string pattern.
(pair
  key: (_) @field)

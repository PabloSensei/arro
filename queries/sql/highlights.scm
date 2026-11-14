; Keywords, literals and comments of an SQL script. The grammar names every keyword
; node after the word it matches, so the common ones are listed by node.

[
  (keyword_select)
  (keyword_from)
  (keyword_where)
  (keyword_and)
  (keyword_or)
  (keyword_not)
  (keyword_in)
  (keyword_is)
  (keyword_between)
  (keyword_join)
  (keyword_left)
  (keyword_right)
  (keyword_inner)
  (keyword_outer)
  (keyword_full)
  (keyword_cross)
  (keyword_on)
  (keyword_as)
  (keyword_order)
  (keyword_by)
  (keyword_group)
  (keyword_having)
  (keyword_limit)
  (keyword_offset)
  (keyword_insert)
  (keyword_into)
  (keyword_values)
  (keyword_update)
  (keyword_set)
  (keyword_delete)
  (keyword_create)
  (keyword_table)
  (keyword_alter)
  (keyword_drop)
  (keyword_index)
  (keyword_view)
  (keyword_primary)
  (keyword_key)
  (keyword_foreign)
  (keyword_references)
  (keyword_unique)
  (keyword_constraint)
  (keyword_default)
  (keyword_check)
  (keyword_distinct)
  (keyword_union)
  (keyword_all)
  (keyword_exists)
  (keyword_case)
  (keyword_when)
  (keyword_then)
  (keyword_else)
  (keyword_end)
  (keyword_begin)
  (keyword_commit)
  (keyword_rollback)
  (keyword_transaction)
  (keyword_with)
  (keyword_returns)
  (keyword_return)
  (keyword_function)
  (keyword_procedure)
  (keyword_database)
  (keyword_schema)
  (keyword_if)
  (keyword_cascade)
  (keyword_restrict)
  (keyword_add)
  (keyword_column)
  (keyword_rename)
  (keyword_to)
  (keyword_type)
  (keyword_using)
  (keyword_cast)
  (keyword_asc)
  (keyword_desc)
] @keyword

(comment) @comment
(marginalia) @comment

(literal) @string

[
  (keyword_true)
  (keyword_false)
  (keyword_null)
] @constant

((literal) @number
 (#match? @number "^[-+]?[0-9]+$"))
((literal) @number
 (#match? @number "^[-+]?[0-9]*\\.[0-9]+$"))

(object_reference
  name: (identifier) @field)
(column_definition
  name: (identifier) @field)
(field
  name: (identifier) @field)

(invocation
  (object_reference
    name: (identifier) @function))

(parameter) @constant

; The units Move Statement moves: a statement of a block, a member of a type, an
; element of a comma separated list. Moving swaps the unit with its nearest sibling on
; the same level, so nothing leaves its block.

(block (_) @unit)
(constructor_body (_) @unit)
(class_body (_) @unit)
(interface_body (_) @unit)
(annotation_type_body (_) @unit)
(enum_body (_) @unit)
(enum_body_declarations (_) @unit)
(switch_block_statement_group (_) @unit)

(argument_list (_) @item)
(annotation_argument_list (_) @item)
(array_initializer (_) @item)
(element_value_array_initializer (_) @item)

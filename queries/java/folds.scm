(import_declaration) @fold.imports
((block_comment) @fold.doc
 (#match? @fold.doc "^/[*][*]"))
(block_comment) @fold.comment

(block) @fold
(class_body) @fold
(interface_body) @fold
(enum_body) @fold
(annotation_type_body) @fold
(constructor_body) @fold
(switch_block) @fold
(array_initializer) @fold
(module_body) @fold

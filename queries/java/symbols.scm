; Declarations for Go to Class, Go to Symbol and the file structure.

(package_declaration
  [(identifier) (scoped_identifier)] @name) @definition.module

(class_declaration
  name: (identifier) @name) @definition.class

(record_declaration
  name: (identifier) @name) @definition.class

(interface_declaration
  name: (identifier) @name) @definition.interface

(annotation_type_declaration
  name: (identifier) @name) @definition.interface

(enum_declaration
  name: (identifier) @name) @definition.enum

(method_declaration
  name: (identifier) @name) @definition.method

(constructor_declaration
  name: (identifier) @name) @definition.constructor

(compact_constructor_declaration
  name: (identifier) @name) @definition.constructor

(field_declaration
  declarator: (variable_declarator
    name: (identifier) @name)) @definition.field

(enum_constant
  name: (identifier) @name) @definition.constant

; Rules, variables and comments of a Makefile.

[
  "define" "else" "endef" "endif" "export" "if" "ifdef" "ifndef"
  "ifeq" "ifneq" "include" "override" "private" "sinclude" "undefine"
  "unexport" "vpath"
] @keyword

[
  (text)
  (string)
] @string

(comment) @comment

(variable_assignment
  name: (word) @field)
(variable_reference
  (word) @field)

[
  "call"
  "foreach"
  "patsubst"
  "shell"
  "subst"
  "wildcard"
] @function

(targets
  (word) @constant)

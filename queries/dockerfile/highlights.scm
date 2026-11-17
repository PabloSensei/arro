; Instructions, comments, strings and ARG or ENV names of a Dockerfile.

[
  "FROM"
  "AS"
  "RUN"
  "CMD"
  "LABEL"
  "EXPOSE"
  "ENV"
  "ADD"
  "COPY"
  "ENTRYPOINT"
  "VOLUME"
  "USER"
  "WORKDIR"
  "ARG"
  "ONBUILD"
  "STOPSIGNAL"
  "HEALTHCHECK"
  "SHELL"
  "MAINTAINER"
  "CROSS_BUILD"
] @keyword

(comment) @comment

[
  (double_quoted_string)
  (single_quoted_string)
  (json_string)
  (heredoc_line)
] @string

((variable) @constant
  (#match? @constant "^[A-Z][A-Z_0-9]*$"))

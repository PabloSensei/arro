; Headings, code blocks and block markers of a Markdown document. Inline content
; needs the second inline grammar, so emphasis and links stay plain here.

(atx_heading
  (inline) @keyword)
(setext_heading
  (paragraph) @keyword)

[
  (atx_h1_marker)
  (atx_h2_marker)
  (atx_h3_marker)
  (atx_h4_marker)
  (atx_h5_marker)
  (atx_h6_marker)
  (setext_h1_underline)
  (setext_h2_underline)
] @keyword

(fenced_code_block
  info_string: (info_string) @annotation)

(code_fence_content) @string
(indented_code_block) @string

(fenced_code_block_delimiter) @comment
(html_block) @comment

(link_label) @field
(link_destination) @string
(link_title) @string

[
  (list_marker_plus)
  (list_marker_minus)
  (list_marker_star)
  (list_marker_dot)
  (list_marker_parenthesis)
  (thematic_break)
] @keyword

(block_quote_marker) @constant

[
  (task_list_marker_checked)
  (task_list_marker_unchecked)
  (entity_reference)
  (numeric_character_reference)
] @constant

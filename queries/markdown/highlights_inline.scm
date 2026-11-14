; The inline grammar of a Markdown document: emphasis, code spans, links and the
; markers that stay muted around them. The block grammar captures the paragraph, this
; query captures what the paragraph holds.

(strong_emphasis) @markup.strong
(emphasis) @markup.emphasis
(strikethrough) @markup.strikethrough

(code_span) @markup.raw

[
  (emphasis_delimiter)
  (code_span_delimiter)
] @punctuation.special

(inline_link
  (link_text) @markup.link
  (link_destination) @markup.link.url)

(inline_link
  "[" @punctuation.special
  "]" @punctuation.special
  "(" @punctuation.special
  ")" @punctuation.special)

(image
  (image_description) @markup.link
  (link_destination) @markup.link.url)

(image
  "!" @punctuation.special
  "[" @punctuation.special
  "]" @punctuation.special
  "(" @punctuation.special
  ")" @punctuation.special)

(shortcut_link
  (link_text) @markup.link)
(full_reference_link
  (link_text) @markup.link
  (link_label) @markup.link.url)
(collapsed_reference_link
  (link_text) @markup.link)

(shortcut_link
  "[" @punctuation.special
  "]" @punctuation.special)
(full_reference_link
  "[" @punctuation.special
  "]" @punctuation.special)
(collapsed_reference_link
  "[" @punctuation.special
  "]" @punctuation.special
  "[" @punctuation.special
  "]" @punctuation.special)

(uri_autolink) @markup.link
(email_autolink) @markup.link
(backslash_escape) @markup.escape

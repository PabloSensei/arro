//! Colouring of request files and of JSON bodies, from the lines alone: the format has
//! no grammar, so a small scanner decides which word of a line is what.

/// What a coloured piece of a text is. The names match the colours of the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    Keyword,
    String,
    Field,
    Constant,
    Comment,
    Number,
}

/// A coloured piece of a text, in bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: u32,
    pub end: u32,
    pub kind: TokenKind,
}

/// Colours a request file: the method, the URL, the headers, the variables, the
/// separator and the body, which is JSON or XML by its content type.
pub fn highlight(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut body = None;
    let mut declared = None;
    let mut url_continuation = false;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let base = offset;
        offset += line.len();
        let trimmed = content.trim();
        let mut variables = Vec::new();
        variables_in(content, base, &mut variables);

        if let Some(kind) = body {
            if trimmed.starts_with("###") {
                push(&mut spans, base, content, TokenKind::Comment);
                body = None;
                declared = None;
            } else if trimmed.starts_with('#') {
                push(&mut spans, base, content, TokenKind::Comment);
            } else {
                let start = base + content.len() - content.trim_start().len();
                match kind {
                    BodyKind::Json => json_line(trimmed, start, &mut spans),
                    BodyKind::Xml => xml_line(trimmed, start, &mut spans),
                    BodyKind::Text => {}
                }
            }
            overlay(&mut spans, variables);
            continue;
        }
        if trimmed.starts_with("###") {
            push(&mut spans, base, content, TokenKind::Comment);
            url_continuation = false;
            declared = None;
            continue;
        }
        if trimmed.is_empty() {
            url_continuation = false;
            continue;
        }
        if trimmed.starts_with('#') || trimmed.starts_with("//") {
            push(&mut spans, base, content, TokenKind::Comment);
            overlay(&mut spans, variables);
            continue;
        }
        if url_continuation && content.starts_with(' ') {
            let start = base + content.len() - content.trim_start().len();
            let end = start + trimmed.len();
            spans.push(Span {
                start: start as u32,
                end: end as u32,
                kind: TokenKind::String,
            });
            overlay(&mut spans, variables);
            continue;
        }
        url_continuation = false;
        if let Some((method, url_at)) = request_line(content) {
            push(&mut spans, base, method, TokenKind::Keyword);
            let start =
                base + url_at + content[url_at..].len() - content[url_at..].trim_start().len();
            let url = content[url_at..].trim();
            if !url.is_empty() {
                spans.push(Span {
                    start: start as u32,
                    end: (start + url.len()) as u32,
                    kind: TokenKind::String,
                });
            }
            overlay(&mut spans, variables);
            url_continuation = true;
            continue;
        }
        if let Some(url_at) = url_only_line(content) {
            let start = base + url_at;
            let url = content[url_at..].trim();
            spans.push(Span {
                start: start as u32,
                end: (start + url.len()) as u32,
                kind: TokenKind::String,
            });
            overlay(&mut spans, variables);
            url_continuation = true;
            continue;
        }
        if let Some((name, value)) = header_parts(content) {
            if name.eq_ignore_ascii_case("content-type") {
                let lower = value.to_ascii_lowercase();
                declared = if lower.contains("json") {
                    Some(BodyKind::Json)
                } else if lower.contains("xml") {
                    Some(BodyKind::Xml)
                } else {
                    Some(BodyKind::Text)
                };
            }
            let name_at = base + content.len() - content.trim_start().len();
            spans.push(Span {
                start: name_at as u32,
                end: (name_at + name.len()) as u32,
                kind: TokenKind::Field,
            });
            let value_at = name_at + name.len() + 1;
            if !value.trim().is_empty() {
                let value_at = value_at + content[value_at - base..].len()
                    - content[value_at - base..].trim_start().len();
                spans.push(Span {
                    start: value_at as u32,
                    end: (value_at + value.trim().len()) as u32,
                    kind: TokenKind::String,
                });
            }
            overlay(&mut spans, variables);
            continue;
        }
        // A line that is neither a header nor a comment starts the body.
        let kind = declared.unwrap_or_else(|| {
            if trimmed.starts_with('{') || trimmed.starts_with('[') {
                BodyKind::Json
            } else if trimmed.starts_with('<') {
                BodyKind::Xml
            } else {
                BodyKind::Text
            }
        });
        body = Some(kind);
        let start = base + content.len() - content.trim_start().len();
        match kind {
            BodyKind::Json => json_line(trimmed, start, &mut spans),
            BodyKind::Xml => xml_line(trimmed, start, &mut spans),
            BodyKind::Text => {}
        }
        overlay(&mut spans, variables);
    }
    spans.sort_by_key(|span| (span.start, span.end));
    spans
}

/// Colours JSON in a text: keys, strings, numbers and the three literals.
pub fn highlight_json(text: &str) -> Vec<Span> {
    let mut spans = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let content = line.trim_end_matches(['\n', '\r']);
        let start = offset + content.len() - content.trim_start().len();
        json_line(content.trim(), start, &mut spans);
        offset += line.len();
    }
    spans
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BodyKind {
    Json,
    Xml,
    Text,
}

fn push(spans: &mut Vec<Span>, base: usize, content: &str, kind: TokenKind) {
    let start = base + content.len() - content.trim_start().len();
    let text = content.trim();
    if text.is_empty() {
        return;
    }
    spans.push(Span {
        start: start as u32,
        end: (start + text.len()) as u32,
        kind,
    });
}

/// The method and the offset of the rest of a request line, when the line starts with a
/// known method.
fn request_line(line: &str) -> Option<(&str, usize)> {
    let trimmed = line.trim_start();
    let name_end = trimmed.find(char::is_whitespace).unwrap_or(trimmed.len());
    let word = &trimmed[..name_end];
    crate::parse::Method::parse(word)?;
    let at = line.len() - trimmed.len();
    Some((word, at + name_end))
}

fn url_only_line(line: &str) -> Option<usize> {
    let trimmed = line.trim_start();
    (trimmed.starts_with("http://") || trimmed.starts_with("https://") || trimmed.starts_with("{{"))
        .then_some(line.len() - trimmed.len())
}

fn header_parts(line: &str) -> Option<(&str, &str)> {
    if line.starts_with(char::is_whitespace) {
        return None;
    }
    let colon = line.find(':')?;
    let name = &line[..colon];
    if name.is_empty()
        || !name.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        return None;
    }
    Some((name, &line[colon + 1..]))
}

/// The `{{...}}` names of a line.
fn variables_in(line: &str, base: usize, out: &mut Vec<Span>) {
    let mut rest = line;
    let mut at = 0;
    while let Some(start) = rest.find("{{") {
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            break;
        };
        let from = base + at + start;
        let to = from + 2 + end + 2;
        out.push(Span {
            start: from as u32,
            end: to as u32,
            kind: TokenKind::Constant,
        });
        at += start + 2 + end + 2;
        rest = &after[end + 2..];
    }
}

/// A line of JSON: strings, keys, numbers and `true`, `false`, `null`.
fn json_line(line: &str, base: usize, out: &mut Vec<Span>) {
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        match bytes[at] {
            b'"' => {
                let start = at;
                at += 1;
                while at < bytes.len() {
                    if bytes[at] == b'\\' {
                        at += 2;
                    } else if bytes[at] == b'"' {
                        at += 1;
                        break;
                    } else {
                        at += 1;
                    }
                }
                let at = at.min(bytes.len());
                let after = line[at..].trim_start();
                let kind = if after.starts_with(':') {
                    TokenKind::Field
                } else {
                    TokenKind::String
                };
                out.push(Span {
                    start: (base + start) as u32,
                    end: (base + at) as u32,
                    kind,
                });
            }
            b'-' | b'0'..=b'9' => {
                let start = at;
                at += 1;
                while at < bytes.len()
                    && (bytes[at].is_ascii_digit()
                        || matches!(bytes[at], b'.' | b'e' | b'E' | b'+' | b'-'))
                {
                    at += 1;
                }
                out.push(Span {
                    start: (base + start) as u32,
                    end: (base + at) as u32,
                    kind: TokenKind::Number,
                });
            }
            b't' if line[at..].starts_with("true") => {
                literal(out, base, at, 4);
                at += 4;
            }
            b'f' if line[at..].starts_with("false") => {
                literal(out, base, at, 5);
                at += 5;
            }
            b'n' if line[at..].starts_with("null") => {
                literal(out, base, at, 4);
                at += 4;
            }
            _ => at += 1,
        }
    }
}

fn literal(out: &mut Vec<Span>, base: usize, at: usize, len: usize) {
    out.push(Span {
        start: (base + at) as u32,
        end: (base + at + len) as u32,
        kind: TokenKind::Constant,
    });
}

/// A line of XML: tags, attribute names, quoted values and comments.
fn xml_line(line: &str, base: usize, out: &mut Vec<Span>) {
    let bytes = line.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if line[at..].starts_with("<!--") {
            let end = line[at + 4..]
                .find("-->")
                .map_or(line.len(), |end| at + 4 + end + 3);
            out.push(Span {
                start: (base + at) as u32,
                end: (base + end) as u32,
                kind: TokenKind::Comment,
            });
            at = end;
            continue;
        }
        if bytes[at] != b'<' {
            at += 1;
            continue;
        }
        at += 1;
        if bytes.get(at) == Some(&b'/')
            || bytes.get(at) == Some(&b'?')
            || bytes.get(at) == Some(&b'!')
        {
            at += 1;
        }
        let start = at;
        while at < bytes.len()
            && (bytes[at].is_ascii_alphanumeric() || matches!(bytes[at], b'-' | b'_' | b':' | b'.'))
        {
            at += 1;
        }
        if at > start {
            out.push(Span {
                start: (base + start) as u32,
                end: (base + at) as u32,
                kind: TokenKind::Keyword,
            });
        }
        // The attributes of this tag, up to its end.
        while at < bytes.len() && bytes[at] != b'>' {
            if bytes[at].is_ascii_whitespace() || bytes[at] == b'/' {
                at += 1;
                continue;
            }
            let name = at;
            while at < bytes.len()
                && !bytes[at].is_ascii_whitespace()
                && !matches!(bytes[at], b'=' | b'>' | b'/')
            {
                at += 1;
            }
            if at > name {
                out.push(Span {
                    start: (base + name) as u32,
                    end: (base + at) as u32,
                    kind: TokenKind::Field,
                });
            }
            while at < bytes.len() && bytes[at].is_ascii_whitespace() {
                at += 1;
            }
            if bytes.get(at) == Some(&b'=') {
                at += 1;
                while at < bytes.len() && bytes[at].is_ascii_whitespace() {
                    at += 1;
                }
                let quote = bytes.get(at).copied();
                if quote == Some(b'"') || quote == Some(b'\'') {
                    let start = at;
                    at += 1;
                    while at < bytes.len() && Some(bytes[at]) != quote {
                        at += 1;
                    }
                    at = (at + 1).min(bytes.len());
                    out.push(Span {
                        start: (base + start) as u32,
                        end: (base + at) as u32,
                        kind: TokenKind::String,
                    });
                }
            }
        }
        if bytes.get(at) == Some(&b'>') {
            at += 1;
        }
    }
}

/// Puts the variables over the colours of the line, cutting the pieces they cover.
fn overlay(spans: &mut Vec<Span>, variables: Vec<Span>) {
    if variables.is_empty() {
        return;
    }
    for variable in &variables {
        let mut pieces = Vec::with_capacity(spans.len());
        for span in spans.drain(..) {
            if span.end <= variable.start || span.start >= variable.end {
                pieces.push(span);
                continue;
            }
            if span.start < variable.start {
                pieces.push(Span {
                    start: span.start,
                    end: variable.start,
                    kind: span.kind,
                });
            }
            if span.end > variable.end {
                pieces.push(Span {
                    start: variable.end,
                    end: span.end,
                    kind: span.kind,
                });
            }
        }
        *spans = pieces;
    }
    spans.extend(variables);
}

#[cfg(test)]
mod tests {
    use super::{TokenKind, highlight, highlight_json};

    fn coloured(text: &str, kind: TokenKind) -> Vec<String> {
        highlight(text)
            .into_iter()
            .filter(|span| span.kind == kind)
            .map(|span| text[span.start as usize..span.end as usize].to_owned())
            .collect()
    }

    fn coloured_json(text: &str, kind: TokenKind) -> Vec<String> {
        highlight_json(text)
            .into_iter()
            .filter(|span| span.kind == kind)
            .map(|span| text[span.start as usize..span.end as usize].to_owned())
            .collect()
    }

    #[test]
    fn the_method_the_url_and_the_headers_of_a_request_are_coloured() {
        let text = "### Get owners\nGET {{host}}/owners\nAccept: application/json\n";
        assert_eq!(coloured(text, TokenKind::Keyword), ["GET"]);
        assert_eq!(
            coloured(text, TokenKind::String),
            ["/owners", "application/json"]
        );
        assert_eq!(coloured(text, TokenKind::Field), ["Accept"]);
        assert_eq!(
            coloured(text, TokenKind::Constant),
            ["{{host}}"],
            "the variable stands over the URL"
        );
        assert_eq!(
            coloured(text, TokenKind::Comment),
            ["### Get owners"],
            "the separator is a comment"
        );
    }

    #[test]
    fn a_json_body_is_coloured_as_json() {
        let text = "POST http://localhost/api\nContent-Type: application/json\n\n\
                    {\"name\": \"Ann\", \"age\": 42, \"ok\": true}\n";
        assert_eq!(
            coloured(text, TokenKind::Field),
            ["Content-Type", "\"name\"", "\"age\"", "\"ok\""]
        );
        assert_eq!(
            coloured(text, TokenKind::String),
            ["http://localhost/api", "application/json", "\"Ann\""]
        );
        assert_eq!(coloured(text, TokenKind::Number), ["42"]);
        assert_eq!(coloured(text, TokenKind::Constant), ["true"]);
    }

    #[test]
    fn an_xml_body_colours_its_tags_and_attributes() {
        let text = "POST http://localhost/api\nContent-Type: application/xml\n\n\
                    <owner id=\"7\">Ann</owner>\n";
        assert_eq!(
            coloured(text, TokenKind::Keyword),
            ["POST", "owner", "owner"]
        );
        assert_eq!(coloured(text, TokenKind::Field), ["Content-Type", "id"]);
        assert_eq!(
            coloured(text, TokenKind::String),
            ["http://localhost/api", "application/xml", "\"7\""]
        );
    }

    #[test]
    fn a_plain_body_stays_plain_and_its_variables_are_coloured() {
        let text = "POST http://localhost/api\n\nhello {{name}}\n";
        assert_eq!(coloured(text, TokenKind::Constant), ["{{name}}"]);
        assert!(!coloured(text, TokenKind::Number).contains(&"hello".to_owned()));
    }

    #[test]
    fn json_keys_strings_numbers_and_literals_are_found() {
        let text =
            "{\n  \"name\": \"Ann\",\n  \"port\": 8080,\n  \"open\": false,\n  \"none\": null\n}";
        assert_eq!(
            coloured_json(text, TokenKind::Field),
            ["\"name\"", "\"port\"", "\"open\"", "\"none\""]
        );
        assert_eq!(coloured_json(text, TokenKind::String), ["\"Ann\""]);
        assert_eq!(coloured_json(text, TokenKind::Number), ["8080"]);
        assert_eq!(coloured_json(text, TokenKind::Constant), ["false", "null"]);
    }

    #[test]
    fn an_escaped_quote_does_not_end_a_string() {
        let text = r#"{"name": "Ann \"A\" Lee"}"#;
        assert_eq!(
            coloured_json(text, TokenKind::String),
            [r#""Ann \"A\" Lee""#]
        );
    }

    #[test]
    fn the_spans_do_not_overlap() {
        let text = "GET {{host}}/owners\nContent-Type: application/json\n\n{\"a\": [1, 2]}\n";
        let spans = highlight(text);
        for pair in spans.windows(2) {
            assert!(pair[0].end <= pair[1].start, "{spans:?}");
        }
    }
}

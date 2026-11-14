//! The text format of a request file: requests separated by `###`, each one a request
//! line with a method and a URL, headers, and an optional body.

/// A method of a request. A request line without a method is a `GET`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Head,
    Options,
}

impl Method {
    pub const ALL: [Self; 7] = [
        Self::Get,
        Self::Post,
        Self::Put,
        Self::Patch,
        Self::Delete,
        Self::Head,
        Self::Options,
    ];

    /// The word of the request line.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        }
    }

    /// The method of a word of the request line, whatever its case.
    pub fn parse(word: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|method| method.as_str().eq_ignore_ascii_case(word))
    }

    /// Whether a request of this method carries a body.
    pub const fn has_body(self) -> bool {
        matches!(self, Self::Post | Self::Put | Self::Patch)
    }
}

/// The body of a request: the text in the file, or the text of a file next to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Inline(String),
    /// The path as written after `<`, relative to the request file unless absolute.
    File(String),
}

/// One request of a file with the lines it covers.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Request {
    /// The name after `###` or `# @name`; `None` when the file gives none.
    pub name: Option<String>,
    pub method: Method,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Body>,
    /// The line of the request line, from zero.
    pub line: usize,
    /// The last non-empty line of the request, from zero.
    pub end_line: usize,
}

impl Request {
    /// Whether `line`, from zero, is inside this request.
    pub fn contains_line(&self, line: usize) -> bool {
        (self.line..=self.end_line).contains(&line)
    }
}

/// A place of the file that cannot be a request.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub line: usize,
    pub message: String,
}

/// Everything a request file holds.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedFile {
    pub requests: Vec<Request>,
    pub problems: Vec<Problem>,
}

impl ParsedFile {
    /// The request whose lines hold `line`, from zero.
    pub fn at_line(&self, line: usize) -> Option<&Request> {
        self.requests
            .iter()
            .find(|request| request.contains_line(line))
    }

    /// The labels of the requests for menus and history: the name, or the request line.
    pub fn labels(&self) -> Vec<String> {
        self.requests
            .iter()
            .enumerate()
            .map(|(index, request)| {
                let label = match &request.name {
                    Some(name) if !name.trim().is_empty() => name.trim().to_owned(),
                    _ => format!("{} {}", request.method.as_str(), request.url)
                        .trim()
                        .to_owned(),
                };
                if label.is_empty() {
                    format!("#{}", index + 1)
                } else {
                    label
                }
            })
            .collect()
    }
}

fn is_comment(line: &str) -> bool {
    line.starts_with('#') || line.starts_with("//")
}

/// The name of `# @name login` or `// @name login`, whatever follows it.
fn at_name(line: &str) -> Option<String> {
    let rest = line
        .strip_prefix("//")
        .or_else(|| line.strip_prefix('#'))?
        .trim();
    let rest = rest.strip_prefix("@name")?.trim();
    (!rest.is_empty()).then(|| rest.to_owned())
}

/// A header line: a name without spaces and a colon, then the value.
fn header_line(line: &str) -> Option<(String, String)> {
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
    Some((name.to_owned(), line[colon + 1..].trim().to_owned()))
}

/// The method and the URL of a request line. A line that starts with neither a method
/// nor a URL is a `GET` of the whole line, which the request then rejects.
fn request_line(line: &str) -> (Method, String) {
    let line = line.trim();
    let (first, rest) = match line.split_once(char::is_whitespace) {
        Some((first, rest)) => (first, rest.trim()),
        None => (line, ""),
    };
    match Method::parse(first) {
        Some(method) => (method, rest.to_owned()),
        None => (Method::Get, line.to_owned()),
    }
}

/// Reads every request of `text`.
pub fn parse(text: &str) -> ParsedFile {
    let lines: Vec<&str> = text.lines().collect();
    let mut file = ParsedFile::default();
    let mut pending_name: Option<String> = None;
    let mut at = 0;
    while at < lines.len() {
        let trimmed = lines[at].trim();
        if trimmed.is_empty() {
            at += 1;
            continue;
        }
        if let Some(separator) = trimmed.strip_prefix("###") {
            let name = separator.trim().trim_matches('#').trim();
            pending_name = (!name.is_empty()).then(|| name.to_owned());
            at += 1;
            continue;
        }
        if is_comment(trimmed) {
            if let Some(name) = at_name(trimmed) {
                pending_name = Some(name);
            }
            at += 1;
            continue;
        }

        let start = at;
        let (method, mut url) = request_line(lines[at]);
        let mut name = pending_name.take();
        at += 1;
        // A URL that does not fit one line continues on the indented lines after it.
        while at < lines.len() && !lines[at].trim().is_empty() && lines[at].starts_with(' ') {
            url.push_str(lines[at].trim());
            at += 1;
        }
        if url.trim().is_empty() {
            file.problems.push(Problem {
                line: start,
                message: "the request has no URL".to_owned(),
            });
        }

        let mut headers: Vec<(String, String)> = Vec::new();
        let mut body_start = None;
        while at < lines.len() {
            let trimmed = lines[at].trim();
            let line = lines[at];
            if trimmed.starts_with("###") {
                break;
            }
            if trimmed.is_empty() {
                at += 1;
                body_start = Some(at);
                break;
            }
            if is_comment(trimmed) {
                if let Some(found) = at_name(trimmed) {
                    name = Some(found);
                }
                at += 1;
                continue;
            }
            let Some((header, value)) = header_line(line) else {
                body_start = Some(at);
                break;
            };
            headers.push((header, value));
            at += 1;
            // A continuation of a header value is indented and is not a header itself.
            while at < lines.len()
                && !lines[at].trim().is_empty()
                && lines[at].starts_with(' ')
                && header_line(lines[at]).is_none()
            {
                if let Some(last) = headers.last_mut() {
                    last.1.push(' ');
                    last.1.push_str(lines[at].trim());
                }
                at += 1;
            }
        }

        let mut body_lines: Vec<&str> = Vec::new();
        if let Some(mut body_at) = body_start {
            while body_at < lines.len() && !lines[body_at].trim_start().starts_with("###") {
                body_lines.push(lines[body_at]);
                body_at += 1;
            }
            at = body_at;
        }
        while body_lines.last().is_some_and(|line| line.trim().is_empty()) {
            body_lines.pop();
        }
        while body_lines
            .first()
            .is_some_and(|line| line.trim().is_empty())
        {
            body_lines.remove(0);
        }
        let body = body_of(&body_lines);
        let end_line = (start..at)
            .rev()
            .find(|index| !lines[*index].trim().is_empty())
            .unwrap_or(start);
        file.requests.push(Request {
            name,
            method,
            url: url.trim().to_owned(),
            headers,
            body,
            line: start,
            end_line,
        });
    }
    file
}

/// The body of the lines between the headers and the next separator. A body of one line
/// that starts with `<` is the text of the file named after it.
fn body_of(lines: &[&str]) -> Option<Body> {
    if lines.is_empty() {
        return None;
    }
    let text = lines.join("\n");
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Some(path) = trimmed.strip_prefix('<') {
        let path = path.trim().trim_matches('"').trim();
        if !path.is_empty() && !path.contains('\n') {
            return Some(Body::File(path.to_owned()));
        }
    }
    Some(Body::Inline(text))
}

#[cfg(test)]
mod tests {
    use super::{Body, Method, parse};

    const FILE: &str = "\
### Get owners
GET http://localhost:8080/owners?lastName=Davis
Accept: application/json

### Create
POST http://localhost:8080/api/owners
Content-Type: application/json

{\"firstName\": \"Ann\", \"lastName\": \"Lee\"}
";

    #[test]
    fn the_sample_file_has_two_named_requests() {
        let file = parse(FILE);
        assert_eq!(file.problems, []);
        assert_eq!(file.requests.len(), 2);

        let first = &file.requests[0];
        assert_eq!(first.name.as_deref(), Some("Get owners"));
        assert_eq!(first.method, Method::Get);
        assert_eq!(first.url, "http://localhost:8080/owners?lastName=Davis");
        assert_eq!(
            first.headers,
            [("Accept".to_owned(), "application/json".to_owned())]
        );
        assert_eq!(first.body, None);
        assert_eq!((first.line, first.end_line), (1, 2));

        let second = &file.requests[1];
        assert_eq!(second.name.as_deref(), Some("Create"));
        assert_eq!(second.method, Method::Post);
        assert_eq!(
            second.body,
            Some(Body::Inline(
                "{\"firstName\": \"Ann\", \"lastName\": \"Lee\"}".to_owned()
            ))
        );
        assert_eq!(second.line, 5);
        assert_eq!(second.end_line, 8);
    }

    #[test]
    fn a_request_without_a_method_is_a_get() {
        let file = parse("### Ping\nhttp://localhost/ping\n");
        assert_eq!(file.requests[0].method, Method::Get);
        assert_eq!(file.requests[0].url, "http://localhost/ping");
    }

    #[test]
    fn a_url_continues_on_the_indented_lines() {
        let file = parse(
            "GET http://localhost:8080/owners\n    ?lastName=Davis\n    &page=2\nAccept: */*\n",
        );
        let request = &file.requests[0];
        assert_eq!(
            request.url,
            "http://localhost:8080/owners?lastName=Davis&page=2"
        );
        assert_eq!(request.headers.len(), 1);
        assert_eq!(request.headers[0].0, "Accept");
    }

    #[test]
    fn a_body_can_come_from_a_file_next_to_the_request_file() {
        let file = parse("POST http://localhost/api\n\n< ./owner.json\n");
        assert_eq!(
            file.requests[0].body,
            Some(Body::File("./owner.json".to_owned()))
        );
    }

    #[test]
    fn comments_name_a_request_and_are_not_its_body() {
        let file = parse(
            "// the first request\n# @name login\nPOST http://localhost/login\n\
             // a note between the headers\nContent-Type: text/plain\n\n# a comment\nhello\n",
        );
        let request = &file.requests[0];
        assert_eq!(request.name.as_deref(), Some("login"));
        assert_eq!(
            request.body,
            Some(Body::Inline("# a comment\nhello".to_owned()))
        );
    }

    #[test]
    fn the_name_of_a_separator_is_replaced_by_a_name_of_the_request() {
        let file = parse("### From separator\n# @name from comment\nGET http://localhost/\n");
        assert_eq!(file.requests[0].name.as_deref(), Some("from comment"));
    }

    #[test]
    fn a_request_without_a_url_is_a_problem() {
        let file = parse("### Broken\nGET   \nAccept: */*\n");
        assert_eq!(file.problems.len(), 1);
        assert_eq!(file.problems[0].line, 1);
    }

    #[test]
    fn the_line_of_a_request_selects_it() {
        let file = parse(FILE);
        assert_eq!(file.at_line(1).map(|request| request.line), Some(1));
        assert_eq!(file.at_line(8).map(|request| request.line), Some(5));
        assert!(file.at_line(0).is_none(), "a separator is no request line");
        assert!(file.at_line(4).is_none());
    }

    #[test]
    fn the_labels_fall_back_to_the_request_line() {
        let file = parse("GET http://localhost/one\n### Two\nPOST http://localhost/two\n");
        assert_eq!(file.labels(), ["GET http://localhost/one", "Two"]);
    }

    #[test]
    fn every_method_of_the_list_is_read() {
        for method in Method::ALL {
            let text = format!("{} http://localhost/\n", method.as_str());
            let file = parse(&text);
            assert_eq!(file.requests[0].method, method);
        }
        let file = parse("get http://localhost/\n");
        assert_eq!(file.requests[0].method, Method::Get);
    }
}

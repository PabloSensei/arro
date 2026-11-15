//! The command lines of `curl`: a request file turned into a command line, and what a
//! browser or the documentation copies turned back into a request of the file.

use crate::variables::PreparedRequest;

/// The boundary of the parts of a multipart body a command line brings.
const BOUNDARY: &str = "WebAppBoundary";

/// A request read from a command line, with the keys the client does not know.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Imported {
    /// The request as the lines of a request file.
    pub text: String,
    /// The keys that are written as comments over the request.
    pub unsupported: Vec<String>,
}

/// The quoting of a command line.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shell {
    /// `bash`, as Linux and macOS use it.
    Bash,
    /// `cmd`, with the carets of a copied command line of Windows.
    Cmd,
}

/// Whether the text starts a `curl` command: the first line names the program.
pub fn is_curl(text: &str) -> bool {
    text.lines()
        .map(str::trim_start)
        .find(|line| !line.is_empty())
        .is_some_and(starts_with_curl)
}

fn starts_with_curl(text: &str) -> bool {
    match text.strip_prefix("curl") {
        Some(rest) => rest.is_empty() || rest.starts_with(char::is_whitespace),
        None => false,
    }
}

/// The command line of the text, with the lines that continue it joined. `None` when the
/// text is not a `curl` command; the lines after the last one of the command are not read.
fn command_text(text: &str) -> Option<String> {
    let mut joined = String::new();
    let mut first = true;
    for line in text.lines() {
        let line = line.trim_end_matches('\r').trim_end();
        let continued = line.ends_with('\\') || line.ends_with('^') || line.ends_with('`');
        let content = if continued {
            line[..line.len() - 1].trim_end()
        } else {
            line
        };
        let content = if first {
            let content = content.trim_start();
            if content.is_empty() {
                continue;
            }
            if !starts_with_curl(content) {
                return None;
            }
            first = false;
            content
        } else {
            content.trim()
        };
        if !joined.is_empty() {
            joined.push(' ');
        }
        joined.push_str(content);
        if !continued {
            break;
        }
    }
    (!first).then_some(joined)
}

/// The kinds of bodies a command line may carry.
#[derive(Clone, Debug, Default)]
struct Command {
    method: Option<String>,
    url: Option<String>,
    headers: Vec<(String, String)>,
    data: Vec<String>,
    json: Option<String>,
    forms: Vec<(String, bool)>,
    user: Option<String>,
    get: bool,
    head: bool,
    unsupported: Vec<String>,
}

/// Reads a command line of `curl` as the text of a request. `None` when the text is not
/// a `curl` command; `Err` when it is one the client cannot read.
pub fn from_curl(text: &str) -> Option<Result<Imported, String>> {
    let command = command_text(text)?;
    Some(read_command(&command))
}

fn read_command(command: &str) -> Result<Imported, String> {
    let unescaped = if command.contains("^\"") {
        cmd_unescape(command)
    } else {
        command.to_owned()
    };
    let tokens = tokens(&unescaped)?;
    let mut command = Command::default();
    let mut at = 1;
    while at < tokens.len() {
        let token = tokens[at].clone();
        let (name, inline) = option_of(&token);
        match name {
            "-X" | "--request" => command.method = Some(value(&tokens, &mut at, inline)?),
            "-H" | "--header" => {
                let header = value(&tokens, &mut at, inline)?;
                match header.split_once(':') {
                    Some((header, header_value)) => command
                        .headers
                        .push((header.trim().to_owned(), header_value.trim().to_owned())),
                    None => command.unsupported.push(format!("-H {header}")),
                }
            }
            "-d" | "--data" | "--data-raw" | "--data-binary" | "--data-urlencode" => {
                command.data.push(value(&tokens, &mut at, inline)?);
            }
            "--json" => command.json = Some(value(&tokens, &mut at, inline)?),
            "-F" | "--form" => {
                command
                    .forms
                    .push((value(&tokens, &mut at, inline)?, false));
            }
            "--form-string" => {
                command.forms.push((value(&tokens, &mut at, inline)?, true));
            }
            "-u" | "--user" => command.user = Some(value(&tokens, &mut at, inline)?),
            "-A" | "--user-agent" => {
                let value = value(&tokens, &mut at, inline)?;
                command.headers.push(("User-Agent".to_owned(), value));
            }
            "-e" | "--referer" => {
                let value = value(&tokens, &mut at, inline)?;
                command.headers.push(("Referer".to_owned(), value));
            }
            "-b" | "--cookie" => {
                let value = value(&tokens, &mut at, inline)?;
                command.headers.push(("Cookie".to_owned(), value));
            }
            "-G" | "--get" => command.get = true,
            "-I" | "--head" => command.head = true,
            "--url" => command.url = Some(value(&tokens, &mut at, inline)?),
            "--compressed"
            | "-k"
            | "--insecure"
            | "-L"
            | "--location"
            | "-s"
            | "--silent"
            | "-S"
            | "--show-error"
            | "-v"
            | "--verbose"
            | "-i"
            | "--include"
            | "-N"
            | "--no-buffer"
            | "-g"
            | "--globoff"
            | "--http1.1"
            | "--http2"
            | "--http2-prior-knowledge"
            | "--no-progress-meter"
            | "--fail"
            | "-f"
            | "--fail-with-body"
            | "--path-as-is"
            | "-sS"
            | "--tlsv1.2"
            | "--tlsv1.3" => {}
            other if other.starts_with('-') => {
                command.unsupported.push(other.to_owned());
                // A key this client does not know may still take the word after it, as
                // `-o file` does: the value is not the URL.
                if inline.is_none() && takes_value(other) {
                    at += 1;
                }
            }
            _ => {
                if command.url.is_none() {
                    command.url = Some(token.clone());
                }
            }
        }
        at += 1;
    }
    build(command)
}

fn build(mut command: Command) -> Result<Imported, String> {
    let Some(mut url) = command.url.take() else {
        return Err("the command has no URL".to_owned());
    };
    if command.get && !command.data.is_empty() {
        let query = command.data.join("&");
        if !query.is_empty() {
            url.push(if url.contains('?') { '&' } else { '?' });
            url.push_str(&query);
        }
        command.data.clear();
    }
    if command.json.is_some() {
        if !has_header(&command.headers, "Content-Type") {
            command
                .headers
                .push(("Content-Type".to_owned(), "application/json".to_owned()));
        }
        if !has_header(&command.headers, "Accept") {
            command
                .headers
                .push(("Accept".to_owned(), "application/json".to_owned()));
        }
    }
    if let Some(user) = &command.user
        && !has_header(&command.headers, "Authorization")
    {
        command.headers.push((
            "Authorization".to_owned(),
            format!("Basic {}", base64(user.as_bytes())),
        ));
    }
    let mut multipart = false;
    let body = if let Some(json) = command.json.take() {
        json
    } else if !command.forms.is_empty() {
        multipart = true;
        let boundary = boundary_of(&command.headers).unwrap_or_else(|| BOUNDARY.to_owned());
        if !has_header(&command.headers, "Content-Type") {
            command.headers.push((
                "Content-Type".to_owned(),
                format!("multipart/form-data; boundary={boundary}"),
            ));
        }
        multipart_body(&command.forms, &boundary)?
    } else if command.get || command.head {
        String::new()
    } else {
        command.data.join("&")
    };
    let method = command.method.unwrap_or_else(|| {
        if command.head {
            "HEAD".to_owned()
        } else if command.get {
            "GET".to_owned()
        } else if !body.is_empty() || multipart {
            "POST".to_owned()
        } else {
            "GET".to_owned()
        }
    });
    let mut text = String::new();
    for key in &command.unsupported {
        text.push_str("# unsupported: ");
        text.push_str(key);
        text.push('\n');
    }
    text.push_str(&method);
    text.push(' ');
    text.push_str(&url);
    text.push('\n');
    for (name, value) in &command.headers {
        text.push_str(name);
        text.push_str(": ");
        text.push_str(value);
        text.push('\n');
    }
    if !body.is_empty() {
        text.push('\n');
        text.push_str(&body);
        text.push('\n');
    }
    Ok(Imported {
        text,
        unsupported: command.unsupported,
    })
}

fn has_header(headers: &[(String, String)], name: &str) -> bool {
    headers
        .iter()
        .any(|(header, _)| header.eq_ignore_ascii_case(name))
}

fn boundary_of(headers: &[(String, String)]) -> Option<String> {
    headers
        .iter()
        .find(|(name, value)| {
            name.eq_ignore_ascii_case("content-type")
                && value.to_ascii_lowercase().contains("multipart/form-data")
        })
        .and_then(|(_, value)| {
            value
                .split(';')
                .find_map(|part| part.trim().strip_prefix("boundary="))
                .map(|boundary| boundary.trim_matches('"').trim().to_owned())
        })
}

/// A `-F` value as one part of a multipart body: the field or the file it sends.
fn multipart_body(forms: &[(String, bool)], boundary: &str) -> Result<String, String> {
    let mut text = String::new();
    for (form, literal) in forms {
        let Some((name, value)) = form.split_once('=') else {
            return Err(format!("the form {form} has no value"));
        };
        let name = name.trim().trim_matches('"');
        let mut value = value.trim().to_owned();
        let mut filename = None;
        let mut file = false;
        if !literal {
            let mut parts = value.split(';');
            let first = parts.next().unwrap_or_default().trim().to_owned();
            for option in parts {
                if let Some(named) = option.trim().strip_prefix("filename=") {
                    filename = Some(named.trim().trim_matches('"').to_owned());
                }
            }
            if let Some(path) = first.strip_prefix(['@', '<']) {
                file = true;
                value = path.trim().trim_matches('"').trim().to_owned();
                filename.get_or_insert_with(|| {
                    value
                        .rsplit(['/', '\\'])
                        .next()
                        .unwrap_or(value.as_str())
                        .to_owned()
                });
            } else {
                value = first;
            }
        }
        text.push_str("--");
        text.push_str(boundary);
        text.push('\n');
        text.push_str("Content-Disposition: form-data; name=\"");
        text.push_str(name);
        text.push('"');
        if let Some(filename) = &filename {
            text.push_str("; filename=\"");
            text.push_str(filename);
            text.push('"');
        }
        if file {
            text.push_str("\n\n< ");
            text.push_str(&value);
        } else {
            text.push_str("\n\n");
            text.push_str(&value);
        }
        text.push('\n');
    }
    text.push_str("--");
    text.push_str(boundary);
    text.push_str("--");
    Ok(text)
}

/// The name of a key and the value written after `=` or right after a short key.
fn option_of(token: &str) -> (&str, Option<&str>) {
    if token.starts_with("--") {
        match token.split_once('=') {
            Some((name, value)) => (name, Some(value)),
            None => (token, None),
        }
    } else if token.len() > 2 && token.starts_with('-') {
        let short = &token[..2];
        if matches!(
            short,
            "-X" | "-H"
                | "-d"
                | "-F"
                | "-u"
                | "-A"
                | "-e"
                | "-b"
                | "-o"
                | "-T"
                | "-m"
                | "-x"
                | "-w"
        ) {
            (short, Some(&token[2..]))
        } else {
            (token, None)
        }
    } else {
        (token, None)
    }
}

fn value(tokens: &[String], at: &mut usize, inline: Option<&str>) -> Result<String, String> {
    if let Some(value) = inline {
        return Ok(value.to_owned());
    }
    let key = tokens[*at].clone();
    *at += 1;
    tokens
        .get(*at)
        .cloned()
        .ok_or_else(|| format!("{key} needs a value"))
}

/// Keys that take the word after them although this client does not read them.
fn takes_value(key: &str) -> bool {
    matches!(
        key,
        "-o" | "--output"
            | "-x"
            | "--proxy"
            | "--noproxy"
            | "--cacert"
            | "--cert"
            | "--key"
            | "--resolve"
            | "--connect-timeout"
            | "-m"
            | "--max-time"
            | "--retry"
            | "--retry-delay"
            | "--upload-file"
            | "-T"
            | "--write-out"
            | "-w"
            | "--trace"
            | "--trace-ascii"
            | "--limit-rate"
            | "--max-redirs"
            | "--interface"
            | "--proto"
            | "--ciphers"
    )
}

/// The tokens of a shell command line: quotes group spaces, and a backslash of the
/// unquoted text and `$'...'` escapes its character.
fn tokens(text: &str) -> Result<Vec<String>, String> {
    let mut out = Vec::new();
    let mut chars = text.chars().peekable();
    loop {
        while chars
            .peek()
            .is_some_and(|character| character.is_whitespace())
        {
            chars.next();
        }
        if chars.peek().is_none() {
            return Ok(out);
        }
        let mut token = String::new();
        loop {
            match chars.peek().copied() {
                None => break,
                Some(character) if character.is_whitespace() => break,
                Some('\'') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            Some('\'') => break,
                            Some(character) => token.push(character),
                            None => return Err("a quote is not closed".to_owned()),
                        }
                    }
                }
                Some('"') => {
                    chars.next();
                    loop {
                        match chars.next() {
                            Some('"') => break,
                            Some('\\') => match chars.next() {
                                Some(escaped) => token.push(escaped),
                                None => return Err("a quote is not closed".to_owned()),
                            },
                            Some(character) => token.push(character),
                            None => return Err("a quote is not closed".to_owned()),
                        }
                    }
                }
                Some('$') if matches!(chars.clone().nth(1), Some('\'')) => {
                    chars.next();
                    chars.next();
                    loop {
                        match chars.next() {
                            Some('\'') => break,
                            Some('\\') => match chars.next() {
                                Some('n') => token.push('\n'),
                                Some('r') => token.push('\r'),
                                Some('t') => token.push('\t'),
                                Some('0') => token.push('\0'),
                                Some('x') => {
                                    token.push(hex(&mut chars, 2)?);
                                }
                                Some('u') => {
                                    token.push(hex(&mut chars, 4)?);
                                }
                                Some(escaped) => token.push(escaped),
                                None => return Err("a quote is not closed".to_owned()),
                            },
                            Some(character) => token.push(character),
                            None => return Err("a quote is not closed".to_owned()),
                        }
                    }
                }
                Some('\\') => {
                    chars.next();
                    match chars.next() {
                        Some(escaped) => token.push(escaped),
                        None => token.push('\\'),
                    }
                }
                Some(character) => {
                    chars.next();
                    token.push(character);
                }
            }
        }
        out.push(token);
    }
}

fn hex(
    chars: &mut std::iter::Peekable<std::str::Chars<'_>>,
    digits: usize,
) -> Result<char, String> {
    let mut text = String::with_capacity(digits);
    for _ in 0..digits {
        match chars.next() {
            Some(character) if character.is_ascii_hexdigit() => text.push(character),
            _ => return Err("a hexadecimal escape is short".to_owned()),
        }
    }
    let code = u32::from_str_radix(&text, 16).map_err(|error| error.to_string())?;
    char::from_u32(code).ok_or_else(|| "an escape is not a character".to_owned())
}

/// What `cmd` gives the program of a command line: the carets are removed, and the
/// quotes they escape survive for the parser of the program.
fn cmd_unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(character) = chars.next() {
        if character == '^' {
            if let Some(escaped) = chars.next() {
                out.push(escaped);
            }
        } else {
            out.push(character);
        }
    }
    out
}

fn base64(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let first = chunk[0];
        let second = chunk.get(1).copied().unwrap_or(0);
        let third = chunk.get(2).copied().unwrap_or(0);
        out.push(TABLE[(first >> 2) as usize] as char);
        out.push(TABLE[(((first & 0x03) << 4) | (second >> 4)) as usize] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(((second & 0x0f) << 2) | (third >> 6)) as usize] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[(third & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    out
}

/// The command line of a prepared request, with the variables already in place.
pub fn to_curl(request: &PreparedRequest, shell: Shell) -> String {
    let join = match shell {
        Shell::Bash => " \\\n  ",
        Shell::Cmd => " ^\n  ",
    };
    let mut out = format!("curl --request {}", request.method.as_str());
    out.push(' ');
    out.push_str(&quote(&request.url, shell));
    for (name, value) in &request.headers {
        out.push_str(join);
        out.push_str("--header ");
        out.push_str(&quote(&format!("{name}: {value}"), shell));
    }
    if let Some(body) = &request.body
        && !body.is_empty()
    {
        out.push_str(join);
        out.push_str("--data-raw ");
        out.push_str(&quote(&String::from_utf8_lossy(body), shell));
    }
    out
}

/// An argument of a command line, quoted for the shell of the platform.
pub fn quote(value: &str, shell: Shell) -> String {
    match shell {
        Shell::Bash => {
            let mut out = String::with_capacity(value.len() + 2);
            out.push('\'');
            for character in value.chars() {
                if character == '\'' {
                    out.push_str("'\\''");
                } else {
                    out.push(character);
                }
            }
            out.push('\'');
            out
        }
        Shell::Cmd => {
            let mut out = String::with_capacity(value.len() + 4);
            out.push_str("^\"");
            for character in value.chars() {
                match character {
                    '"' => out.push_str("^\""),
                    '^' | '&' | '|' | '<' | '>' | '(' | ')' | '%' => {
                        out.push('^');
                        out.push(character);
                    }
                    other => out.push(other),
                }
            }
            out.push_str("^\"");
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Imported, Shell, from_curl, is_curl, quote, to_curl};
    use crate::parse::Method;
    use crate::variables::PreparedRequest;

    fn imported(text: &str) -> Imported {
        from_curl(text)
            .expect("the text is a curl command")
            .expect("the command is read")
    }

    fn request(method: Method, url: &str) -> PreparedRequest {
        PreparedRequest {
            name: None,
            method,
            url: url.to_owned(),
            headers: Vec::new(),
            body: None,
            line: 0,
        }
    }

    #[test]
    fn the_first_line_names_the_program() {
        assert!(is_curl("curl http://localhost/"));
        assert!(is_curl("  curl \\\n  http://localhost/"));
        assert!(!is_curl("curly http://localhost/"));
        assert!(!is_curl("GET http://localhost/"));
        assert!(!is_curl(""));
    }

    #[test]
    fn a_simple_get_of_the_documentation() {
        let read = imported("curl http://example.com");
        assert_eq!(read.text, "GET http://example.com\n");
        assert!(read.unsupported.is_empty());
    }

    #[test]
    fn the_method_and_the_headers_of_a_command_line() {
        let read = imported(
            "curl -X POST 'http://localhost:8080/api/owners' -H 'Content-Type: application/json' \
             -H 'Accept: application/json'",
        );
        assert_eq!(
            read.text,
            "POST http://localhost:8080/api/owners\n\
             Content-Type: application/json\n\
             Accept: application/json\n"
        );
    }

    #[test]
    fn the_long_keys_of_the_documentation() {
        let read = imported(
            "curl --request PUT --url http://localhost/api --header 'X-One: 1' \
             --header 'X-Two: 2'",
        );
        assert_eq!(read.text, "PUT http://localhost/api\nX-One: 1\nX-Two: 2\n");
    }

    #[test]
    fn data_keys_are_joined_with_an_ampersand() {
        let read = imported("curl -d 'firstName=Ann' -d 'lastName=Lee' http://localhost/api");
        assert_eq!(
            read.text,
            "POST http://localhost/api\n\nfirstName=Ann&lastName=Lee\n"
        );
    }

    #[test]
    fn the_data_spellings_all_come_to_the_body() {
        for key in ["--data", "--data-raw", "--data-binary", "--data-urlencode"] {
            let text = format!("curl {key} '{{}}' http://localhost/api");
            let read = imported(&text);
            assert!(read.text.ends_with("\n\n{}\n"), "{key}: {}", read.text);
        }
    }

    #[test]
    fn json_sets_the_content_type() {
        let read = imported("curl --json '{\"a\":1}' http://localhost/api");
        assert_eq!(
            read.text,
            "POST http://localhost/api\n\
             Content-Type: application/json\n\
             Accept: application/json\n\n\
             {\"a\":1}\n"
        );
    }

    #[test]
    fn the_user_becomes_basic_authorization() {
        let read = imported("curl -u 'ann:secret' http://localhost/api");
        assert_eq!(
            read.text,
            "GET http://localhost/api\nAuthorization: Basic YW5uOnNlY3JldA==\n"
        );
    }

    #[test]
    fn a_get_moves_the_data_into_the_query() {
        let read = imported("curl -G -d 'lastName=Davis' 'http://localhost/owners?page=1'");
        assert_eq!(
            read.text,
            "GET http://localhost/owners?page=1&lastName=Davis\n"
        );
    }

    #[test]
    fn the_ignored_keys_leave_no_comment() {
        let read = imported("curl --compressed -k -L -s -v -S -i --http1.1 http://localhost/api");
        assert!(read.unsupported.is_empty(), "{:?}", read.unsupported);
        assert_eq!(read.text, "GET http://localhost/api\n");
    }

    #[test]
    fn an_unknown_key_is_a_comment_over_the_request() {
        let read = imported("curl --cert ./client.pem -H 'Accept: */*' http://localhost/api");
        assert_eq!(read.unsupported, ["--cert".to_owned()]);
        assert_eq!(
            read.text,
            "# unsupported: --cert\nGET http://localhost/api\nAccept: */*\n"
        );
    }

    #[test]
    fn a_form_makes_a_multipart_body() {
        let read = imported("curl -F 'note=hello' -F 'file=@./a.png' http://localhost/upload");
        assert_eq!(
            read.text,
            "POST http://localhost/upload\n\
             Content-Type: multipart/form-data; boundary=WebAppBoundary\n\n\
             --WebAppBoundary\n\
             Content-Disposition: form-data; name=\"note\"\n\nhello\n\
             --WebAppBoundary\n\
             Content-Disposition: form-data; name=\"file\"; filename=\"a.png\"\n\n< ./a.png\n\
             --WebAppBoundary--\n"
        );
    }

    #[test]
    fn the_lines_of_a_copied_command_line_continue() {
        let read = imported(
            "curl 'http://localhost/api' \\\n  -H 'Accept: application/json' \\\n  \
             -H 'X-Test: 1'\n",
        );
        assert_eq!(
            read.text,
            "GET http://localhost/api\nAccept: application/json\nX-Test: 1\n"
        );
    }

    #[test]
    fn the_command_line_of_chrome_for_bash() {
        let read = imported(
            "curl 'http://localhost:8080/api' \\\n  -H 'accept: application/json' \\\n  \
             -H 'content-type: application/json' \\\n  \
             --data-raw '{\"owner\":\"Ann\",\"page\":2}' \\\n  --compressed",
        );
        assert_eq!(
            read.text,
            "POST http://localhost:8080/api\n\
             accept: application/json\n\
             content-type: application/json\n\n\
             {\"owner\":\"Ann\",\"page\":2}\n"
        );
    }

    #[test]
    fn the_command_line_of_chrome_for_cmd() {
        let read = imported(
            "curl ^\"http://localhost:8080/api^\" ^\n  \
             -H ^\"accept: application/json^\" ^\n  \
             -H ^\"content-type: application/json^\" ^\n  \
             --data-raw ^\"{\\\"owner\\\":\\\"Ann\\\"}^\" ^\n  --compressed",
        );
        assert_eq!(
            read.text,
            "POST http://localhost:8080/api\n\
             accept: application/json\n\
             content-type: application/json\n\n\
             {\"owner\":\"Ann\"}\n"
        );
    }

    #[test]
    fn the_dollar_quotes_of_a_shell_are_read() {
        let read = imported("curl $'http://localhost/api?line=1' -H $'X-Tab:\\t1'");
        assert_eq!(read.text, "GET http://localhost/api?line=1\nX-Tab: 1\n");
    }

    #[test]
    fn the_quotes_of_a_command_line_hold_spaces() {
        let read = imported("curl \"http://localhost/a b?q='one'\"");
        assert_eq!(read.text, "GET http://localhost/a b?q='one'\n");
    }

    #[test]
    fn a_command_line_without_a_url_is_an_error() {
        let error = from_curl("curl -H 'Accept: */*'")
            .expect("a curl command")
            .expect_err("there is no URL");
        assert!(error.contains("URL"), "{error}");
    }

    #[test]
    fn the_multi_line_text_after_the_command_is_not_part_of_it() {
        let read = imported("curl http://localhost/one\ncurl http://localhost/two\n");
        assert_eq!(read.text, "GET http://localhost/one\n");
    }

    #[test]
    fn a_request_goes_back_to_a_command_line() {
        let mut prepared = request(Method::Post, "http://localhost/api");
        prepared.headers = vec![
            ("Content-Type".to_owned(), "application/json".to_owned()),
            ("Accept".to_owned(), "application/json".to_owned()),
        ];
        prepared.body = Some(br#"{"a":1}"#.to_vec());
        assert_eq!(
            to_curl(&prepared, Shell::Bash),
            "curl --request POST 'http://localhost/api' \\\n  \
             --header 'Content-Type: application/json' \\\n  \
             --header 'Accept: application/json' \\\n  \
             --data-raw '{\"a\":1}'"
        );
    }

    #[test]
    fn a_command_line_without_a_body_stays_one_line() {
        assert_eq!(
            to_curl(&request(Method::Get, "http://localhost/"), Shell::Bash),
            "curl --request GET 'http://localhost/'"
        );
    }

    #[test]
    fn the_quoting_of_bash_keeps_a_quote_of_the_value() {
        assert_eq!(quote("a'b", Shell::Bash), "'a'\\''b'");
        assert_eq!(quote("a b", Shell::Bash), "'a b'");
    }

    #[test]
    fn the_quoting_of_cmd_keeps_the_special_characters() {
        assert_eq!(quote(r#"{"a":"b"}"#, Shell::Cmd), "^\"{^\"a^\":^\"b^\"}^\"");
        assert_eq!(
            quote("http://localhost/?a=1&b=2", Shell::Cmd),
            "^\"http://localhost/?a=1^&b=2^\""
        );
    }

    #[test]
    fn the_cmd_style_of_a_request_is_a_copied_command_line() {
        let mut prepared = request(Method::Get, "http://localhost/owners?page=1");
        prepared.headers = vec![("Accept".to_owned(), "application/json".to_owned())];
        assert_eq!(
            to_curl(&prepared, Shell::Cmd),
            "curl --request GET ^\"http://localhost/owners?page=1^\" ^\n  \
             --header ^\"Accept: application/json^\""
        );
    }
}

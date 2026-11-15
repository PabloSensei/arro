//! Links in the text a program printed: web addresses, files with the line the text
//! names, and the frames of a stack trace. Nothing here opens anything: the panel
//! decides what a link does.

use std::path::{Path, PathBuf};

/// What a run of text names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LinkKind {
    /// An address the browser opens.
    Url,
    /// A file, with its line and column counted from one when the text named them.
    File {
        path: String,
        line: Option<u32>,
        column: Option<u32>,
    },
    /// A frame of a stack trace: it names a source file only, and the language finds
    /// the place of the class.
    Frame { file: String, line: u32 },
}

/// A link and the columns of the line it takes. Columns count the characters of the
/// line as the screen shows them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkText {
    pub start: u16,
    pub end: u16,
    pub kind: LinkKind,
}

impl LinkText {
    pub fn contains(&self, column: u16) -> bool {
        column >= self.start && column < self.end
    }

    /// The text the line shows for the link: the path with its place, a frame, an address.
    pub fn text<'a>(&self, line: &'a str) -> &'a str {
        let chars: Vec<(usize, char)> = line.char_indices().collect();
        let start = chars
            .get(self.start as usize)
            .map_or(line.len(), |(at, _)| *at);
        let end = chars
            .get(self.end as usize)
            .map_or(line.len(), |(at, _)| *at);
        &line[start..end]
    }
}

/// The punctuation a sentence or a bracket puts around a link.
const LEADING: &[char] = &['(', '[', '{', '<', '\'', '"', '`', '\u{ab}', '\u{201c}'];
const TRAILING: &[char] = &[
    '.', ',', ';', ':', '!', '?', ')', '}', '>', '\'', '"', '`', '\u{bb}', '\u{201d}',
];

/// Every link of a line, in its order, without the ones inside another.
pub fn links(line: &str) -> Vec<LinkText> {
    let mut found = frames(line);
    for (start, token) in tokens(line) {
        if found
            .iter()
            .any(|link| start >= link.start as usize && start < link.end as usize)
        {
            // The text of a frame belongs to the frame, not to a file under it.
            continue;
        }
        let Some((lead, len, kind)) = kind_of(token) else {
            continue;
        };
        found.push(LinkText {
            start: (start + lead) as u16,
            end: (start + lead + len) as u16,
            kind,
        });
    }
    found.sort_by_key(|link| link.start);
    found
}

/// The link at `column` of a line, when the line has one there.
pub fn link_at(line: &str, column: u16) -> Option<LinkText> {
    links(line).into_iter().find(|link| link.contains(column))
}

/// Whitespace-separated tokens with the column each starts at.
fn tokens(line: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut start: Option<usize> = None;
    for (index, ch) in line.chars().enumerate() {
        if ch.is_whitespace() {
            if let Some(at) = start.take() {
                out.push((at, &line[byte_of(line, at)..byte_of(line, index)]));
            }
        } else if start.is_none() {
            start = Some(index);
        }
    }
    if let Some(at) = start {
        out.push((at, &line[byte_of(line, at)..]));
    }
    out
}

/// The byte of a character index of a line.
fn byte_of(line: &str, column: usize) -> usize {
    line.char_indices()
        .nth(column)
        .map_or(line.len(), |(at, _)| at)
}

/// What a token names, and where the link starts and how long it is inside the token.
fn kind_of(token: &str) -> Option<(usize, usize, LinkKind)> {
    let lead = token
        .find(|ch: char| !LEADING.contains(&ch))
        .unwrap_or(token.len());
    let rest = &token[lead..];
    if rest.is_empty() {
        return None;
    }
    if rest.starts_with("http://") || rest.starts_with("https://") {
        let url = rest.trim_end_matches(TRAILING).trim_end_matches([')', ']']);
        if url.len() > "https://".len() {
            return Some((lead, url.chars().count(), LinkKind::Url));
        }
        return None;
    }
    let text = rest.trim_end_matches(TRAILING);
    if text.is_empty() {
        return None;
    }
    let len = text.chars().count();
    if let Some((path, line, column)) = compiler_place(text) {
        return Some((
            lead,
            len,
            LinkKind::File {
                path: clean_path(path),
                line,
                column,
            },
        ));
    }
    let (path, line, column) = place(text)?;
    let path = clean_path(path);
    if !looks_like_path(&path) {
        return None;
    }
    Some((lead, len, LinkKind::File { path, line, column }))
}

/// `File.kt:[12,5]` and `File.kt:[12]`, which compilers print.
fn compiler_place(token: &str) -> Option<(&str, Option<u32>, Option<u32>)> {
    let (path, place) = token.rsplit_once(":[")?;
    let inner = place.strip_suffix(']')?;
    let (line, column) = match inner.split_once(',') {
        Some((line, column)) => (line, Some(column)),
        None => (inner, None),
    };
    let line = number(line)?;
    Some((path, Some(line), column.and_then(number)))
}

/// `path:12:5`, `path:12` and `path` with the numbers the text names.
fn place(token: &str) -> Option<(&str, Option<u32>, Option<u32>)> {
    let mut cut = token.len();
    let mut numbers = Vec::new();
    while numbers.len() < 2 {
        let before = &token[..cut];
        let Some((rest, digits)) = before.rsplit_once(':') else {
            break;
        };
        if digits.is_empty() || !digits.chars().all(|ch| ch.is_ascii_digit()) {
            break;
        }
        numbers.push(number(digits)?);
        cut = rest.len();
    }
    let path = &token[..cut];
    let (line, column) = match numbers.as_slice() {
        [] => return None,
        [line] => (Some(*line), None),
        [column, line, ..] => (Some(*line), Some(*column)),
    };
    Some((path, line, column))
}

fn number(text: &str) -> Option<u32> {
    text.parse().ok()
}

fn clean_path(path: &str) -> String {
    path.strip_prefix("./").unwrap_or(path).to_string()
}

/// Whether the text can name a file rather than, say, `a:b` or a clock time.
fn looks_like_path(path: &str) -> bool {
    if path.is_empty() || path.chars().any(char::is_whitespace) {
        return false;
    }
    if path.chars().any(|ch| {
        matches!(
            ch,
            '(' | ')' | '[' | ']' | '{' | '}' | '"' | '\'' | '<' | '>'
        )
    }) {
        return false;
    }
    if path.contains('/') || path.contains('\\') {
        return true;
    }
    match path.rsplit_once('.') {
        Some((name, extension)) => {
            !name.is_empty()
                && (1..=6).contains(&extension.len())
                && extension.chars().all(|ch| ch.is_ascii_alphanumeric())
        }
        None => false,
    }
}

/// The frames of a stack trace, `at pkg.Cls.method(File.kt:42)`, with their places.
fn frames(line: &str) -> Vec<LinkText> {
    let chars: Vec<char> = line.chars().collect();
    let mut found = Vec::new();
    let mut at = 0;
    while at + 3 <= chars.len() {
        let start_of_line = at == 0 || chars[at - 1].is_whitespace();
        if start_of_line && chars[at] == 'a' && chars[at + 1] == 't' && chars[at + 2] == ' ' {
            let rest_start = at + 3;
            if let Some((end, file, place)) = frame_of(&chars, rest_start) {
                found.push(LinkText {
                    start: at as u16,
                    end: end as u16,
                    kind: LinkKind::Frame { file, line: place },
                });
                at = end;
                continue;
            }
        }
        at += 1;
    }
    found
}

/// The end of a frame that starts at `from`, its source file and its line.
fn frame_of(chars: &[char], from: usize) -> Option<(usize, String, u32)> {
    let open = (from..chars.len()).find(|&index| chars[index] == '(')?;
    let close = (open..chars.len()).find(|&index| chars[index] == ')')?;
    let head: String = chars[from..open].iter().collect();
    let head = head.trim();
    if head.is_empty() || head.contains(char::is_whitespace) || !head.contains('.') {
        return None;
    }
    let inside: String = chars[open + 1..close].iter().collect();
    let (file, digits) = inside.rsplit_once(':')?;
    if file.contains(char::is_whitespace) || !looks_like_path(file) {
        return None;
    }
    let line = number(digits)?;
    Some((close + 1, file.to_string(), line))
}

/// The folder an escape of the shell names: the URI of OSC 7, or the path of the
/// OSC 9;9 of Windows terminals.
pub fn directory(text: &str) -> Option<PathBuf> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    let Some(rest) = text.strip_prefix("file://") else {
        return Some(PathBuf::from(text));
    };
    let at = rest.find('/')?;
    let path = &rest[at..];
    let path = percent_decode(path);
    // A URI that names a folder of Windows starts with a slash before the drive.
    let bytes = path.as_bytes();
    let drive =
        bytes.len() > 2 && bytes[0] == b'/' && bytes[1].is_ascii_alphabetic() && bytes[2] == b':';
    Some(PathBuf::from(if drive {
        &path[1..]
    } else {
        path.as_str()
    }))
}

fn percent_decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && at + 2 < bytes.len()
            && let (Some(high), Some(low)) = (hex(bytes[at + 1]), hex(bytes[at + 2]))
        {
            out.push(high << 4 | low);
            at += 3;
            continue;
        }
        out.push(bytes[at]);
        at += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex(byte: u8) -> Option<u8> {
    (byte as char).to_digit(16).map(|digit| digit as u8)
}

/// The file `path` names: what the text says when it is absolute, else the directory of
/// the shell and then the root of the project.
pub fn resolve(path: &str, cwd: Option<&Path>, root: &Path) -> Option<PathBuf> {
    let path = PathBuf::from(path);
    if path.is_absolute() {
        return path.is_file().then_some(path);
    }
    let mut places = Vec::new();
    if let Some(cwd) = cwd {
        places.push(cwd.join(&path));
    }
    places.push(root.join(&path));
    places.into_iter().find(|place| place.is_file())
}

#[cfg(test)]
mod tests {
    use super::{LinkKind, directory, links, resolve};
    use std::path::Path;

    /// The extension of the source files of the language, kept out of this neutral
    /// crate's words.
    fn extension() -> String {
        ["ja", "va"].concat()
    }

    fn file(path: &str, line: Option<u32>, column: Option<u32>) -> LinkKind {
        LinkKind::File {
            path: path.to_string(),
            line,
            column,
        }
    }

    #[test]
    fn a_web_address_is_a_link() {
        let found = links("see (https://example.com/a?b=1) and http://localhost:8080/x.");
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].kind, LinkKind::Url);
        assert_eq!((found[0].start, found[0].end), (5, 30));
        assert_eq!(found[1].kind, LinkKind::Url);
        let url = found[1].text("see (https://example.com/a?b=1) and http://localhost:8080/x.");
        assert_eq!(url, "http://localhost:8080/x");
    }

    #[test]
    fn a_path_with_a_line_and_a_column_is_a_link() {
        let found = links("error in src/main/A.kt:42:7 here");
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, file("src/main/A.kt", Some(42), Some(7)));

        let found = links(r"C:\work\src\A.kt:42");
        assert_eq!(found[0].kind, file(r"C:\work\src\A.kt", Some(42), None));

        let found = links("./src/A.kt:3");
        assert_eq!(found[0].kind, file("src/A.kt", Some(3), None));
    }

    #[test]
    fn the_output_of_a_compiler_names_a_path_and_a_place() {
        let source = format!("Main.{}", extension());
        let found = links(&format!("{source}:[12,5] cannot find symbol"));
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, file(&source, Some(12), Some(5)), "{found:?}");

        let found = links(&format!("{source}:[12]"));
        assert_eq!(found[0].kind, file(&source, Some(12), None));

        let found = links(&format!("src/{source}:7: error: missing return"));
        assert_eq!(found[0].kind, file(&format!("src/{source}"), Some(7), None));
    }

    #[test]
    fn a_frame_of_a_stack_trace_is_a_link_for_the_language() {
        let source = format!("Main.{}", extension());
        let line = format!("    at pkg.Cls.run({source}:42)");
        let found = links(&line);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(
            found[0].kind,
            LinkKind::Frame {
                file: source.clone(),
                line: 42
            }
        );
        // The file inside the frame is a part of the frame, not a second link.
        assert_eq!(
            found[0].text(&line).trim(),
            format!("at pkg.Cls.run({source}:42)")
        );

        assert!(links("    at pkg.Cls.run(Native Method)").is_empty());
        assert!(links("at run(F.kt:1)").is_empty(), "no class, no frame");
    }

    #[test]
    fn text_that_only_looks_like_a_place_is_no_link() {
        for line in [
            "a:b",
            "time 12:30",
            "ratio 4:3",
            "note text.txt",
            "call m(1,2)",
        ] {
            assert!(links(line).is_empty(), "{line}: {:?}", links(line));
        }
    }

    #[test]
    fn a_folder_of_an_escape_is_read_back() {
        assert_eq!(
            directory("file://host/tmp/a%20b").as_deref(),
            Some(Path::new("/tmp/a b"))
        );
        assert_eq!(
            directory("file:///C:/work/x").as_deref(),
            Some(Path::new("C:/work/x"))
        );
        assert_eq!(directory("").as_deref(), None);
        assert_eq!(
            directory(r"C:\work\x").as_deref(),
            Some(Path::new(r"C:\work\x"))
        );
    }

    #[test]
    fn a_relative_path_is_found_in_the_folder_of_the_shell_then_in_the_project() {
        let shell = test_support::scratch("term-links-shell");
        let root = test_support::scratch("term-links-root");
        std::fs::create_dir_all(shell.join("src")).unwrap();
        std::fs::write(shell.join("src/A.kt"), "class A {}").unwrap();
        std::fs::write(root.join("B.kt"), "class B {}").unwrap();

        assert_eq!(
            resolve("src/A.kt", Some(&shell), &root),
            Some(shell.join("src/A.kt"))
        );
        assert_eq!(
            resolve("B.kt", Some(&shell), &root),
            Some(root.join("B.kt"))
        );
        assert_eq!(resolve("gone.kt", Some(&shell), &root), None);
    }
}

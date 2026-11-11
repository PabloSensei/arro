//! The `.editorconfig` files of a project: the properties of one file, found from its
//! own folder up to the folder that says `root = true`.
//!
//! The values of the nearest file win over the farther one, and inside one file a later
//! section wins over an earlier one. Patterns are matched against the path relative to
//! the folder of the `.editorconfig`.

use std::path::Path;

use crate::encoding::{Eol, EolStyle, FileFormat, TextEncoding};

/// How a file is indented.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndentStyle {
    Space,
    Tab,
}

/// The value of `indent_size`: a number of columns, or `tab` for the width of a tab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndentSize {
    Number(u32),
    Tab,
}

/// The line ends of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndOfLine {
    Lf,
    CrLf,
    Cr,
}

/// The encoding of a file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Charset {
    Utf8,
    Utf8Bom,
    Latin1,
    Utf16Le,
    Utf16Be,
}

/// The properties of one file that `.editorconfig` files name. `None` means that no
/// matching section named it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Properties {
    pub indent_style: Option<IndentStyle>,
    pub indent_size: Option<IndentSize>,
    pub tab_width: Option<u32>,
    pub end_of_line: Option<EndOfLine>,
    pub charset: Option<Charset>,
    pub insert_final_newline: Option<bool>,
    pub trim_trailing_whitespace: Option<bool>,
    pub max_line_length: Option<u32>,
}

impl Properties {
    /// The properties of a closer file or a later section win over these.
    pub fn merge(&mut self, later: &Self) {
        if later.indent_style.is_some() {
            self.indent_style = later.indent_style;
        }
        if later.indent_size.is_some() {
            self.indent_size = later.indent_size;
        }
        if later.tab_width.is_some() {
            self.tab_width = later.tab_width;
        }
        if later.end_of_line.is_some() {
            self.end_of_line = later.end_of_line;
        }
        if later.charset.is_some() {
            self.charset = later.charset;
        }
        if later.insert_final_newline.is_some() {
            self.insert_final_newline = later.insert_final_newline;
        }
        if later.trim_trailing_whitespace.is_some() {
            self.trim_trailing_whitespace = later.trim_trailing_whitespace;
        }
        if later.max_line_length.is_some() {
            self.max_line_length = later.max_line_length;
        }
    }

    /// The indentation these properties name: tabs or spaces, the size of one step and
    /// the width of a tab on screen. `None` when they say nothing about the unit, or
    /// name only the width of a tab.
    pub fn indentation(&self) -> Option<(bool, usize, usize)> {
        if self.indent_style.is_none() && self.indent_size.is_none() {
            return None;
        }
        let size = match self.indent_size {
            Some(IndentSize::Number(size)) => size as usize,
            Some(IndentSize::Tab) => self.tab_width.unwrap_or(4) as usize,
            None => self.tab_width.unwrap_or(4) as usize,
        }
        .max(1);
        let tab_width = self.tab_width.map_or(size, |width| (width as usize).max(1));
        let tabs = match self.indent_style {
            Some(IndentStyle::Tab) => true,
            Some(IndentStyle::Space) => false,
            None => matches!(self.indent_size, Some(IndentSize::Tab)),
        };
        Some((tabs, size, tab_width))
    }
}

/// One section of an `.editorconfig` file.
#[derive(Clone, Debug)]
struct Section {
    pattern: String,
    properties: Properties,
}

/// The text of one `.editorconfig` file.
#[derive(Clone, Debug, Default)]
struct ConfigFile {
    root: bool,
    sections: Vec<Section>,
}

/// Reads the `.editorconfig` files around `path` and gives the properties of the file.
/// Missing or unreadable files are skipped: a project without them is normal.
pub fn for_file(path: &Path) -> Properties {
    let mut configs = Vec::new();
    let mut folder = path.parent();
    while let Some(dir) = folder {
        if let Ok(text) = std::fs::read_to_string(dir.join(".editorconfig")) {
            let parsed = parse(&text);
            let root = parsed.root;
            configs.push((dir.to_path_buf(), parsed));
            if root {
                break;
            }
        }
        folder = dir.parent();
    }
    let mut properties = Properties::default();
    // The farther files go first, so that the nearer ones and the later sections of one
    // file win.
    for (dir, parsed) in configs.iter().rev() {
        for section in &parsed.sections {
            if matches_section(&section.pattern, dir, path) {
                properties.merge(&section.properties);
            }
        }
    }
    properties
}

/// The text as a save writes it, with the save properties of the `.editorconfig` files.
/// `new_file` is true for a file that had no text when it was opened: only such a file
/// takes the line ends of the properties, so the line ends of an existing file never
/// change silently.
pub fn apply_save(
    text: &str,
    properties: &Properties,
    new_file: bool,
    format: &mut FileFormat,
) -> String {
    let mut text = trim_trailing(text, properties.trim_trailing_whitespace == Some(true));
    if let Some(final_newline) = properties.insert_final_newline {
        if final_newline {
            if !text.ends_with('\n') {
                text.push('\n');
            }
        } else {
            while text.ends_with('\n') {
                text.pop();
            }
        }
    }
    if let Some(charset) = properties.charset {
        apply_charset(format, charset);
    }
    if new_file && let Some(eol) = properties.end_of_line {
        format.eol = EolStyle::Uniform(match eol {
            EndOfLine::Lf => Eol::Lf,
            EndOfLine::CrLf => Eol::CrLf,
            EndOfLine::Cr => Eol::Cr,
        });
    }
    text
}

fn trim_trailing(text: &str, trim: bool) -> String {
    if !trim {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('\n') {
        out.push_str(rest[..at].trim_end_matches([' ', '\t']));
        out.push('\n');
        rest = &rest[at + 1..];
    }
    out.push_str(rest.trim_end_matches([' ', '\t']));
    out
}

fn apply_charset(format: &mut FileFormat, charset: Charset) {
    match charset {
        Charset::Utf8 => {
            format.encoding = TextEncoding::Utf8;
            format.bom = false;
        }
        Charset::Utf8Bom => {
            format.encoding = TextEncoding::Utf8;
            format.bom = true;
        }
        Charset::Latin1 => {
            if let Some(encoding) = encoding_rs::Encoding::for_label(b"latin1") {
                format.encoding = TextEncoding::Legacy(encoding);
            }
        }
        Charset::Utf16Le => {
            format.encoding = TextEncoding::Utf16Le;
            format.bom = true;
        }
        Charset::Utf16Be => {
            format.encoding = TextEncoding::Utf16Be;
            format.bom = true;
        }
    }
}

/// The sections of one `.editorconfig` text, in the order they appear.
fn parse(text: &str) -> ConfigFile {
    let mut config = ConfigFile::default();
    let mut current: Option<Section> = None;
    let mut preamble = true;
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with(';') {
            continue;
        }
        if let Some(section) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            preamble = false;
            if let Some(section) = current.take() {
                config.sections.push(section);
            }
            current = Some(Section {
                pattern: section.trim().to_owned(),
                properties: Properties::default(),
            });
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim().to_ascii_lowercase();
        let value = value.trim().trim_end_matches(['#', ';']).trim();
        if preamble {
            if key == "root" && value.eq_ignore_ascii_case("true") {
                config.root = true;
            }
            continue;
        }
        if let Some(section) = &mut current {
            set_property(&mut section.properties, &key, value);
        }
    }
    if let Some(section) = current {
        config.sections.push(section);
    }
    config
}

fn set_property(properties: &mut Properties, key: &str, value: &str) {
    if value.eq_ignore_ascii_case("unset") {
        return;
    }
    match key {
        "indent_style" => {
            properties.indent_style = match value.to_ascii_lowercase().as_str() {
                "space" => Some(IndentStyle::Space),
                "tab" => Some(IndentStyle::Tab),
                _ => None,
            };
        }
        "indent_size" => {
            properties.indent_size = if value.eq_ignore_ascii_case("tab") {
                Some(IndentSize::Tab)
            } else {
                value.parse().ok().map(IndentSize::Number)
            };
        }
        "tab_width" => properties.tab_width = value.parse().ok(),
        "end_of_line" => {
            properties.end_of_line = match value.to_ascii_lowercase().as_str() {
                "lf" => Some(EndOfLine::Lf),
                "crlf" => Some(EndOfLine::CrLf),
                "cr" => Some(EndOfLine::Cr),
                _ => None,
            };
        }
        "charset" => {
            properties.charset = match value.to_ascii_lowercase().as_str() {
                "utf-8" => Some(Charset::Utf8),
                "utf-8-bom" => Some(Charset::Utf8Bom),
                "latin1" => Some(Charset::Latin1),
                "utf-16le" => Some(Charset::Utf16Le),
                "utf-16be" => Some(Charset::Utf16Be),
                _ => None,
            };
        }
        "insert_final_newline" => properties.insert_final_newline = parse_bool(value),
        "trim_trailing_whitespace" => properties.trim_trailing_whitespace = parse_bool(value),
        "max_line_length" => {
            properties.max_line_length = value.parse().ok();
        }
        _ => {}
    }
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

/// The path of `path` from `dir`, with `/` separators, or `None` when it is not below it.
fn relative_path(dir: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(dir).ok()?;
    let parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    Some(parts.join("/"))
}

/// Whether a section pattern matches the file, with its path relative to `dir`.
fn matches_section(pattern: &str, dir: &Path, path: &Path) -> bool {
    let Some(relative) = relative_path(dir, path) else {
        return false;
    };
    for pattern in expand_braces(pattern) {
        let anchored = pattern.strip_prefix('/').unwrap_or(&pattern);
        // A pattern without a slash names files in any folder below the config.
        let candidate = if anchored.contains('/') {
            relative.as_str()
        } else {
            relative.rsplit('/').next().unwrap_or(&relative)
        };
        if glob(anchored, candidate) {
            return true;
        }
    }
    false
}

/// `{pom,wro}.xml` and `*.{txt,xml}` into their plain forms.
fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(open) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(close) = pattern[open..].find('}').map(|at| at + open) else {
        return vec![pattern.to_owned()];
    };
    let before = &pattern[..open];
    let after = &pattern[close + 1..];
    let mut out = Vec::new();
    for choice in pattern[open + 1..close].split(',') {
        for expanded in expand_braces(&format!("{before}{choice}{after}")) {
            out.push(expanded);
        }
    }
    out
}

/// A glob with `*`, `**`, `?` and `[abc]`; `*` and `?` stop at a `/`, `**` does not.
fn glob(pattern: &str, text: &str) -> bool {
    expand_braces(pattern).iter().any(|pattern| {
        let pattern: Vec<char> = pattern.chars().collect();
        let text: Vec<char> = text.chars().collect();
        glob_at(&pattern, &text)
    })
}

fn glob_at(pattern: &[char], text: &[char]) -> bool {
    let Some((&first, rest)) = pattern.split_first() else {
        return text.is_empty();
    };
    match first {
        '*' => {
            let double = rest.first() == Some(&'*');
            let rest = if double { &rest[1..] } else { rest };
            // `**/` also matches no folder at all.
            if double && rest.first() == Some(&'/') && glob_at(&rest[1..], text) {
                return true;
            }
            let mut end = 0;
            loop {
                if glob_at(rest, &text[end..]) {
                    return true;
                }
                if end == text.len() || (!double && text[end] == '/') {
                    return false;
                }
                end += 1;
            }
        }
        '?' => {
            if text.first().is_some_and(|character| *character != '/') {
                glob_at(rest, &text[1..])
            } else {
                false
            }
        }
        '[' => {
            let Some((matched, used, negated)) = class(rest, text.first().copied()) else {
                return text.first() == Some(&'[') && glob_at(rest, &text[1..]);
            };
            if matched != negated && !text.is_empty() && text[0] != '/' {
                glob_at(&rest[used..], &text[1..])
            } else {
                false
            }
        }
        '\\' => {
            if let Some((&escaped, rest)) = rest.split_first() {
                text.first() == Some(&escaped) && glob_at(rest, &text[1..])
            } else {
                text.first() == Some(&'\\') && glob_at(rest, &text[1..])
            }
        }
        character => text.first() == Some(&character) && glob_at(rest, &text[1..]),
    }
}

/// A `[...]` class in `rest` and a character: whether it matched, how much of `rest` the
/// class took and whether it was negated.
fn class(rest: &[char], character: Option<char>) -> Option<(bool, usize, bool)> {
    let mut index = 0;
    let negated = rest.first() == Some(&'!');
    if negated {
        index += 1;
    }
    let start = index;
    let mut matched = false;
    while index < rest.len() && rest[index] != ']' {
        if index + 2 < rest.len() && rest[index + 1] == '-' && rest[index + 2] != ']' {
            let (low, high) = (rest[index], rest[index + 2]);
            if let Some(character) = character
                && low <= character
                && character <= high
            {
                matched = true;
            }
            index += 3;
        } else {
            if character == Some(rest[index]) {
                matched = true;
            }
            index += 1;
        }
    }
    if index == start || index >= rest.len() {
        return None;
    }
    // `[abc]` is `a`, `b`, `c`; the class itself ends at `]`.
    Some((matched, index + 1, negated))
}

#[cfg(test)]
mod tests {
    use super::{Charset, EndOfLine, IndentSize, IndentStyle, Properties, for_file, glob, parse};
    use std::fs;
    use std::path::{Path, PathBuf};

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ide-editorconfig-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, text).unwrap();
        path
    }

    const PROJECT: &str = "\
# top-most EditorConfig file
root = true

[*]
charset = utf-8
end_of_line = lf
insert_final_newline = true
indent_style = space

[*.{txt,xml}]
indent_size = 4
trim_trailing_whitespace = true
indent_style = tab
tab_width = 4

[{pom,wro}.xml]
indent_size = 2
indent_style = space

[*.{html,sql,less}]
indent_size = 2
";

    #[test]
    fn a_pattern_sets_tabs_and_a_later_one_sets_two_spaces() {
        let root = scratch("sections");
        write(&root, ".editorconfig", PROJECT);
        let source = write(&root, "src/main/source/A.txt", "text\n");
        let properties = for_file(&source);
        assert_eq!(properties.indent_style, Some(IndentStyle::Tab));
        assert_eq!(properties.indent_size, Some(IndentSize::Number(4)));
        assert_eq!(properties.tab_width, Some(4));
        assert_eq!(properties.indentation(), Some((true, 4, 4)));
        assert_eq!(properties.charset, Some(Charset::Utf8));
        assert_eq!(properties.end_of_line, Some(EndOfLine::Lf));
        assert_eq!(properties.insert_final_newline, Some(true));
        assert_eq!(properties.trim_trailing_whitespace, Some(true));

        let pom = write(&root, "pom.xml", "<project/>\n");
        let properties = for_file(&pom);
        assert_eq!(properties.indent_style, Some(IndentStyle::Space));
        assert_eq!(properties.indent_size, Some(IndentSize::Number(2)));
        assert_eq!(properties.indentation(), Some((false, 2, 4)));
        assert_eq!(properties.charset, Some(Charset::Utf8));
    }

    #[test]
    fn a_nearer_file_wins_and_root_stops_the_search() {
        let root = scratch("nearer");
        write(
            &root,
            ".editorconfig",
            "root = true\n[*]\nindent_style = space\nindent_size = 8\n",
        );
        write(&root, "module/.editorconfig", "[*.txt]\nindent_size = 2\n");
        let text = write(&root, "module/A.txt", "text\n");
        let properties = for_file(&text);
        assert_eq!(properties.indent_style, Some(IndentStyle::Space));
        assert_eq!(properties.indent_size, Some(IndentSize::Number(2)));

        // An upper file without `root = true` would also count, but this one stops it.
        let upper = scratch("upper");
        write(&upper, ".editorconfig", "[*]\nmax_line_length = 120\n");
        let nested = upper.join("project");
        fs::create_dir_all(&nested).unwrap();
        write(
            &nested,
            ".editorconfig",
            "root = true\n[*.txt]\nindent_size = 3\n",
        );
        let txt = write(&nested, "note.txt", "x\n");
        let properties = for_file(&txt);
        assert_eq!(properties.indent_size, Some(IndentSize::Number(3)));
        assert_eq!(properties.max_line_length, None);
    }

    #[test]
    fn a_later_section_wins_inside_one_file() {
        let root = scratch("later");
        write(
            &root,
            ".editorconfig",
            "[*]\nindent_size = 8\nindent_style = space\n[*.txt]\nindent_size = 2\n",
        );
        let text = write(&root, "A.txt", "text\n");
        assert_eq!(for_file(&text).indent_size, Some(IndentSize::Number(2)));
    }

    #[test]
    fn patterns_match_names_paths_and_braces() {
        assert!(glob("*.txt", "A.txt"));
        assert!(glob("**.txt", "src/main/A.txt"));
        assert!(glob("src/**.txt", "src/main/A.txt"));
        assert!(glob("**/A.txt", "A.txt"));
        assert!(glob("**/A.txt", "src/main/A.txt"));
        assert!(glob("[abc].txt", "b.txt"));
        assert!(!glob("[!abc].txt", "b.txt"));
        assert!(glob("[a-c].txt", "b.txt"));
        assert!(glob("*.{txt,xml}", "pom.xml"));
    }

    #[test]
    fn a_section_without_a_property_leaves_the_none() {
        let config = parse("[*]\n# commentary\nindent_style = tab\n");
        let properties: &Properties = &config.sections[0].properties;
        assert_eq!(properties.indent_style, Some(IndentStyle::Tab));
        assert_eq!(properties.indent_size, None);
        assert_eq!(properties.insert_final_newline, None);
        assert_eq!(properties.indentation(), Some((true, 4, 4)));
    }

    #[test]
    fn comments_and_unknown_values_are_ignored() {
        let config = parse(
            "; a comment\n[*]\nindent_size = two\nindent_style = quad\nmax_line_length = 100\n",
        );
        let properties = &config.sections[0].properties;
        assert_eq!(properties.indent_size, None);
        assert_eq!(properties.indent_style, None);
        assert_eq!(properties.max_line_length, Some(100));
    }

    #[test]
    fn a_save_trims_and_adds_the_final_newline() {
        let properties = Properties {
            insert_final_newline: Some(true),
            trim_trailing_whitespace: Some(true),
            ..Properties::default()
        };
        let mut format = crate::FileFormat::default();
        let text = super::apply_save("alpha  \nbeta\t", &properties, false, &mut format);
        assert_eq!(text, "alpha\nbeta\n");

        let properties = Properties {
            insert_final_newline: Some(false),
            ..Properties::default()
        };
        let text = super::apply_save("alpha\n\n", &properties, false, &mut format);
        assert_eq!(text, "alpha");
    }

    #[test]
    fn the_line_ends_of_an_existing_file_stay_and_a_new_file_takes_the_property() {
        let properties = Properties {
            end_of_line: Some(EndOfLine::Lf),
            ..Properties::default()
        };
        let mut format = crate::FileFormat::default().with_eol(crate::Eol::CrLf);
        super::apply_save("alpha\r\nbeta\r\n", &properties, false, &mut format);
        assert_eq!(format.eol, crate::EolStyle::Uniform(crate::Eol::CrLf));

        let mut format = crate::FileFormat::default().with_eol(crate::Eol::CrLf);
        super::apply_save("alpha", &properties, true, &mut format);
        assert_eq!(format.eol, crate::EolStyle::Uniform(crate::Eol::Lf));
    }

    #[test]
    fn the_charset_of_the_property_replaces_the_encoding_of_the_file() {
        let properties = Properties {
            charset: Some(Charset::Utf8Bom),
            ..Properties::default()
        };
        let mut format = crate::FileFormat::default();
        super::apply_save("alpha\n", &properties, false, &mut format);
        assert_eq!(format.encoding, crate::TextEncoding::Utf8);
        assert!(format.bom);
    }
}

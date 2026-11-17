//! The `.properties` format: keys, values, the escapes between the text of a file and
//! the text a user reads, and the changes that keep every other line as it was.
//!
//! A logical line is a key and a value separated by `=`, `:` or whitespace. A line that
//! ends with an odd number of backslashes continues on the next one, whose leading
//! spaces are not part of the value. The text a user sees is decoded: `\n`, `\t`, `\\`
//! and `\uXXXX` stand for their characters.
//!
//! A file of a localized set is named `<base>.properties` or `<base>_<locale>.properties`
//! and sits next to the other files of the set; [`bundle_files`] finds them by name.

use std::collections::BTreeSet;
use std::ops::Range;
use std::path::{Path, PathBuf};

use crate::Edit;

/// One logical entry of a `.properties` text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    /// The key as the user reads it, escapes decoded.
    pub key: String,
    /// The value as the user reads it, escapes decoded and continuation lines joined.
    pub value: String,
    /// The physical line the key starts on, from zero.
    pub line: usize,
    /// The key as written in the text.
    pub key_range: Range<usize>,
    /// The value as written in the text, continuation lines included.
    pub value_range: Range<usize>,
    /// The whole entry, leading spaces of the first line and continuation lines
    /// included, without the line end after the last one.
    pub entry_range: Range<usize>,
    /// Whether an `=`, a `:` or a space separated the key from the value.
    pub has_separator: bool,
}

/// One file of a localized set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BundleFile {
    pub path: PathBuf,
    /// `None` for the base file, the name of the locale for the others.
    pub locale: Option<String>,
}

/// The entries of a `.properties` text, in the order of the file.
pub fn entries(text: &str) -> Vec<Entry> {
    let lines = physical_lines(text);
    let mut found = Vec::new();
    let mut at = 0;
    while at < lines.len() {
        let (start, content) = lines[at];
        let body = content.trim_start();
        if body.is_empty() || body.starts_with('#') || body.starts_with('!') {
            at += 1;
            continue;
        }
        let first = start;
        let mut logical = String::new();
        let mut offsets: Vec<usize> = Vec::new();
        let mut current_start = start + (content.len() - body.len());
        let mut current = body;
        let mut next = at;
        let mut entry_end;
        loop {
            match continuation_of(current) {
                Some(head) => {
                    push_slice(&mut logical, &mut offsets, head, current_start);
                    entry_end = current_start + head.len();
                    next += 1;
                    if next >= lines.len() {
                        break;
                    }
                    let (line_start, line) = lines[next];
                    let segment = line.trim_start();
                    current_start = line_start + (line.len() - segment.len());
                    current = segment;
                }
                None => {
                    push_slice(&mut logical, &mut offsets, current, current_start);
                    entry_end = current_start + current.len();
                    next += 1;
                    break;
                }
            }
        }
        at = next;
        let Some((key_end, value_start, has_separator)) = split_logical(&logical) else {
            continue;
        };
        let key_range = key_span(&offsets, key_end);
        let value_range = value_span(&offsets, value_start, entry_end);
        found.push(Entry {
            key: unescape(&logical[..key_end]),
            value: unescape(&logical[value_start..]),
            line: start_line(text, first),
            key_range,
            value_range,
            entry_range: first..entry_end,
            has_separator,
        });
    }
    found
}

/// The entry of `key`, if the text has it.
pub fn entry<'a>(entries: &'a [Entry], key: &str) -> Option<&'a Entry> {
    entries.iter().find(|entry| entry.key == key)
}

/// Replaces the value of `key`, and nothing else. `None` when the text has no such key.
pub fn set_value(text: &str, key: &str, value: &str, non_ascii: bool) -> Option<Edit> {
    let entry = entry(&entries(text), key)?.clone();
    let encoded = escape_value(value, non_ascii);
    if entry.has_separator {
        return Some(Edit::new(entry.value_range, encoded));
    }
    let at = entry.entry_range.end;
    Some(Edit::new(at..at, format!("={encoded}")))
}

/// Adds `key` to the text: after the nearest key in alphabetical order when the file is
/// sorted, at the end otherwise.
pub fn add_entry(text: &str, key: &str, value: &str, non_ascii: bool) -> Edit {
    let found = entries(text);
    let line = format!(
        "{}={}\n",
        escape_key(key, non_ascii),
        escape_value(value, non_ascii)
    );
    if found.windows(2).all(|pair| pair[0].key <= pair[1].key)
        && let Some(after) = found.iter().find(|entry| entry.key.as_str() > key)
    {
        return Edit::insert(after.entry_range.start, line);
    }
    let mut insertion = String::new();
    if !text.is_empty() && !text.ends_with('\n') {
        insertion.push('\n');
    }
    insertion.push_str(&line);
    Edit::insert(text.len(), insertion)
}

/// Removes `key` and its line end from the text. `None` when the text has no such key.
pub fn remove_entry(text: &str, key: &str) -> Option<Edit> {
    let entry = entry(&entries(text), key)?.clone();
    let mut end = entry.entry_range.end;
    if text[end..].starts_with("\r\n") {
        end += 2;
    } else if text[end..].starts_with('\n') {
        end += 1;
    }
    Some(Edit::delete(entry.entry_range.start..end))
}

/// The text a user reads from the value of an entry, or from a key.
pub fn unescape(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(character) = chars.next() {
        if character != '\\' {
            out.push(character);
            continue;
        }
        let Some(next) = chars.next() else {
            out.push('\\');
            break;
        };
        match next {
            'n' => out.push('\n'),
            'r' => out.push('\r'),
            't' => out.push('\t'),
            'f' => out.push('\u{000C}'),
            'u' => match unicode_escape(&mut chars) {
                Some(character) => out.push(character),
                None => out.push_str("\\u"),
            },
            other => out.push(other),
        }
    }
    out
}

/// The text of a value with every character that has a meaning escaped. A file that is
/// not UTF-8 writes the characters outside ASCII as `\uXXXX`, as the format expects.
pub fn escape_value(value: &str, non_ascii: bool) -> String {
    let mut out = String::with_capacity(value.len());
    let mut at_start = true;
    for character in value.chars() {
        escape_character(character, non_ascii, at_start, &mut out);
        at_start = false;
    }
    out
}

/// The text of a key with the separators escaped, for a new entry.
pub fn escape_key(key: &str, non_ascii: bool) -> String {
    let mut out = String::with_capacity(key.len());
    let mut at_start = true;
    for character in key.chars() {
        match character {
            '=' | ':' | ' ' => {
                out.push('\\');
                out.push(character);
            }
            '#' | '!' if at_start => {
                out.push('\\');
                out.push(character);
            }
            other => escape_character(other, non_ascii, false, &mut out),
        }
        at_start = false;
    }
    out
}

fn escape_character(character: char, non_ascii: bool, at_start: bool, out: &mut String) {
    match character {
        '\\' => out.push_str("\\\\"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\u{000C}' => out.push_str("\\f"),
        ' ' if at_start => out.push_str("\\ "),
        character if character.is_control() => {
            out.push_str(&format!("\\u{:04X}", character as u32));
        }
        character if non_ascii && !character.is_ascii() => {
            let mut units = [0u16; 2];
            for unit in character.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{unit:04X}"));
            }
        }
        character => out.push(character),
    }
}

/// One `\uXXXX`, with a surrogate pair joined into the character it stands for.
fn unicode_escape(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<char> {
    let code = four_hex(chars)?;
    if (0xD800..=0xDBFF).contains(&code) {
        let checkpoint = chars.clone();
        if chars.next() == Some('\\')
            && chars.next() == Some('u')
            && let Some(low) = four_hex(chars)
            && (0xDC00..=0xDFFF).contains(&low)
        {
            let combined = 0x10000 + ((code - 0xD800) << 10) + (low - 0xDC00);
            return char::from_u32(combined);
        }
        *chars = checkpoint;
        return char::from_u32(code);
    }
    char::from_u32(code)
}

fn four_hex(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> Option<u32> {
    let mut code = 0;
    for _ in 0..4 {
        let digit = chars.peek()?.to_digit(16)?;
        chars.next();
        code = code * 16 + digit;
    }
    Some(code)
}

fn physical_lines(text: &str) -> Vec<(usize, &str)> {
    let mut lines = Vec::new();
    let mut start = 0;
    for piece in text.split_inclusive('\n') {
        let content = piece.trim_end_matches('\n').trim_end_matches('\r');
        lines.push((start, content));
        start += piece.len();
    }
    if start < text.len() {
        lines.push((start, &text[start..]));
    }
    lines
}

fn continuation_of(line: &str) -> Option<&str> {
    let trailing = line.bytes().rev().take_while(|byte| *byte == b'\\').count();
    (trailing % 2 == 1).then(|| &line[..line.len() - 1])
}

fn push_slice(logical: &mut String, offsets: &mut Vec<usize>, slice: &str, start: usize) {
    logical.push_str(slice);
    offsets.extend(start..start + slice.len());
}

/// The end of the key, the start of the value and whether a separator stood between
/// them, in the joined logical line.
fn split_logical(logical: &str) -> Option<(usize, usize, bool)> {
    let bytes = logical.as_bytes();
    let mut escaped = false;
    let mut key_end = bytes.len();
    let mut separator = None;
    for (at, byte) in bytes.iter().enumerate() {
        if escaped {
            escaped = false;
            continue;
        }
        match byte {
            b'\\' => escaped = true,
            b'=' | b':' => {
                key_end = at;
                separator = Some(at);
                break;
            }
            byte if byte.is_ascii_whitespace() => {
                key_end = at;
                separator = Some(at);
                break;
            }
            _ => {}
        }
    }
    if key_end == 0 {
        return None;
    }
    let Some(mut at) = separator else {
        return Some((key_end, key_end, false));
    };
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    if at < bytes.len() && matches!(bytes[at], b'=' | b':') {
        at += 1;
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
    }
    Some((key_end, at, true))
}

fn key_span(offsets: &[usize], key_end: usize) -> Range<usize> {
    if offsets.is_empty() || key_end == 0 {
        return 0..0;
    }
    offsets[0]..offsets[key_end - 1] + 1
}

fn value_span(offsets: &[usize], value_start: usize, entry_end: usize) -> Range<usize> {
    let start = offsets
        .get(value_start)
        .copied()
        .unwrap_or(entry_end)
        .min(entry_end);
    start..entry_end
}

fn start_line(text: &str, at: usize) -> usize {
    text[..at].bytes().filter(|byte| *byte == b'\n').count()
}

/// The files of the localized set `path` belongs to: the base file and the files of
/// every locale of it in the same folder. A path that is not a `.properties` file has
/// no set.
pub fn bundle_files(path: &Path) -> Vec<BundleFile> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_none_or(|extension| !extension.eq_ignore_ascii_case("properties"))
    {
        return Vec::new();
    }
    let Some(folder) = path.parent() else {
        return Vec::new();
    };
    let Some(stem) = path.file_stem().and_then(|stem| stem.to_str()) else {
        return Vec::new();
    };
    let mut stems = BTreeSet::new();
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    for entry in entries.flatten() {
        let candidate = entry.path();
        if candidate
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("properties"))
            && let Some(stem) = candidate.file_stem().and_then(|stem| stem.to_str())
        {
            stems.insert(stem.to_owned());
        }
    }
    let base = base_name(stem, &stems);
    let prefix = format!("{base}_");
    let mut files: Vec<BundleFile> = stems
        .iter()
        .filter_map(|candidate| {
            let locale = if candidate == &base {
                None
            } else {
                Some(candidate.strip_prefix(&prefix)?.to_owned())
            };
            Some(BundleFile {
                path: folder.join(format!("{candidate}.properties")),
                locale,
            })
        })
        .collect();
    files.sort_by(|left, right| left.locale.cmp(&right.locale));
    files
}

/// The base name of a file of a set: the longest prefix of `stem` before an underscore
/// that is a file of the folder, or a prefix followed by a locale-like part.
fn base_name(stem: &str, stems: &BTreeSet<String>) -> String {
    let mut best = None;
    for (at, character) in stem.char_indices() {
        if character != '_' || at == 0 {
            continue;
        }
        let prefix = &stem[..at];
        if stems.contains(prefix) {
            best = Some(prefix.to_owned());
        }
    }
    if let Some(base) = best {
        return base;
    }
    if let Some((prefix, rest)) = stem.split_once('_')
        && !prefix.is_empty()
        && looks_like_locale(rest)
    {
        return prefix.to_owned();
    }
    stem.to_owned()
}

/// A part after an underscore that reads as a locale: `ru`, `de_AT`, `en_US`.
fn looks_like_locale(rest: &str) -> bool {
    let mut parts = rest.split('_');
    let Some(language) = parts.next() else {
        return false;
    };
    if language.len() < 2 || language.len() > 3 || !language.chars().all(|c| c.is_ascii_lowercase())
    {
        return false;
    }
    parts.all(|part| {
        (2..=4).contains(&part.len()) && part.chars().all(|c| c.is_ascii_alphanumeric())
    })
}

#[cfg(test)]
mod tests {
    use super::{
        add_entry, bundle_files, entries, escape_key, escape_value, remove_entry, set_value,
        unescape,
    };

    fn text_after(text: &str, edit: crate::Edit) -> String {
        let mut out = text.to_owned();
        out.replace_range(edit.range, &edit.text);
        out
    }

    #[test]
    fn escapes_decode_to_the_text_a_user_reads() {
        assert_eq!(unescape("line\\nnext"), "line\nnext");
        assert_eq!(unescape("a\\tb\\\\c"), "a\tb\\c");
        assert_eq!(unescape("caf\\u00E9"), "café");
        assert_eq!(unescape("\\uD83D\\uDE00"), "😀");
        assert_eq!(unescape("\\="), "=");
        assert_eq!(unescape("\\ "), " ");
    }

    #[test]
    fn a_value_written_back_keeps_its_characters() {
        let value = "first\nsecond\twith \\ and café";
        let encoded = escape_value(value, true);
        assert_eq!(unescape(&encoded), value);
        assert!(
            encoded.contains("\\u00E9"),
            "a non-ASCII character is escaped"
        );
        assert_eq!(unescape(&escape_value(value, false)), value);
        assert!(
            escape_value("café", false).contains('é'),
            "a UTF-8 file keeps the character"
        );
        assert_eq!(escape_value(" leading", false), "\\ leading");
    }

    #[test]
    fn a_key_escapes_its_separators() {
        assert_eq!(
            escape_key("a key=value:part", true),
            "a\\ key\\=value\\:part"
        );
        assert_eq!(escape_key("#hash", true), "\\#hash");
    }

    #[test]
    fn the_three_separators_split_a_key_from_its_value() {
        let found = entries("one=First\ntwo: Second\nthree Third\n");
        let pairs: Vec<(&str, &str)> = found
            .iter()
            .map(|entry| (entry.key.as_str(), entry.value.as_str()))
            .collect();
        assert_eq!(
            pairs,
            [("one", "First"), ("two", "Second"), ("three", "Third")]
        );
    }

    #[test]
    fn escaped_spaces_and_equals_stay_in_the_key() {
        let found = entries("a\\ key=a\\=b\n");
        assert_eq!(found[0].key, "a key");
        assert_eq!(found[0].value, "a=b");
    }

    #[test]
    fn a_line_continuation_joins_the_next_line() {
        let found = entries("message=one \\\n    two\nother=three\n");
        assert_eq!(found[0].key, "message");
        assert_eq!(found[0].value, "one two");
        assert_eq!(found[1].value, "three");
    }

    #[test]
    fn a_comment_and_a_blank_line_are_not_entries() {
        let found = entries("# note\n\n! other\nkey=value\n");
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].key, "key");
        assert_eq!(found[0].line, 3);
    }

    #[test]
    fn a_value_replaces_only_its_own_line() {
        let text = "# keep\nfirst=one\nsecond=two\n# tail\nthird=three\n";
        let edit = set_value(text, "second", "new", false).unwrap();
        assert_eq!(
            text_after(text, edit),
            "# keep\nfirst=one\nsecond=new\n# tail\nthird=three\n"
        );
    }

    #[test]
    fn a_value_with_a_continuation_is_replaced_whole() {
        let text = "one=first \\\n    second\nother=last\n";
        let edit = set_value(text, "one", "short", false).unwrap();
        assert_eq!(text_after(text, edit), "one=short\nother=last\n");
    }

    #[test]
    fn a_key_without_a_value_gets_a_separator() {
        let text = "one\n";
        let edit = set_value(text, "one", "first", false).unwrap();
        assert_eq!(text_after(text, edit), "one=first\n");
    }

    #[test]
    fn a_sorted_file_gets_the_new_key_in_place() {
        let text = "alpha=1\ngamma=3\n";
        let edit = add_entry(text, "beta", "2", false);
        assert_eq!(text_after(text, edit), "alpha=1\nbeta=2\ngamma=3\n");
    }

    #[test]
    fn an_unsorted_file_gets_the_new_key_at_the_end() {
        let text = "gamma=3\nalpha=1\n";
        let edit = add_entry(text, "beta", "2", false);
        assert_eq!(text_after(text, edit), "gamma=3\nalpha=1\nbeta=2\n");
    }

    #[test]
    fn a_missing_final_newline_does_not_join_two_keys() {
        let edit = add_entry("alpha=1", "beta", "2", false);
        assert_eq!(text_after("alpha=1", edit), "alpha=1\nbeta=2\n");
    }

    #[test]
    fn a_removed_key_takes_its_line_with_it() {
        let text = "alpha=1\nbeta=2\ngamma=3\n";
        let edit = remove_entry(text, "beta").unwrap();
        assert_eq!(text_after(text, edit), "alpha=1\ngamma=3\n");
    }

    #[test]
    fn the_files_of_a_set_share_their_base_name() {
        let folder = test_support::scratch("properties-bundle");
        let write = |name: &str| std::fs::write(folder.join(name), "key=value\n").unwrap();
        write("messages.properties");
        write("messages_ru.properties");
        write("messages_de_AT.properties");
        write("other.properties");
        let files = bundle_files(&folder.join("messages_de_AT.properties"));
        let locales: Vec<Option<&str>> = files.iter().map(|file| file.locale.as_deref()).collect();
        assert_eq!(locales, [None, Some("de_AT"), Some("ru")]);
    }

    #[test]
    fn a_lone_locale_file_finds_its_base_name() {
        let folder = test_support::scratch("properties-lone-locale");
        std::fs::write(folder.join("messages_ru.properties"), "key=value\n").unwrap();
        let files = bundle_files(&folder.join("messages_ru.properties"));
        assert_eq!(files.len(), 1);
        assert_eq!(files[0].locale.as_deref(), Some("ru"));
    }
}

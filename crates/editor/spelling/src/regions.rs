//! The text of a file a language does not answer for: prose by its name, the comments
//! of a configuration file, the values of a properties file.

use std::ops::Range;
use std::path::Path;

/// What a file name says about the text inside it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    /// No rule: the whole text is prose.
    Plain,
    Markdown,
    Properties,
    Yaml,
    Xml,
}

pub fn format_of(path: &Path) -> Format {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let extension = name.rsplit_once('.').map_or("", |(_, extension)| extension);
    match extension {
        "md" | "markdown" => Format::Markdown,
        "properties" => Format::Properties,
        "yaml" | "yml" => Format::Yaml,
        "xml" | "html" | "htm" | "xhtml" | "xsd" | "svg" => Format::Xml,
        _ => Format::Plain,
    }
}

/// The pieces of `text` a dictionary should read, in the order they appear.
pub fn regions(format: Format, text: &str) -> Vec<Range<usize>> {
    match format {
        Format::Plain => (!text.is_empty())
            .then_some(0..text.len())
            .into_iter()
            .collect(),
        Format::Markdown => markdown(text),
        Format::Properties => properties(text),
        Format::Yaml => line_comments(text, '#'),
        Format::Xml => xml_comments(text),
    }
}

/// Markdown prose: no fenced code, no code spans, no addresses of links. The words
/// of the text itself stay, and so do the words of a link title.
fn markdown(text: &str) -> Vec<Range<usize>> {
    let mut regions = Vec::new();
    let mut fence: Option<&str> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        let tick = if trimmed.starts_with("```") {
            Some("```")
        } else if trimmed.starts_with("~~~") {
            Some("~~~")
        } else {
            None
        };
        if let Some(tick) = tick {
            match fence {
                Some(open) if open == tick => fence = None,
                None => fence = Some(tick),
                Some(_) => {}
            }
            offset += line.len();
            continue;
        }
        if fence.is_none() {
            regions.extend(without_inline_code(line, offset));
        }
        offset += line.len();
    }
    regions
}

/// The pieces of one line of prose that are neither code nor the address of a link.
fn without_inline_code(line: &str, base: usize) -> Vec<Range<usize>> {
    let mut regions = Vec::new();
    let mut start = 0;
    let mut at = 0;
    while at < line.len() {
        let rest = &line[at..];
        if let Some(found) = rest.find('`') {
            let open = at + found;
            push_region(&mut regions, base + start, base + open);
            let after = open + 1;
            let close = line[after..]
                .find('`')
                .map_or(line.len(), |index| after + index + 1);
            at = close;
            start = close;
            continue;
        }
        if let Some(found) = rest.find("](") {
            let link = at + found;
            push_region(&mut regions, base + start, base + link);
            let after = link + 2;
            let close = line[after..]
                .find(')')
                .map_or(line.len(), |index| after + index + 1);
            at = close;
            start = close;
            continue;
        }
        break;
    }
    push_region(&mut regions, base + start, base + line.len());
    regions
}

fn push_region(regions: &mut Vec<Range<usize>>, start: usize, end: usize) {
    if start < end {
        regions.push(start..end);
    }
}

/// The comments of a properties file and the values after `=` or `:`.
fn properties(text: &str) -> Vec<Range<usize>> {
    let mut regions = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_start();
        if trimmed.starts_with('#') || trimmed.starts_with('!') {
            push_region(&mut regions, offset, offset + line.trim_end().len());
        } else if let Some(at) = line.find(['=', ':']) {
            let value = &line[at + 1..];
            let leading = value.len() - value.trim_start().len();
            let start = at + 1 + leading;
            push_region(&mut regions, offset + start, offset + line.trim_end().len());
        }
        offset += line.len();
    }
    regions
}

/// The text after `#` on every line, outside a quoted string.
fn line_comments(text: &str, marker: char) -> Vec<Range<usize>> {
    let mut regions = Vec::new();
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let mut quote: Option<char> = None;
        for (index, character) in line.char_indices() {
            match (quote, character) {
                (None, '"') | (None, '\'') => quote = Some(character),
                (Some(open), found) if open == found => quote = None,
                (None, found) if found == marker => {
                    push_region(&mut regions, offset + index, offset + line.trim_end().len());
                    break;
                }
                _ => {}
            }
        }
        offset += line.len();
    }
    regions
}

fn xml_comments(text: &str) -> Vec<Range<usize>> {
    let mut regions = Vec::new();
    let mut from = 0;
    while let Some(start) = text[from..].find("<!--") {
        let start = from + start;
        let end = text[start..]
            .find("-->")
            .map_or(text.len(), |found| start + found + 3);
        push_region(&mut regions, start, end);
        from = end;
    }
    regions
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(format: Format, text: &str) -> Vec<&str> {
        regions(format, text)
            .into_iter()
            .map(|range| &text[range])
            .collect()
    }

    #[test]
    fn a_name_tells_the_format_of_the_file() {
        assert_eq!(format_of(Path::new("a.md")), Format::Markdown);
        assert_eq!(format_of(Path::new("a.properties")), Format::Properties);
        assert_eq!(format_of(Path::new("a.yml")), Format::Yaml);
        assert_eq!(format_of(Path::new("a.xml")), Format::Xml);
        assert_eq!(format_of(Path::new("a.txt")), Format::Plain);
    }

    #[test]
    fn markdown_leaves_out_fences_and_code_spans() {
        let text = "# The calcualte\n\n```\nlet calcualte = 1;\n```\n\nUse `calcualte` now.\n\
                    [link](https://example.com/calcualte) and calcualte.\n";
        let kept: String = texts(Format::Markdown, text).concat();
        assert!(kept.contains("The calcualte"));
        assert!(kept.contains("and calcualte"));
        assert!(!kept.contains("let calcualte = 1"), "a fence is code");
        assert!(!kept.contains("example.com"));
        assert_eq!(kept.matches("calcualte").count(), 2);
    }

    #[test]
    fn properties_give_comments_and_values_but_no_keys() {
        let text = "# a calcualte note\ncalcualteTotal=the calcualte\nother : calcualte\n";
        let kept: String = texts(Format::Properties, text).concat();
        assert!(kept.contains("a calcualte note"));
        assert!(kept.contains("the calcualte"));
        assert_eq!(kept.matches("calcualte").count(), 3);
        assert!(!kept.contains("calcualteTotal"));
    }

    #[test]
    fn yaml_and_xml_give_their_comments() {
        let yaml = "key: value # a calcualte\nquoted: \"# not a comment\"\n";
        assert_eq!(texts(Format::Yaml, yaml), ["# a calcualte"]);
        let xml = "<a><!-- a calcualte --><b/></a>";
        assert_eq!(texts(Format::Xml, xml), ["<!-- a calcualte -->"]);
    }
}

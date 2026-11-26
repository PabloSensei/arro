//! How a file is indented: the unit Enter and Tab insert, the width of a tab on screen
//! and where the answer came from.

use crate::editorconfig::Properties;

/// Where an indentation rule came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IndentSource {
    /// A `.editorconfig` file names it.
    EditorConfig,
    /// The code style files of the project name it.
    Project,
    /// The indentation of the lines in the file itself.
    Content,
    /// The unit of the language, when neither of the two says anything.
    Language,
    /// The user picked it for the open file.
    Manual,
}

/// One indentation rule of one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndentSettings {
    /// What one step of indentation inserts: a tab or spaces.
    pub unit: String,
    /// How many columns one step takes.
    pub size: usize,
    /// How many columns a tab takes on screen.
    pub tab_width: usize,
    pub source: IndentSource,
}

impl IndentSettings {
    /// The rule from `.editorconfig`, else from the indentation already in the text, else
    /// from the language.
    pub fn resolve(properties: &Properties, text: &str, language_unit: &str) -> Self {
        Self::resolve_with_project(properties, false, text, language_unit)
    }

    /// The rule from `.editorconfig` or the project code style, else from the indentation
    /// already in the text, else from the language. `from_project` says that the
    /// indentation of `properties` comes from the project style alone.
    pub fn resolve_with_project(
        properties: &Properties,
        from_project: bool,
        text: &str,
        language_unit: &str,
    ) -> Self {
        if let Some((tabs, size, tab_width)) = properties.indentation() {
            let source = if from_project {
                IndentSource::Project
            } else {
                IndentSource::EditorConfig
            };
            return Self::new(tabs, size, tab_width, source);
        }
        if let Some((tabs, size)) = detect(text) {
            let tab_width = properties.tab_width.map_or(4, |width| width as usize);
            return Self::new(tabs, size, tab_width, IndentSource::Content);
        }
        let tabs = language_unit.starts_with('\t');
        let size = if tabs { 4 } else { language_unit.len().max(1) };
        Self::new(tabs, size, 4, IndentSource::Language)
    }

    pub fn new(tabs: bool, size: usize, tab_width: usize, source: IndentSource) -> Self {
        let size = size.max(1);
        let tab_width = tab_width.max(1);
        Self {
            unit: if tabs {
                "\t".to_owned()
            } else {
                " ".repeat(size)
            },
            size,
            tab_width,
            source,
        }
    }

    /// A rule the user picked for this open file.
    pub fn manual(tabs: bool, size: usize, tab_width: usize) -> Self {
        Self::new(tabs, size, tab_width, IndentSource::Manual)
    }

    pub fn uses_tabs(&self) -> bool {
        self.unit.starts_with('\t')
    }

    /// `Tab` or `4 spaces`, as the status bar says it.
    pub fn label(&self) -> String {
        if self.uses_tabs() {
            "Tab".to_owned()
        } else {
            format!("{} spaces", self.size)
        }
    }
}

/// The indentation of the lines of a text: `(tabs, size)`. `None` when no line is
/// indented or no line tells the step. Tabs win only when more lines start with a tab
/// than with spaces. The lines of a block comment, ` * text`, say nothing about the step.
pub fn detect(text: &str) -> Option<(bool, usize)> {
    let mut tab_lines = 0usize;
    let mut space_lines = 0usize;
    let mut widths: Vec<usize> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim_start_matches([' ', '\t']);
        if trimmed.starts_with('*') {
            continue;
        }
        let indent = &line[..line.len() - trimmed.len()];
        if indent.is_empty() {
            continue;
        }
        if indent.starts_with('\t') {
            tab_lines += 1;
        } else {
            space_lines += 1;
            widths.push(indent.len());
        }
    }
    if tab_lines == 0 && space_lines == 0 {
        return None;
    }
    if tab_lines >= space_lines {
        return Some((true, 4));
    }
    // The step is the smallest of the usual steps 2, 4 and 8 that some line is indented
    // by. Lines aligned to a bracket have any width, so neither the most frequent width
    // nor the greatest common divisor is a safe guess; the fallback is the divisor only
    // when none of the usual steps occurs, such as in a file indented by three.
    let step = [2, 4, 8].into_iter().find(|step| widths.contains(step));
    let size = step.or_else(|| widths.iter().copied().reduce(gcd).filter(|size| *size >= 2))?;
    Some((false, size))
}

fn gcd(mut left: usize, mut right: usize) -> usize {
    while right != 0 {
        let next = left % right;
        left = right;
        right = next;
    }
    left.max(1)
}

#[cfg(test)]
mod tests {
    use super::{IndentSettings, IndentSource, detect};
    use crate::editorconfig::{IndentSize, IndentStyle, Properties};

    #[test]
    fn editorconfig_comes_before_the_content() {
        let properties = Properties {
            indent_style: Some(IndentStyle::Tab),
            indent_size: Some(IndentSize::Number(4)),
            tab_width: Some(4),
            ..Properties::default()
        };
        let settings = IndentSettings::resolve(&properties, "    two spaces\n", "    ");
        assert_eq!(settings.unit, "\t");
        assert_eq!(settings.size, 4);
        assert_eq!(settings.tab_width, 4);
        assert_eq!(settings.source, IndentSource::EditorConfig);
        assert_eq!(settings.label(), "Tab");
    }

    #[test]
    fn two_spaces_in_the_content_beat_the_language_unit() {
        let settings = IndentSettings::resolve(&Properties::default(), "a\n  b\n    c\n", "    ");
        assert_eq!(settings.unit, "  ");
        assert_eq!(settings.size, 2);
        assert_eq!(settings.source, IndentSource::Content);
        assert_eq!(settings.label(), "2 spaces");
    }

    #[test]
    fn tabs_in_the_content_are_tabs() {
        let settings = IndentSettings::resolve(&Properties::default(), "a\n\tb\n\t\tc\n", "    ");
        assert!(settings.uses_tabs());
        assert_eq!(settings.source, IndentSource::Content);
    }

    #[test]
    fn a_text_without_indentation_falls_back_to_the_language() {
        let settings = IndentSettings::resolve(&Properties::default(), "a\nb\n", "    ");
        assert_eq!(settings.unit, "    ");
        assert_eq!(settings.source, IndentSource::Language);
    }

    #[test]
    fn the_step_of_nested_spaces_is_the_smallest_usual_width() {
        assert_eq!(detect("a\n    b\n        c\n"), Some((false, 4)));
        assert_eq!(detect("a\n  b\n    c\n      d\n"), Some((false, 2)));
        assert_eq!(detect("a\n\tb\n"), Some((true, 4)));
        assert_eq!(detect("a\nb\n"), None);
    }

    #[test]
    fn comment_continuation_lines_do_not_change_the_step() {
        let mut text = String::from("class A {\n");
        for _ in 0..6 {
            text.push_str("     * Documented.\n");
        }
        for _ in 0..10 {
            text.push_str("        step();\n");
        }
        for _ in 0..5 {
            text.push_str("    field;\n");
        }
        assert_eq!(detect(&text), Some((false, 4)));
    }

    #[test]
    fn a_line_aligned_to_a_bracket_keeps_the_step_of_the_blocks() {
        let mut text = String::from("a\n");
        for _ in 0..6 {
            text.push_str("        block();\n");
        }
        text.push_str("    field;\n    field;\n");
        text.push_str("       aligned(\n");
        assert_eq!(detect(&text), Some((false, 4)));
    }

    #[test]
    fn a_step_that_no_usual_width_shows_is_the_divisor() {
        assert_eq!(detect("a\n   b\n      c\n"), Some((false, 3)));
    }

    #[test]
    fn a_single_space_names_no_step() {
        let settings = IndentSettings::resolve(&Properties::default(), "a\n b\n", "    ");
        assert_eq!(detect("a\n b\n"), None);
        assert_eq!(settings.source, IndentSource::Language);
        assert_eq!(settings.unit, "    ");
    }
}

//! The code style of a project in `.idea/codeStyles`: the common options and the
//! indentation of one language. A project that came from another IDE keeps its style
//! there. The parser knows no language: the caller names the one the file belongs to.
//!
//! `.editorconfig` wins over these files, and these files win over the settings of the
//! window, as the other IDE reads them with EditorConfig support on.

use std::path::{Path, PathBuf};

use roxmltree::Document;

use crate::editorconfig::{IndentSize, IndentStyle, Properties};

/// The folder of the project that keeps the code style of the other IDE.
pub const STYLE_FOLDER: &str = ".idea/codeStyles";
/// Says whether the project style is used rather than the global one.
pub const CONFIG_FILE: &str = "codeStyleConfig.xml";
/// The options of the project style.
pub const PROJECT_FILE: &str = "Project.xml";
/// The file that turns the project style off for this project; the settings window
/// writes it, and its presence is the whole switch.
pub const DISABLED_FILE: &str = ".arro/no-project-code-style";
/// The language section of a file that no syntax names.
const TEXT_LANGUAGE: &str = "TEXT";

/// The style one project names for one language.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProjectStyle {
    /// The column the other IDE wraps at.
    pub right_margin: Option<u32>,
    /// One step of indentation: tabs or spaces, the size of a step and the width of a
    /// tab on screen.
    pub indentation: Option<(bool, usize, usize)>,
}

/// The file the code style of a project comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StyleSource {
    /// A `.editorconfig` file of the project.
    EditorConfig,
    /// `.idea/codeStyles/Project.xml`.
    ProjectXml,
}

impl StyleSource {
    /// The name the Settings window shows.
    pub fn label(self) -> &'static str {
        match self {
            StyleSource::EditorConfig => ".editorconfig",
            StyleSource::ProjectXml => ".idea/codeStyles/Project.xml",
        }
    }
}

/// The folder of the project whose `.idea/codeStyles` holds the nearest style of `path`.
fn style_root(path: &Path) -> Option<PathBuf> {
    let mut folder = path.parent();
    while let Some(dir) = folder {
        if dir.join(STYLE_FOLDER).is_dir() {
            return Some(dir.to_path_buf());
        }
        folder = dir.parent();
    }
    None
}

/// Whether the user asked the window to use its own code style for the project of
/// `path`: the settings window writes the switch at the root of the project.
pub fn project_style_disabled(path: &Path) -> bool {
    let mut folder = path.parent();
    while let Some(dir) = folder {
        if dir.join(DISABLED_FILE).is_file() {
            return true;
        }
        folder = dir.parent();
    }
    false
}

/// The style of the project around `path` for the language `language`, or `None` when
/// the project has none, the other IDE is told to use the global style, or the user
/// asked the window to use its own settings for this project.
pub fn project_style(path: &Path, language: Option<&str>) -> Option<ProjectStyle> {
    let root = style_root(path)?;
    if root.join(DISABLED_FILE).is_file() {
        return None;
    }
    let folder = root.join(STYLE_FOLDER);
    if !use_per_project(&folder.join(CONFIG_FILE)) {
        return None;
    }
    let text = std::fs::read_to_string(folder.join(PROJECT_FILE)).ok()?;
    parse_project(&text, language)
}

/// The properties of a file: the `.idea` style of the project under its `.editorconfig`.
/// The flag is true when the indentation comes only from the project style, so that the
/// status bar and the settings know which file named it.
pub fn properties_for(path: &Path, language: Option<&str>) -> (Properties, bool) {
    if project_style_disabled(path) {
        return (Properties::default(), false);
    }
    let own = crate::editorconfig::for_file(path);
    let mut properties = Properties::default();
    let mut from_project = false;
    if let Some(style) = project_style(path, language) {
        if let Some((tabs, size, tab_width)) = style.indentation {
            properties.indent_style = Some(if tabs {
                IndentStyle::Tab
            } else {
                IndentStyle::Space
            });
            properties.indent_size = Some(IndentSize::Number(size as u32));
            properties.tab_width = Some(tab_width as u32);
            from_project = true;
        }
        properties.max_line_length = style.right_margin;
    }
    if own.indentation().is_some() {
        from_project = false;
    }
    properties.merge(&own);
    (properties, from_project)
}

/// Which file names the code style of the project at `root`, for the Settings window.
/// `None` means that the window settings apply.
pub fn style_source(root: &Path) -> Option<StyleSource> {
    if root.join(DISABLED_FILE).is_file() {
        return None;
    }
    if root.join(".editorconfig").is_file() {
        return Some(StyleSource::EditorConfig);
    }
    let folder = root.join(STYLE_FOLDER);
    if !use_per_project(&folder.join(CONFIG_FILE)) {
        return None;
    }
    folder
        .join(PROJECT_FILE)
        .is_file()
        .then_some(StyleSource::ProjectXml)
}

/// The style of one `.idea` code style text, taken from the first language section that
/// names an indentation. The settings of another IDE are imported without a language of
/// their own, so the caller needs no name here.
pub fn parse_style_text(text: &str) -> Option<ProjectStyle> {
    let document = Document::parse(text).ok()?;
    let section = document
        .descendants()
        .find(|node| node.has_tag_name("codeStyleSettings") && indentation_of(*node).is_some());
    let right_margin = section
        .and_then(|section| number_option(section, "RIGHT_MARGIN"))
        .or_else(|| number_option(document.root_element(), "RIGHT_MARGIN"));
    let indentation = section.and_then(indentation_of);
    (indentation.is_some() || right_margin.is_some()).then_some(ProjectStyle {
        right_margin,
        indentation,
    })
}

/// The `.idea` style of one `Project.xml` text for one language.
fn parse_project(text: &str, language: Option<&str>) -> Option<ProjectStyle> {
    let document = Document::parse(text).ok()?;
    let wanted = language.unwrap_or(TEXT_LANGUAGE);
    let section = document.descendants().find(|node| {
        node.has_tag_name("codeStyleSettings")
            && node
                .attribute("language")
                .is_some_and(|name| name.eq_ignore_ascii_case(wanted))
    });
    let right_margin = section
        .and_then(|section| number_option(section, "RIGHT_MARGIN"))
        .or_else(|| number_option(document.root_element(), "RIGHT_MARGIN"));
    let indentation = section.and_then(indentation_of);
    (indentation.is_some() || right_margin.is_some()).then_some(ProjectStyle {
        right_margin,
        indentation,
    })
}

/// The indentation a `codeStyleSettings` node names, when it names any part of it.
fn indentation_of(section: roxmltree::Node) -> Option<(bool, usize, usize)> {
    let size = number_option(section, "INDENT_SIZE");
    let tab_size = number_option(section, "TAB_SIZE");
    let tabs = bool_option(section, "USE_TAB_CHARACTER");
    if size.is_none() && tab_size.is_none() && tabs.is_none() {
        return None;
    }
    let width = size.or(tab_size).unwrap_or(4).max(1);
    let tab_width = tab_size.or(size).unwrap_or(width).max(1);
    Some((tabs.unwrap_or(false), width as usize, tab_width as usize))
}

/// Whether the project style is enabled. A missing file leaves the style of the project
/// in use: the IDE writes the file with the style it creates.
fn use_per_project(config: &Path) -> bool {
    let Ok(text) = std::fs::read_to_string(config) else {
        return true;
    };
    let Ok(document) = Document::parse(&text) else {
        return true;
    };
    document
        .descendants()
        .find(|node| {
            node.has_tag_name("option")
                && node.attribute("name") == Some("USE_PER_PROJECT_SETTINGS")
        })
        .and_then(|node| node.attribute("value"))
        .is_none_or(|value| !value.eq_ignore_ascii_case("false"))
}

/// The value of the direct `option` child of `node` with this name.
fn option_value<'a>(node: roxmltree::Node<'a, 'a>, name: &str) -> Option<&'a str> {
    node.children()
        .find(|child| child.has_tag_name("option") && child.attribute("name") == Some(name))
        .and_then(|child| child.attribute("value"))
}

fn number_option(node: roxmltree::Node, name: &str) -> Option<u32> {
    option_value(node, name)?.trim().parse().ok()
}

fn bool_option(node: roxmltree::Node, name: &str) -> Option<bool> {
    match option_value(node, name)?
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use super::{ProjectStyle, StyleSource, project_style, properties_for, style_source};

    const PROJECT: &str = r#"<code_scheme name="Project" version="173">
  <option name="RIGHT_MARGIN" value="100" />
  <codeStyleSettings language="TEXT">
    <option name="INDENT_SIZE" value="2" />
    <option name="TAB_SIZE" value="2" />
    <option name="USE_TAB_CHARACTER" value="true" />
  </codeStyleSettings>
  <codeStyleSettings language="Markdown">
    <option name="INDENT_SIZE" value="4" />
    <option name="USE_TAB_CHARACTER" value="false" />
  </codeStyleSettings>
</code_scheme>"#;

    const CONFIG_ON: &str = r#"<component name="ProjectCodeStyleConfiguration">
  <state>
    <option name="USE_PER_PROJECT_SETTINGS" value="true" />
  </state>
</component>"#;

    const CONFIG_OFF: &str = r#"<component name="ProjectCodeStyleConfiguration">
  <state>
    <option name="USE_PER_PROJECT_SETTINGS" value="false" />
  </state>
</component>"#;

    fn scratch(name: &str) -> PathBuf {
        test_support::scratch(&format!("idea-style-{name}"))
    }

    fn write(root: &Path, relative: &str, text: &str) -> PathBuf {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, text).unwrap();
        path
    }

    fn project(root: &Path, config: &str) {
        write(root, ".idea/codeStyles/Project.xml", PROJECT);
        write(root, ".idea/codeStyles/codeStyleConfig.xml", config);
    }

    #[test]
    fn the_project_file_gives_the_margin_and_the_indent_of_the_language() {
        let root = scratch("language");
        project(&root, CONFIG_ON);
        let text = write(&root, "src/main.txt", "alpha\n");

        let style = project_style(&text, Some("TEXT")).unwrap();
        assert_eq!(style.right_margin, Some(100));
        assert_eq!(style.indentation, Some((true, 2, 2)));
        assert_eq!(
            project_style(&text, Some("Markdown")).unwrap().indentation,
            Some((false, 4, 4))
        );
        assert_eq!(
            project_style(&text, Some("Unknown")),
            Some(ProjectStyle {
                right_margin: Some(100),
                indentation: None,
            })
        );
    }

    #[test]
    fn a_missing_language_uses_the_text_section() {
        let root = scratch("text");
        project(&root, CONFIG_ON);
        let text = write(&root, "notes.txt", "alpha\n");
        assert_eq!(
            project_style(&text, None),
            Some(ProjectStyle {
                right_margin: Some(100),
                indentation: Some((true, 2, 2)),
            })
        );
    }

    #[test]
    fn the_config_switches_the_project_style_off() {
        let root = scratch("config");
        project(&root, CONFIG_OFF);
        let text = write(&root, "main.txt", "alpha\n");
        assert_eq!(project_style(&text, Some("TEXT")), None);
        assert_eq!(style_source(&root), None);
    }

    #[test]
    fn editorconfig_wins_over_the_project_file() {
        let root = scratch("priority");
        project(&root, CONFIG_ON);
        let text = write(&root, "src/main.txt", "alpha\n");

        let (properties, from_project) = properties_for(&text, Some("TEXT"));
        assert!(from_project);
        assert_eq!(properties.indentation(), Some((true, 2, 2)));

        write(
            &root,
            ".editorconfig",
            "root = true\n[*.txt]\nindent_style = space\nindent_size = 4\n",
        );
        let (properties, from_project) = properties_for(&text, Some("TEXT"));
        assert!(!from_project);
        assert_eq!(properties.indentation(), Some((false, 4, 2)));

        // The `.editorconfig` names the unit, so the project size fills the rest.
        write(
            &root,
            ".editorconfig",
            "root = true\n[*.txt]\nindent_style = space\n",
        );
        let (properties, from_project) = properties_for(&text, Some("TEXT"));
        assert!(!from_project);
        assert_eq!(properties.indentation(), Some((false, 2, 2)));
    }

    #[test]
    fn the_settings_window_uses_our_switch_file() {
        let root = scratch("switch");
        project(&root, CONFIG_ON);
        assert_eq!(style_source(&root), Some(StyleSource::ProjectXml));

        write(
            &root,
            ".editorconfig",
            "root = true\n[*]\nindent_size = 2\n",
        );
        assert_eq!(style_source(&root), Some(StyleSource::EditorConfig));

        fs::remove_file(root.join(".editorconfig")).unwrap();
        write(&root, super::DISABLED_FILE, "");
        assert_eq!(style_source(&root), None);
    }

    #[test]
    fn the_switch_turns_the_editorconfig_rules_off_too() {
        let root = scratch("switch-editorconfig");
        let text = write(&root, "src/main.txt", "alpha\n");
        write(
            &root,
            ".editorconfig",
            "root = true\n[*.txt]\nindent_style = space\nindent_size = 3\n",
        );
        assert_eq!(
            properties_for(&text, Some("TEXT")).0.indentation(),
            Some((false, 3, 3))
        );
        write(&root, super::DISABLED_FILE, "");
        assert_eq!(properties_for(&text, Some("TEXT")).0.indentation(), None);
        assert_eq!(style_source(&root), None);
    }
}

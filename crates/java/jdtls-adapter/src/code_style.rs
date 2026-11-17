//! The code style of a project that came from another IDE: the `.idea/codeStyles`
//! options of one language, the Eclipse formatter profile the server reads and the
//! import order its organizer keeps.
//!
//! The profile is an Eclipse XML file under the state folder of the server, named after
//! its content, so a changed style is a new path that the running server reads without a
//! restart.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use roxmltree::Document;
use text_core::idea_style::{CONFIG_FILE, DISABLED_FILE, PROJECT_FILE, STYLE_FOLDER};

/// The name the generated Eclipse profile carries.
pub const PROFILE_NAME: &str = "Arro Project";
/// The version the profile names; the server upgrades an older profile on its own.
const PROFILE_VERSION: u32 = 21;
const FORMATTER_PREFIX: &str = "formatter-";
const FORMATTER_SUFFIX: &str = ".xml";

/// One entry of the import layout table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImportEntry {
    /// A package group; an empty name means every import that no other group took.
    Package { name: String, static_import: bool },
    /// A blank line between two groups.
    EmptyLine,
}

/// The Java part of `.idea/codeStyles/Project.xml`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectStyle {
    pub indent_size: Option<u32>,
    pub continuation_indent: Option<u32>,
    pub tab_size: Option<u32>,
    pub use_tabs: Option<bool>,
    pub right_margin: Option<u32>,
    pub class_count_to_use_import_on_demand: Option<u32>,
    pub names_count_to_use_import_on_demand: Option<u32>,
    pub packages_to_use_import_on_demand: Vec<String>,
    pub import_layout: Vec<ImportEntry>,
}

/// What a start prepares from the project style.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Prepared {
    /// The Eclipse profile, when the project names any formatting option.
    pub formatter: Option<PathBuf>,
    /// The Eclipse preferences the organizer reads.
    pub prefs: Vec<(String, String)>,
    /// The package groups of the completion list.
    pub import_order: Vec<String>,
}

impl ProjectStyle {
    fn formats(&self) -> bool {
        self.indent_size.is_some()
            || self.continuation_indent.is_some()
            || self.tab_size.is_some()
            || self.use_tabs.is_some()
            || self.right_margin.is_some()
    }
}

/// Reads the code style of `root`: the `.idea` files when the project uses them, with a
/// `.editorconfig` over them, which wins. A project that names neither gives `None`.
pub fn read_project_style(root: &Path) -> Option<ProjectStyle> {
    if root.join(DISABLED_FILE).is_file() {
        return None;
    }
    let mut style = {
        let folder = root.join(STYLE_FOLDER);
        if folder.is_dir() && use_per_project(&folder.join(CONFIG_FILE)) {
            std::fs::read_to_string(folder.join(PROJECT_FILE))
                .ok()
                .and_then(|text| parse_project(&text))
                .unwrap_or_default()
        } else {
            ProjectStyle::default()
        }
    };
    // The formatter of the server is one for the whole project, so the file of the
    // project root names its rules; a `.editorconfig` of a module still reaches the
    // editor of that module.
    let probe = root.join("src").join("ArroProbe.java");
    let editorconfig = text_core::editorconfig::for_file(&probe);
    if let Some((tabs, size, tab_width)) = editorconfig.indentation() {
        style.use_tabs = Some(tabs);
        style.indent_size = Some(size as u32);
        style.tab_size = Some(tab_width as u32);
    }
    if let Some(margin) = editorconfig.max_line_length {
        style.right_margin = Some(margin);
    }
    (style.formats()
        || !style.import_layout.is_empty()
        || !style.packages_to_use_import_on_demand.is_empty())
    .then_some(style)
}

/// The style of one `Project.xml` text.
pub fn parse_project(text: &str) -> Option<ProjectStyle> {
    let document = Document::parse(text).ok()?;
    let section = document.descendants().find(|node| {
        node.has_tag_name("codeStyleSettings")
            && node
                .attribute("language")
                .is_some_and(|name| name.eq_ignore_ascii_case("JAVA"))
    });
    let java = document
        .descendants()
        .find(|node| node.has_tag_name("JavaCodeStyleSettings"));
    let mut style = ProjectStyle {
        indent_size: section.and_then(|node| number_option(node, "INDENT_SIZE")),
        continuation_indent: section
            .and_then(|node| number_option(node, "CONTINUATION_INDENT_SIZE")),
        tab_size: section.and_then(|node| number_option(node, "TAB_SIZE")),
        use_tabs: section.and_then(|node| bool_option(node, "USE_TAB_CHARACTER")),
        right_margin: number_option(document.root_element(), "RIGHT_MARGIN")
            .or_else(|| section.and_then(|node| number_option(node, "RIGHT_MARGIN"))),
        class_count_to_use_import_on_demand: java
            .and_then(|node| number_option(node, "CLASS_COUNT_TO_USE_IMPORT_ON_DEMAND")),
        names_count_to_use_import_on_demand: java
            .and_then(|node| number_option(node, "NAMES_COUNT_TO_USE_IMPORT_ON_DEMAND")),
        ..ProjectStyle::default()
    };
    if let Some(java) = java {
        style.packages_to_use_import_on_demand = java
            .children()
            .find(|node| {
                node.has_tag_name("option")
                    && node.attribute("name") == Some("PACKAGES_TO_USE_IMPORT_ON_DEMAND")
            })
            .map(|option| {
                option
                    .descendants()
                    .filter(|node| node.has_tag_name("package"))
                    .filter_map(|node| node.attribute("name"))
                    .filter(|name| !name.is_empty())
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default();
        style.import_layout = java
            .children()
            .find(|node| {
                node.has_tag_name("option") && node.attribute("name") == Some("IMPORT_LAYOUT_TABLE")
            })
            .map(|option| {
                option
                    .descendants()
                    .filter_map(|node| {
                        if node.has_tag_name("package") {
                            Some(ImportEntry::Package {
                                name: node.attribute("name").unwrap_or_default().to_owned(),
                                static_import: node
                                    .attribute("static")
                                    .is_some_and(|value| value.eq_ignore_ascii_case("true")),
                            })
                        } else {
                            node.has_tag_name("emptyLine")
                                .then_some(ImportEntry::EmptyLine)
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
    }
    (style.formats()
        || !style.import_layout.is_empty()
        || !style.packages_to_use_import_on_demand.is_empty())
    .then_some(style)
}

/// The Eclipse profile of the formatter options the project names.
pub fn formatter_profile(style: &ProjectStyle) -> String {
    let mut settings = String::new();
    if let Some(tabs) = style.use_tabs {
        push_setting(
            &mut settings,
            "org.eclipse.jdt.core.formatter.tabulation.char",
            if tabs { "tab" } else { "space" },
        );
    }
    if let Some(size) = style.tab_size.or(style.indent_size) {
        push_setting(
            &mut settings,
            "org.eclipse.jdt.core.formatter.tabulation.size",
            &size.to_string(),
        );
    }
    if let Some(size) = style.indent_size {
        push_setting(
            &mut settings,
            "org.eclipse.jdt.core.formatter.indentation.size",
            &size.to_string(),
        );
    }
    if let Some(continuation) = style.continuation_indent {
        // Eclipse counts continuation levels, not columns.
        let level = (continuation / style.indent_size.unwrap_or(4).max(1)).max(1);
        push_setting(
            &mut settings,
            "org.eclipse.jdt.core.formatter.continuation_indentation",
            &level.to_string(),
        );
    }
    if let Some(margin) = style.right_margin {
        push_setting(
            &mut settings,
            "org.eclipse.jdt.core.formatter.lineSplit",
            &margin.to_string(),
        );
    }
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"no\"?>\n\
         <profiles version=\"{PROFILE_VERSION}\">\n\
         <profile kind=\"CodeFormatterProfile\" name=\"{}\" version=\"{PROFILE_VERSION}\">\n\
         {settings}\
         </profile>\n\
         </profiles>\n",
        escape_xml(PROFILE_NAME)
    )
}

/// The preferences the Eclipse import organizer reads: the group order, the count that
/// turns imports into `*`, and the same count for static imports.
pub fn import_prefs(style: &ProjectStyle) -> Vec<(String, String)> {
    let mut prefs = Vec::new();
    let order: Vec<&str> = style
        .import_layout
        .iter()
        .filter_map(|entry| match entry {
            ImportEntry::Package {
                name,
                static_import: false,
            } => Some(name.as_str()),
            _ => None,
        })
        .collect();
    if !order.is_empty() {
        prefs.push((
            "org.eclipse.jdt.ui.importorder".to_owned(),
            format!("{};", order.join(";")),
        ));
    }
    let threshold = style
        .names_count_to_use_import_on_demand
        .or(style.class_count_to_use_import_on_demand);
    if let Some(threshold) = threshold {
        prefs.push((
            "org.eclipse.jdt.ui.ondemandthreshold".to_owned(),
            threshold.to_string(),
        ));
        prefs.push((
            "org.eclipse.jdt.ui.staticondemandthreshold".to_owned(),
            threshold.to_string(),
        ));
    }
    prefs
}

/// The package groups of `java.completion.importOrder`.
pub fn import_order(style: &ProjectStyle) -> Vec<String> {
    style
        .import_layout
        .iter()
        .filter_map(|entry| match entry {
            ImportEntry::Package {
                name,
                static_import: false,
            } if !name.is_empty() => Some(name.clone()),
            _ => None,
        })
        .collect()
}

/// Writes the formatter profile under the state folder and leaves the one of the
/// previous style out. A project that names no formatting option gets no profile.
pub fn prepare(state_dir: &Path, style: &ProjectStyle) -> Prepared {
    let mut prepared = Prepared {
        prefs: import_prefs(style),
        import_order: import_order(style),
        formatter: None,
    };
    if !style.formats() {
        return prepared;
    }
    let profile = formatter_profile(style);
    let path = state_dir.join(format!(
        "{FORMATTER_PREFIX}{:016x}{FORMATTER_SUFFIX}",
        hash(&profile)
    ));
    if let Some(parent) = path.parent()
        && let Err(error) = std::fs::create_dir_all(parent)
    {
        eprintln!("the formatter profile of the project is not saved: {error}");
        return prepared;
    }
    if let Err(error) = std::fs::write(&path, profile) {
        eprintln!("the formatter profile of the project is not saved: {error}");
        return prepared;
    }
    for entry in std::fs::read_dir(state_dir).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(FORMATTER_PREFIX)
            && name.ends_with(FORMATTER_SUFFIX)
            && entry.path() != path
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
    prepared.formatter = Some(path);
    prepared
}

/// The text of a preferences file of the server, one `key=value` per line.
pub fn prefs_text(prefs: &[(String, String)]) -> String {
    prefs
        .iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect()
}

fn push_setting(out: &mut String, id: &str, value: &str) {
    let _ = writeln!(
        out,
        "<setting id=\"{}\" value=\"{}\"/>",
        escape_xml(id),
        escape_xml(value)
    );
}

fn escape_xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn hash(text: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in text.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

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

fn option(node: roxmltree::Node, name: &str) -> Option<String> {
    node.children()
        .find(|child| child.has_tag_name("option") && child.attribute("name") == Some(name))
        .and_then(|child| child.attribute("value"))
        .map(str::to_owned)
}

fn number_option(node: roxmltree::Node, name: &str) -> Option<u32> {
    option(node, name)?.trim().parse().ok()
}

fn bool_option(node: roxmltree::Node, name: &str) -> Option<bool> {
    match option(node, name)?.trim().to_ascii_lowercase().as_str() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use super::{
        ImportEntry, PROFILE_NAME, ProjectStyle, formatter_profile, import_prefs, parse_project,
        prepare, read_project_style,
    };

    const PROJECT: &str = r#"<code_scheme name="Project" version="173">
  <option name="RIGHT_MARGIN" value="100" />
  <JavaCodeStyleSettings>
    <option name="CLASS_COUNT_TO_USE_IMPORT_ON_DEMAND" value="5" />
    <option name="NAMES_COUNT_TO_USE_IMPORT_ON_DEMAND" value="3" />
    <option name="PACKAGES_TO_USE_IMPORT_ON_DEMAND">
      <value>
        <package name="java.util" />
        <package name="org.junit" />
      </value>
    </option>
    <option name="IMPORT_LAYOUT_TABLE">
      <value>
        <package name="" withSubpackages="true" static="false" />
        <emptyLine />
        <package name="javax" withSubpackages="true" static="false" />
        <package name="java" withSubpackages="true" static="false" />
        <package name="org" withSubpackages="true" static="false" />
        <package name="com" withSubpackages="true" static="false" />
        <package name="" withSubpackages="true" static="true" />
      </value>
    </option>
  </JavaCodeStyleSettings>
  <codeStyleSettings language="JAVA">
    <option name="INDENT_SIZE" value="2" />
    <option name="CONTINUATION_INDENT_SIZE" value="4" />
    <option name="TAB_SIZE" value="2" />
    <option name="USE_TAB_CHARACTER" value="true" />
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

    fn write(root: &Path, relative: &str, text: &str) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    #[test]
    fn the_java_section_gives_the_indent_and_the_import_layout() {
        let style = parse_project(PROJECT).unwrap();
        assert_eq!(style.indent_size, Some(2));
        assert_eq!(style.continuation_indent, Some(4));
        assert_eq!(style.tab_size, Some(2));
        assert_eq!(style.use_tabs, Some(true));
        assert_eq!(style.right_margin, Some(100));
        assert_eq!(style.names_count_to_use_import_on_demand, Some(3));
        assert_eq!(
            style.packages_to_use_import_on_demand,
            ["java.util", "org.junit"]
        );
        assert!(matches!(style.import_layout[1], ImportEntry::EmptyLine));
        assert_eq!(
            style.import_layout.last(),
            Some(&ImportEntry::Package {
                name: String::new(),
                static_import: true
            })
        );
    }

    #[test]
    fn the_eclipse_profile_carries_the_sizes_and_the_margin() {
        let profile = formatter_profile(&parse_project(PROJECT).unwrap());
        assert!(profile.contains(&format!("name=\"{PROFILE_NAME}\"")));
        assert!(profile.contains("tabulation.char\" value=\"tab\""));
        assert!(profile.contains("tabulation.size\" value=\"2\""));
        assert!(profile.contains("indentation.size\" value=\"2\""));
        assert!(profile.contains("continuation_indentation\" value=\"2\""));
        assert!(profile.contains("lineSplit\" value=\"100\""));
    }

    #[test]
    fn the_import_preferences_name_the_order_and_the_star_count() {
        let prefs = import_prefs(&parse_project(PROJECT).unwrap());
        assert!(prefs.contains(&(
            "org.eclipse.jdt.ui.importorder".to_owned(),
            ";javax;java;org;com;".to_owned()
        )));
        assert!(prefs.contains(&(
            "org.eclipse.jdt.ui.ondemandthreshold".to_owned(),
            "3".to_owned()
        )));
        assert!(prefs.contains(&(
            "org.eclipse.jdt.ui.staticondemandthreshold".to_owned(),
            "3".to_owned()
        )));
    }

    #[test]
    fn the_prepared_profile_lands_in_the_state_folder_and_the_switch_turns_it_off() {
        let root = test_support::scratch("jdtls-code-style");
        write(&root, ".idea/codeStyles/Project.xml", PROJECT);
        write(&root, ".idea/codeStyles/codeStyleConfig.xml", CONFIG_ON);
        let style = read_project_style(&root).unwrap();
        let state = root.join("state");
        let prepared = prepare(&state, &style);
        let formatter = prepared.formatter.unwrap();
        assert!(formatter.is_file());
        assert_eq!(
            fs::read_to_string(&formatter).unwrap(),
            formatter_profile(&style)
        );
        assert!(!prepared.prefs.is_empty());

        fs::write(
            root.join(".idea/codeStyles/codeStyleConfig.xml"),
            CONFIG_OFF,
        )
        .unwrap();
        assert_eq!(read_project_style(&root), None);

        write(&root, ".idea/codeStyles/codeStyleConfig.xml", CONFIG_ON);
        write(&root, ".arro/no-project-code-style", "");
        assert_eq!(read_project_style(&root), None);
    }

    #[test]
    fn editorconfig_wins_over_the_project_files_for_the_formatter() {
        let root = test_support::scratch("jdtls-code-style-editorconfig");
        write(&root, ".idea/codeStyles/Project.xml", PROJECT);
        write(&root, ".idea/codeStyles/codeStyleConfig.xml", CONFIG_ON);
        write(
            &root,
            ".editorconfig",
            "root = true\n[*.java]\nindent_style = space\nindent_size = 6\nmax_line_length = 80\n",
        );
        let style = read_project_style(&root).unwrap();
        assert_eq!(style.indent_size, Some(6));
        assert_eq!(style.use_tabs, Some(false));
        assert_eq!(style.right_margin, Some(80));
        assert!(
            style.formats(),
            "the project style still has the import layout"
        );
    }

    #[test]
    fn a_project_without_the_java_options_has_no_style() {
        let style = parse_project("<code_scheme name=\"Project\" />");
        assert_eq!(style, None);
        let empty = ProjectStyle::default();
        assert_eq!(prepare(Path::new("no-such-state"), &empty).formatter, None);
    }
}

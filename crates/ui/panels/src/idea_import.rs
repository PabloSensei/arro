//! File > Import Settings from IntelliJ IDEA: the settings of another IDE, read from its
//! configuration folder or from the zip Export Settings writes. The parts are chosen in a
//! dialog, laid over the keymap and the settings of the window, and the dialog ends with a
//! short list of what came and what stayed out.
//!
//! Only the files named here are opened. The passwords, tokens and keys of the other IDE
//! live in `options/security.xml`, `*.pwd` and `c.kdbx`; they are named nowhere in this
//! module and are never read.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use keymap::{KeymapFile, Shortcut};
use templates::{CustomTemplate, TemplateField, TemplateOverrides, id_of};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

const WIDTH: f32 = 560.;
/// The folder of the other IDE under the profile of the user.
const CONFIG_FOLDERS: &[&str] = &["IntelliJIdea", "IdeaIC"];
/// The names of the folder where the other IDE keeps the code style.
const CODE_STYLE_FOLDER: &str = "codestyles";
const KEYMAP_FOLDER: &str = "keymaps";
const TEMPLATE_FOLDER: &str = "templates";

/// The parts of the other IDE the dialog offers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Choices {
    pub keymap: bool,
    pub font: bool,
    pub code_style: bool,
    pub theme: bool,
    pub templates: bool,
}

impl Default for Choices {
    fn default() -> Self {
        Self {
            keymap: true,
            font: true,
            code_style: true,
            theme: true,
            templates: true,
        }
    }
}

/// One configuration folder of the other IDE.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct IdeaConfig {
    pub path: PathBuf,
    pub label: String,
}

/// What the other IDE named and the window can take.
#[derive(Clone, Debug, Default)]
pub(crate) struct Prepared {
    /// The lines the dialog shows at the end.
    pub summary: Vec<String>,
    pub font: Option<FontSettings>,
    pub style: Option<StyleSettings>,
    /// The name of the theme of the other IDE, looked up in the catalog when it is used.
    pub theme_name: Option<String>,
    /// The text of `keymap.json` to write, when the keymap part is chosen.
    pub keymap_text: Option<String>,
    /// The text of `templates.json` to write, when the live templates are chosen.
    pub templates_text: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct FontSettings {
    pub family: Option<String>,
    pub size: Option<f32>,
    pub line_spacing: Option<f32>,
    pub ligatures: Option<bool>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct StyleSettings {
    pub use_tabs: bool,
    pub indent_size: u32,
    pub tab_size: u32,
    pub right_margin: Option<u32>,
}

/// The File menu command.
pub(crate) fn bind_commands(workspace: &mut Workspace, _cx: &mut Context<Workspace>) {
    workspace.register_command(
        "settings.import_idea",
        "Import Settings from IntelliJ IDEA...",
        import,
    );
}

fn import(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let weak = cx.weak_entity();
    let settings_dir = workspace.settings_dir().to_path_buf();
    let dialog = cx.new(|cx| IdeaImportDialog {
        workspace: weak,
        focus: cx.focus_handle(),
        settings_dir,
        sources: Vec::new(),
        selected: 0,
        chosen: None,
        choices: Choices::default(),
        scanning: true,
        busy: false,
        result: Vec::new(),
        error: None,
    });
    let focus = dialog.read(cx).focus.clone();
    workspace.show_overlay(dialog.clone().into(), focus, window, cx);
    cx.spawn_in(window, async move |_workspace, cx| {
        let found = cx
            .background_executor()
            .spawn(async move { detected_configs() })
            .await;
        dialog.update(cx, |dialog, cx| {
            dialog.sources = found;
            dialog.scanning = false;
            cx.notify();
        });
    })
    .detach();
}

/// The configuration folders of the other IDE under the home of the user, newest first.
/// A test never looks at the real home: it names its own folders in [`configs_in`].
pub(crate) fn detected_configs() -> Vec<IdeaConfig> {
    #[cfg(test)]
    return Vec::new();
    #[cfg(not(test))]
    {
        let Some(home) = home_folder() else {
            return Vec::new();
        };
        configs_in(&jetbrains_folder(&home))
    }
}

/// The profile of the user, where the other IDE keeps its configuration.
#[cfg(not(test))]
fn home_folder() -> Option<PathBuf> {
    #[cfg(windows)]
    let name = std::env::var_os("APPDATA").or_else(|| std::env::var_os("USERPROFILE"))?;
    #[cfg(not(windows))]
    let name = std::env::var_os("HOME")?;
    Some(PathBuf::from(name))
}

/// The folder of the other IDE under the profile of the user, per system.
pub(crate) fn jetbrains_folder(home: &Path) -> PathBuf {
    #[cfg(windows)]
    return home.join("JetBrains");
    #[cfg(target_os = "macos")]
    return home
        .join("Library")
        .join("Application Support")
        .join("JetBrains");
    #[cfg(all(unix, not(target_os = "macos")))]
    return home.join(".config").join("JetBrains");
}

/// The configuration folders in `folder`, newest version first. A folder stays out when
/// it is not one of the two families of the IDE or names no version.
pub(crate) fn configs_in(folder: &Path) -> Vec<IdeaConfig> {
    let mut found: Vec<((u32, u32), IdeaConfig)> = std::fs::read_dir(folder)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .filter_map(|entry| {
            let label = entry.file_name().to_string_lossy().into_owned();
            let version = version_of(&label)?;
            Some((
                version,
                IdeaConfig {
                    path: entry.path(),
                    label,
                },
            ))
        })
        .collect();
    found.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| left.1.label.cmp(&right.1.label))
    });
    found.into_iter().map(|(_, config)| config).collect()
}

/// The version of a folder name such as `IntelliJIdea2025.2`.
fn version_of(label: &str) -> Option<(u32, u32)> {
    let rest = CONFIG_FOLDERS
        .iter()
        .find_map(|prefix| label.strip_prefix(prefix))?;
    let (major, minor) = rest.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// Reads the chosen parts of `source`: a configuration folder or a zip of Export Settings.
/// Blocks: the dialog runs it in the background.
pub(crate) fn read(
    source: &Path,
    choices: Choices,
    settings_dir: &Path,
) -> Result<Prepared, String> {
    if !source.exists() {
        return Err(format!("{} does not exist", source.display()));
    }
    let files = read_source(source)?;
    let mut prepared = Prepared::default();
    if choices.font {
        read_font(&files, &mut prepared);
    }
    if choices.code_style {
        read_code_style(&files, &mut prepared);
    }
    if choices.theme {
        read_theme(&files, &mut prepared);
    }
    if choices.keymap {
        read_keymap(&files, settings_dir, &mut prepared)?;
    }
    if choices.templates {
        read_templates(&files, settings_dir, &mut prepared);
    }
    if prepared.summary.is_empty() {
        prepared.summary.push(
            "The chosen parts are not in this folder or archive, nothing is imported.".to_owned(),
        );
    }
    Ok(prepared)
}

/// The files the import may open, as `(name, text)`. A folder gives its own files of the
/// named subfolders; an archive gives the same names, and no other entry is opened.
fn read_source(source: &Path) -> Result<Vec<(String, String)>, String> {
    if source.is_dir() {
        let mut files = Vec::new();
        for name in [
            "options/keymap.xml",
            "options/laf.xml",
            "options/colors.scheme.xml",
            "options/editor-font.xml",
        ] {
            if let Ok(text) = std::fs::read_to_string(source.join(name)) {
                files.push((name.to_owned(), text));
            }
        }
        for folder in [KEYMAP_FOLDER, CODE_STYLE_FOLDER, TEMPLATE_FOLDER] {
            let directory = source.join(folder);
            let Ok(entries) = std::fs::read_dir(&directory) else {
                continue;
            };
            let mut names: Vec<String> = entries
                .flatten()
                .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_file()))
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| is_xml(name))
                .collect();
            names.sort();
            for name in names {
                let relative = format!("{folder}/{name}");
                if let Ok(text) = std::fs::read_to_string(directory.join(&name)) {
                    files.push((relative, text));
                }
            }
        }
        return Ok(files);
    }
    store::settings_archive::read_wanted_texts(source, |name| {
        matches!(
            name,
            "options/keymap.xml"
                | "options/laf.xml"
                | "options/colors.scheme.xml"
                | "options/editor-font.xml"
        ) || [KEYMAP_FOLDER, CODE_STYLE_FOLDER, TEMPLATE_FOLDER]
            .iter()
            .any(|folder| {
                name.strip_prefix(folder)
                    .and_then(|rest| rest.strip_prefix('/'))
                    .is_some_and(|rest| !rest.is_empty() && !rest.contains('/') && is_xml(rest))
            })
    })
    .map_err(|error| format!("{}: {error}", source.display()))
}

fn is_xml(name: &str) -> bool {
    name.to_ascii_lowercase().ends_with(".xml")
}

/// The editor font of `options/editor-font.xml`.
fn read_font(files: &[(String, String)], prepared: &mut Prepared) {
    let Some((_, text)) = files
        .iter()
        .find(|(name, _)| name == "options/editor-font.xml")
    else {
        prepared
            .summary
            .push("Editor font: options/editor-font.xml is not there.".to_owned());
        return;
    };
    let font = parse_editor_font(text);
    let mut parts = Vec::new();
    if let Some(family) = &font.family {
        parts.push(format!("typeface {family}"));
    }
    if let Some(size) = font.size {
        parts.push(format!("size {size}"));
    }
    if let Some(spacing) = font.line_spacing {
        parts.push(format!("line spacing {spacing}"));
    }
    if parts.is_empty() {
        prepared
            .summary
            .push("Editor font: the file names no font.".to_owned());
        return;
    }
    prepared
        .summary
        .push(format!("Editor font: {}.", parts.join(", ")));
    if font.ligatures == Some(true) {
        prepared.summary.push(
            "Ligatures: turned off in Arro, so the setting of the other IDE is not imported."
                .to_owned(),
        );
    }
    prepared.font = Some(font);
}

/// The font of one `editor-font.xml` text.
pub(crate) fn parse_editor_font(text: &str) -> FontSettings {
    let mut font = FontSettings::default();
    for tag in tags(text) {
        if tag.end || tag.name != "option" {
            continue;
        }
        let Some(name) = attr(&tag, "name") else {
            continue;
        };
        let Some(value) = attr(&tag, "value") else {
            continue;
        };
        let value = decode(value);
        match name {
            "EDITOR_FONT_NAME" | "FONT_NAME" => {
                if !value.trim().is_empty() {
                    font.family = Some(value);
                }
            }
            "EDITOR_FONT_SIZE" | "FONT_SIZE" => {
                if let Ok(size) = value.trim().parse::<f32>()
                    && size.is_finite()
                {
                    font.size =
                        Some(size.clamp(workspace::MIN_FONT_SIZE, workspace::MAX_FONT_SIZE));
                }
            }
            "EDITOR_LINE_SPACING" | "LINE_SPACING" => {
                if let Ok(spacing) = value.trim().parse::<f32>()
                    && spacing.is_finite()
                {
                    font.line_spacing = Some(
                        spacing.clamp(workspace::MIN_LINE_SPACING, workspace::MAX_LINE_SPACING),
                    );
                }
            }
            "EDITOR_LIGATURES" | "USE_LIGATURES" | "LIGATURES" => {
                font.ligatures = Some(value.eq_ignore_ascii_case("true"));
            }
            _ => {}
        }
    }
    font
}

/// The default code style of `codestyles/*.xml`, with the parser of `.idea/codeStyles`.
fn read_code_style(files: &[(String, String)], prepared: &mut Prepared) {
    let Some((name, text)) = style_file(files) else {
        prepared
            .summary
            .push("Code style: no file in codestyles, nothing is imported.".to_owned());
        return;
    };
    let Some(style) = text_core::idea_style::parse_style_text(text) else {
        prepared
            .summary
            .push(format!("Code style: {name} names no indentation."));
        return;
    };
    let Some((use_tabs, indent_size, tab_size)) = style.indentation else {
        prepared
            .summary
            .push(format!("Code style: {name} names no indentation."));
        return;
    };
    prepared.style = Some(StyleSettings {
        use_tabs,
        indent_size: indent_size as u32,
        tab_size: tab_size as u32,
        right_margin: style.right_margin,
    });
    prepared.summary.push(format!(
        "Code style: {name}, indent {indent_size} with {tab_size} columns of a tab."
    ));
}

fn style_file(files: &[(String, String)]) -> Option<&(String, String)> {
    files
        .iter()
        .find(|(name, _)| name == "codestyles/Project.xml")
        .or_else(|| {
            files
                .iter()
                .find(|(name, _)| name.starts_with("codestyles/"))
        })
}

/// The theme of the other IDE: the look it chose and its colour scheme.
fn read_theme(files: &[(String, String)], prepared: &mut Prepared) {
    let name = files
        .iter()
        .filter(|(name, _)| name == "options/laf.xml")
        .find_map(|(_, text)| parse_theme_name(text))
        .or_else(|| {
            files
                .iter()
                .filter(|(name, _)| name == "options/colors.scheme.xml")
                .find_map(|(_, text)| parse_theme_name(text))
        });
    match name {
        Some(name) => prepared.theme_name = Some(name),
        None => prepared
            .summary
            .push("Theme: the other IDE names no theme, nothing is imported.".to_owned()),
    }
}

/// The name of the theme in a `laf.xml` or `colors.scheme.xml` text.
pub(crate) fn parse_theme_name(text: &str) -> Option<String> {
    for tag in tags(text) {
        if tag.end {
            continue;
        }
        if tag.name == "global_color_scheme" {
            let name = attr(&tag, "name").map(decode)?;
            if !name.trim().is_empty() {
                return Some(name);
            }
        }
        if tag.name == "laf" {
            let name = attr(&tag, "name").map(decode)?;
            if !name.trim().is_empty() {
                return Some(name);
            }
        }
        if tag.name == "option" {
            let Some(name) = attr(&tag, "name") else {
                continue;
            };
            if !matches!(
                name,
                "currentTheme" | "CURRENT_THEME" | "laf" | "LafName" | "theme"
            ) {
                continue;
            }
            let value = attr(&tag, "value").map(decode)?;
            if !value.trim().is_empty() {
                return Some(value);
            }
        }
    }
    None
}

/// The changed keymap of `keymaps/*.xml` over the layout of Arro.
fn read_keymap(
    files: &[(String, String)],
    settings_dir: &Path,
    prepared: &mut Prepared,
) -> Result<(), String> {
    let Some((name, text)) = keymap_file(files) else {
        prepared
            .summary
            .push("Keymap: no keymap of the user in keymaps, nothing is imported.".to_owned());
        return Ok(());
    };
    let layout = parse_keymap(text);
    let (registry, base) = keymap::default_keymap();
    let path = settings_dir.join(keymap::KEYMAP_FILE);
    let mut file = match KeymapFile::load(&path, &registry) {
        Ok(loaded) => loaded.file,
        Err(_) => {
            prepared.summary.push(format!(
                "Keymap: {} is broken and is left as it is.",
                keymap::KEYMAP_FILE
            ));
            return Ok(());
        }
    };
    let effective = file
        .apply(&registry, &base)
        .unwrap_or_else(|_| base.clone());
    let actions = action_of();
    let mut applied = 0;
    let mut skipped = Vec::new();
    for (idea, shortcuts) in &layout.actions {
        let Some(action) = actions.get(idea.as_str()) else {
            skipped.push(idea.clone());
            continue;
        };
        let current = effective.shortcuts_for(action);
        for shortcut in shortcuts {
            if !current.contains(shortcut) {
                file.add_shortcut(action, shortcut);
            }
        }
        for shortcut in &current {
            if !shortcuts.contains(shortcut) {
                file.remove_shortcut(action, shortcut);
            }
        }
        applied += 1;
    }
    prepared.keymap_text = Some(file.to_text());
    let mut line = format!("Keymap: {applied} actions from {name}");
    if !skipped.is_empty() {
        line.push_str("; actions Arro does not have: ");
        line.push_str(&skipped.join(", "));
    }
    line.push('.');
    prepared.summary.push(line);
    Ok(())
}

/// The keymap of the user that the import takes: the one the other IDE chose, otherwise
/// the first file that is not the default layout.
fn keymap_file(files: &[(String, String)]) -> Option<(&String, &String)> {
    let active = files
        .iter()
        .find(|(name, _)| name == "options/keymap.xml")
        .and_then(|(_, text)| active_keymap(text));
    let keymaps: Vec<&(String, String)> = files
        .iter()
        .filter(|(name, _)| name.starts_with("keymaps/"))
        .collect();
    if let Some(active) = active
        && let Some(found) = keymaps
            .iter()
            .find(|(_, text)| parse_keymap(text).name == active)
    {
        return Some((&found.0, &found.1));
    }
    keymaps
        .into_iter()
        .find(|(_, text)| {
            let layout = parse_keymap(text);
            layout.name != "$default" && layout.parent == "$default"
        })
        .map(|(name, text)| (name, text))
}

/// The name of the keymap `options/keymap.xml` says is in use.
fn active_keymap(text: &str) -> Option<String> {
    for tag in tags(text) {
        if tag.end || tag.name != "option" {
            continue;
        }
        if attr(&tag, "name") == Some("activeKeymap") {
            return attr(&tag, "value").map(decode);
        }
    }
    None
}

/// One keymap file of the other IDE: its name, its parent layout and the actions the user
/// changed relative to that parent.
#[derive(Debug, Default)]
struct IdeaKeymap {
    name: String,
    parent: String,
    actions: Vec<(String, Vec<Shortcut>)>,
}

fn parse_keymap(text: &str) -> IdeaKeymap {
    let mut layout = IdeaKeymap::default();
    let mut current: Option<(String, Vec<Shortcut>)> = None;
    for tag in tags(text) {
        if tag.end {
            if tag.name == "action"
                && let Some((id, keys)) = current.take()
            {
                layout.actions.push((id, keys));
            }
            continue;
        }
        match tag.name {
            "keymap" => {
                layout.name = attr(&tag, "name").map(decode).unwrap_or_default();
                layout.parent = attr(&tag, "parent").map(decode).unwrap_or_default();
            }
            "action" => {
                let id = attr(&tag, "id").map(decode).unwrap_or_default();
                if tag.self_closing {
                    layout.actions.push((id, Vec::new()));
                } else {
                    current = Some((id, Vec::new()));
                }
            }
            "keyboard-shortcut" => {
                let Some((_, keys)) = current.as_mut() else {
                    continue;
                };
                let first = attr(&tag, "first-keystroke").unwrap_or_default();
                let second = attr(&tag, "second-keystroke");
                if let Some(shortcut) = idea_shortcut(first, second) {
                    keys.push(shortcut);
                }
            }
            _ => {}
        }
    }
    layout
}

/// The shortcut of one `keyboard-shortcut` element, in the form the keymap file reads.
fn idea_shortcut(first: &str, second: Option<&str>) -> Option<Shortcut> {
    let mut text = idea_stroke(first)?;
    if let Some(second) = second {
        text.push(' ');
        text.push_str(&idea_stroke(second)?);
    }
    Shortcut::parse(&text).ok()
}

/// One keystroke of the other IDE, such as `ctrl alt LEFT`, as `ctrl-alt-left`.
fn idea_stroke(text: &str) -> Option<String> {
    let (mut ctrl, mut alt, mut shift, mut cmd) = (false, false, false, false);
    let mut key = None;
    for word in text.split_whitespace() {
        match word.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => ctrl = true,
            "alt" => alt = true,
            "shift" => shift = true,
            "meta" => cmd = true,
            _ => key = Some(idea_key(word)?),
        }
    }
    let mut parts = Vec::new();
    if ctrl {
        parts.push("ctrl");
    }
    if alt {
        parts.push("alt");
    }
    if shift {
        parts.push("shift");
    }
    if cmd {
        parts.push("cmd");
    }
    let key = key?;
    parts.push(&key);
    Some(parts.join("-"))
}

/// The name of one key of the other IDE in the words of the keymap file.
fn idea_key(word: &str) -> Option<String> {
    let named = match word.to_ascii_uppercase().as_str() {
        "LEFT" => "left",
        "RIGHT" => "right",
        "UP" => "up",
        "DOWN" => "down",
        "PAGE_UP" => "pageup",
        "PAGE_DOWN" => "pagedown",
        "HOME" => "home",
        "END" => "end",
        "INSERT" => "insert",
        "DELETE" => "delete",
        "ENTER" => "enter",
        "TAB" => "tab",
        "SPACE" => "space",
        "ESCAPE" => "escape",
        "BACK_SPACE" => "backspace",
        "CONTEXT_MENU" => "menu",
        "COMMA" => ",",
        "PERIOD" => ".",
        "SLASH" => "/",
        "BACK_SLASH" => "\\",
        "SEMICOLON" => ";",
        "EQUALS" => "=",
        "MINUS" => "-",
        "OPEN_BRACKET" => "[",
        "CLOSE_BRACKET" => "]",
        "QUOTE" => "'",
        "BACK_QUOTE" => "`",
        other if other.starts_with('F') && other[1..].parse::<u8>().is_ok() => {
            return Some(other.to_ascii_lowercase());
        }
        _ => "",
    };
    if !named.is_empty() {
        return Some(named.to_owned());
    }
    if word.chars().count() == 1 && !word.starts_with(['-', ' ']) {
        return Some(word.to_ascii_lowercase());
    }
    None
}

/// The actions of the window, by the id of the other IDE.
fn action_of() -> HashMap<&'static str, &'static str> {
    let mut actions = HashMap::new();
    for (idea, action) in keymap::editor::IDEA_IDS
        .iter()
        .chain(keymap::editor::HANDLED_IDS)
        .chain(keymap::workspace::IDEA_IDS)
        .chain(keymap::workspace::EXTRA_IDEA_IDS)
    {
        actions.insert(*idea, *action);
    }
    actions
}

/// The live templates of `templates/*.xml`, added to `templates.json` when their variable
/// format matches the one of Arro.
fn read_templates(files: &[(String, String)], settings_dir: &Path, prepared: &mut Prepared) {
    let templates: Vec<IdeaTemplate> = files
        .iter()
        .filter(|(name, _)| name.starts_with("templates/"))
        .flat_map(|(_, text)| parse_templates(text))
        .collect();
    if templates.is_empty() {
        prepared
            .summary
            .push("Live templates: no template in templates, nothing is imported.".to_owned());
        return;
    }
    let path = settings_dir.join(templates::TEMPLATES_FILE);
    let mut overrides = match TemplateOverrides::load(&path) {
        Ok(loaded) => loaded.file,
        Err(_) => {
            prepared.summary.push(format!(
                "Live templates: {} is broken and is left as it is.",
                templates::TEMPLATES_FILE
            ));
            return;
        }
    };
    let mut added = 0;
    let mut present = 0;
    for template in templates {
        let group = if template.group.trim().is_empty() {
            "user".to_owned()
        } else {
            template.group.clone()
        };
        let id = id_of(&group, &template.abbreviation);
        if overrides.custom.iter().any(|known| known.id() == id) {
            present += 1;
            continue;
        }
        overrides.custom.push(CustomTemplate {
            id,
            kind: templates::Kind::Abbreviation,
            group,
            abbreviation: template.abbreviation,
            description: template.description,
            body: template.body,
            fields: template
                .variables
                .into_iter()
                .map(|variable| TemplateField {
                    name: variable.name,
                    expression: variable.expression,
                    default: variable.default,
                    skip: !variable.stop_at,
                })
                .collect(),
            contexts: vec![templates::ANY.to_owned()],
            expand_with: None,
            enabled: true,
        });
        added += 1;
    }
    prepared.templates_text = Some(overrides.to_text());
    let mut line = format!("Live templates: {added} added");
    if present > 0 {
        line.push_str(&format!(", {present} already present"));
    }
    line.push('.');
    prepared.summary.push(line);
}

/// One live template of the other IDE.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct IdeaTemplate {
    pub group: String,
    pub abbreviation: String,
    pub description: String,
    pub body: String,
    pub variables: Vec<IdeaVariable>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct IdeaVariable {
    pub name: String,
    pub expression: String,
    pub default: String,
    pub stop_at: bool,
}

/// The live templates of one `templates/*.xml` text. The body names fields as `$NAME$`,
/// the same form Arro reads.
pub(crate) fn parse_templates(text: &str) -> Vec<IdeaTemplate> {
    let mut templates = Vec::new();
    let mut group = String::new();
    let mut current: Option<IdeaTemplate> = None;
    for tag in tags(text) {
        if tag.end {
            if tag.name == "template"
                && let Some(template) = current.take()
            {
                templates.push(template);
            }
            continue;
        }
        match tag.name {
            "templateSet" => {
                group = attr(&tag, "group").map(decode).unwrap_or_default();
            }
            "template" => {
                let template = IdeaTemplate {
                    group: group.clone(),
                    abbreviation: attr(&tag, "name").map(decode).unwrap_or_default(),
                    description: attr(&tag, "description").map(decode).unwrap_or_default(),
                    body: attr(&tag, "value").map(decode).unwrap_or_default(),
                    variables: Vec::new(),
                };
                if tag.self_closing {
                    templates.push(template);
                } else {
                    current = Some(template);
                }
            }
            "variable" => {
                let Some(template) = current.as_mut() else {
                    continue;
                };
                template.variables.push(IdeaVariable {
                    name: attr(&tag, "name").map(decode).unwrap_or_default(),
                    expression: attr(&tag, "expression").map(decode).unwrap_or_default(),
                    default: attr(&tag, "defaultValue").map(decode).unwrap_or_default(),
                    stop_at: attr(&tag, "alwaysStopAt").is_none_or(|value| value != "false"),
                });
            }
            _ => {}
        }
    }
    templates
        .into_iter()
        .filter(|t| !t.abbreviation.is_empty())
        .collect()
}

/// Writes the prepared keymap, templates and settings into the window. The dialog calls it
/// on the window thread once `read` is done; the files are written in the background.
pub(crate) fn apply(
    workspace: &mut Workspace,
    prepared: Prepared,
    cx: &mut Context<Workspace>,
) -> Vec<String> {
    let Prepared {
        mut summary,
        font,
        style,
        theme_name,
        keymap_text,
        templates_text,
    } = prepared;
    if font.is_some() || style.is_some() {
        workspace.update_settings(
            |settings| {
                if let Some(font) = &font {
                    if let Some(family) = &font.family {
                        settings.editor_font_family = family.clone();
                    }
                    if let Some(size) = font.size {
                        settings.font_size = size;
                    }
                    if let Some(spacing) = font.line_spacing {
                        settings.line_spacing = spacing;
                    }
                }
                if let Some(style) = &style {
                    settings.use_tabs = style.use_tabs;
                    settings.indent_size = style.indent_size;
                    settings.tab_size = style.tab_size;
                    if let Some(margin) = style.right_margin {
                        settings.right_margin = margin;
                    }
                }
            },
            cx,
        );
    }
    if let Some(name) = theme_name {
        match theme_id(cx, &name) {
            Some(id) => {
                workspace.choose_theme(&id, cx);
                summary.push(format!("Theme: {name}."));
            }
            None => summary.push(format!(
                "Theme: {name} is not a theme of Arro, nothing is imported."
            )),
        }
    }
    if let Some(text) = keymap_text {
        let path = workspace.keymap_path().to_path_buf();
        let view = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let written = cx
                .background_executor()
                .spawn(async move { std::fs::write(path, text) })
                .await;
            view.update(cx, |workspace, cx| match written {
                Ok(()) => workspace.reload_keymap(cx),
                Err(error) => {
                    workspace.notify_user("Cannot write the keymap", error.to_string(), cx)
                }
            })
            .ok();
        })
        .detach();
    }
    if let Some(text) = templates_text {
        let folder = workspace.settings_dir().to_path_buf();
        let view = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let written = cx
                .background_executor()
                .spawn(async move {
                    std::fs::create_dir_all(&folder)?;
                    std::fs::write(folder.join(templates::TEMPLATES_FILE), text)
                })
                .await;
            view.update(cx, |workspace, cx| match written {
                Ok(()) => crate::settings_templates::load(workspace, cx),
                Err(error) => {
                    workspace.notify_user("Cannot write the templates", error.to_string(), cx)
                }
            })
            .ok();
        })
        .detach();
    }
    summary
}

/// The theme of the window that stands for the name of the other IDE.
fn theme_id(cx: &App, name: &str) -> Option<String> {
    let library = ThemeCatalog::library_of(cx);
    if let Some(found) = library.themes().iter().find(|theme| {
        theme.id.eq_ignore_ascii_case(name.trim()) || theme.name.eq_ignore_ascii_case(name.trim())
    }) {
        return Some(found.id.clone());
    }
    let known = match name.trim().to_ascii_lowercase().as_str() {
        "darcula" | "dark" | "intellij dark" | "high contrast" => "dark",
        "light" | "intellij light" | "sunlight" => "light",
        "high contrast dark" => "high-contrast",
        _ => return None,
    };
    library
        .themes()
        .iter()
        .any(|theme| theme.id == known)
        .then(|| known.to_owned())
}

/// The name of the source the dialog describes: the folder of the other IDE or the zip.
pub(crate) fn source_label(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

struct IdeaImportDialog {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    settings_dir: PathBuf,
    sources: Vec<IdeaConfig>,
    selected: usize,
    /// A folder or archive the user chose instead of the found folders.
    chosen: Option<PathBuf>,
    choices: Choices,
    scanning: bool,
    busy: bool,
    result: Vec<String>,
    error: Option<String>,
}

impl IdeaImportDialog {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn source(&self) -> Option<PathBuf> {
        self.chosen.clone().or_else(|| {
            self.sources
                .get(self.selected)
                .map(|config| config.path.clone())
        })
    }

    fn toggle(&mut self, part: &str, cx: &mut Context<Self>) {
        match part {
            "keymap" => self.choices.keymap = !self.choices.keymap,
            "font" => self.choices.font = !self.choices.font,
            "code-style" => self.choices.code_style = !self.choices.code_style,
            "theme" => self.choices.theme = !self.choices.theme,
            "templates" => self.choices.templates = !self.choices.templates,
            _ => return,
        }
        cx.notify();
    }

    fn choose(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let options = PathPromptOptions {
            files: true,
            directories: true,
            multiple: false,
            prompt: Some("Choose the IntelliJ IDEA Configuration or an Export".into()),
        };
        let paths = cx.prompt_for_paths(options);
        cx.spawn_in(window, async move |this, cx| {
            let chosen = paths.await.ok().and_then(Result::ok).flatten();
            let Some(path) = chosen.and_then(|paths| paths.into_iter().next()) else {
                return;
            };
            this.update(cx, |this, cx| {
                this.chosen = Some(path);
                this.result.clear();
                this.error = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        if !self.result.is_empty() {
            self.close(window, cx);
            return;
        }
        let Some(source) = self.source() else {
            self.error = Some("Choose the configuration folder or the export first.".to_owned());
            cx.notify();
            return;
        };
        let choices = self.choices;
        let settings_dir = self.settings_dir.clone();
        self.busy = true;
        self.result.clear();
        self.error = None;
        cx.notify();
        let read = cx
            .background_executor()
            .spawn(async move { read(&source, choices, &settings_dir) });
        cx.spawn_in(window, async move |this, cx| {
            let prepared = read.await;
            this.update_in(cx, |this, _window, cx| {
                this.busy = false;
                match prepared {
                    Ok(prepared) => {
                        let lines = this
                            .workspace
                            .update(cx, |workspace, cx| apply(workspace, prepared, cx));
                        match lines {
                            Ok(lines) => this.result = lines,
                            Err(_) => {
                                this.error = Some("The window was closed.".to_owned());
                            }
                        }
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                self.accept(window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.close(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }

    fn checkbox_row(
        &self,
        part: &'static str,
        label: &'static str,
        on: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        div()
            .id(part)
            .debug_selector(move || format!("idea-import-part-{part}"))
            .flex()
            .items_center()
            .gap(px(SPACE_M))
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .hover(|style| style.bg(theme.selection_hover))
            .child(checkbox(on, theme))
            .child(label)
            .on_click(cx.listener(move |this, _, _, cx| this.toggle(part, cx)))
    }
}

impl Render for IdeaImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let left = (window.viewport_size().width - px(WIDTH)) / 2.;
        let top = window.viewport_size().height * 0.12;
        let source_rows: Vec<Stateful<Div>> = self
            .sources
            .iter()
            .enumerate()
            .map(|(index, config)| {
                let chosen = self.chosen.is_none() && index == self.selected;
                div()
                    .id(("idea-import-source", index))
                    .debug_selector(move || format!("idea-import-source-{index}"))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .px(px(ROW_PADDING_X))
                    .py(px(SPACE_XS))
                    .rounded(px(ROW_RADIUS))
                    .when(chosen, |this| this.bg(theme.tab_selected))
                    .hover(|style| style.bg(theme.selection_hover))
                    .child(config.label.clone())
                    .child(
                        div()
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_secondary)
                            .child(config.path.display().to_string()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.selected = index;
                        this.chosen = None;
                        this.result.clear();
                        this.error = None;
                        cx.notify();
                    }))
            })
            .collect();
        let source = self
            .chosen
            .as_ref()
            .map(|path| source_label(path))
            .or_else(|| {
                self.sources
                    .get(self.selected)
                    .map(|config| config.label.clone())
            });
        let source_note = match (&self.chosen, self.scanning, source) {
            (Some(path), _, _) => format!("Chosen: {}", path.display()),
            (None, true, _) => {
                "Looking for the configuration folders of IntelliJ IDEA...".to_owned()
            }
            (None, false, Some(label)) => format!("Using {label}."),
            (None, false, None) => {
                "No configuration folder is found. Choose a folder or an export.".to_owned()
            }
        };
        let choices = self.choices;
        let rows = vec![
            self.checkbox_row("keymap", "Keymap", choices.keymap, &theme, cx),
            self.checkbox_row("font", "Editor font", choices.font, &theme, cx),
            self.checkbox_row("code-style", "Code style", choices.code_style, &theme, cx),
            self.checkbox_row("theme", "Theme", choices.theme, &theme, cx),
            self.checkbox_row("templates", "Live templates", choices.templates, &theme, cx),
        ];
        let results: Vec<Div> = self
            .result
            .iter()
            .enumerate()
            .map(|(index, line)| {
                div()
                    .debug_selector(move || format!("idea-import-result-{index}"))
                    .text_size(px(TEXT_SMALL))
                    .child(line.clone())
            })
            .collect();
        let body = surface(&theme)
            .id("idea-import")
            .debug_selector(|| "idea-import".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .text_size(px(TEXT_TITLE))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Import Settings from IntelliJ IDEA"),
                    )
                    .child(div().text_color(theme.text_secondary).child(source_note)),
            )
            .child(div().flex().flex_col().children(source_rows))
            .child(
                div()
                    .id("idea-import-choose")
                    .debug_selector(|| "idea-import-choose".to_owned())
                    .flex()
                    .items_center()
                    .px(px(SPACE_M))
                    .h(px(CONTROL_HEIGHT))
                    .rounded(px(CONTROL_RADIUS))
                    .hover(|style| style.bg(theme.selection_hover))
                    .child("Choose a Folder or an Export...")
                    .on_click(cx.listener(|this, _, window, cx| this.choose(window, cx))),
            )
            .child(div().flex().flex_col().children(rows))
            .when_some(self.error.clone(), |this, error| {
                this.child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.error)
                        .child(error),
                )
            })
            .child(div().flex().flex_col().children(results))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .id("idea-import-cancel")
                            .debug_selector(|| "idea-import-cancel".to_owned())
                            .px(px(SPACE_L))
                            .h(px(CONTROL_HEIGHT))
                            .flex()
                            .items_center()
                            .rounded(px(CONTROL_RADIUS))
                            .hover(|style| style.bg(theme.selection_hover))
                            .child("Close")
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        div()
                            .id("idea-import-run")
                            .debug_selector(|| "idea-import-run".to_owned())
                            .px(px(SPACE_L))
                            .h(px(CONTROL_HEIGHT))
                            .flex()
                            .items_center()
                            .rounded(px(CONTROL_RADIUS))
                            .bg(theme.accent)
                            .text_color(theme.over_accent)
                            .child(if self.busy { "Importing..." } else { "Import" })
                            .on_click(cx.listener(|this, _, window, cx| this.accept(window, cx))),
                    ),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("idea-import-enter", body))
    }
}

/// One XML tag of the files of the other IDE: its name, its attributes and whether it
/// closes an element. The files are simple lists, so the import reads tags only.
struct Tag<'a> {
    name: &'a str,
    attrs: Vec<(&'a str, &'a str)>,
    end: bool,
    self_closing: bool,
}

fn tags(text: &str) -> Vec<Tag<'_>> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(open) = text[at..].find('<').map(|offset| at + offset) {
        if text[open..].starts_with("<!--") {
            match text[open..].find("-->") {
                Some(end) => at = open + end + 3,
                None => break,
            }
            continue;
        }
        if text[open..].starts_with("<![CDATA[") {
            match text[open..].find("]]>") {
                Some(end) => at = open + end + 3,
                None => break,
            }
            continue;
        }
        let mut close = open + 1;
        let mut quote = None;
        while close < bytes.len() {
            match bytes[close] {
                b'"' | b'\'' if quote.is_none() => quote = Some(bytes[close]),
                byte if Some(byte) == quote => quote = None,
                b'>' if quote.is_none() => break,
                _ => {}
            }
            close += 1;
        }
        if close >= bytes.len() {
            break;
        }
        let inner = &text[open + 1..close];
        at = close + 1;
        if inner.starts_with(['!', '?']) {
            continue;
        }
        let end = inner.starts_with('/');
        let body = inner.trim_start_matches('/').trim();
        let self_closing = body.ends_with('/');
        let body = body.trim_end_matches('/');
        let mut parts = body.splitn(2, char::is_whitespace);
        let name = parts.next().unwrap_or_default();
        let attrs = attributes(parts.next().unwrap_or_default());
        found.push(Tag {
            name,
            attrs,
            end,
            self_closing,
        });
    }
    found
}

fn attributes(text: &str) -> Vec<(&str, &str)> {
    let bytes = text.as_bytes();
    let mut attrs = Vec::new();
    let mut at = 0;
    while at < bytes.len() {
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while at < bytes.len() && !bytes[at].is_ascii_whitespace() && bytes[at] != b'=' {
            at += 1;
        }
        let name = &text[start..at];
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if at >= bytes.len() || bytes[at] != b'=' {
            break;
        }
        at += 1;
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let Some(quote) = bytes.get(at).copied().filter(|b| *b == b'"' || *b == b'\'') else {
            break;
        };
        at += 1;
        let value_start = at;
        while at < bytes.len() && bytes[at] != quote {
            at += 1;
        }
        attrs.push((name, &text[value_start..at]));
        at += 1;
    }
    attrs
}

fn attr<'a>(tag: &Tag<'a>, name: &str) -> Option<&'a str> {
    tag.attrs
        .iter()
        .find(|(known, _)| *known == name)
        .map(|(_, value)| *value)
}

fn decode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find('&') {
        out.push_str(&rest[..at]);
        rest = &rest[at..];
        let Some(end) = rest.find(';') else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ => entity
                .strip_prefix("#x")
                .or_else(|| entity.strip_prefix("#X"))
                .and_then(|code| u32::from_str_radix(code, 16).ok())
                .or_else(|| {
                    entity
                        .strip_prefix('#')
                        .and_then(|code| code.parse::<u32>().ok())
                })
                .and_then(char::from_u32),
        };
        match decoded {
            Some(character) => {
                out.push(character);
                rest = &rest[end + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        IdeaTemplate, IdeaVariable, StyleSettings, configs_in, parse_editor_font, parse_keymap,
        parse_templates, parse_theme_name, read, read_source,
    };

    const KEYMAP: &str = r#"<keymap version="1" name="Mine" parent="$default">
  <action id="EditorLeft">
    <keyboard-shortcut first-keystroke="ctrl alt J" />
  </action>
  <action id="EditorDuplicate">
    <keyboard-shortcut first-keystroke="ctrl alt K" />
    <keyboard-shortcut first-keystroke="ctrl alt shift K" />
  </action>
  <action id="AnActionArroDoesNotHave">
    <keyboard-shortcut first-keystroke="ctrl alt U" />
  </action>
</keymap>"#;

    #[test]
    fn the_keymap_names_its_parent_and_the_changed_actions() {
        let layout = parse_keymap(KEYMAP);
        assert_eq!(layout.name, "Mine");
        assert_eq!(layout.parent, "$default");
        assert_eq!(layout.actions.len(), 3);
        assert_eq!(
            layout.actions[0]
                .1
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["ctrl-alt-j"]
        );
        assert_eq!(
            layout.actions[1]
                .1
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>(),
            ["ctrl-alt-k", "ctrl-alt-shift-k"]
        );
    }

    #[test]
    fn the_editor_font_gives_the_typeface_the_size_and_the_spacing() {
        let font = parse_editor_font(
            r#"<application>
  <component name="EditorColorsSchemeImpl">
    <option name="EDITOR_FONT_NAME" value="JetBrains Mono" />
    <option name="EDITOR_FONT_SIZE" value="15" />
    <option name="EDITOR_LINE_SPACING" value="1.2" />
    <option name="EDITOR_LIGATURES" value="true" />
  </component>
</application>"#,
        );
        assert_eq!(font.family.as_deref(), Some("JetBrains Mono"));
        assert_eq!(font.size, Some(15.));
        assert_eq!(font.line_spacing, Some(1.2));
        assert_eq!(font.ligatures, Some(true));
    }

    #[test]
    fn the_newest_configuration_folder_is_the_default() {
        let home = test_support::scratch("idea-configs");
        for name in [
            "IntelliJIdea2025.2",
            "IntelliJIdea2026.1",
            "IdeaIC2024.3",
            "Other2027.1",
        ] {
            fs::create_dir_all(home.join(name)).unwrap();
        }
        let found = configs_in(&home);
        let labels: Vec<&str> = found.iter().map(|config| config.label.as_str()).collect();
        assert_eq!(
            labels,
            ["IntelliJIdea2026.1", "IntelliJIdea2025.2", "IdeaIC2024.3"]
        );
    }

    #[test]
    fn the_profile_of_the_user_holds_the_folder_of_the_other_ide() {
        let home = std::path::Path::new("home");
        #[cfg(windows)]
        assert_eq!(
            super::jetbrains_folder(home),
            home.join("JetBrains"),
            "the application data of the profile"
        );
        #[cfg(target_os = "macos")]
        assert_eq!(
            super::jetbrains_folder(home),
            home.join("Library/Application Support/JetBrains")
        );
        #[cfg(all(unix, not(target_os = "macos")))]
        assert_eq!(
            super::jetbrains_folder(home),
            home.join(".config/JetBrains")
        );
    }

    #[test]
    fn the_theme_name_comes_from_the_look_or_the_colour_scheme() {
        assert_eq!(
            parse_theme_name(
                r#"<application><component name="LafManager"><laf name="Darcula" /></component></application>"#
            )
            .as_deref(),
            Some("Darcula")
        );
        assert_eq!(
            parse_theme_name(
                r#"<application><component name="EditorColorsManagerImpl"><global_color_scheme name="IntelliJ Light" /></component></application>"#
            )
            .as_deref(),
            Some("IntelliJ Light")
        );
    }

    #[test]
    fn the_live_templates_keep_their_variables() {
        let templates = parse_templates(
            r#"<templateSet group="mine">
  <template name="hello" value="System.out.println($WHAT$);$END$" description="prints" toReformat="false" toShortenFQNames="true">
    <variable name="WHAT" expression="" defaultValue="&quot;hi&quot;" alwaysStopAt="true" />
  </template>
</templateSet>"#,
        );
        assert_eq!(
            templates,
            [IdeaTemplate {
                group: "mine".to_owned(),
                abbreviation: "hello".to_owned(),
                description: "prints".to_owned(),
                body: "System.out.println($WHAT$);$END$".to_owned(),
                variables: vec![IdeaVariable {
                    name: "WHAT".to_owned(),
                    expression: String::new(),
                    default: "\"hi\"".to_owned(),
                    stop_at: true,
                }],
            }]
        );
    }

    #[test]
    fn only_the_named_files_are_opened() {
        let root = test_support::scratch("idea-secret");
        fs::create_dir_all(root.join("options")).unwrap();
        fs::write(root.join("options/security.xml"), b"\xff\xfe secret").unwrap();
        fs::write(
            root.join("options/editor-font.xml"),
            r#"<application>
  <component><option name="EDITOR_FONT_SIZE" value="16" /></component>
</application>"#,
        )
        .unwrap();
        let files = read_source(&root).unwrap();
        let names: Vec<&str> = files.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(names, ["options/editor-font.xml"]);

        let prepared = read(
            &root,
            Default::default(),
            &test_support::scratch("idea-secret-out"),
        )
        .unwrap();
        assert!(
            prepared
                .summary
                .iter()
                .all(|line| !line.contains("security")),
            "{:?}",
            prepared.summary
        );
        assert_eq!(prepared.font.unwrap().size, Some(16.));
    }

    #[test]
    fn the_code_style_comes_from_the_default_file() {
        let root = test_support::scratch("idea-code-style");
        fs::create_dir_all(root.join("codestyles")).unwrap();
        fs::write(
            root.join("codestyles/Project.xml"),
            r#"<code_scheme name="Project">
  <codeStyleSettings language="TEXT">
    <option name="INDENT_SIZE" value="2" />
    <option name="TAB_SIZE" value="4" />
    <option name="USE_TAB_CHARACTER" value="true" />
  </codeStyleSettings>
</code_scheme>"#,
        )
        .unwrap();
        let prepared = read(
            &root,
            Default::default(),
            &test_support::scratch("idea-code-style-out"),
        )
        .unwrap();
        assert_eq!(
            prepared.style,
            Some(StyleSettings {
                use_tabs: true,
                indent_size: 2,
                tab_size: 4,
                right_margin: None,
            })
        );
    }
}

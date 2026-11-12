//! The templates of the user over the built-in set. The built-in templates stay in the
//! language and `templates.json` holds only the differences: an edited template by its
//! group and abbreviation, the disabled ones, the new ones and the text of new files.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::template::{ANY, Kind, Template, TemplateLanguage};

/// The file in the settings folder, next to `keymap.json`.
pub const TEMPLATES_FILE: &str = "templates.json";
/// The format of the file this build reads and writes.
pub const TEMPLATES_VERSION: u64 = 1;

/// The key that expands a template.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExpandWith {
    #[default]
    Tab,
    Enter,
    Space,
}

impl ExpandWith {
    pub fn label(self) -> &'static str {
        match self {
            ExpandWith::Tab => "Tab",
            ExpandWith::Enter => "Enter",
            ExpandWith::Space => "Space",
        }
    }

    pub fn next(self) -> Self {
        match self {
            ExpandWith::Tab => ExpandWith::Enter,
            ExpandWith::Enter => ExpandWith::Space,
            ExpandWith::Space => ExpandWith::Tab,
        }
    }
}

/// One variable of a template: its name, the expression that fills it and the text it
/// starts with. A variable with a value and Skip if defined takes no Tab stop.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateField {
    pub name: String,
    pub expression: String,
    pub default: String,
    pub skip: bool,
}

/// What the user changed in one built-in template. Every field is optional: only the
/// changes are written.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LiveTemplateEdit {
    pub abbreviation: Option<String>,
    pub description: Option<String>,
    pub body: Option<String>,
    pub fields: Option<Vec<TemplateField>>,
    pub contexts: Option<Vec<String>>,
    pub expand_with: Option<ExpandWith>,
}

impl LiveTemplateEdit {
    pub fn is_empty(&self) -> bool {
        self.abbreviation.is_none()
            && self.description.is_none()
            && self.body.is_none()
            && self.fields.is_none()
            && self.contexts.is_none()
            && self.expand_with.is_none()
    }
}

/// A template the user added.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct CustomTemplate {
    /// The stable key of the entry in the file, kept while the abbreviation changes.
    pub id: String,
    pub kind: Kind,
    pub group: String,
    pub abbreviation: String,
    pub description: String,
    pub body: String,
    pub fields: Vec<TemplateField>,
    pub contexts: Vec<String>,
    /// The key that expands the template; `None` follows the default of the file.
    pub expand_with: Option<ExpandWith>,
    pub enabled: bool,
}

impl Default for CustomTemplate {
    fn default() -> Self {
        Self {
            id: String::new(),
            kind: Kind::Abbreviation,
            group: ANY.to_owned(),
            abbreviation: String::new(),
            description: String::new(),
            body: String::new(),
            fields: Vec::new(),
            contexts: vec![ANY.to_owned()],
            expand_with: None,
            enabled: true,
        }
    }
}

impl CustomTemplate {
    /// The key of the entry, also for files written before ids existed.
    pub fn id(&self) -> String {
        if self.id.is_empty() {
            id_of(&self.group, &self.abbreviation)
        } else {
            self.id.clone()
        }
    }
}

/// Everything `templates.json` holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateOverrides {
    pub version: u64,
    /// The switch of the Postfix Completion page.
    pub postfix_enabled: bool,
    /// Changed built-in templates, by [`id_of`].
    pub live: BTreeMap<String, LiveTemplateEdit>,
    /// Disabled templates, built-in and added, by [`id_of`].
    pub disabled: BTreeSet<String>,
    pub custom: Vec<CustomTemplate>,
    /// The text of the new-file templates, by their id.
    pub files: BTreeMap<String, String>,
    /// The key that expands a template which follows the default.
    pub expand_with: ExpandWith,
}

impl Default for TemplateOverrides {
    fn default() -> Self {
        Self {
            version: TEMPLATES_VERSION,
            postfix_enabled: true,
            live: BTreeMap::new(),
            disabled: BTreeSet::new(),
            custom: Vec::new(),
            files: BTreeMap::new(),
            expand_with: ExpandWith::Tab,
        }
    }
}

/// The parsed file and what the reader skipped.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LoadedTemplates {
    pub file: TemplateOverrides,
    pub warnings: Vec<String>,
}

/// The group and abbreviation of a template in the file.
pub fn id_of(group: &str, key: &str) -> String {
    format!("{group}/{key}")
}

/// The group of a built-in template: its first context.
pub fn group_of(template: &Template) -> &'static str {
    template.contexts.first().copied().unwrap_or(ANY)
}

/// One template as a page shows it, built-in or added.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TemplateView {
    pub id: String,
    pub group: String,
    pub kind: Kind,
    pub key: String,
    pub description: String,
    pub body: String,
    pub fields: Vec<TemplateField>,
    pub contexts: Vec<String>,
    /// The key the template chose; `None` follows the default of the file.
    pub expand_with: Option<ExpandWith>,
    pub enabled: bool,
    /// The user changed this built-in template.
    pub changed: bool,
    /// The user added this template.
    pub custom: bool,
}

/// A template in force with the parts of the user's file the expander reads.
#[derive(Clone, Debug)]
pub struct ResolvedTemplate {
    pub template: Template,
    /// The key that expands it, with the default of the file already applied.
    pub expand_with: ExpandWith,
    /// The variables in the order Tab visits them, with their expressions.
    pub fields: Vec<TemplateField>,
}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}

impl TemplateView {
    /// The template of the expander. The text of the user lives for the process.
    pub fn to_template(&self) -> Template {
        let fields: Vec<(&'static str, &'static str)> = self
            .fields
            .iter()
            .map(|field| (leak(field.name.clone()), leak(field.default.clone())))
            .collect();
        let contexts: Vec<&'static str> = self
            .contexts
            .iter()
            .map(|name| leak(name.clone()))
            .collect();
        Template {
            key: leak(self.key.clone()),
            description: leak(self.description.clone()),
            kind: self.kind,
            body: leak(self.body.clone()),
            fields: Box::leak(fields.into_boxed_slice()),
            contexts: Box::leak(contexts.into_boxed_slice()),
        }
    }

    /// The variables of the body that the table does not list yet.
    pub fn missing_fields(&self) -> Vec<TemplateField> {
        let mut found = Vec::new();
        for name in crate::expand::field_names(&self.body) {
            let special = name == "END" || name == "SELECTION" || name == "EXPR";
            if special || self.fields.iter().any(|field| field.name == name) {
                continue;
            }
            if !found.iter().any(|field: &TemplateField| field.name == name) {
                found.push(TemplateField {
                    name,
                    ..TemplateField::default()
                });
            }
        }
        found
    }
}

impl TemplateOverrides {
    /// Reads the file. A missing file is the empty set; a broken one is an error, so that
    /// the caller keeps the built-ins and leaves the file alone.
    pub fn load(path: &Path) -> Result<LoadedTemplates, String> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(LoadedTemplates::default());
            }
            Err(error) => return Err(format!("{}: {error}", path.display())),
        };
        Self::parse(&text)
    }

    /// Parses the text of a templates file.
    pub fn parse(text: &str) -> Result<LoadedTemplates, String> {
        let file: TemplateOverrides =
            serde_json::from_str(text).map_err(|error| error.to_string())?;
        if file.version != TEMPLATES_VERSION {
            return Err(format!(
                "version {} is not the version {TEMPLATES_VERSION} of this build",
                file.version
            ));
        }
        Ok(LoadedTemplates {
            file,
            warnings: Vec::new(),
        })
    }

    /// Writes the file, creating its folder. The text goes to a temporary file first and
    /// replaces the old one in one step, so a reader never sees half of it. Blocks: not
    /// for the UI thread.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let temporary = path.with_extension("json.tmp");
        std::fs::write(&temporary, self.to_text())?;
        std::fs::rename(&temporary, path)
    }

    /// The JSON of the file, as it is written.
    pub fn to_text(&self) -> String {
        serde_json::to_string_pretty(self).expect("a templates file is JSON")
    }

    /// Whether the file holds nothing of the user.
    pub fn is_empty(&self) -> bool {
        self.live.is_empty()
            && self.disabled.is_empty()
            && self.custom.is_empty()
            && self.files.is_empty()
            && self.expand_with == ExpandWith::default()
    }

    /// Whether the template is switched off.
    pub fn is_disabled(&self, id: &str) -> bool {
        self.disabled.contains(id)
    }

    /// Switches a template on or off.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) {
        if enabled {
            self.disabled.remove(id);
        } else {
            self.disabled.insert(id.to_owned());
        }
    }

    /// The changes of one built-in template, added when it has none yet.
    pub fn edit(&mut self, id: &str) -> &mut LiveTemplateEdit {
        self.live.entry(id.to_owned()).or_default()
    }

    /// Forgets every change of one template: the built-in comes back as it was.
    pub fn reset(&mut self, id: &str) {
        self.live.remove(id);
        self.disabled.remove(id);
    }

    /// The text of a new-file template, when the user changed it.
    pub fn file_text(&self, id: &str) -> Option<&str> {
        self.files.get(id).map(String::as_str)
    }

    /// The effective text of a new-file template.
    pub fn file_or_default(&self, id: &str, default: &str) -> String {
        self.file_text(id).unwrap_or(default).to_owned()
    }

    pub fn set_file_text(&mut self, id: &str, text: &str) {
        self.files.insert(id.to_owned(), text.to_owned());
    }

    pub fn reset_file(&mut self, id: &str) {
        self.files.remove(id);
    }

    /// Every template of the language with the changes of this file on top, disabled
    /// ones included, in the order the page shows them.
    pub fn describe(&self, language: &TemplateLanguage) -> Vec<TemplateView> {
        let mut shown = Vec::new();
        for base in language.templates {
            let group = group_of(base);
            let id = id_of(group, base.key);
            let edit = self.live.get(&id);
            let changed = edit.is_some_and(|edit| !edit.is_empty());
            let key = edit
                .and_then(|edit| edit.abbreviation.clone())
                .unwrap_or_else(|| base.key.to_owned());
            let description = edit
                .and_then(|edit| edit.description.clone())
                .unwrap_or_else(|| base.description.to_owned());
            let body = edit
                .and_then(|edit| edit.body.clone())
                .unwrap_or_else(|| base.body.to_owned());
            let fields = match edit.and_then(|edit| edit.fields.clone()) {
                Some(fields) => fields,
                None => base
                    .fields
                    .iter()
                    .map(|(name, default)| TemplateField {
                        name: (*name).to_owned(),
                        default: (*default).to_owned(),
                        ..TemplateField::default()
                    })
                    .collect(),
            };
            let contexts = match edit.and_then(|edit| edit.contexts.clone()) {
                Some(contexts) => contexts,
                None => base
                    .contexts
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect(),
            };
            shown.push(TemplateView {
                enabled: !self.disabled.contains(&id),
                id,
                group: group.to_owned(),
                kind: base.kind,
                key,
                description,
                body,
                fields,
                contexts,
                expand_with: edit.and_then(|edit| edit.expand_with),
                changed,
                custom: false,
            });
        }
        for custom in &self.custom {
            let id = custom.id();
            shown.push(TemplateView {
                enabled: custom.enabled && !self.disabled.contains(&id),
                id,
                group: custom.group.clone(),
                kind: custom.kind,
                key: custom.abbreviation.clone(),
                description: custom.description.clone(),
                body: custom.body.clone(),
                fields: custom.fields.clone(),
                contexts: custom.contexts.clone(),
                expand_with: custom.expand_with,
                changed: true,
                custom: true,
            });
        }
        shown
    }
}

impl TemplateLanguage {
    /// The built-in templates with the changes of the user on top, in the order they are
    /// declared, then the added ones. The postfix templates leave when the switch is off.
    pub fn resolve(&self, overrides: &TemplateOverrides) -> Vec<Template> {
        let described = overrides.describe(self);
        let mut templates = Vec::with_capacity(described.len());
        for view in described {
            if !view.enabled {
                continue;
            }
            if view.kind == Kind::Postfix && !overrides.postfix_enabled {
                continue;
            }
            templates.push(view.to_template());
        }
        templates
    }

    /// The templates in force with what the expander reads from the file of the user:
    /// the key of each one and the expressions of its variables.
    pub fn resolved(&self, overrides: &TemplateOverrides) -> Vec<ResolvedTemplate> {
        let described = overrides.describe(self);
        let mut resolved = Vec::with_capacity(described.len());
        for view in described {
            if !view.enabled {
                continue;
            }
            if view.kind == Kind::Postfix && !overrides.postfix_enabled {
                continue;
            }
            let expand_with = view.expand_with.unwrap_or(overrides.expand_with);
            resolved.push(ResolvedTemplate {
                template: view.to_template(),
                expand_with,
                fields: view.fields.clone(),
            });
        }
        resolved
    }

    /// The context names the query captures, for the Applicable in list.
    pub fn context_names(&self) -> Vec<String> {
        let Some(query) = self.query() else {
            return vec![ANY.to_owned()];
        };
        let mut names: Vec<String> = query
            .capture_names()
            .iter()
            .map(|name| (*name).to_owned())
            .filter(|name| !name.is_empty())
            .collect();
        names.sort();
        names.dedup();
        names
    }
}

#[cfg(test)]
mod tests {
    use std::sync::OnceLock;

    use super::*;

    fn language() -> &'static TemplateLanguage {
        static LANGUAGE: OnceLock<TemplateLanguage> = OnceLock::new();
        LANGUAGE.get_or_init(|| TemplateLanguage {
            templates: &[
                Template {
                    key: "sout",
                    description: "Prints",
                    kind: Kind::Abbreviation,
                    body: "System.out.println($END$);",
                    fields: &[],
                    contexts: &["statement"],
                },
                Template {
                    key: "psvm",
                    description: "main",
                    kind: Kind::Abbreviation,
                    body: "void main() {\n\t$END$\n}",
                    fields: &[],
                    contexts: &["declaration"],
                },
                Template {
                    key: "nn",
                    description: "not null",
                    kind: Kind::Postfix,
                    body: "if ($EXPR$ != null) {\n\t$END$\n}",
                    fields: &[],
                    contexts: &["statement"],
                },
            ],
            contexts: "",
            grammar: || panic!("the merge tests do not parse"),
            indent_unit: "    ",
            receiver_prefixes: &[],
            query: OnceLock::new(),
            declarations: None,
        })
    }

    fn find<'a>(templates: &'a [Template], key: &str) -> Option<&'a Template> {
        templates.iter().find(|template| template.key == key)
    }

    #[test]
    fn the_user_changes_are_merged_over_the_built_ins() {
        let mut overrides = TemplateOverrides::default();
        overrides.edit("statement/sout").body = Some("log($END$);".to_owned());
        overrides.set_enabled("declaration/psvm", false);
        overrides.custom.push(CustomTemplate {
            group: "statement".to_owned(),
            abbreviation: "mine".to_owned(),
            description: "My own".to_owned(),
            body: "mine($END$);".to_owned(),
            ..CustomTemplate::default()
        });

        let resolved = language().resolve(&overrides);
        assert_eq!(find(&resolved, "sout").unwrap().body, "log($END$);");
        assert!(find(&resolved, "psvm").is_none(), "a disabled one is gone");
        assert_eq!(find(&resolved, "mine").unwrap().body, "mine($END$);");
    }

    #[test]
    fn the_postfix_switch_takes_every_postfix_template_away() {
        let mut overrides = TemplateOverrides::default();
        assert!(find(&language().resolve(&overrides), "nn").is_some());
        overrides.postfix_enabled = false;
        assert!(find(&language().resolve(&overrides), "nn").is_none());
        assert!(find(&language().resolve(&overrides), "sout").is_some());
    }

    #[test]
    fn a_disabled_postfix_template_is_not_offered() {
        let mut overrides = TemplateOverrides::default();
        overrides.set_enabled("statement/nn", false);
        let resolved = language().resolve(&overrides);
        assert!(find(&resolved, "nn").is_none());
    }

    #[test]
    fn the_file_round_trips_and_only_the_changes_are_written() {
        let mut overrides = TemplateOverrides::default();
        overrides.edit("statement/sout").description = Some("Logs".to_owned());
        overrides.set_enabled("declaration/psvm", false);
        overrides.set_file_text("class", "class ${NAME} {}");
        let text = overrides.to_text();
        assert!(text.contains("statement/sout"));
        assert!(text.contains("class"));

        let parsed = TemplateOverrides::parse(&text).unwrap().file;
        assert_eq!(parsed, overrides);
    }

    #[test]
    fn a_broken_file_is_an_error_and_keeps_the_built_ins() {
        let broken = TemplateOverrides::parse("{ not json").unwrap_err();
        assert!(!broken.is_empty());
        let newer = TemplateOverrides::parse(r#"{"version": 9}"#).unwrap_err();
        assert!(newer.contains("version"));
        assert!(
            TemplateOverrides::load(Path::new("Z:/no/such/templates.json"))
                .unwrap()
                .file
                .is_empty()
        );
    }

    #[test]
    fn a_changed_and_added_template_keeps_the_variables_of_the_table() {
        let mut overrides = TemplateOverrides::default();
        let edit = overrides.edit("statement/sout");
        edit.body = Some("log($NAME$);".to_owned());
        edit.fields = Some(vec![TemplateField {
            name: "NAME".to_owned(),
            default: "value".to_owned(),
            ..TemplateField::default()
        }]);
        let views = overrides.describe(language());
        let view = views
            .iter()
            .find(|view| view.id == "statement/sout")
            .unwrap();
        assert!(view.changed);
        assert_eq!(view.fields[0].default, "value");
    }
}

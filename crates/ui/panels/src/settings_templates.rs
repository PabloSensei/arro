//! Settings -> Editor -> Live Templates, Postfix Completion and File and Code
//! Templates. The pages edit `templates.json` in the settings folder; every change takes
//! effect in the open editors at once and goes to the file in one atomic step.

use std::sync::Arc;

use editor_view::{TemplateLanguage, TemplateSettings};
use templates::{
    CustomTemplate, EXPRESSIONS, ExpandWith, FILE_TEMPLATE_VARIABLES, FileTemplateDefinition, Kind,
    LiveTemplateEdit, TEMPLATES_FILE, TemplateField, TemplateOverrides, TemplateView,
    expression_warning, id_of,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::TextField;
use crate::settings::SettingsView;

/// The built-in templates of the language and the new-file templates of the application,
/// before `templates.json` is read.
#[derive(Clone, Default)]
pub struct TemplatesSetup {
    pub language: Option<&'static TemplateLanguage>,
    pub files: Vec<FileTemplateDefinition>,
}

impl TemplatesSetup {
    /// The settings of a window where no file of the user was read yet.
    pub fn initial(self) -> TemplateSettings {
        TemplateSettings {
            language: self.language,
            overrides: Arc::new(TemplateOverrides::default()),
            files: self.files,
            generation: 0,
        }
    }
}

/// Reads `templates.json` and puts its changes over the built-ins. A broken file is
/// reported and left alone: the built-in set stays. Blocks in the background only.
pub(crate) fn load(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(current) = cx.try_global::<TemplateSettings>().cloned() else {
        return;
    };
    let before = current.generation;
    let path = workspace.settings_dir().join(TEMPLATES_FILE);
    let read = cx
        .background_executor()
        .spawn(async move { TemplateOverrides::load(&path) });
    let view = cx.weak_entity();
    cx.spawn(async move |_, cx| {
        let loaded = read.await;
        view.update(cx, |workspace, cx| {
            let mut settings = cx
                .try_global::<TemplateSettings>()
                .cloned()
                .unwrap_or(current);
            // A newer change of the user is already in the window: the text on disk
            // is older and must not undo the change.
            if settings.generation != before {
                return;
            }
            match loaded {
                Ok(loaded) => {
                    for warning in &loaded.warnings {
                        eprintln!("templates: {warning}");
                    }
                    apply(&mut settings, loaded.file);
                }
                Err(error) => {
                    workspace.notify_user(
                        "Templates",
                        format!("{TEMPLATES_FILE} is not read: {error}"),
                        cx,
                    );
                }
            }
            cx.set_global(settings);
            cx.notify();
        })
        .ok();
    })
    .detach();
}

fn apply(settings: &mut TemplateSettings, overrides: TemplateOverrides) {
    settings.overrides = Arc::new(overrides);
    let overrides = settings.overrides.clone();
    for file in &mut settings.files {
        file.apply(&overrides);
    }
    settings.generation += 1;
}

/// The definition of the new-file template with this label, when the application gave one.
pub(crate) fn file_template(cx: &App, label: &str) -> Option<FileTemplateDefinition> {
    cx.try_global::<TemplateSettings>()?
        .files
        .iter()
        .find(|file| file.label == label)
        .cloned()
}

/// The part of a page the window shows.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum TemplateSection {
    Live,
    Postfix,
    Files,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Abbreviation,
    Description,
    Body,
    Variable(usize, VariablePart),
    FileText,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum VariablePart {
    Name,
    Expression,
    Default,
}

#[derive(Clone, Default)]
struct VariableRow {
    name: TextField,
    expression: TextField,
    default: TextField,
    skip: bool,
}

pub(crate) struct TemplatesPage {
    workspace: WeakEntity<Workspace>,
    selected_live: Option<String>,
    selected_file: Option<String>,
    field: Option<Field>,
    abbreviation: TextField,
    description: TextField,
    body: TextField,
    variables: Vec<VariableRow>,
    contexts: Vec<String>,
    expand_with: Option<ExpandWith>,
    file_text: TextField,
    message: Option<String>,
}

impl TemplatesPage {
    fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self {
            workspace,
            selected_live: None,
            selected_file: None,
            field: None,
            abbreviation: TextField::default(),
            description: TextField::default(),
            body: TextField::default(),
            variables: Vec::new(),
            contexts: Vec::new(),
            expand_with: None,
            file_text: TextField::default(),
            message: None,
        }
    }

    fn views(&self, cx: &App) -> Vec<TemplateView> {
        let Some(settings) = cx.try_global::<TemplateSettings>() else {
            return Vec::new();
        };
        let Some(language) = settings.language else {
            return Vec::new();
        };
        settings.overrides.describe(language)
    }

    fn files(&self, cx: &App) -> Vec<FileTemplateDefinition> {
        cx.try_global::<TemplateSettings>()
            .map_or_else(Vec::new, |settings| settings.files.clone())
    }

    fn selected_view(&self, cx: &App) -> Option<TemplateView> {
        let id = self.selected_live.as_deref()?;
        self.views(cx).into_iter().find(|view| view.id == id)
    }

    fn select_live(&mut self, id: String, cx: &mut Context<SettingsView>) {
        let Some(view) = self.views(cx).into_iter().find(|view| view.id == id) else {
            return;
        };
        self.selected_live = Some(id);
        self.selected_file = None;
        self.field = None;
        self.abbreviation = TextField::from_text(view.key.clone());
        self.description = TextField::from_text(view.description.clone());
        self.body = TextField::from_text(view.body.clone());
        self.variables = view
            .fields
            .iter()
            .map(|field| VariableRow {
                name: TextField::from_text(field.name.clone()),
                expression: TextField::from_text(field.expression.clone()),
                default: TextField::from_text(field.default.clone()),
                skip: field.skip,
            })
            .collect();
        self.sync_variables();
        self.contexts = view.contexts.clone();
        self.expand_with = view.expand_with;
        self.message = None;
        cx.notify();
    }

    fn select_file(&mut self, id: &str, cx: &mut Context<SettingsView>) {
        let Some(file) = self.files(cx).into_iter().find(|file| file.id == id) else {
            return;
        };
        self.selected_file = Some(id.to_owned());
        self.selected_live = None;
        self.field = None;
        self.file_text = TextField::from_text(file.text);
        self.message = None;
        cx.notify();
    }

    /// The variables of the body that the table does not list yet, in the order of the
    /// body.
    fn sync_variables(&mut self) {
        for name in body_fields(&self.body.text) {
            if self.variables.iter().any(|row| row.name.text == name) {
                continue;
            }
            self.variables.push(VariableRow {
                name: TextField::from_text(name),
                ..VariableRow::default()
            });
        }
    }

    fn fields_of(&self) -> Vec<TemplateField> {
        self.variables
            .iter()
            .filter(|row| !row.name.text.is_empty())
            .map(|row| TemplateField {
                name: row.name.text.clone(),
                expression: row.expression.text.clone(),
                default: row.default.text.clone(),
                skip: row.skip,
            })
            .collect()
    }

    fn save(&mut self, cx: &mut Context<SettingsView>) {
        if let Some(id) = self.selected_file.clone() {
            self.save_file(&id, cx);
            return;
        }
        let Some(id) = self.selected_live.clone() else {
            return;
        };
        let Some(settings) = cx.try_global::<TemplateSettings>().cloned() else {
            return;
        };
        let Some(language) = settings.language else {
            return;
        };
        let Some(view) = settings
            .overrides
            .describe(language)
            .into_iter()
            .find(|view| view.id == id)
        else {
            return;
        };
        let mut overrides = (*settings.overrides).clone();
        if view.custom {
            let Some(custom) = overrides.custom.iter_mut().find(|custom| custom.id() == id) else {
                return;
            };
            custom.abbreviation = self.abbreviation.text.clone();
            custom.description = self.description.text.clone();
            custom.body = self.body.text.clone();
            custom.fields = self.fields_of();
            custom.contexts = self.contexts.clone();
            custom.expand_with = self.expand_with;
            custom.enabled = true;
        } else {
            let base = language
                .templates
                .iter()
                .find(|base| id_of(first_context(base.contexts), base.key) == id);
            let Some(base) = base else {
                return;
            };
            let fields = self.fields_of();
            let base_fields: Vec<TemplateField> = base
                .fields
                .iter()
                .map(|(name, default)| TemplateField {
                    name: (*name).to_owned(),
                    default: (*default).to_owned(),
                    ..TemplateField::default()
                })
                .collect();
            let mut edit = LiveTemplateEdit {
                abbreviation: (self.abbreviation.text != base.key)
                    .then(|| self.abbreviation.text.clone()),
                description: (self.description.text != base.description)
                    .then(|| self.description.text.clone()),
                body: (self.body.text != base.body).then(|| self.body.text.clone()),
                fields: (fields != base_fields).then_some(fields),
                contexts: (self.contexts != base.contexts).then(|| self.contexts.clone()),
                expand_with: self.expand_with,
            };
            if edit.is_empty() {
                overrides.live.remove(&id);
            } else {
                edit.abbreviation.get_or_insert_with(|| base.key.to_owned());
                overrides.live.insert(id.clone(), edit);
            }
        }
        self.store(overrides, cx);
    }

    fn save_file(&mut self, id: &str, cx: &mut Context<SettingsView>) {
        let Some(settings) = cx.try_global::<TemplateSettings>().cloned() else {
            return;
        };
        let mut overrides = (*settings.overrides).clone();
        let default = settings
            .files
            .iter()
            .find(|file| file.id == id)
            .map_or("", |file| file.default_text.as_str());
        if self.file_text.text == default {
            overrides.reset_file(id);
        } else {
            overrides.set_file_text(id, &self.file_text.text);
        }
        self.store(overrides, cx);
    }

    /// Puts the changes in the window at once and writes the file in one atomic step.
    fn store(&mut self, overrides: TemplateOverrides, cx: &mut Context<SettingsView>) {
        let mut settings = cx
            .try_global::<TemplateSettings>()
            .cloned()
            .unwrap_or_default();
        settings.overrides = Arc::new(overrides.clone());
        let shared = settings.overrides.clone();
        for file in &mut settings.files {
            file.apply(&shared);
        }
        if let Some(workspace) = self.workspace.upgrade() {
            let path = workspace.read(cx).settings_dir().join(TEMPLATES_FILE);
            if let Err(error) = overrides.save(&path) {
                eprintln!("templates are not saved: {error}");
            }
        }
        settings.generation += 1;
        cx.set_global(settings);
        cx.notify();
    }

    /// Switches one template on or off. Postfix templates also have the switch of the
    /// whole page.
    fn toggle(&mut self, id: &str, cx: &mut Context<SettingsView>) {
        let Some(settings) = cx.try_global::<TemplateSettings>().cloned() else {
            return;
        };
        let mut overrides = (*settings.overrides).clone();
        let view = overrides
            .describe(settings.language.unwrap_or_else(empty_language))
            .into_iter()
            .find(|view| view.id == id);
        let enabled = view.as_ref().is_none_or(|view| view.enabled);
        if view.is_some_and(|view| view.custom) {
            if let Some(custom) = overrides.custom.iter_mut().find(|custom| custom.id() == id) {
                custom.enabled = !enabled;
            }
            overrides.set_enabled(id, true);
        } else {
            overrides.set_enabled(id, !enabled);
        }
        self.store(overrides, cx);
    }

    fn toggle_postfix(&mut self, cx: &mut Context<SettingsView>) {
        let Some(settings) = cx.try_global::<TemplateSettings>().cloned() else {
            return;
        };
        let mut overrides = (*settings.overrides).clone();
        overrides.postfix_enabled = !overrides.postfix_enabled;
        self.store(overrides, cx);
    }

    fn add(&mut self, group: Option<String>, cx: &mut Context<SettingsView>) {
        let Some(settings) = cx.try_global::<TemplateSettings>().cloned() else {
            return;
        };
        let group = group.unwrap_or_else(|| {
            settings.language.map_or_else(
                || "any".to_owned(),
                |language| {
                    language
                        .context_names()
                        .first()
                        .cloned()
                        .unwrap_or_else(|| "any".to_owned())
                },
            )
        });
        let mut overrides = (*settings.overrides).clone();
        let abbreviation =
            unique_abbreviation(&overrides, settings.language, &group, "newTemplate");
        overrides.custom.push(CustomTemplate {
            id: id_of(&group, &abbreviation),
            group: group.clone(),
            abbreviation: abbreviation.clone(),
            description: "My template".to_owned(),
            body: "$END$".to_owned(),
            ..CustomTemplate::default()
        });
        self.store(overrides, cx);
        self.select_live(id_of(&group, &abbreviation), cx);
    }

    fn duplicate(&mut self, cx: &mut Context<SettingsView>) {
        let Some(view) = self.selected_view(cx) else {
            return;
        };
        let Some(settings) = cx.try_global::<TemplateSettings>().cloned() else {
            return;
        };
        let mut overrides = (*settings.overrides).clone();
        let abbreviation = unique_abbreviation(
            &overrides,
            settings.language,
            &view.group,
            &format!("{}Copy", view.key),
        );
        overrides.custom.push(CustomTemplate {
            id: id_of(&view.group, &abbreviation),
            kind: view.kind,
            group: view.group.clone(),
            abbreviation: abbreviation.clone(),
            description: view.description.clone(),
            body: view.body.clone(),
            fields: view.fields.clone(),
            contexts: view.contexts.clone(),
            expand_with: view.expand_with,
            enabled: true,
        });
        self.store(overrides, cx);
        self.select_live(id_of(&view.group, &abbreviation), cx);
    }

    fn remove(&mut self, cx: &mut Context<SettingsView>) {
        let Some(view) = self.selected_view(cx) else {
            return;
        };
        let Some(settings) = cx.try_global::<TemplateSettings>().cloned() else {
            return;
        };
        let mut overrides = (*settings.overrides).clone();
        if view.custom {
            overrides.custom.retain(|custom| custom.id() != view.id);
            overrides.disabled.remove(&view.id);
            self.selected_live = None;
        } else {
            overrides.set_enabled(&view.id, false);
        }
        self.store(overrides, cx);
        cx.notify();
    }

    fn restore(&mut self, cx: &mut Context<SettingsView>) {
        let Some(id) = self.selected_live.clone() else {
            return;
        };
        let Some(settings) = cx.try_global::<TemplateSettings>().cloned() else {
            return;
        };
        let mut overrides = (*settings.overrides).clone();
        overrides.reset(&id);
        self.store(overrides, cx);
        self.select_live(id, cx);
    }

    pub(crate) fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<SettingsView>) -> bool {
        let Some(field) = self.field else {
            return false;
        };
        if event.keystroke.key == "escape" {
            self.field = None;
            cx.notify();
            return true;
        }
        let multiline = field == Field::Body || field == Field::FileText;
        let taken = match field {
            Field::Abbreviation => self.abbreviation.key_with_clipboard(event, false, cx),
            Field::Description => self.description.key_with_clipboard(event, false, cx),
            Field::Body => self.body.key_with_clipboard(event, true, cx),
            Field::Variable(index, part) => {
                let Some(row) = self.variables.get_mut(index) else {
                    return false;
                };
                match part {
                    VariablePart::Name => row.name.key_with_clipboard(event, false, cx),
                    VariablePart::Expression => row.expression.key_with_clipboard(event, false, cx),
                    VariablePart::Default => row.default.key_with_clipboard(event, false, cx),
                }
            }
            Field::FileText => self.file_text.key_with_clipboard(event, true, cx),
        };
        if taken {
            if field == Field::Body {
                self.sync_variables();
            }
            self.save(cx);
        }
        taken || multiline
    }

    pub(crate) fn render(
        &mut self,
        section: TemplateSection,
        _window: &mut Window,
        cx: &mut Context<SettingsView>,
    ) -> Div {
        let theme = *cx.global::<Theme>();
        match section {
            TemplateSection::Live => self.render_live(&theme, cx),
            TemplateSection::Postfix => self.render_postfix(&theme, cx),
            TemplateSection::Files => self.render_files(&theme, cx),
        }
    }

    fn render_live(&mut self, theme: &Theme, cx: &mut Context<SettingsView>) -> Div {
        let views = self.views(cx);
        let live: Vec<TemplateView> = views
            .into_iter()
            .filter(|view| view.kind != Kind::Postfix)
            .collect();
        let groups = group_names(&live);
        let selected = self.selected_live.clone();

        let mut page = div().flex().flex_col().flex_none().gap(px(SPACE_XS));
        page = page.child(self.toolbar(theme, cx));
        page = page.child(self.default_expand_row(theme, cx));
        if let Some(id) = selected.as_deref()
            && live.iter().any(|view| view.id == id)
        {
            page = page.child(self.editor(id, theme, cx));
        }
        page = page.child(self.list(&live, &groups, selected.as_deref(), theme, cx));
        if selected.is_none() {
            page = page.child(
                div()
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("Choose a template or add one."),
            );
        }
        if let Some(message) = &self.message {
            page = page.child(
                div()
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child(message.clone()),
            );
        }
        page
    }

    fn toolbar(&self, theme: &Theme, cx: &mut Context<SettingsView>) -> Div {
        let selected = self.selected_view(cx);
        let builtin_changed = selected
            .as_ref()
            .is_some_and(|view| view.changed && !view.custom);
        div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .py(px(SPACE_S))
            .child(
                icon_button("template-add", "general/add", "Add Template", None, theme)
                    .debug_selector(|| "template-add".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| {
                        let group = this.templates_page.selected_view(cx).map(|view| view.group);
                        this.templates_page.add(group, cx);
                    })),
            )
            .child(
                icon_button(
                    "template-remove",
                    "general/minus",
                    "Remove Template",
                    None,
                    theme,
                )
                .debug_selector(|| "template-remove".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.templates_page.remove(cx);
                })),
            )
            .child(
                icon_button(
                    "template-duplicate",
                    "general/copy",
                    "Duplicate Template",
                    None,
                    theme,
                )
                .debug_selector(|| "template-duplicate".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.templates_page.duplicate(cx);
                })),
            )
            .child(
                icon_button(
                    "template-restore",
                    "general/refresh",
                    "Restore Defaults",
                    None,
                    theme,
                )
                .debug_selector(|| "template-restore".to_owned())
                .when(!builtin_changed, |this| this.opacity(0.4))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if builtin_changed {
                        this.templates_page.restore(cx);
                    }
                })),
            )
    }

    /// The key that expands a template which follows the default.
    fn default_expand_row(&self, theme: &Theme, cx: &mut Context<SettingsView>) -> Div {
        let current = cx
            .try_global::<TemplateSettings>()
            .map_or(ExpandWith::Tab, |settings| settings.overrides.expand_with);
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .py(px(SPACE_S))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child("By default expand with"),
            );
        for key in [ExpandWith::Tab, ExpandWith::Enter, ExpandWith::Space] {
            let on = current == key;
            let selector = key.label().to_lowercase();
            let debug = selector.clone();
            row = row.child(
                div()
                    .id(SharedString::from(format!(
                        "template-default-expand-{selector}"
                    )))
                    .debug_selector(move || format!("template-default-expand-{debug}"))
                    .flex()
                    .items_center()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(if on { theme.accent } else { theme.border })
                    .when(on, |this| this.bg(theme.selection))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.templates_page.set_default_expand_with(key, cx);
                    }))
                    .child(key.label()),
            );
        }
        row
    }

    /// The key that expands this template, or the default of the page.
    fn expand_with_row(&self, theme: &Theme, cx: &mut Context<SettingsView>) -> Div {
        let choices: [(Option<ExpandWith>, &'static str, &'static str); 4] = [
            (None, "default", "Default"),
            (Some(ExpandWith::Tab), "tab", "Tab"),
            (Some(ExpandWith::Enter), "enter", "Enter"),
            (Some(ExpandWith::Space), "space", "Space"),
        ];
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .child(
                div()
                    .w(px(120.))
                    .text_color(theme.text_secondary)
                    .child("Expand with"),
            );
        for (choice, selector, label) in choices {
            let on = self.expand_with == choice;
            row = row.child(
                div()
                    .id(SharedString::from(format!("template-expand-{selector}")))
                    .debug_selector(move || format!("settings-template-expand-{selector}"))
                    .flex()
                    .items_center()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(if on { theme.accent } else { theme.border })
                    .when(on, |this| this.bg(theme.selection))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.templates_page.expand_with = choice;
                        this.templates_page.save(cx);
                        cx.notify();
                    }))
                    .child(label),
            );
        }
        row
    }

    /// The default key of the page, written to the file.
    fn set_default_expand_with(&mut self, key: ExpandWith, cx: &mut Context<SettingsView>) {
        let Some(settings) = cx.try_global::<TemplateSettings>().cloned() else {
            return;
        };
        let mut overrides = (*settings.overrides).clone();
        if overrides.expand_with == key {
            return;
        }
        overrides.expand_with = key;
        self.store(overrides, cx);
    }

    fn list(
        &self,
        views: &[TemplateView],
        groups: &[String],
        selected: Option<&str>,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> Div {
        let mut list = div().flex().flex_col().flex_none().gap(px(SPACE_XS));
        if views.is_empty() {
            return list.child(
                div()
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("The language offers no live templates."),
            );
        }
        for group in groups {
            let heading = group.replace('.', " ");
            list = list.child(
                div()
                    .id(SharedString::from(format!("template-group-{heading}")))
                    .debug_selector(move || format!("settings-template-group-{heading}"))
                    .pt(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .font_weight(FontWeight::MEDIUM)
                    .child(group.clone()),
            );
            for view in views.iter().filter(|view| view.group == *group) {
                let id = view.id.clone();
                let selector = selector_of(&id);
                let enabled = view.enabled;
                let chosen = selected == Some(id.as_str());
                let row_selector = selector.clone();
                let row = div()
                    .id(SharedString::from(format!("template-row-{selector}")))
                    .debug_selector(move || format!("settings-template-{row_selector}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .gap(px(SPACE_S))
                    .min_h(px(24.))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(chosen, |this| this.bg(theme.selection))
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.templates_page.select_live(id.clone(), cx)
                    }))
                    .child(
                        div()
                            .id(SharedString::from(format!("template-toggle-{selector}")))
                            .debug_selector({
                                let selector = selector.clone();
                                move || format!("settings-template-enabled-{selector}")
                            })
                            .occlude()
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |this, _, _, cx| this.templates_page.toggle(&id, cx)
                            }))
                            .child(checkbox(enabled, theme)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .whitespace_nowrap()
                            .overflow_hidden()
                            .child(view.key.clone()),
                    )
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .whitespace_nowrap()
                            .child(view.description.clone()),
                    );
                list = list.child(row);
            }
        }
        list
    }

    fn editor(&mut self, id: &str, theme: &Theme, cx: &mut Context<SettingsView>) -> AnyElement {
        let Some(view) = self.views(cx).into_iter().find(|view| view.id == id) else {
            return div().into_any_element();
        };
        let mut form = div()
            .id("template-editor")
            .debug_selector(|| "template-editor".to_owned())
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_S))
            .pt(px(SPACE_M))
            .border_t_1()
            .border_color(theme.border);
        form = form.child(self.labeled(
            "Abbreviation",
            "template-abbreviation",
            Field::Abbreviation,
            &self.abbreviation.clone(),
            false,
            theme,
            cx,
        ));
        form = form.child(self.labeled(
            "Description",
            "template-description",
            Field::Description,
            &self.description.clone(),
            false,
            theme,
            cx,
        ));
        form = form.child(self.labeled(
            "Template text",
            "template-text",
            Field::Body,
            &self.body.clone(),
            true,
            theme,
            cx,
        ));
        form = form.child(self.variables_table(theme, cx));
        form = form.child(self.expand_with_row(theme, cx));
        form = form.child(self.contexts_row(&view, theme, cx));
        form.into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn labeled(
        &self,
        label: &'static str,
        id: &'static str,
        field: Field,
        text: &TextField,
        multiline: bool,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> Div {
        let active = self.field == Some(field);
        let shown = text.text.clone();
        let selector = id.to_owned();
        let mut boxed = div()
            .id(SharedString::from(format!("template-{id}-box")))
            .debug_selector(move || selector.clone())
            .flex()
            .items_start()
            .flex_1()
            .min_h(if multiline {
                px(96.)
            } else {
                px(CONTROL_HEIGHT)
            })
            .px(px(SPACE_M))
            .py(px(SPACE_XS))
            .rounded(px(CONTROL_RADIUS))
            .border_2()
            .border_color(if active { theme.accent } else { theme.border })
            .overflow_hidden()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.templates_page.field = Some(field);
                cx.notify();
            }));
        if multiline {
            boxed = boxed.flex_col().font_family(SharedString::from(MONO_FONT));
        } else {
            boxed = boxed.items_center();
        }
        if active {
            boxed = boxed.children(text_field_children(
                text,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                18.,
                true,
            ));
        } else if shown.is_empty() {
            boxed = boxed.child(div().text_color(theme.text_secondary).child("Empty"));
        } else {
            boxed = boxed.child(div().child(shown));
        }
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .child(
                div()
                    .w(px(120.))
                    .text_color(theme.text_secondary)
                    .child(label),
            )
            .child(boxed)
    }

    fn variables_table(&self, theme: &Theme, cx: &mut Context<SettingsView>) -> Div {
        let names: Vec<String> = self
            .variables
            .iter()
            .map(|row| row.name.text.clone())
            .filter(|name| !name.is_empty())
            .collect();
        let mut table = div().flex().flex_col().flex_none().gap(px(SPACE_XS)).child(
            div()
                .flex()
                .gap(px(SPACE_S))
                .text_color(theme.text_secondary)
                .child(div().w(px(120.)).child("Variable"))
                .child(div().flex_1().child("Expression"))
                .child(div().flex_1().child("Default value"))
                .child(div().w(px(120.)).child("Skip if defined")),
        );
        for (index, row) in self.variables.iter().enumerate() {
            let selector = row.name.text.to_lowercase();
            let warning = expression_warning(&row.expression.text, &names);
            let active_expression =
                self.field == Some(Field::Variable(index, VariablePart::Expression));
            let skip_selector = selector.clone();
            let skip = div()
                .id(SharedString::from(format!(
                    "template-variable-{selector}-skip"
                )))
                .debug_selector(move || format!("template-variable-{skip_selector}-skip"))
                .flex()
                .items_center()
                .w(px(120.))
                .h(px(CONTROL_HEIGHT))
                .occlude()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(row) = this.templates_page.variables.get_mut(index) {
                        row.skip = !row.skip;
                    }
                    this.templates_page.save(cx);
                    cx.notify();
                }))
                .child(checkbox(row.skip, theme));
            let mut line = div().flex().flex_col().flex_none().gap(px(SPACE_XS)).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(div().w(px(120.)).child(self.small_field(
                        &format!("template-variable-{selector}-name"),
                        Field::Variable(index, VariablePart::Name),
                        &row.name,
                        theme,
                        cx,
                    )))
                    .child(self.small_field(
                        &format!("template-variable-{selector}-expression"),
                        Field::Variable(index, VariablePart::Expression),
                        &row.expression,
                        theme,
                        cx,
                    ))
                    .child(self.small_field(
                        &format!("template-variable-{selector}-default"),
                        Field::Variable(index, VariablePart::Default),
                        &row.default,
                        theme,
                        cx,
                    ))
                    .child(skip),
            );
            if let Some(warning) = warning {
                line = line.child(
                    div()
                        .pl(px(120. + SPACE_S))
                        .text_color(theme.text_secondary)
                        .child(warning),
                );
            }
            if active_expression {
                line =
                    line.child(self.expression_suggestions(index, &row.expression.text, theme, cx));
            }
            table = table.child(line);
        }
        table
    }

    /// The names of the known expressions that start with what the field holds, as a
    /// list under it. A click puts the whole expression in.
    fn expression_suggestions(
        &self,
        index: usize,
        text: &str,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> Div {
        let typed = text
            .split(['(', ' ', '\t'])
            .next()
            .unwrap_or_default()
            .to_lowercase();
        let mut row = div()
            .flex()
            .flex_wrap()
            .flex_none()
            .gap(px(SPACE_XS))
            .pl(px(120. + SPACE_S));
        let mut shown = 0;
        for expression in EXPRESSIONS {
            if shown == 8 {
                break;
            }
            let name = expression.trim_end_matches(['(', ')']);
            if !name.to_lowercase().starts_with(&typed) {
                continue;
            }
            shown += 1;
            let selector = name.to_lowercase();
            let value = *expression;
            row = row.child(
                div()
                    .id(SharedString::from(format!(
                        "template-expression-{selector}"
                    )))
                    .debug_selector(move || format!("template-expression-{selector}"))
                    .flex()
                    .items_center()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(row) = this.templates_page.variables.get_mut(index) {
                            row.expression = TextField::from_text(value.to_owned());
                        }
                        this.templates_page.save(cx);
                        cx.notify();
                    }))
                    .child(expression.to_string()),
            );
        }
        row
    }

    fn small_field(
        &self,
        id: &str,
        field: Field,
        text: &TextField,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> Stateful<Div> {
        let active = self.field == Some(field);
        let shown = text.text.clone();
        let selector = id.to_owned();
        div()
            .id(SharedString::from(format!("template-field-{id}")))
            .debug_selector(move || selector.clone())
            .flex()
            .items_center()
            .flex_1()
            .min_w(px(64.))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if active { theme.accent } else { theme.border })
            .on_click(cx.listener(move |this, _, _, cx| {
                this.templates_page.field = Some(field);
                cx.notify();
            }))
            .child(div().child(shown))
    }

    fn contexts_row(
        &self,
        view: &TemplateView,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> Div {
        let language = cx
            .try_global::<TemplateSettings>()
            .and_then(|settings| settings.language);
        let mut names = language.map_or_else(Vec::new, |language| language.context_names());
        for context in &view.contexts {
            if !names.contains(context) {
                names.push(context.clone());
            }
        }
        let mut row = div()
            .flex()
            .flex_wrap()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child("Applicable in"),
            );
        for name in names {
            let on = self.contexts.contains(&name);
            let selector = name.replace('.', "-");
            let label = name.clone();
            row = row.child(
                div()
                    .id(SharedString::from(format!("template-context-{selector}")))
                    .debug_selector(move || format!("settings-template-context-{selector}"))
                    .flex()
                    .items_center()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(if on { theme.accent } else { theme.border })
                    .when(on, |this| this.bg(theme.selection))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let name = name.clone();
                        if let Some(at) = this
                            .templates_page
                            .contexts
                            .iter()
                            .position(|known| *known == name)
                        {
                            this.templates_page.contexts.remove(at);
                        } else {
                            this.templates_page.contexts.push(name);
                        }
                        this.templates_page.save(cx);
                        cx.notify();
                    }))
                    .child(label),
            );
        }
        row
    }

    fn render_postfix(&mut self, theme: &Theme, cx: &mut Context<SettingsView>) -> Div {
        let views = self.views(cx);
        let postfix: Vec<TemplateView> = views
            .into_iter()
            .filter(|view| view.kind == Kind::Postfix)
            .collect();
        let enabled = cx
            .try_global::<TemplateSettings>()
            .is_none_or(|settings| settings.overrides.postfix_enabled);
        let mut page = div().flex().flex_col().flex_none().gap(px(SPACE_XS));
        page = page.child(
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .py(px(SPACE_S))
                .child(
                    div()
                        .id("postfix-enabled")
                        .debug_selector(|| "postfix-enabled".to_owned())
                        .occlude()
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.templates_page.toggle_postfix(cx);
                        }))
                        .child(checkbox(enabled, theme)),
                )
                .child("Enable postfix completion"),
        );
        if postfix.is_empty() {
            return page.child(
                div()
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("The language offers no postfix templates."),
            );
        }
        for view in &postfix {
            let id = view.id.clone();
            let selector = selector_of(&id);
            let row_selector = selector.clone();
            let on = view.enabled;
            page = page.child(
                div()
                    .id(SharedString::from(format!("postfix-{selector}")))
                    .debug_selector(move || format!("settings-postfix-{row_selector}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .min_h(px(24.))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .child(
                        div()
                            .id(SharedString::from(format!("postfix-toggle-{selector}")))
                            .debug_selector(move || format!("settings-postfix-enabled-{selector}"))
                            .occlude()
                            .on_click(cx.listener({
                                let id = id.clone();
                                move |this, _, _, cx| this.templates_page.toggle(&id, cx)
                            }))
                            .child(checkbox(on, theme)),
                    )
                    .child(div().w(px(80.)).child(view.key.clone()))
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(view.description.clone()),
                    ),
            );
        }
        page
    }

    fn render_files(&mut self, theme: &Theme, cx: &mut Context<SettingsView>) -> Div {
        let files = self.files(cx);
        let settings = cx.try_global::<TemplateSettings>().cloned();
        let selected = self.selected_file.clone();
        let mut page = div().flex().flex_col().flex_none().gap(px(SPACE_XS));
        if files.is_empty() {
            return page.child(
                div()
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("The application offers no file templates."),
            );
        }
        if let Some(id) = selected.clone()
            && let Some(file) = files.iter().find(|file| file.id == id).cloned()
        {
            page = page.child(self.file_editor(&file, theme, cx));
        }
        for file in &files {
            let id = file.id.clone();
            let selector = selector_of(&id);
            let chosen = selected.as_deref() == Some(id.as_str());
            let changed = settings
                .as_ref()
                .is_some_and(|settings| file.is_changed(&settings.overrides));
            page = page.child(
                div()
                    .id(SharedString::from(format!("file-template-{selector}")))
                    .debug_selector(move || format!("settings-file-template-{selector}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .min_h(px(24.))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(chosen, |this| this.bg(theme.selection))
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| this.templates_page.select_file(&id, cx)
                    }))
                    .child(div().flex_1().child(file.label.clone()))
                    .when(changed, |this| {
                        this.child(div().text_color(theme.text_secondary).child("changed"))
                    }),
            );
        }
        if selected.is_none() {
            page = page.child(
                div()
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("Choose a template to change its text."),
            );
        }
        page
    }

    fn file_editor(
        &mut self,
        file: &FileTemplateDefinition,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> Div {
        let id = file.id.clone();
        let active = self.field == Some(Field::FileText);
        let shown = self.file_text.text.clone();
        let mut boxed = div()
            .id("file-template-text-box")
            .debug_selector(|| "file-template-text".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(144.))
            .px(px(SPACE_M))
            .py(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .border_2()
            .border_color(if active { theme.accent } else { theme.border })
            .font_family(SharedString::from(MONO_FONT))
            .overflow_hidden()
            .on_click(cx.listener(|this, _, _, cx| {
                this.templates_page.field = Some(Field::FileText);
                cx.notify();
            }));
        boxed = if active {
            boxed.children(text_field_children(
                &self.file_text,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                18.,
                true,
            ))
        } else {
            boxed.child(div().child(shown))
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_S))
            .pt(px(SPACE_M))
            .border_b_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(
                        div()
                            .flex_1()
                            .child(format!("{} ({})", file.label, file.id)),
                    )
                    .child(
                        icon_button(
                            "file-template-reset",
                            "general/refresh",
                            "Reset to Default",
                            None,
                            theme,
                        )
                        .debug_selector(|| "file-template-reset".to_owned())
                        .on_click(cx.listener({
                            let id = id.clone();
                            move |this, _, _, cx| {
                                let Some(settings) = cx.try_global::<TemplateSettings>().cloned()
                                else {
                                    return;
                                };
                                let mut overrides = (*settings.overrides).clone();
                                overrides.reset_file(&id);
                                this.templates_page.store(overrides, cx);
                                this.templates_page.select_file(&id, cx);
                            }
                        })),
                    ),
            )
            .child(boxed)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_none()
                    .gap(px(SPACE_XS))
                    .pt(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .children(
                        FILE_TEMPLATE_VARIABLES
                            .iter()
                            .map(|(name, about)| div().child(format!("${{{name}}} — {about}"))),
                    ),
            )
    }
}

fn empty_language() -> &'static TemplateLanguage {
    static LANGUAGE: std::sync::OnceLock<TemplateLanguage> = std::sync::OnceLock::new();
    LANGUAGE.get_or_init(|| TemplateLanguage {
        templates: &[],
        contexts: "",
        grammar: || panic!("there is no grammar without a language"),
        indent_unit: "    ",
        receiver_prefixes: &[],
        query: std::sync::OnceLock::new(),
        declarations: None,
    })
}

fn first_context(contexts: &[&'static str]) -> &'static str {
    contexts.first().copied().unwrap_or(templates::ANY)
}

fn selector_of(id: &str) -> String {
    id.replace(['/', ' ', '.'], "-").to_lowercase()
}

fn group_names(views: &[TemplateView]) -> Vec<String> {
    let mut groups: Vec<String> = views.iter().map(|view| view.group.clone()).collect();
    groups.sort();
    groups.dedup();
    groups
}

/// The children of a unique name inside a group.
fn unique_abbreviation(
    overrides: &TemplateOverrides,
    language: Option<&'static TemplateLanguage>,
    group: &str,
    start: &str,
) -> String {
    let taken = |key: &str| {
        let id = id_of(group, key);
        overrides.live.contains_key(&id)
            || overrides.custom.iter().any(|custom| custom.id() == id)
            || language.is_some_and(|language| {
                language
                    .templates
                    .iter()
                    .any(|template| id_of(first_context(template.contexts), template.key) == id)
            })
    };
    if !taken(start) {
        return start.to_owned();
    }
    let mut serial = 1;
    loop {
        let name = format!("{start}{serial}");
        if !taken(&name) {
            return name;
        }
        serial += 1;
    }
}

/// The names of the variables of a template body that are not the special ones.
fn body_fields(body: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find('$') {
        let after = &rest[start + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            rest = tail;
            continue;
        }
        let Some(end) = after.find('$') else {
            break;
        };
        let name = &after[..end];
        let special = matches!(name, "END" | "SELECTION" | "EXPR");
        if !special && !name.is_empty() && !names.iter().any(|known| known == name) {
            names.push(name.to_owned());
        }
        rest = &after[end + 1..];
    }
    names
}

// The pages of the Settings window. `SettingsPage` decides which one shows.
impl TemplateSection {
    pub(crate) fn section(page: crate::settings_catalog::SettingsPage) -> Option<TemplateSection> {
        match page {
            crate::settings_catalog::SettingsPage::EditorLiveTemplates => {
                Some(TemplateSection::Live)
            }
            crate::settings_catalog::SettingsPage::EditorPostfix => Some(TemplateSection::Postfix),
            crate::settings_catalog::SettingsPage::EditorFileTemplates => {
                Some(TemplateSection::Files)
            }
            _ => None,
        }
    }
}

// The page belongs to the Settings window and is built with it.
pub(crate) fn page(workspace: WeakEntity<Workspace>) -> TemplatesPage {
    TemplatesPage::new(workspace)
}

#[cfg(test)]
mod tests {
    use super::{body_fields, selector_of};

    #[test]
    fn the_variables_of_a_body_leave_out_the_special_names() {
        assert_eq!(
            body_fields("log($NAME$);\n$NAME$ = $VALUE$;$END$"),
            ["NAME", "VALUE"]
        );
        assert_eq!(body_fields("no variables"), Vec::<String>::new());
        assert_eq!(body_fields("$SELECTION$ $EXPR$"), Vec::<String>::new());
    }

    #[test]
    fn a_selector_has_no_slashes_or_spaces() {
        assert_eq!(selector_of("statement/sout"), "statement-sout");
    }
}

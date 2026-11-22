//! The Run/Debug Configurations dialog: the stored configurations of the project, the
//! ones found in it, and the fields their providers describe. The dialog knows no
//! language: a provider reads and writes every field in the settings of a configuration.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;

use project_model::ProjectModel;
use runner::{
    BeforeLaunchStep, ConfigField, ConfigFieldKind, ConfigTemplate, RunConfig, RunConfigProvider,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::TextField;
use crate::run::RunPanel;

const WIDTH: f32 = 980.;
/// The share of the window height the dialog takes, as the Settings window does.
const HEIGHT_SHARE: f32 = 0.8;
const LIST_WIDTH: f32 = 260.;
const CHECKBOX: f32 = 14.;
/// The keys of the optional fields of the Modify options menu, as a configuration keeps
/// them in `shown_options`.
const ENVIRONMENT_FILE: &str = "environment_file";
const ALLOW_MULTIPLE_INSTANCES: &str = "allow_multiple_instances";
/// The optional fields in the order of the menu, with their labels.
const OPTIONS: [(&str, &str); 2] = [
    (ENVIRONMENT_FILE, "Environment file"),
    (ALLOW_MULTIPLE_INSTANCES, "Allow multiple instances"),
];
/// What the buttons of the steps say while no step is selected.
const STEP_HINT: &str = "Select a step to move or remove it";

/// One configuration of the dialog and what the last read of it was.
#[derive(Clone)]
struct Item {
    config: RunConfig,
    original: RunConfig,
    /// The configuration goes into `.arro/run.toml`.
    saved: bool,
    /// The configuration is temporary: it is not in the file and is not lost until the
    /// dialog changes it.
    temporary: bool,
    /// What the user chose with Store as project file, when the choice is theirs rather
    /// than the kind of the configuration.
    store_choice: Option<bool>,
}

/// The field of a provider as the dialog shows it.
struct FieldRow {
    field: ConfigField,
    text: TextField,
    /// The value of a `Bool` field.
    flag: bool,
}

/// A before launch step and the input of its goal, task or configuration name.
struct StepRow {
    step: BeforeLaunchStep,
    target: TextField,
}

impl StepRow {
    fn new(step: BeforeLaunchStep, configs: &[String]) -> Self {
        let mut target = match &step {
            BeforeLaunchStep::Task { target } => target.clone(),
            BeforeLaunchStep::Configuration { name } => name.clone(),
            BeforeLaunchStep::Build => String::new(),
        };
        if target.is_empty()
            && let BeforeLaunchStep::Configuration { .. } = step
            && let Some(first) = configs.first()
        {
            target = first.clone();
        }
        let step = match &step {
            BeforeLaunchStep::Configuration { .. } => BeforeLaunchStep::Configuration {
                name: target.clone(),
            },
            other => other.clone(),
        };
        Self {
            step,
            target: TextField::from_text(target),
        }
    }
}

/// A template of the Add button with the provider that creates it.
struct TemplateRow {
    provider: Arc<dyn RunConfigProvider>,
    template: ConfigTemplate,
}

/// The configurations of one kind, as the tree of the dialog groups them.
struct ConfigGroup {
    provider: String,
    name: &'static str,
    icon: &'static str,
    /// The name of the group in the selectors of a test.
    slug: String,
    items: Vec<usize>,
}

fn slug_of(name: &str) -> String {
    let mut slug = String::new();
    for character in name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    slug.trim_end_matches('-').to_owned()
}

/// The optional fields a configuration shows when the dialog opens it: the ones it keeps
/// on show and the ones that hold a value. Keys the dialog does not know are left out.
fn options_implied(config: &RunConfig) -> BTreeSet<&'static str> {
    OPTIONS
        .iter()
        .map(|&(key, _)| key)
        .filter(|&key| {
            config
                .shown_options
                .iter()
                .any(|shown| shown.as_str() == key)
                || match key {
                    ENVIRONMENT_FILE => config
                        .environment_file
                        .as_ref()
                        .is_some_and(|path| !path.as_os_str().is_empty()),
                    ALLOW_MULTIPLE_INSTANCES => config.allow_multiple_instances,
                    _ => false,
                }
        })
        .collect()
}

/// The Run/Debug Configurations dialog over the workspace.
pub struct RunConfigDialog {
    items: Vec<Item>,
    selected: Option<usize>,
    name: TextField,
    /// The steps before the program, in order.
    steps: Vec<StepRow>,
    /// The step the buttons of the list act on.
    step_selected: Option<usize>,
    /// The step whose goal is being typed.
    step_active: Option<usize>,
    /// The list of the kinds of a new step is open.
    steps_add_open: bool,
    /// The configurations a compound configuration starts.
    compound: Vec<String>,
    allow_multiple_instances: bool,
    environment_file: TextField,
    /// The optional fields the form shows: the ones the menu checked, and the ones that held
    /// a value when the configuration was read. A field stays shown until the menu unchecks
    /// it, so clearing its text does not make it vanish under the caret.
    options_shown: BTreeSet<&'static str>,
    /// The Modify options menu is open.
    options_open: bool,
    rows: Vec<FieldRow>,
    /// `0` is the name, then one per row, then the environment file.
    active: usize,
    templates: Vec<TemplateRow>,
    templates_open: bool,
    /// The text that filters the kinds of the Add menu.
    template_search: TextField,
    /// The row of the filtered Add menu the keys are on.
    template_mark: usize,
    /// The groups of the tree the user closed.
    collapsed: HashSet<String>,
    /// The list or the fields changed since the last write.
    changed: bool,
    error: Option<String>,
    providers: Vec<Arc<dyn RunConfigProvider>>,
    model: Option<Arc<ProjectModel>>,
    panel: Entity<RunPanel>,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    scroll_list: ScrollHandle,
    scroll_fields: ScrollHandle,
}

impl RunConfigDialog {
    pub(crate) fn show(
        panel: Entity<RunPanel>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        Self::show_selected(panel, workspace, window, cx);
    }

    /// Opens the dialog on `config`: a configuration that is not in the list becomes
    /// temporary first, as Run of a place does.
    pub(crate) fn show_config(
        panel: Entity<RunPanel>,
        config: RunConfig,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        panel.update(cx, |panel, cx| panel.choose_configuration(config, cx));
        Self::show_selected(panel, workspace, window, cx);
    }

    fn show_selected(
        panel: Entity<RunPanel>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let (items, selected, providers, model, store_error) = {
            let panel = panel.read(cx);
            let items: Vec<Item> = panel
                .configuration_items()
                .into_iter()
                .map(|(config, saved, temporary)| Item {
                    original: config.clone(),
                    config,
                    saved,
                    temporary,
                    store_choice: None,
                })
                .collect();
            (
                items,
                panel.selected_configuration(),
                panel.providers(),
                panel.model(),
                panel.store_error(),
            )
        };
        let templates = providers
            .iter()
            .flat_map(|provider| {
                provider
                    .templates()
                    .into_iter()
                    .map(|template| TemplateRow {
                        provider: provider.clone(),
                        template,
                    })
            })
            .collect();
        let weak = cx.weak_entity();
        let selected = selected.filter(|index| *index < items.len());
        let view = cx.new(|cx| {
            let mut dialog = Self {
                items,
                selected,
                name: TextField::default(),
                steps: Vec::new(),
                step_selected: None,
                step_active: None,
                steps_add_open: false,
                compound: Vec::new(),
                allow_multiple_instances: false,
                environment_file: TextField::default(),
                options_shown: BTreeSet::new(),
                options_open: false,
                rows: Vec::new(),
                active: usize::MAX,
                templates,
                templates_open: false,
                template_search: TextField::default(),
                template_mark: 0,
                collapsed: HashSet::new(),
                changed: false,
                error: store_error,
                providers,
                model,
                panel,
                focus: cx.focus_handle(),
                workspace: weak,
                scroll_list: ScrollHandle::new(),
                scroll_fields: ScrollHandle::new(),
            };
            if dialog.selected.is_none() {
                dialog.selected = (!dialog.items.is_empty()).then_some(0);
            }
            dialog.load_fields();
            dialog
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn provider_of(&self, config: &RunConfig) -> Option<&Arc<dyn RunConfigProvider>> {
        self.providers
            .iter()
            .find(|provider| provider.id() == config.provider)
    }

    fn item(&self) -> Option<&Item> {
        self.selected.and_then(|index| self.items.get(index))
    }

    /// Reads the fields of the selected configuration into the dialog.
    fn load_fields(&mut self) {
        self.name = TextField::default();
        self.steps.clear();
        self.step_selected = None;
        self.step_active = None;
        self.steps_add_open = false;
        self.compound.clear();
        self.allow_multiple_instances = false;
        self.environment_file = TextField::default();
        self.options_shown.clear();
        self.rows.clear();
        self.active = usize::MAX;
        let Some(index) = self.selected else {
            return;
        };
        let Some(config) = self.items.get(index).map(|item| item.config.clone()) else {
            return;
        };
        self.name = TextField::from_text(config.name.clone());
        let other_names: Vec<String> = self
            .items
            .iter()
            .filter(|item| item.config.name != config.name && !item.config.is_compound())
            .map(|item| item.config.name.clone())
            .collect();
        self.steps = config
            .before_launch_steps()
            .into_iter()
            .map(|step| StepRow::new(step, &other_names))
            .collect();
        self.compound = config
            .compound
            .iter()
            .filter(|name| self.items.iter().any(|item| item.config.name == **name))
            .cloned()
            .collect();
        self.allow_multiple_instances = config.allow_multiple_instances;
        self.environment_file = TextField::from_text(
            config
                .environment_file
                .as_ref()
                .map(|path| path.to_string_lossy().into_owned())
                .unwrap_or_default(),
        );
        self.options_shown = options_implied(&config);
        let Some(provider) = self.provider_of(&config).cloned() else {
            return;
        };
        self.rows = provider
            .fields(&config)
            .into_iter()
            .map(|field| {
                let text = provider.read_field(&config, field.key);
                let flag = text == "true";
                FieldRow {
                    field,
                    text: TextField::from_text(text),
                    flag,
                }
            })
            .collect();
    }

    /// The list of steps as the dialog shows it.
    fn steps_from_fields(&self) -> Vec<BeforeLaunchStep> {
        self.steps
            .iter()
            .map(|row| match &row.step {
                BeforeLaunchStep::Task { .. } => BeforeLaunchStep::Task {
                    target: row.target.text.trim().to_owned(),
                },
                BeforeLaunchStep::Configuration { .. } => BeforeLaunchStep::Configuration {
                    name: row.target.text.trim().to_owned(),
                },
                BeforeLaunchStep::Build => BeforeLaunchStep::Build,
            })
            .collect()
    }

    /// The configuration the fields of the dialog describe, on top of `base`.
    fn draft_config(&self, base: &RunConfig) -> RunConfig {
        let mut config = base.clone();
        config.name = self.name.text.trim().to_owned();
        let steps = self.steps_from_fields();
        // An untouched configuration keeps the old flag instead of gaining a list: an old
        // file reads the same after a visit to the dialog.
        if base.before_launch.is_none()
            && ((steps == [BeforeLaunchStep::Build] && base.assemble_first)
                || (steps.is_empty() && !base.assemble_first))
        {
            config.before_launch = None;
            config.assemble_first = base.assemble_first;
        } else {
            config.before_launch = Some(steps);
            config.assemble_first = false;
        }
        config.allow_multiple_instances = self.allow_multiple_instances;
        let file = self.environment_file.text.trim();
        config.environment_file = (!file.is_empty()).then(|| file.into());
        // The list is written only when the menu changed which optional fields show, so an
        // untouched configuration keeps its list, keys the dialog does not know included.
        if self.options_shown != options_implied(base) {
            config.shown_options = OPTIONS
                .iter()
                .map(|(key, _)| *key)
                .filter(|key| self.options_shown.contains(key))
                .map(str::to_owned)
                .collect();
        }
        if config.is_compound() {
            config.compound = self.compound.clone();
        }
        if let Some(provider) = self.provider_of(&config) {
            for row in &self.rows {
                let value = match row.field.kind {
                    ConfigFieldKind::Bool => {
                        if row.flag {
                            "true"
                        } else {
                            "false"
                        }
                    }
                    _ => &row.text.text,
                };
                provider.write_field(&mut config, row.field.key, value);
            }
        }
        config
    }

    /// Writes the fields into the selected configuration. A discovered configuration
    /// that changed becomes a stored one.
    fn commit(&mut self) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(item) = self.items.get(index) else {
            return;
        };
        let config = self.draft_config(&item.config);
        let item = &mut self.items[index];
        item.config = config;
        if item.config != item.original {
            let stored = item.store_choice.unwrap_or(true);
            item.saved = stored;
            item.temporary = !stored;
            self.changed = true;
        }
    }

    /// Whether the fields of the dialog differ from the selected configuration.
    fn fields_changed(&self, item: &Item) -> bool {
        if self.name.text.trim() != item.config.name {
            return true;
        }
        if self.steps_from_fields() != item.config.before_launch_steps() {
            return true;
        }
        if self.allow_multiple_instances != item.config.allow_multiple_instances {
            return true;
        }
        let stored_file = item
            .config
            .environment_file
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned())
            .unwrap_or_default();
        if self.environment_file.text.trim() != stored_file {
            return true;
        }
        if self.options_shown != options_implied(&item.config) {
            return true;
        }
        if item.config.is_compound() && self.compound != item.config.compound {
            return true;
        }
        let Some(provider) = self.provider_of(&item.config) else {
            return false;
        };
        for row in &self.rows {
            let shown = provider.read_field(&item.config, row.field.key);
            let same = if row.field.kind == ConfigFieldKind::Bool {
                (shown == "true") == row.flag
            } else {
                shown == row.text.text
            };
            if !same {
                return true;
            }
        }
        false
    }

    /// Whether an optional field holds a value. Such a field stays shown and checked.
    fn option_has_value(&self, key: &str) -> bool {
        match key {
            ENVIRONMENT_FILE => !self.environment_file.text.trim().is_empty(),
            ALLOW_MULTIPLE_INSTANCES => self.allow_multiple_instances,
            _ => false,
        }
    }

    /// Shows or hides an optional field from the Modify options menu. A field that holds
    /// a value cannot be unchecked: clear it first.
    fn toggle_option(&mut self, key: &'static str) {
        if self.option_has_value(key) {
            return;
        }
        if self.options_shown.remove(key) {
            // A hidden field takes no keys: the caret leaves it.
            if key == ENVIRONMENT_FILE && self.active == self.rows.len() + 1 {
                self.active = usize::MAX;
            }
        } else {
            self.options_shown.insert(key);
        }
    }

    /// Whether the dialog holds a change that OK or Apply would write.
    fn dirty(&self) -> bool {
        if self.changed {
            return true;
        }
        self.item().is_some_and(|item| self.fields_changed(item))
    }

    /// What keeps OK and Apply from saving the list.
    fn problem(&self) -> Option<String> {
        self.selected?;
        let name = self.name.text.trim();
        if name.is_empty() {
            return Some("The name must not be empty".to_owned());
        }
        if self
            .items
            .iter()
            .enumerate()
            .any(|(index, item)| Some(index) != self.selected && item.config.name == name)
        {
            return Some("A configuration with this name exists".to_owned());
        }
        None
    }

    fn unique_name(&self, base: &str) -> String {
        if !self.items.iter().any(|item| item.config.name == base) {
            return base.to_owned();
        }
        let mut number = 2;
        loop {
            let candidate = format!("{base} ({number})");
            if !self.items.iter().any(|item| item.config.name == candidate) {
                return candidate;
            }
            number += 1;
        }
    }

    /// The groups of the tree, in the order their first configuration appears.
    fn groups(&self) -> Vec<ConfigGroup> {
        let mut groups: Vec<ConfigGroup> = Vec::new();
        for (index, item) in self.items.iter().enumerate() {
            let provider = self.provider_of(&item.config);
            let (id, name, icon) = provider.map_or(("", "Application", "run/run"), |provider| {
                (
                    provider.id(),
                    provider.group_name(&item.config),
                    provider.group_icon(&item.config),
                )
            });
            if let Some(group) = groups
                .iter_mut()
                .find(|group| group.provider == id && group.name == name)
            {
                group.items.push(index);
            } else {
                groups.push(ConfigGroup {
                    provider: id.to_owned(),
                    name,
                    icon,
                    slug: slug_of(name),
                    items: vec![index],
                });
            }
        }
        groups
    }

    /// The kinds of the Add menu, filtered by the text typed into it.
    fn filtered_templates(&self) -> Vec<usize> {
        let needle = self.template_search.text.trim().to_ascii_lowercase();
        self.templates
            .iter()
            .enumerate()
            .filter(|(_, row)| {
                needle.is_empty() || row.template.name.to_ascii_lowercase().contains(&needle)
            })
            .map(|(index, _)| index)
            .collect()
    }

    /// Opens the list of kinds of the Add menu.
    fn open_templates(&mut self, cx: &mut Context<Self>) {
        self.templates_open = true;
        self.active = usize::MAX;
        self.step_active = None;
        self.steps_add_open = false;
        self.templates_add_reset();
        cx.notify();
    }

    fn templates_add_reset(&mut self) {
        self.template_search = TextField::default();
        self.template_mark = 0;
    }

    fn move_template_mark(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.filtered_templates().len();
        if count == 0 {
            return;
        }
        let current = self.template_mark.min(count - 1) as isize;
        self.template_mark = (current + delta).clamp(0, count as isize - 1) as usize;
        cx.notify();
    }

    fn add_marked(&mut self, cx: &mut Context<Self>) {
        let filtered = self.filtered_templates();
        let Some(index) = filtered.get(self.template_mark).copied() else {
            return;
        };
        self.add_from(index, cx);
    }

    /// Whether the selected configuration lives in the project file.
    fn stores_file(&self) -> bool {
        self.item().is_some_and(|item| item.saved)
    }

    /// Writes the selected configuration into the project file or keeps it out of it.
    fn set_stored(&mut self, stored: bool, cx: &mut Context<Self>) {
        if self.selected.is_none() {
            return;
        }
        if self.problem().is_some() {
            self.error = self.problem();
            cx.notify();
            return;
        }
        self.commit();
        if let Some(item) = self.items.get_mut(self.selected.unwrap_or(usize::MAX)) {
            item.store_choice = Some(stored);
            if item.saved != stored || item.temporary == stored {
                item.saved = stored;
                item.temporary = !stored;
                self.changed = true;
            }
        }
        cx.notify();
    }

    /// Moves the selected configuration one place up (`-1`) or down (`1`) inside its
    /// group. The list of the file keeps the same order.
    fn move_item(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        if self.problem().is_some() {
            self.error = self.problem();
            cx.notify();
            return;
        }
        self.commit();
        let Some(group) = self
            .groups()
            .into_iter()
            .find(|group| group.items.contains(&index))
        else {
            return;
        };
        let Some(position) = group.items.iter().position(|item| *item == index) else {
            return;
        };
        let Some(target) = position
            .checked_add_signed(delta)
            .and_then(|target| group.items.get(target))
            .copied()
        else {
            return;
        };
        self.items.swap(index, target);
        self.selected = Some(target);
        self.changed = true;
        cx.notify();
    }

    /// Sorts every group by name, keeping the selected configuration selected.
    fn sort_items(&mut self, cx: &mut Context<Self>) {
        if self.problem().is_some() {
            self.error = self.problem();
            cx.notify();
            return;
        }
        self.commit();
        let selected_name = self.item().map(|item| item.config.name.clone());
        let mut next: Vec<Item> = Vec::with_capacity(self.items.len());
        for group in self.groups() {
            let mut indexes = group.items;
            indexes.sort_by(|left, right| {
                self.items[*left]
                    .config
                    .name
                    .cmp(&self.items[*right].config.name)
            });
            for index in indexes {
                next.push(self.items[index].clone());
            }
        }
        self.items = next;
        self.selected = selected_name
            .and_then(|name| self.items.iter().position(|item| item.config.name == name))
            .or_else(|| (!self.items.is_empty()).then_some(0));
        self.load_fields();
        self.changed = true;
        cx.notify();
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.selected == Some(index) || index >= self.items.len() {
            return;
        }
        if self.problem().is_some() {
            self.error = self.problem();
            cx.notify();
            return;
        }
        self.commit();
        self.selected = Some(index);
        self.error = None;
        self.load_fields();
        cx.notify();
    }

    fn add(&mut self, cx: &mut Context<Self>) {
        if self.templates.is_empty() {
            return;
        }
        if self.templates_open {
            self.templates_open = false;
            cx.notify();
        } else {
            self.open_templates(cx);
        }
    }

    fn add_from(&mut self, index: usize, cx: &mut Context<Self>) {
        if self.problem().is_some() {
            self.error = self.problem();
            cx.notify();
            return;
        }
        self.commit();
        let Some(template) = self.templates.get(index) else {
            return;
        };
        let model = self.model.clone().unwrap_or_default();
        let created = template
            .provider
            .create_config(template.template.id, &model);
        match created {
            Ok(mut config) => {
                config.name = self.unique_name(&config.name);
                self.items.push(Item {
                    original: config.clone(),
                    config,
                    saved: true,
                    temporary: false,
                    store_choice: None,
                });
                self.select_silently(self.items.len() - 1);
                self.changed = true;
            }
            Err(error) => self.error = Some(error.to_string()),
        }
        self.templates_open = false;
        cx.notify();
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        if self.problem().is_some() {
            self.error = self.problem();
            cx.notify();
            return;
        }
        self.commit();
        let Some(item) = self.item() else {
            return;
        };
        let mut config = item.config.clone();
        config.name = self.unique_name(&config.name);
        let config = RunConfig {
            name: config.name,
            ..config
        };
        self.items.push(Item {
            original: config.clone(),
            config,
            saved: true,
            temporary: false,
            store_choice: None,
        });
        self.select_silently(self.items.len() - 1);
        self.changed = true;
        cx.notify();
    }

    /// Adds a step and gives its field the focus.
    fn add_step(&mut self, step: BeforeLaunchStep, cx: &mut Context<Self>) {
        let names = self
            .items
            .iter()
            .filter(|item| !item.config.is_compound())
            .map(|item| item.config.name.clone())
            .collect::<Vec<_>>();
        self.steps.push(StepRow::new(step, &names));
        self.step_selected = Some(self.steps.len() - 1);
        self.step_active = Some(self.steps.len() - 1);
        self.steps_add_open = false;
        self.active = usize::MAX;
        cx.notify();
    }

    fn remove_step(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.steps.len() {
            return;
        }
        self.steps.remove(index);
        self.step_active = None;
        self.step_selected = (!self.steps.is_empty()).then(|| index.min(self.steps.len() - 1));
        cx.notify();
    }

    /// Moves a step one place up (`-1`) or down (`1`).
    fn move_step(&mut self, index: usize, delta: isize, cx: &mut Context<Self>) {
        let Some(target) = index
            .checked_add_signed(delta)
            .filter(|target| *target < self.steps.len())
        else {
            return;
        };
        self.steps.swap(index, target);
        if self.step_selected == Some(index) {
            self.step_selected = Some(target);
        } else if self.step_selected == Some(target) {
            self.step_selected = Some(index);
        }
        self.step_active = None;
        cx.notify();
    }

    fn remove(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        if index >= self.items.len() {
            return;
        }
        self.items.remove(index);
        let next = (!self.items.is_empty()).then(|| index.min(self.items.len() - 1));
        self.selected = next;
        self.changed = true;
        self.error = None;
        self.load_fields();
        cx.notify();
    }

    /// Makes `index` the selected configuration and reads its fields without the checks
    /// of a click in the list: the caller knows the configuration it just made is valid.
    fn select_silently(&mut self, index: usize) {
        self.selected = Some(index);
        self.error = None;
        self.load_fields();
    }

    fn apply(&mut self, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(problem) = self.problem() {
            self.error = Some(problem);
            cx.notify();
            return;
        }
        self.commit();
        let items: Vec<(RunConfig, bool, bool)> = self
            .items
            .iter()
            .map(|item| (item.config.clone(), item.saved, item.temporary))
            .collect();
        let selected = self.selected;
        let changed = self.changed;
        let saved = self.panel.update(cx, |panel, cx| {
            panel.apply_configurations(items, selected, changed, cx)
        });
        match saved {
            Ok(()) => {
                for item in &mut self.items {
                    item.original = item.config.clone();
                }
                self.changed = false;
                self.error = None;
                if close {
                    self.close(window, cx);
                } else {
                    self.load_fields();
                    cx.notify();
                }
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let stroke = &event.keystroke;
        let primary = stroke.modifiers.control || stroke.modifiers.platform;
        if self.templates_open {
            match stroke.key.as_str() {
                "escape" => {
                    self.templates_open = false;
                    cx.stop_propagation();
                    cx.notify();
                }
                "up" => {
                    self.move_template_mark(-1, cx);
                    cx.stop_propagation();
                }
                "down" => {
                    self.move_template_mark(1, cx);
                    cx.stop_propagation();
                }
                "enter" => {
                    self.add_marked(cx);
                    cx.stop_propagation();
                }
                _ => {
                    if self.template_search.key_with_clipboard(event, false, cx) {
                        self.template_mark = 0;
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
            }
            return;
        }
        if (primary && stroke.key.as_str() == "n")
            || (stroke.modifiers.alt && stroke.key.as_str() == "insert")
        {
            self.add(cx);
            cx.stop_propagation();
            return;
        }
        if primary && stroke.key.as_str() == "d" {
            self.copy(cx);
            cx.stop_propagation();
            return;
        }
        if stroke.key.as_str() == "delete"
            && self.active == usize::MAX
            && self.step_active.is_none()
        {
            if let Some(index) = self.step_selected {
                self.remove_step(index, cx);
            } else {
                self.remove(cx);
            }
            cx.stop_propagation();
            return;
        }
        if self.steps_add_open && stroke.key.as_str() == "escape" {
            self.steps_add_open = false;
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if !matches!(event.keystroke.key.as_str(), "escape" | "enter")
            && let Some(index) = self.step_active
            && let Some(row) = self.steps.get_mut(index)
        {
            if row.target.key_with_clipboard(event, false, cx) {
                cx.stop_propagation();
                cx.notify();
            }
            return;
        }
        let env_active = self.rows.len() + 1;
        match event.keystroke.key.as_str() {
            "escape" => self.close(window, cx),
            // Enter presses OK, which takes no press while the configuration has a problem.
            "enter" if self.problem().is_none() => self.apply(true, window, cx),
            "space" if self.active > 0 && self.active <= self.rows.len() => {
                if let Some(row) = self.rows.get_mut(self.active - 1)
                    && row.field.kind == ConfigFieldKind::Bool
                {
                    row.flag = !row.flag;
                    cx.stop_propagation();
                    cx.notify();
                }
            }
            _ if self.active == 0 => {
                if self.name.key_with_clipboard(event, false, cx) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }
            _ if self.active == env_active => {
                if self.environment_file.key_with_clipboard(event, false, cx) {
                    cx.stop_propagation();
                    cx.notify();
                }
            }
            _ => {
                if let Some(row) = self.rows.get_mut(self.active.saturating_sub(1)) {
                    if matches!(
                        row.field.kind,
                        ConfigFieldKind::Bool | ConfigFieldKind::ReadOnly
                    ) {
                        return;
                    }
                    if row.text.key_with_clipboard(event, false, cx) {
                        cx.stop_propagation();
                        cx.notify();
                    }
                }
            }
        }
    }
}

impl Render for RunConfigDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        // The label column fits the widest label of the form: the fixed ones and the labels of
        // the fields of the configuration type.
        let mut labels = vec!["Name", "Configurations", "Environment file"];
        labels.extend(self.rows.iter().map(|row| row.field.label));
        let label_width = form_label_width(window, &labels);
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(2. * DIALOG_PADDING));
        let height =
            (viewport.height * HEIGHT_SHARE).min(viewport.height - px(2. * DIALOG_PADDING));
        let left = (viewport.width - width) / 2.;
        let top = ((viewport.height - height) / 2.).max(px(DIALOG_PADDING));

        let mut tree_rows: Vec<AnyElement> = Vec::new();
        for (group_index, group) in self.groups().into_iter().enumerate() {
            let collapsed = self.collapsed.contains(&group.slug);
            let slug = group.slug.clone();
            let group_selector = format!("run-config-group-{slug}");
            let count_selector = format!("run-config-group-count-{slug}-{}", group.items.len());
            let group_name = group.name;
            let group_icon = group.icon;
            let count = group.items.len();
            let toggle_slug = slug.clone();
            tree_rows.push(
                div()
                    .id(("run-config-group", group_index))
                    .debug_selector(move || group_selector)
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .text_color(theme.text)
                    .hover({
                        let hover = theme.hover;
                        move |style| style.bg(hover)
                    })
                    .child(icon_sized(
                        if collapsed {
                            "general/chevronRight"
                        } else {
                            "general/chevronDown"
                        },
                        TEXT_SMALL,
                        &theme,
                    ))
                    .child(icon(group_icon, &theme))
                    .child(group_name)
                    .child(div().flex_1())
                    .child(
                        div()
                            .debug_selector(move || count_selector)
                            .text_color(theme.text_muted)
                            .child(count.to_string()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.collapsed.remove(&toggle_slug) {
                            this.collapsed.insert(toggle_slug.clone());
                        }
                        this.active = usize::MAX;
                        this.step_active = None;
                        this.step_selected = None;
                        cx.notify();
                    }))
                    .into_any_element(),
            );
            if collapsed {
                continue;
            }
            for index in group.items {
                let item = &self.items[index];
                let selected = self.selected == Some(index);
                let temporary = item.temporary;
                let changed =
                    item.config != item.original || (selected && self.fields_changed(item));
                let kind = self
                    .provider_of(&item.config)
                    .map_or("run/run", |provider| provider.icon(&item.config));
                let label: SharedString = if changed {
                    format!("{} *", item.config.name).into()
                } else {
                    item.config.name.clone().into()
                };
                tree_rows.push(
                    div()
                        .id(("run-config-item", index))
                        .debug_selector(move || format!("run-config-item-{index}"))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_S))
                        .h(px(CONTROL_HEIGHT))
                        .pl(px(SPACE_L))
                        .pr(px(SPACE_M))
                        .rounded(px(CONTROL_RADIUS))
                        .text_color(if selected {
                            theme.text
                        } else {
                            theme.text_secondary
                        })
                        .when(selected, |this| this.bg(theme.selection))
                        .when(!selected, |this| {
                            let hover = theme.hover;
                            this.hover(move |style| style.bg(hover))
                        })
                        .child(
                            div()
                                .flex_none()
                                .when(temporary, |this| this.opacity(0.45))
                                .child(icon(kind, &theme)),
                        )
                        .child(label)
                        .on_click(cx.listener(move |this, _, _, cx| this.select(index, cx)))
                        .into_any_element(),
                );
            }
        }
        if !self.templates.is_empty() {
            let hover = theme.hover;
            tree_rows.push(
                div()
                    .id("run-config-edit-templates")
                    .debug_selector(|| "run-config-edit-templates".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .text_color(theme.text_secondary)
                    .hover(move |style| style.bg(hover))
                    .child(icon_sized("general/settings", TEXT_SMALL, &theme))
                    .child("Edit configuration templates…")
                    .on_click(cx.listener(|this, _, _, cx| this.open_templates(cx)))
                    .into_any_element(),
            );
        }

        let templates = self.templates_open.then(|| {
            let filtered = self.filtered_templates();
            let rows = filtered.iter().enumerate().map(|(position, index)| {
                let index = *index;
                let template = &self.templates[index];
                let id = template.template.id;
                let row_icon = template.provider.template_icon(&template.template);
                let label: SharedString = template.template.name.into();
                let marked = position == self.template_mark;
                div()
                    .id(("run-config-template", index))
                    .debug_selector(move || format!("run-config-template-{id}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .text_color(theme.text)
                    .when(marked, |this| this.bg(theme.selection))
                    .when(!marked, |this| {
                        this.hover(move |style| style.bg(theme.hover))
                    })
                    .child(icon(row_icon, &theme))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.add_from(index, cx)))
            });
            let empty = filtered.is_empty().then(|| {
                div()
                    .px(px(SPACE_M))
                    .py(px(SPACE_S))
                    .text_color(theme.text_muted)
                    .child("No matching configuration type")
            });
            let search_active = self.templates_open;
            surface(&theme)
                .mt(px(SPACE_XS))
                .p(px(SPACE_XS))
                .gap(px(2.))
                .child(field_box(
                    FieldMouse::new(cx, |this| Some(&mut this.template_search)),
                    "run-config-template-search",
                    "run-config-template-search".to_owned(),
                    &self.template_search,
                    search_active,
                    &theme,
                ))
                .children(rows)
                .children(empty)
                .into_any_element()
        });

        let name_active = self.active == 0;
        let name = field_box(
            FieldMouse::new(cx, |this| Some(&mut this.name)),
            "run-config-name",
            "run-config-name".to_owned(),
            &self.name,
            name_active,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.step_active = None;
            this.active = 0;
            window.focus(&this.focus, cx);
            cx.notify();
        }));
        let live = self.problem();
        let error = live.or_else(|| self.error.clone()).map(|error| {
            div()
                .debug_selector(|| "run-config-error".to_owned())
                .text_color(theme.text)
                .child(error)
        });

        let step_rows = self
            .steps
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let label: SharedString = row.step.label().into();
                let selected = self.step_selected == Some(index);
                let step_icon = match &row.step {
                    BeforeLaunchStep::Build => "run/run",
                    BeforeLaunchStep::Task { .. } => "run/run",
                    BeforeLaunchStep::Configuration { .. } => "nodes/class",
                };
                let edit = match &row.step {
                    BeforeLaunchStep::Task { .. } => Some(
                        field_box(
                            FieldMouse::new(cx, move |this| {
                                this.steps.get_mut(index).map(|row| &mut row.target)
                            }),
                            ("run-config-step-target", index),
                            format!("run-config-step-target-{index}"),
                            &row.target,
                            self.step_active == Some(index),
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.step_active = Some(index);
                            this.active = usize::MAX;
                            window.focus(&this.focus, cx);
                            cx.stop_propagation();
                            cx.notify();
                        }))
                        .into_any_element(),
                    ),
                    BeforeLaunchStep::Configuration { name } => {
                        let choices = self
                            .items
                            .iter()
                            .filter(|item| !item.config.is_compound())
                            .map(|item| item.config.name.clone())
                            .filter(|choice| choice != name)
                            .collect::<Vec<_>>();
                        let buttons = choices.into_iter().map(|choice| {
                            let value = choice.clone();
                            let selector = format!("run-config-step-choice-{index}-{choice}");
                            small_button(
                                format!("run-config-step-choice-{index}-{choice}"),
                                selector,
                                &choice,
                                &theme,
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    if let Some(row) = this.steps.get_mut(index) {
                                        row.step = BeforeLaunchStep::Configuration {
                                            name: value.clone(),
                                        };
                                        row.target = TextField::from_text(value.clone());
                                        this.step_active = None;
                                        cx.notify();
                                    }
                                },
                            ))
                        });
                        Some(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(SPACE_XS))
                                .child(
                                    div()
                                        .text_color(theme.text_secondary)
                                        .child(format!("Run {name}")),
                                )
                                .children(buttons)
                                .into_any_element(),
                        )
                    }
                    BeforeLaunchStep::Build => None,
                };
                div()
                    .id(("run-config-step", index))
                    .debug_selector(move || format!("run-config-step-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_XS))
                    .rounded(px(CONTROL_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .child(icon_sized(step_icon, TEXT_SMALL, &theme))
                    .child(div().w(px(60.)).flex_none().child(label))
                    .children(edit)
                    .child(div().flex_1())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.step_selected = Some(index);
                        this.step_active = None;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let step_selected = self.step_selected;
        // Without a selected step the buttons of the list say which step they need.
        let (remove_tip, up_tip, down_tip) = if step_selected.is_some() {
            (
                "Remove Before Launch Step",
                "Move Step Up",
                "Move Step Down",
            )
        } else {
            (STEP_HINT, STEP_HINT, STEP_HINT)
        };
        let add_step = icon_button(
            "run-config-step-add",
            "general/add",
            "Add Before Launch Step",
            None,
            &theme,
        )
        .debug_selector(|| "run-config-step-add".to_owned())
        .on_click(cx.listener(|this, _, _, cx| {
            this.steps_add_open = !this.steps_add_open;
            cx.notify();
        }));
        let remove_step = icon_button(
            "run-config-step-remove",
            "general/minus",
            remove_tip,
            None,
            &theme,
        )
        .debug_selector(|| "run-config-step-remove".to_owned())
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(index) = this.step_selected {
                this.remove_step(index, cx);
            }
        }));
        let step_up = icon_button(
            "run-config-step-up",
            "general/arrowUp",
            up_tip,
            None,
            &theme,
        )
        .debug_selector(|| "run-config-step-up".to_owned())
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(index) = this.step_selected {
                this.move_step(index, -1, cx);
            }
        }));
        let step_down = icon_button(
            "run-config-step-down",
            "general/arrowDown",
            down_tip,
            None,
            &theme,
        )
        .debug_selector(|| "run-config-step-down".to_owned())
        .on_click(cx.listener(move |this, _, _, cx| {
            if let Some(index) = this.step_selected {
                this.move_step(index, 1, cx);
            }
        }));
        let add_rows = [
            ("Build", "run/run", 0usize),
            ("Task", "run/run", 1),
            ("Configuration", "nodes/class", 2),
        ]
        .into_iter()
        .map(|(label, row_icon, kind)| {
            let id = match kind {
                0 => "run-config-step-add-build",
                1 => "run-config-step-add-task",
                _ => "run-config-step-add-config",
            };
            div()
                .id((id, kind))
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .text_color(theme.text)
                .hover({
                    let hover = theme.hover;
                    move |style| style.bg(hover)
                })
                .child(icon(row_icon, &theme))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let step = match kind {
                        0 => BeforeLaunchStep::Build,
                        1 => BeforeLaunchStep::Task {
                            target: String::new(),
                        },
                        _ => BeforeLaunchStep::Configuration {
                            name: String::new(),
                        },
                    };
                    this.add_step(step, cx);
                }))
        });
        let add_menu = self.steps_add_open.then(|| {
            surface(&theme)
                .mt(px(SPACE_XS))
                .p(px(SPACE_XS))
                .gap(px(2.))
                .children(add_rows)
                .into_any_element()
        });
        let steps = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .children(add_menu)
            .children(step_rows);

        let multiple = checkbox_row(
            "run-config-multiple",
            "Allow multiple instances",
            self.allow_multiple_instances,
            &theme,
        )
        .debug_selector(|| "run-config-multiple".to_owned())
        .on_click(cx.listener(|this, _, _, cx| {
            this.allow_multiple_instances = !this.allow_multiple_instances;
            cx.notify();
        }));

        let environment_active = self.active == self.rows.len() + 1;
        let environment = field_box(
            FieldMouse::new(cx, |this| Some(&mut this.environment_file)),
            "run-config-environment",
            "run-config-environment".to_owned(),
            &self.environment_file,
            environment_active,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.step_active = None;
            this.active = this.rows.len() + 1;
            window.focus(&this.focus, cx);
            cx.notify();
        }));

        // The optional fields show while the form keeps them on show, see `options_shown`.
        let environment_row = self.options_shown.contains(ENVIRONMENT_FILE).then(|| {
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_M))
                .child(form_label(label_width, "Environment file", &theme))
                .child(environment)
        });
        let multiple_row = self
            .options_shown
            .contains(ALLOW_MULTIPLE_INSTANCES)
            .then_some(multiple);

        let compound = self
            .item()
            .is_some_and(|item| item.config.is_compound())
            .then(|| {
                let rows = self.compound.iter().enumerate().map(|(index, name)| {
                    let value = name.clone();
                    let remove = small_button(
                        ("run-config-compound-remove", index),
                        format!("run-config-compound-remove-{index}"),
                        "×",
                        &theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.compound.retain(|name| name != &value);
                        cx.notify();
                    }));
                    div()
                        .id(("run-config-compound", index))
                        .debug_selector(move || format!("run-config-compound-{index}"))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_S))
                        .h(px(CONTROL_HEIGHT))
                        .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                        .child(remove)
                });
                let choices = self
                    .items
                    .iter()
                    .filter(|item| {
                        !item.config.is_compound() && !self.compound.contains(&item.config.name)
                    })
                    .map(|item| item.config.name.clone())
                    .collect::<Vec<_>>();
                let add = choices.into_iter().enumerate().map(|(index, name)| {
                    let value = name.clone();
                    let selector = format!("run-config-compound-add-{name}");
                    small_button(
                        format!("run-config-compound-add-{index}"),
                        selector,
                        &name,
                        &theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.compound.push(value.clone());
                        cx.notify();
                    }))
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .child(form_label(label_width, "Configurations", &theme))
                            .children(add),
                    )
                    .children(rows)
            });

        let rows = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let active = self.active == index + 1;
                let control = match row.field.kind {
                    ConfigFieldKind::Bool => {
                        let on = row.flag;
                        div()
                            .id(("run-config-toggle", index))
                            .debug_selector({
                                let key = row.field.key;
                                move || format!("run-config-field-{key}")
                            })
                            .flex()
                            .items_center()
                            .h(px(CONTROL_HEIGHT))
                            .child(
                                div()
                                    .flex_none()
                                    .w(px(CHECKBOX))
                                    .h(px(CHECKBOX))
                                    .rounded(px(CONTROL_RADIUS))
                                    .border_1()
                                    .border_color(if on {
                                        theme.accent
                                    } else {
                                        theme.control_border
                                    })
                                    .when(on, |this| this.bg(theme.accent)),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if let Some(row) = this.rows.get_mut(index) {
                                    row.flag = !row.flag;
                                    this.active = index + 1;
                                    this.changed = true;
                                    cx.notify();
                                }
                            }))
                            .into_any_element()
                    }
                    ConfigFieldKind::ReadOnly => {
                        let key = row.field.key;
                        let text = row.text.text.clone();
                        let field = field_box(
                            FieldMouse::new(cx, move |this| {
                                this.rows.get_mut(index).map(|row| &mut row.text)
                            }),
                            ("run-config-read-only", index),
                            format!("run-config-field-{key}"),
                            &row.text,
                            false,
                            &theme,
                        );
                        let copy = icon_button(
                            ("run-config-copy", index),
                            "general/copy",
                            "Copy",
                            None,
                            &theme,
                        )
                        .debug_selector(move || format!("run-config-copy-{key}"))
                        .on_click(cx.listener(move |_, _, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
                        }));
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .flex_1()
                            .min_w(px(0.))
                            .child(field)
                            .child(copy)
                            .into_any_element()
                    }
                    _ => {
                        let key = row.field.key;
                        let choices = match &row.field.kind {
                            ConfigFieldKind::Choice(choices) => choices.clone(),
                            _ => Vec::new(),
                        };
                        let field = field_box(
                            FieldMouse::new(cx, move |this| {
                                this.rows.get_mut(index).map(|row| &mut row.text)
                            }),
                            ("run-config-field", index),
                            format!("run-config-field-{key}"),
                            &row.text,
                            active,
                            &theme,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.active = index + 1;
                                this.step_active = None;
                                window.focus(&this.focus, cx);
                                cx.notify();
                            },
                        ));
                        if choices.is_empty() {
                            field.into_any_element()
                        } else {
                            let buttons = choices.into_iter().enumerate().map(|(choice, value)| {
                                let label = value.clone();
                                small_button(
                                    format!("run-config-choice-{index}-{choice}"),
                                    format!("run-config-choice-{key}-{choice}"),
                                    &label,
                                    &theme,
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        if let Some(row) = this.rows.get_mut(index) {
                                            row.text = TextField::from_text(value.clone());
                                            this.active = index + 1;
                                            cx.notify();
                                        }
                                    },
                                ))
                            });
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w(px(0.))
                                .gap(px(SPACE_XS))
                                .child(field)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(SPACE_XS))
                                        .children(buttons),
                                )
                                .into_any_element()
                        }
                    }
                };
                let hint = row
                    .field
                    .hint
                    .map(|hint| form_hint(label_width, hint, &theme));
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_M))
                            .child(form_label(label_width, row.field.label, &theme))
                            .child(control),
                    )
                    .children(hint)
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let add = icon_button(
            "run-config-add",
            "general/add",
            "Add New Configuration",
            Some("Alt+Insert".into()),
            &theme,
        )
        .debug_selector(|| "run-config-add".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.add(cx)));
        let copy = icon_button(
            "run-config-copy",
            "general/copy",
            "Copy Configuration",
            Some("Ctrl+D".into()),
            &theme,
        )
        .debug_selector(|| "run-config-copy".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.copy(cx)));
        let remove = icon_button(
            "run-config-remove",
            "general/minus",
            "Remove Configuration",
            Some("Delete".into()),
            &theme,
        )
        .debug_selector(|| "run-config-remove".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.remove(cx)));
        let save = icon_button(
            "run-config-save",
            "general/download",
            "Save as Project File",
            None,
            &theme,
        )
        .debug_selector(|| "run-config-save".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.set_stored(true, cx)));
        let move_up = icon_button(
            "run-config-move-up",
            "general/arrowUp",
            "Move Up",
            None,
            &theme,
        )
        .debug_selector(|| "run-config-move-up".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.move_item(-1, cx)));
        let move_down = icon_button(
            "run-config-move-down",
            "general/arrowDown",
            "Move Down",
            None,
            &theme,
        )
        .debug_selector(|| "run-config-move-down".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.move_item(1, cx)));
        let sort = icon_button(
            "run-config-sort",
            "general/filter",
            "Sort Configurations",
            None,
            &theme,
        )
        .debug_selector(|| "run-config-sort".to_owned())
        .on_click(cx.listener(|this, _, _, cx| this.sort_items(cx)));
        let toolbar = div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .child(add)
            .child(remove)
            .child(copy)
            .child(save)
            .child(div().w(px(SPACE_S)))
            .child(move_up)
            .child(move_down)
            .child(sort);
        let store = checkbox_row(
            "run-config-store",
            "Store as project file",
            self.item().is_some_and(|item| item.saved),
            &theme,
        )
        .flex_none()
        .debug_selector(|| "run-config-store".to_owned())
        .on_click(cx.listener(|this, _, _, cx| {
            let stored = this.stores_file();
            this.set_stored(!stored, cx);
        }));

        let can_apply = self.problem().is_none();
        let apply_enabled = can_apply && self.dirty();
        // A disabled OK takes no click: the problem of the configuration is shown above.
        let mut ok = dialog_button(
            "run-config-ok",
            "run-config-ok".to_owned(),
            "OK",
            true,
            can_apply,
            &theme,
        );
        if can_apply {
            ok = ok.on_click(cx.listener(|this, _, window, cx| this.apply(true, window, cx)));
        }
        let mut apply = dialog_button(
            "run-config-apply",
            "run-config-apply".to_owned(),
            "Apply",
            false,
            apply_enabled,
            &theme,
        );
        if apply_enabled {
            apply =
                apply.on_click(cx.listener(|this, _, window, cx| this.apply(false, window, cx)));
        } else {
            apply = apply.child(
                div()
                    .debug_selector(|| "run-config-apply-disabled".to_owned())
                    .size(px(1.)),
            );
        }

        let options_link =
            toolbar_dropdown("run-config-modify-options", None, "Modify options", &theme)
                .debug_selector(|| "run-config-modify-options".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.options_open = !this.options_open;
                    cx.notify();
                }));
        let options_menu = self.options_open.then(|| {
            let menu_rows = OPTIONS.iter().map(|&(key, label)| {
                let on = self.options_shown.contains(key);
                // A field with a value stays checked: its row is dimmed until it is empty.
                let locked = self.option_has_value(key);
                checkbox_row(format!("run-config-option-{key}"), label, on, &theme)
                    .debug_selector(move || format!("run-config-option-{key}"))
                    .when(locked, |row| row.opacity(0.5))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_option(key);
                        cx.notify();
                    }))
            });
            surface(&theme)
                .mt(px(SPACE_XS))
                .p(px(SPACE_XS))
                .gap(px(2.))
                .children(menu_rows)
                .into_any_element()
        });

        let shell = surface(&theme)
            .id("run-configs")
            .debug_selector(|| "run-configs".to_owned())
            .key_context("RunConfigs")
            .track_focus(&self.focus)
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .child(
                div()
                    .flex()
                    .gap(px(DIALOG_GAP))
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(SPACE_S))
                            .w(px(LIST_WIDTH))
                            .flex_none()
                            .child(toolbar)
                            .children(templates)
                            .child(
                                scrollable(
                                    "run-config-list",
                                    &self.scroll_list,
                                    div()
                                        .id("run-config-list")
                                        .debug_selector(|| "run-config-list".to_owned())
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.))
                                        .overflow_y_scroll()
                                        .track_scroll(&self.scroll_list)
                                        .children(tree_rows),
                                    &theme,
                                    window,
                                    cx,
                                )
                                .flex_1(),
                            ),
                    )
                    .child(
                        scrollable(
                            "run-config-fields",
                            &self.scroll_fields,
                            div()
                                .id("run-config-fields")
                                .debug_selector(|| "run-config-fields".to_owned())
                                .flex()
                                .flex_col()
                                .gap(px(SPACE_M))
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll_fields)
                                .pb(px(SPACE_M))
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(SPACE_M))
                                        .child(form_label(label_width, "Name", &theme))
                                        .child(name)
                                        .child(store),
                                )
                                .children(error)
                                .child(section_header("Build and run", [options_link], &theme))
                                .children(options_menu)
                                .children(rows)
                                .children(compound)
                                .children(environment_row)
                                .children(multiple_row)
                                .child(section_header(
                                    "Before launch",
                                    [add_step, remove_step, step_up, step_down],
                                    &theme,
                                ))
                                .child(steps),
                            &theme,
                            window,
                            cx,
                        )
                        .flex_1()
                        .min_w(px(0.)),
                    ),
            )
            .child(
                DialogButtons::new()
                    .primary(ok)
                    .other(
                        dialog_button(
                            "run-config-cancel",
                            "run-config-cancel".to_owned(),
                            "Cancel",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .apply(apply)
                    .into_row(),
            );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "run-configs",
                title: "Run/Debug Configurations".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: Some(f32::from(height)),
                },
                min: (640., 400.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

/// The title of one part of the form, with the controls of the part in its row at the
/// right.
fn section_header(
    label: &str,
    controls: impl IntoIterator<Item = impl IntoElement>,
    theme: &Theme,
) -> Div {
    let title = div().flex_1().min_w_0().child(label.to_owned());
    // The layout tests read the title from the drawn frame.
    #[cfg(test)]
    let title = title.child(bounds_probe(
        format!("run-config-section-{}", slug_of(label)),
        Some(label.to_owned().into()),
    ));
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_XS))
        .h(px(CONTROL_HEIGHT))
        .mt(px(SPACE_S))
        .text_color(theme.text)
        .font_weight(FontWeight::MEDIUM)
        .child(title)
        .children(controls)
}

/// A text input of the dialog.
fn field_box(
    mouse: FieldMouse,
    id: impl Into<ElementId>,
    selector: String,
    field: &TextField,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .min_w(px(0.))
        .flex_1()
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(if active {
            theme.accent
        } else {
            theme.control_border
        })
        .overflow_hidden()
        .text_color(theme.text)
        .child(
            div()
                .flex()
                .items_center()
                .overflow_hidden()
                .whitespace_nowrap()
                .children(text_field_children(
                    mouse,
                    field,
                    theme.text,
                    theme.text,
                    theme.selection,
                    theme.caret,
                    18.,
                    active,
                )),
        )
}

fn small_button(
    id: impl Into<ElementId>,
    selector: String,
    label: &str,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .justify_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.control_border)
        .text_color(theme.text)
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .child(label.to_owned())
}

#[cfg(test)]
mod tests {
    use super::slug_of;

    #[test]
    fn a_group_name_becomes_the_name_of_its_selector() {
        assert_eq!(slug_of("Application"), "application");
        assert_eq!(slug_of("Spring Boot"), "spring-boot");
        assert_eq!(slug_of("Remote JVM Debug"), "remote-jvm-debug");
        assert_eq!(slug_of("Shell/Command"), "shell-command");
        assert_eq!(slug_of(""), "");
    }
}

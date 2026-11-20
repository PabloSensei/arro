//! The Run/Debug Configurations dialog: the stored configurations of the project, the
//! ones found in it, and the fields their providers describe. The dialog knows no
//! language: a provider reads and writes every field in the settings of a configuration.

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
const LABEL_WIDTH: f32 = 150.;
const FIELD_HEIGHT: f32 = 26.;
const CHECKBOX: f32 = 14.;

/// One configuration of the dialog and what the last read of it was.
struct Item {
    config: RunConfig,
    original: RunConfig,
    /// The configuration goes into `.arro/run.toml`.
    saved: bool,
    /// The configuration is temporary: it is not in the file and is not lost until the
    /// dialog changes it.
    temporary: bool,
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

/// The Run/Debug Configurations dialog over the workspace.
pub struct RunConfigDialog {
    items: Vec<Item>,
    selected: Option<usize>,
    name: TextField,
    /// The steps before the program, in order.
    steps: Vec<StepRow>,
    /// The step whose goal is being typed.
    step_active: Option<usize>,
    /// The configurations a compound configuration starts.
    compound: Vec<String>,
    allow_multiple_instances: bool,
    environment_file: TextField,
    rows: Vec<FieldRow>,
    /// `0` is the name, then one per row, then the environment file.
    active: usize,
    templates: Vec<TemplateRow>,
    templates_open: bool,
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
                step_active: None,
                compound: Vec::new(),
                allow_multiple_instances: false,
                environment_file: TextField::default(),
                rows: Vec::new(),
                active: 0,
                templates,
                templates_open: false,
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
        self.step_active = None;
        self.compound.clear();
        self.allow_multiple_instances = false;
        self.environment_file = TextField::default();
        self.rows.clear();
        self.active = 0;
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

    /// Writes the fields into the selected configuration. A discovered configuration
    /// that changed becomes a stored one.
    fn commit(&mut self) {
        let Some(item) = self.items.get_mut(self.selected.unwrap_or(usize::MAX)) else {
            return;
        };
        item.config.name = self.name.text.trim().to_owned();
        let steps: Vec<BeforeLaunchStep> = self
            .steps
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
            .collect();
        // An untouched configuration keeps the old flag instead of gaining a list: an old
        // file reads the same after a visit to the dialog.
        if steps == [BeforeLaunchStep::Build]
            && item.original.before_launch.is_none()
            && item.original.assemble_first
        {
            item.config.before_launch = None;
            item.config.assemble_first = true;
        } else {
            item.config.before_launch = Some(steps);
            item.config.assemble_first = false;
        }
        item.config.allow_multiple_instances = self.allow_multiple_instances;
        let file = self.environment_file.text.trim();
        item.config.environment_file = (!file.is_empty()).then(|| file.into());
        if item.config.is_compound() {
            item.config.compound = self.compound.clone();
        }
        let provider = self
            .providers
            .iter()
            .find(|provider| provider.id() == item.config.provider)
            .cloned();
        if let Some(provider) = provider {
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
                provider.write_field(&mut item.config, row.field.key, value);
            }
        }
        if item.config != item.original {
            item.saved = true;
            item.temporary = false;
            self.changed = true;
        }
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
        self.templates_open = !self.templates_open;
        cx.notify();
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
        self.step_active = Some(self.steps.len() - 1);
        self.active = usize::MAX;
        cx.notify();
    }

    fn remove_step(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.steps.len() {
            return;
        }
        self.steps.remove(index);
        self.step_active = None;
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
        let items = self
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
            "enter" => self.apply(true, window, cx),
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
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(2. * DIALOG_PADDING));
        let height =
            (viewport.height * HEIGHT_SHARE).min(viewport.height - px(2. * DIALOG_PADDING));
        let left = (viewport.width - width) / 2.;
        let top = ((viewport.height - height) / 2.).max(px(DIALOG_PADDING));

        let items = self.items.iter().enumerate().map(|(index, item)| {
            let selected = self.selected == Some(index);
            let temporary = item.temporary;
            let kind = self
                .provider_of(&item.config)
                .map_or("run/run", |provider| provider.icon(&item.config));
            let label: SharedString = item.config.name.clone().into();
            div()
                .id(("run-config-item", index))
                .debug_selector(move || format!("run-config-item-{index}"))
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
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
        });

        let templates = self.templates_open.then(|| {
            let rows = self.templates.iter().enumerate().map(|(index, template)| {
                let id = template.template.id;
                let label: SharedString = template.template.name.into();
                div()
                    .id(("run-config-template", index))
                    .debug_selector(move || format!("run-config-template-{id}"))
                    .flex()
                    .items_center()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .text_color(theme.text)
                    .hover({
                        let hover = theme.hover;
                        move |style| style.bg(hover)
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.add_from(index, cx)))
            });
            div().flex().flex_col().children(rows).into_any_element()
        });

        let name_active = self.active == 0;
        let name = field_box(
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
                let up = small_button(
                    ("run-config-step-up", index),
                    format!("run-config-step-up-{index}"),
                    "↑",
                    &theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.move_step(index, -1, cx)));
                let down = small_button(
                    ("run-config-step-down", index),
                    format!("run-config-step-down-{index}"),
                    "↓",
                    &theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.move_step(index, 1, cx)));
                let remove = small_button(
                    ("run-config-step-remove", index),
                    format!("run-config-step-remove-{index}"),
                    "×",
                    &theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.remove_step(index, cx)));
                let edit = match &row.step {
                    BeforeLaunchStep::Task { .. } => Some(
                        field_box(
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
                    .h(px(FIELD_HEIGHT))
                    .child(div().w(px(60.)).flex_none().child(label))
                    .children(edit)
                    .child(div().flex_1())
                    .child(up)
                    .child(down)
                    .child(remove)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        let add_build = small_button(
            "run-config-step-add-build",
            "run-config-step-add-build".to_owned(),
            "+ Build",
            &theme,
        )
        .on_click(cx.listener(|this, _, _, cx| this.add_step(BeforeLaunchStep::Build, cx)));
        let add_task = small_button(
            "run-config-step-add-task",
            "run-config-step-add-task".to_owned(),
            "+ Task",
            &theme,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.add_step(
                BeforeLaunchStep::Task {
                    target: String::new(),
                },
                cx,
            )
        }));
        let add_config = small_button(
            "run-config-step-add-config",
            "run-config-step-add-config".to_owned(),
            "+ Configuration",
            &theme,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.add_step(
                BeforeLaunchStep::Configuration {
                    name: String::new(),
                },
                cx,
            )
        }));
        let steps = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(
                        div()
                            .w(px(LABEL_WIDTH))
                            .flex_none()
                            .text_color(theme.text_muted)
                            .child("Before launch"),
                    )
                    .child(add_build)
                    .child(add_task)
                    .child(add_config),
            )
            .children(step_rows);

        let multiple = {
            let on = self.allow_multiple_instances;
            let (hover, pressed) = (theme.hover, theme.pressed);
            div()
                .id("run-config-multiple")
                .debug_selector(|| "run-config-multiple".to_owned())
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .h(px(FIELD_HEIGHT))
                .cursor_pointer()
                .hover(move |style| style.bg(hover))
                .active(move |style| style.bg(pressed))
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
                .child(
                    div()
                        .text_color(theme.text)
                        .child("Allow multiple instances"),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.allow_multiple_instances = !this.allow_multiple_instances;
                    cx.notify();
                }))
        };

        let environment_active = self.active == self.rows.len() + 1;
        let environment = field_box(
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
                        .h(px(FIELD_HEIGHT))
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
                            .child(
                                div()
                                    .w(px(LABEL_WIDTH))
                                    .flex_none()
                                    .text_color(theme.text_muted)
                                    .child("Configurations"),
                            )
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
                            .h(px(FIELD_HEIGHT))
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
                let hint = row.field.hint.map(|hint| {
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_muted)
                        .child(hint)
                });
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_M))
                            .child(
                                div()
                                    .w(px(LABEL_WIDTH))
                                    .flex_none()
                                    .text_color(theme.text_muted)
                                    .child(row.field.label),
                            )
                            .child(control),
                    )
                    .children(hint)
                    .into_any_element()
            })
            .collect::<Vec<_>>();

        let add = small_button("run-config-add", "run-config-add".to_owned(), "+", &theme)
            .on_click(cx.listener(|this, _, _, cx| this.add(cx)));
        let copy = small_button(
            "run-config-copy",
            "run-config-copy".to_owned(),
            "Copy",
            &theme,
        )
        .on_click(cx.listener(|this, _, _, cx| this.copy(cx)));
        let remove = small_button(
            "run-config-remove",
            "run-config-remove".to_owned(),
            "Remove",
            &theme,
        )
        .on_click(cx.listener(|this, _, _, cx| this.remove(cx)));

        let can_apply = self.problem().is_none();
        let ok = dialog_button(
            "run-config-ok",
            "run-config-ok".to_owned(),
            "OK",
            true,
            can_apply,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.apply(true, window, cx)));
        let apply = dialog_button(
            "run-config-apply",
            "run-config-apply".to_owned(),
            "Apply",
            false,
            can_apply,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.apply(false, window, cx)));

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
                                        .children(items),
                                    &theme,
                                    window,
                                    cx,
                                )
                                .flex_1(),
                            )
                            .children(templates)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(SPACE_S))
                                    .child(add)
                                    .child(copy)
                                    .child(remove),
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
                                        .child(
                                            div()
                                                .w(px(LABEL_WIDTH))
                                                .flex_none()
                                                .text_color(theme.text_muted)
                                                .child("Name"),
                                        )
                                        .child(name),
                                )
                                .children(error)
                                .child(steps)
                                .children(compound)
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap(px(SPACE_M))
                                        .child(
                                            div()
                                                .w(px(LABEL_WIDTH))
                                                .flex_none()
                                                .text_color(theme.text_muted)
                                                .child("Environment file"),
                                        )
                                        .child(environment),
                                )
                                .child(multiple)
                                .children(rows),
                            &theme,
                            window,
                            cx,
                        )
                        .flex_1()
                        .min_w(px(0.)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .flex_none()
                    .gap(px(SPACE_M))
                    .child(
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
                    .child(apply)
                    .child(ok),
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

/// A text input of the dialog.
fn field_box(
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
        .h(px(FIELD_HEIGHT))
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
        .h(px(FIELD_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.control_border)
        .text_color(theme.text)
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .child(label.to_owned())
}

/// A button of the dialog: the primary one is filled with the accent, and a button that
/// cannot be used now is grey and has no handler.
fn dialog_button(
    id: impl Into<ElementId>,
    selector: String,
    label: &str,
    primary: bool,
    enabled: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_L))
        .rounded(px(CONTROL_RADIUS))
        .when(primary && enabled, |this| {
            this.bg(theme.accent).text_color(theme.over_accent)
        })
        .when(primary && !enabled, |this| {
            this.text_color(theme.text_disabled)
                .border_1()
                .border_color(theme.control_border)
        })
        .when(!primary && enabled, |this| {
            this.border_1()
                .border_color(theme.control_border)
                .text_color(theme.text)
                .hover(move |style| style.bg(hover))
                .active(move |style| style.bg(pressed))
        })
        .when(!primary && !enabled, |this| {
            this.text_color(theme.text_disabled)
                .border_1()
                .border_color(theme.control_border)
        })
        .child(label.to_owned())
}

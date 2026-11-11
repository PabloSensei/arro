//! The Run/Debug Configurations dialog: the stored configurations of the project, the
//! ones found in it, and the fields their providers describe. The dialog knows no
//! language: a provider reads and writes every field in the settings of a configuration.

use std::sync::Arc;

use project_model::ProjectModel;
use runner::{ConfigField, ConfigFieldKind, ConfigTemplate, RunConfig, RunConfigProvider};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::TextField;
use crate::run::RunPanel;

const WIDTH: f32 = 820.;
/// The height of the list and the fields together.
const BODY_HEIGHT: f32 = 360.;
const LIST_WIDTH: f32 = 240.;
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
    assemble_first: bool,
    rows: Vec<FieldRow>,
    /// `0` is the name, then one per row.
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
                assemble_first: true,
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
        self.assemble_first = true;
        self.rows.clear();
        self.active = 0;
        let Some(index) = self.selected else {
            return;
        };
        let Some(config) = self.items.get(index).map(|item| item.config.clone()) else {
            return;
        };
        self.name = TextField::from_text(config.name.clone());
        self.assemble_first = config.assemble_first;
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
        item.config.assemble_first = self.assemble_first;
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
        match event.keystroke.key.as_str() {
            "escape" => self.close(window, cx),
            "enter" => self.apply(true, window, cx),
            "space" if self.active > 0 => {
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
            _ => {
                if let Some(row) = self.rows.get_mut(self.active - 1) {
                    if row.field.kind == ConfigFieldKind::Bool {
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
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = (viewport.height - px(BODY_HEIGHT)) * 0.25;

        let items = self.items.iter().enumerate().map(|(index, item)| {
            let selected = self.selected == Some(index);
            let temporary = item.temporary;
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
                        .child(icon("run/run", &theme)),
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
        );
        let live = self.problem();
        let error = live.or_else(|| self.error.clone()).map(|error| {
            div()
                .debug_selector(|| "run-config-error".to_owned())
                .text_color(theme.text)
                .child(error)
        });

        let build_row = {
            let on = self.assemble_first;
            let (hover, pressed) = (theme.hover, theme.pressed);
            div()
                .id("run-config-assemble")
                .debug_selector(|| "run-config-assemble".to_owned())
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
                .child(div().text_color(theme.text).child("Build before run"))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.assemble_first = !this.assemble_first;
                    cx.notify();
                }))
        };

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

        let body = surface(&theme)
            .id("run-configs")
            .debug_selector(|| "run-configs".to_owned())
            .key_context("RunConfigs")
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .text_size(px(TEXT_TITLE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Run/Debug Configurations"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(DIALOG_GAP))
                    .h(px(BODY_HEIGHT))
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
                                .child(build_row)
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

        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("run-configs-enter", body))
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

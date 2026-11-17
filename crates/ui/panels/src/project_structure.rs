//! Project Structure, Ctrl+Alt+Shift+S: the SDK of the project and the folders it may
//! use, plus the language level of its build. The window knows no language: everything
//! it shows and changes comes from [`project_model::EnvironmentProvider`].

use std::path::PathBuf;
use std::sync::Arc;

use project_model::{
    EnvironmentDetail, EnvironmentProvider, ProjectEnvironmentInfo, RuntimeInfo, RuntimeReason,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::navigation;

const WIDTH: f32 = 760.;
const BODY_HEIGHT: f32 = 380.;
const SECTIONS_WIDTH: f32 = 160.;
const LABEL_WIDTH: f32 = 130.;
const FIELD_HEIGHT: f32 = 26.;

/// The environment of the project, for the windows that show and change it.
pub(crate) struct ProjectEnvironmentGlobal(pub Option<Arc<dyn EnvironmentProvider>>);

impl Global for ProjectEnvironmentGlobal {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Project,
    Sdks,
}

/// Ctrl+Alt+Shift+S shows the window.
pub fn bind_actions(workspace: WeakEntity<Workspace>, window: AnyWindowHandle, cx: &mut App) {
    cx.on_action(move |_: &workspace::ProjectStructure, cx| {
        let workspace = workspace.clone();
        // The window is being updated while its action runs, so the rest waits.
        cx.defer(move |cx| {
            let show = move |_: AnyView, window: &mut Window, cx: &mut App| {
                let open = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                    ProjectStructureDialog::show(workspace, window, cx)
                };
                workspace.update(cx, open).ok();
            };
            window.update(cx, show).ok();
        });
    });
}

/// The dialog over the workspace.
pub(crate) struct ProjectStructureDialog {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    environment: Option<Arc<dyn EnvironmentProvider>>,
    section: Section,
    runtimes: Vec<RuntimeInfo>,
    selected: Option<PathBuf>,
    reason: RuntimeReason,
    notice: Option<String>,
    language: Option<project_model::LanguageLevelInfo>,
    /// Read-only lines the language fills, such as a dependency it found.
    details: Vec<EnvironmentDetail>,
    /// The SDK list under the project field is open.
    sdk_open: bool,
    /// What went wrong with the last action, under the field that caused it.
    error: Option<String>,
    loading: bool,
    applying: bool,
    scroll: ScrollHandle,
}

impl ProjectStructureDialog {
    pub(crate) fn show(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let environment = cx
            .try_global::<ProjectEnvironmentGlobal>()
            .and_then(|global| global.0.clone());
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            environment: environment.clone(),
            section: Section::Project,
            runtimes: Vec::new(),
            selected: None,
            reason: RuntimeReason::Default,
            notice: None,
            language: None,
            details: Vec::new(),
            sdk_open: false,
            error: None,
            loading: environment.is_some(),
            applying: false,
            scroll: ScrollHandle::new(),
        });
        if let Some(environment) = environment {
            let read = cx.background_executor().spawn(async move {
                let info = environment.describe();
                let details = environment.details();
                (info, details)
            });
            let dialog = view.downgrade();
            cx.spawn(async move |_, cx| {
                let (info, details) = read.await;
                dialog
                    .update(cx, |dialog, cx| dialog.loaded(info, details, cx))
                    .ok();
            })
            .detach();
        }
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn loaded(
        &mut self,
        info: ProjectEnvironmentInfo,
        details: Vec<EnvironmentDetail>,
        cx: &mut Context<Self>,
    ) {
        self.runtimes = info.runtimes;
        self.selected = info.selected;
        self.reason = info.reason;
        self.notice = info.notice;
        self.language = info.language;
        self.details = details;
        self.loading = false;
        cx.notify();
    }

    fn selected_name(&self) -> String {
        self.selected
            .as_deref()
            .and_then(|home| self.runtimes.iter().find(|runtime| runtime.home == home))
            .map(|runtime| runtime.name.clone())
            .unwrap_or_else(|| "No SDK".to_owned())
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtimes.get(index) else {
            return;
        };
        self.selected = Some(runtime.home.clone());
        self.sdk_open = false;
        self.error = None;
        cx.notify();
    }

    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtimes.get(index) else {
            return;
        };
        if !runtime.added {
            return;
        }
        let removed = self.runtimes.remove(index);
        if self.selected.as_deref() == Some(removed.home.as_path()) {
            self.selected = self.runtimes.first().map(|runtime| runtime.home.clone());
        }
        self.error = None;
        cx.notify();
    }

    /// Adds a folder the user picked. A folder that is not an SDK is an error under the
    /// field, and nothing changes.
    pub(crate) fn add_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            self.error = Some("No language of this project can choose an SDK".to_owned());
            cx.notify();
            return;
        };
        let read = folder.clone();
        let task = cx
            .background_executor()
            .spawn(async move { environment.runtime_at(&read) });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| match outcome {
                Ok(runtime) => {
                    if !this.runtimes.iter().any(|known| known.home == runtime.home) {
                        this.runtimes.push(runtime.clone());
                    }
                    this.selected = Some(runtime.home.clone());
                    this.error = None;
                    cx.notify();
                }
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let chosen = paths.await.ok().and_then(Result::ok).flatten();
            if let Some(folder) = chosen.and_then(|paths| paths.into_iter().next()) {
                this.update(cx, |this, cx| this.add_folder(folder, cx)).ok();
            }
        })
        .detach();
    }

    fn open_build_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(language) = self.language.clone() else {
            return;
        };
        let Some(file) = language.file else {
            return;
        };
        let spot = language.line.map(|line| editor_view::Spot::Line {
            line: line as usize,
            column: 0,
        });
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                navigation::go_to(workspace, file, spot, true, window, cx);
            })
            .ok();
    }

    /// Stores the choice and the folders. OK closes the window when the write succeeded.
    fn apply(&mut self, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.applying || self.loading || self.error.is_some() {
            return;
        }
        let Some(environment) = self.environment.clone() else {
            self.error = Some("No language of this project can choose an SDK".to_owned());
            cx.notify();
            return;
        };
        let selected = self.selected.clone();
        let added: Vec<PathBuf> = self
            .runtimes
            .iter()
            .filter(|runtime| runtime.added)
            .map(|runtime| runtime.home.clone())
            .collect();
        self.applying = true;
        let task = cx.background_executor().spawn(async move {
            environment
                .apply(selected.as_deref(), &added)
                .map(|info| (info, environment.details()))
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| {
                this.applying = false;
                match outcome {
                    Ok((info, details)) => {
                        this.loaded(info, details, cx);
                        if close {
                            this.close(window, cx);
                        }
                    }
                    Err(error) => {
                        this.error = Some(error);
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// What went wrong with the last action, for the window tests.
    #[cfg(test)]
    pub(crate) fn error_text(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The SDKs the dialog shows, for the window tests.
    #[cfg(test)]
    pub(crate) fn runtime_count(&self) -> usize {
        self.runtimes.len()
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.close(window, cx),
            "enter" => self.apply(true, window, cx),
            _ => {}
        }
    }
}

fn reason_text(reason: RuntimeReason) -> &'static str {
    match reason {
        RuntimeReason::User => "Chosen in Project Structure",
        RuntimeReason::Project => "Requested by the project",
        RuntimeReason::Default => "The default SDK",
    }
}

fn section_row(
    id: &'static str,
    selector: &'static str,
    title: &'static str,
    chosen: bool,
    theme: &Theme,
    cx: &mut Context<ProjectStructureDialog>,
    section: Section,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || selector.to_owned())
        .flex()
        .items_center()
        .h(px(FIELD_HEIGHT))
        .px(px(ROW_PADDING_X))
        .rounded(px(CONTROL_RADIUS))
        .text_color(if chosen {
            theme.text
        } else {
            theme.text_secondary
        })
        .when(chosen, |this| this.bg(theme.selection))
        .when(!chosen, |this| {
            let hover = theme.hover;
            this.hover(move |style| style.bg(hover))
        })
        .child(title)
        .on_click(cx.listener(move |this, _, _, cx| {
            this.section = section;
            this.sdk_open = false;
            cx.notify();
        }))
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

/// A button of the dialog: the primary one is filled with the accent.
pub(crate) fn dialog_button(
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

impl Render for ProjectStructureDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = (viewport.height - px(BODY_HEIGHT)) * 0.2;

        let sections = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .w(px(SECTIONS_WIDTH))
            .flex_none()
            .child(section_row(
                "project-structure-section-project",
                "project-structure-section-project",
                "Project",
                self.section == Section::Project,
                &theme,
                cx,
                Section::Project,
            ))
            .child(section_row(
                "project-structure-section-sdks",
                "project-structure-section-sdks",
                "SDKs",
                self.section == Section::Sdks,
                &theme,
                cx,
                Section::Sdks,
            ));

        let body: AnyElement = match self.section {
            Section::Project => self.project_page(&theme, cx).into_any_element(),
            Section::Sdks => self.sdks_page(&theme, cx).into_any_element(),
        };

        let can_apply = !self.applying && !self.loading && self.error.is_none();
        let ok = dialog_button(
            "project-structure-ok",
            "project-structure-ok".to_owned(),
            "OK",
            true,
            can_apply,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.apply(true, window, cx)));
        let apply = dialog_button(
            "project-structure-apply",
            "project-structure-apply".to_owned(),
            "Apply",
            false,
            can_apply,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.apply(false, window, cx)));
        let cancel = dialog_button(
            "project-structure-cancel",
            "project-structure-cancel".to_owned(),
            "Cancel",
            false,
            true,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)));

        let error = self.error.clone().map(|error| {
            div()
                .debug_selector(|| "project-structure-error".to_owned())
                .text_color(theme.text)
                .child(error)
        });
        let notice = self.notice.clone().map(|notice| {
            div()
                .debug_selector(|| "project-structure-notice".to_owned())
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_secondary)
                .child(notice)
        });

        let panel = surface(&theme)
            .id("project-structure")
            .debug_selector(|| "project-structure".to_owned())
            .key_context("ProjectStructure")
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
                    .debug_selector(|| "project-structure-title".to_owned())
                    .text_size(px(TEXT_TITLE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child("Project Structure"),
            )
            .child(
                div()
                    .flex()
                    .gap(px(DIALOG_GAP))
                    .h(px(BODY_HEIGHT))
                    .child(sections)
                    .child(
                        scrollable(
                            "project-structure-body",
                            &self.scroll,
                            div()
                                .id("project-structure-body")
                                .flex()
                                .flex_col()
                                .gap(px(SPACE_M))
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .child(body)
                                .children(error)
                                .children(notice),
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
                    .child(cancel)
                    .child(apply)
                    .child(ok),
            );

        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("project-structure-enter", panel))
    }
}

impl ProjectStructureDialog {
    fn project_page(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let name = self.selected_name();
        let dropdown = div()
            .id("project-structure-sdk")
            .debug_selector(|| "project-structure-sdk".to_owned())
            .flex()
            .items_center()
            .h(px(FIELD_HEIGHT))
            .min_w(px(0.))
            .flex_1()
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if self.sdk_open {
                theme.accent
            } else {
                theme.control_border
            })
            .text_color(theme.text)
            .cursor_pointer()
            .child(name)
            .on_click(cx.listener(|this, _, _, cx| {
                this.sdk_open = !this.sdk_open;
                cx.notify();
            }));

        let items = self.sdk_open.then(|| {
            let rows = self.runtimes.iter().enumerate().map(|(index, runtime)| {
                let label = format!("{}  {}", runtime.name, runtime.version);
                let chosen = self.selected.as_deref() == Some(runtime.home.as_path());
                div()
                    .id(("project-structure-sdk-item", index))
                    .debug_selector(move || format!("project-structure-sdk-item-{index}"))
                    .flex()
                    .items_center()
                    .h(px(FIELD_HEIGHT))
                    .px(px(SPACE_M))
                    .text_color(if chosen {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(chosen, |this| this.bg(theme.selection))
                    .hover({
                        let hover = theme.hover;
                        move |style| style.bg(hover)
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(index, cx)))
            });
            let add = div()
                .id("project-structure-add-jdk")
                .debug_selector(|| "project-structure-add-jdk".to_owned())
                .flex()
                .items_center()
                .h(px(FIELD_HEIGHT))
                .px(px(SPACE_M))
                .text_color(theme.text)
                .hover({
                    let hover = theme.hover;
                    move |style| style.bg(hover)
                })
                .child("Add JDK...")
                .on_click(cx.listener(|this, _, window, cx| this.choose_folder(window, cx)));
            div()
                .flex()
                .flex_col()
                .border_1()
                .border_color(theme.control_border)
                .rounded(px(CONTROL_RADIUS))
                .children(rows)
                .child(add)
                .into_any_element()
        });

        let level = self
            .language
            .as_ref()
            .and_then(|language| language.selected.clone())
            .unwrap_or_else(|| "Not set".to_owned());
        let open = self
            .language
            .as_ref()
            .and_then(|language| language.file.as_ref())
            .map(|_| {
                small_button(
                    "project-structure-open-build",
                    "project-structure-open-build".to_owned(),
                    "Open Build File",
                    theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_build_file(window, cx)))
            });

        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_M))
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
                            .child("Project SDK"),
                    )
                    .child(dropdown),
            )
            .children(items)
            .child(
                div()
                    .debug_selector(|| "project-structure-reason".to_owned())
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child(reason_text(self.reason)),
            )
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
                            .child("Language level"),
                    )
                    .child(
                        div()
                            .debug_selector(|| "project-structure-language".to_owned())
                            .flex()
                            .items_center()
                            .h(px(FIELD_HEIGHT))
                            .px(px(SPACE_M))
                            .rounded(px(CONTROL_RADIUS))
                            .border_1()
                            .border_color(theme.control_border)
                            .child(level),
                    )
                    .children(open),
            )
            .child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child("The language level is edited in the build file."),
            )
            .children(self.details.iter().enumerate().map(|(index, detail)| {
                div()
                    .id(("project-structure-detail", index))
                    .debug_selector(move || format!("project-structure-detail-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .w(px(LABEL_WIDTH))
                            .flex_none()
                            .text_color(theme.text_muted)
                            .child(format!("{}:", detail.label)),
                    )
                    .child(
                        div()
                            .debug_selector(move || {
                                format!("project-structure-detail-value-{index}")
                            })
                            .child(detail.value.clone()),
                    )
            }))
    }

    fn sdks_page(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let rows = self.runtimes.iter().enumerate().map(|(index, runtime)| {
            let remove = runtime.added.then(|| {
                small_button(
                    ("project-structure-remove", index),
                    format!("project-structure-remove-{index}"),
                    "Remove",
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.remove(index, cx)))
            });
            div()
                .id(("project-structure-sdk-row", index))
                .debug_selector(move || format!("project-structure-sdk-row-{index}"))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(SPACE_M))
                .min_h(px(FIELD_HEIGHT))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.))
                        .child(
                            div()
                                .text_color(theme.text)
                                .child(format!("{} {}", runtime.name, runtime.version)),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_SMALL))
                                .text_color(theme.text_muted)
                                .child(runtime.home.display().to_string()),
                        ),
                )
                .children(remove)
        });
        let add = small_button(
            "project-structure-add",
            "project-structure-add".to_owned(),
            "Add",
            theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.choose_folder(window, cx)));
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child("SDKs of the project. Only the ones added here can be removed."),
            )
            .children(rows)
            .child(add)
    }
}

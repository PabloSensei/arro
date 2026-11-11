//! Small popups of the window: the file path, the navigation bar, the scheme switch
//! and the choice of a language for a scratch file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use editor_view::{EditorView, Spot};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{UI_SCALE_STEP, Workspace};

use crate::navigation;

const WIDTH: f32 = 520.;
const ROWS_HEIGHT: f32 = 320.;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(1);

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Ctrl+Alt+F12: the folders of the active file, from the file up to the drive root.
pub struct FilePathPopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    rows: Vec<(String, PathBuf)>,
    selected: usize,
}

pub(crate) fn show_file_path(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(file) = workspace.active_path() else {
        return;
    };
    let rows: Vec<(String, PathBuf)> = file
        .ancestors()
        .map(|path| (path.display().to_string(), path.to_path_buf()))
        .collect();
    if rows.is_empty() {
        return;
    }
    // The folder of the file is the useful place to open first.
    let selected = usize::from(rows.len() > 1);
    let weak = cx.weak_entity();
    let view = cx.new(|cx| FilePathPopup {
        workspace: weak,
        focus: cx.focus_handle(),
        rows,
        selected,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

impl FilePathPopup {
    fn pick(&mut self, index: usize, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = index;
        ui_kit::reveal_path(cx, &path);
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "up" | "left" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                cx.notify();
            }
            "down" | "right" => {
                self.selected = (self.selected + 1).min(self.rows.len() - 1);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                if let Some((_, path)) = self.rows.get(self.selected) {
                    ui_kit::reveal_path(cx, path);
                }
                self.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
                cx.stop_propagation();
            }
            "escape" => {
                self.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for FilePathPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.2;
        let rows: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, (label, path))| {
                let selected = index == self.selected;
                let path = path.clone();
                div()
                    .id(("file-path-row", index))
                    .debug_selector(move || format!("file-path-row-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(SharedString::from(label.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick(index, path.clone(), window, cx)
                    }))
            })
            .collect();
        let body = surface(&theme)
            .id("file-path-popup")
            .debug_selector(|| "file-path-popup".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_h(px(ROWS_HEIGHT))
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
            }))
            .child(
                div()
                    .px(px(ROW_PADDING_X))
                    .h(px(CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("File Path"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .overflow_hidden()
                    .children(rows),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("file-path-enter", body))
    }
}

/// One step of the navigation bar: a folder, the file, or a declaration of the file.
#[derive(Clone)]
struct NavRow {
    label: String,
    path: PathBuf,
    offset: Option<usize>,
    folder: bool,
}

/// Alt+Home: the chain of packages, the file, and the declaration at the caret.
pub struct NavBarPopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    rows: Vec<NavRow>,
    selected: usize,
}

pub(crate) fn show_nav_bar(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(file) = workspace.active_path() else {
        return;
    };
    let mut rows: Vec<NavRow> = file
        .ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|path| NavRow {
            label: file_name(path),
            path: path.to_path_buf(),
            offset: None,
            folder: path != file,
        })
        .collect();
    if let Some(view) = workspace
        .view_of(&file)
        .and_then(|view| view.downcast::<EditorView>().ok())
    {
        let view = view.read(cx);
        let caret = view.head_offset().unwrap_or(0);
        for item in view.outline() {
            if item.range.contains(&caret) {
                rows.push(NavRow {
                    label: item.name.clone(),
                    path: file.clone(),
                    offset: Some(item.range.start),
                    folder: false,
                });
            }
        }
    }
    if rows.is_empty() {
        return;
    }
    let selected = rows.len() - 1;
    let weak = cx.weak_entity();
    let view = cx.new(|cx| NavBarPopup {
        workspace: weak,
        focus: cx.focus_handle(),
        rows,
        selected,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

impl NavBarPopup {
    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        self.selected = index;
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                if row.folder {
                    ui_kit::reveal_path(cx, &row.path);
                } else if let Some(offset) = row.offset {
                    navigation::go_to(
                        workspace,
                        row.path.clone(),
                        Some(Spot::Offset(offset)),
                        false,
                        window,
                        cx,
                    );
                } else {
                    workspace.open_file(row.path.clone(), window, cx);
                }
            })
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "left" | "up" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                cx.notify();
            }
            "right" | "down" => {
                self.selected = (self.selected + 1).min(self.rows.len() - 1);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                self.pick(self.selected, window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for NavBarPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.12;
        let rows: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let selected = index == self.selected;
                div()
                    .id(("nav-bar-row", index))
                    .debug_selector(move || format!("nav-bar-row-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X + SPACE_L * row.folder as u32 as f32))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(SharedString::from(row.label.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(index, window, cx)))
            })
            .collect();
        let body = surface(&theme)
            .id("nav-bar-popup")
            .debug_selector(|| "nav-bar-popup".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_h(px(ROWS_HEIGHT))
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
            }))
            .child(
                div()
                    .px(px(ROW_PADDING_X))
                    .h(px(CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("Navigation Bar"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .overflow_hidden()
                    .children(rows),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("nav-bar-enter", body))
    }
}

/// Ctrl+Backquote: the theme, the keymap and the scale of the interface in one list.
pub struct ChangeSchemePopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    selected: usize,
    theme_dark: bool,
    scale: f32,
}

pub(crate) fn show_change_scheme(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let weak = cx.weak_entity();
    let theme_dark = cx.global::<Theme>().dark;
    let scale = workspace.ui_scale(cx);
    let view = cx.new(|cx| ChangeSchemePopup {
        workspace: weak,
        focus: cx.focus_handle(),
        selected: 0,
        theme_dark,
        scale,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

impl ChangeSchemePopup {
    fn rows(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "Theme",
                if self.theme_dark { "Dark" } else { "Light" }.to_owned(),
            ),
            ("Keymap", "IntelliJ IDEA (Windows)".to_owned()),
            ("Scale", format!("{}%", (self.scale * 100.).round())),
        ]
    }

    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = index;
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                match index {
                    0 => workspace.toggle_theme(cx),
                    1 => workspace.notify_user(
                        "Keymap",
                        "IntelliJ IDEA (Windows) is the only keymap",
                        cx,
                    ),
                    _ => workspace.zoom_ide(UI_SCALE_STEP, cx),
                }
            })
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "up" | "left" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                cx.notify();
            }
            "down" | "right" => {
                self.selected = (self.selected + 1).min(2);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                self.pick(self.selected, window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for ChangeSchemePopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.18;
        let rows: Vec<_> = self
            .rows()
            .into_iter()
            .enumerate()
            .map(|(index, (title, value))| {
                let selected = index == self.selected;
                div()
                    .id(("scheme-row", index))
                    .debug_selector(move || format!("scheme-row-{index}"))
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(title)
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(value)),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(index, window, cx)))
            })
            .collect();
        let body = surface(&theme)
            .id("scheme-popup")
            .debug_selector(|| "scheme-popup".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
            }))
            .child(
                div()
                    .px(px(ROW_PADDING_X))
                    .h(px(CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("Scheme"),
            )
            .children(rows);
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("scheme-enter", body))
    }
}

/// Ctrl+Alt+Shift+Insert: the language of a new scratch file, from the running servers.
pub struct ScratchLanguagePopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    rows: Vec<(String, &'static str)>,
    selected: usize,
}

pub(crate) fn show_scratch_languages(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let mut rows = vec![("Plain text".to_owned(), "txt")];
    if let Some(languages) = cx.try_global::<crate::AgentLanguages>() {
        for id in languages.0.ids() {
            rows.push((id.0.to_owned(), id.0));
        }
    }
    let weak = cx.weak_entity();
    let view = cx.new(|cx| ScratchLanguagePopup {
        workspace: weak,
        focus: cx.focus_handle(),
        rows,
        selected: 0,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

impl ScratchLanguagePopup {
    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some((_, extension)) = self.rows.get(index).cloned() else {
            return;
        };
        self.selected = index;
        let serial = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                let dir = workspace.state_dir().join("scratches");
                let path = dir.join(format!("scratch_{serial}.{extension}"));
                let write = cx.background_executor().spawn(async move {
                    std::fs::create_dir_all(&dir)?;
                    std::fs::write(&path, "")?;
                    Ok::<PathBuf, std::io::Error>(path)
                });
                cx.spawn_in(window, async move |workspace, cx| {
                    let Ok(path) = write.await else {
                        return;
                    };
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_file(path, window, cx)
                        })
                        .ok();
                })
                .detach();
            })
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "up" | "left" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                cx.notify();
            }
            "down" | "right" => {
                self.selected = (self.selected + 1).min(self.rows.len() - 1);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                self.pick(self.selected, window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for ScratchLanguagePopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.18;
        let rows: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, (label, _))| {
                let selected = index == self.selected;
                div()
                    .id(("scratch-language-row", index))
                    .debug_selector(move || format!("scratch-language-row-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(SharedString::from(label.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(index, window, cx)))
            })
            .collect();
        let body = surface(&theme)
            .id("scratch-language-popup")
            .debug_selector(|| "scratch-language-popup".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_h(px(ROWS_HEIGHT))
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
            }))
            .child(
                div()
                    .px(px(ROW_PADDING_X))
                    .h(px(CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("New Scratch File"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .overflow_hidden()
                    .children(rows),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("scratch-language-enter", body))
    }
}

/// The editor of the active tab, when it has one.
fn active_editor(workspace: &Workspace, _cx: &App) -> Option<Entity<EditorView>> {
    workspace.active_view()?.downcast::<EditorView>().ok()
}

/// The menu of the editor at the caret, for the Menu key.
pub(crate) fn show_editor_menu(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(view) = active_editor(workspace, cx) else {
        return;
    };
    let position = view.read(cx).context_menu_position(cx);
    let model = crate::editor_menu::MenuModel::of(workspace);
    let weak = cx.weak_entity();
    crate::editor_menu::show_now(workspace, view, weak, model, position, cx);
}

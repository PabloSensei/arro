//! Alt+F1, Select In: a popup in the middle of the window that lists the places the open file
//! can be shown in. A row runs the action of its place; a digit key or Enter picks a row and
//! Escape closes the popup. Only the places that apply to the file are listed.

use std::path::Path;

use keymap::Platform;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{COMMIT_TOOL, STRUCTURE_TOOL, SelectIn, SelectInProject, TERMINAL_TOOL, Workspace};

use crate::git::ActiveGit;

/// The width of the popup.
const WIDTH: f32 = 360.;

/// The places the popup lists, in the order of IDEA.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Place {
    ProjectView,
    Structure,
    Explorer,
    Terminal,
    Commit,
    GitLog,
}

/// The name of the row that shows the file in the file manager of the system.
pub fn reveal_row_name(platform: Platform) -> &'static str {
    match platform {
        Platform::Windows => "Explorer",
        Platform::MacOs => "Finder",
        Platform::Linux => "File Manager",
    }
}

fn label(place: Place, platform: Platform) -> &'static str {
    match place {
        Place::ProjectView => "Project View",
        Place::Structure => "Structure",
        Place::Explorer => reveal_row_name(platform),
        Place::Terminal => "Terminal",
        Place::Commit => "Commit",
        Place::GitLog => "Git Log",
    }
}

/// The places that apply to the file. Explorer needs the file on disk and Terminal a folder
/// of its own. Commit needs a file that Git sees as changed, and Git Log a repository.
fn places_for(path: &Path, cx: &App) -> Vec<Place> {
    let mut places = vec![Place::ProjectView, Place::Structure];
    if path.exists() {
        places.push(Place::Explorer);
    }
    if path.parent().is_some_and(|folder| folder.is_dir()) {
        places.push(Place::Terminal);
    }
    if let Some(git) = cx.try_global::<ActiveGit>() {
        let git = git.0.read(cx);
        if git.repo().is_some() {
            if git.kind_of(path).is_some() {
                places.push(Place::Commit);
            }
            places.push(Place::GitLog);
        }
    }
    places
}

/// Runs the action of a place for the file. The windows that a row opens are shown, not
/// toggled, so that a row never closes the window it was chosen to open.
fn run(
    place: Place,
    path: &Path,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    match place {
        Place::ProjectView => window.dispatch_action(Box::new(SelectInProject), cx),
        Place::Structure => workspace.show_tool(STRUCTURE_TOOL, window, cx),
        Place::Explorer => ui_kit::reveal_path(cx, path),
        Place::Terminal => {
            if let Some(folder) = path.parent() {
                crate::terminal_panel::open_tab_in_directory(folder.to_path_buf(), window, cx);
            }
            workspace.show_tool(TERMINAL_TOOL, window, cx);
        }
        Place::Commit => workspace.show_tool(COMMIT_TOOL, window, cx),
        Place::GitLog => crate::git_log::show_file(path.to_path_buf(), workspace, window, cx),
    }
}

/// Shows the Select In popup of the open file. A window without an open editor has no file
/// to select, so nothing opens.
pub(crate) fn show(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some((path, _)) = workspace.active_editor() else {
        return;
    };
    let platform = keymap::platform();
    // Each row keeps its own copy of the path and a weak handle of the workspace, so that it
    // runs on its own.
    let entries: Vec<MenuEntry> = places_for(&path, cx)
        .into_iter()
        .map(|place| {
            let (weak, path) = (cx.weak_entity(), path.clone());
            MenuEntry::new(label(place, platform), move |window, cx| {
                weak.update(cx, |workspace, cx| run(place, &path, workspace, window, cx))
                    .ok();
            })
        })
        .collect();
    let weak = cx.weak_entity();
    let view = cx.new(|cx| SelectInPopup {
        workspace: weak,
        focus: cx.focus_handle(),
        entries: entries.clone(),
        selected: 0,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay_with_rows(view.into(), focus, Some(entries), window, cx);
}

/// The row a digit key picks: `1` is the first row, `9` the ninth.
fn digit_row(key: &str) -> Option<usize> {
    match key.as_bytes() {
        [digit @ b'1'..=b'9'] => Some(usize::from(digit - b'1')),
        _ => None,
    }
}

/// The popup of Select In: a header and a column of rows, numbered from one.
struct SelectInPopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    /// The rows of the popup. The window reports the same rows to the checks of the menus.
    entries: Vec<MenuEntry>,
    selected: usize,
}

impl SelectInPopup {
    /// Runs the row once the popup has closed. The row then finds no popup open, and the
    /// focus it moves goes to the window that the row opens, not back to the editor.
    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(run) = self
            .entries
            .get(index)
            .and_then(|entry| entry.on_click.clone())
        else {
            return;
        };
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
        let handle = window.window_handle();
        cx.defer(move |cx| {
            handle.update(cx, |_, window, cx| run(window, cx)).ok();
        });
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if modifiers.control || modifiers.alt || modifiers.platform {
            return;
        }
        let last = self.entries.len().saturating_sub(1);
        match key {
            "up" => {
                self.selected = self.selected.saturating_sub(1);
                cx.notify();
            }
            "down" => {
                self.selected = (self.selected + 1).min(last);
                cx.notify();
            }
            "enter" => {
                let selected = self.selected;
                self.pick(selected, window, cx);
            }
            "escape" => {
                self.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
            }
            _ => {
                let Some(index) = digit_row(key).filter(|index| *index < self.entries.len()) else {
                    return;
                };
                self.pick(index, window, cx);
            }
        }
        cx.stop_propagation();
    }
}

impl Render for SelectInPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.2;
        let rows: Vec<_> = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let selected = index == self.selected;
                let hover = theme.selection_hover;
                div()
                    .id(("select-in-row", index))
                    .debug_selector(move || format!("select-in-row-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .gap(px(SPACE_M))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| this.hover(move |style| style.bg(hover)))
                    .child(
                        div()
                            .w(px(SPACE_L))
                            .text_color(theme.text_secondary)
                            .child(format!("{}", index + 1)),
                    )
                    .child(entry.label.clone())
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(index, window, cx)))
            })
            .collect();
        let body = surface(&theme)
            .id("select-in-popup")
            .debug_selector(|| "select-in-popup".to_owned())
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
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .font_weight(FontWeight::MEDIUM)
                    .child("Select In"),
            )
            .child(div().flex().flex_col().children(rows));
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("select-in-enter", body))
    }
}

/// Alt+F1: the Select In popup of the open file. The window is being updated while its
/// action runs, so the popup waits until that update is over.
pub fn bind_actions(workspace: WeakEntity<Workspace>, window: AnyWindowHandle, cx: &mut App) {
    cx.on_action(move |_: &SelectIn, cx| {
        let workspace = workspace.clone();
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    workspace.update(cx, |this, cx| show(this, window, cx)).ok();
                })
                .ok();
        });
    });
}

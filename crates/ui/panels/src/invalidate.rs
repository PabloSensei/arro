//! File > Invalidate Caches: the caches the IDE keeps for the project, the ones the user
//! picks, and a restart of the project window after they go.

use std::path::PathBuf;

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::project_structure::dialog_button;
use crate::script::ActiveRunPanel;

const WIDTH: f32 = 520.;

/// Cache folders the application adds to the ones the panels know themselves. The window
/// knows no language: the folder of the language server comes from the application.
#[derive(Clone, Default)]
pub struct CacheLocations {
    pub server: Option<PathBuf>,
}

impl Global for CacheLocations {}

struct CacheItem {
    label: &'static str,
    path: PathBuf,
    /// Shared by every project: off until the user asks for it.
    checked: bool,
}

pub(crate) struct InvalidateDialog {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    items: Vec<CacheItem>,
    checked: Vec<bool>,
    error: Option<String>,
}

impl InvalidateDialog {
    pub(crate) fn show(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let cache_root = cx
            .try_global::<ActiveRunPanel>()
            .map(|panel| panel.0.read(cx).cache_root().to_path_buf())
            .unwrap_or_else(|| workspace.state_dir().join("projects"));
        let model = project_model::cache_file(&cache_root, workspace.root());
        let server = cx
            .try_global::<CacheLocations>()
            .and_then(|locations| locations.server.clone());
        let mut items = Vec::new();
        if let Some(folder) = model.parent() {
            items.push(CacheItem {
                label: "Project model cache",
                path: folder.to_path_buf(),
                checked: true,
            });
        }
        if let Some(server) = server {
            items.push(CacheItem {
                label: "Language server workspace",
                path: server,
                checked: true,
            });
        }
        items.push(CacheItem {
            label: "Local history of all projects",
            path: workspace.state_dir().join("history"),
            checked: false,
        });
        let checked = items.iter().map(|item| item.checked).collect();
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            items,
            checked,
            error: None,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(checked) = self.checked.get_mut(index) {
            *checked = !*checked;
            cx.notify();
        }
    }

    /// Leaves the chosen caches for the next start and restarts the project: the running
    /// language server holds its folder, so nothing is deleted while it lives. `invalidate`
    /// false is Just Restart.
    fn restart(&mut self, invalidate: bool, window: &mut Window, cx: &mut Context<Self>) {
        let paths: Vec<PathBuf> = self
            .items
            .iter()
            .zip(&self.checked)
            .filter(|(_, checked)| invalidate && **checked)
            .map(|(item, _)| item.path.clone())
            .collect();
        let Ok(state_dir) = self
            .workspace
            .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        else {
            return;
        };
        if !paths.is_empty()
            && let Err(error) = workspace::schedule_invalidation(&state_dir, &paths)
        {
            self.error = Some(format!("The caches are not invalidated: {error}"));
            cx.notify();
            return;
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                workspace.restart_project(cx);
            })
            .ok();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.close(window, cx),
            "enter" => self.restart(true, window, cx),
            _ => {}
        }
    }
}

impl Render for InvalidateDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.25;

        let rows = self
            .items
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let on = self.checked[index];
                div()
                    .id(("invalidate-item", index))
                    .debug_selector(move || format!("invalidate-item-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .h(px(CONTROL_HEIGHT))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
                    .child(checkbox(on, &theme))
                    .child(div().child(item.label))
            })
            .collect::<Vec<_>>();

        let actions = div()
            .flex()
            .justify_end()
            .gap(px(SPACE_M))
            .child(
                dialog_button(
                    "invalidate-cancel",
                    "invalidate-cancel".to_owned(),
                    "Cancel",
                    false,
                    true,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            )
            .child(
                dialog_button(
                    "invalidate-just-restart",
                    "invalidate-just-restart".to_owned(),
                    "Just Restart",
                    false,
                    true,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.restart(false, window, cx))),
            )
            .child(
                dialog_button(
                    "invalidate-restart",
                    "invalidate-restart".to_owned(),
                    "Invalidate and Restart",
                    true,
                    true,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.restart(true, window, cx))),
            );

        let error = self.error.clone().map(|error| {
            div()
                .debug_selector(|| "invalidate-error".to_owned())
                .text_color(theme.text)
                .child(error)
        });

        div().absolute().left(left).top(top).child(
            surface(&theme)
                .id("invalidate-caches")
                .debug_selector(|| "invalidate-caches".to_owned())
                .key_context("InvalidateCaches")
                .track_focus(&self.focus)
                .occlude()
                .w(width)
                .p(px(DIALOG_PADDING))
                .gap(px(DIALOG_GAP))
                .text_size(px(UI_SIZE))
                .text_color(theme.text)
                .on_key_down(
                    cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        this.key(event, window, cx)
                    }),
                )
                .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
                .child(
                    div()
                        .text_size(px(TEXT_TITLE))
                        .font_weight(FontWeight::SEMIBOLD)
                        .child("Invalidate Caches"),
                )
                .child(
                    div()
                        .text_color(theme.text_secondary)
                        .child("The project restarts; the chosen caches are deleted before it opens and rebuilt after."),
                )
                .children(rows)
                .children(error)
                .child(actions),
        )
    }
}

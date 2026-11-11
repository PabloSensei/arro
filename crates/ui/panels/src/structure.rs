//! The Structure tool window: the declarations of the active file as a tree. A click goes
//! to the declaration.

use std::path::PathBuf;

use editor_view::{EditorView, OutlineItem, Spot};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{STRUCTURE_TOOL, Workspace};

use crate::navigation;
use crate::outline::{OpenEditors, icon_of};

const GROUP: &str = "left-panel";
const ROW_MARGIN: f32 = SPACE_L;

pub struct StructurePanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    path: Option<PathBuf>,
    editor: Option<WeakEntity<EditorView>>,
    /// The row the keyboard moved to; `None` follows the caret of the editor.
    selected: Option<usize>,
    scroll: ScrollHandle,
    _editor: Option<Subscription>,
}

/// Lets the window say which file is active.
pub struct ActiveStructure(pub Entity<StructurePanel>);

impl Global for ActiveStructure {}

impl StructurePanel {
    pub fn create(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            workspace,
            focus: cx.focus_handle(),
            path: None,
            editor: None,
            selected: None,
            scroll: ScrollHandle::new(),
            _editor: None,
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The file the panel shows, now the active one.
    pub fn show(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        let editor = OpenEditors::get(cx, &path);
        self._editor = editor
            .as_ref()
            .map(|editor| cx.observe(editor, |_, _, cx| cx.notify()));
        self.editor = editor.map(|editor| editor.downgrade());
        if self.path.as_ref() != Some(&path) {
            self.selected = None;
        }
        self.path = Some(path);
        cx.notify();
    }

    /// The names of the rows, indented, for scripts.
    pub fn lines(&self, cx: &App) -> Vec<String> {
        let Some(editor) = self.editor.as_ref().and_then(WeakEntity::upgrade) else {
            return Vec::new();
        };
        editor
            .read(cx)
            .outline()
            .iter()
            .map(|item| format!("{}{}", "  ".repeat(item.depth as usize), item.label()))
            .collect()
    }

    fn go(&mut self, item: &OutlineItem, window: &mut Window, cx: &mut Context<Self>) {
        let Some(path) = self.path.clone() else {
            return;
        };
        let spot = Spot::Offset(item.name_range.start);
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        // The window is being updated while the panel handles the key or the click, so the
        // move waits until it is free again.
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            navigation::go_to(workspace, path, Some(spot), true, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    fn outline(&self, cx: &App) -> Vec<OutlineItem> {
        self.editor
            .as_ref()
            .and_then(WeakEntity::upgrade)
            .map(|editor| editor.read(cx).outline().to_vec())
            .unwrap_or_default()
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.outline(cx).len();
        if count == 0 {
            return;
        }
        let last = count as isize - 1;
        let next = match self.selected {
            Some(index) => (index as isize + delta).clamp(0, last),
            None if delta < 0 => last,
            None => 0,
        };
        self.selected = Some(next as usize);
        cx.notify();
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let Some(item) = self.outline(cx).get(index).cloned() else {
            return;
        };
        self.go(&item, window, cx);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = &event.keystroke;
        if key.modifiers.control && !key.modifiers.alt && key.key == "c" {
            if let Some(item) = self
                .selected
                .and_then(|index| self.outline(cx).get(index).cloned())
            {
                cx.write_to_clipboard(ClipboardItem::new_string(item.name));
            }
            cx.stop_propagation();
            return;
        }
        match key.key.as_str() {
            "up" => {
                cx.stop_propagation();
                self.move_selection(-1, cx);
            }
            "down" => {
                cx.stop_propagation();
                self.move_selection(1, cx);
            }
            "enter" | "f4" => {
                cx.stop_propagation();
                self.open_selected(window, cx);
            }
            _ => {}
        }
    }
}

impl Render for StructurePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(STRUCTURE_TOOL, window, cx)
                })
                .ok();
        };
        let editor = self.editor.as_ref().and_then(WeakEntity::upgrade);
        let (items, current) = match &editor {
            Some(editor) => {
                let view = editor.read(cx);
                let current = view.enclosing().last().map(|item| item.name_range.start);
                (view.outline().to_vec(), current)
            }
            None => (Vec::new(), None),
        };
        let empty = items.is_empty();
        let selected_row = self.selected;
        let rows: Vec<_> =
            items
                .into_iter()
                .enumerate()
                .map(|(index, item)| {
                    let selected = selected_row
                        .map_or(current == Some(item.name_range.start), |row| row == index);
                    let row = tree_row(
                        TreeRow {
                            id: ("structure-row", index).into(),
                            depth: item.depth as usize,
                            label: item.label().to_owned().into(),
                            hint: None,
                            icon: icon_of(item.kind),
                            expandable: false,
                            open: false,
                            selected,
                        },
                        &theme,
                    );
                    div().w_full().px(px(ROW_MARGIN)).child(row.on_click(
                        cx.listener(move |this, _, window, cx| this.go(&item, window, cx)),
                    ))
                })
                .collect();
        island(&theme)
            .id("structure-panel")
            .key_context("StructurePanel")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(panel_header("Structure", GROUP, hide, &theme))
            .child(
                scrollable(
                    "structure-rows",
                    &self.scroll,
                    div()
                        .id("structure-rows")
                        .flex()
                        .flex_col()
                        .pt(px(SPACE_S))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows)
                        .when(empty, |this| {
                            this.child(
                                div()
                                    .px(px(ROW_MARGIN + SPACE_M))
                                    .text_color(theme.text_secondary)
                                    .child("No declarations"),
                            )
                        }),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
    }
}

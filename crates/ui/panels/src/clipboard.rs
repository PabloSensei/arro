//! Ctrl+Shift+V: what was copied in the window, newest first. A pick is pasted into the
//! editor that asked.

use editor_view::EditorView;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

const WIDTH: f32 = 560.;
const HEIGHT: f32 = 360.;
/// Characters of the first line shown for an entry.
const PREVIEW: usize = 90;

pub struct ClipboardPopup {
    workspace: WeakEntity<Workspace>,
    editor: WeakEntity<EditorView>,
    focus: FocusHandle,
    entries: Vec<String>,
    selected: usize,
}

/// The text now on the system clipboard comes first when the window did not copy it.
fn entries_of(cx: &App) -> Vec<String> {
    let mut entries = editor_view::clipboard_history(cx);
    let current = cx.read_from_clipboard().and_then(|item| item.text());
    if let Some(current) = current.filter(|text| !text.is_empty() && !entries.contains(text)) {
        entries.insert(0, current);
    }
    entries
}

impl ClipboardPopup {
    pub fn show(
        editor: Entity<EditorView>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let entries = entries_of(cx);
        if entries.is_empty() {
            workspace.notify_user("Paste", "Nothing was copied yet", cx);
            return;
        }
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            workspace: weak,
            editor: editor.downgrade(),
            focus: cx.focus_handle(),
            entries,
            selected: 0,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(text) = self.entries.get(index).cloned() else {
            return;
        };
        self.dismiss(window, cx);
        if let Some(editor) = self.editor.upgrade() {
            window.focus(&editor.focus_handle(cx), cx);
            editor.update(cx, |editor, cx| editor.paste_text(&text, cx));
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let last = self.entries.len().saturating_sub(1);
        match event.keystroke.key.as_str() {
            "up" => self.selected = self.selected.saturating_sub(1),
            "down" => self.selected = (self.selected + 1).min(last),
            "enter" => return self.pick(self.selected, window, cx),
            "escape" => return self.dismiss(window, cx),
            digit if digit.len() == 1 && digit.as_bytes()[0].is_ascii_digit() => {
                let number = usize::from(digit.as_bytes()[0] - b'0');
                if number >= 1 {
                    return self.pick(number - 1, window, cx);
                }
            }
            _ => return,
        }
        cx.stop_propagation();
        cx.notify();
    }
}

fn preview(text: &str) -> String {
    let first = text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("");
    let mut shown: String = first.trim().chars().take(PREVIEW).collect();
    if first.trim().chars().count() > PREVIEW {
        shown.push('\u{2026}');
    }
    let more = text.lines().count().saturating_sub(1);
    if more > 0 {
        shown.push_str(&format!("  (+{more} lines)"));
    }
    shown
}

impl Render for ClipboardPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.205;
        let rows: Vec<_> = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, text)| {
                let chosen = index == self.selected;
                div()
                    .id(("clipboard-row", index))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .gap(px(SPACE_M))
                    .rounded(px(ROW_RADIUS))
                    .when(chosen, |this| this.bg(theme.selection))
                    .child(div().w(px(16.)).text_color(theme.text_secondary).child(
                        SharedString::from(if index < 9 {
                            (index + 1).to_string()
                        } else {
                            String::new()
                        }),
                    ))
                    .child(div().flex_1().min_w_0().truncate().child(preview(text)))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(index, window, cx)))
            })
            .collect();
        let body = surface(&theme)
            .id("clipboard-popup")
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_h(px(HEIGHT))
            .p(px(POPUP_PADDING))
            .flex()
            .flex_col()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
            .child(
                div()
                    .px(px(ROW_PADDING_X))
                    .h(px(CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("Choose content to paste"),
            )
            .children(rows);
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("clipboard-enter", body))
    }
}

#[cfg(test)]
mod checks {
    use super::preview;

    #[test]
    fn an_entry_shows_its_first_line_and_the_rest_as_a_count() {
        assert_eq!(preview("\n  one\ntwo\nthree"), "one  (+3 lines)");
        assert_eq!(preview("short"), "short");
    }

    #[test]
    fn a_long_line_is_cut() {
        let long = "x".repeat(200);
        assert_eq!(preview(&long).chars().count(), 91);
    }
}

//! The Keymap page of Settings: the actions with their shortcuts, a search by name and
//! by a pressed shortcut, and the edits that go to `keymap.json`. A changed action is
//! marked as IDEA marks it, and a shortcut that another action already runs asks what to
//! do with that action.

use keymap::{Keystroke, Shortcut};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::TextField;
use crate::settings::SettingsView;

/// A keyboard shortcut as the page writes it: `Ctrl+Shift+N`, `Ctrl+K, Ctrl+C`, and the
/// glyphs of macOS.
fn present(shortcut: &Shortcut) -> String {
    keymap::shortcut_label(&shortcut.to_string(), keymap::platform())
}

/// The last part of an action id, as one debug selector: `keymap-row-editor-copy`.
fn selector_of(prefix: &str, action: &str) -> String {
    let action = action.replace(['.', ':'], "-");
    format!("{prefix}-{action}")
}

/// One action of the page.
#[derive(Clone)]
struct KeyRow {
    action: &'static str,
    title: String,
    shortcuts: Vec<Shortcut>,
    changed: bool,
}

impl KeyRow {
    fn matches(&self, query: &str, shortcut: Option<&Shortcut>) -> bool {
        if let Some(shortcut) = shortcut
            && !self.shortcuts.iter().any(|own| own == shortcut)
        {
            return false;
        }
        let text = format!(
            "{} {}",
            self.title,
            self.shortcuts
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" ")
        )
        .to_lowercase();
        query
            .split_whitespace()
            .all(|word| text.contains(&word.to_lowercase()))
    }
}

#[derive(Clone)]
enum CaptureKind {
    Add(&'static str),
    Find,
}

#[derive(Clone)]
struct Capture {
    kind: CaptureKind,
    keys: Vec<Keystroke>,
}

struct Conflict {
    action: &'static str,
    shortcut: Shortcut,
    others: Vec<&'static str>,
}

pub struct KeymapPage {
    workspace: WeakEntity<Workspace>,
    search: TextField,
    /// The shortcut Find by Shortcut asks about.
    shortcut_search: Option<Shortcut>,
    /// What a popup is listening for.
    capture: Option<Capture>,
    conflict: Option<Conflict>,
    reset_all: bool,
    /// The action whose row menu is open.
    menu: Option<&'static str>,
    message: Option<String>,
}

impl KeymapPage {
    pub fn new(workspace: WeakEntity<Workspace>) -> Self {
        Self {
            workspace,
            search: TextField::default(),
            shortcut_search: None,
            capture: None,
            conflict: None,
            reset_all: false,
            menu: None,
            message: None,
        }
    }

    /// Escape inside the page: a capture, a conflict, a menu or a shortcut search closes
    /// first, and the window only after them.
    pub(crate) fn escape(&mut self, cx: &mut Context<SettingsView>) -> bool {
        if self.capture.is_some() {
            self.capture = None;
            cx.notify();
            return true;
        }
        if self.conflict.is_some() || self.reset_all {
            self.conflict = None;
            self.reset_all = false;
            cx.notify();
            return true;
        }
        if self.menu.is_some() {
            self.menu = None;
            cx.notify();
            return true;
        }
        if self.shortcut_search.is_some() {
            self.shortcut_search = None;
            cx.notify();
            return true;
        }
        false
    }

    /// Takes a key while the page or its popup is on screen. Returns whether the key
    /// belonged to it.
    pub fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<SettingsView>) -> bool {
        if self.capture.is_some() {
            self.capture_key(event, cx);
            return true;
        }
        if self.conflict.is_some() || self.reset_all {
            match event.keystroke.key.as_str() {
                "escape" => {
                    self.conflict = None;
                    self.reset_all = false;
                    cx.notify();
                }
                "enter" if self.reset_all => self.reset_everything(cx),
                "enter" => {
                    self.keep_conflict(cx);
                }
                _ => {}
            }
            return true;
        }
        if self.search.key_with_clipboard(event, false, cx) {
            cx.notify();
            return true;
        }
        if event.keystroke.key == "escape" && self.menu.is_some() {
            self.menu = None;
            cx.notify();
            return true;
        }
        if event.keystroke.key == "escape" && self.shortcut_search.is_some() {
            self.shortcut_search = None;
            cx.notify();
            return true;
        }
        false
    }

    fn capture_key(&mut self, event: &KeyDownEvent, cx: &mut Context<SettingsView>) {
        let Some(capture) = self.capture.as_mut() else {
            return;
        };
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        match key {
            "escape" => {
                self.capture = None;
            }
            "enter" if !capture.keys.is_empty() => {
                self.commit_capture(cx);
            }
            "backspace" if !(modifiers.control || modifiers.alt || modifiers.shift) => {
                capture.keys.pop();
            }
            "shift" | "control" | "alt" | "platform" | "windows" | "command" => {}
            _ => {
                if capture.keys.len() >= 2 {
                    capture.keys.clear();
                }
                capture.keys.push(Keystroke {
                    ctrl: modifiers.control,
                    alt: modifiers.alt,
                    shift: modifiers.shift,
                    cmd: modifiers.platform,
                    key: key.to_lowercase(),
                });
            }
        }
        cx.notify();
    }

    fn commit_capture(&mut self, cx: &mut Context<SettingsView>) {
        let Some(capture) = self.capture.take() else {
            return;
        };
        if capture.keys.is_empty() {
            return;
        }
        let shortcut = Shortcut::Keys(capture.keys);
        match capture.kind {
            CaptureKind::Find => {
                self.shortcut_search = Some(shortcut);
                self.message = None;
            }
            CaptureKind::Add(action) => {
                let conflicts = self
                    .workspace
                    .upgrade()
                    .map(|workspace| workspace.read(cx).keymap_conflicts(action, &shortcut))
                    .unwrap_or_default();
                if conflicts.is_empty() {
                    self.workspace
                        .update(cx, |workspace, cx| {
                            workspace.add_keymap_shortcut(action, &shortcut, cx);
                        })
                        .ok();
                    self.message = None;
                } else {
                    self.conflict = Some(Conflict {
                        action,
                        shortcut,
                        others: conflicts,
                    });
                }
            }
        }
        cx.notify();
    }

    /// Leave keeps the shortcuts as they were: the other actions keep the key.
    fn keep_conflict(&mut self, cx: &mut Context<SettingsView>) {
        self.conflict = None;
        cx.notify();
    }

    /// Remove Other takes the key away from the actions that had it.
    fn strip_others(&mut self, cx: &mut Context<SettingsView>) {
        let Some(conflict) = self.conflict.take() else {
            return;
        };
        let Conflict {
            action,
            shortcut,
            others,
        } = conflict;
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.assign_keymap_shortcut(action, &shortcut, &others, cx);
            })
            .ok();
        self.message = None;
        cx.notify();
    }

    fn open_capture(&mut self, kind: CaptureKind, cx: &mut Context<SettingsView>) {
        self.capture = Some(Capture {
            kind,
            keys: Vec::new(),
        });
        self.conflict = None;
        self.reset_all = false;
        self.menu = None;
        self.message = None;
        cx.notify();
    }

    fn remove(&mut self, action: &'static str, shortcut: Shortcut, cx: &mut Context<SettingsView>) {
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.remove_keymap_shortcut(action, &shortcut, cx);
            })
            .ok();
        cx.notify();
    }

    fn reset_action(&mut self, action: &'static str, cx: &mut Context<SettingsView>) {
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.reset_keymap_action(action, cx);
            })
            .ok();
        self.message = None;
        cx.notify();
    }

    fn reset_everything(&mut self, cx: &mut Context<SettingsView>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.reset_keymap(cx))
            .ok();
        self.reset_all = false;
        self.message = None;
        cx.notify();
    }

    fn rows(&self, cx: &App) -> Vec<KeyRow> {
        let Some(workspace) = self.workspace.upgrade() else {
            return Vec::new();
        };
        let workspace = workspace.read(cx);
        let keymap = workspace.keymap();
        let file = workspace.keymap_file();
        let mut rows: Vec<KeyRow> = workspace
            .commands()
            .all()
            .filter(|command| workspace.keymap_has_action(command.id))
            .map(|command| KeyRow {
                action: command.id,
                title: command.title.clone(),
                shortcuts: keymap.shortcuts_for(command.id),
                changed: file.is_customized(command.id),
            })
            .collect();
        rows.sort_by(|left, right| {
            left.title
                .to_lowercase()
                .cmp(&right.title.to_lowercase())
                .then(left.action.cmp(right.action))
        });
        rows
    }

    pub fn render(&mut self, _window: &mut Window, cx: &mut Context<SettingsView>) -> Div {
        let theme = *cx.global::<Theme>();
        let rows = self.rows(cx);
        let query = self.search.text.clone();
        let filter = self.shortcut_search.clone();

        let mut head = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .py(px(SPACE_S))
            .child(self.search_field(&theme))
            .child(
                icon_button(
                    "keymap-find-by-shortcut",
                    "general/search",
                    "Find by Shortcut",
                    None,
                    &theme,
                )
                .debug_selector(|| "keymap-find-by-shortcut".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.keymap_page.open_capture(CaptureKind::Find, cx);
                })),
            )
            .child(
                div()
                    .id("keymap-reset-all")
                    .debug_selector(|| "keymap-reset-all".to_owned())
                    .flex()
                    .items_center()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .hover(move |style| style.bg(theme.hover))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.keymap_page.reset_all = true;
                        cx.notify();
                    }))
                    .child("Reset All"),
            );
        if let Some(shortcut) = &self.shortcut_search {
            let text = format!("Shortcut: {}", present(shortcut));
            head = head.child(
                div()
                    .id("keymap-shortcut-filter")
                    .debug_selector(|| "keymap-shortcut-filter".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .bg(theme.selection)
                    .child(text)
                    .child(
                        icon_button(
                            "keymap-clear-shortcut-filter",
                            "general/closeSmall",
                            "Clear Shortcut Filter",
                            None,
                            &theme,
                        )
                        .debug_selector(|| "keymap-clear-shortcut-filter".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.keymap_page.shortcut_search = None;
                            cx.notify();
                        })),
                    ),
            );
        }

        let mut page = div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(SPACE_XS))
            .child(head);
        if let Some(capture) = self.capture.clone() {
            page = page.child(self.capture_popup(&capture, &theme, cx));
        }
        if let Some(conflict) = &self.conflict {
            page = page.child(self.conflict_popup(conflict, &theme, cx));
        }
        if self.reset_all {
            page = page.child(self.reset_all_popup(&theme, cx));
        }
        if let Some(action) = self.menu
            && let Some(row) = rows.iter().find(|row| row.action == action)
        {
            page = page.child(self.row_menu(row, &theme, cx));
        }
        if let Some(message) = &self.message {
            page = page.child(
                div()
                    .flex_none()
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child(message.clone()),
            );
        }

        let shown: Vec<KeyRow> = rows
            .into_iter()
            .filter(|row| row.matches(&query, filter.as_ref()))
            .collect();
        if shown.is_empty() {
            page = page.child(
                div()
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("No action matches."),
            );
        }
        for row in &shown {
            page = page.child(self.row(row, &theme, cx));
        }
        page
    }

    fn search_field(&self, theme: &Theme) -> impl IntoElement + use<> {
        let empty = self.search.text.is_empty();
        let active = self.capture.is_none() && self.conflict.is_none() && !self.reset_all;
        div()
            .id("keymap-search")
            .debug_selector(|| "keymap-search".to_owned())
            .flex()
            .items_center()
            .flex_1()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_2()
            .border_color(theme.accent)
            .text_color(if empty {
                theme.text_secondary
            } else {
                theme.text
            })
            .children(text_field_children_named(
                "keymap-search-field",
                &self.search,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                16.,
                active,
            ))
            .when(empty, |this| this.child("Search by action name"))
    }

    fn row(
        &self,
        row: &KeyRow,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> impl IntoElement + use<> {
        let id = selector_of("keymap-row", row.action);
        let action = row.action;
        let add_id = SharedString::from(selector_of("keymap-add", row.action));
        let reset_id = SharedString::from(selector_of("keymap-reset", row.action));
        let changed = row.changed;
        let mut line = div()
            .id(SharedString::from(id.clone()))
            .debug_selector(move || id)
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .min_h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(changed, |this| this.bg(theme.selection))
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(move |this, _, _, cx| {
                    this.keymap_page.menu = Some(action);
                    cx.notify();
                }),
            )
            .child(
                div()
                    .flex_1()
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .child(row.title.clone()),
            );
        for (index, shortcut) in row.shortcuts.iter().enumerate() {
            let remove_id = SharedString::from(format!(
                "{}-{index}",
                selector_of("keymap-remove", row.action)
            ));
            let remove_selector = remove_id.to_string();
            let text = present(shortcut);
            let shortcut = shortcut.clone();
            line = line.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .whitespace_nowrap()
                            .child(text),
                    )
                    .child(
                        icon_button(
                            remove_id,
                            "general/closeSmall",
                            "Remove Keyboard Shortcut",
                            None,
                            theme,
                        )
                        .debug_selector(move || remove_selector.clone())
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.keymap_page.remove(action, shortcut.clone(), cx);
                        })),
                    ),
            );
        }
        line.child(
            icon_button(add_id, "general/add", "Add Keyboard Shortcut", None, theme)
                .debug_selector(move || selector_of("keymap-add", action))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.keymap_page.open_capture(CaptureKind::Add(action), cx);
                })),
        )
        .child(
            icon_button(reset_id, "general/refresh", "Reset to Default", None, theme)
                .debug_selector(move || selector_of("keymap-reset", action))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.keymap_page.reset_action(action, cx);
                })),
        )
    }

    /// The menu of a row, opened by the right click on it. The buttons of the row do the
    /// same without the menu.
    fn row_menu(
        &self,
        row: &KeyRow,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> impl IntoElement + use<> {
        let action = row.action;
        let first = row.shortcuts.first().cloned();
        let mut menu = div()
            .id("keymap-row-menu")
            .debug_selector(|| "keymap-row-menu".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .py(px(SPACE_S))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.accent)
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(row.title.clone()),
            );
        let add_id = SharedString::from(selector_of("keymap-menu-add", action));
        menu = menu.child(
            div()
                .id(add_id)
                .debug_selector(move || selector_of("keymap-menu-add", action))
                .px(px(SPACE_S))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.keymap_page.open_capture(CaptureKind::Add(action), cx);
                }))
                .child("Add Keyboard Shortcut"),
        );
        if let Some(shortcut) = first {
            let remove_id = SharedString::from(selector_of("keymap-menu-remove", action));
            let label = format!("Remove {}", present(&shortcut));
            menu = menu.child(
                div()
                    .id(remove_id)
                    .debug_selector(move || selector_of("keymap-menu-remove", action))
                    .px(px(SPACE_S))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.keymap_page.remove(action, shortcut.clone(), cx);
                    }))
                    .child(label),
            );
        }
        let reset_id = SharedString::from(selector_of("keymap-menu-reset", action));
        menu.child(
            div()
                .id(reset_id)
                .debug_selector(move || selector_of("keymap-menu-reset", action))
                .px(px(SPACE_S))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.keymap_page.reset_action(action, cx);
                }))
                .child("Reset"),
        )
    }

    fn capture_popup(
        &self,
        capture: &Capture,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> impl IntoElement + use<> {
        let heading = match capture.kind {
            CaptureKind::Add(_) => "Add Keyboard Shortcut",
            CaptureKind::Find => "Find by Shortcut",
        };
        let confirm = match capture.kind {
            CaptureKind::Add(_) => "Add",
            CaptureKind::Find => "Find",
        };
        let keys = if capture.keys.is_empty() {
            "Press a shortcut".to_owned()
        } else {
            present(&Shortcut::Keys(capture.keys.clone()))
        };
        div()
            .id("keymap-capture")
            .debug_selector(|| "keymap-capture".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_M))
            .py(px(SPACE_S))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.accent)
            .child(div().text_color(theme.text_secondary).child(heading))
            .child(div().flex_1().child(keys))
            .child(
                div()
                    .id("keymap-capture-confirm")
                    .debug_selector(|| "keymap-capture-confirm".to_owned())
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.keymap_page.commit_capture(cx);
                    }))
                    .child(confirm),
            )
            .child(
                div()
                    .id("keymap-capture-cancel")
                    .debug_selector(|| "keymap-capture-cancel".to_owned())
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.keymap_page.capture = None;
                        cx.notify();
                    }))
                    .child("Cancel"),
            )
    }

    fn conflict_popup(
        &self,
        conflict: &Conflict,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> impl IntoElement + use<> {
        let text = format!(
            "{} is already used by other actions.",
            present(&conflict.shortcut)
        );
        let mut popup = div()
            .id("keymap-conflict")
            .debug_selector(|| "keymap-conflict".to_owned())
            .flex()
            .flex_wrap()
            .items_center()
            .flex_none()
            .gap(px(SPACE_M))
            .py(px(SPACE_S))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.accent)
            .child(div().text_color(theme.text_secondary).child(text));
        for other in &conflict.others {
            let id = selector_of("keymap-conflict", other);
            let title = self
                .workspace
                .upgrade()
                .and_then(|workspace| {
                    workspace
                        .read(cx)
                        .commands()
                        .get(other)
                        .map(|command| command.title.clone())
                })
                .unwrap_or_else(|| (*other).to_owned());
            popup = popup.child(
                div()
                    .id(SharedString::from(id.clone()))
                    .debug_selector(move || id)
                    .child(title),
            );
        }
        popup
            .child(
                div()
                    .id("keymap-conflict-leave")
                    .debug_selector(|| "keymap-conflict-leave".to_owned())
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.keymap_page.keep_conflict(cx);
                    }))
                    .child("Leave"),
            )
            .child(
                div()
                    .id("keymap-conflict-remove")
                    .debug_selector(|| "keymap-conflict-remove".to_owned())
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.keymap_page.strip_others(cx);
                    }))
                    .child("Remove other"),
            )
    }

    fn reset_all_popup(
        &self,
        theme: &Theme,
        cx: &mut Context<SettingsView>,
    ) -> impl IntoElement + use<> {
        div()
            .id("keymap-reset-all-confirm")
            .debug_selector(|| "keymap-reset-all-confirm".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_M))
            .py(px(SPACE_S))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.accent)
            .child(
                div()
                    .flex_1()
                    .text_color(theme.text_secondary)
                    .child("Reset every action to its default shortcuts?"),
            )
            .child(
                div()
                    .id("keymap-reset-all-yes")
                    .debug_selector(|| "keymap-reset-all-yes".to_owned())
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.keymap_page.reset_everything(cx);
                    }))
                    .child("Reset"),
            )
            .child(
                div()
                    .id("keymap-reset-all-no")
                    .debug_selector(|| "keymap-reset-all-no".to_owned())
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(theme.border)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.keymap_page.reset_all = false;
                        cx.notify();
                    }))
                    .child("Cancel"),
            )
    }
}

#[cfg(test)]
mod tests {
    use keymap::Shortcut;

    use super::present;

    #[test]
    fn a_shortcut_is_written_as_the_page_shows_it() {
        assert_eq!(
            present(&Shortcut::parse("ctrl-shift-n").unwrap()),
            "Ctrl+Shift+N"
        );
        assert_eq!(
            present(&Shortcut::parse("ctrl-k ctrl-c").unwrap()),
            "Ctrl+K, Ctrl+C"
        );
    }
}

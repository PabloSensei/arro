//! The chat of an ACP session: the transcript, the question the agent waits on as buttons,
//! and the message box with `@` mentions.

use agent_core::{PermissionReply, PromptPart, ToolStatus};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::agents::AgentsPanel;
use crate::composer::{Composer, Selection};
use crate::outline::OpenEditors;
use crate::transcript::Item;

impl AgentsPanel {
    /// The selection of the active editor, for `@selection`.
    fn editor_selection(&self, cx: &App) -> Option<Selection> {
        let workspace = self.workspace.upgrade()?;
        let path = workspace.read(cx).active_path()?.to_path_buf();
        let (first, last, text) = OpenEditors::selection(&path, cx)?;
        Some((path, (first as u32, last as u32), text))
    }

    fn candidates(&self, cx: &App) -> Vec<String> {
        let files = self.files.read(cx).clone();
        self.composer.candidates(&self.root, files.as_slice())
    }

    /// Sends the message box. A message typed while the agent works waits in the queue.
    pub(crate) fn send(&mut self, cx: &mut Context<Self>) {
        let text = self.composer.text.trim().to_string();
        if text.is_empty() {
            return;
        }
        let selection = self
            .composer
            .kept_selection()
            .or_else(|| self.editor_selection(cx));
        let parts = Composer::parts(&text, &self.root, selection);
        if self.deliver(&parts, cx) {
            self.composer.take();
        }
    }

    /// Sends a message that is complete already, such as the problem of Fix with AI. The
    /// draft in the message box and its selection stay for the user.
    pub(crate) fn send_finished(&mut self, message: &str, cx: &mut Context<Self>) {
        let text = message.trim().to_string();
        if text.is_empty() {
            return;
        }
        self.deliver(&Composer::parts(&text, &self.root, None), cx);
    }

    /// Gives the parts to the session of the chat; false when it did not take them.
    fn deliver(&mut self, parts: &[PromptPart], cx: &mut Context<Self>) -> bool {
        let Some(entry) = self.active_entry_mut() else {
            return false;
        };
        match entry.session.prompt_parts(parts) {
            Ok(()) => {
                entry.sent += 1;
                cx.notify();
                true
            }
            Err(error) => {
                self.error = Some(format!("The message is not sent: {error}").into());
                cx.notify();
                false
            }
        }
    }

    pub(crate) fn answer(&mut self, reply: PermissionReply, cx: &mut Context<Self>) {
        let Some(entry) = self.active_entry_mut() else {
            return;
        };
        let Some(id) = entry.transcript.pending_permission().map(str::to_string) else {
            return;
        };
        match entry.session.respond(&id, reply) {
            Ok(()) => entry.transcript.answer(&id, reply),
            Err(error) => self.error = Some(format!("The answer is not sent: {error}").into()),
        }
        cx.notify();
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(entry) = self.active_entry_mut() else {
            return;
        };
        match entry.session.cancel() {
            // The question of a stopped turn will never be answered: the buttons go away,
            // so that their answer cannot reach a later question of the session.
            Ok(()) => entry.transcript.clear_pending(),
            Err(error) => self.error = Some(format!("The turn is not stopped: {error}").into()),
        }
        cx.notify();
    }

    /// Keys of the message box; false leaves the key to the window.
    pub(crate) fn compose_key(
        &mut self,
        keystroke: &Keystroke,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = keystroke.modifiers;
        let candidates = self.candidates(cx);
        let control = modifiers.control || modifiers.platform;
        match keystroke.key.as_str() {
            "enter" if modifiers.shift => self.composer.insert("\n"),
            "enter" | "tab" if !candidates.is_empty() => {
                let chosen = self.composer.chosen.min(candidates.len() - 1);
                self.composer.complete(&candidates[chosen]);
            }
            "enter" => self.send(cx),
            "up" if !candidates.is_empty() => self.composer.move_choice(-1, candidates.len()),
            "down" if !candidates.is_empty() => self.composer.move_choice(1, candidates.len()),
            "escape"
                if self
                    .active_entry()
                    .is_some_and(|entry| entry.transcript.running) =>
            {
                self.stop(cx)
            }
            "backspace" => self.composer.backspace(),
            "v" if control => {
                let text = cx.read_from_clipboard().and_then(|item| item.text());
                match text {
                    Some(text) => self.composer.insert(&text.replace("\r\n", "\n")),
                    None => return false,
                }
            }
            _ if control || modifiers.alt => return false,
            _ => match &keystroke.key_char {
                Some(text) if !text.is_empty() => self.composer.insert(text),
                _ => return false,
            },
        }
        cx.notify();
        true
    }

    fn tool_row(&self, item: &Item, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let Item::Tool {
            id,
            title,
            input,
            status,
            output,
        } = item
        else {
            return div();
        };
        let (mark, color) = match status {
            ToolStatus::Pending => ("…", theme.text_secondary),
            ToolStatus::Running => ("●", theme.accent),
            ToolStatus::Done => ("✓", theme.accent),
            ToolStatus::Failed => ("✕", theme.close_hover),
        };
        let open = self.expanded.contains(id);
        let key = id.clone();
        let header = div()
            .id(("chat-tool", index))
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .h(px(ROW_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.expanded.remove(&key) {
                    this.expanded.insert(key.clone());
                }
                cx.notify();
            }))
            .child(div().text_color(color).child(mark))
            .child(div().flex_none().child(title.clone()))
            .child(
                div()
                    .flex_1()
                    .truncate()
                    .text_color(theme.text_secondary)
                    .child(input.clone()),
            );
        let details = open.then(|| {
            let shown = output.clone().unwrap_or_default();
            div()
                .ml(px(20.))
                .p(px(SPACE_S))
                .rounded(px(CONTROL_RADIUS))
                .bg(theme.editor_bg)
                .font_family(MONO_FONT)
                .text_color(theme.text_secondary)
                .child(format!("{input}\n{shown}"))
        });
        div().flex().flex_col().child(header).children(details)
    }

    fn permission_card(&self, item: &Item, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let Item::Permission {
            tool,
            description,
            answer,
            ..
        } = item
        else {
            return div();
        };
        let card = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .p(px(8.))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.warning_mark)
            .child(format!("{tool} asks for permission"))
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .child(description.clone()),
            );
        if let Some(answer) = answer {
            let answer = match answer {
                PermissionReply::Deny => "Denied",
                PermissionReply::AllowAlways => "Allowed always",
                _ => "Allowed once",
            };
            return card.child(div().text_color(theme.text_secondary).child(answer));
        }
        let choice = |id: &'static str, label: &'static str, reply, cx: &mut Context<Self>| {
            text_button(id, label, theme)
                .on_click(cx.listener(move |this, _, _, cx| this.answer(reply, cx)))
        };
        card.child(
            div()
                .flex()
                .gap(px(SPACE_S))
                .child(choice(
                    "allow-once",
                    "Allow once",
                    PermissionReply::AllowOnce,
                    cx,
                ))
                .child(choice(
                    "allow-always",
                    "Allow always",
                    PermissionReply::AllowAlways,
                    cx,
                ))
                .child(choice("deny", "Deny", PermissionReply::Deny, cx)),
        )
    }

    fn chat_item(&self, item: &Item, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let bubble = |text: &str, color: Hsla| {
            div()
                .p(px(8.))
                .rounded(px(CONTROL_RADIUS))
                .bg(color)
                .child(text.to_string())
        };
        match item {
            Item::User(text) => bubble(text, theme.selection_inactive),
            Item::Agent(text) => div().px(px(SPACE_S)).child(text.clone()),
            Item::Tool { .. } => self.tool_row(item, index, theme, cx),
            Item::Permission { .. } => self.permission_card(item, theme, cx),
            Item::Notice(text) => div()
                .px(px(SPACE_S))
                .text_color(theme.text_secondary)
                .child(text.clone()),
            Item::Error(text) => div()
                .px(px(SPACE_S))
                .text_color(theme.close_hover)
                .child(text.clone()),
        }
    }

    fn composer_view(&self, focused: bool, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let candidates = self.candidates(cx);
        let mut list = div().flex().flex_col();
        for (index, name) in candidates.iter().enumerate() {
            let chosen = index == self.composer.chosen.min(candidates.len() - 1);
            let name = name.clone();
            let row = div()
                .id(("mention", index))
                .px(px(8.))
                .h(px(ROW_HEIGHT))
                .flex()
                .items_center()
                .rounded(px(CONTROL_RADIUS))
                .when(chosen, |this| this.bg(theme.selection))
                .on_click(cx.listener({
                    let name = name.clone();
                    move |this, _, _, cx| {
                        this.composer.complete(&name);
                        cx.notify();
                    }
                }))
                .child(format!("@{name}"));
            list = list.child(row);
        }
        let shown = if self.composer.text.is_empty() {
            div()
                .text_color(theme.text_disabled)
                .child("Ask the agent, @ to mention a file or the selection")
        } else {
            let caret = if focused { "▏" } else { "" };
            div().child(format!("{}{caret}", self.composer.text))
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(4.))
            .m(px(8.))
            .p(px(8.))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if focused {
                theme.accent
            } else {
                theme.control_border
            })
            .child(list)
            .child(shown)
    }

    pub(crate) fn chat_view(
        &self,
        focused: bool,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Div {
        let Some(entry) = self.active_entry() else {
            return div();
        };
        let items: Vec<Item> = entry.transcript.items.clone();
        let mut list = div().flex().flex_col().gap(px(SPACE_M)).p(px(SPACE_M));
        for (index, item) in items.iter().enumerate() {
            list = list.child(self.chat_item(item, index, theme, cx));
        }
        let list = scrollable("chat", &self.chat_scroll, list, theme, window, cx);
        let running = entry.transcript.running;
        let queued = entry.queued();
        let mut status = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(SPACE_M))
            .text_color(theme.text_secondary);
        if running {
            status = status.child("The agent is working").child(
                text_button("chat-stop", "Stop", theme)
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
            );
        }
        if queued > 0 {
            status = status.child(format!("{queued} queued"));
        }
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(list)
            .child(status)
            .child(self.composer_view(focused, theme, cx))
    }
}

/// A small bordered button with a label.
pub(crate) fn text_button(
    id: impl Into<ElementId>,
    label: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    div()
        .id(id)
        .flex()
        .items_center()
        .h(px(24.))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.control_border)
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .child(label)
}

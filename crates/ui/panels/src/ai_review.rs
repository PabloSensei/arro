//! The Changes tab: files the sessions changed, the hunks of one of them against the base, and
//! accept or revert by hunk, by file and for the whole session. A revert never writes under
//! unsaved edits of an open editor: the file is left alone and the panel says why.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use change_tracker::{ChangeSet, ChangeTracker, FileReview};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::agents::AgentsPanel;
use crate::chat::text_button;
use crate::outline::OpenEditors;

/// How long the result of the AGENTS.md button stays next to it, unless the user acts first.
pub(crate) const AGENTS_MD_NOTE_TIME: Duration = Duration::from_secs(5);

/// What the AGENTS.md button reported last: a result, or why nothing was written.
#[derive(Clone)]
pub(crate) struct AgentsMdNote {
    pub(crate) text: SharedString,
    pub(crate) failed: bool,
}

#[derive(Default)]
pub(crate) struct Review {
    /// The file shown, relative to the project with forward slashes.
    pub selected: Option<String>,
    pub file: Option<FileReview>,
    /// Why the last action left something alone, or what failed.
    pub notice: Option<String>,
    /// Every file a session changed so far, also after a revert, for scripts.
    touched: Vec<String>,
    generation: u64,
    /// The lines of the shown file that the user selected, by their place in the review: where
    /// the selection began and where it ends. A new file clears them.
    pub(crate) lines: Option<(usize, usize)>,
    /// Whether the button is down over the lines, so that a move extends the selection.
    pub(crate) dragging: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    RevertHunk(usize),
    AcceptHunk(usize),
    RevertFile,
    AcceptFile,
    RevertAll,
    AcceptAll,
}

impl Action {
    fn reverts(self) -> bool {
        matches!(
            self,
            Self::RevertHunk(_) | Self::RevertFile | Self::RevertAll
        )
    }

    fn whole_session(self) -> bool {
        matches!(self, Self::RevertAll | Self::AcceptAll)
    }

    /// The actions that work on a binary file, which has no hunks to review.
    fn whole_file(self) -> bool {
        matches!(
            self,
            Self::RevertFile | Self::AcceptFile | Self::RevertAll | Self::AcceptAll
        )
    }
}

/// Puts `bytes` on disk, or removes the file for `None`.
fn write_bytes(root: &Path, path: &str, bytes: Option<&[u8]>) -> io::Result<()> {
    let file = root.join(path);
    match bytes {
        Some(bytes) => {
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&file, bytes)
        }
        None => match std::fs::remove_file(&file) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        },
    }
}

/// Puts `text` on disk, or removes the file for `None`.
pub(crate) fn write(root: &Path, path: &str, text: Option<String>) -> io::Result<()> {
    write_bytes(root, path, text.as_deref().map(str::as_bytes))
}

/// Does `action` to one file; true when the file on disk changed.
fn act_on(
    tracker: &ChangeTracker,
    root: &Path,
    review: FileReview,
    action: Action,
) -> Result<bool, String> {
    let path = review.path.clone();
    let failed = |error: io::Error| format!("{path}: {error}");
    if review.binary && !action.whole_file() {
        return Err(format!("{path}: a binary file has no hunks to review"));
    }
    let missing = || format!("{path}: the hunk is gone, look again");
    let no_snapshot = || {
        format!("{path}: the snapshot from before the agent's edit is gone, nothing to revert to")
    };
    let no_bytes = || format!("{path}: the review kept no bytes of the file");
    match action {
        Action::RevertHunk(index) => {
            let text = review.reverted_hunk(index).ok_or_else(missing)?;
            write(root, &path, text).map_err(failed)?;
            Ok(true)
        }
        Action::AcceptHunk(index) => {
            let base = review.accepted_hunk(index).ok_or_else(missing)?;
            tracker.accept(&path, base);
            Ok(false)
        }
        Action::RevertFile | Action::RevertAll if review.binary => {
            let bytes = review.binary_bytes().ok_or_else(no_snapshot)?;
            write_bytes(root, &path, bytes.base.as_deref()).map_err(failed)?;
            Ok(true)
        }
        Action::RevertFile | Action::RevertAll => {
            write(root, &path, review.base).map_err(failed)?;
            Ok(true)
        }
        Action::AcceptFile | Action::AcceptAll if review.binary => {
            let bytes = review.binary_bytes().ok_or_else(no_bytes)?;
            tracker.accept_bytes(&path, bytes.current.clone());
            Ok(false)
        }
        Action::AcceptFile | Action::AcceptAll => {
            tracker.accept(&path, review.current);
            Ok(false)
        }
    }
}

impl AgentsPanel {
    /// The tracker published a new list: clean editors of changed files read them again, so an
    /// agent's edit shows up in an open file as one step that undo takes back.
    pub(crate) fn changes_arrived(&mut self, changes: Arc<ChangeSet>, cx: &mut Context<Self>) {
        let paths = changes.session.iter().chain(&self.changes.session);
        let paths: Vec<_> = paths.map(|change| self.root.join(&change.path)).collect();
        for change in &changes.session {
            if !self.review.touched.contains(&change.path) {
                self.review.touched.push(change.path.clone());
            }
        }
        self.changes = changes;
        for path in paths {
            if !OpenEditors::is_dirty(&path, cx) {
                OpenEditors::reload(&path, cx);
            }
        }
        if self.review.selected.is_some() || self.tab == crate::agents::Tab::Changes {
            self.reload_review(cx);
        }
        self.refresh_agent_changes(cx);
    }

    pub(crate) fn select_change(&mut self, path: String, cx: &mut Context<Self>) {
        self.review.selected = Some(path);
        self.review.notice = None;
        self.review.lines = None;
        self.reload_review(cx);
    }

    /// Shows `file` as the review of the selected file. Its lines are new, so the selection of
    /// the lines of the file before goes.
    fn show_file(&mut self, file: Option<FileReview>) {
        self.review.file = file;
        self.review.lines = None;
        self.review.dragging = false;
    }

    /// Accepts or reverts one file, for the bar above its editor.
    pub(crate) fn act_on_file(&mut self, path: String, action: Action, cx: &mut Context<Self>) {
        if let Some(position) = self
            .proposals
            .iter()
            .position(|proposal| proposal.relative == path)
        {
            self.act_on_proposal(position, action, cx);
            return;
        }
        self.review.selected = Some(path.clone());
        if self
            .review
            .file
            .as_ref()
            .is_some_and(|file| file.path != path)
        {
            self.show_file(None);
        }
        self.act(action, cx);
    }

    /// Shows the review of one file, for the Review button of the bar above its editor.
    pub(crate) fn review_file(&mut self, path: String, cx: &mut Context<Self>) {
        self.tab = crate::agents::Tab::Changes;
        self.select_change(path, cx);
        cx.notify();
    }

    /// Reads the review of the selected file, or of the first changed one, off the UI thread.
    pub(crate) fn reload_review(&mut self, cx: &mut Context<Self>) {
        let proposal = self.review.selected.as_deref().and_then(|path| {
            self.proposals
                .iter()
                .position(|proposal| proposal.relative == path)
        });
        if let Some(position) = proposal {
            let file = self.proposals[position].view();
            self.show_file(Some(file));
            cx.notify();
            return;
        }
        if self.review.selected.is_none()
            && let Some(proposal) = self.proposals.first()
        {
            self.review.selected = Some(proposal.relative.clone());
            let file = proposal.view();
            self.show_file(Some(file));
            cx.notify();
            return;
        }
        let session = &self.changes.session;
        let listed = |path: &String| session.iter().any(|change| &change.path == path);
        if !self.review.selected.as_ref().is_some_and(listed) {
            self.review.selected = session.first().map(|change| change.path.clone());
        }
        let (Some(path), Some(tracker)) = (self.review.selected.clone(), self.tracker().cloned())
        else {
            self.show_file(None);
            cx.notify();
            return;
        };
        self.review.generation += 1;
        let generation = self.review.generation;
        let task = cx
            .background_executor()
            .spawn(async move { tracker.review(&path) });
        cx.spawn(async move |this, cx| {
            let result = task.await;
            this.update(cx, |this, cx| {
                if this.review.generation != generation {
                    return;
                }
                match result {
                    Ok(file) => this.show_file(Some(file)),
                    Err(error) => {
                        this.show_file(None);
                        this.review.notice = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Accepts or reverts. Files with unsaved edits in an editor are not reverted.
    pub(crate) fn act(&mut self, action: Action, cx: &mut Context<Self>) {
        if let Some(position) = self.review.selected.as_deref().and_then(|path| {
            self.proposals
                .iter()
                .position(|proposal| proposal.relative == path)
        }) {
            self.act_on_proposal(position, action, cx);
            return;
        }
        let Some(tracker) = self.tracker().cloned() else {
            return;
        };
        let targets: Vec<String> = if action.whole_session() {
            let session = self.changes.session.iter();
            session.map(|change| change.path.clone()).collect()
        } else {
            self.review.selected.clone().into_iter().collect()
        };
        let root = self.root.clone();
        let (dirty, clean): (Vec<String>, Vec<String>) = targets
            .into_iter()
            .partition(|path| action.reverts() && OpenEditors::is_dirty(&root.join(path), cx));
        self.review.notice = (!dirty.is_empty()).then(|| {
            format!(
                "Not reverted, the editor has unsaved edits: {}. Save or undo them, then revert.",
                dirty.join(", ")
            )
        });
        // Hunk numbers refer to the review on screen, so that one is used as it is.
        let shown = self.review.file.clone();
        let task = cx.background_executor().spawn(async move {
            let mut written = Vec::new();
            let mut errors = Vec::new();
            for path in clean {
                let review = match &shown {
                    Some(review) if review.path == path && !action.whole_session() => {
                        Ok(review.clone())
                    }
                    _ => tracker
                        .review(&path)
                        .map_err(|error| format!("{path}: {error}")),
                };
                match review.and_then(|review| act_on(&tracker, &root, review, action)) {
                    Ok(true) => written.push(path),
                    Ok(false) => {}
                    Err(error) => errors.push(error),
                }
            }
            if !written.is_empty() {
                tracker.refresh();
            }
            (written, errors)
        });
        cx.spawn(async move |this, cx| {
            let (written, errors) = task.await;
            this.update(cx, |this, cx| {
                for path in &written {
                    OpenEditors::reload(&this.root.join(path), cx);
                }
                if !errors.is_empty() {
                    let mut notice = this.review.notice.take().unwrap_or_default();
                    for error in errors {
                        notice.push_str(&format!("\n{error}"));
                    }
                    this.review.notice = Some(notice.trim().to_string());
                }
                this.reload_review(cx);
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Adds the section on IDE tools to `AGENTS.md`, or refreshes it, unless an editor holds
    /// the file with unsaved edits. The result shows next to the button, and a written file
    /// joins the changes at once.
    pub(crate) fn add_agents_md(&mut self, cx: &mut Context<Self>) {
        let file = self.root.join("AGENTS.md");
        if OpenEditors::is_dirty(&file, cx) {
            self.show_agents_md_note(
                "AGENTS.md has unsaved edits in the editor, save them first",
                true,
                cx,
            );
            return;
        }
        let existing = match std::fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
            // A file that cannot be read stays as it is: writing over it would lose its text.
            Err(error) => {
                self.show_agents_md_note(format!("AGENTS.md is not read: {error}"), true, cx);
                return;
            }
        };
        let text = tool_gateway::with_agents_md_section(&existing, agent_pty::MCP_SERVER_NAME);
        if text == existing {
            self.show_agents_md_note("AGENTS.md already lists the IDE tools", false, cx);
            return;
        }
        match std::fs::write(&file, text) {
            Ok(()) => {
                OpenEditors::reload(&file, cx);
                // A snapshot now, as the other actions take one after they write a file.
                if let Some(tracker) = self.tracker() {
                    tracker.refresh();
                }
                self.show_agents_md_note("AGENTS.md lists the IDE tools now", false, cx);
            }
            Err(error) => {
                self.show_agents_md_note(format!("AGENTS.md is not written: {error}"), true, cx)
            }
        }
    }

    /// Shows what the AGENTS.md button did, next to it, until the next action or until
    /// `AGENTS_MD_NOTE_TIME` passes. A newer result is not cleared by an older timer.
    fn show_agents_md_note(
        &mut self,
        text: impl Into<SharedString>,
        failed: bool,
        cx: &mut Context<Self>,
    ) {
        self.agents_md_generation = self.agents_md_generation.wrapping_add(1);
        let mine = self.agents_md_generation;
        self.agents_md = Some(AgentsMdNote {
            text: text.into(),
            failed,
        });
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AGENTS_MD_NOTE_TIME).await;
            this.update(cx, |this, cx| {
                if this.agents_md_generation == mine {
                    this.agents_md = None;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn file_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let mut list = div().flex().flex_col().flex_none();
        let mut paths: Vec<(String, &'static str)> = self
            .changes
            .session
            .iter()
            .map(|change| {
                let kind = match change.kind {
                    change_tracker::ChangeKind::Added => "A",
                    change_tracker::ChangeKind::Modified => "M",
                    change_tracker::ChangeKind::Deleted => "D",
                };
                (change.path.clone(), kind)
            })
            .collect();
        for proposal in &self.proposals {
            if !paths.iter().any(|(path, _)| path == &proposal.relative) {
                paths.push((proposal.relative.clone(), "P"));
            }
        }
        for (index, (path, kind)) in paths.into_iter().enumerate() {
            let selected = self.review.selected.as_ref() == Some(&path);
            let shown = path.clone();
            let hover = theme.selection_hover;
            let row = div()
                .id(("change-row", index))
                .flex()
                .items_center()
                .gap(px(8.))
                .h(px(ROW_HEIGHT))
                .px(px(8.))
                .rounded(px(CONTROL_RADIUS))
                .when(selected, |this| this.bg(theme.selection_inactive))
                .when(!selected, |this| this.hover(move |style| style.bg(hover)))
                .on_click(cx.listener(move |this, _, _, cx| this.select_change(path.clone(), cx)))
                .child(
                    div()
                        .w(px(12.))
                        .text_color(theme.text_secondary)
                        .child(kind),
                )
                .child(div().flex_1().truncate().child(shown));
            list = list.child(row);
        }
        list
    }

    /// The lines of a hunk; `first` is the place of its first line among the lines of the file.
    fn hunk_view(
        &self,
        index: usize,
        first: usize,
        hunk: &change_tracker::ReviewHunk,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let header = format!(
            "@@ -{},{} +{},{} @@",
            hunk.before.start + 1,
            hunk.before.len(),
            hunk.after.start + 1,
            hunk.after.len()
        );
        let picked = self
            .review
            .lines
            .map(|(from, to)| (from.min(to), from.max(to)));
        let mut lines = div().flex().flex_col().font_family(MONO_FONT);
        let mut place = first;
        for line in &hunk.removed {
            let row = self.review_line(
                place,
                format!("- {line}"),
                theme.close_hover,
                picked,
                theme,
                cx,
            );
            lines = lines.child(row);
            place += 1;
        }
        for (number, line) in hunk.added.iter().enumerate() {
            let row = self.review_line(place, format!("+ {line}"), theme.string, picked, theme, cx);
            // The last line of a hunk has a selector, so a test can see that the list reaches it.
            let row = if number + 1 == hunk.added.len() {
                div()
                    .debug_selector(move || format!("review-last-line-{index}"))
                    .child(row)
                    .into_any_element()
            } else {
                row.into_any_element()
            };
            lines = lines.child(row);
            place += 1;
        }
        div()
            .flex()
            .flex_col()
            .gap(px(4.))
            .p(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .bg(theme.editor_bg)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(
                        div()
                            .flex_1()
                            .text_color(theme.text_secondary)
                            .child(header),
                    )
                    .child(
                        text_button(("accept-hunk", index), "Accept", theme)
                            .debug_selector(move || format!("accept-hunk-{index}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.act(Action::AcceptHunk(index), cx)
                            })),
                    )
                    .child(
                        text_button(("revert-hunk", index), "Revert", theme)
                            .debug_selector(move || format!("revert-hunk-{index}"))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.act(Action::RevertHunk(index), cx)
                            })),
                    ),
            )
            .child(lines)
    }

    /// One line of a hunk. A press selects it, and a drag from it extends the selection over
    /// the lines it passes. The text is what the line shows, with its sign.
    fn review_line(
        &self,
        place: usize,
        text: String,
        color: Hsla,
        picked: Option<(usize, usize)>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let chosen = picked.is_some_and(|(from, to)| (from..=to).contains(&place));
        div()
            .id(("review-line", place))
            .debug_selector(move || format!("review-line-{place}"))
            .when(chosen, |this| this.bg(theme.selection))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.press_review_line(place, event.modifiers.shift, cx)
                }),
            )
            .on_mouse_move(
                cx.listener(move |this, _: &MouseMoveEvent, _, cx| {
                    this.enter_review_line(place, cx)
                }),
            )
            .text_color(color)
            .child(text)
    }

    /// A press on a line of the review. Shift keeps the start of the selection and moves its end.
    pub(crate) fn press_review_line(&mut self, place: usize, extend: bool, cx: &mut Context<Self>) {
        let start = match (extend, self.review.lines) {
            (true, Some((start, _))) => start,
            _ => place,
        };
        self.review.lines = Some((start, place));
        self.review.dragging = true;
        cx.notify();
    }

    /// The pointer moved over a line while the button is down: the selection reaches it.
    pub(crate) fn enter_review_line(&mut self, place: usize, cx: &mut Context<Self>) {
        if !self.review.dragging {
            return;
        }
        if let Some((start, end)) = self.review.lines
            && end != place
        {
            self.review.lines = Some((start, place));
            cx.notify();
        }
    }

    /// The button came up: the selection stays, and a move no longer extends it.
    pub(crate) fn release_review_lines(&mut self, cx: &mut Context<Self>) {
        if self.review.dragging {
            self.review.dragging = false;
            cx.notify();
        }
    }

    /// Copies the selected lines of the review without their signs, one line each. False when
    /// no line is selected.
    pub(crate) fn copy_review_lines(&self, cx: &mut App) -> bool {
        let Some((start, end)) = self.review.lines else {
            return false;
        };
        let Some(file) = &self.review.file else {
            return false;
        };
        let lines: Vec<&str> = file
            .hunks
            .iter()
            .flat_map(|hunk| hunk.removed.iter().chain(&hunk.added))
            .map(String::as_str)
            .collect();
        if lines.is_empty() {
            return false;
        }
        // The selection may name lines of the file before a change: it is cut to the lines there are.
        let (first, last) = (start.min(end), start.max(end).min(lines.len() - 1));
        if first > last {
            return false;
        }
        cx.write_to_clipboard(ClipboardItem::new_string(lines[first..=last].join("\n")));
        true
    }

    pub(crate) fn changes_view(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        // The content scrolls as a whole, the notice and the buttons with the files and the
        // hunks, and the wrapper takes the height that the body of the panel leaves over. A
        // drag that ends off the lines ends here too.
        let view = div()
            .id("changes-content")
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .p(px(SPACE_M))
            .overflow_y_scroll()
            .track_scroll(&self.review_scroll)
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.release_review_lines(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _: &MouseUpEvent, _, cx| this.release_review_lines(cx)),
            );
        let state = if !self.proposals.is_empty() {
            None
        } else {
            match &self.services {
                None => Some("Changes show up once a session runs".to_string()),
                Some(services) => match &services.tracker {
                    Err(error) => Some(format!("Changes are not tracked: {error}")),
                    Ok(_) if self.changes.session.is_empty() => {
                        Some("The sessions have not changed any file".to_string())
                    }
                    Ok(_) => None,
                },
            }
        };
        let notice = self.review.notice.clone().map(|notice| {
            div()
                .p(px(SPACE_S))
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(theme.warning_mark)
                .child(notice)
        });
        if let Some(state) = state {
            let shown = div().text_color(theme.text_secondary).child(state);
            let view = view.children(notice).child(shown);
            return scrollable("changes", &self.review_scroll, view, theme, window, cx)
                .flex_1()
                .min_h_0();
        }
        let session = div()
            .flex()
            .gap(px(SPACE_S))
            .child(
                text_button("accept-all", "Accept all", theme)
                    .debug_selector(|| "accept-all".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.act(Action::AcceptAll, cx))),
            )
            .child(
                text_button("revert-all", "Revert all", theme)
                    .debug_selector(|| "revert-all".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| this.act(Action::RevertAll, cx))),
            );
        let mut view = view
            .children(notice)
            .child(session)
            .child(self.file_rows(theme, cx));
        if let Some(file) = &self.review.file {
            let actions = div()
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .pt(px(SPACE_S))
                .border_t_1()
                .border_color(theme.border)
                .child(div().flex_1().truncate().child(file.path.clone()))
                .child(
                    text_button("accept-file", "Accept file", theme)
                        .debug_selector(|| "accept-file".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.act(Action::AcceptFile, cx))),
                )
                .child(
                    text_button("revert-file", "Revert file", theme)
                        .debug_selector(|| "revert-file".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.act(Action::RevertFile, cx))),
                );
            view = view.child(actions);
            if file.binary {
                view = view.child(div().text_color(theme.text_secondary).child("Binary file"));
            }
            let mut first = 0;
            for (index, hunk) in file.hunks.iter().enumerate() {
                view = view.child(self.hunk_view(index, first, hunk, theme, cx));
                first += hunk.removed.len() + hunk.added.len();
            }
        }
        scrollable("changes", &self.review_scroll, view, theme, window, cx)
            .flex_1()
            .min_h_0()
    }
}

impl Review {
    /// Lines for scripts: the review on screen and the open editors of changed files.
    pub fn report(&self, root: &Path, cx: &App) -> Vec<String> {
        let mut lines = Vec::new();
        if let Some(file) = &self.file {
            lines.push(format!("review {} hunks {}", file.path, file.hunks.len()));
            for hunk in &file.hunks {
                let removed = hunk.removed.join("|");
                let added = hunk.added.join("|");
                lines.push(format!("review-hunk -{removed} +{added}"));
            }
        }
        if let Some(notice) = &self.notice {
            lines.extend(notice.lines().map(|line| format!("review-notice {line}")));
        }
        for path in &self.touched {
            let file = root.join(path);
            if let Some(text) = OpenEditors::text(&file, cx) {
                let first = text.lines().next().unwrap_or_default();
                let dirty = OpenEditors::is_dirty(&file, cx);
                lines.push(format!("editor {path} dirty {dirty} {first}"));
            }
        }
        lines
    }
}

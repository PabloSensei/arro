//! The Changes tab: files the sessions changed, the hunks of one of them against the base, and
//! accept or revert by hunk, by file and for the whole session. A revert never writes under
//! unsaved edits of an open editor: the file is left alone and the panel says why.

use std::io;
use std::path::Path;
use std::sync::Arc;

use change_tracker::{ChangeSet, ChangeTracker, FileReview};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::agents::AgentsPanel;
use crate::chat::text_button;
use crate::outline::OpenEditors;

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
        self.reload_review(cx);
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
            self.review.file = None;
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
            self.review.file = Some(self.proposals[position].view());
            cx.notify();
            return;
        }
        if self.review.selected.is_none()
            && let Some(proposal) = self.proposals.first()
        {
            self.review.selected = Some(proposal.relative.clone());
            self.review.file = Some(proposal.view());
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
            self.review.file = None;
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
                    Ok(file) => this.review.file = Some(file),
                    Err(error) => {
                        this.review.file = None;
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
    /// the file with unsaved edits.
    pub(crate) fn add_agents_md(&mut self, cx: &mut Context<Self>) {
        let file = self.root.join("AGENTS.md");
        if OpenEditors::is_dirty(&file, cx) {
            self.review.notice =
                Some("AGENTS.md has unsaved edits in the editor, save them first".to_string());
            cx.notify();
            return;
        }
        let existing = std::fs::read_to_string(&file).unwrap_or_default();
        let text = tool_gateway::with_agents_md_section(&existing, agent_pty::MCP_SERVER_NAME);
        self.review.notice = match std::fs::write(&file, text) {
            Ok(()) => Some("AGENTS.md lists the IDE tools now".to_string()),
            Err(error) => Some(format!("AGENTS.md is not written: {error}")),
        };
        OpenEditors::reload(&file, cx);
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

    fn hunk_view(
        &self,
        index: usize,
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
        let mut lines = div().flex().flex_col().font_family(MONO_FONT);
        for line in &hunk.removed {
            lines = lines.child(
                div()
                    .text_color(theme.close_hover)
                    .child(format!("- {line}")),
            );
        }
        for line in &hunk.added {
            lines = lines.child(div().text_color(theme.string).child(format!("+ {line}")));
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

    pub(crate) fn changes_view(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let view = div().flex().flex_col().gap(px(SPACE_S)).p(px(SPACE_M));
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
            return scrollable("changes", &self.review_scroll, view, theme, window, cx);
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
            for (index, hunk) in file.hunks.iter().enumerate() {
                view = view.child(self.hunk_view(index, hunk, theme, cx));
            }
        }
        scrollable("changes", &self.review_scroll, view, theme, window, cx)
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

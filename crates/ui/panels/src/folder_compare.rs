//! Compare two folders: the files only one side has, the files that differ and, when
//! asked for, the identical ones. The folders are read off the window thread; a row
//! opens its two files in the diff, and the synchronization actions copy one file to the
//! other side or remove the extra one.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use local_history::Cause;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::diff_view::DiffView;
use crate::git::{ActiveGit, Kind};
use crate::tree_ops::delete_on_disk;

const WIDTH: f32 = 980.;
const HEIGHT: f32 = 620.;
const CONTEXT: &str = "FolderCompare";
/// Never compared: the object store of the version control.
const HIDDEN: &[&str] = &[".git"];

actions!(
    folder_compare,
    [
        NextDiff,
        PreviousDiff,
        SynchronizeDiff,
        SynchronizeDiffAll,
        OpenDiff,
        CancelCompare
    ]
);

/// The keys of the window and, from the same rows, the table the keymap check reads.
macro_rules! compare_keys {
    ($(($keys:literal, $action:ident, $idea:expr, $mac:expr)),* $(,)?) => {
        pub fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        /// Binds the keys of one platform, so that a test reads the macOS table on every
        /// runner.
        fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = workspace::KeyRow {
                    keys: $keys,
                    mac: $mac,
                    action: concat!("folder_compare::", stringify!($action)),
                    context: Some(CONTEXT),
                    idea: $idea,
                };
                for key in row.keys_for(platform) {
                    cx.bind_keys([KeyBinding::new(key, $action, Some(CONTEXT))]);
                }
            )*
        }

        #[cfg(test)]
        pub(crate) const KEYS: &[workspace::KeyRow] = &[
            $(workspace::KeyRow {
                keys: $keys,
                mac: $mac,
                action: concat!("folder_compare::", stringify!($action)),
                context: Some(CONTEXT),
                idea: $idea,
            }),*
        ];
    };
}

compare_keys![
    ("f7", NextDiff, Some("NextDiff"), None),
    ("shift-f7", PreviousDiff, Some("PreviousDiff"), None),
    (
        "enter",
        SynchronizeDiff,
        Some("DirDiffMenu.SynchronizeDiff"),
        None
    ),
    (
        "ctrl-enter",
        SynchronizeDiffAll,
        Some("DirDiffMenu.SynchronizeDiff.All"),
        None
    ),
    ("ctrl-d", OpenDiff, Some("Compare.SameVersion"), None),
    ("escape", CancelCompare, None, Some("escape")),
];

/// The side a file is copied to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Direction {
    LeftToRight,
    RightToLeft,
}

impl Direction {
    fn flipped(self) -> Self {
        match self {
            Self::LeftToRight => Self::RightToLeft,
            Self::RightToLeft => Self::LeftToRight,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::LeftToRight => "Left to Right",
            Self::RightToLeft => "Right to Left",
        }
    }
}

/// What one relative path is on the two sides.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RowKind {
    OnlyLeft,
    OnlyRight,
    Differ,
    Same,
}

impl RowKind {
    fn differs(self) -> bool {
        self != Self::Same
    }

    /// The mark a row shows before its path.
    fn mark(self) -> &'static str {
        match self {
            Self::OnlyLeft => "-",
            Self::OnlyRight => "+",
            Self::Differ => "~",
            Self::Same => "=",
        }
    }

    fn color(self, theme: &Theme) -> Hsla {
        match self {
            Self::OnlyLeft => Kind::Deleted.color(theme.dark),
            Self::OnlyRight => Kind::Added.color(theme.dark),
            Self::Differ => Kind::Modified.color(theme.dark),
            Self::Same => theme.text_muted,
        }
    }
}

/// One file of the comparison, by its path inside the two folders.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct FolderRow {
    pub(crate) relative: String,
    pub(crate) kind: RowKind,
}

/// What the scan read about one file before it looks inside.
struct FileMeta {
    path: PathBuf,
    size: u64,
}

/// One file operation the synchronization runs off the window thread.
#[derive(Clone, Debug)]
enum SyncOp {
    Copy { from: PathBuf, to: PathBuf },
    Delete { path: PathBuf },
}

/// A step with the row state it leaves behind; `None` means the row goes away.
#[derive(Clone, Debug)]
struct SyncStep {
    relative: String,
    op: SyncOp,
    after: Option<RowKind>,
}

/// Actions that wait for the user to confirm them.
#[derive(Clone, Debug)]
struct Pending {
    question: String,
    steps: Vec<SyncStep>,
}

pub struct FolderCompareView {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    left: PathBuf,
    right: PathBuf,
    rows: Vec<FolderRow>,
    show_same: bool,
    selected: Option<usize>,
    direction: Direction,
    scanning: bool,
    error: Option<String>,
    pending: Option<Pending>,
    cancel: Arc<AtomicBool>,
    scroll: ScrollHandle,
}

impl FolderCompareView {
    pub(crate) fn open(
        left: PathBuf,
        right: PathBuf,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            left,
            right,
            rows: Vec::new(),
            show_same: false,
            selected: None,
            direction: Direction::LeftToRight,
            scanning: true,
            error: None,
            pending: None,
            cancel: Arc::new(AtomicBool::new(false)),
            scroll: ScrollHandle::new(),
        });
        let focus = view.read(cx).focus.clone();
        view.update(cx, |view, cx| view.start_scan(cx));
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn rows(&self) -> &[FolderRow] {
        &self.rows
    }

    #[cfg(test)]
    pub(crate) fn visible_rows(&self) -> Vec<FolderRow> {
        self.rows
            .iter()
            .filter(|row| self.show_same || row.kind.differs())
            .cloned()
            .collect()
    }

    #[cfg(test)]
    pub(crate) fn is_scanning(&self) -> bool {
        self.scanning
    }

    #[cfg(test)]
    pub(crate) fn shows_same(&self) -> bool {
        self.show_same
    }

    #[cfg(test)]
    pub(crate) fn direction(&self) -> Direction {
        self.direction
    }

    #[cfg(test)]
    pub(crate) fn selected_relative(&self) -> Option<&str> {
        let index = self.selected?;
        Some(self.rows.get(index)?.relative.as_str())
    }

    #[cfg(test)]
    pub(crate) fn select(&mut self, relative: &str) -> bool {
        let Some(index) = self.rows.iter().position(|row| row.relative == relative) else {
            return false;
        };
        self.selected = Some(index);
        true
    }

    #[cfg(test)]
    pub(crate) fn is_confirming(&self) -> bool {
        self.pending.is_some()
    }

    fn start_scan(&mut self, cx: &mut Context<Self>) {
        self.scanning = true;
        self.error = None;
        let (left, right, cancel) = (self.left.clone(), self.right.clone(), self.cancel.clone());
        let job = cx
            .background_executor()
            .spawn(async move { compare_folders(&left, &right, &cancel) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| {
                if this.cancel.load(Ordering::Relaxed) {
                    return;
                }
                this.scanning = false;
                match result {
                    Ok(rows) => {
                        this.rows = rows;
                        this.selected = this.first_difference();
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn first_difference(&self) -> Option<usize> {
        self.rows.iter().position(|row| row.kind.differs())
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cancel.store(true, Ordering::Relaxed);
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn escape(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            self.pending = None;
            cx.notify();
        } else {
            self.close(window, cx);
        }
    }

    fn confirm(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        self.apply(pending.steps, cx);
    }

    fn next_diff(&mut self, delta: isize, cx: &mut Context<Self>) {
        let len = self.rows.len();
        if len == 0 {
            return;
        }
        let mut index = match (self.selected, delta > 0) {
            (Some(index), true) => index,
            (Some(index), false) => index,
            (None, true) => len - 1,
            (None, false) => 0,
        };
        for _ in 0..len {
            index = if delta > 0 {
                (index + 1) % len
            } else {
                (index + len - 1) % len
            };
            if self.rows[index].kind.differs() {
                self.selected = Some(index);
                self.scroll_to(index);
                cx.notify();
                return;
            }
        }
    }

    fn scroll_to(&self, index: usize) {
        let position = if self.show_same {
            index
        } else {
            self.rows[..index]
                .iter()
                .filter(|row| row.kind.differs())
                .count()
        };
        self.scroll.scroll_to_item(position);
    }

    /// Enter and Ctrl+Enter: the selected file, or every difference, copied to the other
    /// side. A file that is missing on the source side is the extra one and is deleted,
    /// which asks first.
    fn synchronize(&mut self, all: bool, cx: &mut Context<Self>) {
        if self.pending.is_some() {
            self.confirm(cx);
            return;
        }
        if self.scanning {
            return;
        }
        let indexes: Vec<usize> = if all {
            self.rows
                .iter()
                .enumerate()
                .filter(|(_, row)| row.kind.differs())
                .map(|(index, _)| index)
                .collect()
        } else {
            self.selected.into_iter().collect()
        };
        let steps: Vec<SyncStep> = indexes
            .into_iter()
            .filter_map(|index| self.step(index))
            .collect();
        if steps.is_empty() {
            return;
        }
        let deletes = steps
            .iter()
            .filter(|step| matches!(step.op, SyncOp::Delete { .. }))
            .count();
        if deletes > 0 {
            let question = if all {
                format!(
                    "Delete {deletes} extra file(s) from the {} folder?",
                    self.target_label()
                )
            } else {
                let relative = steps
                    .iter()
                    .find(|step| matches!(step.op, SyncOp::Delete { .. }))
                    .map_or("", |step| step.relative.as_str());
                format!(
                    "Delete \"{relative}\" from the {} folder?",
                    self.target_label()
                )
            };
            self.pending = Some(Pending { question, steps });
            cx.notify();
            return;
        }
        self.apply(steps, cx);
    }

    fn target_label(&self) -> &'static str {
        match self.direction {
            Direction::LeftToRight => "right",
            Direction::RightToLeft => "left",
        }
    }

    fn step(&self, index: usize) -> Option<SyncStep> {
        let row = self.rows.get(index)?;
        let left = self.left.join(&row.relative);
        let right = self.right.join(&row.relative);
        let (op, after) = match (self.direction, row.kind) {
            (_, RowKind::Same) => return None,
            (Direction::LeftToRight, RowKind::OnlyLeft | RowKind::Differ) => (
                SyncOp::Copy {
                    from: left,
                    to: right,
                },
                Some(RowKind::Same),
            ),
            (Direction::LeftToRight, RowKind::OnlyRight) => (SyncOp::Delete { path: right }, None),
            (Direction::RightToLeft, RowKind::OnlyRight | RowKind::Differ) => (
                SyncOp::Copy {
                    from: right,
                    to: left,
                },
                Some(RowKind::Same),
            ),
            (Direction::RightToLeft, RowKind::OnlyLeft) => (SyncOp::Delete { path: left }, None),
        };
        Some(SyncStep {
            relative: row.relative.clone(),
            op,
            after,
        })
    }

    fn apply(&mut self, steps: Vec<SyncStep>, cx: &mut Context<Self>) {
        let history = crate::history::get(cx);
        let job = cx
            .background_executor()
            .spawn(async move { run_steps(steps, history) });
        cx.spawn(async move |this, cx| {
            let result = job.await;
            this.update(cx, |this, cx| match result {
                Ok(done) => this.finish(done, cx),
                Err((error, done)) => {
                    this.finish(done, cx);
                    this.report(error, cx);
                }
            })
            .ok();
        })
        .detach();
    }

    fn finish(&mut self, done: Vec<SyncStep>, cx: &mut Context<Self>) {
        for step in done {
            let Some(index) = self
                .rows
                .iter()
                .position(|row| row.relative == step.relative)
            else {
                continue;
            };
            match step.after {
                Some(kind) => self.rows[index].kind = kind,
                None => {
                    self.rows.remove(index);
                    if let Some(selected) = self.selected {
                        if selected == index {
                            self.selected = None;
                        } else if selected > index {
                            self.selected = Some(selected - 1);
                        }
                    }
                }
            }
        }
        if self.selected.is_none() {
            self.selected = self.first_difference();
        }
        cx.notify();
    }

    fn report(&self, error: String, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.notify_user("Compare", error, cx)
            })
            .ok();
    }

    fn open_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = self.selected {
            self.open_diff(index, window, cx);
        }
    }

    fn open_diff(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index) else {
            return;
        };
        let left = self.left.join(&row.relative);
        let right = self.right.join(&row.relative);
        let Some(git) = cx.try_global::<ActiveGit>().map(|git| git.0.clone()) else {
            return;
        };
        let workspace = self.workspace.clone();
        self.workspace
            .update(cx, |_workspace, cx| {
                DiffView::compare_files(git, workspace, left, right, window, cx);
            })
            .ok();
    }

    fn file_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<AnyElement> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| self.show_same || row.kind.differs())
            .map(|(index, row)| {
                let selected = self.selected == Some(index);
                let hover = theme.hover;
                let background = theme.selection;
                let mark_color = row.kind.color(theme);
                let mark = row.kind.mark();
                let relative = row.relative.clone();
                div()
                    .id(("folder-compare-row", index))
                    .debug_selector(move || format!("folder-compare-row-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(background))
                    .when(!selected, |this| this.hover(move |style| style.bg(hover)))
                    .child(
                        div()
                            .flex_none()
                            .w(px(16.))
                            .text_color(mark_color)
                            .child(mark),
                    )
                    .child(div().flex_1().min_w_0().truncate().child(relative))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        this.selected = Some(index);
                        if event.click_count() >= 2 {
                            this.open_diff(index, window, cx);
                        }
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect()
    }
}

impl Drop for FolderCompareView {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

impl Render for FolderCompareView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(40.));
        let height = px(HEIGHT).min(viewport.height - px(40.));
        let left = (viewport.width - width) / 2.;
        let top = (viewport.height - height) / 2.;
        let heading = format!(
            "Compare '{}' with '{}'",
            folder_name(&self.left),
            folder_name(&self.right)
        );
        let differing = self.rows.iter().filter(|row| row.kind.differs()).count();
        let identical = self.rows.len() - differing;
        let status = if self.scanning {
            "Scanning the folders...".to_owned()
        } else if let Some(error) = &self.error {
            error.clone()
        } else {
            format!("{differing} file(s) differ, {identical} identical")
        };
        let rows = self.file_rows(&theme, cx);
        let rows_empty = rows.is_empty();
        let direction_icon = match self.direction {
            Direction::LeftToRight => "general/arrowRight",
            Direction::RightToLeft => "general/arrowLeft",
        };
        let direction_label = format!("Synchronize {}", self.direction.label());
        let filter_label = if self.show_same {
            "Hide Identical Files"
        } else {
            "Show Identical Files"
        };
        let body = surface(&theme)
            .id("folder-compare")
            .debug_selector(|| "folder-compare".to_owned())
            .track_focus(&self.focus)
            .key_context(CONTEXT)
            .occlude()
            .w(width)
            .h(height)
            .p(px(POPUP_PADDING))
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .h(px(CONTROL_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(TEXT_TITLE))
                            .child(heading),
                    )
                    .child(
                        icon_button(
                            "folder-compare-direction",
                            direction_icon,
                            direction_label,
                            Some("Enter".into()),
                            &theme,
                        )
                        .debug_selector(|| "folder-compare-direction".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.direction = this.direction.flipped();
                            cx.notify();
                        })),
                    )
                    .child(
                        icon_button(
                            "folder-compare-show-same",
                            "general/filter",
                            filter_label,
                            None,
                            &theme,
                        )
                        .debug_selector(|| "folder-compare-show-same".to_owned())
                        .when(self.show_same, |this| this.bg(theme.pressed))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.show_same = !this.show_same;
                            cx.notify();
                        })),
                    ),
            )
            .child(
                div()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child(status),
            )
            .child(
                scrollable(
                    "folder-compare-files",
                    &self.scroll,
                    div()
                        .id("folder-compare-files")
                        .debug_selector(|| "folder-compare-files".to_owned())
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows)
                        .when(rows_empty, |this| {
                            this.text_color(theme.text_muted).child(if self.scanning {
                                "Scanning..."
                            } else {
                                "No files differ"
                            })
                        }),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .when_some(self.pending.clone(), |this, pending| {
                this.child(
                    div()
                        .id("folder-compare-confirm")
                        .debug_selector(|| "folder-compare-confirm".to_owned())
                        .flex()
                        .flex_col()
                        .gap(px(SPACE_XS))
                        .p(px(SPACE_M))
                        .rounded(px(CONTROL_RADIUS))
                        .border_1()
                        .border_color(theme.border)
                        .bg(theme.popup_bg)
                        .child(pending.question)
                        .child(
                            div()
                                .text_color(theme.text_secondary)
                                .child("Enter to confirm, Escape to cancel"),
                        ),
                )
            })
            .on_action(cx.listener(|this, _: &NextDiff, _, cx| this.next_diff(1, cx)))
            .on_action(cx.listener(|this, _: &PreviousDiff, _, cx| this.next_diff(-1, cx)))
            .on_action(cx.listener(|this, _: &SynchronizeDiff, _, cx| this.synchronize(false, cx)))
            .on_action(
                cx.listener(|this, _: &SynchronizeDiffAll, _, cx| this.synchronize(true, cx)),
            )
            .on_action(cx.listener(|this, _: &OpenDiff, window, cx| this.open_selected(window, cx)))
            .on_action(cx.listener(|this, _: &CancelCompare, window, cx| this.escape(window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)));
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("folder-compare-enter", body))
    }
}

fn folder_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Reads both folders and puts their files in one list. The size of a file comes first:
/// files of different sizes differ without a read, and only the bytes of the same size
/// are compared.
fn compare_folders(
    left: &Path,
    right: &Path,
    cancel: &AtomicBool,
) -> Result<Vec<FolderRow>, String> {
    #[cfg(test)]
    scan_gate::wait(left);
    let left_files = collect(left, cancel).map_err(|error| error.to_string())?;
    let right_files = collect(right, cancel).map_err(|error| error.to_string())?;
    if cancel.load(Ordering::Relaxed) {
        return Ok(Vec::new());
    }
    let keys: BTreeSet<&String> = left_files.keys().chain(right_files.keys()).collect();
    let mut rows = Vec::with_capacity(keys.len());
    for relative in keys {
        if cancel.load(Ordering::Relaxed) {
            return Ok(Vec::new());
        }
        let kind = match (left_files.get(relative), right_files.get(relative)) {
            (Some(_), None) => RowKind::OnlyLeft,
            (None, Some(_)) => RowKind::OnlyRight,
            (Some(left_file), Some(right_file)) => {
                if left_file.size != right_file.size {
                    RowKind::Differ
                } else if same_bytes(&left_file.path, &right_file.path) {
                    RowKind::Same
                } else {
                    RowKind::Differ
                }
            }
            (None, None) => continue,
        };
        rows.push(FolderRow {
            relative: relative.clone(),
            kind,
        });
    }
    Ok(rows)
}

/// Walks one folder in the background and keeps the size and the time of every file.
fn collect(root: &Path, cancel: &AtomicBool) -> std::io::Result<BTreeMap<String, FileMeta>> {
    let mut files = BTreeMap::new();
    let mut queue = VecDeque::from([(root.to_path_buf(), String::new())]);
    while let Some((folder, prefix)) = queue.pop_front() {
        if cancel.load(Ordering::Relaxed) {
            break;
        }
        let Ok(entries) = std::fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            if cancel.load(Ordering::Relaxed) {
                break;
            }
            let name = entry.file_name().to_string_lossy().into_owned();
            if HIDDEN.contains(&name.as_str()) {
                continue;
            }
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                queue.push_back((entry.path(), relative));
            } else {
                let Ok(meta) = entry.metadata() else {
                    continue;
                };
                files.insert(
                    relative,
                    FileMeta {
                        path: entry.path(),
                        size: meta.len(),
                    },
                );
            }
        }
    }
    Ok(files)
}

fn same_bytes(left: &Path, right: &Path) -> bool {
    match (std::fs::read(left), std::fs::read(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// Copies or deletes the steps in order, taking a Local History snapshot of what the
/// operation is about to rewrite.
fn run_steps(
    steps: Vec<SyncStep>,
    history: Option<Arc<local_history::LocalHistory>>,
) -> Result<Vec<SyncStep>, (String, Vec<SyncStep>)> {
    let mut done = Vec::new();
    for step in steps {
        let result = match &step.op {
            SyncOp::Copy { from, to } => {
                let mut result = Ok(());
                if let Some(parent) = to.parent() {
                    result = std::fs::create_dir_all(parent);
                }
                if result.is_ok() {
                    if let Some(history) = &history {
                        let _ = history.record_file(to, Cause::BeforeReplace);
                    }
                    result = std::fs::copy(from, to).map(|_| ());
                }
                if result.is_ok()
                    && let Some(history) = &history
                {
                    let _ = history.record_file(to, Cause::Saved);
                }
                result
            }
            SyncOp::Delete { path } => {
                if let Some(history) = &history {
                    let _ = history.record_tree(path, Cause::BeforeDelete);
                }
                let result = delete_on_disk(path);
                if result.is_ok()
                    && let Some(history) = &history
                {
                    let _ = history.record_removed(path);
                }
                result
            }
        };
        match result {
            Ok(()) => done.push(step),
            Err(error) => return Err((format!("{}: {error}", step.relative), done)),
        }
    }
    Ok(done)
}

#[cfg(test)]
pub(crate) fn hold_scan(root: &Path) -> scan_gate::Hold {
    scan_gate::hold(root)
}

/// The tests hold the scan of one folder so that the window can be checked while the
/// disk is still being read.
#[cfg(test)]
mod scan_gate {
    use std::path::{Path, PathBuf};
    use std::sync::{Condvar, Mutex, OnceLock};

    struct Gate {
        held: Mutex<Option<PathBuf>>,
        wake: Condvar,
    }

    static GATE: OnceLock<Gate> = OnceLock::new();

    fn gate() -> &'static Gate {
        GATE.get_or_init(|| Gate {
            held: Mutex::new(None),
            wake: Condvar::new(),
        })
    }

    pub(crate) struct Hold;

    impl Drop for Hold {
        fn drop(&mut self) {
            *gate().held.lock().unwrap() = None;
            gate().wake.notify_all();
        }
    }

    pub(crate) fn hold(root: &Path) -> Hold {
        *gate().held.lock().unwrap() = Some(root.to_path_buf());
        Hold
    }

    pub(crate) fn wait(root: &Path) {
        let gate = gate();
        let mut held = gate.held.lock().unwrap();
        while held.as_deref() == Some(root) {
            held = gate.wake.wait(held).unwrap();
        }
    }
}

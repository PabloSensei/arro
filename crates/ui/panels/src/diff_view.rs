//! The changes of one file against the last commit: two editors, the version before on
//! the left and the working text on the right, with the words that differ marked and a
//! strip between the panes that takes one hunk back. Unified is the other layout: one
//! list of lines with the same hunk actions.

use std::ops::Range;
use std::path::{Path, PathBuf};

use editor_view::{EditorEvent, EditorView, PaneDecor};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{Hunk, TextEdit};
use workspace::{Settings, Workspace};

use crate::editor_tab::{options_of, style_of};
use crate::git::{GitState, Kind, relative_of};

const WIDTH: f32 = 1180.;
const CONTEXT_LINES: usize = 2;
const LINE_HEIGHT: f32 = 20.;
const STRIP_WIDTH: f32 = 68.;
const CONTEXT: &str = "DiffView";
/// The keys of the popup while one of its editors has the focus. The editor is deeper
/// than the popup in the key contexts, so the keys of the diff win over the keys of the
/// window there.
const EDITOR_CONTEXT: &str = "DiffEditor";

actions!(
    diff_view,
    [
        NextDiff,
        PreviousDiff,
        NextChange,
        PreviousChange,
        ApplyLeftSide,
        ApplyRightSide,
        FocusOppositePane
    ]
);

/// One key of the diff and the IDEA action it implements, if any.
#[cfg(test)]
pub(crate) struct DiffKey {
    pub(crate) keys: &'static str,
    pub(crate) action: &'static str,
    pub(crate) idea: Option<&'static str>,
}

macro_rules! diff_keys {
    ($(($keys:literal, $action:ident, $idea:expr)),* $(,)?) => {
        /// The diff keys, while the popup has the focus, and while one of its editors
        /// has it.
        pub(crate) fn bind_keys(cx: &mut App) {
            cx.bind_keys([
                $(
                    KeyBinding::new($keys, $action, Some(CONTEXT)),
                    KeyBinding::new($keys, $action, Some(EDITOR_CONTEXT)),
                )*
            ]);
        }

        #[cfg(test)]
        pub(crate) const KEYS: &[DiffKey] = &[
            $(DiffKey {
                keys: $keys,
                action: concat!("diff_view::", stringify!($action)),
                idea: $idea,
            }),*
        ];
    };
}

diff_keys![
    ("f7", NextDiff, Some("NextDiff")),
    ("shift-f7", PreviousDiff, Some("PreviousDiff")),
    ("alt-shift-right", NextChange, Some("Diff.NextChange")),
    ("alt-shift-left", PreviousChange, Some("Diff.PrevChange")),
    ("ctrl-alt-r", ApplyLeftSide, Some("Diff.ApplyLeftSide")),
    ("ctrl-alt-a", ApplyRightSide, Some("Diff.ApplyRightSide")),
    // Tab stays an indent in the editable pane, as in IDEA.
    (
        "ctrl-shift-tab",
        FocusOppositePane,
        Some("Diff.FocusOppositePane")
    ),
];

/// The pane the keys edit or read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    Left,
    Right,
}

/// Which editor an event came from: one of the panes, or the tab the right pane shares
/// its buffer with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorSide {
    Left,
    Right,
    Tab,
}

pub struct DiffView {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
    path: PathBuf,
    focus: FocusHandle,
    changes: Changes,
    source: Source,
    /// The files of the changes list, in the order the next file walks them.
    files: Vec<PathBuf>,
    /// Where `path` sits in `files`.
    index: usize,
    /// The hunk the diff keys act on.
    selected: usize,
    /// The open tab of the file. The right pane shows its buffer when it is there.
    tab: Option<Entity<EditorView>>,
    /// The version before, read only.
    left: Entity<EditorView>,
    /// The working text: the buffer of the tab, or an editor of its own.
    right: Entity<EditorView>,
    /// Side by side when the setting says so, one list otherwise.
    side_by_side: bool,
    /// The pane the keys work in.
    pane: Pane,
    /// Tells a late diff answer which edit it belongs to.
    serial: u64,
    /// A scroll of one pane is being copied to the other, so it must not copy back.
    syncing: bool,
    scroll: ScrollHandle,
}

/// What the diff is taken from: the working tree, one commit, or the clipboard.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Source {
    WorkingTree,
    Commit(String),
    /// The working tree against the head of a branch, as Compare shows it.
    Branch(String),
    Clipboard,
    /// Another file, the left side of Compare Two Files.
    File(PathBuf),
    /// A shelf, the left side of its diff is `HEAD`.
    Shelf(String),
}

/// What the file is now and what the last commit has, with `\n` line ends, and the hunks
/// between them.
struct Changes {
    current: String,
    base: String,
    hunks: Vec<Hunk>,
}

impl Changes {
    /// The edit that takes the hunk back, if the text is still the one the diff was taken
    /// from. `None` means the text moved, so the hunk may sit elsewhere now.
    fn revert(&self, index: usize, text_now: &str) -> Option<TextEdit> {
        if text_now != self.current {
            return None;
        }
        Some(vcs::revert_hunk(
            &self.base,
            &self.current,
            self.hunks.get(index)?,
        ))
    }
}

/// One line of one pane: its own number, the text and the words that differ from the
/// line on the other side.
pub(crate) struct DiffLine {
    number: usize,
    text: String,
    words: Vec<Range<usize>>,
}

/// One row of the side-by-side view. A line missing on a side means that side has no
/// text there, so both sides stay on the same height and the hunks line up.
pub(crate) struct DiffRow {
    left: Option<DiffLine>,
    right: Option<DiffLine>,
    /// The hunk the row belongs to; `None` is unchanged context.
    pub(crate) hunk: Option<usize>,
    kind: Option<Kind>,
}

/// The text of the file as the user sees it: the open editor, or the disk.
fn current_text(path: &Path, editor: Option<&Entity<EditorView>>, cx: &App) -> String {
    let text = editor
        .and_then(|editor| editor.read(cx).text())
        .or_else(|| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    text.replace("\r\n", "\n")
}

fn editor_of(
    workspace: &WeakEntity<Workspace>,
    path: &Path,
    cx: &App,
) -> Option<Entity<EditorView>> {
    let open = workspace
        .read_with(cx, |workspace, _| workspace.open_editors())
        .ok()?;
    let (_, view) = open.into_iter().find(|(open, _)| open == path)?;
    view.downcast::<EditorView>().ok()
}

/// The lines of a text as the editor shows them: the empty line after a trailing newline
/// is a line of its own.
fn lines_of(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

/// The byte offset of the start of every line.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect()
}

/// The kind of change a hunk is, for the colour of its lines and its strip.
fn hunk_kind(hunk: &Hunk) -> Kind {
    match (hunk.before.is_empty(), hunk.after.is_empty()) {
        (true, _) => Kind::Added,
        (false, true) => Kind::Deleted,
        (false, false) => Kind::Modified,
    }
}

fn line_tint(kind: Option<Kind>, right: bool, theme: &Theme) -> Option<Hsla> {
    match kind? {
        Kind::Added if !right => None,
        Kind::Deleted if right => None,
        kind => Some(kind.color(theme.dark)),
    }
}

fn word_tint(kind: Option<Kind>, right: bool, theme: &Theme) -> Option<Hsla> {
    (kind == Some(Kind::Modified)).then(|| {
        if right {
            Kind::Added.color(theme.dark)
        } else {
            Kind::Conflicted.color(theme.dark)
        }
    })
}

/// The rows of the side-by-side view: the whole file on both sides, with the lines of
/// one hunk facing each other and blank lines where a side has none.
pub(crate) fn rows_of(base: &str, current: &str, hunks: &[Hunk]) -> Vec<DiffRow> {
    let before = lines_of(base);
    let after = lines_of(current);
    let line = |number: usize, text: &str| DiffLine {
        number,
        text: text.to_owned(),
        words: Vec::new(),
    };
    let mut rows = Vec::new();
    let (mut b, mut a) = (0, 0);

    let push_unchanged = |rows: &mut Vec<DiffRow>, b: usize, a: usize| {
        rows.push(DiffRow {
            left: before.get(b).map(|text| line(b + 1, text)),
            right: after.get(a).map(|text| line(a + 1, text)),
            hunk: None,
            kind: None,
        });
    };

    for (index, hunk) in hunks.iter().enumerate() {
        let b_from = (hunk.before.start as usize).min(before.len());
        let a_from = (hunk.after.start as usize).min(after.len());
        while b < b_from && a < a_from {
            push_unchanged(&mut rows, b, a);
            b += 1;
            a += 1;
        }
        while b < b_from {
            push_unchanged(&mut rows, b, a);
            b += 1;
        }
        while a < a_from {
            push_unchanged(&mut rows, b, a);
            a += 1;
        }

        let b_to = (hunk.before.end as usize).min(before.len()).max(b_from);
        let a_to = (hunk.after.end as usize).min(after.len()).max(a_from);
        let kind = hunk_kind(hunk);
        let count = (b_to - b_from).max(a_to - a_from);
        for offset in 0..count {
            let left_index = b_from + offset;
            let right_index = a_from + offset;
            let mut left = before
                .get(left_index)
                .map(|text| line(left_index + 1, text));
            let mut right = after
                .get(right_index)
                .map(|text| line(right_index + 1, text));
            let same = match (&left, &right) {
                (Some(left), Some(right)) => left.text == right.text,
                (None, None) => true,
                _ => false,
            };
            if kind == Kind::Modified
                && let (Some(left), Some(right)) = (&mut left, &mut right)
                && !same
            {
                let changes = vcs::word_changes(&left.text, &right.text);
                left.words = changes.before;
                right.words = changes.after;
            }
            rows.push(DiffRow {
                left,
                right,
                hunk: Some(index),
                kind: (!same).then_some(kind),
            });
        }
        b = b_to;
        a = a_to;
    }

    while b < before.len() || a < after.len() {
        push_unchanged(&mut rows, b, a);
        b += 1;
        a += 1;
    }
    rows
}

/// The decorations of two panes whose folds may hide some buffer lines. The rows that
/// stand in for a line the other side has, or for rows a fold took away, are counted in
/// display rows, so the first row of every hunk stays on the same height on both sides.
pub(crate) fn decorations_with_visible(
    rows: &[DiffRow],
    base: &str,
    current: &str,
    selected: usize,
    theme: &Theme,
    left_visible: &[usize],
    right_visible: &[usize],
) -> (PaneDecor, PaneDecor) {
    use std::collections::BTreeMap;

    let base_starts = line_starts(base);
    let current_starts = line_starts(current);
    let left_shown = shown_lines(lines_of(base).len(), left_visible);
    let right_shown = shown_lines(lines_of(current).len(), right_visible);
    let left_anchor = anchors(
        rows,
        |row| row.left.as_ref().map(|line| line.number - 1),
        &left_shown,
    );
    let right_anchor = anchors(
        rows,
        |row| row.right.as_ref().map(|line| line.number - 1),
        &right_shown,
    );
    let mut left = PaneDecor::default();
    let mut right = PaneDecor::default();
    let mut left_fillers = BTreeMap::new();
    let mut right_fillers = BTreeMap::new();
    let (mut left_rows, mut right_rows) = (0usize, 0usize);
    for (index, row) in rows.iter().enumerate() {
        if left_rows < right_rows {
            put_filler(
                &mut left_fillers,
                &mut left.trailing_fillers,
                left_anchor[index],
                right_rows - left_rows,
            );
            left_rows = right_rows;
        } else if right_rows < left_rows {
            put_filler(
                &mut right_fillers,
                &mut right.trailing_fillers,
                right_anchor[index],
                left_rows - right_rows,
            );
            right_rows = left_rows;
        }
        if let Some(line) = &row.left {
            let start = base_starts
                .get(line.number - 1)
                .copied()
                .unwrap_or_default();
            if let Some(color) = line_color(row, false, selected, theme) {
                left.lines.push((line.number - 1, color));
            }
            if let Some(color) = word_tint(row.kind, false, theme) {
                for range in &line.words {
                    left.ranges
                        .push((start + range.start..start + range.end, color));
                }
            }
            if left_shown.get(line.number - 1).copied().unwrap_or(false) {
                left_rows += 1;
            }
        }
        if let Some(line) = &row.right {
            let start = current_starts
                .get(line.number - 1)
                .copied()
                .unwrap_or_default();
            if let Some(color) = line_color(row, true, selected, theme) {
                right.lines.push((line.number - 1, color));
            }
            if let Some(color) = word_tint(row.kind, true, theme) {
                for range in &line.words {
                    right
                        .ranges
                        .push((start + range.start..start + range.end, color));
                }
            }
            if right_shown.get(line.number - 1).copied().unwrap_or(false) {
                right_rows += 1;
            }
        }
    }
    if left_rows < right_rows {
        left.trailing_fillers += right_rows - left_rows;
    } else if right_rows < left_rows {
        right.trailing_fillers += left_rows - right_rows;
    }
    left.fillers = left_fillers.into_iter().collect();
    right.fillers = right_fillers.into_iter().collect();
    (left, right)
}

/// Which buffer lines the display has a row for, by their number from zero.
fn shown_lines(count: usize, visible: &[usize]) -> Vec<bool> {
    let mut shown = vec![false; count];
    for line in visible {
        if let Some(slot) = shown.get_mut(*line) {
            *slot = true;
        }
    }
    shown
}

/// The first visible line at or after each row, for an empty row of that side.
fn anchors(
    rows: &[DiffRow],
    line: impl Fn(&DiffRow) -> Option<usize>,
    shown: &[bool],
) -> Vec<Option<usize>> {
    let mut found = vec![None; rows.len()];
    let mut next = None;
    for index in (0..rows.len()).rev() {
        if let Some(line) = line(&rows[index])
            && shown.get(line).copied().unwrap_or(false)
        {
            next = Some(line);
        }
        found[index] = next;
    }
    found
}

fn put_filler(
    fillers: &mut std::collections::BTreeMap<usize, usize>,
    trailing: &mut usize,
    anchor: Option<usize>,
    count: usize,
) {
    match anchor {
        Some(line) => *fillers.entry(line).or_default() += count,
        None => *trailing += count,
    }
}

fn line_color(row: &DiffRow, right: bool, selected: usize, theme: &Theme) -> Option<Hsla> {
    let tint = line_tint(row.kind, right, theme);
    if row.hunk == Some(selected) {
        tint.map(|tint| tint.opacity(0.28))
            .or(Some(theme.selection_inactive))
    } else {
        tint.map(|tint| tint.opacity(0.18))
    }
}

/// The first and last row of every hunk, for the strip.
pub(crate) fn hunk_row_spans(rows: &[DiffRow], hunks: usize) -> Vec<Option<(usize, usize)>> {
    (0..hunks)
        .map(|hunk| {
            let first = rows.iter().position(|row| row.hunk == Some(hunk))?;
            let last = rows
                .iter()
                .rposition(|row| row.hunk == Some(hunk))
                .unwrap_or(first);
            Some((first, last))
        })
        .collect()
}

impl DiffView {
    /// Reads the diff in the background and opens the popup when it is there.
    pub fn open(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        window: &mut Window,
        cx: &mut App,
    ) {
        let files = vec![path.clone()];
        Self::open_list(git, workspace, files, 0, window, cx);
    }

    /// Opens the diff of one file of a changes list, so that the diff keys walk the list.
    pub fn open_list(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        files: Vec<PathBuf>,
        index: usize,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(path) = files.get(index).cloned() else {
            return;
        };
        Self::open_source(
            git,
            workspace,
            path,
            Source::WorkingTree,
            files,
            index,
            window,
            cx,
        );
    }

    /// The diff of a file as one commit changed it, for the Git Log window.
    pub fn open_commit(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        commit: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        let files = vec![path.clone()];
        Self::open_source(
            git,
            workspace,
            path,
            Source::Commit(commit),
            files,
            0,
            window,
            cx,
        );
    }

    /// The working tree of a file of a Compare window against the head of `branch`.
    pub fn open_branch(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        files: Vec<PathBuf>,
        index: usize,
        branch: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(path) = files.get(index).cloned() else {
            return;
        };
        Self::open_source(
            git,
            workspace,
            path,
            Source::Branch(branch),
            files,
            index,
            window,
            cx,
        );
    }

    /// Ctrl+D on a file of a shelf: `HEAD` against the version the shelf holds.
    pub fn open_shelf(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        shelf: String,
        path: PathBuf,
        patch: Vec<u8>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(repo) = git.read(cx).repo() else {
            return;
        };
        let job_path = path.clone();
        let job = cx.background_executor().spawn(async move {
            let relative = relative_of(repo.root(), &job_path);
            repo.shelf_diff(&patch, &relative).ok().flatten()
        });
        window
            .spawn(cx, async move |cx| {
                let Some(diff) = job.await else {
                    return;
                };
                cx.update(|window, cx| {
                    Self::show(
                        git,
                        workspace,
                        path,
                        Source::Shelf(shelf),
                        Vec::new(),
                        0,
                        diff.current,
                        diff.base,
                        diff.hunks,
                        window,
                        cx,
                    );
                })
                .ok();
            })
            .detach();
    }

    /// Ctrl+D on one file: the difference between the file and the clipboard.
    pub fn compare_clipboard(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        clipboard: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        Self::open_text(
            git,
            workspace,
            path,
            Source::Clipboard,
            window,
            cx,
            move |path| {
                let current = std::fs::read_to_string(path)
                    .unwrap_or_default()
                    .replace("\r\n", "\n");
                (clipboard, current)
            },
        );
    }

    /// Ctrl+D on two files of the project tree: `left` against `right`, both read in
    /// the background.
    pub fn compare_files(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        left: PathBuf,
        right: PathBuf,
        window: &mut Window,
        cx: &mut App,
    ) {
        let other = left.clone();
        Self::open_text(
            git,
            workspace,
            right,
            Source::File(left),
            window,
            cx,
            move |path| {
                let read = |path: &Path| {
                    std::fs::read_to_string(path)
                        .unwrap_or_default()
                        .replace("\r\n", "\n")
                };
                (read(&other), read(path))
            },
        );
    }

    #[allow(clippy::too_many_arguments)]
    fn open_source(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        source: Source,
        files: Vec<PathBuf>,
        index: usize,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(repo) = git.read(cx).repo() else {
            return;
        };
        let current = match source {
            Source::WorkingTree | Source::Branch(_) => {
                current_text(&path, editor_of(&workspace, &path, cx).as_ref(), cx)
            }
            Source::Commit(_) | Source::Shelf(_) => String::new(),
            Source::Clipboard | Source::File(_) => String::new(),
        };
        let (job_path, job_text) = (path.clone(), current.clone());
        let job_source = source.clone();
        let job = cx.background_executor().spawn(async move {
            let relative = relative_of(repo.root(), &job_path);
            let mut blobs = repo.blobs().ok()?;
            match &job_source {
                Source::WorkingTree => repo.file_diff(&mut blobs, &relative, &job_text).ok(),
                Source::Branch(branch) => repo
                    .file_diff_with(&mut blobs, branch, &relative, &job_text)
                    .ok(),
                Source::Commit(commit) => repo.commit_file_diff(&mut blobs, commit, &relative).ok(),
                Source::Clipboard | Source::File(_) | Source::Shelf(_) => None,
            }
        });
        window
            .spawn(cx, async move |cx| {
                let Some(diff) = job.await else {
                    return;
                };
                cx.update(|window, cx| {
                    // A commit has both sides in the blob; the working tree has the
                    // current text that was read above.
                    let current = if matches!(source, Source::Commit(_)) {
                        diff.current
                    } else {
                        current
                    };
                    Self::show(
                        git, workspace, path, source, files, index, current, diff.base, diff.hunks,
                        window, cx,
                    );
                })
                .ok();
            })
            .detach();
    }

    /// A diff that is not taken from git: two files, or one file and the clipboard.
    fn open_text(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        source: Source,
        window: &mut Window,
        cx: &mut App,
        read: impl FnOnce(&Path) -> (String, String) + Send + 'static,
    ) {
        let job_path = path.clone();
        let job = cx
            .background_executor()
            .spawn(async move { read(&job_path) });
        window
            .spawn(cx, async move |cx| {
                let (base, current) = job.await;
                cx.update(|window, cx| {
                    let hunks = vcs::line_hunks(&base, &current);
                    Self::show(
                        git,
                        workspace,
                        path,
                        source,
                        Vec::new(),
                        0,
                        current,
                        base,
                        hunks,
                        window,
                        cx,
                    );
                })
                .ok();
            })
            .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn show(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        source: Source,
        files: Vec<PathBuf>,
        index: usize,
        current: String,
        base: String,
        hunks: Vec<Hunk>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(shared) = workspace.upgrade() else {
            return;
        };
        let weak = workspace.clone();
        let tab = editor_of(&workspace, &path, cx);
        let side_by_side = cx
            .try_global::<Settings>()
            .is_none_or(|settings| settings.diff_side_by_side);
        let pane = if source == Source::WorkingTree {
            Pane::Right
        } else {
            Pane::Left
        };
        let style = style_of(cx);
        let options = options_of(cx);
        let left = {
            let path = path.clone();
            let base = base.clone();
            let style = style.clone();
            let options = options.clone();
            cx.new(move |cx| {
                let mut view = EditorView::open_text(path, base, true, style, cx);
                view.set_options(options, cx);
                view
            })
        };
        let right = match (&tab, source == Source::WorkingTree) {
            (Some(tab), true) => EditorView::open_shared(tab, cx).unwrap_or_else(|| {
                let text = current.clone();
                let path = path.clone();
                let style = style.clone();
                let options = options.clone();
                cx.new(move |cx| {
                    let mut view = EditorView::open_text(path, text, false, style, cx);
                    view.set_options(options, cx);
                    view.lock_soft_wrap();
                    view
                })
            }),
            _ => {
                let text = current.clone();
                let read_only = source != Source::WorkingTree;
                let path = path.clone();
                let style = style.clone();
                cx.new(move |cx| {
                    let mut view = EditorView::open_text(path, text, read_only, style, cx);
                    view.set_options(options, cx);
                    view.lock_soft_wrap();
                    view
                })
            }
        };
        left.update(cx, |left, _| left.lock_soft_wrap());
        let top = right.read(cx).scroll_top();
        left.update(cx, |left, cx| left.scroll_to_top(top, cx));
        let pane_focus = match pane {
            Pane::Left => left.read(cx).focus_handle(cx),
            Pane::Right => right.read(cx).focus_handle(cx),
        };
        let window_handle = window.window_handle();
        shared.update(cx, |workspace, cx| {
            let view = cx.new(|cx| Self {
                git,
                workspace: weak,
                window: window_handle,
                path,
                focus: cx.focus_handle(),
                changes: Changes {
                    current,
                    base,
                    hunks,
                },
                source,
                files,
                index,
                selected: 0,
                tab,
                left,
                right,
                side_by_side,
                pane,
                serial: 0,
                syncing: false,
                scroll: ScrollHandle::new(),
            });
            view.update(cx, |view, cx| view.subscribe_editors(cx));
            let focus = view.read(cx).focus.clone();
            workspace.show_overlay(view.into(), focus, window, cx);
        });
        if side_by_side {
            window.focus(&pane_focus, cx);
            // The editor joins the dispatch tree with the first frame: the keys are its
            // own once it is there.
            let focus = pane_focus.clone();
            window.on_next_frame(move |window, cx| window.focus(&focus, cx));
        }
    }

    fn revertable(&self) -> bool {
        self.source == Source::WorkingTree
    }

    /// Follows the text of the right pane and the scroll of both panes. The tab shares
    /// its buffer with the right pane, so an edit in either one comes here.
    fn subscribe_editors(&mut self, cx: &mut Context<Self>) {
        cx.subscribe(
            &self.left,
            |this: &mut Self, source: Entity<EditorView>, event: &EditorEvent, cx| {
                if this.left == source {
                    this.editor_event(EditorSide::Left, event, cx);
                }
            },
        )
        .detach();
        cx.subscribe(
            &self.right,
            |this: &mut Self, source: Entity<EditorView>, event: &EditorEvent, cx| {
                if this.right == source {
                    this.editor_event(EditorSide::Right, event, cx);
                }
            },
        )
        .detach();
        if let Some(tab) = self.tab.clone()
            && tab != self.right
        {
            cx.subscribe(
                &tab,
                |this: &mut Self, source: Entity<EditorView>, event: &EditorEvent, cx| {
                    if this.tab.as_ref() == Some(&source) {
                        this.editor_event(EditorSide::Tab, event, cx);
                    }
                },
            )
            .detach();
        }
    }

    fn editor_event(&mut self, from: EditorSide, event: &EditorEvent, cx: &mut Context<Self>) {
        match event {
            EditorEvent::Changed => self.text_changed(from, cx),
            EditorEvent::Scrolled(top) if from != EditorSide::Tab => {
                let pane = if from == EditorSide::Left {
                    Pane::Left
                } else {
                    Pane::Right
                };
                self.scrolled(pane, *top, cx);
            }
            // A fold, an unfold or a caret move changes the rows a pane shows, and with
            // them the empty rows that keep the hunks face to face.
            EditorEvent::Moved if from != EditorSide::Left => cx.notify(),
            _ => {}
        }
    }

    /// The text of one view of the buffer changed: the other view takes the same text,
    /// and the hunks are read again.
    fn text_changed(&mut self, from: EditorSide, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        if from == EditorSide::Right {
            let path = self.path.clone();
            self.workspace
                .update(cx, |workspace, cx| workspace.remember_edited(&path, cx))
                .ok();
        }
        let Some((source, target)) = self.synced_pair(from) else {
            if from != EditorSide::Left {
                self.reread(cx);
            }
            return;
        };
        let source_text = source.read(cx).text().unwrap_or_default();
        let target_text = target.read(cx).text().unwrap_or_default();
        if target_text != source_text
            && let Some(edit) = text_core::difference(&target_text, &source_text)
        {
            self.syncing = true;
            target.update(cx, |target, cx| target.follow_edits(vec![edit], cx));
            self.syncing = false;
        }
        self.reread(cx);
    }

    /// The two editors that share one buffer, when the tab of the file is open.
    fn synced_pair(&self, from: EditorSide) -> Option<(Entity<EditorView>, Entity<EditorView>)> {
        let tab = self.tab.clone().filter(|tab| tab != &self.right)?;
        match from {
            EditorSide::Tab => Some((tab, self.right.clone())),
            EditorSide::Right => Some((self.right.clone(), tab)),
            EditorSide::Left => None,
        }
    }

    /// One pane scrolled: the other one follows, so the hunks stay face to face.
    fn scrolled(&mut self, from: Pane, top: f32, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        let other = match from {
            Pane::Left => &self.right,
            Pane::Right => &self.left,
        };
        if (other.read(cx).scroll_top() - top).abs() <= 0.5 {
            return;
        }
        self.syncing = true;
        other.update(cx, |other, cx| other.scroll_to_top(top, cx));
        self.syncing = false;
        cx.notify();
    }

    /// F7 and Shift+F7: the next or previous hunk; the last one goes to the next file.
    fn next_diff(&mut self, cx: &mut Context<Self>) {
        if self.selected + 1 < self.changes.hunks.len() {
            self.selected += 1;
            self.scroll_to_selected(cx);
            cx.notify();
        } else {
            self.move_file(1, false, cx);
        }
    }

    fn previous_diff(&mut self, cx: &mut Context<Self>) {
        if self.selected > 0 {
            self.selected -= 1;
            self.scroll_to_selected(cx);
            cx.notify();
        } else {
            self.move_file(-1, true, cx);
        }
    }

    /// Alt+Shift+Right and Alt+Shift+Left: the next or previous file of the list.
    fn next_change(&mut self, cx: &mut Context<Self>) {
        self.move_file(1, false, cx);
    }

    fn previous_change(&mut self, cx: &mut Context<Self>) {
        self.move_file(-1, true, cx);
    }

    /// Scrolls both panes so that the selected hunk comes into sight, with two lines of
    /// the hunk above the top of the pane.
    fn scroll_to_selected(&mut self, cx: &mut Context<Self>) {
        if !self.side_by_side {
            self.scroll.scroll_to_item(self.selected);
            return;
        }
        let Some(row) = self
            .rows()
            .iter()
            .position(|row| row.hunk == Some(self.selected))
        else {
            return;
        };
        let line_height = self.right.read(cx).line_height().max(1.);
        let top = (row as f32 * line_height - line_height * 2.).max(0.);
        self.syncing = true;
        for editor in [&self.left, &self.right] {
            editor.update(cx, |editor, cx| editor.scroll_to_top(top, cx));
        }
        self.syncing = false;
        cx.notify();
    }

    /// Shows another file of the list. `last` selects the last hunk, as going back does.
    fn move_file(&mut self, step: isize, last: bool, cx: &mut Context<Self>) {
        if !matches!(
            self.source,
            Source::WorkingTree | Source::Commit(_) | Source::Branch(_)
        ) {
            return;
        }
        let next = self.index as isize + step;
        if next < 0 || next as usize >= self.files.len() {
            return;
        }
        let index = next as usize;
        let Some(path) = self.files.get(index).cloned() else {
            return;
        };
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let source = self.source.clone();
        let tab = editor_of(&self.workspace, &path, cx);
        let current = match source {
            Source::WorkingTree | Source::Branch(_) => current_text(&path, tab.as_ref(), cx),
            _ => String::new(),
        };
        let (job_path, job_text) = (path.clone(), current.clone());
        let job = cx.background_executor().spawn(async move {
            let relative = relative_of(repo.root(), &job_path);
            let mut blobs = repo.blobs().ok()?;
            match source {
                Source::WorkingTree => repo.file_diff(&mut blobs, &relative, &job_text).ok(),
                Source::Branch(branch) => repo
                    .file_diff_with(&mut blobs, &branch, &relative, &job_text)
                    .ok(),
                Source::Commit(commit) => {
                    repo.commit_file_diff(&mut blobs, &commit, &relative).ok()
                }
                Source::Clipboard | Source::File(_) | Source::Shelf(_) => None,
            }
        });
        self.serial += 1;
        let serial = self.serial;
        cx.spawn(async move |this, cx| {
            let Some(diff) = job.await else {
                return;
            };
            this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                let selected = if last {
                    diff.hunks.len().saturating_sub(1)
                } else {
                    0
                };
                let current = if matches!(this.source, Source::Commit(_)) {
                    diff.current
                } else {
                    current
                };
                this.tab = editor_of(&this.workspace, &path, cx);
                this.set_text(&path, &diff.base, &current, cx);
                this.path = path;
                this.changes = Changes {
                    current,
                    base: diff.base,
                    hunks: diff.hunks,
                };
                this.index = index;
                this.selected = selected;
                if this.side_by_side {
                    let editor = match this.pane {
                        Pane::Left => &this.left,
                        Pane::Right => &this.right,
                    };
                    let focus = editor.read(cx).focus_handle(cx);
                    this.window
                        .update(cx, |_, window, cx| window.focus(&focus, cx))
                        .ok();
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn revert(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.revertable() {
            return;
        }
        let text_now = self.right.read(cx).text().unwrap_or_default();
        let Some(TextEdit { range, text }) = self.changes.revert(index, &text_now) else {
            // The text moved while the popup was open: the old hunk may land in the wrong
            // place, so read the diff again and say why nothing was taken back.
            self.git.update(cx, |git, cx| {
                git.tell("Revert", "The text changed, so the diff was read again", cx)
            });
            self.reread(cx);
            return;
        };
        let path = self.path.clone();
        if let Some(tab) = self.tab.clone() {
            let bytes = tab.read(cx).file_bytes();
            let cause = local_history::Cause::BeforeGitRevert;
            crate::history::snapshot_bytes(&path, bytes, cause, cx);
            self.syncing = true;
            tab.update(cx, |tab, cx| tab.replace_text(range.clone(), &text, cx));
            self.syncing = false;
            self.workspace
                .update(cx, |workspace, cx| workspace.save_editor(&path, cx))
                .ok();
        } else {
            // The right pane is an editor of its own: it writes the file when it saves.
            if let Some(history) = crate::history::get(cx) {
                let _ = history.record_file(&path, local_history::Cause::BeforeGitRevert);
            }
            let right = self.right.clone();
            self.syncing = true;
            right.update(cx, |right, cx| right.replace_text(range, &text, cx));
            let error = right.update(cx, |right, cx| right.save_blocking(cx));
            self.syncing = false;
            if let Some(error) = error {
                let message = format!("{}: {error}", path.display());
                self.git
                    .update(cx, |git, cx| git.report("Revert", message, cx));
                return;
            }
        }
        self.reread(cx);
    }

    /// Ctrl+Alt+R: the left side of the selected change lands in the working tree.
    fn apply_left(&mut self, cx: &mut Context<Self>) {
        if self.changes.hunks.is_empty() {
            return;
        }
        self.revert(self.selected, cx);
    }

    /// Ctrl+Alt+A: the right side stays, and the diff moves on to the next change.
    fn apply_right(&mut self, cx: &mut Context<Self>) {
        if self.changes.hunks.is_empty() {
            return;
        }
        self.next_diff(cx);
    }

    /// Tab and Ctrl+Shift+Tab: the keys work in the other pane.
    fn focus_opposite(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.side_by_side {
            return;
        }
        self.pane = match self.pane {
            Pane::Left => Pane::Right,
            Pane::Right => Pane::Left,
        };
        self.focus_pane(window, cx);
        self.scroll_to_selected(cx);
        cx.notify();
    }

    /// The keys of the diff while a pane has the focus. The bindings of the popup work
    /// when the popup itself has the keys; an editor inside it takes the keys first, so
    /// the keys are read here as well.
    fn diff_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        match (key, modifiers.control, modifiers.alt, modifiers.shift) {
            ("f7", false, false, false) => self.next_diff(cx),
            ("f7", false, false, true) => self.previous_diff(cx),
            ("right", false, true, true) => self.next_change(cx),
            ("left", false, true, true) => self.previous_change(cx),
            ("r", true, true, false) => self.apply_left(cx),
            ("a", true, true, false) => self.apply_right(cx),
            ("tab", true, false, true) => self.focus_opposite(window, cx),
            _ => return false,
        }
        true
    }

    /// Gives the keys to the pane the diff works in.
    fn focus_pane(&self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = match self.pane {
            Pane::Left => &self.left,
            Pane::Right => &self.right,
        };
        let focus = editor.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
    }

    /// Switch Side-by-side / Unified and keep it in the settings file.
    fn toggle_layout(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.side_by_side = !self.side_by_side;
        let side_by_side = self.side_by_side;
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.update_settings(|settings| settings.diff_side_by_side = side_by_side, cx)
            })
            .ok();
        if side_by_side {
            self.focus_pane(window, cx);
        } else {
            window.focus(&self.focus, cx);
        }
        cx.notify();
    }

    /// Takes the diff again after a change.
    fn reread(&mut self, cx: &mut Context<Self>) {
        if !self.revertable() {
            return;
        }
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let current = self
            .right
            .read(cx)
            .text()
            .map(|text| text.replace("\r\n", "\n"))
            .unwrap_or_default();
        let (path, text) = (self.path.clone(), current.clone());
        let job = cx.background_executor().spawn(async move {
            let relative = relative_of(repo.root(), &path);
            let mut blobs = repo.blobs().ok()?;
            repo.file_diff(&mut blobs, &relative, &text).ok()
        });
        self.serial += 1;
        let serial = self.serial;
        cx.spawn(async move |this, cx| {
            let Some(diff) = job.await else {
                return;
            };
            this.update(cx, |this, cx| {
                if this.serial != serial {
                    return;
                }
                this.tab = editor_of(&this.workspace, &this.path, cx);
                this.changes = Changes {
                    current,
                    base: diff.base,
                    hunks: diff.hunks,
                };
                this.selected = this
                    .selected
                    .min(this.changes.hunks.len().saturating_sub(1));
                this.git.update(cx, |git, cx| git.refresh(cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Replaces both panes with editors of `base` and `current`, and follows them.
    fn set_text(&mut self, path: &Path, base: &str, current: &str, cx: &mut Context<Self>) {
        let style = style_of(cx);
        let options = options_of(cx);
        let left = {
            let path = path.to_path_buf();
            let base = base.to_owned();
            let style = style.clone();
            let options = options.clone();
            cx.new(move |cx| {
                let mut view = EditorView::open_text(path, base, true, style, cx);
                view.set_options(options, cx);
                view
            })
        };
        let right = match (&self.tab, self.source == Source::WorkingTree) {
            (Some(tab), true) => EditorView::open_shared(tab, cx).unwrap_or_else(|| {
                let path = path.to_path_buf();
                let current = current.to_owned();
                let style = style.clone();
                let options = options.clone();
                cx.new(move |cx| {
                    let mut view = EditorView::open_text(path, current, false, style, cx);
                    view.set_options(options, cx);
                    view.lock_soft_wrap();
                    view
                })
            }),
            _ => {
                let path = path.to_path_buf();
                let current = current.to_owned();
                let read_only = self.source != Source::WorkingTree;
                let style = style.clone();
                cx.new(move |cx| {
                    let mut view = EditorView::open_text(path, current, read_only, style, cx);
                    view.set_options(options, cx);
                    view.lock_soft_wrap();
                    view
                })
            }
        };
        left.update(cx, |left, _| left.lock_soft_wrap());
        self.left = left;
        self.right = right;
        self.subscribe_editors(cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// The file the diff shows, for tests.
    #[cfg(test)]
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// The selected hunk, for tests.
    #[cfg(test)]
    pub(crate) fn hunk_index(&self) -> usize {
        self.selected
    }

    /// How many hunks the diff has, for tests.
    #[cfg(test)]
    pub(crate) fn hunk_count(&self) -> usize {
        self.changes.hunks.len()
    }

    /// Whether the two panes are shown, for tests.
    #[cfg(test)]
    pub(crate) fn side_by_side(&self) -> bool {
        self.side_by_side
    }

    /// The pane the keys work in, for tests.
    #[cfg(test)]
    pub(crate) fn pane(&self) -> &'static str {
        match self.pane {
            Pane::Left => "left",
            Pane::Right => "right",
        }
    }

    /// The text of the right version, for tests.
    #[cfg(test)]
    pub(crate) fn current(&self) -> &str {
        &self.changes.current
    }

    /// The editor of the left pane, for tests.
    #[cfg(test)]
    pub(crate) fn left_editor(&self) -> Entity<EditorView> {
        self.left.clone()
    }

    /// The editor of the right pane, for tests.
    #[cfg(test)]
    pub(crate) fn right_editor(&self) -> Entity<EditorView> {
        self.right.clone()
    }

    /// The buffer line, from zero, of the first row of every hunk on both sides, for
    /// tests of the row alignment.
    #[cfg(test)]
    pub(crate) fn hunk_starts(&self) -> Vec<(Option<usize>, Option<usize>)> {
        let rows = self.rows();
        hunk_row_spans(&rows, self.changes.hunks.len())
            .into_iter()
            .map(|span| {
                let (left, right) = span.map_or((None, None), |(first, _)| {
                    let row = &rows[first];
                    (
                        row.left.as_ref().map(|line| line.number - 1),
                        row.right.as_ref().map(|line| line.number - 1),
                    )
                });
                (left, right)
            })
            .collect()
    }

    /// The text of every side-by-side row, for tests.
    #[cfg(test)]
    pub(crate) fn side_rows_text(&self) -> Vec<(Option<String>, Option<String>)> {
        self.rows()
            .into_iter()
            .map(|row| {
                (
                    row.left.map(|line| line.text),
                    row.right.map(|line| line.text),
                )
            })
            .collect()
    }

    /// The changed words of every side-by-side row, for tests.
    #[cfg(test)]
    #[allow(clippy::type_complexity)]
    pub(crate) fn word_marks(
        &self,
    ) -> Vec<(Option<(String, Vec<String>)>, Option<(String, Vec<String>)>)> {
        self.rows()
            .into_iter()
            .map(|row| {
                let words = |line: Option<DiffLine>| {
                    let line = line?;
                    let marks = line
                        .words
                        .iter()
                        .map(|range| line.text[range.clone()].to_owned())
                        .collect();
                    Some((line.text, marks))
                };
                (words(row.left), words(row.right))
            })
            .collect()
    }

    /// The rows of the side-by-side view: the whole file on both sides, with the lines
    /// of one hunk facing each other and blank lines where a side has none.
    fn rows(&self) -> Vec<DiffRow> {
        rows_of(
            &self.changes.base,
            &self.changes.current,
            &self.changes.hunks,
        )
    }

    fn hunk_rows(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let hunk = &self.changes.hunks[index];
        let rows = hunk_lines(&self.changes.base, &self.changes.current, hunk, theme);
        let start = (hunk.after.start as usize).min(self.changes.current.split('\n').count());
        let revert = self.revertable().then(|| {
            div()
                .id(("revert-hunk", index))
                .debug_selector(move || format!("diff-revert-{index}"))
                .flex()
                .items_center()
                .h(px(22.))
                .px(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .text_color(theme.text)
                .hover({
                    let pressed = theme.pressed;
                    move |style| style.bg(pressed)
                })
                .child("Revert")
                .on_click(cx.listener(move |this, _, _, cx| this.revert(index, cx)))
        });
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .when(self.selected == index, |this| {
                this.bg(theme.selection_inactive)
            })
            .when(self.selected != index, |this| this.bg(theme.hover))
            .text_color(theme.text_secondary)
            .child(format!("Line {}", start + 1))
            .children(revert);
        div()
            .flex()
            .flex_col()
            .mb(px(SPACE_M))
            .child(header)
            .child(rows)
    }

    /// The empty rows and the backgrounds of both panes, so that the rows of a hunk face
    /// each other and the words that differ stand out. A fold takes rows away, so the
    /// empty rows are counted on the rows the panes show now.
    fn decorations(&self, rows: &[DiffRow], theme: &Theme, cx: &App) -> (PaneDecor, PaneDecor) {
        let left_visible = self.left.read(cx).shown_lines();
        let right_visible = self.right.read(cx).shown_lines();
        decorations_with_visible(
            rows,
            &self.changes.base,
            &self.changes.current,
            self.selected,
            theme,
            &left_visible,
            &right_visible,
        )
    }

    /// The strip between the panes: a band for every hunk and its buttons, at the rows
    /// the two panes show.
    fn strip(&self, rows: &[DiffRow], theme: &Theme, cx: &mut Context<Self>) -> Div {
        let line_height = self.right.read(cx).line_height().max(1.);
        let top = self.right.read(cx).scroll_top();
        let mut strip = div()
            .relative()
            .flex_none()
            .w(px(STRIP_WIDTH))
            .h_full()
            .overflow_hidden();
        for hunk in 0..self.changes.hunks.len() {
            let Some(first) = rows.iter().position(|row| row.hunk == Some(hunk)) else {
                continue;
            };
            let last = rows
                .iter()
                .rposition(|row| row.hunk == Some(hunk))
                .unwrap_or(first);
            let y = first as f32 * line_height - top;
            let height = (last - first + 1) as f32 * line_height;
            let kind = hunk_kind(&self.changes.hunks[hunk]);
            strip = strip.child(
                div()
                    .absolute()
                    .left(px((STRIP_WIDTH - 4.) / 2.))
                    .top(px(y))
                    .w(px(4.))
                    .h(px(height))
                    .rounded(px(CONTROL_RADIUS))
                    .bg(kind.color(theme.dark))
                    .opacity(0.8),
            );
            let apply_left = div()
                .id(("diff-apply-left", hunk))
                .debug_selector(move || format!("diff-apply-left-{hunk}"))
                .flex()
                .items_center()
                .justify_center()
                .w(px(18.))
                .h(px(18.))
                .rounded(px(CONTROL_RADIUS))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text)
                .hover({
                    let pressed = theme.pressed;
                    move |style| style.bg(pressed)
                })
                .tooltip(|_, cx| tooltip("Apply the left side to the right", None, cx))
                .child(">>")
                .on_click(cx.listener(move |this, _, _, cx| this.revert(hunk, cx)));
            let apply_right = div()
                .id(("diff-apply-right", hunk))
                .debug_selector(move || format!("diff-apply-right-{hunk}"))
                .flex()
                .items_center()
                .justify_center()
                .w(px(18.))
                .h(px(18.))
                .rounded(px(CONTROL_RADIUS))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text)
                .hover({
                    let pressed = theme.pressed;
                    move |style| style.bg(pressed)
                })
                .tooltip(|_, cx| tooltip("Apply the right side to the left", None, cx))
                .child("<<")
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.selected = hunk.min(this.changes.hunks.len().saturating_sub(1));
                    cx.notify();
                }));
            let discard = div()
                .id(("revert-hunk", hunk))
                .debug_selector(move || format!("diff-revert-{hunk}"))
                .flex()
                .items_center()
                .justify_center()
                .w(px(18.))
                .h(px(18.))
                .rounded(px(CONTROL_RADIUS))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text)
                .hover({
                    let pressed = theme.pressed;
                    move |style| style.bg(pressed)
                })
                .tooltip(|_, cx| tooltip("Discard the change", None, cx))
                .child("\u{00d7}")
                .on_click(cx.listener(move |this, _, _, cx| this.revert(hunk, cx)));
            strip = strip.child(
                div()
                    .absolute()
                    .left(px(SPACE_XS))
                    .top(px(y))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .child(apply_left)
                    .child(apply_right)
                    .child(discard),
            );
        }
        strip
    }

    /// The title of a side: what the version on that side is.
    fn side_title(&self, left: bool) -> String {
        match (&self.source, left) {
            (Source::WorkingTree, true) => "HEAD".to_owned(),
            (Source::Branch(branch), true) => branch.clone(),
            (Source::Commit(commit), true) => {
                format!("{}^", &commit[..commit.len().min(7)])
            }
            (Source::Clipboard, true) => "Clipboard".to_owned(),
            (Source::File(other), true) => other.file_name().map_or_else(
                || other.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
            (Source::Shelf(_), true) => "HEAD".to_owned(),
            (_, false) => self.path.file_name().map_or_else(
                || self.path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            ),
        }
    }

    fn layout_button(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let label = if self.side_by_side {
            "Side-by-side"
        } else {
            "Unified"
        };
        div()
            .id("diff-layout")
            .debug_selector(|| "diff-layout".to_owned())
            .flex()
            .items_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .text_size(px(UI_SIZE))
            .text_color(theme.text_secondary)
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .tooltip(|_, cx| tooltip("Toggle between side-by-side and unified", None, cx))
            .child(label)
            .on_click(cx.listener(|this, _, window, cx| this.toggle_layout(window, cx)))
    }
}

fn line(text: &str, sign: &'static str, tint: Option<Hsla>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_none()
        .h(px(LINE_HEIGHT))
        .items_center()
        .when_some(tint, |this, tint| this.bg(tint.opacity(0.18)))
        .child(
            div()
                .w(px(22.))
                .flex_none()
                .text_color(theme.text_secondary)
                .child(sign),
        )
        .child(div().whitespace_nowrap().child(text.to_string()))
}

/// The lines of one hunk between `base` and `current`, with a little context around:
/// removed lines red, added green. Shared with the Local History window.
pub(crate) fn hunk_lines(base: &str, current: &str, hunk: &Hunk, theme: &Theme) -> Div {
    let before: Vec<&str> = base.split('\n').collect();
    let after: Vec<&str> = current.split('\n').collect();
    let clamp = |line: u32| (line as usize).min(after.len());
    let (start, end) = (clamp(hunk.after.start), clamp(hunk.after.end));
    let from = start.saturating_sub(CONTEXT_LINES);
    let to = (end + CONTEXT_LINES).min(after.len());
    let removed_from = (hunk.before.start as usize).min(before.len());
    let removed_to = (hunk.before.end as usize).min(before.len());
    let removed = Some(Kind::Conflicted.color(theme.dark));
    let added = Some(Kind::Added.color(theme.dark));

    let mut rows = div().flex().flex_col();
    for text in &after[from..start] {
        rows = rows.child(line(text, " ", None, theme));
    }
    for text in &before[removed_from..removed_to] {
        rows = rows.child(line(text, "-", removed, theme));
    }
    for text in &after[start..end] {
        rows = rows.child(line(text, "+", added, theme));
    }
    for text in &after[end..to] {
        rows = rows.child(line(text, " ", None, theme));
    }
    rows
}

impl Render for DiffView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(40.));
        let height = viewport.height * 0.7;
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.12;
        let repo = self.git.read(cx).repo();
        let root = repo.as_ref().map_or(Path::new(""), |repo| repo.root());
        let title = relative_of(root, &self.path);
        let title = match &self.source {
            Source::WorkingTree => {
                if self.files.len() > 1 {
                    format!(
                        "Changes in {title} ({} of {})",
                        self.index + 1,
                        self.files.len()
                    )
                } else {
                    format!("Changes in {title}")
                }
            }
            Source::Commit(commit) => {
                format!("Changes in {title} at {}", &commit[..commit.len().min(7)])
            }
            Source::Branch(branch) => format!("Difference between '{branch}' and {title}"),
            Source::Clipboard => format!("Difference between the clipboard and {title}"),
            Source::File(other) => {
                let other = other.file_name().map_or_else(
                    || other.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                format!("Difference between {other} and {title}")
            }
            Source::Shelf(shelf) => format!("HEAD and {title} in shelf {shelf}"),
        };
        let pane: AnyElement = if self.side_by_side {
            let side_rows = self.rows();
            let (left_decor, right_decor) = self.decorations(&side_rows, &theme, cx);
            let left_pane = self.left.update(cx, |editor, cx| {
                editor.pane(left_decor, Some(EDITOR_CONTEXT), window, cx)
            });
            let right_pane = self.right.update(cx, |editor, cx| {
                editor.pane(right_decor, Some(EDITOR_CONTEXT), window, cx)
            });
            let empty = side_rows.is_empty();
            let header = div()
                .flex()
                .flex_none()
                .items_center()
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .text_color(theme.text_secondary)
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(self.side_title(true)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(self.side_title(false)),
                );
            let panes = div()
                .id("diff-side")
                .debug_selector(|| "diff-side-by-side".to_owned())
                .flex()
                .flex_1()
                .min_h_0()
                .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                    if this.diff_key(event, window, cx) {
                        cx.stop_propagation();
                    }
                }))
                .child(
                    div()
                        .debug_selector(|| "diff-pane-left".to_owned())
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(left_pane),
                )
                .child(self.strip(&side_rows, &theme, cx))
                .child(
                    div()
                        .debug_selector(|| "diff-pane-right".to_owned())
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .child(right_pane),
                );
            let panes = if empty {
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .items_center()
                    .justify_center()
                    .text_color(theme.text_secondary)
                    .child("No changes")
                    .into_any_element()
            } else {
                panes.into_any_element()
            };
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(header)
                .child(panes)
                .into_any_element()
        } else {
            let rows: Vec<Div> = (0..self.changes.hunks.len())
                .map(|index| self.hunk_rows(index, &theme, cx))
                .collect();
            let empty = rows.is_empty();
            scrollable(
                "diff-list",
                &self.scroll,
                div()
                    .id("diff-list")
                    .debug_selector(|| "diff-unified".to_owned())
                    .flex()
                    .flex_col()
                    .px(px(SPACE_M))
                    .font_family(MONO_FONT)
                    .text_size(px(CODE_SIZE))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows)
                    .when(empty, |this| {
                        this.items_center()
                            .justify_center()
                            .text_color(theme.text_secondary)
                            .child("No changes")
                    }),
                &theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        let body = surface(&theme)
            .id("diff")
            .track_focus(&self.focus)
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &NextDiff, _, cx| this.next_diff(cx)))
            .on_action(cx.listener(|this, _: &PreviousDiff, _, cx| this.previous_diff(cx)))
            .on_action(cx.listener(|this, _: &NextChange, _, cx| this.next_change(cx)))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| this.previous_change(cx)))
            .on_action(cx.listener(|this, _: &ApplyLeftSide, _, cx| this.apply_left(cx)))
            .on_action(cx.listener(|this, _: &ApplyRightSide, _, cx| this.apply_right(cx)))
            .on_action(cx.listener(|this, _: &FocusOppositePane, window, cx| {
                this.focus_opposite(window, cx)
            }))
            .occlude()
            .w(width)
            .h(height)
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .debug_selector(|| "diff-title".to_owned())
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .h(px(40.))
                    .px(px(DIALOG_PADDING))
                    .font_weight(FontWeight::MEDIUM)
                    .child(title)
                    .child(self.layout_button(&theme, cx)),
            )
            .child(pane);
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("diff-enter", body))
    }
}

#[cfg(test)]
mod tests {
    use super::Changes;

    fn apply(mut text: String, edit: vcs::TextEdit) -> String {
        text.replace_range(edit.range, &edit.text);
        text
    }

    #[test]
    fn a_hunk_is_taken_back_only_over_the_text_the_diff_was_taken_from() {
        let base = "a\nb\nc\n";
        let current = "a\nB\nc\n";
        let changes = Changes {
            current: current.into(),
            base: base.into(),
            hunks: vcs::line_hunks(base, current),
        };

        let edit = changes.revert(0, current).unwrap();
        assert_eq!(apply(current.to_string(), edit), base);
    }

    #[test]
    fn a_changed_text_gives_no_edit_and_the_caller_rereads_the_diff() {
        let base = "a\nb\nc\n";
        let current = "a\nB\nc\n";
        let changes = Changes {
            current: current.into(),
            base: base.into(),
            hunks: vcs::line_hunks(base, current),
        };

        assert_eq!(changes.revert(0, "a\nB\nC\n"), None);
        assert_eq!(changes.revert(7, current), None);
    }
}

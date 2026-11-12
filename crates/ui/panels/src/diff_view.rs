//! The changes of one file against the last commit: two panes, the version before on
//! the left and the working text on the right, with the words that differ marked and a
//! strip between the panes that takes one hunk back. Unified is the other layout: one
//! list of lines with the same hunk actions.

use std::ops::Range;
use std::path::{Path, PathBuf};

use editor_view::{EditorStyle, EditorView, HighlightKind, HighlightSpan};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{Hunk, TextEdit};
use workspace::{Settings, Workspace};

use crate::git::{GitState, Kind, relative_of};

const WIDTH: f32 = 1180.;
const CONTEXT_LINES: usize = 2;
const LINE_HEIGHT: f32 = 20.;
const LINE_NUMBER_WIDTH: f32 = 44.;
const STRIP_WIDTH: f32 = 68.;
/// The advance of the code font at the default size, for the caret of the right pane.
const CHAR_WIDTH: f32 = 8.4;
const CONTEXT: &str = "DiffView";

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
        /// The diff keys, while the popup has the focus.
        pub(crate) fn bind_keys(cx: &mut App) {
            cx.bind_keys([$(KeyBinding::new($keys, $action, Some(CONTEXT))),*]);
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

pub struct DiffView {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
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
    /// The open editor of the file, whose buffer the right pane edits.
    editor: Option<Entity<EditorView>>,
    /// Side by side when the setting says so, one list otherwise.
    side_by_side: bool,
    /// The pane the keys work in.
    pane: Pane,
    /// The syntax colours of the two versions, once the syntax thread has read them.
    left_spans: Vec<HighlightSpan>,
    right_spans: Vec<HighlightSpan>,
    /// Tells a late syntax answer which version of the text it belongs to.
    spans_serial: u64,
    /// Tells a late diff answer which edit it belongs to.
    serial: u64,
    /// The window x of the text of the right pane, for a click at a column.
    right_text_x: f32,
    scroll: ScrollHandle,
    side_scroll: ScrollHandle,
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

/// One line of one pane: its own number, the text, the syntax colours and the words
/// that differ from the line on the other side.
struct DiffLine {
    number: usize,
    text: String,
    syntax: Vec<(Range<usize>, HighlightKind)>,
    words: Vec<Range<usize>>,
}

/// One row of the side-by-side view. A line missing on a side means that side has no
/// text there, so both sides stay on the same height and the hunks line up.
struct DiffRow {
    left: Option<DiffLine>,
    right: Option<DiffLine>,
    /// The hunk the row belongs to; `None` is unchanged context.
    hunk: Option<usize>,
    kind: Option<Kind>,
}

/// Registers the popup as the text input of the window while the right pane has the
/// keys, so that typing reaches the buffer of the open editor.
struct DiffInput {
    view: Entity<DiffView>,
    focus: FocusHandle,
}

impl IntoElement for DiffInput {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for DiffInput {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = relative(1.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        window.handle_input(
            &self.focus,
            ElementInputHandler::new(bounds, self.view.clone()),
            cx,
        );
    }
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

/// The lines of a text, without the empty line a trailing newline would add.
fn lines_of(text: &str) -> Vec<String> {
    if text.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&str> = text.split('\n').collect();
    if text.ends_with('\n') {
        lines.pop();
    }
    lines.into_iter().map(str::to_owned).collect()
}

/// The byte offset of the start of every line.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect()
}

/// The syntax spans of one line, moved to offsets inside the line.
fn line_syntax(
    spans: &[HighlightSpan],
    start: usize,
    end: usize,
) -> Vec<(Range<usize>, HighlightKind)> {
    spans
        .iter()
        .filter(|span| (span.start as usize) < end && (span.end as usize) > start)
        .map(|span| {
            let from = (span.start as usize).max(start) - start;
            let to = (span.end as usize).min(end) - start;
            (from..to, span.kind)
        })
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

/// A line of code with its syntax colours and the background of its changed words.
fn styled_line(
    text: &str,
    syntax: &[(Range<usize>, HighlightKind)],
    words: &[Range<usize>],
    word_color: Option<Hsla>,
    style: &EditorStyle,
) -> StyledText {
    let mut points = vec![0, text.len()];
    for (range, _) in syntax {
        points.push(range.start);
        points.push(range.end);
    }
    for range in words {
        points.push(range.start);
        points.push(range.end);
    }
    points.retain(|point| *point <= text.len());
    points.sort_unstable();
    points.dedup();

    let mut highlights = Vec::new();
    for pair in points.windows(2) {
        let (from, to) = (pair[0], pair[1]);
        if from == to {
            continue;
        }
        let kind = syntax
            .iter()
            .find(|(range, _)| range.start <= from && from < range.end)
            .map(|(_, kind)| *kind);
        let color = kind.map_or(style.code, |kind| style.color_of(kind));
        let background = words
            .iter()
            .any(|range| range.start <= from && from < range.end)
            .then_some(word_color)
            .flatten();
        if color != style.code || background.is_some() {
            highlights.push((
                from..to,
                HighlightStyle {
                    color: Some(color),
                    background_color: background,
                    ..HighlightStyle::default()
                },
            ));
        }
    }
    StyledText::new(text.to_owned()).with_highlights(highlights)
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
        let editor = editor_of(&workspace, &path, cx);
        shared.update(cx, |workspace, cx| {
            let side_by_side = cx
                .try_global::<Settings>()
                .is_none_or(|settings| settings.diff_side_by_side);
            let pane = if editor.is_some() && source == Source::WorkingTree {
                Pane::Right
            } else {
                Pane::Left
            };
            let view = cx.new(|cx| Self {
                git,
                workspace: weak,
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
                editor,
                side_by_side,
                pane,
                left_spans: Vec::new(),
                right_spans: Vec::new(),
                spans_serial: 0,
                serial: 0,
                right_text_x: 0.,
                scroll: ScrollHandle::new(),
                side_scroll: ScrollHandle::new(),
            });
            let focus = view.read(cx).focus.clone();
            view.update(cx, |view, cx| view.start_highlighting(cx));
            workspace.show_overlay(view.into(), focus, window, cx);
        });
    }

    fn revertable(&self) -> bool {
        self.source == Source::WorkingTree
    }

    /// The buffer the right pane edits, when it is the pane with the keys.
    fn editing_editor(&self) -> Option<Entity<EditorView>> {
        (self.side_by_side && self.pane == Pane::Right && self.revertable())
            .then(|| self.editor.clone())
            .flatten()
    }

    /// F7 and Shift+F7: the next or previous hunk; the last one goes to the next file.
    fn next_diff(&mut self, cx: &mut Context<Self>) {
        if self.selected + 1 < self.changes.hunks.len() {
            self.selected += 1;
            self.scroll_to_selected();
            cx.notify();
        } else {
            self.move_file(1, false, cx);
        }
    }

    fn previous_diff(&mut self, cx: &mut Context<Self>) {
        if self.selected > 0 {
            self.selected -= 1;
            self.scroll_to_selected();
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

    /// Scrolls the pane of the selected hunk so that the hunk comes into sight.
    fn scroll_to_selected(&self) {
        if self.side_by_side {
            if let Some(row) = self
                .rows()
                .iter()
                .position(|row| row.hunk == Some(self.selected))
            {
                self.side_scroll.scroll_to_item(row);
            }
        } else {
            self.scroll.scroll_to_item(self.selected);
        }
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
        let editor = editor_of(&self.workspace, &path, cx);
        let current = match source {
            Source::WorkingTree | Source::Branch(_) => current_text(&path, editor.as_ref(), cx),
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
                this.editor = editor_of(&this.workspace, &path, cx);
                this.path = path;
                this.changes = Changes {
                    current,
                    base: diff.base,
                    hunks: diff.hunks,
                };
                this.index = index;
                this.selected = selected;
                this.start_highlighting(cx);
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
        let editor = editor_of(&self.workspace, &self.path, cx);
        let text_now = current_text(&self.path, editor.as_ref(), cx);
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
        match editor {
            Some(editor) => {
                let bytes = editor.read(cx).file_bytes();
                let cause = local_history::Cause::BeforeGitRevert;
                crate::history::snapshot_bytes(&path, bytes, cause, cx);
                editor.update(cx, |editor, cx| {
                    editor.replace_text(range.clone(), &text, cx)
                });
                self.workspace
                    .update(cx, |workspace, cx| workspace.save_editor(&path, cx))
                    .ok();
            }
            None => {
                // A small file, read and written here anyway: the snapshot goes first.
                if let Some(history) = crate::history::get(cx) {
                    let _ = history.record_file(&path, local_history::Cause::BeforeGitRevert);
                }
                let mut next = text_now;
                next.replace_range(range, &text);
                if let Err(error) = std::fs::write(&path, next) {
                    let message = format!("{}: {error}", path.display());
                    self.git
                        .update(cx, |git, cx| git.report("Revert", message, cx));
                    return;
                }
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
        window.focus(&self.focus, cx);
        if self.pane == Pane::Right {
            self.focus_right_editor(cx);
        }
        cx.notify();
    }

    /// Puts the caret of the shared buffer on the first line of the selected hunk, so
    /// that typing in the right pane changes the text the diff shows.
    fn focus_right_editor(&mut self, cx: &mut Context<Self>) {
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let line = self
            .changes
            .hunks
            .get(self.selected)
            .map_or(0, |hunk| hunk.after.start as usize);
        editor.update(cx, |editor, cx| {
            let column = editor.caret().1;
            editor.restore_caret(line, column, cx);
        });
    }

    /// Feeds typing and movement to the buffer behind the right pane.
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.side_by_side || self.pane != Pane::Right || !self.revertable() {
            return;
        }
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if modifiers.alt || modifiers.platform {
            return;
        }
        let typed = !modifiers.control
            && event
                .keystroke
                .key_char
                .as_deref()
                .is_some_and(|text| !text.is_empty() && !text.chars().all(char::is_control));
        let action = matches!(
            key,
            "backspace"
                | "delete"
                | "enter"
                | "left"
                | "right"
                | "up"
                | "down"
                | "home"
                | "end"
                | "pageup"
                | "pagedown"
                | "tab"
        );
        if !typed && !action {
            return;
        }
        let Some(editor) = self.editor.clone() else {
            return;
        };
        let before = editor.read(cx).text().unwrap_or_default();
        editor.update(cx, |editor, cx| editor.handle_key(event, window, cx));
        let after = editor.read(cx).text().unwrap_or_default();
        if action {
            // The editor changed the text itself; the platform must not type the key again.
            cx.stop_propagation();
        }
        if after != before {
            self.reread(cx);
        }
    }

    /// A click puts the keys in one pane; on the right it places the caret of the buffer.
    fn click_line(
        &mut self,
        side: Pane,
        line: Option<usize>,
        x: f32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !self.side_by_side {
            return;
        }
        self.pane = side;
        window.focus(&self.focus, cx);
        if side == Pane::Right
            && let Some(editor) = self.editor.clone()
            && self.revertable()
            && let Some(line) = line
        {
            let column = ((x - self.right_text_x) / CHAR_WIDTH).max(0.) as usize;
            editor.update(cx, |editor, cx| editor.restore_caret(line, column, cx));
        }
        cx.notify();
    }

    /// Switch Side-by-side / Unified and keep it in the settings file.
    fn toggle_layout(&mut self, cx: &mut Context<Self>) {
        self.side_by_side = !self.side_by_side;
        let side_by_side = self.side_by_side;
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.update_settings(|settings| settings.diff_side_by_side = side_by_side, cx)
            })
            .ok();
        cx.notify();
    }

    /// Asks the syntax thread for the colours of both versions.
    fn start_highlighting(&mut self, cx: &mut Context<Self>) {
        self.spans_serial += 1;
        let serial = self.spans_serial;
        self.left_spans.clear();
        self.right_spans.clear();
        let path = self.path.clone();
        if let Some(task) = editor_view::highlight_text(&path, self.changes.base.clone(), cx) {
            cx.spawn(async move |this, cx| {
                let spans = task.await;
                this.update(cx, |this, cx| {
                    if this.spans_serial != serial {
                        return;
                    }
                    this.left_spans = spans;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        if let Some(task) = editor_view::highlight_text(&path, self.changes.current.clone(), cx) {
            cx.spawn(async move |this, cx| {
                let spans = task.await;
                this.update(cx, |this, cx| {
                    if this.spans_serial != serial {
                        return;
                    }
                    this.right_spans = spans;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
    }

    /// Takes the diff again after a change.
    fn reread(&mut self, cx: &mut Context<Self>) {
        if !self.revertable() {
            return;
        }
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let editor = editor_of(&self.workspace, &self.path, cx);
        let current = current_text(&self.path, editor.as_ref(), cx);
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
                this.editor = editor_of(&this.workspace, &this.path, cx);
                this.changes = Changes {
                    current,
                    base: diff.base,
                    hunks: diff.hunks,
                };
                this.selected = this
                    .selected
                    .min(this.changes.hunks.len().saturating_sub(1));
                this.start_highlighting(cx);
                this.git.update(cx, |git, cx| git.refresh(cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
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
        let before = lines_of(&self.changes.base);
        let after = lines_of(&self.changes.current);
        let before_starts = line_starts(&self.changes.base);
        let after_starts = line_starts(&self.changes.current);
        let line = |number: usize, text: &str, start: usize, spans: &[HighlightSpan]| DiffLine {
            number,
            text: text.to_owned(),
            syntax: line_syntax(spans, start, start + text.len()),
            words: Vec::new(),
        };
        let mut rows = Vec::new();
        let (mut b, mut a) = (0, 0);

        let push_unchanged = |rows: &mut Vec<DiffRow>, b: usize, a: usize| {
            rows.push(DiffRow {
                left: before.get(b).map(|text| {
                    line(
                        b + 1,
                        text,
                        before_starts.get(b).copied().unwrap_or_default(),
                        &self.left_spans,
                    )
                }),
                right: after.get(a).map(|text| {
                    line(
                        a + 1,
                        text,
                        after_starts.get(a).copied().unwrap_or_default(),
                        &self.right_spans,
                    )
                }),
                hunk: None,
                kind: None,
            });
        };

        for (index, hunk) in self.changes.hunks.iter().enumerate() {
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
                let mut left = before.get(left_index).map(|text| {
                    line(
                        left_index + 1,
                        text,
                        before_starts.get(left_index).copied().unwrap_or_default(),
                        &self.left_spans,
                    )
                });
                let mut right = after.get(right_index).map(|text| {
                    line(
                        right_index + 1,
                        text,
                        after_starts.get(right_index).copied().unwrap_or_default(),
                        &self.right_spans,
                    )
                });
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

    /// One row of the two panes with its strip cell.
    fn side_row(
        &self,
        row: &DiffRow,
        first_of_hunk: bool,
        caret: Option<(usize, usize)>,
        style: &EditorStyle,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let selected = row.hunk == Some(self.selected);
        let tint = |right: bool| {
            let tint = line_tint(row.kind, right, theme);
            if selected {
                tint.map(|tint| tint.opacity(0.28))
                    .or(Some(theme.selection_inactive))
            } else {
                tint.map(|tint| tint.opacity(0.18))
            }
        };
        let cell = |line: Option<&DiffLine>, right: bool| {
            let mut cell = div()
                .flex()
                .items_center()
                .flex_1()
                .min_w_0()
                .relative()
                .h(px(LINE_HEIGHT))
                .overflow_hidden()
                .when_some(tint(right), |this, tint| this.bg(tint));
            let Some(line) = line else {
                return cell.child(div().w(px(LINE_NUMBER_WIDTH)).flex_none());
            };
            let side = if right { Pane::Right } else { Pane::Left };
            let number = line.number;
            let line_index = number - 1;
            cell = cell
                .child(
                    div()
                        .w(px(LINE_NUMBER_WIDTH))
                        .flex_none()
                        .pr(px(SPACE_S))
                        .text_color(theme.line_number)
                        .child(format!("{number}")),
                )
                .child(div().whitespace_nowrap().child(styled_line(
                    &line.text,
                    &line.syntax,
                    &line.words,
                    word_tint(row.kind, right, theme),
                    style,
                )));
            if right
                && self.side_by_side
                && self.pane == Pane::Right
                && caret.is_some_and(|(line, _)| line == line_index)
            {
                let column = caret.map_or(0, |(_, column)| column) as f32;
                cell = cell.child(
                    div()
                        .absolute()
                        .left(px(LINE_NUMBER_WIDTH + column * CHAR_WIDTH))
                        .top(px(SPACE_XS))
                        .w(px(1.))
                        .h(px(LINE_HEIGHT - SPACE_S))
                        .bg(theme.caret),
                );
            }
            cell.on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, window, cx| {
                    this.click_line(
                        side,
                        Some(line_index),
                        f32::from(event.position.x),
                        window,
                        cx,
                    )
                }),
            )
        };

        let mut strip = div()
            .relative()
            .flex_none()
            .w(px(STRIP_WIDTH))
            .h(px(LINE_HEIGHT))
            .flex()
            .items_center()
            .justify_center();
        if let Some(kind) = row.kind {
            strip = strip.child(
                div()
                    .absolute()
                    .w(px(4.))
                    .h_full()
                    .rounded(px(CONTROL_RADIUS))
                    .bg(kind.color(theme.dark))
                    .opacity(0.8),
            );
        }
        if first_of_hunk && let Some(hunk) = row.hunk {
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
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .child(apply_left)
                    .child(apply_right)
                    .child(discard),
            );
        }

        div()
            .flex()
            .flex_none()
            .h(px(LINE_HEIGHT))
            .child(cell(row.left.as_ref(), false))
            .child(strip)
            .child(cell(row.right.as_ref(), true))
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
            .on_click(cx.listener(|this, _, _, cx| this.toggle_layout(cx)))
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
        self.right_text_x = f32::from(left)
            + f32::from((width - px(STRIP_WIDTH)) / 2.)
            + STRIP_WIDTH
            + LINE_NUMBER_WIDTH;
        let caret = self.editor.as_ref().map(|editor| editor.read(cx).caret());
        let style = EditorStyle::from_theme(&theme);
        let pane: AnyElement = if self.side_by_side {
            let side_rows = self.rows();
            let mut previous = None;
            let mut rows = Vec::with_capacity(side_rows.len());
            for row in &side_rows {
                let first = row.hunk.is_some() && previous != row.hunk;
                previous = row.hunk;
                rows.push(self.side_row(row, first, caret, &style, &theme, cx));
            }
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
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(header)
                .child(
                    scrollable(
                        "diff-side-list",
                        &self.side_scroll,
                        div()
                            .id("diff-side-list")
                            .debug_selector(|| "diff-side-by-side".to_owned())
                            .flex()
                            .flex_col()
                            .font_family(MONO_FONT)
                            .text_size(px(CODE_SIZE))
                            .overflow_y_scroll()
                            .track_scroll(&self.side_scroll)
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
                    .min_h_0(),
                )
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
        let input = (self.side_by_side && self.pane == Pane::Right).then(|| DiffInput {
            view: cx.entity(),
            focus: self.focus.clone(),
        });
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
            .on_key_down(cx.listener(|this, event, window, cx| this.key_down(event, window, cx)))
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
            .child(pane)
            .when_some(input, |this, input| {
                this.child(div().absolute().size_full().child(input))
            });
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("diff-enter", body))
    }
}

impl EntityInputHandler for DiffView {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        let editor = self.editing_editor()?;
        editor.update(cx, |editor, cx| {
            editor.text_for_range(range, adjusted, window, cx)
        })
    }

    fn selected_text_range(
        &mut self,
        ignore_disabled_input: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let editor = self.editing_editor()?;
        editor.update(cx, |editor, cx| {
            editor.selected_text_range(ignore_disabled_input, window, cx)
        })
    }

    fn marked_text_range(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        let editor = self.editing_editor()?;
        editor.update(cx, |editor, cx| editor.marked_text_range(window, cx))
    }

    fn unmark_text(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(editor) = self.editing_editor() else {
            return;
        };
        editor.update(cx, |editor, cx| editor.unmark_text(window, cx));
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editing_editor() else {
            return;
        };
        let before = editor.read(cx).text().unwrap_or_default();
        editor.update(cx, |editor, cx| {
            editor.replace_text_in_range(range, text, window, cx)
        });
        let after = editor.read(cx).text().unwrap_or_default();
        if after != before {
            self.reread(cx);
        }
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        new_text: &str,
        new_selected_range: Option<Range<usize>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.editing_editor() else {
            return;
        };
        editor.update(cx, |editor, cx| {
            editor.replace_and_mark_text_in_range(range, new_text, new_selected_range, window, cx)
        });
        self.reread(cx);
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        element_bounds: Bounds<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let editor = self.editing_editor()?;
        editor.update(cx, |editor, cx| {
            editor.bounds_for_range(range, element_bounds, window, cx)
        })
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let editor = self.editing_editor()?;
        editor.update(cx, |editor, cx| {
            editor.character_index_for_point(point, window, cx)
        })
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

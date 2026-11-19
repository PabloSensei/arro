//! The changes of one file against the last commit: two editors, the version before on
//! the left and the working text on the right, with the words that differ marked and a
//! strip between the panes that takes one hunk back. Unified is the other layout: one
//! list of lines with the same hunk actions.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use editor_view::{
    EditorEvent, EditorOptions, EditorStyle, EditorView, GutterKind, GutterMark, PaneDecor,
};
use text_core::FileFormat;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{Hunk, TextEdit};
use workspace::{DiffHighlighting, DiffWhitespace, Settings, Workspace};

use crate::commit::CommitDraft;
use crate::editor_tab::{options_of, style_of};
use crate::git::{GitState, Kind, relative_of};

const WIDTH: f32 = 1180.;
const LINE_HEIGHT: f32 = 20.;
const STRIP_WIDTH: f32 = 96.;
const CONTEXT: &str = "DiffView";
/// The widths of the menu of the gear and of the list of files.
const MENU_WIDTH: f32 = 300.;
const MENU_ROW_HEIGHT: f32 = 24.;
/// The unchanged lines the diff keeps around a change when the setting is not there.
const CONTEXT_LINES: usize = 3;
/// The values of Context lines the gear offers.
const CONTEXT_CHOICES: [u32; 5] = [1, 2, 3, 5, 10];
/// Numbers the tabs of diffs, so that a new diff opens in a tab of its own.
static NEXT_DIFF_TAB: AtomicU64 = AtomicU64::new(0);

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
        FocusOppositePane,
        JumpToSource,
        NextFile,
        PreviousFile,
        ToggleCollapse
    ]
);

macro_rules! diff_keys {
    ($(($keys:literal, $action:ident, $idea:expr, $mac:expr)),* $(,)?) => {
        /// The diff keys, while the popup has the focus, and while one of its editors
        /// has it.
        pub(crate) fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        /// Binds the keys of one platform, so that a test reads the macOS table on every
        /// runner.
        fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = workspace::KeyRow {
                    keys: $keys,
                    mac: $mac,
                    action: concat!("diff_view::", stringify!($action)),
                    context: Some(CONTEXT),
                    idea: $idea,
                };
                for key in row.keys_for(platform) {
                    cx.bind_keys([
                        KeyBinding::new(key, $action, Some(CONTEXT)),
                        KeyBinding::new(key, $action, Some(EDITOR_CONTEXT)),
                    ]);
                }
            )*
        }

        #[cfg(test)]
        pub(crate) const KEYS: &[workspace::KeyRow] = &[
            $(workspace::KeyRow {
                keys: $keys,
                mac: $mac,
                action: concat!("diff_view::", stringify!($action)),
                context: Some(CONTEXT),
                idea: $idea,
            }),*
        ];
    };
}

diff_keys![
    ("f7", NextDiff, Some("NextDiff"), None),
    ("shift-f7", PreviousDiff, Some("PreviousDiff"), None),
    ("f4", JumpToSource, Some("EditSource"), None),
    ("alt-right", NextFile, Some("NextTab"), None),
    ("alt-left", PreviousFile, Some("PreviousTab"), None),
    ("alt-shift-right", NextChange, Some("Diff.NextChange"), None),
    (
        "alt-shift-left",
        PreviousChange,
        Some("Diff.PrevChange"),
        None
    ),
    (
        "ctrl-alt-r",
        ApplyLeftSide,
        Some("Diff.ApplyLeftSide"),
        None
    ),
    (
        "ctrl-alt-a",
        ApplyRightSide,
        Some("Diff.ApplyRightSide"),
        None
    ),
    // Tab stays an indent in the editable pane, as in IDEA.
    (
        "ctrl-shift-tab",
        FocusOppositePane,
        Some("Diff.FocusOppositePane"),
        None
    ),
];

/// The pane the keys edit or read.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Pane {
    Left,
    Right,
}

/// Which editor an event came from: one of the panes, or the tab the right pane shares
/// its document with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum EditorSide {
    Left,
    Right,
    Tab,
}

/// The list the gear or the counter of files opened, drawn inside the diff so that it
/// does not replace the diff popup with a popup of the window.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Menu {
    Settings,
    Files,
}

/// One row of a menu of the diff: a section name that takes no click, or a row.
enum MenuLine {
    Section(&'static str),
    Row(MenuRow),
}

struct MenuRow {
    label: String,
    /// The name the tests find the row by.
    id: String,
    checked: bool,
    action: MenuAction,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum MenuAction {
    Whitespace(DiffWhitespace),
    Highlighting(DiffHighlighting),
    ToggleSynchronize,
    ToggleLineNumbers,
    ToggleWrap,
    Context(u32),
    OpenInTab,
    SelectFile(usize),
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
    /// The open tab of the file. The right pane shows its document when it is there.
    tab: Option<Entity<EditorView>>,
    /// The right pane is a view of the document of the tab, so a tab opened again for the
    /// same file takes the place of the one that was closed.
    attached: bool,
    /// The text the right pane started from when it became a document of its own, while
    /// no tab of the file is open. Edits of the pane go into a tab opened later only when
    /// the file still holds this text.
    detached_base: Option<String>,
    /// The tab whose save was still on its way when it closed: the pane becomes a
    /// document of its own when the write lands, over the text the file got.
    pending_detach: Option<Entity<EditorView>>,
    /// Why the pane last stayed a document of its own instead of taking the tab, with
    /// the text of the tab then, so the same reason is told once.
    refused: Option<(String, String)>,
    /// The version before, read only.
    left: Entity<EditorView>,
    /// The working text: the document of the tab, or an editor of its own.
    right: Entity<EditorView>,
    /// Side by side when the setting says so, one list otherwise.
    side_by_side: bool,
    /// What the comparison leaves out of two lines.
    whitespace: DiffWhitespace,
    /// What the diff marks inside the lines.
    highlighting: DiffHighlighting,
    /// A scroll of one pane moves the other one.
    synchronize: bool,
    /// The numbers of the lines show in the gutter of the panes.
    show_line_numbers: bool,
    /// Long lines wrap at the edge of the panes.
    soft_wrap: bool,
    /// The unchanged lines the diff keeps in sight around a change.
    context_lines: usize,
    /// The unchanged lines between changes are folded away.
    collapse: bool,
    /// The unchanged gaps a click opened, by the row they start at.
    expanded: HashSet<usize>,
    /// The open menu of the gear or of the counter of files.
    menu: Option<Menu>,
    /// The diff fills an editor tab rather than a popup over the window.
    tabbed: bool,
    /// The pane the keys work in.
    pane: Pane,
    /// Tells a late diff answer which edit it belongs to.
    serial: u64,
    /// A scroll of one pane is being copied to the other, so it must not copy back.
    syncing: bool,
    scroll: ScrollHandle,
    /// The events of the panes and of the tab, replaced when a pane is.
    subscriptions: Vec<Subscription>,
    /// The notify of the window, for a tab that opens or closes under the diff.
    _workspace: Option<Subscription>,
    /// The draft of a commit this diff belongs to: every hunk of the working tree has a
    /// tick that decides whether it goes into the commit.
    commit: Option<Entity<CommitDraft>>,
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
    /// Two texts with no file behind them, such as the bodies of two responses.
    Text,
}

/// What the file is now and what the last commit has, with `\n` line ends, and the hunks
/// between them.
#[derive(Clone)]
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
#[derive(Clone)]
pub(crate) struct DiffLine {
    number: usize,
    text: String,
    words: Vec<Range<usize>>,
}

/// One row of the side-by-side view. A line missing on a side means that side has no
/// text there, so both sides stay on the same height and the hunks line up.
#[derive(Clone)]
pub(crate) struct DiffRow {
    left: Option<DiffLine>,
    right: Option<DiffLine>,
    /// The hunk the row belongs to; `None` is unchanged context.
    pub(crate) hunk: Option<usize>,
    kind: Option<Kind>,
}

/// The text with `\n` line ends, as the buffers keep it.
fn normalized(text: &str) -> String {
    text_core::normalize_newlines(text).into_owned()
}

/// The text of the file as the user sees it, with `\n` line ends, and the format a save
/// writes: the open editor, or the disk. The disk is read with the loader of a tab, so a
/// file in another encoding or with line ends of any kind keeps them.
fn current_document(
    path: &Path,
    editor: Option<&Entity<EditorView>>,
    cx: &App,
) -> (String, FileFormat) {
    if let Some(editor) = editor {
        let editor = editor.read(cx);
        if let Some(text) = editor.text() {
            return (normalized(&text), editor.format().unwrap_or_default());
        }
    }
    match text_core::read_file(path) {
        Ok(decoded) => (decoded.text, decoded.format),
        Err(_) => (String::new(), FileFormat::default()),
    }
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

/// An editor of a text that is not the document of a tab: the version before, or the
/// working text when no tab of the file is open. It reads the bookmarks of the file and
/// never replaces them in the window list.
pub(crate) fn text_view(
    path: PathBuf,
    text: String,
    format: FileFormat,
    read_only: bool,
    style: EditorStyle,
    options: EditorOptions,
    cx: &mut App,
) -> Entity<EditorView> {
    cx.new(move |cx| {
        let mut view =
            EditorView::open_text_document(path, text, format, read_only, false, style, cx);
        view.set_options(options, cx);
        view.lock_soft_wrap();
        view
    })
}

/// The lines of a text as the editor shows them: the empty line after a trailing newline
/// is a line of its own.
fn lines_of(text: &str) -> Vec<String> {
    text.split('\n').map(str::to_owned).collect()
}

/// The byte offset of the start of every line.
pub(crate) fn line_starts(text: &str) -> Vec<usize> {
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

/// One line as the comparison sees it under the whitespace setting.
fn compared_line(mode: DiffWhitespace, line: &str) -> String {
    match mode {
        DiffWhitespace::DoNotIgnore => line.to_owned(),
        DiffWhitespace::Trim => line.trim().to_owned(),
        DiffWhitespace::Ignore | DiffWhitespace::IgnoreAndEmptyLines => line
            .chars()
            .filter(|character| !character.is_whitespace())
            .collect(),
    }
}

/// The text as the comparison sees it. Every line keeps its place, so a hunk counted on
/// this text points at the same lines of the text the panes show.
fn compared_text(mode: DiffWhitespace, text: &str) -> String {
    text.split('\n')
        .map(|line| compared_line(mode, line))
        .collect::<Vec<_>>()
        .join("\n")
}

fn blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// The name the tests find a whitespace choice by.
fn mode_name(mode: DiffWhitespace) -> &'static str {
    match mode {
        DiffWhitespace::DoNotIgnore => "do-not-ignore",
        DiffWhitespace::Trim => "trim",
        DiffWhitespace::Ignore => "ignore",
        DiffWhitespace::IgnoreAndEmptyLines => "ignore-empty",
    }
}

/// The name the tests find a highlighting choice by.
fn mode_name_highlight(highlighting: DiffHighlighting) -> &'static str {
    match highlighting {
        DiffHighlighting::ByLine => "by-line",
        DiffHighlighting::ByWord => "by-word",
        DiffHighlighting::SplitChanges => "split-changes",
        DiffHighlighting::ByCharacter => "by-character",
        DiffHighlighting::None => "none",
    }
}

/// The hunks of two texts under the whitespace setting: lines that differ only in their
/// whitespace do not count, and with the last choice neither does a line of nothing but
/// whitespace.
pub(crate) fn hunks_with(mode: DiffWhitespace, base: &str, current: &str) -> Vec<Hunk> {
    if mode == DiffWhitespace::DoNotIgnore {
        return vcs::line_hunks(base, current);
    }
    let base_seen = compared_text(mode, base);
    let current_seen = compared_text(mode, current);
    let mut hunks = vcs::line_hunks(&base_seen, &current_seen);
    if mode == DiffWhitespace::IgnoreAndEmptyLines {
        let before: Vec<&str> = base.split('\n').collect();
        let after: Vec<&str> = current.split('\n').collect();
        hunks.retain(|hunk| {
            let before_blank = before[hunk.before.start as usize..hunk.before.end as usize]
                .iter()
                .all(|line| blank(line));
            let after_blank = after[hunk.after.start as usize..hunk.after.end as usize]
                .iter()
                .all(|line| blank(line));
            !(before_blank && after_blank)
        });
    }
    hunks
}

/// The characters that differ between two lines, for the character marks of a changed
/// line: the common start and the common end stay plain.
pub(crate) fn character_changes(before: &str, after: &str) -> vcs::WordChanges {
    let left: Vec<(usize, char)> = before.char_indices().collect();
    let right: Vec<(usize, char)> = after.char_indices().collect();
    let mut shared_start = 0;
    while shared_start < left.len()
        && shared_start < right.len()
        && left[shared_start].1 == right[shared_start].1
    {
        shared_start += 1;
    }
    let mut shared_end = 0;
    while shared_end < left.len() - shared_start
        && shared_end < right.len() - shared_start
        && left[left.len() - 1 - shared_end].1 == right[right.len() - 1 - shared_end].1
    {
        shared_end += 1;
    }
    let byte = |chars: &[(usize, char)], at: usize| {
        chars.get(at).map_or_else(
            || {
                chars
                    .last()
                    .map_or(0, |(index, character)| index + character.len_utf8())
            },
            |(index, _)| *index,
        )
    };
    let before_start = byte(&left, shared_start);
    let before_end = byte(&left, left.len() - shared_end);
    let after_start = byte(&right, shared_start);
    let after_end = byte(&right, right.len() - shared_end);
    vcs::WordChanges {
        before: (before_start < before_end)
            .then_some(before_start..before_end)
            .into_iter()
            .collect(),
        after: (after_start < after_end)
            .then_some(after_start..after_end)
            .into_iter()
            .collect(),
    }
}

fn line_tint(kind: Option<Kind>, right: bool, theme: &Theme) -> Option<Hsla> {
    match kind? {
        Kind::Added if !right => None,
        Kind::Deleted if right => None,
        kind => Some(kind.color(theme)),
    }
}

fn word_tint(kind: Option<Kind>, right: bool, theme: &Theme) -> Option<Hsla> {
    (kind == Some(Kind::Modified)).then(|| {
        if right {
            Kind::Added.color(theme)
        } else {
            Kind::Conflicted.color(theme)
        }
    })
}

/// The rows of the side-by-side view: the whole file on both sides, with the lines of
/// one hunk facing each other and blank lines where a side has none.
pub(crate) fn rows_of(base: &str, current: &str, hunks: &[Hunk]) -> Vec<DiffRow> {
    rows_of_with(base, current, hunks, DiffHighlighting::ByWord)
}

/// The same rows with the marks the highlighting setting asks for.
pub(crate) fn rows_of_with(
    base: &str,
    current: &str,
    hunks: &[Hunk],
    highlighting: DiffHighlighting,
) -> Vec<DiffRow> {
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
                let changes = match highlighting {
                    DiffHighlighting::ByWord | DiffHighlighting::SplitChanges => {
                        vcs::word_changes(&left.text, &right.text)
                    }
                    DiffHighlighting::ByCharacter => character_changes(&left.text, &right.text),
                    DiffHighlighting::ByLine | DiffHighlighting::None => {
                        vcs::WordChanges::default()
                    }
                };
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

/// One line of the unified list, or the unchanged lines a gap hides.
pub(crate) enum UnifiedItem {
    Row(DiffRow),
    Gap { at: usize, count: usize },
}

/// Folds the unchanged lines of `rows` away on one side, behind a placeholder that a
/// click opens again.
fn collapse_side(
    editor: &Entity<EditorView>,
    rows: &[DiffRow],
    left: bool,
    starts: &[usize],
    count: usize,
    cx: &mut App,
) {
    let numbers: Vec<usize> = rows
        .iter()
        .filter_map(|row| {
            let line = if left { &row.left } else { &row.right };
            line.as_ref().map(|line| line.number - 1)
        })
        .collect();
    let (Some(first), Some(last)) = (numbers.first(), numbers.last()) else {
        return;
    };
    let text_len = editor.read(cx).text().map_or(0, |text| text.len());
    let from = starts.get(*first).copied().unwrap_or_default();
    let to = starts.get(last + 1).copied().unwrap_or(text_len);
    if from >= to {
        return;
    }
    let placeholder = format!("\u{22ef} {count} unchanged lines");
    editor.update(cx, |editor, cx| {
        editor.collapse_range(from..to, placeholder, cx)
    });
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
        Self::open_working_tree(git, workspace, None, files, index, window, cx);
    }

    /// The diff of a file of the Commit window: every hunk has a tick, and a hunk without
    /// one stays out of the commit and in the working tree.
    pub fn open_commit_list(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        commit: Entity<CommitDraft>,
        files: Vec<PathBuf>,
        index: usize,
        window: &mut Window,
        cx: &mut App,
    ) {
        Self::open_working_tree(git, workspace, Some(commit), files, index, window, cx);
    }

    fn open_working_tree(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        commit: Option<Entity<CommitDraft>>,
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
            None,
            commit,
            window,
            cx,
        );
    }

    /// The diff of a file at `line`: the hunk of that line is the one the keys act on,
    /// as Show Diff of the window of a change opens it.
    pub fn open_at(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        line: u32,
        window: &mut Window,
        cx: &mut App,
    ) {
        let files = vec![path.clone()];
        Self::open_source(
            git,
            workspace,
            path,
            Source::WorkingTree,
            files,
            0,
            Some(line),
            None,
            window,
            cx,
        );
    }

    /// The diff of a file as one commit changed it, for the Git Log window. The other
    /// files of the commit are the set the diff keys walk.
    pub fn open_commit(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        files: Vec<PathBuf>,
        index: usize,
        commit: String,
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
            Source::Commit(commit),
            files,
            index,
            None,
            None,
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
            None,
            None,
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
                        None,
                        diff.current,
                        FileFormat::default(),
                        diff.base,
                        diff.hunks,
                        None,
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
                let current = text_core::read_file(path)
                    .map(|decoded| decoded.text)
                    .unwrap_or_default();
                (clipboard, current)
            },
        );
    }

    /// The selected text of an editor, or its whole text, against the clipboard. Both
    /// sides are read only, and the clipboard is the left one.
    pub fn compare_text_clipboard(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        text: String,
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
            move |_| (clipboard, text),
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
                    text_core::read_file(path)
                        .map(|decoded| decoded.text)
                        .unwrap_or_default()
                };
                (read(&other), read(path))
            },
        );
    }

    /// Two texts of the window, with no file behind them: the bodies of two responses.
    pub fn compare_texts(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        left: String,
        right: String,
        window: &mut Window,
        cx: &mut App,
    ) {
        Self::open_text(git, workspace, path, Source::Text, window, cx, move |_| {
            (left, right)
        });
    }

    #[allow(clippy::too_many_arguments)]
    fn open_source(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        source: Source,
        files: Vec<PathBuf>,
        index: usize,
        target: Option<u32>,
        commit: Option<Entity<CommitDraft>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(top) = git.read(cx).repo() else {
            return;
        };
        let relative = relative_of(top.root(), &path);
        let repo = git.read(cx).repo_of(&relative).unwrap_or(top);
        let (current, format) = match source {
            Source::WorkingTree | Source::Branch(_) => {
                current_document(&path, editor_of(&workspace, &path, cx).as_ref(), cx)
            }
            Source::Commit(_) | Source::Shelf(_) => (String::new(), FileFormat::default()),
            Source::Clipboard | Source::File(_) | Source::Text => {
                (String::new(), FileFormat::default())
            }
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
                Source::Clipboard | Source::File(_) | Source::Shelf(_) | Source::Text => None,
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
                        git, workspace, path, source, files, index, target, current, format,
                        diff.base, diff.hunks, commit, window, cx,
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
                        None,
                        current,
                        FileFormat::default(),
                        base,
                        hunks,
                        None,
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
        target: Option<u32>,
        current: String,
        format: FileFormat,
        base: String,
        _hunks: Vec<Hunk>,
        commit: Option<Entity<CommitDraft>>,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(shared) = workspace.upgrade() else {
            return;
        };
        let settings = cx.try_global::<Settings>();
        let whitespace = settings.map_or(DiffWhitespace::DoNotIgnore, |s| s.diff_whitespace);
        let highlighting = settings.map_or(DiffHighlighting::ByWord, |s| s.diff_highlighting);
        let synchronize = settings.is_none_or(|s| s.diff_synchronize_scrolling);
        let show_line_numbers = settings.is_none_or(|s| s.diff_show_line_numbers);
        let soft_wrap = settings.is_some_and(|s| s.diff_soft_wrap);
        let context_lines = settings.map_or(CONTEXT_LINES, |s| s.diff_context_lines as usize);
        let collapse = settings.is_some_and(|s| s.diff_collapse_unchanged);
        let hunks = hunks_with(whitespace, &base, &current);
        let selected = target
            .and_then(|line| crate::git::hunk_index_at(&hunks, line))
            .unwrap_or(0);
        let weak = workspace.clone();
        let tab = editor_of(&workspace, &path, cx);
        let side_by_side = settings.is_none_or(|s| s.diff_side_by_side);
        let pane = if source == Source::WorkingTree {
            Pane::Right
        } else {
            Pane::Left
        };
        let style = style_of(cx);
        let mut options = options_of(cx);
        options.line_numbers = show_line_numbers;
        options.soft_wrap = soft_wrap;
        let left = {
            let path = path.clone();
            let base = base.clone();
            let style = style.clone();
            let options = options.clone();
            cx.new(move |cx| {
                let mut view = EditorView::open_text_document(
                    path,
                    base,
                    FileFormat::default(),
                    true,
                    false,
                    style,
                    cx,
                );
                view.set_options(options, cx);
                if !soft_wrap {
                    view.lock_soft_wrap();
                }
                view
            })
        };
        let shared_right = match (&tab, source == Source::WorkingTree) {
            (Some(tab), true) => EditorView::open_shared(tab, cx),
            _ => None,
        };
        // A right pane that is a view of the document of the tab keeps following the file
        // when that tab is closed and opened again. A tab still reading the file becomes
        // the document of the pane when it is ready.
        let attached = shared_right.is_some();
        let detached_base = (!attached).then(|| normalized(&current));
        let right = shared_right.unwrap_or_else(|| {
            text_view(
                path.clone(),
                current.clone(),
                format.clone(),
                source != Source::WorkingTree,
                style.clone(),
                options.clone(),
                cx,
            )
        });
        if soft_wrap {
            right.update(cx, |right, _| right.unlock_soft_wrap());
        }
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
                selected,
                tab,
                attached,
                detached_base,
                pending_detach: None,
                refused: None,
                left,
                right,
                side_by_side,
                whitespace,
                highlighting,
                synchronize,
                show_line_numbers,
                soft_wrap,
                context_lines,
                collapse,
                expanded: HashSet::new(),
                menu: None,
                tabbed: false,
                pane,
                serial: 0,
                syncing: false,
                scroll: ScrollHandle::new(),
                subscriptions: Vec::new(),
                _workspace: None,
                commit,
            });
            view.update(cx, |view, cx| {
                view.attach(cx);
                if view.collapse {
                    view.apply_collapse(cx);
                }
                let theme = *cx.global::<Theme>();
                view.publish_marks(&theme, cx);
            });
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

    /// The line the two versions have when the whitespace setting hides every hunk.
    fn raw_differs(&self) -> bool {
        !vcs::line_hunks(&self.changes.base, &self.changes.current).is_empty()
    }

    /// What the toolbar says about the changes: `1 difference`, `No differences`,
    /// `Contents are identical` or `Only whitespace differences`.
    fn difference_text(&self) -> String {
        match self.changes.hunks.len() {
            1 => "1 difference".to_owned(),
            count if count > 1 => format!("{count} differences"),
            _ if self.raw_differs() => "Only whitespace differences".to_owned(),
            _ if self.changes.base != self.changes.current => "No differences".to_owned(),
            _ => "Contents are identical".to_owned(),
        }
    }

    /// The title of the window or of the tab: `Repository Diff: <file>` for the versions
    /// of a file, and the names of the two texts for the other comparisons.
    fn window_title(&self, root: &Path) -> String {
        let title = relative_of(root, &self.path);
        match &self.source {
            Source::WorkingTree | Source::Commit(_) | Source::Shelf(_) => {
                format!("Repository Diff: {title}")
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
            Source::Text => "Difference between two responses".to_owned(),
        }
    }

    /// The counter of files of the toolbar, `2/4 files`.
    fn files_text(&self) -> String {
        format!("{}/{} files", self.index + 1, self.files.len().max(1))
    }

    /// `true` for the left side, which is always the version before.
    fn side_locked(&self, left: bool) -> bool {
        left || self.source != Source::WorkingTree
    }

    /// The revision of each side: a short hash, a branch, `HEAD`, `Working copy` or the
    /// name of the other text.
    pub(crate) fn revisions(&self) -> (String, String) {
        let short = |hash: &str| hash[..hash.len().min(7)].to_owned();
        let name = |path: &Path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        };
        match &self.source {
            Source::WorkingTree => ("HEAD".to_owned(), "Working copy".to_owned()),
            Source::Branch(branch) => (branch.clone(), "Working copy".to_owned()),
            Source::Commit(commit) => (format!("{}^", short(commit)), short(commit)),
            Source::Clipboard => ("Clipboard".to_owned(), "Working copy".to_owned()),
            Source::File(other) => (name(other), name(&self.path)),
            Source::Shelf(shelf) => ("HEAD".to_owned(), format!("Shelf {shelf}")),
            Source::Text => ("First response".to_owned(), "Second response".to_owned()),
        }
    }

    /// The path each side shows under its revision.
    fn side_paths(&self) -> (String, String) {
        let name = |path: &Path| {
            path.file_name().map_or_else(
                || path.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            )
        };
        let left = match &self.source {
            Source::File(other) => name(other),
            Source::Text => String::new(),
            _ => name(&self.path),
        };
        (left, name(&self.path))
    }

    /// Keeps a change of the gear in the settings file.
    fn store(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut Settings)) {
        self.workspace
            .update(cx, |workspace, cx| workspace.update_settings(change, cx))
            .ok();
    }

    /// The options of the panes with the choices of the diff over them.
    fn apply_options(&self, cx: &mut Context<Self>) {
        let mut options = options_of(cx);
        options.line_numbers = self.show_line_numbers;
        options.soft_wrap = self.soft_wrap;
        let soft_wrap = self.soft_wrap;
        for editor in [&self.left, &self.right] {
            editor.update(cx, |editor, cx| {
                if soft_wrap {
                    editor.unlock_soft_wrap();
                } else {
                    editor.lock_soft_wrap();
                }
                editor.set_options(options.clone(), cx);
            });
        }
    }

    fn set_whitespace(&mut self, mode: DiffWhitespace, cx: &mut Context<Self>) {
        if self.whitespace == mode {
            return;
        }
        self.whitespace = mode;
        self.changes.hunks = hunks_with(mode, &self.changes.base, &self.changes.current);
        self.selected = self
            .selected
            .min(self.changes.hunks.len().saturating_sub(1));
        self.expanded.clear();
        self.store(cx, |settings| settings.diff_whitespace = mode);
        self.apply_collapse(cx);
        let theme = *cx.global::<Theme>();
        self.publish_marks(&theme, cx);
        cx.notify();
    }

    fn set_highlighting(&mut self, highlighting: DiffHighlighting, cx: &mut Context<Self>) {
        self.highlighting = highlighting;
        self.store(cx, |settings| settings.diff_highlighting = highlighting);
        cx.notify();
    }

    fn set_context(&mut self, lines: u32, cx: &mut Context<Self>) {
        self.context_lines = lines as usize;
        self.expanded.clear();
        self.store(cx, |settings| settings.diff_context_lines = lines);
        self.apply_collapse(cx);
        cx.notify();
    }

    fn toggle_collapse(&mut self, cx: &mut Context<Self>) {
        self.collapse = !self.collapse;
        let collapse = self.collapse;
        self.expanded.clear();
        self.store(cx, |settings| settings.diff_collapse_unchanged = collapse);
        self.apply_collapse(cx);
        cx.notify();
    }

    fn toggle_synchronize(&mut self, cx: &mut Context<Self>) {
        self.synchronize = !self.synchronize;
        let synchronize = self.synchronize;
        self.store(cx, |settings| {
            settings.diff_synchronize_scrolling = synchronize;
        });
        cx.notify();
    }

    fn toggle_line_numbers(&mut self, cx: &mut Context<Self>) {
        self.show_line_numbers = !self.show_line_numbers;
        let show = self.show_line_numbers;
        self.store(cx, |settings| settings.diff_show_line_numbers = show);
        self.apply_options(cx);
        cx.notify();
    }

    fn toggle_soft_wrap(&mut self, cx: &mut Context<Self>) {
        self.soft_wrap = !self.soft_wrap;
        let soft_wrap = self.soft_wrap;
        self.store(cx, |settings| settings.diff_soft_wrap = soft_wrap);
        self.apply_options(cx);
        cx.notify();
    }

    /// Hides the unchanged lines the diff keeps out of sight, and shows them again when
    /// the setting is off. A gap the user opened stays open.
    fn apply_collapse(&mut self, cx: &mut Context<Self>) {
        self.left.update(cx, |left, cx| left.clear_folds(cx));
        self.right.update(cx, |right, cx| right.clear_folds(cx));
        if !self.collapse {
            return;
        }
        let rows = self.rows();
        let base_starts = line_starts(&self.changes.base);
        let current_starts = line_starts(&self.changes.current);
        let keep = self.context_lines;
        let mut index = 0;
        while index < rows.len() {
            if rows[index].hunk.is_some() {
                index += 1;
                continue;
            }
            let start = index;
            while index < rows.len() && rows[index].hunk.is_none() {
                index += 1;
            }
            let end = index;
            if self.expanded.contains(&start) || end - start <= keep * 2 + 1 {
                continue;
            }
            let hidden = &rows[start + keep..end - keep];
            let count = hidden.len();
            if count == 0 {
                continue;
            }
            collapse_side(&self.left, hidden, true, &base_starts, count, cx);
            collapse_side(&self.right, hidden, false, &current_starts, count, cx);
        }
    }

    /// The marks of the changes on the bars of both panes.
    /// Takes the marks off the bars, before the text they describe moves under them.
    fn clear_marks(&self, cx: &mut Context<Self>) {
        for editor in [&self.left, &self.right] {
            editor.update(cx, |editor, cx| editor.set_gutter_marks(Vec::new(), cx));
        }
    }

    /// The marks of the changes on the bars of both panes.
    fn publish_marks(&mut self, theme: &Theme, cx: &mut Context<Self>) {
        // The marks name lines of one text. A pane that already shows another text takes
        // no marks: a line past its end would point the bar at a line that is not there.
        let live = self.right.read(cx).text().map(|text| normalized(&text));
        if live.as_deref() != Some(self.changes.current.as_str()) {
            self.clear_marks(cx);
            return;
        }
        if self.changes.hunks.is_empty() {
            return;
        }
        let lines = |text: &str| text.split('\n').count().max(1) as u32;
        let base_lines = lines(&self.changes.base);
        // The pane may show text newer than the diff its hunks describe: a mark past the
        // last line would point the bar at a line that is not there.
        let current_lines = self
            .right
            .read(cx)
            .text()
            .map_or_else(|| lines(&self.changes.current), |text| lines(&text));
        let clamp = |range: Range<u32>, lines: u32| {
            let start = range.start.min(lines - 1);
            let end = range.end.min(lines).max(start);
            start..end
        };
        let mark = |lines: Range<u32>, kind: GutterKind| GutterMark {
            lines,
            kind,
            color: match kind {
                GutterKind::Added => Kind::Added.color(theme),
                GutterKind::Deleted => Kind::Deleted.color(theme),
                GutterKind::Modified => Kind::Modified.color(theme),
            },
        };
        let left: Vec<GutterMark> = self
            .changes
            .hunks
            .iter()
            .map(|hunk| {
                let kind = if hunk.before.is_empty() {
                    GutterKind::Added
                } else if hunk.after.is_empty() {
                    GutterKind::Deleted
                } else {
                    GutterKind::Modified
                };
                mark(clamp(hunk.before.clone(), base_lines), kind)
            })
            .collect();
        let right: Vec<GutterMark> = self
            .changes
            .hunks
            .iter()
            .map(|hunk| {
                let kind = if hunk.after.is_empty() {
                    GutterKind::Deleted
                } else if hunk.before.is_empty() {
                    GutterKind::Added
                } else {
                    GutterKind::Modified
                };
                mark(clamp(hunk.after.clone(), current_lines), kind)
            })
            .collect();
        self.left
            .update(cx, |view, cx| view.set_gutter_marks(left, cx));
        self.right
            .update(cx, |view, cx| view.set_gutter_marks(right, cx));
    }

    /// `true` when the menu of the gear or of the files is open.
    fn toggle_menu(&mut self, menu: Menu, cx: &mut Context<Self>) {
        self.menu = (self.menu != Some(menu)).then_some(menu);
        cx.notify();
    }

    fn close_menu(&mut self, cx: &mut Context<Self>) {
        if self.menu.take().is_some() {
            cx.notify();
        }
    }

    fn run_menu(&mut self, action: MenuAction, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        match action {
            MenuAction::Whitespace(mode) => self.set_whitespace(mode, cx),
            MenuAction::Highlighting(highlighting) => self.set_highlighting(highlighting, cx),
            MenuAction::ToggleSynchronize => self.toggle_synchronize(cx),
            MenuAction::ToggleLineNumbers => self.toggle_line_numbers(cx),
            MenuAction::ToggleWrap => self.toggle_soft_wrap(cx),
            MenuAction::Context(lines) => self.set_context(lines, cx),
            MenuAction::OpenInTab => self.open_in_tab(window, cx),
            MenuAction::SelectFile(index) => self.switch_to(index, false, cx),
        }
        cx.notify();
    }

    /// The rows of the open menu. The files come with the counter of the file set, and
    /// the gear carries the choices of the diff.
    fn menu_lines(&self) -> Vec<MenuLine> {
        match self.menu {
            Some(Menu::Files) => self
                .files
                .iter()
                .enumerate()
                .map(|(index, path)| {
                    let name = path.file_name().map_or_else(
                        || path.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                    MenuLine::Row(MenuRow {
                        label: format!("{}. {name}", index + 1),
                        id: format!("diff-files-row-{index}"),
                        checked: index == self.index,
                        action: MenuAction::SelectFile(index),
                    })
                })
                .collect(),
            Some(Menu::Settings) => {
                let mut lines = vec![MenuLine::Section("Ignore whitespaces")];
                for mode in [
                    DiffWhitespace::DoNotIgnore,
                    DiffWhitespace::Trim,
                    DiffWhitespace::Ignore,
                    DiffWhitespace::IgnoreAndEmptyLines,
                ] {
                    lines.push(MenuLine::Row(MenuRow {
                        label: mode.label().to_owned(),
                        id: format!("diff-settings-whitespace-{}", mode_name(mode)),
                        checked: mode == self.whitespace,
                        action: MenuAction::Whitespace(mode),
                    }));
                }
                lines.push(MenuLine::Section("Highlighting"));
                for highlighting in [
                    DiffHighlighting::ByLine,
                    DiffHighlighting::ByWord,
                    DiffHighlighting::SplitChanges,
                    DiffHighlighting::ByCharacter,
                    DiffHighlighting::None,
                ] {
                    lines.push(MenuLine::Row(MenuRow {
                        label: highlighting.label().to_owned(),
                        id: format!(
                            "diff-settings-highlighting-{}",
                            mode_name_highlight(highlighting)
                        ),
                        checked: highlighting == self.highlighting,
                        action: MenuAction::Highlighting(highlighting),
                    }));
                }
                lines.push(MenuLine::Row(MenuRow {
                    label: "Synchronize scrolling".to_owned(),
                    id: "diff-settings-sync".to_owned(),
                    checked: self.synchronize,
                    action: MenuAction::ToggleSynchronize,
                }));
                lines.push(MenuLine::Row(MenuRow {
                    label: "Show line numbers".to_owned(),
                    id: "diff-settings-line-numbers".to_owned(),
                    checked: self.show_line_numbers,
                    action: MenuAction::ToggleLineNumbers,
                }));
                lines.push(MenuLine::Row(MenuRow {
                    label: "Use soft wraps".to_owned(),
                    id: "diff-settings-soft-wrap".to_owned(),
                    checked: self.soft_wrap,
                    action: MenuAction::ToggleWrap,
                }));
                lines.push(MenuLine::Section("Context lines"));
                for lines_shown in CONTEXT_CHOICES {
                    lines.push(MenuLine::Row(MenuRow {
                        label: lines_shown.to_string(),
                        id: format!("diff-settings-context-{lines_shown}"),
                        checked: lines_shown as usize == self.context_lines,
                        action: MenuAction::Context(lines_shown),
                    }));
                }
                lines.push(MenuLine::Row(MenuRow {
                    label: "Open in Editor Tab".to_owned(),
                    id: "diff-settings-open-in-tab".to_owned(),
                    checked: false,
                    action: MenuAction::OpenInTab,
                }));
                lines
            }
            None => Vec::new(),
        }
    }

    /// F4: the file of the pane with the keys opens at the line of its caret.
    fn jump_to_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let editor = match self.pane {
            Pane::Left => &self.left,
            Pane::Right => &self.right,
        };
        let (line, column) = editor.read(cx).caret();
        let path = self.path.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                if !self.tabbed {
                    workspace.dismiss(window, cx);
                }
                workspace.open_file(path.clone(), window, cx);
            })
            .ok();
        if let Some(editor) = editor_of(&self.workspace, &self.path, cx) {
            editor.update(cx, |editor, cx| editor.restore_caret(line, column, cx));
        }
    }

    /// Moves the same diff into an editor tab, as Open in Editor Tab of IDEA does.
    fn open_in_tab(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.tabbed {
            return;
        }
        self.tabbed = true;
        self.menu = None;
        let name = self.path.file_name().map_or_else(
            || self.path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let root = self
            .git
            .read(cx)
            .repo()
            .map_or_else(PathBuf::new, |repo| repo.root().to_path_buf());
        let serial = NEXT_DIFF_TAB.fetch_add(1, Ordering::Relaxed);
        let tab_path = root.join(".diff").join(format!("{serial}-{name}.diff"));
        let title = format!("Diff: {name}");
        let view = cx.entity();
        let focus = self.focus.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                workspace.open_view_tab(
                    tab_path,
                    title,
                    "general/diff",
                    view.into(),
                    focus,
                    window,
                    cx,
                );
            })
            .ok();
    }

    /// Subscribes the panes and the tab, and the window, whose notifications bring a tab
    /// that opens or closes under the diff.
    fn attach(&mut self, cx: &mut Context<Self>) {
        self.subscribe_editors(cx);
        if let Some(workspace) = self.workspace.upgrade() {
            self._workspace =
                Some(cx.observe(&workspace, |this: &mut Self, _, cx| this.adopt_tab(cx)));
        }
    }

    /// Follows the text of the right pane and the scroll of both panes. The tab shares
    /// its document with the right pane, so an edit in either one comes here.
    fn subscribe_editors(&mut self, cx: &mut Context<Self>) {
        let mut subscriptions = Vec::with_capacity(4);
        subscriptions.push(cx.subscribe(
            &self.left,
            |this: &mut Self, source: Entity<EditorView>, event: &EditorEvent, cx| {
                if this.left == source {
                    this.editor_event(EditorSide::Left, event, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe(
            &self.right,
            |this: &mut Self, source: Entity<EditorView>, event: &EditorEvent, cx| {
                if this.right == source {
                    this.editor_event(EditorSide::Right, event, cx);
                }
            },
        ));
        if let Some(tab) = self.tab.clone()
            && tab != self.right
        {
            subscriptions.push(cx.subscribe(
                &tab,
                |this: &mut Self, source: Entity<EditorView>, event: &EditorEvent, cx| {
                    if this.tab.as_ref() == Some(&source) {
                        this.editor_event(EditorSide::Tab, event, cx);
                    }
                },
            ));
        }
        // The tab closed while its save was on its way: its result tells the text the
        // pane takes over.
        if let Some(closing) = self.pending_detach.clone()
            && closing != self.right
            && self.tab.as_ref() != Some(&closing)
        {
            subscriptions.push(cx.subscribe(
                &closing,
                |this: &mut Self, source: Entity<EditorView>, event: &EditorEvent, cx| {
                    if this.pending_detach.as_ref() == Some(&source) {
                        this.closing_event(source, event, cx);
                    }
                },
            ));
        }
        self.subscriptions = subscriptions;
    }

    /// A tab of the file appeared, closed or opened again: the right pane follows it.
    /// While no tab is open the pane is a document of its own; a tab opened later takes
    /// its document over, and the edits the pane made meanwhile go into the tab when the
    /// file still holds the text they were made over. A tab closed again leaves the pane
    /// the text the file has now: a save kept it, "Don't save" dropped it.
    fn adopt_tab(&mut self, cx: &mut Context<Self>) {
        // A closed tab whose save has not landed yet keeps the pane bound to its document.
        if let Some(closed) = self.pending_detach.clone() {
            if closed.read(cx).is_saving() {
                return;
            }
            // The result never came: the document the pane already shares holds the text.
            self.leave_detached(cx);
        }
        let tab = editor_of(&self.workspace, &self.path, cx);
        let wants = self.source == Source::WorkingTree;
        let previous = self.tab.clone();
        let changed = tab != previous;
        if changed {
            self.tab = tab.clone();
            // Follows the new tab from its first event, also while it is still reading the
            // file: its `Loaded` asks here again.
            self.subscribe_editors(cx);
        }
        if !wants {
            return;
        }
        let Some(tab) = tab else {
            if previous.is_some() && self.attached {
                self.detach(previous.as_ref(), cx);
            }
            return;
        };
        if self.attached {
            // The pane already shows the document of the tab. A tab closed and opened
            // again before a notification came is another view of the same document.
            if changed {
                self.bind(tab, cx);
            }
            return;
        }
        // The pane is a document of its own. Its edits go into the tab only when the file
        // still holds the text they were counted on; otherwise the file changed on disk
        // and merging the two silently would lose one of them.
        let own = normalized(&self.right.read(cx).text().unwrap_or_default());
        let Some(theirs) = tab.read(cx).text() else {
            // The tab is still reading the file; its `Loaded` comes here again.
            return;
        };
        let theirs = normalized(&theirs);
        if self.right.read(cx).is_dirty() {
            if self.detached_base.as_deref() != Some(theirs.as_str()) {
                self.refuse(
                    "changed on disk: the pane keeps the text of its own edits",
                    &theirs,
                    cx,
                );
                return;
            }
            if let Some(edit) = text_core::difference(&theirs, &own) {
                self.syncing = true;
                tab.update(cx, |tab, cx| tab.apply_edits(vec![edit], cx));
                self.syncing = false;
            }
        }
        // A tab that could not take the edits of the pane (a read-only file, for one)
        // leaves the pane its own document, so nothing is lost.
        let taken = normalized(&tab.read(cx).text().unwrap_or_default());
        if self.right.read(cx).is_dirty() && taken != own {
            self.refuse(
                "cannot take the edits of the pane: it is read-only",
                &theirs,
                cx,
            );
            return;
        }
        self.bind(tab, cx);
    }

    /// Says why the pane stays a document of its own. The same reason over the same text
    /// of the tab is told once: a later notification does not repeat it.
    fn refuse(&mut self, reason: &str, theirs: &str, cx: &mut Context<Self>) {
        let refusal = (reason.to_owned(), theirs.to_owned());
        if self.refused.as_ref() != Some(&refusal) {
            self.refused = Some(refusal);
            let path = self.path.display().to_string();
            self.git.update(cx, |git, cx| {
                git.tell("Diff", format!("{path} {reason}"), cx)
            });
        }
        cx.notify();
    }

    /// Makes the right pane a view of the document of `tab`.
    fn bind(&mut self, tab: Entity<EditorView>, cx: &mut Context<Self>) {
        let Some(right) = EditorView::open_shared(&tab, cx) else {
            // The tab is still reading the file; its `Loaded` comes here again.
            return;
        };
        let top = self.right.read(cx).scroll_top();
        right.update(cx, |right, cx| right.scroll_to_top(top, cx));
        self.right = right;
        self.attached = true;
        self.detached_base = None;
        self.refused = None;
        self.subscribe_editors(cx);
        // The file may have changed on disk while the tab was closed.
        if normalized(&self.right.read(cx).text().unwrap_or_default()) != self.changes.current {
            self.reread(cx);
        }
        cx.notify();
    }

    /// The tab of the file is gone: the right pane becomes a document of its own over
    /// the text the file has, so the unsaved edits of a closed tab do not stay in it. A
    /// close writes the text first, so while that write is on its way this waits for its
    /// result: a save that lands leaves its snapshot, a failed one leaves the edits. A
    /// tab dropped without a save leaves the text of the disk.
    fn detach(&mut self, closed: Option<&Entity<EditorView>>, cx: &mut Context<Self>) {
        let Some(closed) = closed.cloned() else {
            // No tab is left to read the text from: the pane keeps the document it has.
            self.leave_detached(cx);
            return;
        };
        if closed.read(cx).is_saving() {
            if self.pending_detach.as_ref() != Some(&closed) {
                self.pending_detach = Some(closed);
                self.subscribe_editors(cx);
            }
            cx.notify();
            return;
        }
        if closed.read(cx).is_dirty() {
            // A tab dropped without a save: the file keeps the text of the disk.
            match text_core::read_file(&self.path) {
                Ok(decoded) => self.replace_detached(decoded.text, decoded.format, cx),
                Err(_) => self.leave_detached(cx),
            }
            return;
        }
        // The buffer of a clean tab is what the file has. The snapshot of its last save
        // is not: a reload from the disk after that save leaves the tab clean over newer
        // text, and the snapshot would write the older one back.
        let (text, format) = {
            let tab = closed.read(cx);
            (
                tab.text().unwrap_or_default(),
                tab.format().unwrap_or_default(),
            )
        };
        self.replace_detached(text, format, cx);
    }

    /// The write of the closed tab ended: the pane takes the snapshot that reached the
    /// disk, or keeps the text of the edits when the write failed.
    fn closing_event(
        &mut self,
        closed: Entity<EditorView>,
        event: &EditorEvent,
        cx: &mut Context<Self>,
    ) {
        if closed.read(cx).is_saving() {
            return;
        }
        match event {
            EditorEvent::Saved => {
                let tab = closed.read(cx);
                let current = tab.text().unwrap_or_default();
                match tab.saved_document() {
                    // Text typed into the pane while the write was on its way is not in
                    // the snapshot: the pane keeps the document with it, unsaved.
                    Some((saved, _)) if saved != current => self.leave_detached(cx),
                    Some((saved, format)) => self.replace_detached(saved, format, cx),
                    None => {
                        let format = tab.format().unwrap_or_default();
                        self.replace_detached(current, format, cx);
                    }
                }
            }
            EditorEvent::Failed(_) => {
                // The write failed: the pane keeps the text of the edits.
                self.leave_detached(cx);
            }
            _ => return,
        }
        if self.tab.is_some() {
            self.adopt_tab(cx);
        }
    }

    /// Makes the right pane a document of its own over `text`, written with `format`.
    fn replace_detached(&mut self, text: String, format: FileFormat, cx: &mut Context<Self>) {
        let top = self.right.read(cx).scroll_top();
        let style = style_of(cx);
        let options = options_of(cx);
        let right = text_view(
            self.path.clone(),
            text.clone(),
            format,
            self.source != Source::WorkingTree,
            style,
            options,
            cx,
        );
        right.update(cx, |right, cx| right.scroll_to_top(top, cx));
        self.right = right;
        self.attached = false;
        self.detached_base = Some(normalized(&text));
        self.pending_detach = None;
        self.refused = None;
        self.subscribe_editors(cx);
        if normalized(&text) != self.changes.current {
            self.reread(cx);
        }
        cx.notify();
    }

    /// The pane keeps the document it has and becomes a document of its own: a save
    /// failed, or no tab is left to take the text from.
    fn leave_detached(&mut self, cx: &mut Context<Self>) {
        let text = normalized(&self.right.read(cx).text().unwrap_or_default());
        self.attached = false;
        self.detached_base = Some(text.clone());
        self.pending_detach = None;
        self.refused = None;
        self.subscribe_editors(cx);
        if text != self.changes.current {
            self.reread(cx);
        }
        cx.notify();
    }

    fn editor_event(&mut self, from: EditorSide, event: &EditorEvent, cx: &mut Context<Self>) {
        match event {
            EditorEvent::Changed => self.text_changed(from, cx),
            // The text of a pane is there: the unchanged lines can fold.
            EditorEvent::Loaded if from != EditorSide::Tab => {
                if self.collapse {
                    self.apply_collapse(cx);
                }
            }
            // The tab opened again is ready to be a view of the right pane.
            EditorEvent::Loaded if from == EditorSide::Tab => self.adopt_tab(cx),
            // A save of the pane of its own: the reason to stay apart may be gone.
            EditorEvent::Saved if from == EditorSide::Right => self.adopt_tab(cx),
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

    /// The text of one view of the document changed: the other view moves its carets and
    /// folds with the change, and the hunks are read again.
    fn text_changed(&mut self, from: EditorSide, cx: &mut Context<Self>) {
        if self.syncing || from == EditorSide::Left {
            return;
        }
        // The lines the marks name move with the edit: they go until the diff is read
        // again over the new text.
        self.clear_marks(cx);
        if from == EditorSide::Right {
            let path = self.path.clone();
            self.workspace
                .update(cx, |workspace, cx| workspace.remember_edited(&path, cx))
                .ok();
        }
        if let Some(target) = self.paired(from, cx) {
            self.syncing = true;
            target.update(cx, |target, cx| target.follow_changes(cx));
            self.syncing = false;
        }
        self.reread(cx);
    }

    /// The view of the document of `from`, when the tab of the file is open: an edit in
    /// one of them moves the carets of the other. Two views of different documents are
    /// not paired, because following would move nothing.
    fn paired(&self, from: EditorSide, cx: &App) -> Option<Entity<EditorView>> {
        if self.source != Source::WorkingTree {
            return None;
        }
        let tab = self.tab.clone().filter(|tab| tab != &self.right)?;
        let (source, target) = match from {
            EditorSide::Tab => (&tab, &self.right),
            EditorSide::Right => (&self.right, &tab),
            EditorSide::Left => return None,
        };
        source
            .read(cx)
            .shares_document(target.read(cx))
            .then(|| target.clone())
    }

    /// One pane scrolled: the other one follows, so the hunks stay face to face.
    fn scrolled(&mut self, from: Pane, top: f32, cx: &mut Context<Self>) {
        if self.syncing || !self.synchronize {
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
        if self.synchronize {
            for editor in [&self.left, &self.right] {
                editor.update(cx, |editor, cx| editor.scroll_to_top(top, cx));
            }
        } else {
            let editor = match self.pane {
                Pane::Left => &self.left,
                Pane::Right => &self.right,
            };
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
        self.switch_to(next as usize, last, cx);
    }

    /// Shows another file of the set, `last` selects its last hunk, as going back does.
    fn switch_to(&mut self, index: usize, last: bool, cx: &mut Context<Self>) {
        if !matches!(
            self.source,
            Source::WorkingTree | Source::Commit(_) | Source::Branch(_)
        ) {
            return;
        }
        if index == self.index {
            return;
        }
        let Some(path) = self.files.get(index).cloned() else {
            return;
        };
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let source = self.source.clone();
        let tab = editor_of(&self.workspace, &path, cx);
        let (current, format) = match source {
            Source::WorkingTree | Source::Branch(_) => current_document(&path, tab.as_ref(), cx),
            _ => (String::new(), FileFormat::default()),
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
                Source::Clipboard | Source::File(_) | Source::Shelf(_) | Source::Text => None,
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
                let current = if matches!(this.source, Source::Commit(_)) {
                    diff.current
                } else {
                    current
                };
                let hunks = hunks_with(this.whitespace, &diff.base, &current);
                let selected = if last {
                    hunks.len().saturating_sub(1)
                } else {
                    0
                };
                this.tab = editor_of(&this.workspace, &path, cx);
                this.set_text(&path, &diff.base, &current, format, cx);
                this.path = path;
                this.changes = Changes {
                    current,
                    base: diff.base,
                    hunks,
                };
                this.index = index;
                this.selected = selected;
                this.expanded.clear();
                if this.collapse {
                    this.apply_collapse(cx);
                }
                let theme = *cx.global::<Theme>();
                this.publish_marks(&theme, cx);
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
        // The range is counted in the text of the document the edit goes to: the document
        // of the tab while the pane is a view of it, the pane's own document otherwise.
        let target = if self.attached {
            self.tab.clone().unwrap_or_else(|| self.right.clone())
        } else {
            self.right.clone()
        };
        let Some(text_now) = target.read(cx).text() else {
            self.reread(cx);
            return;
        };
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
        if self.attached
            && let Some(tab) = self.tab.clone()
        {
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
            ("escape", false, false, false) if self.menu.is_some() => {
                self.close_menu(cx);
            }
            ("f7", false, false, false) => self.next_diff(cx),
            ("f7", false, false, true) => self.previous_diff(cx),
            ("f4", false, false, false) => self.jump_to_source(window, cx),
            ("right", false, true, false) => self.next_file(cx),
            ("left", false, true, false) => self.previous_file(cx),
            ("right", false, true, true) => self.next_change(cx),
            ("left", false, true, true) => self.previous_change(cx),
            ("r", true, true, false) => self.apply_left(cx),
            ("a", true, true, false) => self.apply_right(cx),
            ("tab", true, false, true) => self.focus_opposite(window, cx),
            _ => return false,
        }
        true
    }

    /// F7 on the last difference of the last file stays there, as the file set has no
    /// next file.
    fn next_file(&mut self, cx: &mut Context<Self>) {
        self.move_file(1, false, cx);
    }

    fn previous_file(&mut self, cx: &mut Context<Self>) {
        self.move_file(-1, true, cx);
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
        // The hunks describe the text the right pane shows. While the pane is a view of
        // the document of the tab that is the text of the tab; a pane of its own (no tab,
        // or a tab that could not take its edits) is read here.
        let current = self
            .right
            .read(cx)
            .text()
            .map(|text| normalized(&text))
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
                this.adopt_tab(cx);
                let hunks = hunks_with(this.whitespace, &diff.base, &current);
                this.changes = Changes {
                    current,
                    base: diff.base,
                    hunks,
                };
                this.selected = this
                    .selected
                    .min(this.changes.hunks.len().saturating_sub(1));
                this.expanded.clear();
                if this.collapse {
                    this.apply_collapse(cx);
                }
                let theme = *cx.global::<Theme>();
                this.publish_marks(&theme, cx);
                this.git.update(cx, |git, cx| git.refresh(cx));
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Replaces both panes with editors of `base` and `current`, and follows them.
    fn set_text(
        &mut self,
        path: &Path,
        base: &str,
        current: &str,
        format: FileFormat,
        cx: &mut Context<Self>,
    ) {
        let style = style_of(cx);
        let mut options = options_of(cx);
        options.line_numbers = self.show_line_numbers;
        options.soft_wrap = self.soft_wrap;
        let left = text_view(
            path.to_path_buf(),
            base.to_owned(),
            FileFormat::default(),
            true,
            style.clone(),
            options.clone(),
            cx,
        );
        let shared_right = match (&self.tab, self.source == Source::WorkingTree) {
            (Some(tab), true) => EditorView::open_shared(tab, cx),
            _ => None,
        };
        self.attached = shared_right.is_some();
        self.detached_base = (!self.attached).then(|| normalized(current));
        self.pending_detach = None;
        self.refused = None;
        let right = shared_right.unwrap_or_else(|| {
            text_view(
                path.to_path_buf(),
                current.to_owned(),
                format,
                self.source != Source::WorkingTree,
                style.clone(),
                options.clone(),
                cx,
            )
        });
        if self.soft_wrap {
            left.update(cx, |left, _| left.unlock_soft_wrap());
            right.update(cx, |right, _| right.unlock_soft_wrap());
        }
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

    /// Whether the right pane is a view of the document of the tab, for tests.
    #[cfg(test)]
    pub(crate) fn attached(&self) -> bool {
        self.attached
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

    /// The counter of differences of the toolbar, for tests.
    #[cfg(test)]
    pub(crate) fn counter_text(&self) -> String {
        self.difference_text()
    }

    /// The title of the window or of the tab, for tests.
    #[cfg(test)]
    pub(crate) fn title_text(&self, cx: &App) -> String {
        let repo = self.git.read(cx).repo();
        let root = repo.as_ref().map_or(Path::new(""), |repo| repo.root());
        self.window_title(root)
    }

    /// The counter of files of the toolbar, for tests.
    #[cfg(test)]
    pub(crate) fn files_label(&self) -> String {
        self.files_text()
    }

    #[cfg(test)]
    pub(crate) fn whitespace_mode(&self) -> DiffWhitespace {
        self.whitespace
    }

    #[cfg(test)]
    pub(crate) fn highlighting_mode(&self) -> DiffHighlighting {
        self.highlighting
    }

    #[cfg(test)]
    pub(crate) fn context(&self) -> usize {
        self.context_lines
    }

    #[cfg(test)]
    pub(crate) fn synchronizes(&self) -> bool {
        self.synchronize
    }

    #[cfg(test)]
    pub(crate) fn line_numbers(&self) -> bool {
        self.show_line_numbers
    }

    #[cfg(test)]
    pub(crate) fn wraps(&self) -> bool {
        self.soft_wrap
    }

    #[cfg(test)]
    pub(crate) fn collapses(&self) -> bool {
        self.collapse
    }

    #[cfg(test)]
    pub(crate) fn locked(&self, left: bool) -> bool {
        self.side_locked(left)
    }

    #[cfg(test)]
    pub(crate) fn in_tab(&self) -> bool {
        self.tabbed
    }

    /// The labels of the open menu, for tests.
    #[cfg(test)]
    pub(crate) fn menu_labels(&self) -> Vec<String> {
        self.menu_lines()
            .into_iter()
            .map(|line| match line {
                MenuLine::Section(title) => title.to_owned(),
                MenuLine::Row(row) => row.label,
            })
            .collect()
    }

    /// The labels of the gaps the unified list folds away, for tests.
    #[cfg(test)]
    pub(crate) fn unchanged_labels(&self) -> Vec<String> {
        self.unified_items()
            .into_iter()
            .filter_map(|item| match item {
                UnifiedItem::Gap { count, .. } => Some(format!("{count} unchanged lines")),
                UnifiedItem::Row(_) => None,
            })
            .collect()
    }

    /// The gaps the diff shows now, for tests of the collapse switch.
    #[cfg(test)]
    pub(crate) fn gap_count(&self) -> usize {
        self.unchanged_labels().len()
    }

    /// The first row of every gap the unified list folds away, for tests.
    #[cfg(test)]
    pub(crate) fn gap_ids(&self) -> Vec<usize> {
        self.unified_items()
            .into_iter()
            .filter_map(|item| match item {
                UnifiedItem::Gap { at, .. } => Some(at),
                UnifiedItem::Row(_) => None,
            })
            .collect()
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
        rows_of_with(
            &self.changes.base,
            &self.changes.current,
            &self.changes.hunks,
            self.highlighting,
        )
    }

    /// The unified list: every row, with the unchanged gaps the setting folds away
    /// unless a click opened them.
    fn unified_items(&self) -> Vec<UnifiedItem> {
        let rows = self.rows();
        let mut items = Vec::new();
        let mut index = 0;
        while index < rows.len() {
            if rows[index].hunk.is_some() {
                items.push(UnifiedItem::Row(rows[index].clone()));
                index += 1;
                continue;
            }
            let start = index;
            while index < rows.len() && rows[index].hunk.is_none() {
                index += 1;
            }
            let end = index;
            let keep = self.context_lines;
            if !self.collapse || self.expanded.contains(&start) || end - start <= keep * 2 + 1 {
                for row in &rows[start..end] {
                    items.push(UnifiedItem::Row(row.clone()));
                }
            } else {
                for row in &rows[start..start + keep] {
                    items.push(UnifiedItem::Row(row.clone()));
                }
                items.push(UnifiedItem::Gap {
                    at: start,
                    count: end - start - keep * 2,
                });
                for row in &rows[end - keep..end] {
                    items.push(UnifiedItem::Row(row.clone()));
                }
            }
        }
        items
    }

    /// The header of one hunk of the unified list: its first line and its Revert button.
    fn hunk_header(&self, index: usize, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let hunk = &self.changes.hunks[index];
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
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_between()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .mt(px(SPACE_XS))
            .when(self.selected == index, |this| {
                this.bg(theme.selection_inactive)
            })
            .when(self.selected != index, |this| this.bg(theme.hover))
            .text_color(theme.text_secondary)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .children(self.commit_checkbox(index, theme, cx))
                    .child(format!("Line {}", start + 1)),
            )
            .children(revert)
    }

    /// One line of the unified list with its number, its sign and the marks of the words
    /// that differ.
    fn unified_line(
        &self,
        line: Option<&DiffLine>,
        sign: &'static str,
        kind: Option<Kind>,
        right: bool,
        theme: &Theme,
    ) -> AnyElement {
        let Some(line) = line else {
            return div().into_any_element();
        };
        let tint = kind.map(|kind| kind.color(theme));
        let words = (kind.is_some())
            .then(|| word_tint(Some(Kind::Modified), right, theme))
            .flatten();
        div()
            .flex()
            .flex_none()
            .h(px(LINE_HEIGHT))
            .items_center()
            .when_some(tint, |row, tint| row.bg(tint.opacity(0.18)))
            .child(
                div()
                    .flex_none()
                    .w(px(44.))
                    .pr(px(SPACE_XS))
                    .justify_end()
                    .text_color(theme.text_secondary)
                    .child(line.number.to_string()),
            )
            .child(
                div()
                    .flex_none()
                    .w(px(16.))
                    .text_color(theme.text_secondary)
                    .child(sign),
            )
            .child(marked_text(&line.text, &line.words, words, theme))
            .into_any_element()
    }

    /// The path git names the file by, which the draft of a commit keys its hunks with.
    fn status_path(&self, cx: &App) -> String {
        self.git
            .read(cx)
            .repo()
            .map(|repo| relative_of(repo.root(), &self.path))
            .unwrap_or_default()
    }

    /// The tick of one hunk in the diff of the Commit window: a hunk without one stays
    /// out of the commit and in the working tree. None outside that diff.
    fn commit_checkbox(
        &self,
        index: usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let draft = self.commit.clone()?;
        let hunk = self.changes.hunks.get(index)?.clone();
        let path = self.status_path(cx);
        let checked = draft.read(cx).hunk_checked(&path, &hunk);
        let total = self.changes.hunks.len();
        let click_path = path.clone();
        let click_hunk = hunk.clone();
        Some(
            div()
                .id(("commit-hunk", index))
                .debug_selector(move || format!("commit-hunk-{index}"))
                .flex_none()
                .size(px(14.))
                .rounded(px(3.))
                .border_1()
                .border_color(if checked {
                    theme.accent
                } else {
                    theme.text_secondary
                })
                .when(checked, |this| this.bg(theme.accent))
                .on_click(cx.listener(move |_this, _, _, cx| {
                    let now = !draft.read(cx).hunk_checked(&click_path, &click_hunk);
                    draft.update(cx, |draft, cx| {
                        draft.set_hunk_checked(&click_path, &click_hunk, now, total);
                        cx.notify();
                    });
                    cx.notify();
                }))
                .child(
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .when(checked, |this| {
                            this.child(div().size(px(6.)).rounded(px(1.)).bg(theme.over_accent))
                        }),
                ),
        )
    }

    /// The empty rows and the backgrounds of both panes, so that the rows of a hunk face
    /// each other and the words that differ stand out. A fold takes rows away, so the
    /// empty rows are counted on the rows the panes show now.
    fn decorations(&self, rows: &[DiffRow], theme: &Theme, cx: &App) -> (PaneDecor, PaneDecor) {
        let left_visible = self.left.read(cx).shown_lines();
        let right_visible = self.right.read(cx).shown_lines();
        let (mut left, mut right) = decorations_with_visible(
            rows,
            &self.changes.base,
            &self.changes.current,
            self.selected,
            theme,
            &left_visible,
            &right_visible,
        );
        match self.highlighting {
            DiffHighlighting::ByWord
            | DiffHighlighting::SplitChanges
            | DiffHighlighting::ByCharacter => {}
            DiffHighlighting::ByLine => {
                left.ranges.clear();
                right.ranges.clear();
            }
            DiffHighlighting::None => {
                left.lines.clear();
                left.ranges.clear();
                right.lines.clear();
                right.ranges.clear();
            }
        }
        (left, right)
    }

    /// The strip between the panes: a band for every hunk and its buttons, at the rows
    /// the two panes show.
    fn strip(&self, rows: &[DiffRow], theme: &Theme, cx: &mut Context<Self>) -> Div {
        let line_height = self.right.read(cx).line_height().max(1.);
        let top = self.right.read(cx).scroll_top();
        let shown: HashSet<usize> = self.left.read(cx).shown_lines().into_iter().collect();
        // The row a buffer row has on screen, counted without the lines a fold hides.
        let shown_row = |from: usize| {
            rows[..from]
                .iter()
                .filter(|row| match &row.left {
                    Some(line) => shown.contains(&(line.number - 1)),
                    None => true,
                })
                .count() as f32
        };
        let mut spans = Vec::new();
        for hunk in 0..self.changes.hunks.len() {
            let Some(first) = rows.iter().position(|row| row.hunk == Some(hunk)) else {
                continue;
            };
            let last = rows
                .iter()
                .rposition(|row| row.hunk == Some(hunk))
                .unwrap_or(first);
            let y = shown_row(first) * line_height - top;
            let height = (shown_row(last + 1) - shown_row(first)).max(1.) * line_height;
            let kind = hunk_kind(&self.changes.hunks[hunk]);
            spans.push((y, height, kind.color(theme)));
        }
        let connectors = spans.clone();
        let mut strip = div()
            .relative()
            .flex_none()
            .w(px(STRIP_WIDTH))
            .h_full()
            .overflow_hidden()
            .child(
                canvas(
                    move |_, _, _| {},
                    move |bounds, _, window, _| paint_connectors(bounds, &connectors, window),
                )
                .absolute()
                .size_full(),
            );
        for (hunk, (y, height, color)) in spans.into_iter().enumerate() {
            strip = strip.child(
                div()
                    .absolute()
                    .left(px((STRIP_WIDTH - 3.) / 2.))
                    .top(px(y))
                    .w(px(3.))
                    .h(px(height))
                    .rounded(px(CONTROL_RADIUS))
                    .bg(color)
                    .opacity(0.5),
            );
            if let Some(first) = rows.iter().position(|row| row.hunk == Some(hunk)) {
                let last = rows
                    .iter()
                    .rposition(|row| row.hunk == Some(hunk))
                    .unwrap_or(first);
                if let Some(line) = rows[first].left.as_ref() {
                    strip = strip.child(
                        div()
                            .absolute()
                            .left(px(1.))
                            .top(px(y))
                            .text_size(px(9.))
                            .text_color(theme.text_secondary)
                            .child(line.number.to_string()),
                    );
                }
                if let Some(line) = rows[last].right.as_ref() {
                    strip = strip.child(
                        div()
                            .absolute()
                            .right(px(1.))
                            .top(px(y))
                            .text_size(px(9.))
                            .text_color(theme.text_secondary)
                            .child(line.number.to_string()),
                    );
                }
            }
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
                    .children(self.commit_checkbox(hunk, theme, cx))
                    .child(apply_left)
                    .child(apply_right)
                    .child(discard),
            );
        }
        strip
    }

    /// The header of a side: its lock when it is read only, its revision and its path.
    fn header(&self, left: bool, theme: &Theme) -> Div {
        let (revision, path) = {
            let (left_revision, right_revision) = self.revisions();
            let (left_path, right_path) = self.side_paths();
            if left {
                (left_revision, left_path)
            } else {
                (right_revision, right_path)
            }
        };
        let side = if left {
            "diff-side-left"
        } else {
            "diff-side-right"
        };
        let lock_name = if left {
            "diff-lock-left"
        } else {
            "diff-lock-right"
        };
        div()
            .debug_selector(move || side.to_owned())
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_secondary)
            .when(self.side_locked(left), |header| {
                header.child(
                    div()
                        .debug_selector(move || lock_name.to_owned())
                        .flex()
                        .flex_none()
                        .child(icon_sized("general/lock", 12., theme)),
                )
            })
            .child(div().flex_none().child(revision))
            .child(div().flex_1().min_w_0().truncate().child(path))
    }

    fn headers(&self, theme: &Theme) -> Div {
        div()
            .debug_selector(|| "diff-headers".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .border_b_1()
            .border_color(theme.border)
            .child(self.header(true, theme))
            .child(div().flex_none().w(px(STRIP_WIDTH)))
            .child(self.header(false, theme))
    }

    /// The toolbar of the diff, left to right as IDEA has it: the differences, Jump to
    /// Source, the files of the set, the collapse switch, the counter, the layout and the
    /// gear.
    fn toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let files = div()
            .id("diff-files")
            .debug_selector(|| "diff-files".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .tooltip(|_, cx| tooltip("Files of the diff", None, cx))
            .child(self.files_text())
            .child(icon_sized("general/chevronDown", 10., theme))
            .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Files, cx)));
        let layout = |name: &'static str,
                      icon: &'static str,
                      label: &'static str,
                      active: bool,
                      cx: &mut Context<Self>| {
            let id = if name == "side" {
                "diff-layout".to_owned()
            } else {
                format!("diff-layout-{name}")
            };
            let selector = id.clone();
            let button = icon_button(id, icon, label, None, theme)
                .debug_selector(move || selector.clone())
                .when(active, |button| button.bg(theme.selection_inactive));
            button.on_click(cx.listener(|this, _, window, cx| this.toggle_layout(window, cx)))
        };
        div()
            .id("diff-toolbar")
            .debug_selector(|| "diff-toolbar".to_owned())
            .flex()
            .flex_none()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .border_b_1()
            .border_color(theme.border)
            .child(
                icon_button(
                    "diff-prev",
                    "general/arrowUp",
                    "Previous Difference",
                    Some("Shift+F7".into()),
                    theme,
                )
                .debug_selector(|| "diff-prev".to_owned())
                .on_click(cx.listener(|this, _, _, cx| this.previous_diff(cx))),
            )
            .child(
                icon_button(
                    "diff-next",
                    "general/arrowDown",
                    "Next Difference",
                    Some("F7".into()),
                    theme,
                )
                .debug_selector(|| "diff-next".to_owned())
                .on_click(cx.listener(|this, _, _, cx| this.next_diff(cx))),
            )
            .child(
                icon_button(
                    "diff-jump",
                    "general/locate",
                    "Jump to Source",
                    Some("F4".into()),
                    theme,
                )
                .debug_selector(|| "diff-jump".to_owned())
                .on_click(cx.listener(|this, _, window, cx| this.jump_to_source(window, cx))),
            )
            .child(
                icon_button(
                    "diff-prev-file",
                    "general/arrowLeft",
                    "Previous File",
                    Some("Alt+Left".into()),
                    theme,
                )
                .debug_selector(|| "diff-prev-file".to_owned())
                .on_click(cx.listener(|this, _, _, cx| this.previous_file(cx))),
            )
            .child(
                icon_button(
                    "diff-next-file",
                    "general/arrowRight",
                    "Next File",
                    Some("Alt+Right".into()),
                    theme,
                )
                .debug_selector(|| "diff-next-file".to_owned())
                .on_click(cx.listener(|this, _, _, cx| this.next_file(cx))),
            )
            .child(files)
            .child(
                icon_button(
                    "diff-collapse",
                    "general/collapseAll",
                    "Collapse Unchanged Fragments",
                    None,
                    theme,
                )
                .debug_selector(|| "diff-collapse".to_owned())
                .when(self.collapse, |button| button.bg(theme.selection_inactive))
                .on_click(cx.listener(|this, _, _, cx| this.toggle_collapse(cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .debug_selector(|| "diff-count".to_owned())
                    .flex_none()
                    .px(px(SPACE_S))
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child(self.difference_text()),
            )
            .child(layout(
                "side",
                "general/editorAndPreview",
                "Side-by-side",
                self.side_by_side,
                cx,
            ))
            .child(layout(
                "unified",
                "general/editorOnly",
                "Unified",
                !self.side_by_side,
                cx,
            ))
            .child(
                icon_button("diff-settings", "general/settings", "Settings", None, theme)
                    .debug_selector(|| "diff-settings".to_owned())
                    .when(self.menu == Some(Menu::Settings), |button| {
                        button.bg(theme.selection_inactive)
                    })
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_menu(Menu::Settings, cx))),
            )
    }

    /// The menu of the gear or of the counter of files, drawn over the diff.
    fn menu_panel(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(menu) = self.menu else {
            return div().into_any_element();
        };
        let name = match menu {
            Menu::Settings => "diff-settings-menu",
            Menu::Files => "diff-files-menu",
        };
        let mut lines = self.menu_lines();
        // The choices of the diff do not fit one column of the height of the window: the
        // colours of the places sit in the first one, the switches and the context in the
        // second.
        let second = (menu == Menu::Settings && lines.len() > 11).then(|| lines.split_off(11));
        let mut columns = Vec::new();
        columns.push(self.menu_column(lines, theme, cx));
        if let Some(second) = second {
            columns.push(self.menu_column(second, theme, cx));
        }
        let width = if menu == Menu::Settings { 2. } else { 1. };
        let panel = popup_frame(theme)
            .id(name)
            .debug_selector(move || name.to_owned())
            .absolute()
            .top(px(40. + CONTROL_HEIGHT + 2.))
            .right(px(SPACE_M))
            .w(px(MENU_WIDTH * width))
            .max_h(px(560.))
            .p(px(SPACE_XS))
            .flex()
            .flex_row()
            .items_start()
            .children(columns);
        div()
            .id("diff-menu-backdrop")
            .debug_selector(|| "diff-menu-backdrop".to_owned())
            .absolute()
            .left(px(0.))
            .top(px(0.))
            .w_full()
            .h_full()
            .on_click(cx.listener(|this, _, _, cx| this.close_menu(cx)))
            .child(panel)
            .into_any_element()
    }

    /// One column of the open menu.
    fn menu_column(
        &self,
        lines: Vec<MenuLine>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut column = div().flex().flex_col().flex_1().min_w_0();
        for line in lines {
            match line {
                MenuLine::Section(title) => {
                    column = column.child(
                        div()
                            .flex_none()
                            .px(px(SPACE_S))
                            .pt(px(SPACE_XS))
                            .pb(px(2.))
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_secondary)
                            .child(title),
                    );
                }
                MenuLine::Row(row) => column = column.child(self.menu_row(row, theme, cx)),
            }
        }
        column.into_any_element()
    }

    /// One row of the open menu: its check mark, its label and its action.
    fn menu_row(&self, row: MenuRow, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let action = row.action;
        div()
            .id(row.id.clone())
            .debug_selector({
                let id = row.id.clone();
                move || id.clone()
            })
            .flex()
            .flex_none()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(MENU_ROW_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .hover({
                let hover = theme.hover;
                move |style| style.bg(hover)
            })
            .child(if row.checked {
                div()
                    .flex_none()
                    .w(px(12.))
                    .child(icon_sized("general/check", 10., theme))
                    .into_any_element()
            } else {
                div().flex_none().w(px(12.)).into_any_element()
            })
            .child(row.label)
            .on_click(cx.listener(move |this, _, window, cx| this.run_menu(action, window, cx)))
    }
}

/// The figures that join the two panes: a band of the colour of the change that curves
/// from the left edge of the strip to the right one, as IDEA draws them.
fn paint_connectors(bounds: Bounds<Pixels>, spans: &[(f32, f32, Hsla)], window: &mut Window) {
    let height = f32::from(bounds.size.height);
    let left = bounds.left();
    let right = bounds.right();
    let middle = left + bounds.size.width / 2.;
    for (top, rows_height, color) in spans {
        if *top > height || top + rows_height < 0. {
            continue;
        }
        let top = bounds.top() + px(*top);
        let bottom = top + px(*rows_height);
        let mut path = PathBuilder::fill();
        path.move_to(point(left, top));
        path.curve_to(point(right, top), point(middle, top - px(5.)));
        path.line_to(point(right, bottom));
        path.curve_to(point(left, bottom), point(middle, bottom + px(5.)));
        path.close();
        if let Ok(path) = path.build() {
            window.paint_path(path, color.opacity(0.22));
        }
    }
}

/// A line of text with the ranges that differ painted over it.
fn marked_text(text: &str, ranges: &[Range<usize>], color: Option<Hsla>, theme: &Theme) -> Div {
    let mut row = div().flex().whitespace_nowrap().text_color(theme.text);
    if ranges.is_empty() {
        return row.child(text.to_owned());
    }
    let mut at = 0;
    for range in ranges {
        if range.start < at || range.end > text.len() || range.start >= range.end {
            continue;
        }
        if at < range.start {
            row = row.child(text[at..range.start].to_owned());
        }
        row = row.child(
            div()
                .when_some(color, |mark, color| mark.bg(color.opacity(0.4)))
                .child(text[range.clone()].to_owned()),
        );
        at = range.end;
    }
    if at < text.len() {
        row = row.child(text[at..].to_owned());
    }
    row
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
    let removed = Some(Kind::Conflicted.color(theme));
    let added = Some(Kind::Added.color(theme));

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
        let title = self.window_title(root);
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
                .child(panes)
                .into_any_element()
        } else {
            let mut rows: Vec<AnyElement> = Vec::new();
            let mut shown_hunk = None;
            for item in self.unified_items() {
                match item {
                    UnifiedItem::Row(row) => {
                        if let Some(hunk) = row.hunk
                            && shown_hunk != Some(hunk)
                        {
                            shown_hunk = Some(hunk);
                            rows.push(self.hunk_header(hunk, &theme, cx).into_any_element());
                        }
                        if row.kind == Some(Kind::Modified) && row.left.is_some() {
                            rows.push(self.unified_line(
                                row.left.as_ref(),
                                "-",
                                Some(Kind::Deleted),
                                false,
                                &theme,
                            ));
                            rows.push(self.unified_line(
                                row.right.as_ref(),
                                "+",
                                Some(Kind::Added),
                                true,
                                &theme,
                            ));
                        } else if row.kind.is_some() {
                            if row.left.is_some() {
                                rows.push(self.unified_line(
                                    row.left.as_ref(),
                                    "-",
                                    Some(Kind::Deleted),
                                    false,
                                    &theme,
                                ));
                            }
                            rows.push(self.unified_line(
                                row.right.as_ref(),
                                "+",
                                Some(Kind::Added),
                                true,
                                &theme,
                            ));
                        } else {
                            rows.push(self.unified_line(
                                row.right.as_ref().or(row.left.as_ref()),
                                " ",
                                None,
                                true,
                                &theme,
                            ));
                        }
                    }
                    UnifiedItem::Gap { at, count } => {
                        rows.push(
                            div()
                                .id(("diff-gap", at))
                                .debug_selector(move || format!("diff-unchanged-{at}"))
                                .flex()
                                .flex_none()
                                .h(px(LINE_HEIGHT))
                                .items_center()
                                .justify_center()
                                .rounded(px(CONTROL_RADIUS))
                                .bg(theme.hover)
                                .text_color(theme.text_secondary)
                                .text_size(px(TEXT_SMALL))
                                .hover({
                                    let pressed = theme.pressed;
                                    move |style| style.bg(pressed)
                                })
                                .tooltip(|_, cx| tooltip("Show these lines", None, cx))
                                .child(format!("{count} unchanged lines"))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.expanded.insert(at);
                                    this.apply_collapse(cx);
                                    cx.notify();
                                }))
                                .into_any_element(),
                        );
                    }
                }
            }
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
            .relative()
            .on_action(cx.listener(|this, _: &NextDiff, _, cx| this.next_diff(cx)))
            .on_action(cx.listener(|this, _: &PreviousDiff, _, cx| this.previous_diff(cx)))
            .on_action(cx.listener(|this, _: &NextChange, _, cx| this.next_change(cx)))
            .on_action(cx.listener(|this, _: &PreviousChange, _, cx| this.previous_change(cx)))
            .on_action(cx.listener(|this, _: &ApplyLeftSide, _, cx| this.apply_left(cx)))
            .on_action(cx.listener(|this, _: &ApplyRightSide, _, cx| this.apply_right(cx)))
            .on_action(
                cx.listener(|this, _: &JumpToSource, window, cx| this.jump_to_source(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NextFile, _, cx| this.next_file(cx)))
            .on_action(cx.listener(|this, _: &PreviousFile, _, cx| this.previous_file(cx)))
            .on_action(cx.listener(|this, _: &ToggleCollapse, _, cx| this.toggle_collapse(cx)))
            .on_action(cx.listener(|this, _: &FocusOppositePane, window, cx| {
                this.focus_opposite(window, cx)
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                if this.menu.is_some() && event.keystroke.key == "escape" {
                    this.close_menu(cx);
                    cx.stop_propagation();
                } else if this.diff_key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .when(!self.tabbed, |body| {
                body.occlude()
                    .w(width)
                    .h(height)
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            })
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
                    .child(title),
            )
            .child(self.toolbar(&theme, cx))
            .child(self.headers(&theme))
            .child(pane)
            .children(self.menu.map(|_| self.menu_panel(&theme, cx)));
        if self.tabbed {
            body.flex().flex_col().w_full().h_full().into_any_element()
        } else {
            div()
                .absolute()
                .left(left)
                .top(top)
                .child(enter("diff-enter", body))
                .into_any_element()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Changes, DiffWhitespace, character_changes, hunks_with};

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

    #[test]
    fn whitespace_choices_hide_only_what_they_leave_out_of_the_comparison() {
        let base = "alpha\nbeta\n";
        let current = "alpha\n   beta\n";

        assert_eq!(
            hunks_with(DiffWhitespace::DoNotIgnore, base, current).len(),
            1
        );
        assert!(hunks_with(DiffWhitespace::Trim, base, current).is_empty());
        assert!(hunks_with(DiffWhitespace::Ignore, base, current).is_empty());
        assert!(hunks_with(DiffWhitespace::IgnoreAndEmptyLines, base, current).is_empty());

        // A line of nothing but whitespace is a change unless the last choice is on.
        let with_blank = "alpha\n\nbeta\n";
        assert_eq!(
            hunks_with(DiffWhitespace::Ignore, base, with_blank).len(),
            1
        );
        assert!(hunks_with(DiffWhitespace::IgnoreAndEmptyLines, base, with_blank).is_empty());
    }

    #[test]
    fn the_characters_of_a_changed_line_are_marked_around_the_common_ends() {
        let before = "colour = red";
        let after = "colour = blue";
        let changes = character_changes(before, after);

        assert_eq!(changes.before, vec![9..12]);
        assert_eq!(changes.after, vec![9..13]);
    }
}

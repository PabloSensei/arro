//! The Merge window of a conflicted file: our version on the left, their version on the
//! right and the editable result in the middle. Every chunk of a side takes its lines into
//! the result, the conflict keys walk the chunks that disagree, and Apply writes the
//! result over the file and stages it.

use std::ops::Range;
use std::path::{Path, PathBuf};

use editor_view::{EditorEvent, EditorView, PaneDecor};
use text_core::FileFormat;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use vcs::{ConflictSide, Hunk, Segment, SegmentKind, ThreeWay};
use workspace::Workspace;

use crate::diff_view::{line_starts, text_view};
use crate::editor_tab::{options_of, style_of};
use crate::git::{GitState, Kind, relative_of};

const WIDTH: f32 = 1400.;
const STRIP_WIDTH: f32 = 44.;
const CONTEXT: &str = "MergeView";
/// The keys of the popup while one of its editors has the focus, as in the diff.
const EDITOR_CONTEXT: &str = "MergeEditor";

actions!(merge_view, [NextConflict, PreviousConflict]);

/// The keys of the popup and of its editors. F7 and Shift+F7 are the next and previous
/// conflict inside this window only; the window keys keep them for the debugger.
pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("f7", NextConflict, Some(CONTEXT)),
        KeyBinding::new("f7", NextConflict, Some(EDITOR_CONTEXT)),
        KeyBinding::new("shift-f7", PreviousConflict, Some(CONTEXT)),
        KeyBinding::new("shift-f7", PreviousConflict, Some(EDITOR_CONTEXT)),
    ]);
}

/// How many times the display ranged the result against its first text, for tests of big
/// files: the chunks are read when the window needs them, not on every keystroke.
#[cfg(test)]
static CHUNK_RECOMPUTES: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn chunk_recomputes() -> usize {
    CHUNK_RECOMPUTES.load(std::sync::atomic::Ordering::Relaxed)
}

#[cfg(test)]
pub(crate) fn reset_chunk_recomputes() {
    CHUNK_RECOMPUTES.store(0, std::sync::atomic::Ordering::Relaxed);
}

/// One changed place of the file, in the text the result started with.
struct Chunk {
    kind: SegmentKind,
    /// Lines of the first result this chunk takes.
    initial: Range<usize>,
    /// Lines of our version and of their version that face the result.
    yours_lines: Range<usize>,
    theirs_lines: Range<usize>,
    /// The texts the arrows put into the result.
    base: String,
    yours: String,
    theirs: String,
}

/// Where the chunks of the current result sit there, with the text they were counted on.
/// Read when the window draws or a key acts on a chunk, and kept until the text changes.
struct Display {
    version: i32,
    text: String,
    ranges: Vec<Range<usize>>,
}

/// An empty row a pane gets so that the start of every segment faces the start of the
/// same segment on the other side. The result is not padded: a hand edit changes its rows.
struct Alignment {
    yours: Vec<(usize, usize)>,
    theirs: Vec<(usize, usize)>,
    trailing_yours: usize,
    trailing_theirs: usize,
    /// The display row of the first line of every chunk, in ours and in theirs.
    chunks: Vec<(usize, usize)>,
}

/// A question that stands over the window before it does what a button asked.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Question {
    /// Cancel with edits in the result.
    Discard,
    /// Apply with chunks still at the base text.
    ApplyAnyway(usize),
}

/// What a write of the result ended with.
enum Outcome {
    /// The file on disk is not the one the window opened: nothing was written.
    Changed,
    /// The result reached the disk and git staged it. `None` means the file was removed.
    Done(Option<Vec<u8>>),
    /// The result reached the disk but git did not stage it.
    Unstaged {
        bytes: Option<Vec<u8>>,
        message: String,
    },
    Failed(String),
}

/// What the background read of a conflicted file brings to the window.
struct Snapshot {
    relative: String,
    base: String,
    yours: String,
    theirs: String,
    /// The side the file is gone from, for a delete/modify conflict.
    missing: Option<ConflictSide>,
    /// The format the working file is written in: its encoding, BOM and line ends.
    format: FileFormat,
    /// The bytes of the working file when the window opened, to refuse a file that moved.
    opened: Option<Vec<u8>>,
}

pub struct MergeView {
    git: Entity<GitState>,
    workspace: WeakEntity<Workspace>,
    path: PathBuf,
    relative: String,
    focus: FocusHandle,
    yours: Entity<EditorView>,
    result: Entity<EditorView>,
    theirs: Entity<EditorView>,
    yours_text: String,
    theirs_text: String,
    initial_result: String,
    segments: Vec<Segment>,
    chunks: Vec<Chunk>,
    format: FileFormat,
    opened: Option<Vec<u8>>,
    missing: Option<ConflictSide>,
    display: Option<Display>,
    /// The conflict the keys work on, by its place among the conflicts.
    selected: usize,
    question: Option<Question>,
    /// A write is on its way; a second Apply waits for it.
    busy: bool,
    /// A scroll of one pane is being copied to the others.
    syncing: bool,
    subscriptions: Vec<Subscription>,
}

impl MergeView {
    /// Reads the three stages and the working file in the background and opens the window.
    pub fn open(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(repo) = git.read(cx).repo() else {
            return;
        };
        let job_path = path.clone();
        let job = cx.background_executor().spawn(async move {
            read_snapshot(&repo, &job_path).map_err(|error| error.to_string())
        });
        window
            .spawn(cx, async move |cx| {
                let snapshot = job.await;
                cx.update(|window, cx| match snapshot {
                    Ok(snapshot) => {
                        Self::show(git, workspace, path, snapshot, window, cx);
                    }
                    Err(message) => {
                        git.update(cx, |git, cx| git.report("Merge", message, cx));
                    }
                })
                .ok();
            })
            .detach();
    }

    fn show(
        git: Entity<GitState>,
        workspace: WeakEntity<Workspace>,
        path: PathBuf,
        snapshot: Snapshot,
        window: &mut Window,
        cx: &mut App,
    ) {
        let Some(shared) = workspace.upgrade() else {
            return;
        };
        let three = vcs::merge(&snapshot.base, &snapshot.yours, &snapshot.theirs);
        let chunks = chunks_of(&three, &snapshot.base, &snapshot.yours, &snapshot.theirs);
        let style = style_of(cx);
        let options = options_of(cx);
        let read_only = |text: String, style, options, cx: &mut App| {
            text_view(
                path.clone(),
                text,
                FileFormat::default(),
                true,
                style,
                options,
                cx,
            )
        };
        let yours = read_only(snapshot.yours.clone(), style.clone(), options.clone(), cx);
        let theirs = read_only(snapshot.theirs.clone(), style.clone(), options.clone(), cx);
        let result_text = three.result.clone();
        let result_path = path.clone();
        let result_format = snapshot.format.clone();
        let result = {
            let style = style.clone();
            let options = options.clone();
            cx.new(move |cx| {
                let mut view = EditorView::open_text_document(
                    result_path,
                    result_text,
                    result_format,
                    false,
                    false,
                    style,
                    cx,
                );
                view.set_options(options, cx);
                view.lock_soft_wrap();
                view
            })
        };
        let focus = result.read(cx).focus_handle(cx);
        let relative = snapshot.relative;
        let opened = snapshot.opened;
        let missing = snapshot.missing;
        let format = snapshot.format;
        let yours_text = snapshot.yours;
        let theirs_text = snapshot.theirs;
        let initial_result = three.result.clone();
        let segments = three.segments;
        let weak = workspace.clone();
        shared.update(cx, |workspace, cx| {
            let view = cx.new(|cx| Self {
                git,
                workspace: weak,
                path,
                relative,
                focus: cx.focus_handle(),
                yours,
                result,
                theirs,
                yours_text,
                theirs_text,
                initial_result,
                segments,
                chunks,
                format,
                opened,
                missing,
                display: None,
                selected: 0,
                question: None,
                busy: false,
                syncing: false,
                subscriptions: Vec::new(),
            });
            view.update(cx, |view, cx| view.subscribe(cx));
            let focus = focus.clone();
            workspace.show_overlay(view.into(), focus.clone(), window, cx);
            // The editor joins the dispatch tree with the first frame: the keys are its
            // own once it is there.
            window.on_next_frame(move |window, cx| window.focus(&focus, cx));
        });
    }

    fn subscribe(&mut self, cx: &mut Context<Self>) {
        let editors = [self.yours.clone(), self.result.clone(), self.theirs.clone()];
        self.subscriptions = editors
            .into_iter()
            .map(|editor| {
                cx.subscribe(
                    &editor,
                    |this: &mut Self, source: Entity<EditorView>, event: &EditorEvent, cx| {
                        // A text change repaints the editor itself. The chunks are read
                        // when the window needs their bounds, not on every keystroke: the
                        // panes and the strips stay as they were until a key or a button
                        // moves them.
                        if let EditorEvent::Scrolled(top) = event {
                            this.scrolled(&source, *top, cx);
                        }
                    },
                )
            })
            .collect();
    }

    /// One pane scrolled: the other two follow, so the chunks stay face to face.
    fn scrolled(&mut self, source: &Entity<EditorView>, top: f32, cx: &mut Context<Self>) {
        if self.syncing {
            return;
        }
        self.syncing = true;
        let mut moved = false;
        for editor in [&self.yours, &self.result, &self.theirs] {
            if editor == source {
                continue;
            }
            if (editor.read(cx).scroll_top() - top).abs() > 0.5 {
                editor.update(cx, |editor, cx| editor.scroll_to_top(top, cx));
                moved = true;
            }
        }
        self.syncing = false;
        if moved {
            cx.notify();
        }
    }

    /// The byte ranges of the chunks in the current result, counted against its first
    /// text. The ranges are read again when a key, a button or a first draw needs them;
    /// typing repaints the editors alone and never reads the diff again.
    fn chunk_ranges(&mut self, cx: &mut Context<Self>) -> Vec<Range<usize>> {
        let (version, text) = {
            let result = self.result.read(cx);
            (
                result.version().unwrap_or(0),
                result.text().unwrap_or_default(),
            )
        };
        if let Some(display) = &self.display
            && display.version == version
            && display.text == text
        {
            return display.ranges.clone();
        }
        #[cfg(test)]
        CHUNK_RECOMPUTES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let hunks = vcs::line_hunks(&self.initial_result, &text);
        let starts = line_starts(&text);
        let ranges = self
            .chunks
            .iter()
            .map(|chunk| {
                let lines = map_lines(&hunks, chunk.initial.clone());
                let from = starts.get(lines.start).copied().unwrap_or(text.len());
                let to = starts.get(lines.end).copied().unwrap_or(text.len());
                from..to.max(from)
            })
            .collect();
        self.display = Some(Display {
            version,
            text,
            ranges,
        });
        self.display
            .as_ref()
            .map(|display| display.ranges.clone())
            .unwrap_or_default()
    }

    /// The ranges the window draws with: the ones of the last read, or the first read
    /// when the window has none yet. A keystroke does not start a read.
    fn drawn_ranges(&mut self, cx: &mut Context<Self>) -> Vec<Range<usize>> {
        match &self.display {
            Some(display) => display.ranges.clone(),
            None => self.chunk_ranges(cx),
        }
    }

    /// The empty rows that keep the start of every segment on one height in our pane and
    /// theirs, and the display row of every chunk in both.
    fn alignment(&self) -> Alignment {
        let (mut yours_rows, mut theirs_rows) = (0usize, 0usize);
        let mut yours = Vec::new();
        let mut theirs = Vec::new();
        let mut chunks = Vec::new();
        for segment in &self.segments {
            if yours_rows < theirs_rows {
                yours.push((segment.yours.start, theirs_rows - yours_rows));
                yours_rows = theirs_rows;
            } else if theirs_rows < yours_rows {
                theirs.push((segment.theirs.start, yours_rows - theirs_rows));
                theirs_rows = yours_rows;
            }
            if segment.kind != SegmentKind::Unchanged {
                chunks.push((yours_rows, theirs_rows));
            }
            yours_rows += segment.yours.len();
            theirs_rows += segment.theirs.len();
        }
        let (trailing_yours, trailing_theirs) = if yours_rows < theirs_rows {
            (theirs_rows - yours_rows, 0)
        } else {
            (0, yours_rows.saturating_sub(theirs_rows))
        };
        Alignment {
            yours,
            theirs,
            trailing_yours,
            trailing_theirs,
            chunks,
        }
    }

    fn decorations(
        &self,
        ranges: &[Range<usize>],
        current: &str,
        theme: &Theme,
        alignment: &Alignment,
    ) -> (PaneDecor, PaneDecor, PaneDecor) {
        let mut yours = PaneDecor {
            fillers: alignment.yours.clone(),
            trailing_fillers: alignment.trailing_yours,
            ..PaneDecor::default()
        };
        let mut theirs = PaneDecor {
            fillers: alignment.theirs.clone(),
            trailing_fillers: alignment.trailing_theirs,
            ..PaneDecor::default()
        };
        let mut result = PaneDecor::default();
        let starts = line_starts(current);
        let selected = self.conflict_chunk();
        for (index, chunk) in self.chunks.iter().enumerate() {
            let color = chunk_color(chunk.kind, selected == Some(index), theme);
            for line in chunk.yours_lines.clone() {
                yours.lines.push((line, color));
            }
            for line in chunk.theirs_lines.clone() {
                theirs.lines.push((line, color));
            }
            if let Some(range) = ranges.get(index) {
                let lines = lines_of_range(&starts, current, range);
                for line in lines {
                    result.lines.push((line, color));
                }
            }
        }
        (yours, theirs, result)
    }

    /// The strip beside one side: a band for every chunk and the buttons that take its
    /// lines into the result or put the base text back.
    fn strip(
        &self,
        ours: bool,
        alignment: &Alignment,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let editor = if ours { &self.yours } else { &self.theirs };
        let line_height = editor.read(cx).line_height().max(1.);
        let top = editor.read(cx).scroll_top();
        let selected = self.conflict_chunk();
        let mut strip = div()
            .relative()
            .flex_none()
            .w(px(STRIP_WIDTH))
            .h_full()
            .overflow_hidden();
        for (index, chunk) in self.chunks.iter().enumerate() {
            let Some((yours_row, theirs_row)) = alignment.chunks.get(index) else {
                continue;
            };
            let row = if ours { *yours_row } else { *theirs_row };
            let y = row as f32 * line_height - top;
            let rows = chunk.yours_lines.len().max(chunk.theirs_lines.len()).max(1);
            let height = rows as f32 * line_height;
            let mut actions = div()
                .absolute()
                .left(px(SPACE_XS))
                .top(px(y))
                .flex()
                .items_center()
                .gap(px(SPACE_XS));
            let changes = match chunk.kind {
                SegmentKind::Conflict | SegmentKind::Both => true,
                SegmentKind::Yours => ours,
                SegmentKind::Theirs => !ours,
                SegmentKind::Unchanged => false,
            };
            if changes {
                let accept = if ours {
                    strip_button(
                        ("merge-accept-yours", index),
                        format!("merge-accept-yours-{index}"),
                        ">>",
                        "Take our lines into the result",
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.take(index, Some(ConflictSide::Yours), cx)
                    }))
                } else {
                    strip_button(
                        ("merge-accept-theirs", index),
                        format!("merge-accept-theirs-{index}"),
                        "<<",
                        "Take their lines into the result",
                        theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.take(index, Some(ConflictSide::Theirs), cx)
                    }))
                };
                actions = actions.child(accept);
            }
            let reject = if ours {
                strip_button(
                    ("merge-reject-yours", index),
                    format!("merge-reject-yours-{index}"),
                    "\u{00d7}",
                    "Put the base lines back",
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.take(index, None, cx)))
            } else {
                strip_button(
                    ("merge-reject-theirs", index),
                    format!("merge-reject-theirs-{index}"),
                    "\u{00d7}",
                    "Put the base lines back",
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.take(index, None, cx)))
            };
            actions = actions.child(reject);
            strip = strip.child(
                div()
                    .absolute()
                    .left(px((STRIP_WIDTH - 4.) / 2.))
                    .top(px(y))
                    .w(px(4.))
                    .h(px(height.max(line_height)))
                    .rounded(px(CONTROL_RADIUS))
                    .bg(chunk_kind_color(chunk.kind, theme))
                    .opacity(if selected == Some(index) { 0.95 } else { 0.6 }),
            );
            strip = strip.child(actions);
        }
        strip
    }

    /// The chunk the keys work on, the first conflict until the user moves.
    fn conflict_chunk(&self) -> Option<usize> {
        let conflicts: Vec<usize> = self
            .chunks
            .iter()
            .enumerate()
            .filter(|(_, chunk)| chunk.kind == SegmentKind::Conflict)
            .map(|(index, _)| index)
            .collect();
        if conflicts.is_empty() {
            return None;
        }
        Some(conflicts[self.selected.min(conflicts.len() - 1)])
    }

    fn next_conflict(&mut self, cx: &mut Context<Self>) {
        let count = self
            .chunks
            .iter()
            .filter(|chunk| chunk.kind == SegmentKind::Conflict)
            .count();
        if count == 0 {
            return;
        }
        self.selected = (self.selected + 1).min(count - 1);
        self.scroll_to_selected(cx);
        cx.notify();
    }

    fn previous_conflict(&mut self, cx: &mut Context<Self>) {
        self.selected = self.selected.saturating_sub(1);
        self.scroll_to_selected(cx);
        cx.notify();
    }

    fn scroll_to_selected(&mut self, cx: &mut Context<Self>) {
        let Some(chunk) = self.conflict_chunk() else {
            return;
        };
        let alignment = self.alignment();
        let Some((yours_row, theirs_row)) = alignment.chunks.get(chunk).copied() else {
            return;
        };
        let ranges = self.chunk_ranges(cx);
        let current = self.result.read(cx).text().unwrap_or_default();
        let starts = line_starts(&current);
        let result_row = ranges
            .get(chunk)
            .map(|range| lines_of_range(&starts, &current, range).start)
            .unwrap_or_default();
        let heights = [
            self.yours.read(cx).line_height(),
            self.result.read(cx).line_height(),
            self.theirs.read(cx).line_height(),
        ];
        let tops = [
            ((yours_row as f32 - 2.).max(0.)) * heights[0],
            ((result_row as f32 - 2.).max(0.)) * heights[1],
            ((theirs_row as f32 - 2.).max(0.)) * heights[2],
        ];
        self.syncing = true;
        for (editor, top) in [&self.yours, &self.result, &self.theirs]
            .into_iter()
            .zip(tops)
        {
            editor.update(cx, |editor, cx| editor.scroll_to_top(top, cx));
        }
        self.syncing = false;
    }

    /// Takes the lines of one side into a chunk, or puts its base lines back.
    fn take(&mut self, index: usize, side: Option<ConflictSide>, cx: &mut Context<Self>) {
        let Some(chunk) = self.chunks.get(index) else {
            return;
        };
        let text = match side {
            Some(ConflictSide::Yours) => chunk.yours.clone(),
            Some(ConflictSide::Theirs) => chunk.theirs.clone(),
            None => chunk.base.clone(),
        };
        let ranges = self.chunk_ranges(cx);
        let Some(range) = ranges.get(index).cloned() else {
            return;
        };
        self.result
            .update(cx, |result, cx| result.replace_text(range, &text, cx));
        self.display = None;
        cx.notify();
    }

    /// Accept Yours: the whole result becomes our version.
    fn take_all(&mut self, side: ConflictSide, cx: &mut Context<Self>) {
        let text = match side {
            ConflictSide::Yours => self.yours_text.clone(),
            ConflictSide::Theirs => self.theirs_text.clone(),
        };
        self.replace_result(&text, cx);
    }

    /// Apply non-conflicting changes: the side's chunks that do not disagree come into the
    /// result, a conflict stays as the user left it. `None` takes both sides.
    fn apply_non_conflicting(&mut self, side: Option<ConflictSide>, cx: &mut Context<Self>) {
        let ranges = self.chunk_ranges(cx);
        let current = self.result.read(cx).text().unwrap_or_default();
        let mut out = String::with_capacity(current.len());
        let mut at = 0usize;
        for (index, chunk) in self.chunks.iter().enumerate() {
            let Some(range) = ranges.get(index) else {
                continue;
            };
            let (from, to) = (range.start.max(at), range.end.max(at));
            if from > to || to > current.len() {
                continue;
            }
            out.push_str(&current[at..from]);
            let apply = match (side, chunk.kind) {
                (_, SegmentKind::Conflict | SegmentKind::Unchanged) => false,
                (None, _) => true,
                (Some(ConflictSide::Yours), SegmentKind::Yours | SegmentKind::Both) => true,
                (Some(ConflictSide::Theirs), SegmentKind::Theirs | SegmentKind::Both) => true,
                (Some(_), _) => false,
            };
            if apply {
                match chunk.kind {
                    SegmentKind::Yours | SegmentKind::Both => out.push_str(&chunk.yours),
                    SegmentKind::Theirs => out.push_str(&chunk.theirs),
                    _ => out.push_str(&current[from..to]),
                }
            } else {
                out.push_str(&current[from..to]);
            }
            at = to;
        }
        if at < current.len() {
            out.push_str(&current[at..]);
        }
        self.replace_result(&out, cx);
    }

    fn replace_result(&mut self, text: &str, cx: &mut Context<Self>) {
        let length = self.result.read(cx).text().map_or(0, |text| text.len());
        self.result
            .update(cx, |result, cx| result.replace_text(0..length, text, cx));
        self.display = None;
        cx.notify();
    }

    /// How many conflicts still stand at the base text in the result.
    fn unresolved(&mut self, cx: &mut Context<Self>) -> usize {
        let ranges = self.chunk_ranges(cx);
        let current = self.result.read(cx).text().unwrap_or_default();
        self.chunks
            .iter()
            .enumerate()
            .filter(|(_, chunk)| chunk.kind == SegmentKind::Conflict)
            .filter(|(index, chunk)| {
                ranges
                    .get(*index)
                    .and_then(|range| current.get(range.clone()))
                    == Some(chunk.base.as_str())
            })
            .count()
    }

    fn edited(&self, cx: &Context<Self>) -> bool {
        let current = self.result.read(cx).text().unwrap_or_default();
        current != self.initial_result
    }

    /// Apply: the result goes to the file and then to the index, but only when the file
    /// did not move under the window and no tab of it holds unsaved edits.
    fn apply(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy {
            return;
        }
        let unresolved = self.unresolved(cx);
        if unresolved > 0 {
            self.question = Some(Question::ApplyAnyway(unresolved));
            cx.notify();
            return;
        }
        self.write(window, cx);
    }

    fn write(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_editor_dirty(cx) {
            self.git.update(cx, |git, cx| {
                git.report(
                    "Merge",
                    format!(
                        "{}: the editor holds unsaved changes; save it and apply again",
                        self.path.display()
                    ),
                    cx,
                )
            });
            return;
        }
        let Some(repo) = self.git.read(cx).repo() else {
            return;
        };
        let path = self.path.clone();
        let relative = self.relative.clone();
        let format = self.format.clone();
        let opened = self.opened.clone();
        let text = self.result.read(cx).text().unwrap_or_default();
        let removed = self.missing.is_some() && text.is_empty();
        self.busy = true;
        cx.notify();
        let job = cx.background_executor().spawn(async move {
            if std::fs::read(&path).ok() != opened {
                return Outcome::Changed;
            }
            let bytes = (!removed).then(|| text_core::encode(&text, &format));
            let written = if removed {
                if path.exists() {
                    std::fs::remove_file(&path)
                } else {
                    Ok(())
                }
            } else {
                text_core::write_file(&path, &text, &format)
            };
            if let Err(error) = written {
                return Outcome::Failed(format!("{}: {error}", path.display()));
            }
            match repo.mark_resolved(&relative) {
                Ok(()) => Outcome::Done(bytes),
                Err(error) => Outcome::Unstaged {
                    bytes,
                    message: error.to_string(),
                },
            }
        });
        let weak = cx.weak_entity();
        window
            .spawn(cx, async move |cx| {
                let outcome = job.await;
                cx.update(|window, cx| {
                    weak.update(cx, |this, cx| this.applied(outcome, window, cx))
                        .ok();
                })
                .ok();
            })
            .detach();
    }

    fn applied(&mut self, outcome: Outcome, window: &mut Window, cx: &mut Context<Self>) {
        self.busy = false;
        match outcome {
            Outcome::Changed => {
                self.git.update(cx, |git, cx| {
                    git.report(
                        "Merge",
                        "The file changed on disk; close this window and open Merge again"
                            .to_owned(),
                        cx,
                    )
                });
            }
            Outcome::Done(bytes) => {
                self.opened = bytes;
                self.git.update(cx, |git, cx| {
                    git.reload_everything_from_disk(cx);
                    git.refresh(cx);
                });
                self.close(window, cx);
                return;
            }
            Outcome::Unstaged { bytes, message } => {
                // The file is the result now; a second Apply only stages it again.
                self.opened = bytes;
                self.git.update(cx, |git, cx| {
                    git.report("Merge", format!("git add failed: {message}"), cx)
                });
            }
            Outcome::Failed(message) => {
                self.git
                    .update(cx, |git, cx| git.report("Merge", message, cx));
            }
        }
        cx.notify();
    }

    fn file_editor_dirty(&self, cx: &App) -> bool {
        self.workspace
            .read_with(cx, |workspace, cx| {
                workspace
                    .open_editors()
                    .into_iter()
                    .find(|(path, _)| path == &self.path)
                    .and_then(|(_, view)| view.downcast::<EditorView>().ok())
                    .is_some_and(|view| view.read(cx).is_dirty())
            })
            .unwrap_or(false)
    }

    fn cancel(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edited(cx) {
            self.question = Some(Question::Discard);
            cx.notify();
        } else {
            self.close(window, cx);
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// The keys of the window and of its editors: F7 and Shift+F7 walk the conflicts, and
    /// Escape closes a question before it closes the window.
    fn merge_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        match (key, modifiers.control, modifiers.alt, modifiers.shift) {
            ("f7", false, false, false) => self.next_conflict(cx),
            ("f7", false, false, true) => self.previous_conflict(cx),
            ("escape", false, false, false) => {
                if self.question.take().is_some() {
                    cx.notify();
                } else {
                    self.cancel(window, cx);
                }
            }
            _ => return false,
        }
        true
    }

    fn button(
        id: (&'static str, usize),
        label: &'static str,
        primary: bool,
        theme: &Theme,
    ) -> Stateful<Div> {
        let (hover, pressed) = (theme.hover, theme.pressed);
        div()
            .id(id)
            .flex()
            .items_center()
            .justify_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if primary {
                theme.accent
            } else {
                theme.popup_border
            })
            .when(primary, |this| {
                this.bg(theme.accent).text_color(theme.over_accent)
            })
            .when(!primary, |this| {
                this.hover(move |style| style.bg(hover))
                    .active(move |style| style.bg(pressed))
            })
            .child(label)
    }

    fn question_band(&self, question: Question, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let text = match question {
            Question::Discard => "Discard changes?".to_owned(),
            Question::ApplyAnyway(count) => {
                format!("{count} conflicts are not resolved. Apply anyway?")
            }
        };
        let (first_id, first_label, second_id, second_label) = match question {
            Question::Discard => ("merge-discard", "Discard", "merge-keep", "Keep"),
            Question::ApplyAnyway(_) => (
                "merge-apply-anyway",
                "Apply Anyway",
                "merge-apply-back",
                "Back",
            ),
        };
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .gap(px(SPACE_M))
            .px(px(DIALOG_PADDING))
            .py(px(SPACE_M))
            .bg(theme.hover)
            .child(div().text_color(theme.text).child(text))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(
                        Self::button((first_id, 0), first_label, true, theme)
                            .debug_selector(move || first_id.to_owned())
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.question = None;
                                match question {
                                    Question::Discard => this.close(window, cx),
                                    Question::ApplyAnyway(_) => this.write(window, cx),
                                }
                            })),
                    )
                    .child(
                        Self::button((second_id, 0), second_label, false, theme)
                            .debug_selector(move || second_id.to_owned())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.question = None;
                                cx.notify();
                            })),
                    ),
            )
    }

    /// The index of the selected conflict among all conflicts, from zero.
    #[cfg(test)]
    pub(crate) fn selected(&self) -> usize {
        self.selected
    }

    /// The result editor, for tests of typing and of the chunk cache.
    #[cfg(test)]
    pub(crate) fn result_editor(&self) -> Entity<EditorView> {
        self.result.clone()
    }

    /// Whether a write is on its way, for tests.
    #[cfg(test)]
    pub(crate) fn busy(&self) -> bool {
        self.busy
    }
}

/// The three texts split into chunks, keyed by the lines of the first result.
fn chunks_of(three: &ThreeWay, base: &str, yours: &str, theirs: &str) -> Vec<Chunk> {
    let base_lines: Vec<&str> = base.split_inclusive('\n').collect();
    let yours_lines: Vec<&str> = yours.split_inclusive('\n').collect();
    let theirs_lines: Vec<&str> = theirs.split_inclusive('\n').collect();
    let block = |lines: &[&str], range: &Range<usize>| {
        lines
            .get(range.clone())
            .map(|lines| lines.concat())
            .unwrap_or_default()
    };
    let mut result_line = 0usize;
    let mut chunks = Vec::new();
    for segment in &three.segments {
        let result_lines = match segment.kind {
            SegmentKind::Unchanged | SegmentKind::Conflict => segment.base.len(),
            SegmentKind::Yours | SegmentKind::Both => segment.yours.len(),
            SegmentKind::Theirs => segment.theirs.len(),
        };
        if segment.kind != SegmentKind::Unchanged {
            chunks.push(Chunk {
                kind: segment.kind,
                initial: result_line..result_line + result_lines,
                yours_lines: segment.yours.clone(),
                theirs_lines: segment.theirs.clone(),
                base: block(&base_lines, &segment.base),
                yours: block(&yours_lines, &segment.yours),
                theirs: block(&theirs_lines, &segment.theirs),
            });
        }
        result_line += result_lines;
    }
    chunks
}

/// The background read of a conflicted file: the three index stages, decoded to text, and
/// the working file with its format.
fn read_snapshot(repo: &vcs::Repository, path: &Path) -> std::io::Result<Snapshot> {
    let relative = relative_of(repo.root(), path);
    let mut blobs = repo.blobs()?;
    let versions = repo.conflict_versions(&mut blobs, &relative)?;
    let missing = match (versions.yours.is_none(), versions.theirs.is_none()) {
        (true, false) => Some(ConflictSide::Yours),
        (false, true) => Some(ConflictSide::Theirs),
        _ => None,
    };
    let decoded = |bytes: Option<Vec<u8>>| text_core::decode(bytes.as_deref().unwrap_or(b""));
    let base = decoded(versions.base);
    let yours = decoded(versions.yours);
    let theirs = decoded(versions.theirs);
    let binary = [&base, &yours, &theirs]
        .into_iter()
        .any(|decoded| !decoded.lossless);
    if binary {
        return Err(std::io::Error::other(
            "Merge is not available for binary files",
        ));
    }
    let opened = std::fs::read(path).ok();
    let worktree = opened.as_ref().map(|bytes| text_core::decode(bytes));
    if worktree.as_ref().is_some_and(|decoded| !decoded.lossless) {
        return Err(std::io::Error::other(
            "Merge is not available for binary files",
        ));
    }
    let format = worktree
        .as_ref()
        .map(|decoded| decoded.format.clone())
        .or_else(|| {
            // The file is gone from the working tree: the surviving side keeps its format.
            match missing {
                Some(ConflictSide::Yours) => Some(theirs.format.clone()),
                _ => Some(yours.format.clone()),
            }
        })
        .unwrap_or_default();
    Ok(Snapshot {
        relative,
        base: base.text,
        yours: yours.text,
        theirs: theirs.text,
        missing,
        format,
        opened,
    })
}

/// A button of a strip that acts on one chunk.
fn strip_button(
    id: (&'static str, usize),
    selector: String,
    label: &'static str,
    hint: &'static str,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || selector.clone())
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
        .tooltip(move |_, cx| tooltip(hint, None, cx))
        .child(label)
}

/// The colour of a chunk in the strips and in the panes.
fn chunk_kind_color(kind: SegmentKind, theme: &Theme) -> Hsla {
    match kind {
        SegmentKind::Conflict => Kind::Conflicted.color(theme.dark),
        SegmentKind::Yours => Kind::Modified.color(theme.dark),
        SegmentKind::Theirs | SegmentKind::Both => Kind::Added.color(theme.dark),
        SegmentKind::Unchanged => theme.text_secondary,
    }
}

fn chunk_color(kind: SegmentKind, selected: bool, theme: &Theme) -> Hsla {
    let color = chunk_kind_color(kind, theme);
    if selected {
        color.opacity(0.34)
    } else {
        color.opacity(0.18)
    }
}

/// How many rows a chunk takes in the strip of one side.
fn lines_of_range(starts: &[usize], text: &str, range: &Range<usize>) -> Range<usize> {
    let line_at = |offset: usize| {
        starts
            .partition_point(|start| *start <= offset)
            .saturating_sub(1)
    };
    if range.is_empty() {
        let line = line_at(range.start.min(text.len()));
        return line..line;
    }
    let start = line_at(range.start.min(text.len()));
    let end = line_at((range.end.saturating_sub(1)).min(text.len())) + 1;
    start..end
}

/// Where a line range of the first result sits in the current one: the hunks before it
/// shift it whole, a hunk that covers it maps it to the lines that replaced those.
fn map_lines(hunks: &[Hunk], lines: Range<usize>) -> Range<usize> {
    let map = |line: usize, end: bool| {
        let mut delta = 0isize;
        for hunk in hunks {
            let before = hunk.before.start as usize..hunk.before.end as usize;
            if line >= before.end {
                delta += (hunk.after.end - hunk.after.start) as isize
                    - (hunk.before.end - hunk.before.start) as isize;
                continue;
            }
            if line <= before.start {
                break;
            }
            let at = if end {
                hunk.after.end
            } else {
                hunk.after.start
            };
            return (at as isize + delta).max(0) as usize;
        }
        (line as isize + delta).max(0) as usize
    };
    map(lines.start, false)..map(lines.end, true)
}

impl Render for MergeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH).min(viewport.width - px(40.));
        let height = viewport.height * 0.8;
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.1;
        let ranges = self.drawn_ranges(cx);
        let current = self.result.read(cx).text().unwrap_or_default();
        let alignment = self.alignment();
        let (yours_decor, theirs_decor, result_decor) =
            self.decorations(&ranges, &current, &theme, &alignment);
        let yours_pane = self.yours.update(cx, |editor, cx| {
            editor.pane(yours_decor, Some(EDITOR_CONTEXT), window, cx)
        });
        let result_pane = self.result.update(cx, |editor, cx| {
            editor.pane(result_decor, Some(EDITOR_CONTEXT), window, cx)
        });
        let theirs_pane = self.theirs.update(cx, |editor, cx| {
            editor.pane(theirs_decor, Some(EDITOR_CONTEXT), window, cx)
        });
        let yours_strip = self.strip(true, &alignment, &theme, cx);
        let theirs_strip = self.strip(false, &alignment, &theme, cx);
        let conflicts = self
            .chunks
            .iter()
            .filter(|chunk| chunk.kind == SegmentKind::Conflict)
            .count();
        let counter = if conflicts > 0 {
            format!(
                "Conflict {} of {conflicts}",
                self.selected.min(conflicts - 1) + 1
            )
        } else {
            "No conflicts".to_owned()
        };
        let title = format!("Merge {}", self.relative);
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .gap(px(SPACE_M))
            .h(px(40.))
            .px(px(DIALOG_PADDING))
            .font_weight(FontWeight::MEDIUM)
            .child(div().flex_1().min_w_0().truncate().child(title))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .child(div().text_color(theme.text_secondary).child(counter))
                    .child(
                        Self::button(("merge-previous", 0), "Previous", false, &theme)
                            .debug_selector(|| "merge-previous".to_owned())
                            .on_click(cx.listener(|this, _, _, cx| this.previous_conflict(cx))),
                    )
                    .child(
                        Self::button(("merge-next", 0), "Next", false, &theme)
                            .debug_selector(|| "merge-next".to_owned())
                            .on_click(cx.listener(|this, _, _, cx| this.next_conflict(cx))),
                    )
                    .child(
                        Self::button(("merge-accept-all-yours", 0), "Accept Yours", false, &theme)
                            .debug_selector(|| "merge-accept-all-yours".to_owned())
                            .on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.take_all(ConflictSide::Yours, cx)
                                }),
                            ),
                    )
                    .child(
                        Self::button(
                            ("merge-accept-all-theirs", 0),
                            "Accept Theirs",
                            false,
                            &theme,
                        )
                        .debug_selector(|| "merge-accept-all-theirs".to_owned())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.take_all(ConflictSide::Theirs, cx)),
                        ),
                    )
                    .child(
                        Self::button(
                            ("merge-apply-non-conflicting-all", 0),
                            "Apply Non-Conflicting: Both",
                            false,
                            &theme,
                        )
                        .debug_selector(|| "merge-apply-non-conflicting-all".to_owned())
                        .on_click(
                            cx.listener(|this, _, _, cx| this.apply_non_conflicting(None, cx)),
                        ),
                    )
                    .child(
                        Self::button(
                            ("merge-apply-non-conflicting-left", 0),
                            "Left",
                            false,
                            &theme,
                        )
                        .debug_selector(|| "merge-apply-non-conflicting-left".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.apply_non_conflicting(Some(ConflictSide::Yours), cx)
                        })),
                    )
                    .child(
                        Self::button(
                            ("merge-apply-non-conflicting-right", 0),
                            "Right",
                            false,
                            &theme,
                        )
                        .debug_selector(|| "merge-apply-non-conflicting-right".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.apply_non_conflicting(Some(ConflictSide::Theirs), cx)
                        })),
                    )
                    .child(
                        Self::button(("merge-apply", 0), "Apply", true, &theme)
                            .debug_selector(|| "merge-apply".to_owned())
                            .on_click(cx.listener(|this, _, window, cx| this.apply(window, cx))),
                    )
                    .child(
                        Self::button(("merge-cancel", 0), "Cancel", false, &theme)
                            .debug_selector(|| "merge-cancel".to_owned())
                            .on_click(cx.listener(|this, _, window, cx| this.cancel(window, cx))),
                    ),
            );
        let titles = div()
            .flex()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .text_color(theme.text_secondary)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(SPACE_M))
                    .truncate()
                    .child("Yours"),
            )
            .child(div().w(px(STRIP_WIDTH)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(SPACE_M))
                    .truncate()
                    .child("Result"),
            )
            .child(div().w(px(STRIP_WIDTH)))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .px(px(SPACE_M))
                    .truncate()
                    .child("Theirs"),
            );
        let question = self
            .question
            .map(|question| self.question_band(question, &theme, cx));
        let body = surface(&theme)
            .id("merge")
            .track_focus(&self.focus)
            .key_context(CONTEXT)
            .on_action(cx.listener(|this, _: &NextConflict, _, cx| this.next_conflict(cx)))
            .on_action(cx.listener(|this, _: &PreviousConflict, _, cx| this.previous_conflict(cx)))
            .occlude()
            .w(width)
            .h(height)
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.cancel(window, cx)))
            .child(header)
            .children(question)
            .child(titles)
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                        if this.merge_key(event, window, cx) {
                            cx.stop_propagation();
                        }
                    }))
                    .child(
                        div()
                            .debug_selector(|| "merge-yours".to_owned())
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(yours_pane),
                    )
                    .child(yours_strip)
                    .child(
                        div()
                            .debug_selector(|| "merge-result".to_owned())
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(result_pane),
                    )
                    .child(theirs_strip)
                    .child(
                        div()
                            .debug_selector(|| "merge-theirs".to_owned())
                            .flex_1()
                            .min_w_0()
                            .h_full()
                            .child(theirs_pane),
                    ),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("merge-enter", body))
    }
}

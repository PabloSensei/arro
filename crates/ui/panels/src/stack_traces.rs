//! The Stack Trace tool window: the lines of a pasted stack trace, or the threads of a
//! dump. A frame of the project opens its file; the frames of libraries and of the
//! runtime open what the language has for them and stay grey.

use std::collections::HashMap;

use editor_view::{Semantic, Spot};
use futures_util::StreamExt;
use lang_api::{FrameOrigin, LineKind, Report, StackLine, ThreadStack, TraceQuery, ask};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{STACK_TRACE_TOOL, Workspace};

use crate::navigation;
use crate::prompt::TextField;

const GROUP: &str = "bottom-panel";
const ROW_MARGIN: f32 = SPACE_L;
/// The column of the threads of a dump.
const THREADS_WIDTH: f32 = 280.;

/// The panel of the whole window: Analyze Stack Trace and Dump Threads fill it.
pub struct StackTracePanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    report: Option<Report>,
    /// The threads of a dump in the order the list shows them.
    threads: Vec<ThreadStack>,
    /// The threads the filter keeps, as indices into `threads`.
    visible: Vec<usize>,
    /// The thread whose stack the right column shows.
    selected: Option<usize>,
    filter: TextField,
    trace_scroll: ScrollHandle,
    thread_scroll: ScrollHandle,
    line_scroll: ScrollHandle,
    /// Grows with every request, so the answer of an older one is dropped.
    generation: u64,
    /// A remark of the language under the lines: the frames it could not place, for one.
    note: Option<String>,
}

/// The panel of the window, for the commands that fill it.
pub struct ActiveStackTrace(pub Entity<StackTracePanel>);

impl Global for ActiveStackTrace {}

impl StackTracePanel {
    pub fn create(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        cx.new(|cx| Self {
            workspace,
            focus: cx.focus_handle(),
            report: None,
            threads: Vec::new(),
            visible: Vec::new(),
            selected: None,
            filter: TextField::default(),
            trace_scroll: ScrollHandle::new(),
            thread_scroll: ScrollHandle::new(),
            line_scroll: ScrollHandle::new(),
            generation: 0,
            note: None,
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Reads the text through the language, which finds the place of every frame. The
    /// language answers twice: the text and the project frames first, the places of the
    /// libraries after them.
    pub fn analyze(&mut self, text: String, cx: &mut Context<Self>) {
        let support = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone());
        let Some(provider) = support.as_ref().and_then(|support| support.stack_traces()) else {
            self.show(
                Report::Trace {
                    lines: vec![StackLine::text("The language cannot read stack traces")],
                },
                cx,
            );
            return;
        };
        self.generation = self.generation.wrapping_add(1);
        let generation = self.generation;
        let mut replies = ask(provider, TraceQuery(text));
        cx.spawn(async move |this, cx| {
            let mut first = true;
            while let Some(answer) = replies.next().await {
                let note = answer.note.clone();
                let current = this
                    .update(cx, |this, cx| {
                        if this.generation != generation {
                            return false;
                        }
                        if first {
                            first = false;
                            this.show(answer.value, cx);
                        } else {
                            this.refine(answer.value, cx);
                        }
                        this.set_note(note, cx);
                        true
                    })
                    .unwrap_or(false);
                if !current {
                    break;
                }
            }
        })
        .detach();
    }

    /// A remark of the language under the lines, in words for the user. `None` takes the
    /// remark away.
    pub fn set_note(&mut self, note: Option<String>, cx: &mut Context<Self>) {
        if self.note == note {
            return;
        }
        self.note = note;
        cx.notify();
    }

    /// Shows a report the caller already read, such as the dump of a running program.
    pub fn show(&mut self, report: Report, cx: &mut Context<Self>) {
        self.threads.clear();
        self.visible.clear();
        self.selected = None;
        self.filter = TextField::default();
        self.note = None;
        if let Report::Dump { threads, .. } = &report {
            self.threads = threads.clone();
            // Blocked threads and threads of the project come first, as in IDEA.
            self.threads.sort_by(|a, b| {
                let rank = |thread: &ThreadStack| {
                    (thread.state != "BLOCKED", !thread.has_project_frames())
                };
                rank(a).cmp(&rank(b)).then_with(|| a.name.cmp(&b.name))
            });
        }
        self.report = Some(report);
        self.trace_scroll.scroll_to_item(0);
        self.thread_scroll.scroll_to_item(0);
        self.line_scroll.scroll_to_item(0);
        self.refresh(cx);
        cx.notify();
    }

    /// A later answer of the same request: the places of more frames fill in. The report
    /// the window shows keeps its shape, the chosen thread and the scroll position.
    pub fn refine(&mut self, report: Report, cx: &mut Context<Self>) {
        let same_shape = match (&self.report, &report) {
            (Some(Report::Trace { lines }), Report::Trace { lines: next }) => {
                lines.len() == next.len()
            }
            (Some(Report::Dump { threads, .. }), Report::Dump { threads: next, .. }) => {
                threads.len() == next.len()
            }
            _ => false,
        };
        if !same_shape {
            self.show(report, cx);
            return;
        }
        match (&mut self.report, report) {
            (Some(Report::Trace { lines }), Report::Trace { lines: next }) => *lines = next,
            (
                Some(Report::Dump { threads, deadlock }),
                Report::Dump {
                    threads: next,
                    deadlock: next_deadlock,
                },
            ) => {
                *deadlock = next_deadlock;
                let mut found: HashMap<String, ThreadStack> = next
                    .into_iter()
                    .map(|thread| (thread.name.clone(), thread))
                    .collect();
                for thread in self.threads.iter_mut() {
                    if let Some(found) = found.remove(&thread.name) {
                        *thread = found;
                    }
                }
                *threads = self.threads.clone();
            }
            _ => {}
        }
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let needle = self.filter.text.trim().to_lowercase();
        self.visible = self
            .threads
            .iter()
            .enumerate()
            .filter(|(_, thread)| needle.is_empty() || thread.name.to_lowercase().contains(&needle))
            .map(|(index, _)| index)
            .collect();
        if self
            .selected
            .is_none_or(|selected| !self.visible.contains(&selected))
        {
            self.selected = self.visible.first().copied();
        }
        cx.notify();
    }

    /// The lines of the trace, or of the selected thread.
    fn lines(&self) -> &[StackLine] {
        match &self.report {
            Some(Report::Trace { lines }) => lines,
            Some(Report::Dump { .. }) => self
                .selected
                .and_then(|index| self.threads.get(index))
                .map_or(&[], |thread| thread.lines.as_slice()),
            None => &[],
        }
    }

    fn deadlock(&self) -> bool {
        matches!(self.report, Some(Report::Dump { deadlock: true, .. }))
    }

    /// The thread whose stack is on the right, for the list of a dump.
    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        if !self.visible.contains(&index) {
            return;
        }
        self.selected = Some(index);
        self.line_scroll.scroll_to_item(0);
        cx.notify();
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.visible.is_empty() {
            return;
        }
        let at = self
            .selected
            .and_then(|selected| self.visible.iter().position(|index| *index == selected));
        let next = match at {
            Some(at) => (at as isize + delta).clamp(0, self.visible.len() as isize - 1) as usize,
            None if delta < 0 => self.visible.len() - 1,
            None => 0,
        };
        self.choose(self.visible[next], cx);
    }

    /// Opens the place of a frame, through the language when the text is not a file.
    fn open(&mut self, frame_index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(line) = self.lines().get(frame_index) else {
            return;
        };
        let Some(frame) = &line.frame else {
            return;
        };
        let Some(place) = frame.place.clone() else {
            return;
        };
        let extension = frame
            .file
            .as_deref()
            .and_then(|file| file.rsplit_once('.'))
            .map_or_else(String::new, |(_, extension)| extension.to_owned());
        let line_at = place.span.start.line as usize;
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        let missing = self.workspace.clone();
        let span = place.clone();
        editor_view::show_location(
            &span,
            &extension,
            cx,
            move |path, cx| {
                cx.defer(move |cx| {
                    handle
                        .update(cx, |_, window, cx| {
                            workspace
                                .update(cx, |workspace, cx| {
                                    let spot = Some(Spot::Line {
                                        line: line_at,
                                        column: 0,
                                    });
                                    navigation::go_to(workspace, path, spot, true, window, cx)
                                })
                                .ok();
                        })
                        .ok();
                });
            },
            move |cx| {
                cx.defer(move |cx| {
                    missing
                        .update(cx, |workspace, cx| {
                            workspace.notify_user("Stack Trace", "No source for this frame", cx)
                        })
                        .ok();
                });
            },
        );
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if !self.report.as_ref().is_some_and(Report::is_dump) {
            return;
        }
        if self.filter.key_with_clipboard(event, false, cx) {
            cx.stop_propagation();
            self.refresh(cx);
            return;
        }
        match event.keystroke.key.as_str() {
            "up" => {
                cx.stop_propagation();
                self.move_selection(-1, cx);
            }
            "down" => {
                cx.stop_propagation();
                self.move_selection(1, cx);
            }
            "enter" => {
                cx.stop_propagation();
                if let Some(index) = self.selected {
                    self.choose(index, cx);
                    window.focus(&self.focus, cx);
                }
            }
            _ => {}
        }
    }
}

fn state_icon(state: &str, deadlocked: bool) -> &'static str {
    if deadlocked {
        return "status/error";
    }
    match state {
        "BLOCKED" => "status/error",
        "WAITING" | "TIMED_WAITING" => "status/warning",
        "RUNNABLE" => "status/success",
        _ => "status/info",
    }
}

/// One line of a stack, a link when the language found its place.
fn stack_line(
    index: usize,
    line: &StackLine,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> AnyElement {
    let (color, linked) = match &line.frame {
        Some(frame) if frame.origin == FrameOrigin::Project => (theme.code, frame.place.is_some()),
        Some(frame) => (theme.text_muted, frame.place.is_some()),
        None if line.kind == LineKind::Structure => (theme.text_secondary, false),
        None => (theme.code, false),
    };
    let row = div()
        .id(("stack-line", index))
        .debug_selector(move || format!("stack-line-{index}"))
        .flex()
        .items_center()
        .flex_none()
        .min_h(px(CODE_LINE_HEIGHT))
        .px(px(ROW_MARGIN))
        .whitespace_nowrap()
        .text_color(color)
        .child(line.text.clone());
    let row = if linked {
        row.cursor_pointer()
            .hover(move |style| style.text_color(theme.link))
            .on_click(on_click)
    } else {
        row
    };
    row.into_any_element()
}

impl Render for StackTracePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let options =
            crate::tool_menu::options_menu(workspace.clone(), workspace::STACK_TRACE_TOOL, |_| {
                Vec::new()
            });
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(STACK_TRACE_TOOL, window, cx)
                })
                .ok();
        };
        let dump = self.report.as_ref().is_some_and(Report::is_dump);
        let empty = self.report.is_none();
        let deadlock = self.deadlock();
        let trace_lines: Vec<AnyElement> = if dump {
            Vec::new()
        } else {
            (0..self.lines().len())
                .map(|index| {
                    let line = &self.lines()[index];
                    stack_line(
                        index,
                        line,
                        &theme,
                        cx.listener(move |this, _, window, cx| this.open(index, window, cx)),
                    )
                })
                .collect()
        };
        let trace_body = scrollable(
            "stack-trace-lines",
            &self.trace_scroll,
            div()
                .id("stack-trace-lines")
                .flex()
                .flex_col()
                .font_family(MONO_FONT)
                .text_size(px(CODE_SIZE))
                .overflow_y_scroll()
                .track_scroll(&self.trace_scroll)
                .children(trace_lines)
                .when(empty, |this| {
                    this.px(px(ROW_MARGIN)).text_color(theme.text_secondary).child(
                        "Paste a stack trace with Analyze Stack Trace, or dump the threads of a running program",
                    )
                }),
            &theme,
            window,
            cx,
        )
        .flex_1()
        .min_h_0();
        let dump_body = dump.then(|| {
            let rows: Vec<AnyElement> = self
                .visible
                .iter()
                .map(|&index| {
                    let thread = &self.threads[index];
                    let state = if thread.deadlocked {
                        format!("{} (deadlock)", thread.state)
                    } else {
                        thread.state.clone()
                    };
                    let style = TreeRow {
                        id: ("stack-thread", index).into(),
                        depth: 0,
                        label: thread.name.clone().into(),
                        hint: Some(state.into()),
                        icon: state_icon(&thread.state, thread.deadlocked),
                        expandable: false,
                        open: false,
                        selected: self.selected == Some(index),
                    };
                    div()
                        .w_full()
                        .px(px(ROW_MARGIN))
                        .child(
                            tree_row(style, &theme)
                                .debug_selector(move || format!("stack-thread-{index}"))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    window.focus(&this.focus, cx);
                                    this.choose(index, cx);
                                })),
                        )
                        .into_any_element()
                })
                .collect();
            let list = scrollable(
                "stack-thread-list",
                &self.thread_scroll,
                div()
                    .id("stack-thread-list")
                    .flex()
                    .flex_col()
                    .overflow_y_scroll()
                    .track_scroll(&self.thread_scroll)
                    .children(rows),
                &theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0();
            let filter = div()
                .id("stack-thread-filter")
                .debug_selector(|| "stack-thread-filter".to_owned())
                .flex()
                .items_center()
                .flex_none()
                .h(px(CONTROL_HEIGHT))
                .mx(px(SPACE_M))
                .mb(px(SPACE_S))
                .px(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(theme.control_border)
                .overflow_hidden()
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
                )
                .children(text_field_children(
                    FieldMouse::with_edit(
                        cx,
                        |this| Some(&mut this.filter),
                        |this, cx| this.refresh(cx),
                    ),
                    &self.filter,
                    theme.text,
                    theme.text,
                    theme.selection,
                    theme.caret,
                    16.,
                    self.focus.is_focused(window),
                ))
                .when(self.filter.text.is_empty(), |this| {
                    this.text_color(theme.text_secondary)
                        .child("Filter threads")
                });
            let lines: Vec<AnyElement> = (0..self.lines().len())
                .map(|line| {
                    let shown = &self.lines()[line];
                    stack_line(
                        line,
                        shown,
                        &theme,
                        cx.listener(move |this, _, window, cx| this.open(line, window, cx)),
                    )
                })
                .collect();
            let stack = scrollable(
                "stack-thread-lines",
                &self.line_scroll,
                div()
                    .id("stack-thread-lines")
                    .flex()
                    .flex_col()
                    .font_family(MONO_FONT)
                    .text_size(px(CODE_SIZE))
                    .overflow_y_scroll()
                    .track_scroll(&self.line_scroll)
                    .children(lines),
                &theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0();
            div()
                .flex()
                .flex_1()
                .min_h_0()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_none()
                        .w(px(THREADS_WIDTH))
                        .h_full()
                        .child(filter)
                        .child(list),
                )
                .child(div().w(px(1.)).flex_none().h_full().bg(theme.border))
                .child(stack)
                .into_any_element()
        });
        let body = match dump_body {
            Some(body) => body,
            None => trace_body.into_any_element(),
        };
        island(&theme)
            .id("stack-trace-panel")
            .debug_selector(|| "stack-trace-panel".to_owned())
            .key_context("StackTracePanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(panel_header(
                "Stack Trace",
                GROUP,
                "stack-trace",
                options,
                hide,
                &theme,
            ))
            .when(deadlock, |this| {
                this.child(
                    div()
                        .flex_none()
                        .h(px(CONTROL_HEIGHT))
                        .px(px(ROW_MARGIN))
                        .flex()
                        .items_center()
                        .text_color(theme.stripe_error)
                        .child("Found a deadlock between the threads of the program"),
                )
            })
            .child(div().flex_1().min_h_0().child(body))
            .when_some(self.note.clone(), |this, note| {
                this.child(
                    div()
                        .id("stack-note")
                        .debug_selector(|| "stack-note".to_owned())
                        .flex_none()
                        .h(px(CONTROL_HEIGHT))
                        .px(px(ROW_MARGIN))
                        .flex()
                        .items_center()
                        .border_t_1()
                        .border_color(theme.border)
                        .text_color(theme.text_secondary)
                        .child(note),
                )
            })
    }
}

#[cfg(test)]
impl StackTracePanel {
    /// The threads the list shows, with their places in the report.
    pub(crate) fn visible_threads(&self) -> Vec<(usize, String)> {
        self.visible
            .iter()
            .map(|index| (*index, self.threads[*index].name.clone()))
            .collect()
    }

    /// The name of the thread whose stack is on the right.
    pub(crate) fn selected_thread(&self) -> Option<String> {
        self.selected
            .and_then(|index| self.threads.get(index))
            .map(|thread| thread.name.clone())
    }

    /// The lines of the trace, or of the selected thread.
    pub(crate) fn shown_lines(&self) -> Vec<String> {
        self.lines().iter().map(|line| line.text.clone()).collect()
    }

    /// How each shown line is coloured and whether it is a link.
    pub(crate) fn line_marks(&self) -> Vec<(FrameOrigin, bool)> {
        self.lines()
            .iter()
            .map(|line| {
                line.frame
                    .as_ref()
                    .map_or((FrameOrigin::Unknown, false), |frame| {
                        (frame.origin, frame.place.is_some())
                    })
            })
            .collect()
    }

    pub(crate) fn filter_text(&self) -> &str {
        &self.filter.text
    }

    /// The remark of the language under the lines.
    pub(crate) fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
}

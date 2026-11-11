//! Reformat Code and the Reformat File dialog. Ctrl+Alt+L formats with the flags the
//! dialog remembered; the dialog can also organize imports and format only the lines
//! changed since HEAD.

use std::ops::Range;
use std::path::PathBuf;

use ::platform::CanonPath;
use editor_view::{EditorView, Semantic};
use futures_util::StreamExt;
use lang_api::{
    ActionKind, ActionQuery, DocumentEdit, FormatQuery, RangeFormatQuery, SourceEdit, TextPoint,
    TextSpan, WorkspaceChange, ask, offset_in,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::git::ActiveGit;

const WIDTH: f32 = 460.;

pub(crate) fn bind_commands(workspace: &mut Workspace, _cx: &mut Context<Workspace>) {
    workspace.register_command(
        keymap::workspace::SHOW_REFORMAT_DIALOG,
        "Reformat File...",
        show_dialog,
    );
}

/// What Ctrl+Alt+L and the dialog format.
#[derive(Clone, Debug)]
enum Target {
    Whole,
    Selected(TextSpan),
    Changed(Vec<TextSpan>),
}

/// Ctrl+Alt+Shift+L: the dialog with the flags remembered from the last time.
pub(crate) fn show_dialog(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(path) = workspace.active_path() else {
        return;
    };
    let settings = cx
        .try_global::<workspace::Settings>()
        .cloned()
        .unwrap_or_default();
    let weak = cx.weak_entity();
    let view = cx.new(|cx| ReformatDialog {
        workspace: weak,
        focus: cx.focus_handle(),
        path,
        optimize: settings.reformat_optimize_imports,
        only_changed: settings.reformat_only_changed,
        selection: settings.reformat_selection,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

struct ReformatDialog {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    path: PathBuf,
    optimize: bool,
    only_changed: bool,
    selection: bool,
}

impl ReformatDialog {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (optimize, only_changed, selection) =
            (self.optimize, self.only_changed, self.selection);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.update_settings(
                    |settings| {
                        settings.reformat_optimize_imports = optimize;
                        settings.reformat_only_changed = only_changed;
                        settings.reformat_selection = selection;
                    },
                    cx,
                );
            })
            .ok();
        self.close(window, cx);
        let weak = self.workspace.clone();
        weak.update(cx, |workspace, cx| run(workspace, window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                self.accept(window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.close(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for ReformatDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let left = (window.viewport_size().width - px(WIDTH)) / 2.;
        let top = window.viewport_size().height * 0.2;
        let path = self.path.display().to_string();
        let check = |id: &'static str, on: bool, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .gap(px(SPACE_M))
                .h(px(ROW_HEIGHT))
                .px(px(ROW_PADDING_X))
                .rounded(px(ROW_RADIUS))
                .hover(|style| style.bg(theme.selection_hover))
                .child(checkbox(on, &theme))
                .child(label)
        };
        let radio = |id: &'static str, on: bool, label: &'static str| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .gap(px(SPACE_M))
                .h(px(ROW_HEIGHT))
                .px(px(ROW_PADDING_X))
                .rounded(px(ROW_RADIUS))
                .hover(|style| style.bg(theme.selection_hover))
                .child(if on { "●" } else { "○" })
                .child(label)
        };
        let body = surface(&theme)
            .id("reformat-dialog")
            .debug_selector(|| "reformat-dialog".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .debug_selector(|| "reformat-title".to_owned())
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .text_size(px(TEXT_TITLE))
                            .font_weight(FontWeight::MEDIUM)
                            .child("Reformat File"),
                    )
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(path)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        check("reformat-optimize", self.optimize, "Optimize imports").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.optimize = !this.optimize;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        check(
                            "reformat-only-changed",
                            self.only_changed,
                            "Only VCS changed text",
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.only_changed = !this.only_changed;
                            cx.notify();
                        })),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_XS))
                    .child(div().text_color(theme.text_secondary).child("Scope"))
                    .child(
                        radio("reformat-selected", self.selection, "Selected text").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.selection = true;
                                cx.notify();
                            }),
                        ),
                    )
                    .child(
                        radio("reformat-whole-file", !self.selection, "Whole file").on_click(
                            cx.listener(|this, _, _, cx| {
                                this.selection = false;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .id("reformat-cancel")
                            .debug_selector(|| "reformat-cancel".to_owned())
                            .px(px(SPACE_L))
                            .h(px(CONTROL_HEIGHT))
                            .flex()
                            .items_center()
                            .rounded(px(CONTROL_RADIUS))
                            .hover(|style| style.bg(theme.selection_hover))
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        div()
                            .id("reformat-run")
                            .debug_selector(|| "reformat-run".to_owned())
                            .px(px(SPACE_L))
                            .h(px(CONTROL_HEIGHT))
                            .flex()
                            .items_center()
                            .rounded(px(CONTROL_RADIUS))
                            .bg(theme.accent)
                            .text_color(theme.over_accent)
                            .child("Run")
                            .on_click(cx.listener(|this, _, window, cx| this.accept(window, cx))),
                    ),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("reformat-enter", body))
    }
}

/// Ctrl+Alt+L and the dialog: formats the active editor with the remembered flags.
pub(crate) fn run(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<EditorView>().ok())
    else {
        return;
    };
    let path = view.read(cx).path().to_path_buf();
    let settings = cx
        .try_global::<workspace::Settings>()
        .cloned()
        .unwrap_or_default();
    let optimize = settings.reformat_optimize_imports;
    let selection = if settings.reformat_selection {
        view.read(cx).selection().map(|(range, _)| range)
    } else {
        None
    };
    let text = view.read(cx).text().unwrap_or_default();
    if settings.reformat_only_changed {
        only_changed(workspace, view, path, text, optimize, window, cx);
        return;
    }
    let target = match selection.filter(|range| !range.is_empty()) {
        Some(range) => Target::Selected(span_of(text.as_str(), range)),
        None => Target::Whole,
    };
    start(workspace, view, path, target, optimize, window, cx);
}

/// Reads the hunks against HEAD off the UI thread, then formats each of them.
fn only_changed(
    workspace: &mut Workspace,
    view: Entity<EditorView>,
    path: PathBuf,
    text: String,
    optimize: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let repo = cx
        .try_global::<ActiveGit>()
        .and_then(|git| git.0.read(cx).repo());
    let Some(repo) = repo else {
        workspace.notify_user("Reformat", "The project is not in a repository", cx);
        return;
    };
    let relative = path
        .strip_prefix(repo.root())
        .unwrap_or(&path)
        .to_string_lossy()
        .replace('\\', "/");
    let task = cx.background_executor().spawn(async move {
        let mut blobs = repo.blobs().ok()?;
        let diff = repo.file_diff(&mut blobs, &relative, &text).ok()?;
        Some(
            diff.hunks
                .iter()
                .map(|hunk| lines_span(&text, hunk.after.clone()))
                .collect::<Vec<_>>(),
        )
    });
    cx.spawn_in(window, async move |workspace, cx| {
        let ranges = task.await.unwrap_or_default();
        workspace
            .update_in(cx, |workspace, window, cx| {
                if ranges.is_empty() {
                    workspace.notify_user("Reformat", "No changes since HEAD", cx);
                    return;
                }
                start(
                    workspace,
                    view,
                    path,
                    Target::Changed(ranges),
                    optimize,
                    window,
                    cx,
                );
            })
            .ok();
    })
    .detach();
}

fn start(
    _workspace: &mut Workspace,
    view: Entity<EditorView>,
    path: PathBuf,
    target: Target,
    optimize: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(support) = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone())
    else {
        return;
    };
    let (uri, version, text, tab_size, insert_spaces) = {
        let view = view.read(cx);
        let Some(version) = view.version() else {
            return;
        };
        let Some(text) = view.text() else {
            return;
        };
        let Ok(uri) = CanonPath::new(&path) else {
            return;
        };
        let (tab_size, insert_spaces) = view.formatting_options();
        (uri.to_uri(), version, text, tab_size, insert_spaces)
    };
    let ranges = match &target {
        Target::Whole => Vec::new(),
        Target::Selected(span) => vec![*span],
        Target::Changed(spans) => spans.clone(),
    };
    let mut format_replies = Vec::new();
    if let (false, Some(provider)) = (ranges.is_empty(), support.range_format()) {
        for range in &ranges {
            format_replies.push(ask(
                provider,
                RangeFormatQuery {
                    uri: uri.clone(),
                    version,
                    range: *range,
                    tab_size,
                    insert_spaces,
                },
            ));
        }
    } else if let Some(provider) = support.format() {
        format_replies.push(ask(
            provider,
            FormatQuery {
                uri: uri.clone(),
                version,
                tab_size,
                insert_spaces,
            },
        ));
    }
    let action_replies = optimize
        .then(|| support.actions())
        .flatten()
        .map(|provider| {
            ask(
                provider,
                ActionQuery {
                    uri: uri.clone(),
                    version,
                    span: whole(&text),
                    only: Some(ActionKind::OrganizeImports),
                },
            )
        });
    let filter = ranges.clone();
    let uri_for_apply = uri.clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let mut edits = Vec::new();
        for mut replies in format_replies {
            if let Some(answer) = replies.next().await {
                edits.extend(answer.value);
            }
        }
        if !filter.is_empty() {
            edits.retain(|edit| intersects(&text, edit, &filter));
        }
        if let Some(mut replies) = action_replies
            && let Some(answer) = replies.next().await
            && let Some(entry) = answer.value.into_iter().next()
        {
            for source in entry.edits {
                if source.uri == uri_for_apply {
                    edits.extend(source.edits);
                }
            }
        }
        let edits = merge(&text, edits);
        if edits.is_empty() {
            workspace
                .update_in(cx, |workspace, _, cx| {
                    workspace.notify_user("Reformat", "Nothing to reformat", cx)
                })
                .ok();
            return;
        }
        let change = WorkspaceChange {
            edits: vec![SourceEdit {
                uri: uri_for_apply,
                edits,
            }],
            renames: Vec::new(),
        };
        workspace
            .update_in(cx, |workspace, window, cx| {
                crate::changes::apply(workspace, change, window, cx)
            })
            .ok();
    })
    .detach();
}

fn whole(text: &str) -> TextSpan {
    TextSpan {
        start: TextPoint::default(),
        end: point_at(text, text.len()),
    }
}

fn point_at(text: &str, offset: usize) -> TextPoint {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let start = before.rfind('\n').map_or(0, |index| index + 1);
    TextPoint {
        line,
        character: text[start..offset].encode_utf16().count() as u32,
    }
}

fn span_of(text: &str, range: Range<usize>) -> TextSpan {
    TextSpan {
        start: point_at(text, range.start),
        end: point_at(text, range.end),
    }
}

/// The span of whole lines, the shape a diff hunk names.
fn lines_span(text: &str, lines: Range<u32>) -> TextSpan {
    let starts: Vec<usize> = std::iter::once(0)
        .chain(text.match_indices('\n').map(|(index, _)| index + 1))
        .collect();
    let at = |line: u32| starts.get(line as usize).copied().unwrap_or(text.len());
    span_of(text, at(lines.start)..at(lines.end))
}

fn intersects(text: &str, edit: &DocumentEdit, ranges: &[TextSpan]) -> bool {
    let start = offset_in(text, edit.span.start);
    let end = offset_in(text, edit.span.end).max(start);
    ranges.iter().any(|range| {
        let range_start = offset_in(text, range.start);
        let range_end = offset_in(text, range.end).max(range_start);
        start <= range_end && end >= range_start
    })
}

/// Drops the edits that overlap an earlier one: answers of several requests all count
/// from the same text.
fn merge(text: &str, mut edits: Vec<DocumentEdit>) -> Vec<DocumentEdit> {
    let offset = |edit: &DocumentEdit| {
        let start = offset_in(text, edit.span.start);
        let end = offset_in(text, edit.span.end).max(start);
        (start, end)
    };
    edits.sort_by_key(|edit| offset(edit));
    let mut kept: Vec<DocumentEdit> = Vec::new();
    let mut at = 0;
    for edit in edits {
        let (start, end) = offset(&edit);
        if start < at && !(start == end && start == at) {
            continue;
        }
        at = end.max(start);
        kept.push(edit);
    }
    kept
}

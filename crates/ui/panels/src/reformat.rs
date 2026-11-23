//! Reformat Code and the Reformat File dialog. Ctrl+Alt+L formats with the flags the
//! dialog remembered; the dialog can also organize imports and format only the lines
//! changed since HEAD.

use std::collections::HashSet;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ::base::Cancel;
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
use workspace::{StatusProgress, Workspace};

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
    // Selected text is offered only while the active editor has a selection, and the dialog
    // starts on it then. The remembered scope still decides what Ctrl+Alt+L formats.
    let has_selection = workspace
        .active_view()
        .and_then(|view| view.downcast::<EditorView>().ok())
        .is_some_and(|view| {
            view.read(cx)
                .selection()
                .is_some_and(|(range, _)| !range.is_empty())
        });
    let weak = cx.weak_entity();
    let view = cx.new(|cx| ReformatDialog {
        workspace: weak,
        focus: cx.focus_handle(),
        path,
        optimize: settings.reformat_optimize_imports,
        only_changed: settings.reformat_only_changed,
        has_selection,
        selection: has_selection,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

pub(crate) struct ReformatDialog {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    path: PathBuf,
    optimize: bool,
    only_changed: bool,
    /// Whether the active editor has a selection. Selected text is offered only then.
    has_selection: bool,
    /// Whether the scope is the selection. It stays false while there is no selection.
    selection: bool,
}

impl ReformatDialog {
    /// The title of the dialog bar, for tests.
    #[cfg(test)]
    pub(crate) fn title(&self) -> String {
        dialog_title(&self.path)
    }

    /// Whether Selected text is offered, and whether it is the scope, for tests.
    #[cfg(test)]
    pub(crate) fn scope(&self) -> (bool, bool) {
        (self.has_selection, self.selection && self.has_selection)
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (optimize, only_changed, selection, offered) = (
            self.optimize,
            self.only_changed,
            self.selection,
            self.has_selection,
        );
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.update_settings(
                    |settings| {
                        settings.reformat_optimize_imports = optimize;
                        settings.reformat_only_changed = only_changed;
                        // A scope the dialog did not offer keeps the remembered one, which
                        // Ctrl+Alt+L uses in a file that has a selection.
                        if offered {
                            settings.reformat_selection = selection;
                        }
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

/// The title of the Reformat File dialog: the name of the file, without the folders above it.
fn dialog_title(path: &Path) -> String {
    match path.file_name() {
        Some(name) => format!("Reformat File: {}", name.to_string_lossy()),
        None => "Reformat File".to_owned(),
    }
}

impl Render for ReformatDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let left = (window.viewport_size().width - px(WIDTH)) / 2.;
        let top = window.viewport_size().height * 0.2;
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
        // An unavailable radio is drawn in the disabled colour and takes no hover.
        let radio = |id: &'static str, on: bool, label: &'static str, enabled: bool| {
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .gap(px(SPACE_M))
                .h(px(ROW_HEIGHT))
                .px(px(ROW_PADDING_X))
                .rounded(px(ROW_RADIUS))
                .when(enabled, |this| {
                    this.hover(|style| style.bg(theme.selection_hover))
                })
                .when(!enabled, |this| this.text_color(theme.text_disabled))
                .child(if on { "●" } else { "○" })
                .child(label)
        };
        let selection_on = self.selection && self.has_selection;
        let shell = surface(&theme)
            .id("reformat-dialog")
            .debug_selector(|| "reformat-dialog".to_owned())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
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
                        radio(
                            "reformat-selected",
                            selection_on,
                            "Selected text",
                            self.has_selection,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            // Without a selection in the editor there is nothing to choose.
                            if this.has_selection {
                                this.selection = true;
                                cx.notify();
                            }
                        })),
                    )
                    .child(
                        radio("reformat-whole-file", !selection_on, "Whole file", true).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.selection = false;
                                cx.notify();
                            }),
                        ),
                    ),
            )
            .child(
                DialogButtons::new()
                    .primary(
                        dialog_button(
                            "reformat-run",
                            "reformat-run".to_owned(),
                            "Run",
                            true,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.accept(window, cx))),
                    )
                    .other(
                        dialog_button(
                            "reformat-cancel",
                            "reformat-cancel".to_owned(),
                            "Cancel",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .into_row(),
            );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "reformat-file",
                title: dialog_title(&self.path).into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(px(WIDTH)),
                    height: None,
                },
                min: (360., 200.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
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
    if crate::database_console::is_sql_file(&path) {
        let case = match settings.sql_keyword_case {
            workspace::SqlKeywordCase::Upper => database::KeywordCase::Upper,
            workspace::SqlKeywordCase::Lower => database::KeywordCase::Lower,
            workspace::SqlKeywordCase::Keep => database::KeywordCase::Keep,
        };
        let indent = if settings.use_tabs {
            "\t".to_owned()
        } else {
            " ".repeat(settings.indent_size as usize)
        };
        // A selection of a SQL script is always what the reformat changes.
        let selection = view
            .read(cx)
            .selection()
            .map(|(range, _)| range)
            .filter(|range| !range.is_empty());
        let Some(edit) = crate::database_console::format_edit(&text, selection, case, &indent)
        else {
            workspace.notify_user("Reformat", "Nothing to reformat", cx);
            return;
        };
        let Ok(uri) = CanonPath::new(&path) else {
            return;
        };
        let change = WorkspaceChange {
            edits: vec![SourceEdit {
                uri: uri.to_uri(),
                edits: vec![edit],
            }],
            renames: Vec::new(),
        };
        crate::changes::apply_reformat(workspace, change, window, cx);
        return;
    }
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
        workspace.notify_warning("Reformat", "The project is not in a repository", cx);
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
    workspace: &mut Workspace,
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
        workspace.notify_warning("Reformat", "No language server for this file", cx);
        return;
    };
    if support.format().is_none() && support.range_format().is_none() {
        workspace.notify_warning(
            "Reformat",
            "The language server cannot format this file",
            cx,
        );
        return;
    }
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
                crate::changes::apply_reformat(workspace, change, window, cx)
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

/// The status line entry of a reformat that is still running.
fn progress(label: String, cancel: &Cancel) -> StatusProgress {
    let cancel = cancel.clone();
    StatusProgress {
        label: label.into(),
        cancel: Some(Arc::new(move || cancel.cancel())),
    }
}

/// The files Git changed, as the keys two spellings of one path share. `None` when the
/// project has no repository and the list is not available.
fn changed_files(cx: &App) -> Option<HashSet<PathBuf>> {
    let git = cx.try_global::<ActiveGit>()?.0.read(cx);
    git.repo()?;
    Some(
        git.status()
            .files
            .iter()
            .filter_map(|file| git.absolute(&file.path))
            .map(|path| ::platform::path_key(&path))
            .collect(),
    )
}

/// Ctrl+Alt+L in the project tree: a dialog over the selected folders and files.
pub(crate) fn show_paths_dialog(
    workspace: &mut Workspace,
    paths: Vec<PathBuf>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if paths.is_empty() {
        return;
    }
    let settings = cx
        .try_global::<workspace::Settings>()
        .cloned()
        .unwrap_or_default();
    let changed = changed_files(cx).map(|files| files.len());
    let weak = cx.weak_entity();
    let view = cx.new(|cx| PathsDialog {
        workspace: weak,
        focus: cx.focus_handle(),
        paths,
        optimize: settings.reformat_optimize_imports,
        only_changed: false,
        changed,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

struct PathsDialog {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    paths: Vec<PathBuf>,
    optimize: bool,
    only_changed: bool,
    /// How many files Git changed, or `None` when its list is not available.
    changed: Option<usize>,
}

impl PathsDialog {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let (paths, optimize, only_changed) = (
            self.paths.clone(),
            self.optimize,
            self.only_changed && self.changed.is_some(),
        );
        self.close(window, cx);
        self.workspace
            .update(cx, |workspace, cx| {
                run_paths(workspace, paths, optimize, only_changed, window, cx)
            })
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

impl Render for PathsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let left = (window.viewport_size().width - px(WIDTH)) / 2.;
        let top = window.viewport_size().height * 0.2;
        let summary = match self.paths.as_slice() {
            [one] => one.display().to_string(),
            paths => format!("{} selected items", paths.len()),
        };
        let check =
            |id: &'static str, on: bool, enabled: bool, label: &'static str, theme: &Theme| {
                div()
                    .id(id)
                    .debug_selector(move || id.to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .text_color(if enabled {
                        theme.text
                    } else {
                        theme.text_disabled
                    })
                    .when(enabled, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(checkbox(on, theme))
                    .child(label)
            };
        let only = check(
            "reformat-only-changed",
            self.only_changed,
            self.changed.is_some(),
            "Only changed files",
            &theme,
        );
        let only = if self.changed.is_some() {
            only.on_click(cx.listener(|this, _, _, cx| {
                this.only_changed = !this.only_changed;
                cx.notify();
            }))
        } else {
            only
        };
        let shell = surface(&theme)
            .id("reformat-dialog")
            .debug_selector(|| "reformat-dialog".to_owned())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .child(
                div()
                    .debug_selector(|| "reformat-title".to_owned())
                    .flex()
                    .flex_col()
                    .gap(px(SPACE_XS))
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(summary)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        check(
                            "reformat-optimize",
                            self.optimize,
                            true,
                            "Optimize imports",
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.optimize = !this.optimize;
                            cx.notify();
                        })),
                    )
                    .child(only),
            )
            .child(
                DialogButtons::new()
                    .primary(
                        dialog_button(
                            "reformat-run",
                            "reformat-run".to_owned(),
                            "Reformat",
                            true,
                            true,
                            &theme,
                        )
                        .tooltip(|_, cx| {
                            tooltip("Reformat Code", Some(SharedString::from("Enter")), cx)
                        })
                        .tooltip_show_delay(ui_kit::tooltip_show_delay())
                        .on_click(cx.listener(|this, _, window, cx| this.accept(window, cx))),
                    )
                    .other(
                        dialog_button(
                            "reformat-cancel",
                            "reformat-cancel".to_owned(),
                            "Cancel",
                            false,
                            true,
                            &theme,
                        )
                        .tooltip(|_, cx| tooltip("Cancel", Some(SharedString::from("Esc")), cx))
                        .tooltip_show_delay(ui_kit::tooltip_show_delay())
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .into_row(),
            );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "reformat-code",
                title: "Reformat Code".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(px(WIDTH)),
                    height: None,
                },
                min: (360., 200.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

/// Formats every file under the selected paths that the language server can format, as
/// one change of the window: one undo step and the snapshots of Local History that a
/// refactoring of several files takes.
fn run_paths(
    workspace: &mut Workspace,
    paths: Vec<PathBuf>,
    optimize: bool,
    only_changed: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(support) = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone())
    else {
        workspace.notify_warning("Reformat", "No language server for these files", cx);
        return;
    };
    if support.format().is_none() {
        workspace.notify_warning(
            "Reformat",
            "The language server cannot format these files",
            cx,
        );
        return;
    }
    let changed = if only_changed {
        let Some(changed) = changed_files(cx) else {
            workspace.notify_warning("Reformat", "The project is not in a repository", cx);
            return;
        };
        Some(changed)
    } else {
        None
    };
    let cancel = Cancel::new();
    workspace.set_status_progress(Some(progress("Collecting files".to_owned(), &cancel)), cx);
    let collect_cancel = cancel.clone();
    cx.spawn_in(window, async move |workspace, cx| {
        let mut files = cx
            .background_executor()
            .spawn(async move { collect_files(paths, collect_cancel) })
            .await;
        if let Some(changed) = changed {
            files.retain(|path| changed.contains(&::platform::path_key(path)));
        }
        files.retain(|path| support.matches(path));
        let total = files.len();
        if total == 0 {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.set_status_progress(None, cx);
                    workspace.notify_user("Reformat", "Nothing to reformat", cx);
                })
                .ok();
            return;
        }
        let format = support.format();
        let actions = optimize.then(|| support.actions()).flatten();
        let server = support.server();
        let mut change = WorkspaceChange::default();
        for (done, path) in files.iter().enumerate() {
            if cancel.is_cancelled() {
                break;
            }
            let label = format!("Reformatting {} of {}", done + 1, total);
            workspace
                .update(cx, |workspace, cx| {
                    workspace.set_status_progress(Some(progress(label, &cancel)), cx);
                })
                .ok();
            let Ok(uri) = CanonPath::new(path) else {
                continue;
            };
            let uri = uri.to_uri();
            let open = workspace
                .read_with(cx, |workspace, cx| {
                    workspace
                        .view_of(path)
                        .and_then(|view| view.downcast::<EditorView>().ok())
                        .and_then(|view| {
                            let view = view.read(cx);
                            Some((view.version()?, view.text()?, view.formatting_options()))
                        })
                })
                .ok()
                .flatten();
            let (version, temporary, text, tab_size, insert_spaces) = match open {
                Some((version, text, (tab_size, insert_spaces))) => {
                    (version, false, text, tab_size, insert_spaces)
                }
                None => {
                    let target = path.clone();
                    let text = cx
                        .background_executor()
                        .spawn(
                            async move { text_core::read_file(&target).ok().map(|file| file.text) },
                        )
                        .await
                        .unwrap_or_default();
                    let target = path.clone();
                    let (tab_size, insert_spaces) = cx
                        .background_executor()
                        .spawn(async move { formatting_options_of(&target) })
                        .await;
                    // A document the server already holds is asked at the version it
                    // has; a closed file is opened for the time of the request.
                    if let Some(version) = server.and_then(|server| server.version(&uri)) {
                        (version, false, text, tab_size, insert_spaces)
                    } else {
                        if let Some(server) = server {
                            server.open(&uri, 1, text.clone().into());
                        }
                        (1, true, text, tab_size, insert_spaces)
                    }
                }
            };
            let mut edits = Vec::new();
            if let Some(provider) = format {
                let mut replies = ask(
                    provider,
                    FormatQuery {
                        uri: uri.clone(),
                        version,
                        tab_size,
                        insert_spaces,
                    },
                );
                if let Some(answer) = replies.next().await {
                    edits.extend(answer.value);
                }
            }
            if let Some(provider) = actions {
                let mut replies = ask(
                    provider,
                    ActionQuery {
                        uri: uri.clone(),
                        version,
                        span: whole(&text),
                        only: Some(ActionKind::OrganizeImports),
                    },
                );
                if let Some(answer) = replies.next().await
                    && let Some(entry) = answer.value.into_iter().next()
                {
                    for source in entry.edits {
                        if source.uri == uri {
                            edits.extend(source.edits);
                        }
                    }
                }
            }
            if temporary && let Some(server) = server {
                server.close(&uri);
            }
            let edits = merge(&text, edits);
            if edits.is_empty() {
                continue;
            }
            change.edits.push(SourceEdit { uri, edits });
        }
        let cancelled = cancel.is_cancelled();
        workspace
            .update_in(cx, |workspace, window, cx| {
                workspace.set_status_progress(None, cx);
                if cancelled {
                    workspace.notify_user("Reformat", "Reformat cancelled", cx);
                } else if change.edits.is_empty() {
                    workspace.notify_user("Reformat", "Nothing to reformat", cx);
                } else {
                    crate::changes::apply(workspace, change, window, cx);
                }
            })
            .ok();
    })
    .detach();
}

/// Every file under the paths, without hidden folders and without following links.
/// A cancel stops the walk as soon as it reaches the next folder entry.
fn collect_files(paths: Vec<PathBuf>, cancel: Cancel) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for path in paths {
        if cancel.is_cancelled() {
            break;
        }
        walk_files(&path, &cancel, &mut files);
    }
    files.sort();
    files.dedup();
    files
}

fn walk_files(path: &Path, cancel: &Cancel, files: &mut Vec<PathBuf>) {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return;
    };
    if metadata.file_type().is_symlink() {
        return;
    }
    if metadata.is_dir() {
        let Ok(entries) = std::fs::read_dir(path) else {
            return;
        };
        for entry in entries.flatten() {
            if cancel.is_cancelled() {
                return;
            }
            let child = entry.path();
            let hidden = child
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with('.'));
            if hidden {
                continue;
            }
            walk_files(&child, cancel, files);
        }
    } else if metadata.is_file() {
        files.push(path.to_path_buf());
    }
}

/// The tab size and the spaces flag of a file that is not open, from the
/// `.editorconfig` files around it. Runs off the UI thread.
fn formatting_options_of(path: &Path) -> (u32, bool) {
    text_core::for_file(path)
        .indentation()
        .map_or((4, true), |(tabs, size, _)| (size as u32, !tabs))
}

//! Puts `editor-view` behind the tabs of the workspace.

use std::path::PathBuf;
use std::rc::Rc;

use editor_view::{
    CompletionCase, EditorEvent, EditorOptions, EditorStyle, EditorView, SaveOptions, Spot,
    TrimTrailing,
};
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{
    DOCUMENTATION_TOOL, EditorFactory, EditorStatus, EditorTab, IndentChoice, Settings,
    USAGES_TOOL, Workspace,
};

use crate::documentation::ActiveDocumentation;
use crate::outline::{self, OpenEditors};
use crate::problems::ActiveProblems;
use crate::run::TestsChanged;
use crate::script::ActiveRunPanel;
use crate::usages::ActiveUsages;
use crate::{clipboard, debug, marks, navigation, places};

/// A Safe Delete on a file of the project tree, once the editor of the file is loaded.
#[derive(Default)]
pub struct PendingSafeDelete(pub Option<(PathBuf, usize)>);

impl Global for PendingSafeDelete {}

/// The theme's colours with the font size of Settings; the line follows the size.
pub(crate) fn style_of(cx: &App) -> EditorStyle {
    let mut style = EditorStyle::from_theme(cx.global::<Theme>());
    if let Some(settings) = cx.try_global::<Settings>() {
        let ratio = style.line_height / style.font_size;
        let font_size = (settings.font_size * settings.ui_scale).round().max(6.);
        if !settings.editor_font_family.trim().is_empty() {
            style.font_family = settings.editor_font_family.clone().into();
        }
        style.font_size = font_size;
        let spacing = settings
            .line_spacing
            .clamp(workspace::MIN_LINE_SPACING, workspace::MAX_LINE_SPACING);
        style.line_height = (font_size * ratio * spacing).round();
    }
    style
}

/// What Settings turns on and off in an open editor.
pub(crate) fn options_of(cx: &App) -> EditorOptions {
    let Some(settings) = cx.try_global::<Settings>() else {
        return EditorOptions::default();
    };
    EditorOptions {
        line_numbers: settings.show_line_numbers,
        highlight_current_line: settings.highlight_current_line,
        indent_guides: settings.show_indent_guides,
        caret_blink: settings.caret_blink,
        wheel_zoom: settings.zoom_with_wheel,
        save: SaveOptions {
            trim_trailing: match settings.trim_trailing_whitespace {
                workspace::TrimTrailingWhitespace::None => TrimTrailing::None,
                workspace::TrimTrailingWhitespace::Modified => TrimTrailing::Modified,
                workspace::TrimTrailingWhitespace::All => TrimTrailing::All,
            },
            ensure_final_newline: settings.ensure_final_newline,
        },
        completion_case: match settings.completion_case {
            workspace::CompletionCase::None => CompletionCase::None,
            workspace::CompletionCase::FirstLetter => CompletionCase::FirstLetter,
            workspace::CompletionCase::All => CompletionCase::All,
        },
        completion_auto_popup: settings.completion_auto_popup,
        completion_delay_ms: u64::from(settings.completion_delay_ms),
        semantic_highlighting: settings.semantic_highlighting,
        tab_size: settings.tab_size as usize,
        indent_size: settings.indent_size as usize,
        use_tabs: settings.use_tabs,
        inlay_parameter_names: match settings.inlay_parameter_names {
            workspace::InlayParameterNames::None => lang_api::InlayParameterNames::None,
            workspace::InlayParameterNames::Literals => lang_api::InlayParameterNames::Literals,
            workspace::InlayParameterNames::All => lang_api::InlayParameterNames::All,
        },
        inlay_type_hints: settings.inlay_type_hints,
        quick_doc_on_hover: settings.quick_doc_on_hover,
        code_vision_usages: settings.code_vision_usages,
        code_vision_implementations: settings.code_vision_implementations,
        render_doc_comments: settings.render_doc_comments,
    }
}

/// `12:5`, and `12:5 (24 chars)` while text is selected.
fn caret_text(line: usize, column: usize, selected: usize) -> String {
    match selected {
        0 => format!("{}:{}", line + 1, column + 1),
        count => format!("{}:{} ({count} chars)", line + 1, column + 1),
    }
}

fn status_of(view: &EditorView) -> EditorStatus {
    let (line, column) = view.caret();
    let (indent_tabs, indent_size, from_editorconfig) = view.indent_status();
    let mut indent = view.indent_label();
    if from_editorconfig {
        indent.push_str(" (EditorConfig)");
    }
    EditorStatus {
        caret: caret_text(line, column, view.selected_chars()),
        eol: view.line_ending().into(),
        encoding: view.encoding().into(),
        indent,
        language: view.language_name().map(str::to_owned),
        indent_tabs,
        indent_size,
        indent_editorconfig: from_editorconfig,
        dirty: view.is_dirty(),
        insert: if view.overwrite() {
            "Overwrite".into()
        } else {
            "Insert".into()
        },
        read_only: view.is_read_only(),
    }
}

fn open(
    workspace: &WeakEntity<Workspace>,
    path: PathBuf,
    window: &mut Window,
    cx: &mut App,
) -> EditorTab {
    let style = style_of(cx);
    let view = cx.new(|cx| {
        cx.observe_global::<Theme>(|view: &mut EditorView, cx| {
            let style = style_of(cx);
            view.set_style(style, cx);
        })
        .detach();
        cx.observe_global::<Settings>(|view: &mut EditorView, cx| {
            let style = style_of(cx);
            view.set_style(style, cx);
            view.set_options(options_of(cx), cx);
            marks::refresh(view, cx);
        })
        .detach();
        if let Some(panel) = cx
            .try_global::<ActiveRunPanel>()
            .map(|panel| panel.0.clone())
        {
            cx.subscribe(&panel, |view, _, _: &TestsChanged, cx| {
                marks::refresh(view, cx)
            })
            .detach();
            cx.subscribe(&panel, |view, _, _: &crate::run::ConfigsChanged, cx| {
                marks::refresh(view, cx)
            })
            .detach();
        }
        if let Some(panel) = cx
            .try_global::<debug::ActiveDebug>()
            .map(|panel| panel.0.clone())
        {
            cx.subscribe(&panel, |view, _, _: &debug::BreakpointsChanged, cx| {
                debug::refresh_breakpoints(view, cx)
            })
            .detach();
        }
        EditorView::open(path, style, window, cx)
    });
    view.update(cx, debug::refresh_breakpoints);
    view.update(cx, |view, cx| view.set_options(options_of(cx), cx));

    OpenEditors::add(cx, view.read(cx).path().to_path_buf(), view.downgrade());
    outline::refresh(&view, false, cx);
    let runner = workspace.clone();
    window
        .subscribe(
            &view,
            cx,
            move |view, event: &EditorEvent, window, cx| match event {
                EditorEvent::Loaded | EditorEvent::Changed => {
                    if matches!(event, EditorEvent::Loaded) {
                        let path = view.read(cx).path().to_path_buf();
                        let pending = cx.default_global::<PendingSafeDelete>().0.take();
                        if let Some((wanted, offset)) = pending
                            && wanted == path
                        {
                            let view = view.clone();
                            cx.defer(move |cx| {
                                view.update(cx, |view, cx| view.safe_delete_at_offset(offset, cx));
                            });
                        }
                    }
                    view.update(cx, marks::refresh);
                    outline::refresh(&view, matches!(event, EditorEvent::Changed), cx);
                    if matches!(event, EditorEvent::Changed) {
                        places::remember(&view, cx);
                        let path = view.read(cx).path().to_path_buf();
                        runner
                            .update(cx, |workspace, cx| workspace.remember_edited(&path, cx))
                            .ok();
                    }
                    // The status line and the star of the tab show the caret and the
                    // text of the editor; they redraw themselves, not the window.
                    runner
                        .update(cx, |workspace, cx| workspace.editor_chrome_changed(cx))
                        .ok();
                }
                // The star of the tab goes away as soon as undo reaches the saved text.
                EditorEvent::Dirty(_) | EditorEvent::Moved => {
                    runner
                        .update(cx, |workspace, cx| workspace.editor_chrome_changed(cx))
                        .ok();
                }
                EditorEvent::ShowBookmarks => {
                    runner
                        .update(cx, |workspace, cx| {
                            places::show_bookmarks(workspace, window, cx)
                        })
                        .ok();
                }
                EditorEvent::OpenBookmark { path, line } => {
                    let (path, line) = (path.clone(), *line as usize);
                    runner
                        .update(cx, |workspace, cx| {
                            let spot = Spot::Line { line, column: 0 };
                            navigation::go_to(workspace, path, Some(spot), true, window, cx)
                        })
                        .ok();
                }
                EditorEvent::PasteHistory => {
                    let editor = view.clone();
                    runner
                        .update(cx, |workspace, cx| {
                            clipboard::ClipboardPopup::show(editor, workspace, window, cx)
                        })
                        .ok();
                }
                EditorEvent::ShowDocumentation => {
                    let editor = view.clone();
                    runner
                        .update(cx, |workspace, cx| {
                            workspace.show_tool(DOCUMENTATION_TOOL, window, cx);
                            if let Some(panel) = cx.try_global::<ActiveDocumentation>() {
                                let panel = panel.0.clone();
                                panel.update(cx, |panel, cx| panel.show_editor(editor.clone(), cx));
                            }
                            workspace.focus_editor(window, cx);
                        })
                        .ok();
                }
                EditorEvent::ChangeSignature(spot) => {
                    let spot = spot.clone();
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    // The editor event comes from a key of the window being updated:
                    // the overlay opens after that update is over.
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        crate::change_signature::ChangeSignatureDialog::show(
                                            spot, workspace, window, cx,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::CallHierarchy(spot) => {
                    let Some(hub) = cx.try_global::<crate::call_hierarchy::ActiveCallHierarchy>()
                    else {
                        return;
                    };
                    let hub = hub.0.clone();
                    let spot = spot.clone();
                    hub.update(cx, |hub, cx| hub.start(spot, cx));
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        workspace.show_tool(
                                            workspace::CALL_HIERARCHY_TOOL,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::MethodHierarchy(spot) => {
                    let Some(hub) =
                        cx.try_global::<crate::method_hierarchy::ActiveMethodHierarchy>()
                    else {
                        return;
                    };
                    let hub = hub.0.clone();
                    let spot = spot.clone();
                    hub.update(cx, |hub, cx| hub.start(spot, cx));
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        workspace.show_tool(
                                            workspace::METHOD_HIERARCHY_TOOL,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::DeleteFile(path) => {
                    let path = path.clone();
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    let history = crate::history::get(cx);
                    let target = path.clone();
                    let task = cx.background_executor().spawn(async move {
                        if let Some(history) = &history {
                            let _ =
                                history.record_tree(&target, local_history::Cause::BeforeDelete);
                        }
                        crate::tree_ops::delete_on_disk(&target)?;
                        if let Some(history) = &history {
                            let _ = history.record_removed(&target);
                        }
                        Ok::<(), std::io::Error>(())
                    });
                    window
                        .spawn(cx, async move |cx| {
                            let result = task.await;
                            let path = path.clone();
                            let closed = result.is_ok();
                            if closed {
                                handle
                                    .update(cx, |_, window, cx| {
                                        workspace
                                            .update(cx, |workspace, cx| {
                                                workspace.discard_tabs_under(&path, window, cx)
                                            })
                                            .ok();
                                    })
                                    .ok();
                            }
                        })
                        .detach();
                }
                EditorEvent::CreateFiles(files) => {
                    let files = files.clone();
                    let runner = runner.clone();
                    let handle = window.window_handle();
                    // An existing file is never written over: a test class that is already
                    // there opens as it is. The disk work stays off the window thread.
                    let task = cx.background_executor().spawn(async move {
                        let mut opened = None;
                        for file in &files {
                            if let Some(parent) = file.path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            let written = std::fs::OpenOptions::new()
                                .write(true)
                                .create_new(true)
                                .open(&file.path)
                                .and_then(|mut out| {
                                    std::io::Write::write_all(&mut out, file.text.as_bytes())
                                });
                            if (written.is_ok() || file.path.exists()) && opened.is_none() {
                                opened = Some(file.path.clone());
                            }
                        }
                        opened
                    });
                    window
                        .spawn(cx, async move |cx| {
                            let Some(path) = task.await else {
                                return;
                            };
                            handle
                                .update(cx, |_, window, cx| {
                                    runner
                                        .update(cx, |workspace, cx| {
                                            workspace.open_file(path, window, cx)
                                        })
                                        .ok();
                                })
                                .ok();
                        })
                        .detach();
                }
                EditorEvent::GutterClick(line) => {
                    if let Some(panel) = cx.try_global::<debug::ActiveDebug>() {
                        let panel = panel.0.clone();
                        let path = view.read(cx).path().to_path_buf();
                        let line = *line + 1;
                        panel.update(cx, |panel, cx| panel.toggle_breakpoint(path, line, cx));
                    }
                }
                EditorEvent::RunMark(line) => {
                    let view = view.clone();
                    runner
                        .update(cx, |workspace, cx| {
                            marks::run_mark_clicked(&view, *line as usize, workspace, window, cx)
                        })
                        .ok();
                }
                EditorEvent::DebugRunMark(line) => {
                    let view = view.clone();
                    runner
                        .update(cx, |workspace, cx| {
                            marks::debug_mark_clicked(&view, *line as usize, workspace, window, cx)
                        })
                        .ok();
                }
                EditorEvent::ContextMenu(position) => {
                    let model = runner
                        .read_with(cx, |workspace, _| {
                            crate::editor_menu::MenuModel::of(workspace)
                        })
                        .ok();
                    if let Some(model) = model {
                        crate::editor_menu::show(
                            view.clone(),
                            runner.clone(),
                            model,
                            *position,
                            cx,
                        );
                    }
                }
                _ => {}
            },
        )
        .detach();

    let reports = workspace.clone();
    let handle = window.window_handle();
    cx.subscribe(&view, move |view, event: &EditorEvent, cx| match event {
        EditorEvent::Navigate(path) => {
            let path = path.clone();
            let workspace = reports.clone();
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            // A jump of the language is a place Ctrl+Alt+Left returns from.
                            // The caret comes from the reveal the editor already recorded.
                            navigation::go_to(workspace, path, None, true, window, cx);
                        })
                        .ok();
                })
                .ok();
        }
        EditorEvent::Failed(message) => {
            let title = view.read(cx).title();
            let message = message.clone();
            reports
                .update(cx, |workspace, cx| {
                    workspace.notify_user(title, message, cx)
                })
                .ok();
        }
        EditorEvent::Conflict => {
            let title = view.read(cx).title();
            let text = format!(
                "{title} changed on disk and has unsaved edits, which are kept. \
                 Use Reload from Disk in the tab menu to take the disk version; Undo brings the edits back."
            );
            reports
                .update(cx, |workspace, cx| {
                    workspace.notify_user("File changed on disk", text, cx)
                })
                .ok();
        }
        EditorEvent::Saved => {
            let path = view.read(cx).path().to_path_buf();
            crate::history::snapshot_file(&path, local_history::Cause::Saved, cx);
        }
        EditorEvent::ExternalChange(bytes) => {
            let path = view.read(cx).path().to_path_buf();
            crate::history::snapshot_bytes(
                &path,
                Some(bytes.clone()),
                local_history::Cause::BeforeReload,
                cx,
            );
        }
        EditorEvent::SaveAll => {
            reports
                .update(cx, |workspace, cx| workspace.save_all(cx))
                .ok();
        }
        EditorEvent::ApplyChange(change) => {
            let change = change.clone();
            let workspace = reports.clone();
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::changes::apply(workspace, change, window, cx)
                        })
                        .ok();
                })
                .ok();
        }
        EditorEvent::Reformat => {
            let workspace = reports.clone();
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::reformat::run(workspace, window, cx)
                        })
                        .ok();
                })
                .ok();
        }
        EditorEvent::Usages(usages) => {
            let Some(hub) = cx.try_global::<ActiveUsages>() else {
                return;
            };
            let hub = hub.0.clone();
            let usages = usages.clone();
            hub.update(cx, |hub, cx| hub.set_usages(usages, cx));
            let workspace = reports.clone();
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| workspace.show_tool(USAGES_TOOL, window, cx))
                        .ok();
                })
                .ok();
        }
        EditorEvent::UsagesDetailed(usages) => {
            let Some(hub) = cx.try_global::<ActiveUsages>() else {
                return;
            };
            let hub = hub.0.clone();
            let usages = usages.clone();
            hub.update(cx, |hub, cx| hub.set_detailed_usages(usages, cx));
            let workspace = reports.clone();
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| workspace.show_tool(USAGES_TOOL, window, cx))
                        .ok();
                })
                .ok();
        }
        EditorEvent::ShowUsages(update) => {
            let update = update.clone();
            let current = view.read(cx).path().to_path_buf();
            let position = view.read(cx).caret_point();
            let workspace = reports.clone();
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::usages::show_usages(
                                update, current, position, workspace, window, cx,
                            )
                        })
                        .ok();
                })
                .ok();
        }
        EditorEvent::Diagnostics(diagnostics) => {
            let Some(problems) = cx.try_global::<ActiveProblems>() else {
                return;
            };
            let hub = problems.0.clone();
            let path = view.read(cx).path().to_path_buf();
            let diagnostics = diagnostics.clone();
            hub.update(cx, |hub, cx| {
                hub.set_file_diagnostics(path, &diagnostics, cx)
            });
        }
        _ => {}
    })
    .detach();

    let focus = view.focus_handle(cx);
    let (status, save, save_now) = (view.clone(), view.clone(), view.clone());
    let (caret, restore, format) = (view.clone(), view.clone(), view.clone());
    let indent = view.clone();
    let reload = view.clone();
    EditorTab {
        view: view.into(),
        focus,
        status: Rc::new(move |cx| status_of(status.read(cx))),
        save: Rc::new(move |cx| {
            if save.read(cx).is_dirty() {
                save.update(cx, |view, cx| view.save(cx));
            }
        }),
        save_now: Rc::new(move |cx| {
            if save_now.read(cx).is_dirty() {
                save_now.update(cx, |view, cx| view.save_blocking(cx))
            } else {
                None
            }
        }),
        caret: Rc::new(move |cx| caret.read(cx).caret()),
        restore_caret: Rc::new(move |(line, column), cx| {
            restore.update(cx, |view, cx| view.restore_caret(line, column, cx));
        }),
        reload_from_disk: Rc::new(move |cx| {
            reload.update(cx, |view, cx| view.reload_from_disk(cx));
        }),
        set_format: Rc::new(move |name, cx| {
            format.update(cx, |view, cx| view.set_format_name(name, cx));
        }),
        set_indent: Rc::new(move |choice, cx| {
            let (tabs, size) = match choice {
                IndentChoice::Tabs => (Some(true), None),
                IndentChoice::Spaces => (Some(false), None),
                IndentChoice::Size(size) => (None, Some(size)),
            };
            indent.update(cx, |view, cx| view.set_indent(tabs, size, cx));
        }),
    }
}

/// The factory the workspace opens files with.
pub fn factory(workspace: WeakEntity<Workspace>) -> EditorFactory {
    Rc::new(move |path, window, cx| open(&workspace, path, window, cx))
}

#[cfg(test)]
mod tests {
    use super::caret_text;

    #[test]
    fn the_caret_counts_from_one_and_shows_the_selection() {
        assert_eq!(caret_text(0, 0, 0), "1:1");
        assert_eq!(caret_text(11, 4, 24), "12:5 (24 chars)");
    }
}

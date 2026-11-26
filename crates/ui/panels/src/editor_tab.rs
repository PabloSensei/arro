//! Puts `editor-view` behind the tabs of the workspace.

use std::path::PathBuf;
use std::rc::Rc;

use editor_view::{
    ActionsOnSave, BundleMode, CompletionCase, EditorEvent, EditorOptions, EditorStyle, EditorView,
    PreviewMode, SaveOptions, Spot, TrimTrailing,
};
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{
    BundleLayout, BundleTab, DOCUMENTATION_TOOL, EditorFactory, EditorStatus, EditorTab,
    IndentChoice, PreviewLayout, PreviewTab, Settings, USAGES_TOOL, Workspace,
};

use crate::documentation::ActiveDocumentation;
use crate::outline::{self, OpenEditors};
use crate::problems::ActiveProblems;
use crate::run::TestsChanged;
use crate::script::ActiveRunPanel;
use crate::usages::ActiveUsages;
use crate::{clipboard, debug, marks, navigation, places};

/// The buttons of the save notification for a text the encoding cannot store.
const SAVE_AS_UTF8: &str = "editor.save_as_utf8";
const SAVE_CANCEL: &str = "editor.save_cancel";

/// A Safe Delete on a file of the project tree, once the editor of the file is loaded.
#[derive(Default)]
pub struct PendingSafeDelete(pub Option<(PathBuf, usize)>);

impl Global for PendingSafeDelete {}

/// The theme's colours with the font size of Settings; the line follows the size. In
/// Presentation Mode the editor font grows without a change of the settings.
pub(crate) fn style_of(cx: &App) -> EditorStyle {
    let mut style = EditorStyle::from_theme(cx.global::<Theme>());
    if let Some(settings) = cx.try_global::<Settings>() {
        let ratio = style.line_height / style.font_size;
        let points = cx
            .try_global::<workspace::EditorAppearance>()
            .and_then(|appearance| appearance.presentation_font)
            .unwrap_or(settings.font_size);
        let font_size = points.round().max(6.);
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
    let appearance = cx
        .try_global::<workspace::EditorAppearance>()
        .copied()
        .unwrap_or_default();
    EditorOptions {
        line_numbers: settings.show_line_numbers && !appearance.distraction_free,
        gutter_icons: settings.show_gutter_icons && !appearance.distraction_free,
        highlight_current_line: settings.highlight_current_line,
        highlight_occurrences: true,
        indent_guides: settings.show_indent_guides,
        breadcrumbs: settings.show_breadcrumbs
            && settings.navigation_bar_visible
            && !appearance.distraction_free,
        sticky_lines: settings.show_sticky_lines,
        centered_text: appearance.distraction_free,
        right_margin: settings.right_margin as usize,
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
        actions_on_save: ActionsOnSave {
            reformat: settings.actions_on_save_reformat,
            reformat_changed: settings.actions_on_save_reformat_scope
                == workspace::ReformatScope::ChangedLines,
            reformat_files: settings.actions_on_save_reformat_files.clone(),
            optimize_imports: settings.actions_on_save_optimize_imports,
            cleanup: settings.actions_on_save_cleanup,
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
        markdown_images_from_internet: settings.markdown_images_from_internet,
        ai_features: settings.ai_enabled,
        soft_wrap: settings.soft_wrap,
        show_whitespace: settings.show_whitespace,
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
    if let Some(source) = view.indent_source_name() {
        indent.push_str(" (");
        indent.push_str(source);
        indent.push(')');
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

/// Once in a while, records a buffer that has changed and is not saved yet, so that a
/// crash or a command that rewrites the file loses no more than one interval of work. The
/// version of the document tells whether the text changed since the last snapshot.
fn watch_unsaved(view: &Entity<EditorView>, cx: &mut App) {
    let weak = view.downgrade();
    cx.spawn(async move |cx| {
        let mut last = None;
        loop {
            let interval = cx.update(|cx| crate::history::auto_interval(cx));
            cx.background_executor().timer(interval).await;
            let next = weak.update(cx, |view, cx| {
                if !view.is_dirty() {
                    return None;
                }
                let version = view.version()?;
                if last == Some(version) {
                    return None;
                }
                // The history opens in the background: an interval that passes before it
                // is ready leaves the version for the next one.
                crate::history::get(cx)?;
                let path = view.path().to_path_buf();
                crate::history::snapshot_bytes(
                    &path,
                    view.file_bytes(),
                    local_history::Cause::Auto,
                    cx,
                );
                Some(version)
            });
            match next {
                Ok(Some(version)) => last = Some(version),
                Ok(None) => {}
                Err(_) => break,
            }
        }
    })
    .detach();
}

fn open(
    workspace: &WeakEntity<Workspace>,
    state_dir: &std::path::Path,
    path: PathBuf,
    window: &mut Window,
    cx: &mut App,
) -> EditorTab {
    let style = style_of(cx);
    let view = cx.new(|cx| {
        cx.observe_global::<Theme>(|view: &mut EditorView, cx| {
            let style = style_of(cx);
            view.set_style(style, cx);
            crate::coverage::refresh(view, cx);
        })
        .detach();
        cx.observe_global::<Settings>(|view: &mut EditorView, cx| {
            let style = style_of(cx);
            view.set_style(style, cx);
            view.set_options(options_of(cx), cx);
            marks::refresh(view, cx);
            crate::coverage::refresh(view, cx);
            debug::refresh_inline_values(view, cx);
        })
        .detach();
        cx.observe_global::<workspace::EditorAppearance>(|view: &mut EditorView, cx| {
            let style = style_of(cx);
            view.set_style(style, cx);
            view.set_options(options_of(cx), cx);
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
            cx.subscribe(&panel, |view, _, _: &crate::run::CoverageChanged, cx| {
                crate::coverage::refresh(view, cx)
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
            cx.subscribe(&panel, |view, _, _: &debug::ValuesChanged, cx| {
                debug::refresh_inline_values(view, cx)
            })
            .detach();
        }
        EditorView::open(path, style, window, cx)
    });
    view.update(cx, debug::refresh_breakpoints);
    view.update(cx, debug::refresh_inline_values);
    view.update(cx, |view, cx| view.set_options(options_of(cx), cx));
    watch_unsaved(&view, cx);
    let opened = view.read(cx).path().to_path_buf();
    crate::library_sources::tab_opened(&opened, cx);
    crate::jshell_panel::tab_opened(&opened, cx);
    // The tab is built while the window updates its tabs, so the root of the project is
    // read once that update is over.
    let root_workspace = workspace.clone();
    let root_view = view.clone();
    cx.defer(move |cx| {
        let root = root_workspace
            .read_with(cx, |workspace, _| workspace.root().to_path_buf())
            .ok();
        root_view.update(cx, |view, _| view.set_project_root(root));
    });
    let pictures = state_dir.join("markdown-images");
    view.update(cx, |view, _| view.set_picture_cache(pictures));

    OpenEditors::add(cx, view.read(cx).path().to_path_buf(), view.downgrade());
    outline::refresh(&view, false, cx);
    let bundle = crate::bundle_editor::register(&opened, cx).then(|| {
        let read_view = view.clone();
        let write_view = view.clone();
        BundleTab {
            layout: Rc::new(move |cx: &App| match read_view.read(cx).bundle_mode(cx) {
                Some(BundleMode::Bundle) => BundleLayout::Bundle,
                _ => BundleLayout::Text,
            }),
            set_layout: Rc::new(move |layout, cx: &mut App| {
                let mode = match layout {
                    BundleLayout::Text => BundleMode::Text,
                    BundleLayout::Bundle => BundleMode::Bundle,
                };
                write_view.update(cx, |view, cx| view.set_bundle_mode(mode, cx));
            }),
        }
    });
    let runner = workspace.clone();
    window
        .subscribe(
            &view,
            cx,
            move |view, event: &EditorEvent, window, cx| match event {
                EditorEvent::Loaded | EditorEvent::Changed => {
                    let properties = view
                        .read(cx)
                        .path()
                        .extension()
                        .is_some_and(|extension| extension.eq_ignore_ascii_case("properties"));
                    if properties {
                        crate::bundle_editor::files_changed(cx);
                    }
                    if matches!(event, EditorEvent::Loaded) {
                        runner
                            .update(cx, |workspace, cx| places::remember(&view, workspace, cx))
                            .ok();
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
                        crate::http::file_opened(&view, cx);
                        crate::agents::refresh_marks(cx);
                    }
                    view.update(cx, marks::refresh);
                    view.update(cx, crate::kubernetes::refresh_marks);
                    view.update(cx, crate::coverage::refresh);
                    crate::endpoints::editor_changed(&view, cx);
                    crate::spelling::editor_changed(&view, cx);
                    crate::git::editor_changed(&view, cx);
                    outline::refresh(&view, matches!(event, EditorEvent::Changed), cx);
                    if matches!(event, EditorEvent::Changed) {
                        runner
                            .update(cx, |workspace, cx| places::remember(&view, workspace, cx))
                            .ok();
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
                EditorEvent::MemberRefactoring { refactoring, spot } => {
                    let refactoring = *refactoring;
                    let spot = spot.clone();
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        crate::member_form::MemberFormDialog::show(
                                            refactoring,
                                            spot,
                                            workspace,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::MoveMembers(spot) => {
                    let spot = spot.clone();
                    let origin = view.clone();
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        crate::move_members::MoveMembersDialog::show(
                                            spot,
                                            Some(origin),
                                            workspace,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::MoveTargets(spot) => {
                    let spot = spot.clone();
                    let origin = view.clone();
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        crate::move_targets::MoveTargetsDialog::show(
                                            spot,
                                            Some(origin),
                                            workspace,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::RefactorForm(handle_text) => {
                    let handle_text = handle_text.clone();
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    let origin = view.clone();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        crate::refactor_form::RefactorFormDialog::show(
                                            handle_text.clone(),
                                            Some(origin),
                                            workspace,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::DisableInspection { id, title } => {
                    let (id, title) = (id.clone(), title.clone());
                    let workspace = runner.clone();
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::inspection::disable(workspace, &id, &title, cx);
                        })
                        .ok();
                }
                EditorEvent::SaveSpelling { word, project } => {
                    let (word, project) = (word.clone(), *project);
                    let workspace = runner.clone();
                    workspace
                        .update(cx, |_, cx| {
                            crate::spelling::save_word(&word, project, cx);
                        })
                        .ok();
                }
                EditorEvent::EditInspection(id) => {
                    let id = id.clone();
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        crate::settings::SettingsView::show_inspection(
                                            workspace, window, cx, &id,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::CopyElement => {
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    // Refactor This runs the row while the editor is being updated: the
                    // window takes the command once that update is over.
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        workspace.run_command("workspace.copy_element", window, cx)
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
                EditorEvent::TypeHierarchy(spot) => {
                    let Some(hub) = cx.try_global::<crate::type_hierarchy::ActiveTypeHierarchy>()
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
                                            workspace::TYPE_HIERARCHY_TOOL,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::ShowDiagram(spot) => {
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    let spot = spot.clone();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        crate::diagram::show(
                                            workspace,
                                            crate::diagram::DiagramSource::Spot(spot),
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::ShowSequenceDiagram(spot) => {
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    let spot = spot.clone();
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        crate::sequence_diagram::show(
                                            workspace,
                                            spot,
                                            lang_api::SequenceSettings::default(),
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
                EditorEvent::FixWithAi(problem) => {
                    runner
                        .update(cx, |workspace, cx| {
                            let message =
                                crate::fix_with_ai::diagnostic_message(workspace.root(), problem);
                            crate::fix_with_ai::ask(workspace, message, window, cx)
                        })
                        .ok();
                }
                EditorEvent::CreateTest(spot) => {
                    let spot = spot.clone();
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    // The key of the window is being handled: the overlay opens after it.
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        crate::create_test::CreateTestDialog::show(
                                            spot.clone(),
                                            workspace,
                                            window,
                                            cx,
                                        )
                                    })
                                    .ok();
                            })
                            .ok();
                    });
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
                EditorEvent::GutterClick { line, shift } => {
                    if let Some(panel) = cx.try_global::<debug::ActiveDebug>() {
                        let panel = panel.0.clone();
                        let path = view.read(cx).path().to_path_buf();
                        let line = *line + 1;
                        let shift = *shift;
                        let declaration = view
                            .read(cx)
                            .declaration_on_line((line - 1) as usize)
                            .cloned();
                        match declaration {
                            Some(declaration)
                                if matches!(
                                    declaration.kind,
                                    editor_view::OutlineKind::Method
                                        | editor_view::OutlineKind::Constructor
                                        | editor_view::OutlineKind::Function
                                ) =>
                            {
                                panel.update(cx, |panel, cx| {
                                    panel.toggle_function_breakpoint(
                                        path,
                                        line,
                                        declaration.owner,
                                        declaration.name,
                                        cx,
                                    )
                                });
                            }
                            Some(declaration)
                                if matches!(
                                    declaration.kind,
                                    editor_view::OutlineKind::Field
                                        | editor_view::OutlineKind::Constant
                                ) =>
                            {
                                panel.update(cx, |panel, cx| {
                                    panel.toggle_field_watchpoint(
                                        path,
                                        line,
                                        declaration.owner,
                                        declaration.name,
                                        cx,
                                    )
                                });
                            }
                            _ => {
                                if shift {
                                    panel.update(cx, |panel, cx| {
                                        panel.toggle_non_suspending_breakpoint(path, line, cx)
                                    });
                                } else {
                                    panel.update(cx, |panel, cx| {
                                        panel.toggle_breakpoint(path, line, cx)
                                    });
                                }
                            }
                        }
                    }
                }
                EditorEvent::ChangeMarkClick(line) => {
                    let Some(git) = cx.try_global::<crate::git::ActiveGit>() else {
                        return;
                    };
                    let git = git.0.clone();
                    let line = *line;
                    let view = view.clone();
                    git.update(cx, |git, cx| git.change_mark_clicked(view, line, cx));
                }
                EditorEvent::AgentMarkClick(line) => {
                    let Some(panel) = cx.try_global::<crate::agents::ActiveAgents>() else {
                        return;
                    };
                    let panel = panel.0.clone();
                    let line = *line;
                    let view = view.clone();
                    let handle = window.window_handle();
                    // The event comes from the update of the editor: the window of the
                    // hunk opens after it is over.
                    cx.defer(move |cx| {
                        panel.update(cx, |panel, cx| {
                            panel.open_agent_hunk(view.clone(), line, handle, cx)
                        });
                    });
                }
                EditorEvent::ChangeMarkHover(line) => {
                    let Some(git) = cx.try_global::<crate::git::ActiveGit>() else {
                        return;
                    };
                    let git = git.0.clone();
                    let line = *line;
                    let view = view.clone();
                    git.update(cx, |git, cx| git.change_mark_hover(view, line, cx));
                }
                EditorEvent::BlameCommit(id) => {
                    let id = id.clone();
                    let workspace = runner.clone();
                    let handle = window.window_handle();
                    // The click lands while the window is being updated: the log opens
                    // after that update is over.
                    cx.defer(move |cx| {
                        handle
                            .update(cx, |_, window, cx| {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        crate::git_log::show_commit(&id, workspace, window, cx)
                                    })
                                    .ok();
                            })
                            .ok();
                    });
                }
                EditorEvent::RunMark(line) => {
                    let view = view.clone();
                    runner
                        .update(cx, |workspace, cx| {
                            marks::run_mark_clicked(&view, *line as usize, workspace, window, cx)
                        })
                        .ok();
                }
                EditorEvent::EndpointMark(line) => {
                    let view = view.clone();
                    runner
                        .update(cx, |workspace, cx| {
                            marks::show_endpoint_menu(&view, *line as usize, workspace, window, cx)
                        })
                        .ok();
                }
                EditorEvent::PlaceMark(line) => {
                    let view = view.clone();
                    runner
                        .update(cx, |workspace, cx| {
                            crate::beans::show_place_menu(
                                &view,
                                *line as usize,
                                workspace,
                                window,
                                cx,
                            )
                        })
                        .ok();
                }
                EditorEvent::ActionMark(line) => {
                    let view = view.clone();
                    runner
                        .update(cx, |workspace, cx| {
                            crate::kubernetes::show_document_menu(
                                &view,
                                *line as usize,
                                workspace,
                                window,
                                cx,
                            )
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
                EditorEvent::RunRequest(line) => {
                    runner
                        .update(cx, |workspace, cx| {
                            if !crate::database_panel::run_line(
                                &view,
                                *line as usize,
                                workspace,
                                window,
                                cx,
                            ) && !crate::jshell_panel::run_line(
                                &view,
                                *line as usize,
                                workspace,
                                window,
                                cx,
                            ) {
                                crate::http::run_line(&view, *line as usize, workspace, window, cx);
                            }
                        })
                        .ok();
                }
                EditorEvent::StopRequest => {
                    runner
                        .update(cx, |workspace, cx| {
                            crate::database_panel::stop_line(&view, workspace, cx);
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
                EditorEvent::GutterMenu { position, line } => {
                    crate::gutter_menu::show_gutter(
                        view.clone(),
                        runner.clone(),
                        *position,
                        *line,
                        cx,
                    );
                }
                EditorEvent::BlameMenu { position, line } => {
                    crate::gutter_menu::show_blame(
                        view.clone(),
                        runner.clone(),
                        *position,
                        *line,
                        cx,
                    );
                }
                EditorEvent::AddInlineWatch { line, expression } => {
                    let path = view.read(cx).path().to_path_buf();
                    let panel = cx
                        .try_global::<debug::ActiveDebug>()
                        .map(|panel| panel.0.clone());
                    if let Some(panel) = panel {
                        panel.update(cx, |panel, cx| {
                            panel.add_inline_watch(path, *line as u32 + 1, expression.clone(), cx)
                        });
                    }
                }
                EditorEvent::InlineWatchRemoved { line, expression } => {
                    let path = view.read(cx).path().to_path_buf();
                    let panel = cx
                        .try_global::<debug::ActiveDebug>()
                        .map(|panel| panel.0.clone());
                    if let Some(panel) = panel {
                        panel.update(cx, |panel, cx| {
                            panel.remove_inline_watch_at(&path, *line as u32 + 1, expression, cx)
                        });
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
                    workspace.notify_error(title, message, cx)
                })
                .ok();
        }
        EditorEvent::Unmappable(message) => {
            let message = message.clone();
            reports
                .update(cx, |workspace, cx| {
                    workspace.notify_warning_with_commands(
                        "Save File",
                        message,
                        vec![
                            ("save-utf8", "Save as UTF-8".into(), SAVE_AS_UTF8),
                            ("cancel", "Cancel".into(), SAVE_CANCEL),
                        ],
                        cx,
                    )
                })
                .ok();
        }
        EditorEvent::Status(message) => {
            let message = message.clone();
            reports
                .update(cx, |workspace, cx| {
                    workspace.set_language_status(Some(message), cx)
                })
                .ok();
        }
        EditorEvent::SaveNote(message) => {
            let message = message.clone();
            reports
                .update(cx, |workspace, cx| {
                    workspace.notify_warning("Actions on Save", message, cx)
                })
                .ok();
        }
        EditorEvent::StatusMessage(message) => {
            let message = message.clone();
            reports
                .update(cx, |workspace, cx| {
                    workspace.show_status_message(message, cx)
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
                    workspace.notify_warning("File changed on disk", text, cx)
                })
                .ok();
        }
        EditorEvent::Saved => {
            let path = view.read(cx).path().to_path_buf();
            crate::history::snapshot_file(&path, local_history::Cause::Saved, cx);
            crate::endpoints::editor_changed(&view, cx);
            crate::agents::refresh_marks(cx);
            if let Some(panel) = cx
                .try_global::<ActiveRunPanel>()
                .map(|panel| panel.0.clone())
            {
                panel.update(cx, |panel, cx| panel.project_file_saved(&path, cx));
            }
        }
        EditorEvent::BeforeOptimizeImports => {
            let path = view.read(cx).path().to_path_buf();
            let bytes = view.read(cx).file_bytes();
            crate::history::snapshot_bytes(
                &path,
                bytes,
                local_history::Cause::BeforeOptimizeImports,
                cx,
            );
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
        EditorEvent::ApplyChange { change, label } => {
            let change = change.clone();
            let label = label.clone();
            let workspace = reports.clone();
            let origin = view.clone();
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::changes::apply_from(
                                workspace,
                                change,
                                Some(origin),
                                label,
                                window,
                                cx,
                            )
                        })
                        .ok();
                })
                .ok();
        }
        EditorEvent::UndoRefactoring(id) => {
            let id = *id;
            reports
                .update(cx, |workspace, cx| {
                    crate::refactor_undo::undo(workspace, id, cx)
                })
                .ok();
        }
        EditorEvent::RedoRefactoring(id) => {
            let id = *id;
            reports
                .update(cx, |workspace, cx| {
                    crate::refactor_undo::redo(workspace, id, cx)
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
        EditorEvent::UsagesSearchStarted {
            search,
            name,
            in_file,
        } => {
            reports
                .update(cx, |workspace, cx| places::remember(&view, workspace, cx))
                .ok();
            let Some(hub) = cx.try_global::<ActiveUsages>() else {
                return;
            };
            let hub = hub.0.clone();
            let (search, name, in_file) = (*search, name.clone(), *in_file);
            hub.update(cx, |hub, cx| {
                hub.start_search(search, name, in_file, cx)
            });
            let workspace = reports.clone();
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| workspace.show_tool(USAGES_TOOL, window, cx))
                        .ok();
                })
                .ok();
        }
        EditorEvent::Usages { search, usages } => {
            let Some(hub) = cx.try_global::<ActiveUsages>() else {
                return;
            };
            let hub = hub.0.clone();
            let (search, usages) = (*search, usages.clone());
            hub.update(cx, |hub, cx| hub.set_usages_at(search, usages, cx));
        }
        EditorEvent::UsagesInFile { search, usages } => {
            let Some(hub) = cx.try_global::<ActiveUsages>() else {
                return;
            };
            let hub = hub.0.clone();
            let (search, usages) = (*search, usages.clone());
            hub.update(cx, |hub, cx| {
                hub.set_usages_in_file_at(search, usages, cx)
            });
        }
        EditorEvent::UsagesDetailed { search, usages } => {
            let Some(hub) = cx.try_global::<ActiveUsages>() else {
                return;
            };
            let hub = hub.0.clone();
            let (search, usages) = (*search, usages.clone());
            hub.update(cx, |hub, cx| hub.set_detailed_usages(search, usages, cx));
        }
        EditorEvent::UsagesDetailedInFile { search, usages } => {
            let Some(hub) = cx.try_global::<ActiveUsages>() else {
                return;
            };
            let hub = hub.0.clone();
            let (search, usages) = (*search, usages.clone());
            hub.update(cx, |hub, cx| {
                hub.set_detailed_usages_in_file(search, usages, cx)
            });
        }
        EditorEvent::UsagesWaiting {
            search,
            name,
            in_file,
            cancel,
        } => {
            let Some(hub) = cx.try_global::<ActiveUsages>() else {
                return;
            };
            let hub = hub.0.clone();
            let (search, name, in_file, cancel) =
                (*search, name.clone(), *in_file, cancel.clone());
            hub.update(cx, |hub, cx| {
                hub.wait_for_search(search, name, in_file, cancel, cx)
            });
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
    let hidden = view.clone();
    let (status, save, save_now) = (view.clone(), view.clone(), view.clone());
    let (caret, restore, format) = (view.clone(), view.clone(), view.clone());
    let indent = view.clone();
    let reload = view.clone();
    let preview = view.read(cx).preview_mode().is_some().then(|| {
        let read_view = view.clone();
        let write_view = view.clone();
        let saver = workspace.clone();
        PreviewTab {
            layout: Rc::new(move |cx: &App| match read_view.read(cx).preview_mode() {
                Some(PreviewMode::EditorOnly) => PreviewLayout::EditorOnly,
                Some(PreviewMode::PreviewOnly) => PreviewLayout::PreviewOnly,
                _ => PreviewLayout::Both,
            }),
            set_layout: Rc::new(move |layout, cx: &mut App| {
                let mode = match layout {
                    PreviewLayout::EditorOnly => PreviewMode::EditorOnly,
                    PreviewLayout::Both => PreviewMode::EditorAndPreview,
                    PreviewLayout::PreviewOnly => PreviewMode::PreviewOnly,
                };
                write_view.update(cx, |view, cx| view.set_preview_mode(mode, cx));
                // The window may be the one that is being updated, so the saved layout
                // hears of the change once that update is over.
                let saver = saver.clone();
                cx.defer(move |cx| {
                    saver
                        .update(cx, |workspace, cx| workspace.tab_layout_changed(cx))
                        .ok();
                });
            }),
        }
    });
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
        preview,
        bundle,
        can_close: None,
        on_hidden: Some(Rc::new(move |cx| {
            hidden.update(cx, |view, cx| view.dismiss_popups(cx));
        })),
    }
}
/// The factory the workspace opens files with.
pub fn factory(workspace: WeakEntity<Workspace>, state_dir: PathBuf) -> EditorFactory {
    Rc::new(move |path, window, cx| open(&workspace, &state_dir, path, window, cx))
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

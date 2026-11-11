//! The polish of the VCS windows without a window: the diff of a shelf file, popups at
//! the caret and the tree row, the mask of Find in Files, Save As, change lists with
//! grouping and the analysis before a commit. Every repository is a scratch folder and
//! the language answers from memory.

use std::fs;
use std::path::Path;
use std::sync::Arc;

use lang_api::{DiagnosticEntry, Severity, TextPoint, TextSpan};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, Module, ProjectModel};
use ui_kit::gpui;
use ui_kit::gpui::{Modifiers, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{point, px};
use workspace::{COMMIT_TOOL, INSPECTION_TOOL, Workspace};

use crate::diff_view::DiffView;
use crate::finder::ActiveFinder;
use crate::inspect::ActiveInspect;
use crate::project::ActiveProjectPanel;
use crate::run::ProjectSetup;
use crate::script::{ActiveCommitPanel, ActiveRunPanel};
use crate::window_tests::{
    active_path, bounds_of, buffer_of, click, draw, editor_of, open_path, open_window,
    open_window_with, press, sample_project, scratch, type_text,
};
use crate::window_tests_git::{
    git, git_state, has_selector, repository_with_origin, wait_for, wait_for_repository,
};
use crate::window_tests_lang::{FakeLanguage, open_window_with as open_language_window};

fn commit_panel(cx: &TestAppContext) -> gpui::Entity<crate::commit::CommitPanel> {
    cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone())
}

fn finder(cx: &TestAppContext) -> gpui::Entity<crate::finder::Finder> {
    cx.read(|cx| {
        cx.global::<ActiveFinder>()
            .0
            .clone()
            .expect("the finder is open")
    })
}

fn focus<T: 'static>(cx: &mut TestAppContext, window: WindowHandle<T>, handle: gpui::FocusHandle) {
    window
        .update(cx, |_, window, cx| window.focus(&handle, cx))
        .unwrap();
    cx.run_until_parked();
}

fn show_commit(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn focus_panel(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    panel: &gpui::Entity<crate::commit::CommitPanel>,
) {
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    draw(cx, window);
}

fn click_selector(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) {
    draw(cx, window);
    let selector: &'static str = Box::leak(selector.to_string().into_boxed_str());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"));
    visual.simulate_click(bounds.center(), Modifiers::default());
    cx.run_until_parked();
}

fn click_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>, label: &str) {
    wait_for(cx, "the menu row", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace.menu_entries().is_some_and(|entries| {
                    entries.iter().any(|entry| entry.label.as_ref() == label)
                })
            })
            .unwrap()
    });
    let index = window
        .read_with(cx, |workspace, _| {
            workspace.menu_entries().and_then(|entries| {
                entries
                    .iter()
                    .position(|entry| entry.label.as_ref() == label)
            })
        })
        .unwrap()
        .unwrap_or_else(|| panic!("no menu row {label}"));
    click_selector(cx, window, &format!("menu-{index}"));
}

fn read_text(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

fn commit_file(root: &Path, path: &str, text: &str, message: &str) {
    let file = root.join(path);
    if let Some(parent) = file.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(file, text).unwrap();
    let git = git();
    git.run(root, ["add", path]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
}

/// A build tool that answers at once with the modules of the scratch project.
struct ModulesTool(ProjectModel);

impl BuildSystem for ModulesTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.0.clone())
    }

    fn sync(&self, _: &Path, _: &base::Cancel) -> base::Result<ProjectModel> {
        Ok(self.0.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }
}

/// Two modules, the root and `src`, so that the Commit window groups files by module.
fn modules_setup(root: &Path) -> ProjectSetup {
    let model = ProjectModel {
        root: root.to_path_buf(),
        tool: "fixture".into(),
        resolved: true,
        modules: vec![
            Module {
                id: "app".into(),
                name: "app".into(),
                dir: root.to_path_buf(),
                units: Vec::new(),
            },
            Module {
                id: "tests".into(),
                name: "tests".into(),
                dir: root.join("src"),
                units: Vec::new(),
            },
        ],
        ..ProjectModel::default()
    };
    ProjectSetup {
        tool: Some(Arc::new(ModulesTool(model))),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("polish-changelist-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

fn diff_view(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> gpui::Entity<DiffView> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .overlay_view()
                .and_then(|view| view.downcast::<DiffView>().ok())
        })
        .unwrap()
        .expect("the diff popup is open")
}

/// Makes a change in the editor, keeps the buffer unsaved and shelves it by the button.
fn shelve_the_change(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
    message: &str,
) {
    let path = root.join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.replace_text(0..5, "ALPHA", cx);
        assert!(view.save_blocking(cx).is_none());
    });
    let state = git_state(cx);
    state.update(cx, |state, cx| state.refresh(cx));
    wait_for(cx, "the changed file", |cx| {
        state.read_with(cx, |state, _| !state.status().files.is_empty())
    });
    show_commit(cx, window);
    let panel = commit_panel(cx);
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    type_text(cx, window, message);
    click(cx, window, "commit-shelve");
    wait_for(cx, "the shelf", |cx| {
        panel.read_with(cx, |panel, _| panel.shelves().len() == 1)
    });
    wait_for(cx, "the buffer to read the disk", |cx| {
        buffer_of(cx, &view) == "alpha\nbeta\n"
    });
}

#[ui_kit::gpui::test]
fn a_shelf_file_opens_its_head_diff_by_ctrl_d_and_the_preview_colors_it(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("polish-shelf-diff");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    shelve_the_change(cx, window, &root, "my work");
    let panel = commit_panel(cx);
    click(cx, window, "shelf-tab");
    wait_for(cx, "the shelf tree", |cx| {
        has_selector(cx, window, "shelf-row-0")
    });
    click(cx, window, "shelf-row-0");
    wait_for(cx, "the file of the shelf", |cx| {
        has_selector(cx, window, "shelf-file-0")
    });
    click(cx, window, "shelf-file-0");
    wait_for(cx, "the preview of the shelf diff", |cx| {
        panel.read_with(cx, |panel, _| panel.shelf_preview_hunks() == 1)
    });

    focus_panel(cx, window, &panel);
    press(cx, window, "ctrl-d");
    wait_for(cx, "the diff window", |cx| {
        has_selector(cx, window, "diff-title")
    });
    let diff = diff_view(cx, window);
    assert_eq!(
        diff.read_with(cx, |diff, _| diff.path().to_path_buf()),
        root.join("main.txt")
    );
    assert_eq!(diff.read_with(cx, |diff, _| diff.hunk_count()), 1);
}

#[ui_kit::gpui::test]
fn related_files_open_at_the_caret_or_at_the_selected_tree_row(cx: &mut TestAppContext) {
    let root = sample_project("polish-anchor");
    let class = root.join("src").join("Foo.txt");
    let test = root.join("src").join("FooTest.txt");
    fs::write(&class, "class Foo {}\n").unwrap();
    fs::write(&test, "class FooTest {}\n").unwrap();
    let window = open_window(cx, root);
    open_path(cx, window, &class);
    let view = editor_of(cx, window, &class);
    view.update(cx, |view, cx| view.restore_caret(0, 0, cx));
    draw(cx, window);
    let body = bounds_of(cx, window, "editor-body-0");
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_move(point(px(1.), px(1.)), None, Modifiers::default());
    cx.run_until_parked();

    wait_for(cx, "the related menu from the caret", |cx| {
        press(cx, window, "ctrl-alt-home");
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_some())
            .unwrap()
    });
    let position = window
        .read_with(cx, |workspace, _| workspace.menu_position())
        .unwrap()
        .expect("the menu has a place");
    assert!(
        position.y > body.top() && position.y < body.bottom(),
        "the menu is not at the caret: {position:?} in {body:?}"
    );
    assert!(
        position != point(px(1.), px(1.)),
        "the menu stayed at the pointer"
    );
    press(cx, window, "escape");

    let panel = cx.read(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    panel.update(cx, |panel, cx| panel.select_path(&class, cx));
    crate::window_tests_menus::show_project(cx, window);
    window
        .update(cx, |_, window, cx| {
            window.focus(&panel.read(cx).focus(), cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    let row = bounds_of(cx, window, "tree-row-Foo.txt");
    press(cx, window, "ctrl-alt-home");
    wait_for(cx, "the related menu from the tree", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.menu_entries().is_some())
            .unwrap()
    });
    let position = window
        .read_with(cx, |workspace, _| workspace.menu_position())
        .unwrap()
        .expect("the menu has a place");
    assert!(
        position.y >= row.top() - px(2.) && position.y <= row.bottom() + px(2.),
        "the menu is not at the row: {position:?} in {row:?}"
    );
}

#[ui_kit::gpui::test]
fn the_find_in_files_mask_comes_from_the_language_of_the_project(cx: &mut TestAppContext) {
    let root = sample_project("polish-mask-language");
    let window = open_window(cx, root);
    cx.run_until_parked();
    cx.update(|cx| {
        cx.set_global(editor_view::Languages {
            resolve: |path| {
                path.extension()
                    .is_some_and(|extension| extension == "txt")
                    .then(|| {
                        let mut config = dockerfile_syntax::config();
                        config.file_mask = "*.kt !*Test.kt";
                        config
                    })
            },
            templates: |_| None,
        });
    });
    press(cx, window, "ctrl-shift-f");
    wait_for(cx, "the finder", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveFinder>()
                .is_some_and(|global| global.0.is_some())
        })
    });
    type_text(cx, window, "a");
    let finder = finder(cx);
    assert_eq!(
        finder.read_with(cx, |finder, _| finder.mask_hint().to_owned()),
        "*.kt !*Test.kt"
    );
}

#[ui_kit::gpui::test]
fn the_find_in_files_mask_falls_back_to_the_common_extension(cx: &mut TestAppContext) {
    let root = scratch("polish-mask-extension");
    fs::write(root.join("a.txt"), "one\n").unwrap();
    fs::write(root.join("b.txt"), "two\n").unwrap();
    fs::write(root.join("c.md"), "three\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    press(cx, window, "ctrl-shift-f");
    wait_for(cx, "the finder", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveFinder>()
                .is_some_and(|global| global.0.is_some())
        })
    });
    type_text(cx, window, "a");
    let finder = finder(cx);
    assert_eq!(
        finder.read_with(cx, |finder, _| finder.mask_hint().to_owned()),
        "*.txt",
        "the most common extension of the project is used"
    );
}

#[ui_kit::gpui::test]
fn save_as_is_in_the_file_menu_and_keeps_the_old_file_open(cx: &mut TestAppContext) {
    let root = scratch("polish-save-as");
    fs::create_dir_all(root.join("src")).unwrap();
    let source = root.join("main.txt");
    fs::write(&source, "alpha\nbeta\n").unwrap();
    let window = open_window(cx, root.clone());
    draw(cx, window);
    open_path(cx, window, &source);
    let view = editor_of(cx, window, &source);
    view.update(cx, |view, cx| view.replace_text(0..5, "ALPHA", cx));

    let has_row = window
        .update(cx, |workspace, _, cx| {
            workspace
                .main_menu(cx)
                .into_iter()
                .find(|entry| entry.label.as_ref() == "File")
                .is_some_and(|file| {
                    file.submenu
                        .iter()
                        .any(|entry| entry.label.as_ref() == "Save As...")
                })
        })
        .unwrap();
    assert!(has_row, "File has no Save As row");

    let target = root.join("src").join("copy.txt");
    press(cx, window, "ctrl-shift-s");
    cx.simulate_new_path_selection({
        let target = target.clone();
        move |_| Some(target.clone())
    });
    wait_for(cx, "the saved file", |_| target.is_file());
    wait_for(cx, "the new tab", |cx| {
        active_path(cx, window).as_deref() == Some(target.as_path())
    });
    assert_eq!(read_text(&target), "ALPHA\nbeta\n");
    assert_eq!(
        buffer_of(cx, &view),
        "ALPHA\nbeta\n",
        "the old file keeps its unsaved text"
    );
    assert!(
        view.read_with(cx, |view, _| view.is_dirty()),
        "the old file is still unsaved"
    );
    assert_eq!(
        read_text(&source),
        "alpha\nbeta\n",
        "the old file on disk stays"
    );
    let panel = cx.read(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.selected_path().map(Path::to_path_buf)),
        Some(target),
        "the tree shows the saved file of the project"
    );
}

#[ui_kit::gpui::test]
#[allow(clippy::permissions_set_readonly_false)]
fn a_failed_save_as_reports_and_leaves_the_target_alone(cx: &mut TestAppContext) {
    let root = scratch("polish-save-as-error");
    let source = root.join("main.txt");
    fs::write(&source, "alpha\nbeta\n").unwrap();
    let folder = root.join("locked");
    fs::create_dir_all(&folder).unwrap();
    let target = folder.join("copy.txt");
    fs::write(&target, "old\n").unwrap();
    let mut permissions = fs::metadata(&target).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&target, permissions).unwrap();
    let mut folder_permissions = fs::metadata(&folder).unwrap().permissions();
    folder_permissions.set_readonly(true);
    fs::set_permissions(&folder, folder_permissions).unwrap();

    let window = open_window(cx, root.clone());
    draw(cx, window);
    open_path(cx, window, &source);
    let notifications = window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap();
    press(cx, window, "ctrl-shift-s");
    cx.simulate_new_path_selection({
        let target = target.clone();
        move |_| Some(target.clone())
    });
    wait_for(cx, "the failed save", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.notification_serial())
            .unwrap()
            > notifications
    });
    assert_eq!(
        read_text(&target),
        "old\n",
        "the read-only file is untouched"
    );

    let mut permissions = fs::metadata(&target).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(&target, permissions).unwrap();
    let mut permissions = fs::metadata(&folder).unwrap().permissions();
    permissions.set_readonly(false);
    fs::set_permissions(&folder, permissions).unwrap();
}

#[ui_kit::gpui::test]
fn change_lists_grouping_and_the_commit_of_the_active_list_in_one_window(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("polish-changelists");
    commit_file(&root, "src/second.txt", "one\n", "second");
    let window = open_window_with(cx, root.clone(), modules_setup(&root));
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    let runs = cx.read(|cx| cx.global::<ActiveRunPanel>().0.clone());
    wait_for(cx, "the modules of the project", |cx| {
        runs.read_with(cx, |runs, _| !runs.modules().is_empty())
    });
    fs::write(root.join("main.txt"), "ALPHA\nbeta\n").unwrap();
    fs::write(root.join("src").join("second.txt"), "ONE\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));
    show_commit(cx, window);
    let panel = commit_panel(cx);
    wait_for(cx, "the changed files", |cx| {
        panel.read_with(cx, |panel, _| panel.active_changelist() == "Changes")
            && has_selector(cx, window, "commit-row-1")
    });

    // A new list becomes active.
    click(cx, window, "changelist-active");
    click_menu(cx, window, "New Changelist...");
    type_text(cx, window, "Feature");
    press(cx, window, "enter");
    wait_for(cx, "the new active list", |cx| {
        panel.read_with(cx, |panel, _| panel.active_changelist() == "Feature")
    });

    // The file under the cursor moves to Changes by Alt+Shift+M.
    let main_index = state.read_with(cx, |state, _| {
        state
            .status()
            .files
            .iter()
            .position(|file| file.path == "main.txt")
            .expect("main.txt is changed")
    });
    click_selector(cx, window, &format!("commit-row-{main_index}"));
    focus_panel(cx, window, &panel);
    press(cx, window, "alt-shift-m");
    click_menu(cx, window, "Changes");
    wait_for(cx, "the moved file", |cx| {
        panel.read_with(cx, |panel, _| panel.assigned_to_another_list("main.txt"))
    });

    // Ctrl+Space makes Changes active, then Feature again.
    focus_panel(cx, window, &panel);
    press(cx, window, "ctrl-space");
    click_menu(cx, window, "Changes");
    wait_for(cx, "Changes as the active list", |cx| {
        panel.read_with(cx, |panel, _| panel.active_changelist() == "Changes")
    });
    focus_panel(cx, window, &panel);
    press(cx, window, "ctrl-space");
    click_menu(cx, window, "Feature");
    wait_for(cx, "Feature as the active list", |cx| {
        panel.read_with(cx, |panel, _| panel.active_changelist() == "Feature")
    });

    // Ctrl+Alt+P groups by folder, Ctrl+Alt+M by module in the same window.
    focus_panel(cx, window, &panel);
    press(cx, window, "ctrl-alt-p");
    assert_eq!(
        panel.read_with(cx, |panel, cx| panel.group_of_path("src/second.txt", cx)),
        "src"
    );
    press(cx, window, "ctrl-alt-m");
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.grouping()),
        crate::changelists::Grouping::Module
    );
    assert_eq!(
        panel.read_with(cx, |panel, cx| panel.group_of_path("main.txt", cx)),
        "app"
    );
    assert_eq!(
        panel.read_with(cx, |panel, cx| panel.group_of_path("src/second.txt", cx)),
        "tests"
    );

    // The commit takes only the active list, and a file without changes leaves its list.
    focus_panel(cx, window, &panel);
    type_text(cx, window, "only-feature");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the commit of the active list", |cx| {
        state.read_with(cx, |state, _| state.busy().is_none())
            && state.read_with(cx, |state, _| state.status().files.len()) == 1
    });
    let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
    let log = repo.log(1, 0).unwrap();
    assert_eq!(log[0].subject, "only-feature");
    let files = repo.commit_files(&log[0].id).unwrap();
    assert_eq!(files, ["src/second.txt"], "{files:?}");
    assert!(
        panel.read_with(cx, |panel, _| panel.assigned_to_another_list("main.txt")),
        "main.txt stays in Changes"
    );

    git().run(&root, ["checkout", "--", "main.txt"]).unwrap();
    state.update(cx, |state, cx| state.refresh(cx));
    wait_for(cx, "the clean repository", |cx| {
        state.read_with(cx, |state, _| state.status().files.is_empty())
    });
    assert!(
        !panel.read_with(cx, |panel, _| panel.assigned_to_another_list("main.txt")),
        "a file without changes leaves its list"
    );
}

fn error_at_start() -> DiagnosticEntry {
    DiagnosticEntry {
        span: TextSpan {
            start: TextPoint {
                line: 0,
                character: 0,
            },
            end: TextPoint {
                line: 0,
                character: 5,
            },
        },
        severity: Severity::Error,
        message: "the value is never used".to_owned(),
        unnecessary: false,
    }
}

#[ui_kit::gpui::test]
fn a_commit_with_errors_from_the_analysis_asks_review_or_commit_anyway(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("polish-analysis");
    let language = FakeLanguage::new();
    language.set_diagnostics(vec![error_at_start()]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::write(root.join("main.txt"), "ALPHA\nbeta\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));
    show_commit(cx, window);
    let panel = commit_panel(cx);
    wait_for(cx, "the changed file", |cx| {
        has_selector(cx, window, "commit-row-0")
    });

    focus_panel(cx, window, &panel);
    press(cx, window, "ctrl-o");
    click_menu(cx, window, "Run code analysis");
    assert!(panel.read_with(cx, |panel, _| panel.analysis_enabled()));

    focus_panel(cx, window, &panel);
    type_text(cx, window, "with analysis");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the analysis question", |cx| {
        has_selector(cx, window, "analysis-dialog")
    });

    click(cx, window, "analysis-review");
    wait_for(cx, "the inspection results", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(INSPECTION_TOOL))
            .unwrap()
    });
    assert!(
        state.read_with(cx, |state, _| state.status().files.len()) == 1,
        "Review does not commit"
    );
    let hub = cx.read(|cx| cx.global::<ActiveInspect>().0.clone());
    wait_for(cx, "the findings of the analysis", |cx| {
        hub.read_with(cx, |hub, _| !hub.findings().is_empty())
    });

    // The second try commits anyway.
    show_commit(cx, window);
    focus_panel(cx, window, &panel);
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the analysis question again", |cx| {
        has_selector(cx, window, "analysis-dialog")
    });
    click(cx, window, "analysis-commit");
    wait_for(cx, "the commit after the question", |cx| {
        state.read_with(cx, |state, _| {
            state.busy().is_none() && state.status().files.is_empty()
        })
    });
    let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
    assert_eq!(repo.log(1, 0).unwrap()[0].subject, "with analysis");
}

#[ui_kit::gpui::test]
fn a_commit_without_analysis_errors_goes_through_at_once(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("polish-analysis-clean");
    let language = FakeLanguage::new();
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    fs::write(root.join("main.txt"), "ALPHA\nbeta\n").unwrap();
    state.update(cx, |state, cx| state.refresh(cx));
    show_commit(cx, window);
    let panel = commit_panel(cx);
    wait_for(cx, "the changed file", |cx| {
        has_selector(cx, window, "commit-row-0")
    });

    focus_panel(cx, window, &panel);
    press(cx, window, "ctrl-o");
    click_menu(cx, window, "Run code analysis");
    assert!(panel.read_with(cx, |panel, _| panel.analysis_enabled()));
    focus_panel(cx, window, &panel);
    type_text(cx, window, "clean analysis");
    press(cx, window, "ctrl-enter");
    wait_for(cx, "the commit without a question", |cx| {
        state.read_with(cx, |state, _| {
            state.busy().is_none() && state.status().files.is_empty()
        })
    });
    assert!(
        !has_selector(cx, window, "analysis-dialog"),
        "a clean analysis asks nothing"
    );
    let repo = state.read_with(cx, |state, _| state.repo()).unwrap();
    assert_eq!(repo.log(1, 0).unwrap()[0].subject, "clean analysis");
}

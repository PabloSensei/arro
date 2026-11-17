//! The keys of the VCS, build and debugger actions of M11 without a window: diffs on a
//! scratch repository, the Commit window, the branch popup, Compile and the debugger on
//! the mock adapter. The ignored test at the end needs `IDE_CORPUS` and the real adapter.

use std::fs;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use base::Cancel;
use dap_client::DebugState;
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, Module, ProjectModel, Unit};
use runner::{RunPlan, TestListen, TestProvider, TestRequest};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{COMMIT_TOOL, Workspace};

use crate::diff_view::DiffView;
use crate::project::ActiveProjectPanel;
use crate::run::{ProjectSetup, RunPanel};
use crate::script::{ActiveCommitPanel, ActiveRunPanel};
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_window, open_window_with, press,
    sample_project, scratch, type_text, with_window,
};
use crate::window_tests_debug_actions::{
    debug_panel, end_session, last_breakpoint_lines, setup as debug_setup, shown, stop_at_start,
};
use crate::window_tests_git::{git, git_state, repository_with_origin, wait_for as wait_git};

/// How long a test waits for a background step.
const LIMIT: Duration = Duration::from_secs(30);

fn model(root: &Path) -> ProjectModel {
    ProjectModel {
        root: root.to_path_buf(),
        tool: "fixture".into(),
        resolved: true,
        modules: vec![Module {
            id: "demo".into(),
            name: "demo".into(),
            dir: root.to_path_buf(),
            units: vec![Unit {
                name: "main".into(),
                output_dirs: vec![root.join("classes")],
                ..Unit::default()
            }],
        }],
        ..ProjectModel::default()
    }
}

fn setup(tool: Option<Arc<dyn BuildSystem>>, tests: Option<Arc<dyn TestProvider>>) -> ProjectSetup {
    ProjectSetup {
        tool,
        providers: Vec::new(),
        tests,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("vcs-keys-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

fn run_panel(cx: &TestAppContext) -> gpui::Entity<RunPanel> {
    cx.read(|cx| cx.global::<ActiveRunPanel>().0.clone())
}

fn commit_panel(cx: &TestAppContext) -> gpui::Entity<crate::commit::CommitPanel> {
    cx.read(|cx| cx.global::<ActiveCommitPanel>().0.clone())
}

fn focus<T: 'static>(cx: &mut TestAppContext, window: WindowHandle<T>, handle: gpui::FocusHandle) {
    window
        .update(cx, |_, window, cx| window.focus(&handle, cx))
        .unwrap();
    cx.run_until_parked();
}

fn wait_idle(cx: &mut TestAppContext, panel: &gpui::Entity<RunPanel>) {
    wait_git(cx, "the build to end", |cx| {
        panel.read_with(cx, |panel, _| panel.idle())
    });
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

/// Writes `text` to `path` and commits it with `message`.
fn commit_file(root: &Path, path: &str, text: &str, message: &str) {
    fs::write(root.join(path), text).unwrap();
    let git = git();
    git.run(root, ["add", path]).unwrap();
    git.run(root, ["commit", "-m", message]).unwrap();
}

#[ui_kit::gpui::test]
fn diff_keys_walk_the_hunks_and_the_files(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("vcs-keys-diff");
    commit_file(&root, "second.txt", "one\ntwo\n", "second");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let git = git_state(cx);
    wait_git(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });
    fs::write(root.join("main.txt"), "ALPHA\nbeta\nGAMMA\n").unwrap();
    fs::write(root.join("second.txt"), "ONE\ntwo\n").unwrap();

    let files = vec![root.join("main.txt"), root.join("second.txt")];
    let weak = window.root(cx).unwrap().downgrade();
    with_window(cx, window, move |window, cx| {
        DiffView::open_list(git.clone(), weak, files, 0, window, cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    let view = diff_view(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.path().to_path_buf()),
        root.join("main.txt")
    );
    assert_eq!(view.read_with(cx, |view, _| view.hunk_count()), 2);

    press(cx, window, "f7");
    assert_eq!(view.read_with(cx, |view, _| view.hunk_index()), 1);
    // At the last hunk F7 goes to the next file.
    press(cx, window, "f7");
    assert_eq!(
        view.read_with(cx, |view, _| view.path().to_path_buf()),
        root.join("second.txt")
    );
    assert_eq!(view.read_with(cx, |view, _| view.hunk_index()), 0);

    // Shift+F7 returns to the last hunk of the previous file.
    press(cx, window, "shift-f7");
    assert_eq!(
        view.read_with(cx, |view, _| view.path().to_path_buf()),
        root.join("main.txt")
    );
    assert_eq!(view.read_with(cx, |view, _| view.hunk_index()), 1);

    // The changes-list keys walk the files only.
    press(cx, window, "alt-shift-right");
    assert_eq!(
        view.read_with(cx, |view, _| view.path().to_path_buf()),
        root.join("second.txt")
    );
    press(cx, window, "alt-shift-left");
    assert_eq!(
        view.read_with(cx, |view, _| view.path().to_path_buf()),
        root.join("main.txt")
    );
    assert_eq!(view.read_with(cx, |view, _| view.hunk_index()), 1);
}

#[ui_kit::gpui::test]
fn applying_the_left_side_takes_the_change_back(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("vcs-keys-apply");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let git = git_state(cx);
    wait_git(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });
    let path = root.join("main.txt");
    fs::write(&path, "ALPHA\nbeta\n").unwrap();
    let weak = window.root(cx).unwrap().downgrade();
    let shown_path = path.clone();
    with_window(cx, window, move |window, cx| {
        DiffView::open(git.clone(), weak, shown_path, window, cx);
    });
    cx.run_until_parked();
    draw(cx, window);
    let view = diff_view(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.hunk_count()), 1);

    press(cx, window, "ctrl-alt-r");

    wait_git(cx, "the hunk to go back", |_| {
        fs::read_to_string(&path).unwrap() == "alpha\nbeta\n"
    });
    assert_eq!(view.read_with(cx, |view, _| view.hunk_count()), 0);
}

#[ui_kit::gpui::test]
fn ctrl_d_in_the_tree_compares_the_file_with_the_clipboard(cx: &mut TestAppContext) {
    let (root, window) = crate::window_tests::open_sample_window(cx, "vcs-keys-compare");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    panel.update(cx, |panel, cx| panel.select_path(&path, cx));
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);
    cx.update(|cx| {
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string("ALPHA\nbeta\n".into()))
    });

    press(cx, window, "ctrl-d");
    draw(cx, window);
    let view = diff_view(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.hunk_count()), 1);
    assert_eq!(
        view.read_with(cx, |view, _| view.path().to_path_buf()),
        path
    );
}

#[ui_kit::gpui::test]
fn ctrl_d_on_two_tree_files_compares_them(cx: &mut TestAppContext) {
    let root = sample_project("vcs-keys-compare-two");
    let left = root.join("src").join("main.txt");
    let right = root.join("src").join("other.txt");
    fs::write(
        &left,
        "alpha
beta
",
    )
    .unwrap();
    fs::write(
        &right,
        "ALPHA
beta
",
    )
    .unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    panel.update(cx, |panel, cx| panel.select_path(&left, cx));
    cx.run_until_parked();
    panel.update(cx, |panel, cx| {
        let index = panel
            .tree()
            .index_of(&right)
            .expect("the second file has a row");
        panel.toggle_selection(index, cx);
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.selected_paths().len()),
        2
    );
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);

    press(cx, window, "ctrl-d");
    draw(cx, window);
    let view = diff_view(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.hunk_count()), 1);
    assert_eq!(
        view.read_with(cx, |view, _| view.path().to_path_buf()),
        right
    );
}

#[ui_kit::gpui::test]
fn changes_view_revert_rolls_the_ticked_files_back_after_a_confirmation(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("vcs-keys-revert");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let git = git_state(cx);
    wait_git(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let untracked = root.join("fresh.txt");
    fs::write(root.join("main.txt"), "ALPHA\nbeta\n").unwrap();
    fs::write(&untracked, "new\n").unwrap();
    git.update(cx, |git, cx| git.refresh(cx));
    wait_git(cx, "the changed files", |cx| {
        git.read_with(cx, |git, _| git.status().files.len()) >= 2
    });
    let panel = commit_panel(cx);
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);

    press(cx, window, "ctrl-alt-z");
    draw(cx, window);
    assert!(
        crate::window_tests::bounds_of(cx, window, "prompt")
            .size
            .height
            > ui_kit::px(0.),
        "the rollback asks first"
    );
    press(cx, window, "enter");
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.has_overlay())
            .unwrap(),
        "the confirmation closes the prompt"
    );

    wait_git(cx, "the file to go back", |cx| {
        if let Some(error) = git.read_with(cx, |git, _| git.last_error().map(str::to_owned)) {
            panic!("Rollback failed: {error}");
        }
        fs::read_to_string(root.join("main.txt"))
            .unwrap()
            .replace("\r\n", "\n")
            == "alpha\nbeta\n"
    });
    // As in IDEA, Rollback keeps a file the last commit does not have.
    assert!(untracked.exists(), "the untracked file stays on disk");
}

#[ui_kit::gpui::test]
fn amend_takes_the_last_message_and_rewrites_the_commit(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("vcs-keys-amend");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let git = git_state(cx);
    wait_git(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let panel = commit_panel(cx);
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);

    press(cx, window, "alt-m");
    wait_git(cx, "the message of the last commit", |cx| {
        panel.read_with(cx, |panel, _| panel.amending() && panel.message() == "base")
    });

    fs::write(root.join("main.txt"), "ALPHA\nbeta\n").unwrap();
    git.update(cx, |git, cx| git.refresh(cx));
    wait_git(cx, "the changed file", |cx| {
        git.read_with(cx, |git, _| !git.status().files.is_empty())
    });
    press(cx, window, "ctrl-enter");

    wait_git(cx, "the amended commit", |cx| {
        let repo = git.read_with(cx, |git, _| git.repo()).unwrap();
        let log = repo.log(10, 0).unwrap();
        log.len() == 1 && log[0].subject == "base" && repo.status().unwrap().files.is_empty()
    });
}

#[ui_kit::gpui::test]
fn message_history_fills_the_field_from_an_earlier_commit(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("vcs-keys-messages");
    commit_file(&root, "second.txt", "one\n", "second message");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let git = git_state(cx);
    wait_git(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(COMMIT_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let panel = commit_panel(cx);
    let handle = panel.read_with(cx, |panel, _| panel.focus());
    focus(cx, window, handle);

    press(cx, window, "ctrl-m");
    draw(cx, window);
    let labels: Vec<String> = window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap();
    assert!(
        labels.iter().any(|label| label == "second message"),
        "{labels:?}"
    );
    click(cx, window, "menu-0");
    wait_git(cx, "the message to be filled", |cx| {
        panel.read_with(cx, |panel, _| panel.message() == "second message")
    });
}

#[ui_kit::gpui::test]
fn the_branch_popup_creates_and_renames_a_branch_by_its_keys(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("vcs-keys-branches");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let git = git_state(cx);
    wait_git(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });
    wait_git(cx, "the branch", |cx| current_branch(&git, cx).is_some());

    git.update(cx, |git, cx| {
        git.show_branches(ui_kit::point(ui_kit::px(200.), ui_kit::px(200.)), cx)
    });
    wait_git(cx, "the branch menu", |cx| {
        VisualTestContext::from_window(window.into(), cx)
            .debug_bounds("menu-2")
            .is_some()
    });
    press(cx, window, "ctrl-alt-n");
    draw(cx, window);
    assert!(
        crate::window_tests::bounds_of(cx, window, "prompt")
            .size
            .height
            > ui_kit::px(0.),
        "the new branch asks for a name"
    );
    type_text(cx, window, "feature");
    press(cx, window, "enter");
    wait_git(cx, "the new branch", |cx| {
        current_branch(&git, cx).as_deref() == Some("feature")
    });

    git.update(cx, |git, cx| {
        git.show_branches(ui_kit::point(ui_kit::px(200.), ui_kit::px(200.)), cx)
    });
    wait_git(cx, "the branch menu", |cx| {
        VisualTestContext::from_window(window.into(), cx)
            .debug_bounds("menu-2")
            .is_some()
    });
    press(cx, window, "f2");
    draw(cx, window);
    assert!(
        crate::window_tests::bounds_of(cx, window, "prompt")
            .size
            .height
            > ui_kit::px(0.),
        "the rename asks for a name"
    );
    press(cx, window, "ctrl-a");
    type_text(cx, window, "renamed");
    press(cx, window, "enter");
    wait_git(cx, "the renamed branch", |cx| {
        current_branch(&git, cx).as_deref() == Some("renamed")
    });
}

fn current_branch(git: &gpui::Entity<crate::git::GitState>, cx: &TestAppContext) -> Option<String> {
    let repo = git.read_with(cx, |git, _| git.repo())?;
    repo.branches()
        .ok()?
        .into_iter()
        .find(|branch| branch.current)
        .map(|branch| branch.name)
}

#[ui_kit::gpui::test]
fn the_vcs_quick_list_offers_the_operations(cx: &mut TestAppContext) {
    let (root, _bare, _git) = repository_with_origin("vcs-keys-quick-list");
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    let git = git_state(cx);
    wait_git(cx, "the repository", |cx| {
        git.read_with(cx, |git, _| git.repo().is_some())
    });

    press(cx, window, "alt-`");
    draw(cx, window);
    let labels: Vec<String> = window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap();
    for wanted in [
        "Commit...",
        "Update Project",
        "Push...",
        "Pull",
        "Branches...",
        "Show History",
        "Rollback...",
        "Stash Changes...",
    ] {
        assert!(
            labels.iter().any(|label| label == wanted),
            "{wanted}: {labels:?}"
        );
    }
}

/// A build tool that records the module Compile was asked for.
struct CompileTool {
    model: ProjectModel,
    compiled: Arc<Mutex<Vec<Option<String>>>>,
}

impl BuildSystem for CompileTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }

    fn build(&self, _: &Path, _: bool) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }

    fn compile(&self, _: &Path, module: Option<&Module>) -> base::Result<ProcessSpec> {
        self.compiled
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .push(module.map(|module| module.id.clone()));
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }
}

#[ui_kit::gpui::test]
fn compile_compiles_the_module_of_the_active_file(cx: &mut TestAppContext) {
    let root = sample_project("vcs-keys-compile");
    let compiled = Arc::new(Mutex::new(Vec::new()));
    let tool = Arc::new(CompileTool {
        model: model(&root),
        compiled: compiled.clone(),
    });
    let window = open_window_with(cx, root.clone(), setup(Some(tool), None));
    cx.run_until_parked();
    draw(cx, window);
    let panel = run_panel(cx);
    wait_git(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);

    press(cx, window, "ctrl-shift-f9");
    wait_git(cx, "the compile", |_| {
        !compiled
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .is_empty()
    });
    assert_eq!(
        compiled
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone(),
        vec![Some("demo".to_owned())]
    );
    wait_idle(cx, &panel);
}

/// A build tool whose build waits long enough for Stop Background Processes.
struct SlowTool {
    model: ProjectModel,
    builds: Arc<AtomicUsize>,
}

impl BuildSystem for SlowTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }

    fn build(&self, _: &Path, _: bool) -> base::Result<ProcessSpec> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "ping -n 10 127.0.0.1 >nul"]))
    }
}

#[ui_kit::gpui::test]
fn stop_background_processes_stops_the_build(cx: &mut TestAppContext) {
    let root = sample_project("vcs-keys-stop-build");
    let builds = Arc::new(AtomicUsize::new(0));
    let tool = Arc::new(SlowTool {
        model: model(&root),
        builds: builds.clone(),
    });
    let window = open_window_with(cx, root.clone(), setup(Some(tool), None));
    cx.run_until_parked();
    draw(cx, window);
    let panel = run_panel(cx);
    wait_git(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });

    press(cx, window, "ctrl-f9");
    wait_git(cx, "the build to start", |cx| {
        builds.load(Ordering::SeqCst) == 1 && !panel.read_with(cx, |panel, _| panel.idle())
    });
    assert_eq!(
        panel.read_with(cx, |panel, _| panel.background_tasks()),
        vec!["Build"]
    );

    press(cx, window, "ctrl-shift-f2");
    wait_idle(cx, &panel);
    assert!(
        panel
            .read_with(cx, |panel, _| panel.background_tasks())
            .is_empty()
    );
}

/// A test provider that counts its plans.
struct CountingTests {
    plans: Arc<AtomicUsize>,
}

impl TestProvider for CountingTests {
    fn plan(&self, _: &ProjectModel, _: &TestRequest, _: &TestListen) -> base::Result<RunPlan> {
        self.plans.fetch_add(1, Ordering::SeqCst);
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]),
            grace: Duration::from_secs(1),
        })
    }
}

#[ui_kit::gpui::test]
fn rerun_tests_starts_the_last_request_again(cx: &mut TestAppContext) {
    let root = sample_project("vcs-keys-rerun-tests");
    let plans = Arc::new(AtomicUsize::new(0));
    let tests = Arc::new(CountingTests {
        plans: plans.clone(),
    });
    let tool = Arc::new(CompileTool {
        model: model(&root),
        compiled: Arc::new(Mutex::new(Vec::new())),
    });
    let window = open_window_with(cx, root.clone(), setup(Some(tool), Some(tests)));
    cx.run_until_parked();
    draw(cx, window);
    let panel = run_panel(cx);
    wait_git(cx, "the project to load", |cx| {
        panel.read_with(cx, |panel, _| panel.state().hint.is_none())
    });
    panel.update(cx, |panel, cx| {
        panel.run_tests(
            TestRequest {
                suite: Some("A".into()),
                ..TestRequest::default()
            },
            cx,
        )
    });
    wait_git(cx, "the first run", |_| plans.load(Ordering::SeqCst) == 1);
    wait_idle(cx, &panel);

    press(cx, window, "alt-shift-r");
    wait_git(cx, "the second run", |_| plans.load(Ordering::SeqCst) == 2);
    wait_idle(cx, &panel);
}

#[ui_kit::gpui::test]
fn synchronize_reads_the_files_from_disk_again(cx: &mut TestAppContext) {
    let (root, window) = crate::window_tests::open_sample_window(cx, "vcs-keys-sync");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    fs::write(&path, "ALPHA\nbeta\n").unwrap();

    press(cx, window, "ctrl-alt-y");

    wait_git(cx, "the text of the file", |cx| {
        buffer_of(cx, &view) == "ALPHA\nbeta\n"
    });
}

#[ui_kit::gpui::test]
fn temporary_breakpoint_goes_away_at_the_first_stop(cx: &mut TestAppContext) {
    let setup = debug_setup(cx, "vcs-keys-temporary");
    let path = setup.root.join("src").join("main.txt");
    open_path(cx, setup.window, &path);
    let view = editor_of(cx, setup.window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, setup.window);
    let debug = debug_panel(cx);

    press(cx, setup.window, "ctrl-alt-shift-f8");
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.temporary_breakpoint_lines(&path)),
        vec![2]
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.temporary_breakpoints().to_vec()),
        [1],
        "the gutter marks the temporary breakpoint apart"
    );

    stop_at_start(cx, &setup);
    wait_git(cx, "the temporary breakpoint to go", |cx| {
        debug
            .read_with(cx, |panel, _| panel.temporary_breakpoint_lines(&path))
            .is_empty()
    });
    assert!(
        view.read_with(cx, |view, _| view.temporary_breakpoints().is_empty()),
        "the gutter follows"
    );
    end_session(cx, &debug, &setup.service);
}

#[ui_kit::gpui::test]
fn edit_breakpoint_opens_the_properties_and_more_the_list(cx: &mut TestAppContext) {
    let setup = debug_setup(cx, "vcs-keys-edit-breakpoint");
    let path = setup.root.join("src").join("main.txt");
    open_path(cx, setup.window, &path);
    let view = editor_of(cx, setup.window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 0, cx));
    draw(cx, setup.window);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| panel.toggle_breakpoint(path.clone(), 2, cx));
    draw(cx, setup.window);

    press(cx, setup.window, "ctrl-shift-f8");
    draw(cx, setup.window);
    assert!(shown(cx, setup.window, "breakpoint-popup"));
    click(cx, setup.window, "breakpoint-popup-enabled");
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_enabled(&path, 2)),
        Some(false)
    );

    press(cx, setup.window, "ctrl-shift-f8");
    draw(cx, setup.window);
    click(cx, setup.window, "breakpoint-popup-more");
    draw(cx, setup.window);
    assert!(
        crate::window_tests::bounds_of(cx, setup.window, "breakpoints-dialog")
            .size
            .height
            > ui_kit::px(0.),
        "More opens the breakpoints window"
    );
}

#[ui_kit::gpui::test]
fn force_step_over_turns_the_breakpoints_off_and_gives_them_back(cx: &mut TestAppContext) {
    let setup = debug_setup(cx, "vcs-keys-force-step-over");
    let path = setup.root.join("src").join("main.txt");
    open_path(cx, setup.window, &path);
    let debug = debug_panel(cx);
    debug.update(cx, |panel, cx| {
        panel.toggle_breakpoint(path.clone(), 1, cx);
        panel.toggle_breakpoint(path.clone(), 2, cx);
    });
    draw(cx, setup.window);
    stop_at_start(cx, &setup);

    press(cx, setup.window, "alt-shift-f8");

    wait_git(cx, "the step and the breakpoints off", |_| {
        let requests = setup.service.requests();
        let stepped = requests.iter().any(|request| request["command"] == "next");
        let off = requests.iter().any(|request| {
            request["command"] == "setBreakpoints"
                && request["arguments"]["breakpoints"]
                    .as_array()
                    .is_some_and(|list| list.is_empty())
        });
        stepped && off
    });
    wait_git(cx, "the breakpoints back", |_| {
        last_breakpoint_lines(&setup.service.requests(), &path)
            .is_some_and(|lines| lines.contains(&1) && lines.contains(&2))
    });
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&path)),
        vec![1, 2],
        "the breakpoints of the user stay"
    );
    end_session(cx, &debug, &setup.service);
}

// The same temporary breakpoint and Force Step Over on the real adapter.

use crate::window_tests_real::{build_window, project, wait_for as wait_real, wait_ready};

fn real_setup(
    language: &window_language::RealLanguage,
    probe: &window_language::Probe,
) -> ProjectSetup {
    ProjectSetup {
        tool: Some(language.probe_build()),
        providers: language.probe_providers(probe),
        tests: None,
        test_sites: window_language::test_sites,
        cache_root: scratch("vcs-keys-probe-cache"),
        templates: Vec::new(),
        skipped_folders: window_language::skipped_folders(),
        environment: None,
        debug: Some(language.debugger()),
    }
}

fn real_stop(cx: &mut TestAppContext, debug: &gpui::Entity<crate::debug::DebugPanel>) {
    wait_real(cx, "the stop at the breakpoint", LIMIT, |cx| {
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { .. }
        )
    });
    wait_real(cx, "the stack and the variables", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        !snapshot.frames.is_empty() && !snapshot.variables.is_empty()
    });
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn temporary_breakpoint_and_force_step_over_on_the_real_adapter(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-vcs-keys");
    let probe = language.install_probe().expect("the probe compiles");
    let setup = real_setup(&language, &probe);
    let window = build_window(cx, root, &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let panel = run_panel(cx);
    wait_real(cx, "the probe configuration", LIMIT, |cx| {
        panel
            .read_with(cx, |panel, _| panel.state().configs.clone())
            .iter()
            .any(|name| name == "DebugProbe")
    });
    assert!(panel.update(cx, |panel, cx| panel.select_named("DebugProbe", cx)));
    open_path(cx, window, &probe.path);
    let debug = debug_panel(cx);

    // A breakpoint on the call line; a temporary one in add() is set once the session stops.
    let view = editor_of(cx, window, &probe.path);
    debug.update(cx, |panel, cx| {
        panel.toggle_breakpoint(probe.path.clone(), 10, cx)
    });
    press(cx, window, "shift-f9");
    real_stop(cx, &debug);
    wait_real(cx, "the stop at the call line", LIMIT, |cx| {
        debug.read_with(cx, |panel, _| {
            panel.snapshot().frames.first().map(|frame| frame.line)
        }) == Some(10)
    });

    // Force Step Over from the call line does not stop inside add(), and the breakpoints
    // of the user come back.
    debug.update(cx, |panel, cx| {
        panel.toggle_breakpoint(probe.path.clone(), probe.line, cx)
    });
    press(cx, window, "alt-shift-f8");
    wait_real(cx, "the step past the call", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot
                .frames
                .first()
                .is_some_and(|frame| frame.line != probe.line)
    });
    assert_eq!(
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&probe.path)),
        vec![10, probe.line],
        "the breakpoints of the user are back"
    );

    // A temporary breakpoint in add() stops once and goes away.
    debug.update(cx, |panel, cx| {
        panel.remove_breakpoint(probe.path.clone(), 10, cx);
        panel.remove_breakpoint(probe.path.clone(), probe.line, cx);
    });
    view.update(cx, |view, cx| {
        view.restore_caret(probe.line as usize - 1, 8, cx)
    });
    draw(cx, window);
    press(cx, window, "ctrl-alt-shift-f8");
    assert_eq!(
        debug.read_with(cx, |panel, _| {
            panel.temporary_breakpoint_lines(&probe.path)
        }),
        vec![probe.line]
    );
    press(cx, window, "f9");
    wait_real(cx, "the stop of the temporary breakpoint", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        matches!(snapshot.state, DebugState::Stopped { .. })
            && snapshot
                .frames
                .first()
                .is_some_and(|frame| frame.line == probe.line)
    });
    assert!(
        debug
            .read_with(cx, |panel, _| panel.temporary_breakpoint_lines(&probe.path))
            .is_empty(),
        "the temporary breakpoint is gone after the stop"
    );

    crate::window_tests_real::close(cx, window, language, true);
    crate::window_tests_real::finish(
        "temporary_breakpoint_and_force_step_over_on_the_real_adapter",
        started,
    );
}

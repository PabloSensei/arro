//! The Build window without a real build: a stand-in tool prints two errors of two files,
//! the tree counts them and Enter opens the line of a message. The Build menu rows and
//! their hints are read from the menu the window builds.

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, Module, ProjectModel};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{BUILD_TOOL, Workspace};

use crate::build_output::ActiveBuildOutput;
use crate::run::ProjectSetup;
use crate::window_tests::{
    active_path, draw, editor_of, open_window_with, press, sample_project, scratch,
};
use crate::window_tests_run::wait_for;

/// A build tool that prints two compile errors and one warning of two files.
struct ErrorTool {
    model: ProjectModel,
    builds: Arc<AtomicUsize>,
    /// The program of the build waits; a test can stop it.
    slow: bool,
}

impl ErrorTool {
    fn new(root: &Path, builds: Arc<AtomicUsize>, slow: bool) -> Self {
        Self {
            model: ProjectModel {
                root: root.to_path_buf(),
                tool: "fake".into(),
                resolved: true,
                modules: vec![Module {
                    id: "root".into(),
                    name: "demo".into(),
                    dir: root.to_path_buf(),
                    ..Module::default()
                }],
                ..ProjectModel::default()
            },
            builds,
            slow,
        }
    }
}

impl BuildSystem for ErrorTool {
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

    fn build(&self, root: &Path, _: bool) -> base::Result<ProcessSpec> {
        self.builds.fetch_add(1, Ordering::SeqCst);
        let main = root.join("src").join("main.txt");
        let notes = root.join("src").join("notes.txt");
        let mut command = format!(
            "echo [ERROR] {}:[2,1] cannot find symbol & echo [ERROR] {}:[1,1] missing name & echo [WARNING] {}:[3,1] unused name",
            main.display(),
            notes.display(),
            notes.display()
        );
        if self.slow {
            command.push_str(" & ping -n 10 127.0.0.1 >nul");
        }
        Ok(ProcessSpec::new("cmd").args(["/d", "/c"]).arg(command))
    }
}

fn setup(root: &Path, builds: Arc<AtomicUsize>, slow: bool) -> ProjectSetup {
    ProjectSetup {
        tool: Some(Arc::new(ErrorTool::new(root, builds, slow))),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("build-output-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

fn panel(cx: &TestAppContext) -> ui_kit::gpui::Entity<crate::build_output::BuildOutputPanel> {
    cx.read(|cx| cx.global::<ActiveBuildOutput>().0.clone())
}

fn labels(
    cx: &TestAppContext,
    panel: &ui_kit::gpui::Entity<crate::build_output::BuildOutputPanel>,
) -> Vec<String> {
    panel.read_with(cx, |panel, cx| panel.labels(cx))
}

#[ui_kit::gpui::test]
fn a_build_shows_its_errors_and_enter_opens_the_line(cx: &mut TestAppContext) {
    let root = sample_project("build-output-errors");
    let builds = Arc::new(AtomicUsize::new(0));
    let window = open_window_with(cx, root.clone(), setup(&root, builds.clone(), false));
    let panel = panel(cx);
    wait_for(cx, "the project to load", |cx| {
        let run = cx.read(|cx| cx.global::<crate::script::ActiveRunPanel>().0.clone());
        run.read_with(cx, |run, _| run.state().hint.is_none())
    });

    press(cx, window, "ctrl-f9");
    wait_for(cx, "the build errors", |cx| {
        let run = cx.read(|cx| cx.global::<crate::script::ActiveRunPanel>().0.clone());
        !run.read_with(cx, |run, _| run.state().running)
            && panel.read_with(cx, |panel, cx| panel.problems(cx).len()) == 3
    });
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(BUILD_TOOL))
            .unwrap(),
        "Ctrl+F9 shows the Build window"
    );
    let lines = labels(cx, &panel);
    assert!(lines[0].starts_with("Fake Build Project"), "{lines:?}");
    assert!(
        lines.iter().any(|line| line.trim() == "src/main.txt"),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line.trim() == "src/notes.txt"),
        "{lines:?}"
    );
    let hints = panel.read_with(cx, |panel, cx| panel.hints(cx));
    let root_hint = &hints[0].1;
    assert!(root_hint.contains("2 errors"), "{root_hint}");
    assert!(root_hint.contains("1 warning"), "{root_hint}");
    assert!(root_hint.contains("with errors"), "{root_hint}");
    let file_hint = hints
        .iter()
        .find(|(label, _)| label == "src/main.txt")
        .map(|(_, hint)| hint)
        .expect("the file carries its count");
    assert_eq!(file_hint, "1 error");

    // Only errors hides the warning and the counts follow.
    panel.update(cx, |panel, cx| panel.set_only_errors(true, cx));
    assert!(panel.read_with(cx, |panel, _| panel.only_errors()));
    assert!(!panel.read_with(cx, |panel, cx| {
        panel
            .labels(cx)
            .iter()
            .any(|line| line.contains("unused name"))
    }));
    let root_hint = panel.read_with(cx, |panel, cx| panel.hints(cx))[0]
        .1
        .clone();
    assert!(root_hint.contains("2 errors"), "{root_hint}");
    assert!(!root_hint.contains("warning"), "{root_hint}");
    panel.update(cx, |panel, cx| panel.set_only_errors(false, cx));

    let message = lines
        .iter()
        .position(|line| line.trim() == "2:1 cannot find symbol")
        .unwrap_or_else(|| panic!("no message row in {lines:?}"));
    let focus = panel.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    panel.update(cx, |panel, cx| panel.select_row(message, cx));
    press(cx, window, "enter");

    let main = root.join("src").join("main.txt");
    wait_for(cx, "the file of the message", |cx| {
        active_path(cx, window).as_deref() == Some(main.as_path())
    });
    let view = editor_of(cx, window, &main);
    wait_for(cx, "the caret on the line", |cx| {
        view.read_with(cx, |view, _| view.caret()) == (1, 0)
    });

    // Restart runs the last build again.
    panel.update(cx, |panel, cx| panel.restart_now(cx));
    wait_for(cx, "the build again", |_| {
        builds.load(Ordering::SeqCst) == 2
    });
    wait_for(cx, "the output of the second build", |cx| {
        let run = cx.read(|cx| cx.global::<crate::script::ActiveRunPanel>().0.clone());
        !run.read_with(cx, |run, _| run.state().running)
            && panel.read_with(cx, |panel, cx| panel.problems(cx).len()) == 3
    });
}

#[ui_kit::gpui::test]
fn a_message_opens_on_its_column(cx: &mut TestAppContext) {
    let root = sample_project("build-output-column");
    let builds = Arc::new(AtomicUsize::new(0));
    let window = open_window_with(cx, root.clone(), setup(&root, builds, false));
    let panel = panel(cx);
    wait_for(cx, "the project to load", |cx| {
        let run = cx.read(|cx| cx.global::<crate::script::ActiveRunPanel>().0.clone());
        run.read_with(cx, |run, _| run.state().hint.is_none())
    });
    press(cx, window, "ctrl-f9");
    wait_for(cx, "the build errors", |cx| {
        panel.read_with(cx, |panel, cx| panel.problems(cx).len()) == 3
    });
    draw(cx, window);

    let lines = labels(cx, &panel);
    let message = lines
        .iter()
        .position(|line| line.trim() == "2:1 cannot find symbol")
        .unwrap_or_else(|| panic!("no message row in {lines:?}"));
    let focus = panel.read_with(cx, |panel, _| panel.focus());
    window
        .update(cx, |_, window, cx| window.focus(&focus, cx))
        .unwrap();
    panel.update(cx, |panel, cx| panel.select_row(message, cx));
    press(cx, window, "enter");

    let main = root.join("src").join("main.txt");
    wait_for(cx, "the caret on the column", |cx| {
        active_path(cx, window).as_deref() == Some(main.as_path())
            && editor_of(cx, window, &main).read_with(cx, |view, _| view.caret()) == (1, 0)
    });
}

#[ui_kit::gpui::test]
fn stopping_a_build_keeps_its_output(cx: &mut TestAppContext) {
    let root = sample_project("build-output-stop");
    let builds = Arc::new(AtomicUsize::new(0));
    let window = open_window_with(cx, root.clone(), setup(&root, builds, true));
    let panel = panel(cx);
    wait_for(cx, "the project to load", |cx| {
        let run = cx.read(|cx| cx.global::<crate::script::ActiveRunPanel>().0.clone());
        run.read_with(cx, |run, _| run.state().hint.is_none())
    });

    press(cx, window, "ctrl-f9");
    wait_for(cx, "the build to run", |cx| {
        panel.read_with(cx, |panel, cx| panel.problems(cx).len()) == 3
            && panel.read_with(cx, |panel, cx| panel.labels(cx)).len() > 1
    });
    draw(cx, window);
    panel.update(cx, |panel, cx| panel.stop_now(cx));
    wait_for(cx, "the stopped build", |cx| {
        !cx.read(|cx| cx.global::<crate::script::ActiveRunPanel>().0.clone())
            .read_with(cx, |run, _| run.state().running)
    });
    let root_hint = panel.read_with(cx, |panel, cx| panel.hints(cx))[0]
        .1
        .clone();
    assert!(root_hint.contains("stopped"), "{root_hint}");
    assert!(root_hint.contains("2 errors"), "{root_hint}");
}

fn build_menu(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<(String, bool)> {
    window
        .update(cx, |workspace, _, cx| {
            workspace
                .main_menu(cx)
                .into_iter()
                .find(|entry| entry.label == "Build")
                .expect("the Build menu")
                .submenu
                .into_iter()
                .map(|entry| (entry.label.to_string(), entry.tip.is_some()))
                .collect()
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn the_build_menu_names_every_command_with_a_hint(cx: &mut TestAppContext) {
    let root = sample_project("build-menu");
    let builds = Arc::new(AtomicUsize::new(0));
    let window = open_window_with(cx, root.clone(), setup(&root, builds, false));
    draw(cx, window);
    let rows = build_menu(cx, window);

    for wanted in ["Build Project", "Rebuild Project"] {
        let row = rows
            .iter()
            .find(|(label, _)| label == wanted)
            .unwrap_or_else(|| panic!("no {wanted} row in {rows:?}"));
        assert!(row.1, "{wanted} has a hint");
    }
    for wanted in ["Build Module", "Recompile"] {
        let row = rows
            .iter()
            .find(|(label, _)| label.starts_with(wanted))
            .unwrap_or_else(|| panic!("no {wanted} row in {rows:?}"));
        assert!(row.1, "{wanted} has a hint");
    }
    for (label, has_hint) in &rows {
        assert!(has_hint, "{label} has a hint");
    }

    window
        .read_with(cx, |workspace, _| {
            let commands = workspace.commands();
            for id in [
                "workspace.build_project",
                "workspace.build_module",
                "workspace.compile",
                "workspace.rebuild_project",
            ] {
                assert!(commands.get(id).is_some(), "{id} is a Find Action row");
            }
        })
        .unwrap();
}

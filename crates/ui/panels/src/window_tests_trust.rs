//! A project that is not trusted: the window opens it in Safe Mode, asks before anything
//! of it runs, and starts the build and the language server only after Trust Project.
//! The build tool and the language starter are stand-ins that count their calls.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use base::Cancel;
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, Module, ProjectModel};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::Workspace;

use crate::run::ProjectSetup;
use crate::trust::{LanguageStart, ProjectTrust, ServerTrust};
use crate::window_tests::{
    click, draw, open_window_with, sample_project, scratch, wait_for_signal,
};

/// A build tool that counts its syncs: Safe Mode must not ask it for the model.
struct CountingTool {
    model: ProjectModel,
    syncs: Arc<AtomicUsize>,
}

impl CountingTool {
    fn new(root: &Path, syncs: Arc<AtomicUsize>) -> Self {
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
            syncs,
        }
    }
}

impl BuildSystem for CountingTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        self.syncs.fetch_add(1, Ordering::SeqCst);
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }
}

/// The starter of the language server: records the trust it is asked to start with.
#[derive(Clone)]
struct FakeStart {
    called: Arc<Mutex<Vec<ServerTrust>>>,
}

impl LanguageStart for FakeStart {
    fn start(
        &self,
        trust: ServerTrust,
        _started: futures_channel::mpsc::UnboundedSender<Option<crate::StartedServer>>,
    ) {
        self.called
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(trust);
    }
}

fn setup(root: &Path, syncs: Arc<AtomicUsize>) -> ProjectSetup {
    ProjectSetup {
        tool: Some(Arc::new(CountingTool::new(root, syncs))),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("trust-open-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    }
}

fn calls(start: &FakeStart) -> Vec<ServerTrust> {
    start
        .called
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .clone()
}

fn has_overlay(cx: &TestAppContext, window: ui_kit::gpui::WindowHandle<Workspace>) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.has_overlay())
        .unwrap()
}

#[ui_kit::gpui::test]
fn an_untrusted_project_opens_in_safe_mode_and_preview_runs_nothing(cx: &mut TestAppContext) {
    let root = sample_project("trust-open-preview");
    let syncs = Arc::new(AtomicUsize::new(0));
    let start = FakeStart {
        called: Arc::new(Mutex::new(Vec::new())),
    };
    let project_trust = ProjectTrust::untrusted(std::rc::Rc::new(start.clone()));
    cx.update(|cx| cx.set_global(project_trust));

    let window = open_window_with(cx, root.clone(), setup(&root, syncs.clone()));
    draw(cx, window);

    assert!(has_overlay(cx, window), "the trust dialog is shown");
    assert_eq!(syncs.load(Ordering::SeqCst), 0, "safe mode does not sync");
    assert_eq!(calls(&start), [ServerTrust::Untrusted]);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.safe_mode())
            .unwrap(),
        "the window says it is in Safe Mode"
    );

    click(cx, window, "trust-open-preview");
    draw(cx, window);

    assert!(!has_overlay(cx, window), "Preview closes the dialog");
    assert_eq!(syncs.load(Ordering::SeqCst), 0, "Preview runs nothing");
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    assert!(
        !store::TrustList::new(&state_dir).is_trusted(&root),
        "Preview records no trust"
    );

    // The status bar offers the same dialog after Preview.
    draw(cx, window);
    click(cx, window, "st-safe-mode-trust");
    draw(cx, window);
    assert!(has_overlay(cx, window), "the link opens the dialog again");
}

#[ui_kit::gpui::test]
fn trust_project_starts_the_build_and_records_the_answer(cx: &mut TestAppContext) {
    let root = sample_project("trust-open-accept");
    let syncs = Arc::new(AtomicUsize::new(0));
    let start = FakeStart {
        called: Arc::new(Mutex::new(Vec::new())),
    };
    let project_trust = ProjectTrust::untrusted(std::rc::Rc::new(start.clone()));
    cx.update(|cx| cx.set_global(project_trust));

    let window = open_window_with(cx, root.clone(), setup(&root, syncs.clone()));
    draw(cx, window);
    assert!(has_overlay(cx, window), "the trust dialog is shown");

    click(cx, window, "trust-open-accept");
    wait_for_signal(cx, "the build tool to read the project", |_| {
        syncs.load(Ordering::SeqCst) == 1
    });
    draw(cx, window);

    assert!(!has_overlay(cx, window), "Trust Project closes the dialog");
    assert!(
        !window
            .read_with(cx, |workspace, _| workspace.safe_mode())
            .unwrap()
    );
    let state_dir = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    assert!(
        store::TrustList::new(&state_dir).is_trusted(&root),
        "Trust Project records the answer"
    );
    assert_eq!(
        calls(&start),
        [ServerTrust::Untrusted, ServerTrust::Trusted],
        "the language server starts only after the trust"
    );

    // File > Forget Trust returns the project to Safe Mode without a restart.
    window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("file.forget_trust", window, cx);
        })
        .unwrap();
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.safe_mode())
            .unwrap(),
        "Forget Trust turns Safe Mode back on"
    );
    assert!(
        !store::TrustList::new(&state_dir).is_trusted(&root),
        "Forget Trust drops the answer"
    );
}

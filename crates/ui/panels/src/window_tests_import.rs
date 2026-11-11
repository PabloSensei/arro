//! A build import that fails reaches the window: Problems on the build script, the
//! status bar and a notification with Show Log.
//!
//! `cargo test -p panels window_tests_import -- --ignored --test-threads=1`
//!
//! Needs `IDE_CORPUS` and the language server tools. The corpus is copied, the project
//! keeps its own build scripts as they are.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use ui_kit::gpui::WindowHandle;
use window_language::RealLanguage;
use workspace::Workspace;

use crate::problems::ActiveProblems;
use crate::window_tests::{
    active_path, click, draw, open_window, project_setup, sample_project, scratch,
};
use crate::window_tests_real::{build_window, close, wait_for};

const LIMIT: Duration = Duration::from_secs(600);

fn status(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace.language_status().map(ToString::to_string)
        })
        .unwrap()
}

/// Opens the corpus copy whose root build script fails before any dependency resolves.
fn project(name: &str) -> (PathBuf, RealLanguage) {
    let root = scratch(name).join("broken-import");
    let source = window_language::corpus_dir().join("src/dgs-framework");
    window_language::copy_project(&source, &root);
    let language =
        RealLanguage::open_import_project(root.clone(), scratch(&format!("{name}-state")))
            .expect("the language server starts");
    (root, language)
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and the build tool cache"]
fn a_fixed_build_script_clears_the_failure_and_a_new_one_is_shown(cx: &mut TestAppContext) {
    let started = Instant::now();
    let root = scratch("real-import-fix").join("import-fix");
    window_language::write_import_project(&root, false);
    let language =
        RealLanguage::open_import_project(root.clone(), scratch("real-import-fix-state"))
            .expect("the language server starts");
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);

    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    let failure_in_problems = |cx: &mut TestAppContext| {
        problems.read_with(cx, |hub, _| {
            hub.state().items(&root).iter().any(|item| {
                window_language::is_build_script(&item.hint) && item.label.contains("noSuchTask")
            })
        })
    };

    // Break the configuration: the import reimports the project and fails.
    window_language::write_build_script(&root, true);
    wait_for(cx, "the failed import in Problems", LIMIT, |cx| {
        failure_in_problems(cx)
    });
    wait_for(cx, "the reason in the status bar", LIMIT, |cx| {
        status(cx, window)
            .is_some_and(|text| text.contains("import failed") && text.contains("noSuchTask"))
    });
    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("the import failure is a notification");
    assert!(title.contains("Language Server"), "{title}");
    assert!(text.contains("noSuchTask"), "{text}");
    let failed_serial = window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap();

    // Fix the script the way a save does: the file reaches the disk and the watcher
    // of the window tells the server about it. The reimport starts and the old
    // failure must go before it ends.
    window_language::write_build_script(&root, false);
    wait_for(cx, "the failure to leave Problems", LIMIT, |cx| {
        problems.read_with(cx, |hub, _| hub.state().items(&root).is_empty())
    });
    wait_for(cx, "the failure to leave the status bar", LIMIT, |cx| {
        status(cx, window).is_some_and(|text| !text.contains("import failed"))
    });
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.notification_serial())
            .unwrap(),
        failed_serial,
        "a successful import does not notify again"
    );

    // A new failure after the fix is visible again, in Problems, the status bar and
    // a notification.
    window_language::write_build_script(&root, true);
    wait_for(cx, "the new failure in Problems", LIMIT, |cx| {
        failure_in_problems(cx)
    });
    wait_for(cx, "the new reason in the status bar", LIMIT, |cx| {
        status(cx, window)
            .is_some_and(|text| text.contains("import failed") && text.contains("noSuchTask"))
    });
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.notification_serial())
            .unwrap()
            > failed_serial,
        "a new failure notifies again"
    );

    close(cx, window, language, true);
    println!(
        "a_fixed_build_script_clears_the_failure_and_a_new_one_is_shown: {:.1} s",
        started.elapsed().as_secs_f64()
    );
}

#[ui_kit::gpui::test]
fn the_notification_action_opens_its_file(cx: &mut TestAppContext) {
    let root = sample_project("notification-action");
    let log = root.join("server.log");
    std::fs::write(&log, "the server log\n").unwrap();
    let window = open_window(cx, root);
    window
        .update(cx, |workspace, _, cx| {
            workspace.notify_user_with_action(
                "Language Server",
                "import failed: Task with name 'postRelease' not found",
                "Show Log",
                log.clone(),
                cx,
            )
        })
        .unwrap();
    draw(cx, window);
    click(cx, window, "notification-action");
    assert_eq!(active_path(cx, window), Some(log));
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and the build tool cache"]
fn a_failed_build_import_reaches_the_window(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-import");
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);

    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    wait_for(cx, "the failed import in Problems", LIMIT, |cx| {
        problems.read_with(cx, |hub, _| {
            hub.state().items(&root).iter().any(|item| {
                window_language::is_root_build_script(&item.hint)
                    && item.label.contains("postRelease")
            })
        })
    });

    wait_for(cx, "the reason in the status bar", LIMIT, |cx| {
        status(cx, window)
            .is_some_and(|text| text.contains("import failed") && text.contains("postRelease"))
    });

    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("the import failure is a notification");
    assert!(title.contains("Language Server"), "{title}");
    assert!(text.contains("postRelease"), "{text}");
    let (label, log) = window
        .read_with(cx, |workspace, _| workspace.last_notification_action())
        .unwrap()
        .expect("the notification offers the server log");
    assert_eq!(label, "Show Log");
    assert!(log.is_file(), "{}", log.display());

    draw(cx, window);
    click(cx, window, "notification-action");
    wait_for(cx, "the server log in a tab", LIMIT, |cx| {
        window
            .read_with(cx, |workspace, _| workspace.active_path())
            .unwrap()
            == Some(log.clone())
    });

    close(cx, window, language, true);
    println!(
        "a_failed_build_import_reaches_the_window: {:.1} s",
        started.elapsed().as_secs_f64()
    );
}

/// A project that imports cleanly reports no project problem, so the notification does
/// not fire on every open project.
#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn a_healthy_build_import_has_no_project_problem(cx: &mut TestAppContext) {
    let (root, language) = crate::window_tests_real::project("real-import-control");
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);

    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    assert!(
        problems.read_with(cx, |hub, _| hub.state().items(&root).is_empty()),
        "a project that imports has no project problems"
    );
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.last_notification())
            .unwrap()
            .is_none(),
        "no notification for a project that imports"
    );
    close(cx, window, language, true);
}

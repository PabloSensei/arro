//! A language server that gave up reaches the window: the reason stays in the status
//! bar and one notification per run offers a retry.
//!
//! No real process runs here: the status lines come from the test.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use editor_view::Languages;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{draw, project_setup, sample_project, scratch, wait_for_signal};
use crate::{AgentOptions, LanguageServer, connect, install};

fn failed(reason: &str) -> String {
    format!("Fixture: failed: {reason}")
}

fn status(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace.language_status().map(ToString::to_string)
        })
        .unwrap()
}

fn notification(cx: &TestAppContext, window: WindowHandle<Workspace>) -> usize {
    window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap()
}

/// The window with a status stream of the caller, so that a test writes every line
/// the language server would send.
fn window_with_status(
    cx: &mut TestAppContext,
    root: PathBuf,
    status: futures_channel::mpsc::UnboundedReceiver<String>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |path| dockerfile_syntax::is_dockerfile(path).then(dockerfile_syntax::config),
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let server = LanguageServer {
        support: None,
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

#[ui_kit::gpui::test]
fn a_gave_up_server_notifies_once_and_retry_runs_again(cx: &mut TestAppContext) {
    let root = sample_project("language-failure");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let retries = Arc::new(AtomicUsize::new(0));
    let window = window_with_status(cx, root, statuses);
    let counted = retries.clone();
    window
        .update(cx, |workspace, _, _| {
            workspace.register_command(
                "language.restart_server",
                "Restart Language Server",
                move |_, _, _| {
                    counted.fetch_add(1, Ordering::SeqCst);
                },
            );
        })
        .unwrap();

    sender
        .unbounded_send(failed("the runtime is too old to load the server"))
        .unwrap();
    wait_for_signal(cx, "the failed line", |cx| {
        status(cx, window).is_some_and(|text| text.contains("too old"))
    });

    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("a server that gave up is a notification");
    assert!(title.contains("Language Server"), "{title}");
    assert!(text.contains("too old"), "{text}");
    let buttons: Vec<String> = window
        .read_with(cx, |workspace, _| workspace.last_notification_buttons())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(buttons, ["Retry"]);

    let first = notification(cx, window);
    sender
        .unbounded_send(failed("the server gave up again"))
        .unwrap();
    wait_for_signal(cx, "the second failed line", |cx| {
        status(cx, window).is_some_and(|text| text.contains("gave up again"))
    });
    assert_eq!(
        notification(cx, window),
        first,
        "a second failure does not notify again"
    );

    draw(cx, window);
    crate::window_tests::click(cx, window, "notification-action");
    assert_eq!(
        retries.load(Ordering::SeqCst),
        1,
        "the button runs the restart command"
    );
}

#[test]
fn only_a_line_with_a_reason_is_a_failure() {
    assert_eq!(
        crate::failed_reason("Fixture: failed: no such runtime"),
        Some("no such runtime")
    );
    assert_eq!(crate::failed_reason("Fixture: ready"), None);
    assert_eq!(crate::failed_reason("Fixture: failed: "), None);
    assert_eq!(crate::failed_reason("Fixture: unavailable"), None);
}

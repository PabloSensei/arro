//! A language server that the application could not set up: the status bar holds one
//! short line, and a click on it explains in a balloon where the tool goes.
//!
//! No real process runs here: the status lines come from the test and name a fixture.

use std::path::PathBuf;
use std::sync::Arc;

use editor_view::Languages;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{click, draw, project_setup, sample_project, scratch, wait_for_signal};
use crate::{AgentOptions, LanguageServer, connect, install};

const MISSING: &str = "Fixture: server not found";
const DETAILS: &str = "Put the unpacked server in tools\\server-<version>\\ next to the \
                       program, or set FIXTURE_SERVER_HOME to its folder.\n\
                       See README.txt next to the program.";

fn status(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace.language_status().map(ToString::to_string)
        })
        .unwrap()
}

/// The window with a status stream of the caller, so that a test writes every line
/// the application would send.
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
fn the_status_bar_keeps_the_short_line_and_a_click_explains_it(cx: &mut TestAppContext) {
    let root = sample_project("server-setup");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let window = window_with_status(cx, root, statuses);

    sender
        .unbounded_send(format!("{MISSING}\n{DETAILS}"))
        .unwrap();
    wait_for_signal(cx, "the setup line", |cx| {
        status(cx, window).as_deref() == Some(MISSING)
    });

    draw(cx, window);
    click(cx, window, "st-language");

    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("a click on the line explains the setup");
    assert_eq!(title, MISSING);
    assert_eq!(text, DETAILS);
    let buttons: Vec<String> = window
        .read_with(cx, |workspace, _| workspace.last_notification_buttons())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(buttons, ["README"]);
}

#[ui_kit::gpui::test]
fn a_ready_line_has_nothing_to_explain(cx: &mut TestAppContext) {
    let root = sample_project("server-ready");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let window = window_with_status(cx, root, statuses);

    sender.unbounded_send("Fixture: ready".into()).unwrap();
    wait_for_signal(cx, "the ready line", |cx| {
        status(cx, window).as_deref() == Some("Fixture: ready")
    });

    draw(cx, window);
    click(cx, window, "st-language");

    let notification = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap();
    assert_eq!(notification, None);
}

//! A language server that stops for low memory reaches the window: the status line names
//! the stop, and one notice gives the free memory with Start Anyway, the memory settings,
//! Don't Stop on This Machine and a close. On a virtual machine one warning comes when the
//! memory stays low. The memory numbers are set by the test, so they do not depend on the
//! machine. No real process runs here: the status lines come from the test.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use editor_view::Languages;
use platform::MemoryState;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::memory_stop::{MachineReading, WARNING_POLL};
use crate::window_tests::{click, draw, project_setup, sample_project, scratch, wait_for_signal};
use crate::{AgentOptions, LanguageServer, connect, install};

const STOPPED: &str = "Fixture: stopped: memory is low, it starts again when memory frees";
const MANUAL: &str = "Fixture: stopped: memory is low, restart it manually";
const WARNING_TITLE: &str = "Low memory on this virtual machine";

fn gigabytes(count: u64) -> u64 {
    count * 1024 * 1024 * 1024
}

/// 400 MB of 16 GB physical memory free, with room in the page file.
fn physical_bound() -> MemoryState {
    MemoryState {
        available_physical: 400 * 1024 * 1024,
        total_physical: gigabytes(16),
        available_commit: gigabytes(8),
        commit_limit: gigabytes(24),
    }
}

/// The page file runs out first: 512 MB of commit left, the physical memory has room.
fn commit_bound() -> MemoryState {
    MemoryState {
        available_physical: gigabytes(6),
        total_physical: gigabytes(16),
        available_commit: 512 * 1024 * 1024,
        commit_limit: gigabytes(18),
    }
}

/// Plenty of memory free: neither a stop nor a warning.
fn roomy() -> MemoryState {
    MemoryState {
        available_physical: gigabytes(8),
        total_physical: gigabytes(16),
        available_commit: gigabytes(16),
        commit_limit: gigabytes(24),
    }
}

fn status(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace.language_status().map(ToString::to_string)
        })
        .unwrap()
}

fn status_action(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<&'static str> {
    window
        .read_with(cx, |workspace, _| workspace.language_action())
        .unwrap()
}

fn notification(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<(String, String)> {
    window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .map(|(title, text)| (title.to_string(), text.to_string()))
}

fn serial(cx: &TestAppContext, window: WindowHandle<Workspace>) -> usize {
    window
        .read_with(cx, |workspace, _| workspace.notification_serial())
        .unwrap()
}

fn buttons(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    window
        .read_with(cx, |workspace, _| workspace.last_notification_buttons())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect()
}

/// The memory check as the settings of the window have it now.
fn memory_check(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<bool> {
    window
        .read_with(cx, |_, app| app.global::<Settings>().memory_low_check)
        .unwrap()
}

fn has(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

/// The window with a status stream of the test and the memory numbers it sets, on a machine
/// that is not a virtual machine.
fn window_with_status(
    cx: &mut TestAppContext,
    root: PathBuf,
    status: futures_channel::mpsc::UnboundedReceiver<String>,
    memory: MemoryState,
) -> WindowHandle<Workspace> {
    let machine = MachineReading {
        memory: Some(memory),
        virtual_machine: false,
    };
    window_with_machine(cx, root, status, machine)
}

/// The window with a status stream of the test and the machine that the test sets.
fn window_with_machine(
    cx: &mut TestAppContext,
    root: PathBuf,
    status: futures_channel::mpsc::UnboundedReceiver<String>,
    machine: MachineReading,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("memory-stop-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(machine);
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
            project_setup(scratch("memory-stop-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// Counts the restarts of the language server: the command that Start Anyway and Don't Stop
/// on This Machine run.
fn counting_restart(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Arc<AtomicUsize> {
    let restarts = Arc::new(AtomicUsize::new(0));
    let counted = restarts.clone();
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
    restarts
}

/// Runs polls of the watch of a virtual machine: each one waits its interval on the clock of
/// the test.
fn poll(cx: &mut TestAppContext, count: usize) {
    for _ in 0..count {
        cx.executor().advance_clock(WARNING_POLL);
        cx.run_until_parked();
    }
}

#[ui_kit::gpui::test]
fn a_memory_stop_shows_one_notice_with_its_buttons_and_the_numbers(cx: &mut TestAppContext) {
    let root = sample_project("memory-stop-once");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let window = window_with_status(cx, root, statuses, physical_bound());
    sender.unbounded_send("Fixture: ready".to_owned()).unwrap();
    wait_for_signal(cx, "the ready line", |cx| {
        status(cx, window).as_deref() == Some("Fixture: ready")
    });

    sender.unbounded_send(STOPPED.to_owned()).unwrap();
    wait_for_signal(cx, "the notice of the stop", |cx| {
        notification(cx, window).is_some_and(|(title, _)| title.contains("stopped"))
    });
    let (title, text) = notification(cx, window).expect("the stop is a notice");
    assert_eq!(title, "Fixture language server stopped: low memory");
    assert!(text.contains("Free memory: 400 MB of 16384 MB."), "{text}");
    assert!(text.contains("below 512 MB and below 5 %"), "{text}");
    assert_eq!(
        buttons(cx, window),
        [
            "Start Anyway",
            "Memory Settings…",
            "Don't Stop on This Machine",
            "Close"
        ]
    );
    assert_eq!(
        status_action(cx, window),
        Some("language.show_memory_stop"),
        "a click on the stop line shows the notice again"
    );

    // The same stop repeats, and another stop comes while the notice is on screen: the
    // window does not show a second notice.
    let first = serial(cx, window);
    sender.unbounded_send(STOPPED.to_owned()).unwrap();
    sender
        .unbounded_send("Fixture: starting".to_owned())
        .unwrap();
    wait_for_signal(cx, "the starting line", |cx| {
        status(cx, window).as_deref() == Some("Fixture: starting")
    });
    sender.unbounded_send(STOPPED.to_owned()).unwrap();
    wait_for_signal(cx, "the stop line again", |cx| {
        status(cx, window).as_deref() == Some(STOPPED)
    });
    assert_eq!(
        serial(cx, window),
        first,
        "a stop while the notice is on screen shows no second notice"
    );
}

#[ui_kit::gpui::test]
fn closing_the_notice_lets_the_next_stop_show_it_again(cx: &mut TestAppContext) {
    let root = sample_project("memory-stop-again");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let window = window_with_status(cx, root, statuses, physical_bound());
    sender.unbounded_send(STOPPED.to_owned()).unwrap();
    wait_for_signal(cx, "the notice of the stop", |cx| {
        notification(cx, window).is_some()
    });
    let shown = serial(cx, window);

    draw(cx, window);
    click(cx, window, "notification-close");
    assert_ne!(serial(cx, window), shown, "the close hides the notice");

    sender
        .unbounded_send("Fixture: starting".to_owned())
        .unwrap();
    wait_for_signal(cx, "the starting line", |cx| {
        status(cx, window).as_deref() == Some("Fixture: starting")
    });
    sender.unbounded_send(STOPPED.to_owned()).unwrap();
    wait_for_signal(cx, "the notice of the next stop", |cx| {
        serial(cx, window) != shown && notification(cx, window).is_some()
    });
}

#[ui_kit::gpui::test]
fn start_anyway_runs_the_restart_of_the_language_server(cx: &mut TestAppContext) {
    let root = sample_project("memory-stop-start");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let window = window_with_status(cx, root, statuses, physical_bound());
    let restarts = counting_restart(cx, window);

    sender.unbounded_send(STOPPED.to_owned()).unwrap();
    wait_for_signal(cx, "the notice of the stop", |cx| {
        notification(cx, window).is_some()
    });
    draw(cx, window);
    click(cx, window, "notification-action");
    assert_eq!(
        restarts.load(Ordering::SeqCst),
        1,
        "Start Anyway runs the same restart as the command"
    );
}

#[ui_kit::gpui::test]
fn dont_stop_on_this_machine_turns_the_check_off_and_starts_the_server(cx: &mut TestAppContext) {
    let root = sample_project("memory-stop-off");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let window = window_with_status(cx, root, statuses, physical_bound());
    let restarts = counting_restart(cx, window);

    sender.unbounded_send(STOPPED.to_owned()).unwrap();
    wait_for_signal(cx, "the notice of the stop", |cx| {
        notification(cx, window).is_some()
    });
    draw(cx, window);
    click(cx, window, "notification-action-off");

    assert_eq!(
        restarts.load(Ordering::SeqCst),
        1,
        "the button starts the server"
    );
    assert_eq!(
        memory_check(cx, window),
        Some(false),
        "the button turns the memory check off"
    );
}

#[ui_kit::gpui::test]
fn the_memory_settings_button_opens_the_page_of_the_thresholds(cx: &mut TestAppContext) {
    let root = sample_project("memory-stop-settings");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let window = window_with_status(cx, root, statuses, physical_bound());
    sender.unbounded_send(STOPPED.to_owned()).unwrap();
    wait_for_signal(cx, "the notice of the stop", |cx| {
        notification(cx, window).is_some()
    });
    draw(cx, window);
    click(cx, window, "notification-action-settings");
    draw(cx, window);
    assert!(
        has(cx, window, "settings-page-chosen-memory"),
        "the window opens on the Memory page"
    );
    assert!(
        has(cx, window, "settings-memory-low-megabytes-up"),
        "the page shows the free memory threshold with its step buttons"
    );
}

#[ui_kit::gpui::test]
fn the_stop_line_brings_the_notice_back_after_it_was_closed(cx: &mut TestAppContext) {
    let root = sample_project("memory-stop-line");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let window = window_with_status(cx, root, statuses, physical_bound());
    sender.unbounded_send(STOPPED.to_owned()).unwrap();
    wait_for_signal(cx, "the notice of the stop", |cx| {
        notification(cx, window).is_some()
    });
    draw(cx, window);
    click(cx, window, "notification-close");
    let closed = serial(cx, window);

    draw(cx, window);
    click(cx, window, "st-language");
    assert_ne!(
        serial(cx, window),
        closed,
        "a click on the stop line shows the notice again"
    );
    assert!(notification(cx, window).is_some_and(|(title, _)| title.contains("stopped")));
}

#[ui_kit::gpui::test]
fn a_stop_that_does_not_restart_by_itself_says_so_and_a_commit_names_the_page_file(
    cx: &mut TestAppContext,
) {
    let root = sample_project("memory-stop-manual");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let window = window_with_status(cx, root, statuses, commit_bound());
    sender.unbounded_send(MANUAL.to_owned()).unwrap();
    wait_for_signal(cx, "the notice of the manual stop", |cx| {
        notification(cx, window).is_some()
    });
    let (_, text) = notification(cx, window).expect("the stop is a notice");
    assert!(text.contains("does not start by itself"), "{text}");
    assert!(text.contains("Free memory with the"), "{text}");
    assert!(text.contains("512 MB of 18432 MB"), "{text}");
}

#[ui_kit::gpui::test]
fn a_virtual_machine_warns_once_when_memory_stays_low_for_the_hold(cx: &mut TestAppContext) {
    let root = sample_project("memory-stop-vm");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let machine = MachineReading {
        memory: Some(physical_bound()),
        virtual_machine: true,
    };
    let window = window_with_machine(cx, root, statuses, machine);
    sender.unbounded_send("Fixture: ready".to_owned()).unwrap();
    wait_for_signal(cx, "the ready line", |cx| {
        status(cx, window).as_deref() == Some("Fixture: ready")
    });

    // Low for 14 polls is less than the hold of 30 seconds: no warning yet.
    poll(cx, 14);
    assert!(
        notification(cx, window).is_none(),
        "a dip shorter than the hold shows no warning"
    );

    // One poll of recovery starts the count again.
    cx.update(|cx| {
        cx.set_global(MachineReading {
            memory: Some(roomy()),
            virtual_machine: true,
        })
    });
    poll(cx, 1);
    cx.update(|cx| cx.set_global(machine));
    poll(cx, 14);
    assert!(
        notification(cx, window).is_none(),
        "the count starts again after a recovery"
    );

    poll(cx, 1);
    let (title, text) = notification(cx, window).expect("the hold brings the warning");
    assert_eq!(title, WARNING_TITLE);
    assert!(text.contains("Free memory: 400 MB of 16384 MB."), "{text}");
    assert!(
        text.contains("below 512 MB and below 5 % of the total"),
        "{text}"
    );
    assert_eq!(buttons(cx, window), ["Memory Settings…", "Close"]);

    // The warning comes once per session: the watch is over after it.
    let shown = serial(cx, window);
    poll(cx, 20);
    assert_eq!(serial(cx, window), shown, "the warning comes once");
}

#[ui_kit::gpui::test]
fn a_memory_check_the_user_chose_on_a_virtual_machine_gets_no_warning(cx: &mut TestAppContext) {
    let root = sample_project("memory-stop-chosen");
    let (sender, statuses) = futures_channel::mpsc::unbounded();
    let machine = MachineReading {
        memory: Some(physical_bound()),
        virtual_machine: true,
    };
    let window = window_with_machine(cx, root, statuses, machine);
    cx.update(|cx| {
        cx.set_global(Settings {
            memory_low_check: Some(false),
            ..Settings::default()
        });
    });
    sender.unbounded_send("Fixture: ready".to_owned()).unwrap();
    wait_for_signal(cx, "the ready line", |cx| {
        status(cx, window).as_deref() == Some("Fixture: ready")
    });

    poll(cx, 20);
    assert!(
        notification(cx, window).is_none(),
        "a memory check that the user chose leaves no warning"
    );
}

//! The steps of `--script` that press a stripe button, check what is on screen and
//! close the window: the smoke file of the release runs parses, and a window of the
//! test platform presses its buttons like the mouse of a scripted run.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use ui_kit::ScriptSpots;
use ui_kit::gpui;
use ui_kit::gpui::{
    AppContext, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput,
    TestAppContext, WindowHandle,
};
use workspace::{LOG_TOOL, PROJECT_TOOL, Workspace, view_render_count};

use crate::script::{Step, parse_script};
use crate::window_tests::{bounds_of, draw, open_sample_window};
use crate::window_tests_cached_views::frame;

fn run_script(cx: &mut TestAppContext, window: WindowHandle<Workspace>, script: &str) {
    window
        .update(cx, |_workspace, window, cx| {
            crate::script::start(script, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
}

fn tool_open(cx: &TestAppContext, window: WindowHandle<Workspace>, tool: &str) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.tool_open(tool))
        .unwrap()
}

/// Runs the script of the window to its end, drawing the frames a step waits for.
fn wait_for(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        cx.run_until_parked();
        if check(cx) {
            return;
        }
        // A closed window has nothing to draw, and its checks never wait for a frame.
        if window.root(cx).is_ok() {
            draw(cx, window);
        }
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
        if check(cx) {
            return;
        }
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the test waited for {what} in vain"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn report_file(name: &str) -> PathBuf {
    test_support::scratch(name).join("report.txt")
}

/// Presses the left button in the middle of an element, without the real cursor.
fn press(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) {
    let position = bounds_of(cx, window, selector).center();
    cx.update_window(window.into(), |_, window, cx| {
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                position,
                modifiers: Modifiers::default(),
                button: MouseButton::Left,
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent {
                position,
                modifiers: Modifiers::default(),
                button: MouseButton::Left,
                click_count: 1,
            }),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
}

fn holds(cx: &TestAppContext, name: &str) -> bool {
    cx.read(|cx| {
        cx.try_global::<ScriptSpots>()
            .is_some_and(|spots| spots.holds(name))
    })
}

#[test]
fn parses_the_click_and_check_steps() {
    let steps = parse_script(
        "click:stripe-s-ai\nexpect:places-empty;expect-not:places-popup\
         \nexpect-tool:terminal;expect-no-tool:ai;close-notification;close-window",
    )
    .unwrap();
    assert_eq!(
        steps,
        [
            Step::Click("stripe-s-ai".into()),
            Step::Expect("places-empty".into(), true),
            Step::Expect("places-popup".into(), false),
            Step::ExpectTool("terminal".into(), true),
            Step::ExpectTool("ai".into(), false),
            Step::CloseNotification,
            Step::CloseWindow,
        ]
    );
    assert!(parse_script("click:").is_err(), "click needs a selector");
    assert!(parse_script("expect").is_err(), "expect needs a selector");
    assert!(
        parse_script("expect-tool:").is_err(),
        "expect-tool needs a tool"
    );
}

#[test]
fn the_release_script_parses() {
    let path = release_script();
    let text = fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("{} is not read: {error}", path.display()));
    let steps = parse_script(&text).unwrap_or_else(|error| panic!("{error}"));
    assert!(
        steps.len() > 100,
        "the smoke script covers the stripes: {} steps",
        steps.len()
    );
    let clicks: Vec<&str> = steps
        .iter()
        .filter_map(|step| match step {
            Step::Click(selector) => Some(selector.as_str()),
            _ => None,
        })
        .collect();
    for selector in [
        "stripe-s-notifications",
        "stripe-s-ai",
        "stripe-s-tasks",
        "stripe-s-bookmarks",
        "stripe-s-project",
        "stripe-s-commit",
        "stripe-s-log",
        "stripe-s-structure",
        "stripe-s-run",
        "stripe-s-debug",
        "stripe-s-terminal",
        "stripe-s-services",
        "stripe-s-todo",
        "stripe-s-problems",
        "stripe-s-usages",
    ] {
        assert!(
            clicks.contains(&selector),
            "{selector} is pressed by the smoke script"
        );
    }
    assert!(
        steps
            .iter()
            .any(|step| matches!(step, Step::Expect(_, true))),
        "the script checks what opened"
    );
    assert!(
        steps
            .iter()
            .any(|step| matches!(step, Step::Show(tool) if tool == "terminal")),
        "the script opens the terminal"
    );
    // A notification of the window's own must not shift the checks of the balloon: the
    // script closes the balloon before it presses the button that shows it.
    let first_notification = steps
        .iter()
        .position(|step| matches!(step, Step::Click(name) if name == "stripe-s-notifications"))
        .expect("the script presses the notifications button");
    assert!(
        steps[..first_notification].contains(&Step::CloseNotification)
            || steps[..first_notification].contains(&Step::HideTools),
        "the balloon is closed before the first press"
    );
    // The window may have saved a tool window open: the script brings it to a known
    // state before it checks the buttons of the left stripe.
    let first_project = steps
        .iter()
        .position(|step| matches!(step, Step::Click(name) if name == "stripe-s-project"))
        .expect("the script presses the project button");
    assert!(
        steps[..first_project].contains(&Step::HideTools),
        "the tool windows are closed before the project button"
    );
    assert_eq!(
        steps.last(),
        Some(&Step::CloseWindow),
        "the script closes the window last"
    );
}

fn release_script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/smoke/release.script")
}

#[ui_kit::gpui::test]
fn clicking_a_stripe_button_opens_its_tool_window(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "script-click");
    run_script(cx, window, "click:stripe-s-log");
    wait_for(cx, window, "the log tool window", |cx| {
        tool_open(cx, window, LOG_TOOL)
    });
    assert!(
        tool_open(cx, window, LOG_TOOL),
        "the press opens the tool window of the button"
    );
}

#[ui_kit::gpui::test]
fn a_check_writes_its_line_and_the_summary_to_the_report(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "script-report");
    let file = report_file("script-report");
    let script = format!(
        "report:{};expect:places-empty;expect-not:places-popup;expect-tool:project;expect-no-tool:terminal",
        file.display()
    );
    run_script(cx, window, &script);

    let expected = "summary: 3 ok, 1 failed";
    wait_for(cx, window, expected, |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains(expected))
    });
    let text = fs::read_to_string(&file).expect("the report is written");
    assert!(
        text.contains("FAIL expect places-empty"),
        "a popup that is not open fails the check: {text}"
    );
    assert!(text.contains("ok expect-not places-popup"));
    assert!(text.contains("ok expect-tool project"));
    assert!(text.contains("ok expect-no-tool terminal"));
    assert!(
        text.trim_end().ends_with(expected),
        "the summary is the last line: {text}"
    );
}

#[ui_kit::gpui::test]
fn a_report_makes_the_folders_it_needs(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "script-report-folders");
    let file = test_support::scratch("script-report-folders")
        .join("missing")
        .join("nested")
        .join("report.txt");
    assert!(
        !file.exists(),
        "the report starts in a folder that is not there"
    );
    let script = format!("report:{};expect-tool:project", file.display());
    run_script(cx, window, &script);

    let expected = "summary: 1 ok, 0 failed";
    wait_for(cx, window, expected, |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains(expected))
    });
    let text = fs::read_to_string(&file).expect("the report is written into the new folders");
    assert!(text.contains("ok expect-tool project"), "{text}");
}

#[ui_kit::gpui::test]
fn close_window_closes_the_window_and_writes_the_report(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "script-close");
    let file = report_file("script-close");
    let script = format!("report:{};close-window", file.display());
    run_script(cx, window, &script);

    wait_for(cx, window, "the closed window", |cx| {
        window.root(cx).is_err()
    });
    assert!(
        window.root(cx).is_err(),
        "the step closes the window as its close button does"
    );
    let text = fs::read_to_string(&file).expect("the report is written");
    assert!(text.contains("ok close-window"), "{text}");
    assert!(
        text.trim_end().ends_with("summary: 1 ok, 0 failed"),
        "the summary is the last line: {text}"
    );
}

#[ui_kit::gpui::test]
fn a_click_of_an_element_without_a_spot_fails_the_step(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "script-no-element");
    let file = report_file("script-no-element");
    let script = format!("report:{};click:no-such-element", file.display());
    run_script(cx, window, &script);

    let expected = "summary: 0 ok, 1 failed";
    wait_for(cx, window, expected, |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains(expected))
    });
    let text = fs::read_to_string(&file).expect("the report is written");
    assert!(
        text.contains("FAIL click no-such-element"),
        "the missing element is an error of the step: {text}"
    );
}

/// The stripes of the smoke file, without the live terminal at its end, run on the test
/// window: the presses, the popups, the balloon and the tool windows of every button.
#[ui_kit::gpui::test]
fn the_stripes_of_the_release_script_pass(cx: &mut TestAppContext) {
    let text = fs::read_to_string(release_script()).unwrap();
    let stripes: Vec<&str> = text
        .split("# A live terminal")
        .next()
        .unwrap_or_default()
        .lines()
        .filter(|line| !line.trim_start().starts_with("report:"))
        .collect();
    let (_, window) = open_sample_window(cx, "script-stripes");
    let file = report_file("script-stripes");
    let script = format!("report:{}\n{}", file.display(), stripes.join("\n"));
    run_script(cx, window, &script);

    let expected = "expect-no-tool usages";
    wait_for(cx, window, "the end of the smoke script", |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains(expected))
    });
    let text = fs::read_to_string(&file).expect("the report is written");
    assert!(
        !text.contains("FAIL "),
        "every press of the smoke script shows its window: {text}"
    );
    assert!(
        text.trim_end()
            .lines()
            .last()
            .is_some_and(|line| line.starts_with("summary: ") && line.ends_with("0 failed")),
        "the smoke script ends without a failure: {text}"
    );
}

/// The Project block of the smoke file starts from a known window: `hide-tools` closes
/// what a saved state left open, so the same block passes whether the window starts
/// with the Project tool open or closed.
#[ui_kit::gpui::test]
fn the_project_block_passes_from_either_saved_state(cx: &mut TestAppContext) {
    let text = fs::read_to_string(release_script()).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert!(
        lines.iter().any(|line| line.trim() == "hide-tools"),
        "the smoke file brings the window to a known state"
    );
    let start = lines
        .iter()
        .position(|line| line.trim() == "click:stripe-s-project")
        .expect("the smoke file presses the project button");
    let end = lines
        .iter()
        .position(|line| line.trim() == "click:stripe-s-commit")
        .expect("the smoke file goes on to the next button");
    let block = format!("hide-tools\n{}", lines[start..end].join("\n"));

    for (name, project_was_open) in [
        ("script-project-open", true),
        ("script-project-closed", false),
    ] {
        let (_, window) = open_sample_window(cx, name);
        if !project_was_open {
            window
                .update(cx, |workspace, window, cx| {
                    workspace.toggle_tool(PROJECT_TOOL, window, cx)
                })
                .unwrap();
            assert!(
                !tool_open(cx, window, PROJECT_TOOL),
                "the window starts with the Project tool closed"
            );
        }
        let file = report_file(name);
        run_script(cx, window, &format!("report:{}\n{block}", file.display()));

        let expected = "summary: 4 ok, 0 failed";
        wait_for(cx, window, expected, |_| {
            fs::read_to_string(&file).is_ok_and(|text| text.contains(expected))
        });
        let text = fs::read_to_string(&file).expect("the report is written");
        assert!(
            !text.contains("FAIL "),
            "the block passes from either saved state: {text}"
        );
    }
}

/// The live window draws a frame of its own change, not of the test helper's refresh:
/// the cached stripes keep their last frame and the balloon, which is deferred, draws
/// after the rest. Its spot has to be in that frame all the same.
#[ui_kit::gpui::test]
fn a_deferred_balloon_is_seen_in_a_live_frame(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "script-live-frame");
    run_script(cx, window, "");
    draw(cx, window);
    assert!(!holds(cx, "notification"), "the balloon starts closed");

    press(cx, window, "stripe-s-notifications");
    frame(cx, window);
    assert!(
        holds(cx, "notification"),
        "the deferred balloon records its spot in the frame it draws in"
    );

    // The next frame does not refresh the views: the cached pieces keep their last
    // frame while the root and the deferred balloon draw again, as in a live window
    // between two presses.
    let title = cx.read(|cx| view_render_count(cx, "title"));
    frame(cx, window);
    assert!(
        holds(cx, "notification"),
        "a frame that keeps the cached views records the balloon as well"
    );
    assert_eq!(
        cx.read(|cx| view_render_count(cx, "title")),
        title,
        "the frame keeps the cached views, as the live window does"
    );

    press(cx, window, "stripe-s-notifications");
    frame(cx, window);
    assert!(
        !holds(cx, "notification"),
        "the spot goes with the frame the balloon closed in"
    );
}

/// A live window may show a notification of its own, such as a language server that
/// failed to import the project. The block of the smoke file that works the balloon
/// then checks the opposite of what it opened, so the script closes the balloon first.
#[ui_kit::gpui::test]
fn a_notification_open_before_the_script_does_not_shift_the_checks(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "script-open-notification");
    window
        .update(cx, |workspace, _, cx| {
            workspace.notify_user("Language Server", "import failed: no build file", cx)
        })
        .unwrap();
    draw(cx, window);

    // The same presses without the close: this is the live failure, and it shows that
    // the checks read the balloon, not a stale spot.
    let shifted = report_file("script-open-notification-shifted");
    let block = "click:stripe-s-notifications;expect:notification;\
                 click:stripe-s-notifications;expect-not:notification";
    run_script(cx, window, &format!("report:{};{block}", shifted.display()));
    let expected = "summary: 2 ok, 2 failed";
    wait_for(cx, window, expected, |_| {
        fs::read_to_string(&shifted).is_ok_and(|text| text.contains(expected))
    });

    // The block the smoke file runs, from a balloon the window opened on its own.
    let file = report_file("script-open-notification");
    run_script(
        cx,
        window,
        &format!("report:{};close-notification;{block}", file.display()),
    );
    let expected = "summary: 5 ok, 0 failed";
    wait_for(cx, window, expected, |_| {
        fs::read_to_string(&file).is_ok_and(|text| text.contains(expected))
    });
    let text = fs::read_to_string(&file).expect("the report is written");
    assert!(text.contains("ok close-notification"), "{text}");
    assert!(text.contains("ok expect notification"), "{text}");
    assert!(text.contains("ok expect-not notification"), "{text}");
}

/// A window no script drives keeps no table of spots: the elements that a script names
/// are empty and record nothing.
#[ui_kit::gpui::test]
fn a_window_without_a_script_has_no_spots(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "script-none");
    draw(cx, window);
    assert!(
        cx.read(|cx| !cx.has_global::<ScriptSpots>()),
        "a window without a script keeps no spots"
    );

    // A script installs the table, and from then on the window records the spots.
    run_script(cx, window, "expect-tool:project");
    draw(cx, window);
    assert!(
        cx.read(|cx| cx.has_global::<ScriptSpots>()),
        "a script installs the table of spots"
    );
    assert!(holds(cx, "stripe-s-project"));
}

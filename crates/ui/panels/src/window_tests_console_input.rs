//! The Run console takes what the user types: a line reaches the program, output that
//! arrives while the user types stays above the input line, the up and down keys walk
//! the lines sent before, and Send EOF gives the program the end of its input.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use runner::{RunConfig, RunConfigProvider, RunPlan};
use ui_kit::gpui;
use ui_kit::gpui::{MouseButton, TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Modifiers, px};
use workspace::Workspace;

use crate::run::RunPanel;
use crate::window_tests::{bounds_of, click, draw, press, sample_project, type_text};
use crate::window_tests_run::{open_with, run_panel, setup, wait_for};

/// A program that prints three lines while it waits, reads one line of its input, prints
/// what it read, and then waits for the end of the input.
const READER: &str = "$i = 0; while ($i -lt 3) { Write-Output ('tick' + $i); \
Start-Sleep -Milliseconds 150; $i++ }; \
$line = [Console]::In.ReadLine(); Write-Output ('got:' + $line); \
[Console]::In.ReadToEnd() | Out-Null; Write-Output 'ended'";

/// A program of the project that reads its standard input, as `Scanner(System.in)` does.
struct ReadingRun;

impl RunConfigProvider for ReadingRun {
    fn id(&self) -> &'static str {
        "fixture"
    }

    fn discover(&self, _: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(vec![RunConfig {
            name: "Reading".into(),
            provider: "fixture".into(),
            assemble_first: false,
            settings: Default::default(),
        }])
    }

    fn plan(&self, _: &RunConfig, _: &ProjectModel) -> base::Result<RunPlan> {
        Ok(RunPlan {
            before: None,
            program: ProcessSpec::new("powershell.exe")
                .args(["-NoProfile", "-Command", READER])
                .input(),
            grace: Duration::from_secs(1),
        })
    }
}

fn open(cx: &mut TestAppContext, root: &Path) -> WindowHandle<Workspace> {
    let mut setup = setup(root);
    setup.providers = vec![Arc::new(ReadingRun)];
    open_with(cx, root, setup)
}

fn text(cx: &mut TestAppContext, panel: &gpui::Entity<RunPanel>) -> Vec<String> {
    panel.read_with(cx, |panel, _| panel.text())
}

fn input_text(cx: &TestAppContext, panel: &gpui::Entity<RunPanel>) -> String {
    panel.read_with(cx, |panel, _| panel.input_text().to_owned())
}

/// Starts the run and waits until its console waits for a line.
fn started(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    panel: &gpui::Entity<RunPanel>,
) {
    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });
    press(cx, window, "shift-f10");
    wait_for(cx, "the program to wait for input", |cx| {
        text(cx, panel).iter().any(|line| line.contains("tick0"))
            && panel.read_with(cx, |panel, _| panel.takes_input())
    });
    draw(cx, window);
}

fn right_click_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    position: gpui::Point<gpui::Pixels>,
) {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.simulate_mouse_down(position, MouseButton::Right, Modifiers::default());
    visual.simulate_mouse_up(position, MouseButton::Right, Modifiers::default());
    cx.run_until_parked();
    draw(cx, window);
}

#[ui_kit::gpui::test]
fn a_typed_line_reaches_the_program_and_output_does_not_break_it(cx: &mut TestAppContext) {
    let root = sample_project("console-input");
    let window = open(cx, &root);
    let panel = run_panel(cx);
    started(cx, window, &panel);

    let line = bounds_of(cx, window, "run-console-input");
    assert!(
        line.size.height > px(0.),
        "the console shows the input line"
    );

    type_text(cx, window, "hel");
    assert_eq!(input_text(cx, &panel), "hel");

    // Output that arrives while the user types goes above the input line.
    wait_for(cx, "the output during typing", |cx| {
        text(cx, &panel).iter().any(|line| line.contains("tick2"))
    });
    assert_eq!(
        input_text(cx, &panel),
        "hel",
        "the output left the line alone"
    );
    type_text(cx, window, "lo");
    assert_eq!(input_text(cx, &panel), "hello");

    press(cx, window, "enter");
    wait_for(cx, "the program to read the line", |cx| {
        text(cx, &panel)
            .iter()
            .any(|line| line.contains("got:hello"))
    });
    assert!(
        text(cx, &panel).iter().any(|line| line == "hello"),
        "the typed line stays in the console: {:?}",
        text(cx, &panel)
    );
    assert_eq!(input_text(cx, &panel), "", "Enter clears the line");

    // The history of this tab: Up walks back through the lines sent here.
    type_text(cx, window, "second");
    press(cx, window, "enter");
    draw(cx, window);
    press(cx, window, "up");
    assert_eq!(input_text(cx, &panel), "second");
    press(cx, window, "up");
    assert_eq!(input_text(cx, &panel), "hello");
    press(cx, window, "down");
    assert_eq!(input_text(cx, &panel), "second");

    // A multi-line paste sends its complete lines one after another and keeps the rest.
    cx.update(|cx| {
        cx.write_to_clipboard(ui_kit::ClipboardItem::new_string(
            "one\ntwo\nrest".to_owned(),
        ))
    });
    press(cx, window, "ctrl-v");
    let lines = text(cx, &panel);
    assert!(lines.iter().any(|line| line == "one"), "{lines:?}");
    assert!(lines.iter().any(|line| line == "two"), "{lines:?}");
    assert_eq!(input_text(cx, &panel), "rest");

    // Ctrl+D is Send EOF: the program sees the end of its input.
    press(cx, window, "ctrl-d");
    wait_for(cx, "the program to see the end of its input", |cx| {
        text(cx, &panel).iter().any(|line| line.contains("ended"))
    });
    assert!(
        !panel.read_with(cx, |panel, _| panel.takes_input()),
        "nothing is typed after the end of the input"
    );
}

#[ui_kit::gpui::test]
fn send_eof_in_the_console_menu_ends_the_input_of_the_program(cx: &mut TestAppContext) {
    let root = sample_project("console-eof");
    let window = open(cx, &root);
    let panel = run_panel(cx);
    started(cx, window, &panel);

    let line = bounds_of(cx, window, "run-console-input");
    right_click_at(cx, window, line.center());
    let entry = bounds_of(cx, window, "menu-0");
    assert!(
        entry.size.height > px(0.),
        "the console menu has a row: Send EOF"
    );
    click(cx, window, "menu-0");

    wait_for(cx, "the program to see the end of its input", |cx| {
        text(cx, &panel).iter().any(|line| line.contains("ended"))
    });
    assert!(
        !panel.read_with(cx, |panel, _| panel.takes_input()),
        "nothing is typed after the end of the input"
    );
    wait_for(cx, "the end of the program", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().running)
    });
}

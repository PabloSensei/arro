//! The keys of a text field of a tool window: the field keeps the printing characters, its
//! editing keys and Ctrl+A, C, V, X and Z, while a key of the window such as Ctrl+Shift+A,
//! Alt+1 or Ctrl+E leaves the field and runs its binding.

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::{ENDPOINTS_TOOL, PROJECT_TOOL, Workspace};

use crate::endpoints::ActiveEndpoints;
use crate::git_log::ActiveGitLog;
use crate::palette::Palette;
use crate::window_tests::{
    click, draw, git_repository, open_sample_window, open_window, press, type_text,
};
use crate::window_tests_git::{git_state, has_selector, wait_for, wait_for_repository};

/// Opens the Endpoints window and clicks its filter, so that the keys of the field go to
/// the window through it.
fn open_endpoints(cx: &mut TestAppContext, name: &str) -> WindowHandle<Workspace> {
    let (_, window) = open_sample_window(cx, name);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(ENDPOINTS_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    click(cx, window, "endpoints-filter");
    draw(cx, window);
    window
}

fn endpoints_text(cx: &TestAppContext) -> String {
    cx.read(|cx| {
        cx.global::<ActiveEndpoints>()
            .0
            .read(cx)
            .filter_text()
            .to_owned()
    })
}

fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Entity<Palette> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .unwrap()
        .expect("the search popup is open")
        .downcast::<Palette>()
        .unwrap_or_else(|_| panic!("the open popup is the search popup"))
}

fn palette_text(cx: &TestAppContext, window: WindowHandle<Workspace>) -> String {
    palette(cx, window).read_with(cx, |palette, _| palette.query_field().text.clone())
}

#[ui_kit::gpui::test]
fn ctrl_shift_a_leaves_the_endpoints_field_and_opens_find_action(cx: &mut TestAppContext) {
    let window = open_endpoints(cx, "tool-keys-find-action");

    press(cx, window, "ctrl-shift-a");
    draw(cx, window);
    assert!(
        has_selector(cx, window, "palette"),
        "Ctrl+Shift+A opens Find Action from the field of Endpoints"
    );

    type_text(cx, window, "Bookmarks");
    draw(cx, window);
    assert_eq!(
        palette_text(cx, window),
        "Bookmarks",
        "the text goes to the field of Find Action"
    );
    assert_eq!(
        endpoints_text(cx),
        "",
        "the text does not reach the field of Endpoints"
    );
}

#[ui_kit::gpui::test]
fn alt_1_leaves_the_endpoints_field_and_opens_the_project(cx: &mut TestAppContext) {
    let window = open_endpoints(cx, "tool-keys-project");
    // The project window starts open, so the key would close it rather than open it.
    window
        .update(cx, |workspace, window, cx| {
            if workspace.tool_open(PROJECT_TOOL) {
                workspace.toggle_tool(PROJECT_TOOL, window, cx);
            }
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    click(cx, window, "endpoints-filter");
    draw(cx, window);

    press(cx, window, "alt-1");
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(PROJECT_TOOL))
            .unwrap(),
        "Alt+1 opens the Project window from the field of Endpoints"
    );
    assert_eq!(endpoints_text(cx), "", "the field of Endpoints stays empty");
}

#[ui_kit::gpui::test]
fn ctrl_e_leaves_the_endpoints_field_and_opens_recent_files(cx: &mut TestAppContext) {
    let window = open_endpoints(cx, "tool-keys-recent");

    press(cx, window, "ctrl-e");
    draw(cx, window);
    assert!(
        has_selector(cx, window, "palette"),
        "Ctrl+E opens Recent Files from the field of Endpoints"
    );

    type_text(cx, window, "main");
    draw(cx, window);
    assert_eq!(
        palette_text(cx, window),
        "main",
        "the text goes to the field of Recent Files"
    );
    assert_eq!(
        endpoints_text(cx),
        "",
        "the text does not reach the field of Endpoints"
    );
}

#[ui_kit::gpui::test]
fn typing_and_backspace_work_in_the_endpoints_field(cx: &mut TestAppContext) {
    let window = open_endpoints(cx, "tool-keys-typing");

    type_text(cx, window, "vets");
    assert_eq!(endpoints_text(cx), "vets", "the letters reach the field");
    press(cx, window, "backspace");
    assert_eq!(endpoints_text(cx), "vet", "Backspace removes a letter");
    press(cx, window, "ctrl-a");
    type_text(cx, window, "x");
    assert_eq!(
        endpoints_text(cx),
        "x",
        "Ctrl+A selects the text that the typing replaces"
    );
    press(cx, window, "backspace");
    assert_eq!(endpoints_text(cx), "", "Backspace empties the field");
}

#[ui_kit::gpui::test]
fn ctrl_shift_a_leaves_the_git_log_filter_and_opens_find_action(cx: &mut TestAppContext) {
    let root = git_repository("tool-keys-git-log");
    let window = open_window(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);

    press(cx, window, "alt-9");
    wait_for(cx, "the log", |cx| has_selector(cx, window, "log-filter"));
    click(cx, window, "log-filter");
    draw(cx, window);

    press(cx, window, "ctrl-shift-a");
    draw(cx, window);
    assert!(
        has_selector(cx, window, "palette"),
        "Ctrl+Shift+A opens Find Action from the filter of Git Log"
    );

    type_text(cx, window, "Local History");
    draw(cx, window);
    assert_eq!(
        palette_text(cx, window),
        "Local History",
        "the text goes to the field of Find Action"
    );
    let filter = cx.read(|cx| {
        cx.global::<ActiveGitLog>()
            .0
            .read(cx)
            .filter_text()
            .to_owned()
    });
    assert_eq!(filter, "", "the text does not reach the filter of Git Log");
}

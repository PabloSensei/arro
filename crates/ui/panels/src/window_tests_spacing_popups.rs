//! Popups and dialogs on the spacing scale: one field and one row inset in the search
//! popups, one padding in the dialogs, and the configuration menu under its button.

use std::sync::Arc;

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::metrics::{CONTROL_HEIGHT, DIALOG_PADDING, POPUP_PADDING, SPACE_XS};
use ui_kit::{point, px};
use workspace::Workspace;

use crate::window_tests::{
    bounds_of, click, draw, open_palette, open_path, open_sample_window, press, sample_project,
    type_text, wait_for_file_index,
};
use crate::window_tests_run::{EchoRun, open_with, run_panel, setup as run_setup, wait_for};

/// The hairline of `popup_frame`, outside the padding it measures from.
const FRAME: f32 = 1.;

/// The popups that share the search field: Search Everywhere, Go to File, Find Action.
#[ui_kit::gpui::test]
fn the_search_popups_share_the_field_height_and_the_row_inset(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "spacing-search");
    wait_for_file_index(cx);
    draw(cx, window);

    for command in [keys::SEARCH_EVERYWHERE, keys::GOTO_FILE, keys::FIND_ACTION] {
        open_palette(cx, window, command);
        type_text(cx, window, "a");
        draw(cx, window);
        let popup = bounds_of(cx, window, "palette");
        let field = bounds_of(cx, window, "palette-input");
        assert_eq!(
            field.size.height,
            px(CONTROL_HEIGHT),
            "{command} keeps the field of one height"
        );
        assert_eq!(
            field.left() - popup.left(),
            px(POPUP_PADDING + FRAME),
            "{command} keeps the field one inset from the frame"
        );

        let row = bounds_of(cx, window, "palette-row-0");
        assert_eq!(
            row.left() - popup.left(),
            px(POPUP_PADDING + FRAME),
            "{command} keeps the rows one inset from the frame"
        );
        press(cx, window, "escape");
    }
}

/// Settings and Reformat File keep the content `DIALOG_PADDING` from the frame, and the
/// buttons of the dialog share one height.
#[ui_kit::gpui::test]
fn the_dialogs_keep_the_padding_and_one_button_height(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "spacing-dialogs");

    click(cx, window, "t-settings");
    draw(cx, window);
    let settings = bounds_of(cx, window, "settings");
    let title = bounds_of(cx, window, "settings-title");
    assert_eq!(
        title.left() - settings.left(),
        px(DIALOG_PADDING + FRAME),
        "the Settings title starts at the dialog padding"
    );
    press(cx, window, "escape");
    draw(cx, window);

    open_path(cx, window, &root.join("src").join("main.txt"));
    press(cx, window, "ctrl-alt-shift-l");
    draw(cx, window);
    let dialog = bounds_of(cx, window, "reformat-dialog");
    let title = bounds_of(cx, window, "reformat-title");
    assert_eq!(
        title.left() - dialog.left(),
        px(DIALOG_PADDING + FRAME),
        "the Reformat File title starts at the dialog padding"
    );
    let cancel = bounds_of(cx, window, "reformat-cancel");
    let run = bounds_of(cx, window, "reformat-run");
    assert_eq!(
        cancel.size.height, run.size.height,
        "the dialog buttons share one height"
    );
    assert_eq!(run.size.height, px(CONTROL_HEIGHT));
}

/// Alt+Shift+F10 opens the configuration menu in the place a click on `t-config` opens it,
/// highlights the first row, and Enter runs that configuration.
#[ui_kit::gpui::test]
fn alt_shift_f10_opens_the_configuration_menu_under_its_button(cx: &mut TestAppContext) {
    let root = sample_project("spacing-choose-run");
    let mut setup = run_setup(&root);
    setup.providers = vec![Arc::new(EchoRun)];
    let window = open_with(cx, &root, setup);
    let panel = run_panel(cx);
    wait_for(cx, "a run configuration", |cx| {
        !panel.read_with(cx, |panel, _| panel.state().configs.is_empty())
    });

    let button = bounds_of(cx, window, "t-config");
    let expected = point(button.left(), button.bottom() + px(SPACE_XS));

    click(cx, window, "t-config");
    draw(cx, window);
    let clicked = menu_position(cx, window);
    assert_eq!(clicked, Some(expected), "the click opens the menu below");
    press(cx, window, "escape");
    draw(cx, window);

    press(cx, window, "alt-shift-f10");
    draw(cx, window);
    let opened = menu_position(cx, window);
    assert_eq!(
        opened, clicked,
        "Alt+Shift+F10 opens the menu in the same place"
    );
    let highlight = window
        .read_with(cx, |workspace, _| workspace.menu_highlight())
        .unwrap();
    assert_eq!(highlight, Some(vec![0]), "the first row is highlighted");

    press(cx, window, "enter");
    wait_for(cx, "the configuration to run", |cx| {
        panel
            .read_with(cx, |panel, _| panel.text())
            .iter()
            .any(|line| line.contains("hello from run"))
    });
}

fn menu_position(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> Option<gpui::Point<gpui::Pixels>> {
    window
        .read_with(cx, |workspace, _| workspace.menu_position())
        .unwrap()
}

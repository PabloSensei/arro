//! The fixes of the live check of 2026-11-16: a letter in a popup asks the window for
//! the next frame, the paste confirmation of the terminal has its buttons and stands by
//! the terminal, a shortcut leaves no character in the text, and Ctrl+B on a declaration
//! shows its uses.

use std::time::Duration;

use lang_api::SourceSpan;
use ui_kit::ClipboardItem;
use ui_kit::gpui;
use ui_kit::gpui::{AppContext, Entity, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::palette::Palette;
use crate::window_tests::{
    active_path, bounds_of, buffer_of, draw, editor_of, open_palette, open_path,
    open_sample_window, press, type_text,
};
use crate::window_tests_git::has_selector;
use crate::window_tests_lang::{FakeLanguage, open_window_with, project, span_in, uri_of};
use crate::window_tests_terminal_tabs::{open_terminal, session_of};

/// The open search popup of the window.
fn palette_of(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Entity<Palette> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .expect("the window is open")
        .expect("the search popup is open")
        .downcast::<Palette>()
        .unwrap_or_else(|_| panic!("the open popup is the search popup"))
}

/// One frame that honors the cache of every view, as the platform's frame demand draws
/// it, without the `Window::refresh` of the `draw` helper of the window tests.
fn frame(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    cx.update_window(window.into(), |_, window, cx| {
        window.draw(cx).clear(cx);
    })
    .unwrap();
}

fn settle(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    for _ in 0..4 {
        cx.run_until_parked();
        std::thread::sleep(Duration::from_millis(20));
        frame(cx, window);
    }
}

#[ui_kit::gpui::test]
fn a_letter_in_the_search_popup_asks_for_the_frame_that_shows_it(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "live-popup-frame");
    open_path(cx, window, &root.join("src").join("main.txt"));
    open_palette(cx, window, "workspace.goto_class");
    settle(cx, window);
    let palette = palette_of(cx, window);
    let before = palette.read_with(cx, |palette, _| palette.frames_asked);

    type_text(cx, window, "Own");
    assert_eq!(
        palette.read_with(cx, |palette, _| palette.query_field().text.clone()),
        "Own",
        "the letters reach the field"
    );
    assert!(
        palette.read_with(cx, |palette, _| palette.frames_asked) > before,
        "the letter asks the window for the frame that shows the field"
    );

    press(cx, window, "escape");
    assert!(
        !has_selector(cx, window, "palette"),
        "Escape closes the popup on screen"
    );
}

#[ui_kit::gpui::test]
fn opening_a_row_asks_for_the_frame_that_takes_the_popup_off_the_screen(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "live-popup-open-frame");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    open_palette(cx, window, "workspace.recent_files");
    settle(cx, window);
    let palette = palette_of(cx, window);
    let before = palette.read_with(cx, |palette, _| palette.frames_asked);

    press(cx, window, "enter");
    assert_eq!(
        active_path(cx, window),
        Some(path.clone()),
        "Enter opens the row of the list"
    );
    assert!(
        palette.read_with(cx, |palette, _| palette.frames_asked) > before,
        "opening a row asks for the frame that takes the popup off the screen"
    );
    assert!(!has_selector(cx, window, "palette"), "the popup is gone");
}

#[ui_kit::gpui::test]
fn the_character_of_a_shortcut_never_becomes_text(cx: &mut TestAppContext) {
    let (root, window) = open_sample_window(cx, "live-shortcut-char");
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let text = buffer_of(cx, &view);

    // Ctrl+` opens Quick Switch Scheme. The character of the same press does not reach
    // the text, neither through the key nor through the character message after it.
    press(cx, window, "ctrl-`");
    assert!(
        has_selector(cx, window, "scheme-popup"),
        "Ctrl+` opens the scheme popup"
    );
    assert_eq!(
        buffer_of(cx, &view),
        text,
        "the backtick of the shortcut is not typed"
    );

    press(cx, window, "escape");
    assert_eq!(
        buffer_of(cx, &view),
        text,
        "closing the popup leaves the text as it was"
    );
}

#[ui_kit::gpui::test]
fn ctrl_b_on_a_declaration_goes_to_its_only_use(cx: &mut TestAppContext) {
    let root = project(
        "live-declaration-one-use",
        &[("src/Main.txt", "greet\n"), ("src/Other.txt", "greet\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = FakeLanguage::new();
    // The server names the declaration the caret stands on, and one use elsewhere.
    language.set_definition(&main, 0, 0);
    language.set_references(vec![SourceSpan {
        uri: uri_of(&other),
        span: span_in("greet\n", 0, 5),
    }]);
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    open_path(cx, window, &main);

    press(cx, window, "ctrl-b");
    settle(cx, window);
    assert_eq!(
        active_path(cx, window),
        Some(other.clone()),
        "one use of the declaration opens at once"
    );
}

#[ui_kit::gpui::test]
fn ctrl_b_on_a_declaration_lists_several_uses(cx: &mut TestAppContext) {
    let root = project(
        "live-declaration-uses",
        &[
            ("src/Main.txt", "greet\n"),
            ("src/One.txt", "greet\n"),
            ("src/Two.txt", "greet\n"),
        ],
    );
    let main = root.join("src").join("Main.txt");
    let one = root.join("src").join("One.txt");
    let two = root.join("src").join("Two.txt");
    let language = FakeLanguage::new();
    language.set_definition(&main, 0, 0);
    language.set_references(vec![
        SourceSpan {
            uri: uri_of(&one),
            span: span_in("greet\n", 0, 5),
        },
        SourceSpan {
            uri: uri_of(&two),
            span: span_in("greet\n", 0, 5),
        },
    ]);
    let window = open_window_with(cx, root, language);
    cx.run_until_parked();
    open_path(cx, window, &main);

    press(cx, window, "ctrl-b");
    settle(cx, window);
    assert_eq!(
        active_path(cx, window),
        Some(main.clone()),
        "several uses stay in the file of the declaration"
    );
    assert!(
        has_selector(cx, window, "usages-popup"),
        "several uses open the popup of uses, as Ctrl+Alt+F7 does"
    );
}

#[ui_kit::gpui::test]
fn the_paste_confirmation_has_its_buttons_and_stands_by_the_terminal(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "live-terminal-paste");
    let session = session_of(cx, &terminal);
    let text = "echo paste-one
echo paste-two";
    cx.update(|cx| cx.write_to_clipboard(ClipboardItem::new_string(text.to_string())));

    press(cx, window, "ctrl-v");
    let content = bounds_of(cx, window, "prompt");
    assert!(
        has_selector(cx, window, "prompt-confirm"),
        "the paste question has a Paste button"
    );
    assert!(
        has_selector(cx, window, "prompt-cancel"),
        "the paste question has a Cancel button"
    );
    let viewport = window
        .update(cx, |_, window, _| window.viewport_size())
        .unwrap();
    assert!(
        content.center().y > viewport.center().y,
        "the question stands by the terminal in the lower part of the window, not over the editor"
    );

    // The Paste button of the question sends the text as Enter does.
    draw(cx, window);
    crate::window_tests::click(cx, window, "prompt-confirm");
    crate::window_tests::wait_for_signal(cx, "the pasted lines", |_| {
        let screen = session.screen();
        screen.contains("paste-one") && screen.contains("paste-two")
    });
    assert!(
        !has_selector(cx, window, "prompt"),
        "the question goes after the answer"
    );
}

#[ui_kit::gpui::test]
fn a_cancelled_paste_types_nothing(cx: &mut TestAppContext) {
    let (_, window, terminal) = open_terminal(cx, "live-terminal-paste-cancel");
    let session = session_of(cx, &terminal);
    cx.update(|cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(
            "echo cancel-one
echo cancel-two"
                .to_string(),
        ))
    });

    press(cx, window, "ctrl-v");
    assert!(
        has_selector(cx, window, "prompt"),
        "the paste of two lines asks first"
    );
    // Escape is the Cancel button.
    press(cx, window, "escape");
    assert!(!has_selector(cx, window, "prompt"), "Escape closes it");
    std::thread::sleep(Duration::from_millis(300));
    cx.run_until_parked();
    assert!(
        !session.screen().contains("cancel-one"),
        "a cancelled paste types nothing"
    );
}

#[ui_kit::gpui::test]
fn the_configuration_dialog_is_wide_and_its_fields_stay_above_the_buttons(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "live-run-configs-size");
    open_palette(cx, window, "run.edit_configurations");
    draw(cx, window);
    let dialog = bounds_of(cx, window, "run-configs");
    let viewport = window
        .update(cx, |_, window, _| window.viewport_size())
        .unwrap();
    assert!(
        dialog.size.width >= ui_kit::px(900.),
        "the dialog is as wide as the Settings window: {:?}",
        dialog.size.width
    );
    assert!(
        dialog.size.height > viewport.height * 0.6,
        "the dialog is taller than a few field rows: {:?} of {:?}",
        dialog.size.height,
        viewport.height
    );
    let fields = bounds_of(cx, window, "run-config-fields");
    let ok = bounds_of(cx, window, "run-config-ok");
    assert!(
        fields.bottom() <= ok.top(),
        "the scroll area of the fields ends above the buttons: {:?} and {:?}",
        fields.bottom(),
        ok.top()
    );
    assert!(
        fields.size.height > ui_kit::px(240.),
        "the fields have room to scroll in: {:?}",
        fields.size.height
    );
}

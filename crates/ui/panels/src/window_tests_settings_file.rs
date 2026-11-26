//! A settings file that does not read: a change of a setting applies in the window at once,
//! and the file stays as the user left it.

use std::fs;

use keymap::workspace as keys;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::{SETTINGS_FILE, Settings};

use crate::window_tests::{draw, open_palette, open_sample_window};

#[ui_kit::gpui::test]
fn a_broken_settings_file_stays_as_it_is_and_the_switch_changes_in_the_window(
    cx: &mut TestAppContext,
) {
    let (_, window) = open_sample_window(cx, "settings-broken-file");
    let file = window
        .update(cx, |workspace, _, _| {
            workspace.settings_dir().join(SETTINGS_FILE)
        })
        .unwrap();
    let broken = "{\"show_whitespace\": tru";
    fs::write(&file, broken).unwrap();

    assert!(!cx.read_global::<Settings, _>(|settings, _| settings.show_whitespace));
    open_palette(cx, window, keys::FIND_ACTION);
    cx.simulate_keystrokes(window.into(), "w h i t e s p a c e");
    draw(cx, window);
    cx.simulate_keystrokes(window.into(), "right");
    cx.run_until_parked();
    draw(cx, window);

    assert!(
        cx.read_global::<Settings, _>(|settings, _| settings.show_whitespace),
        "the switch changes at once, in memory"
    );
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        broken,
        "the damaged file is not written"
    );
}

//! T-0374: the fields of New Project start on one vertical line and have the height of the
//! fields of the other forms, as the form of the dialogs does.
//!
//! `cargo test -p panels window_tests_new_project_form`

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use ui_kit::metrics::CONTROL_HEIGHT;

use crate::window_tests::bounds_of;
use crate::window_tests_new_project::{generators, open};

#[ui_kit::gpui::test]
fn the_name_and_the_location_start_on_one_line_and_have_one_height(cx: &mut TestAppContext) {
    let screen = open(cx, "new-project-columns", generators("new-project-columns"));
    let name = bounds_of(cx, screen, "new-project-name");
    let location = bounds_of(cx, screen, "new-project-location");
    assert_eq!(
        name.left(),
        location.left(),
        "the name and the location fields start on one vertical line"
    );
    assert_eq!(
        name.size.height, location.size.height,
        "the name and the location fields have one height"
    );
    assert_eq!(
        f32::from(name.size.height),
        CONTROL_HEIGHT,
        "the fields of the form are CONTROL_HEIGHT high"
    );
}

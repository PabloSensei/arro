//! Where the controls a test names were drawn in the last frame.
//!
//! GPUI keeps the bounds of the `debug_selector` elements of a frame to itself: a test
//! asks about one name and gets one box. The button and row builders here record the
//! box of the control they build, so a check of layout can walk every control of a
//! tool window or of a dialog, whether its selector is a constant or a row number.
//! Nothing is recorded without `test-support`: both the table and the probes go away.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui_kit::prelude::*;
use gpui_kit::*;

/// One control, as the frame drew it.
#[derive(Clone)]
pub struct TestBounds {
    /// The box of the control itself.
    pub bounds: Bounds<Pixels>,
    /// The label the control draws, for a control with a text label.
    pub label: Option<SharedString>,
    /// The width the label takes in the text style of the control.
    pub label_width: Pixels,
}

thread_local! {
    static SHOWN: RefCell<HashMap<String, TestBounds>> = RefCell::new(HashMap::new());
}

/// Forgets every box of the frames drawn before, so that a check reads one frame only.
pub fn clear_test_bounds() {
    SHOWN.with(|shown| shown.borrow_mut().clear());
}

/// Every control the frame recorded, by its name, in no particular order.
pub fn test_bounds() -> Vec<(String, TestBounds)> {
    SHOWN.with(|shown| {
        shown
            .borrow()
            .iter()
            .map(|(name, bounds)| (name.clone(), bounds.clone()))
            .collect()
    })
}

/// A spot over the content of a control, which records the box of that content and the
/// place its label takes. The spot paints nothing and takes no room of the layout.
pub fn bounds_probe(name: impl Into<String>, label: Option<SharedString>) -> AnyElement {
    let name = name.into();
    canvas(
        move |bounds, window, _| {
            let width = match &label {
                Some(text) => {
                    let style = window.text_style();
                    let size = style.font_size.to_pixels(window.rem_size());
                    let run = style.to_run(text.len());
                    let line = window.text_system().shape_line(
                        text.clone(),
                        size,
                        std::slice::from_ref(&run),
                        None,
                    );
                    line.width()
                }
                None => px(0.),
            };
            let control = TestBounds {
                bounds,
                label: label.clone(),
                label_width: width,
            };
            SHOWN.with(|shown| {
                shown.borrow_mut().insert(name.clone(), control);
            });
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

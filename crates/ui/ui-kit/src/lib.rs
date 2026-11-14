//! The only crate that depends on GPUI. Everything above reaches it through this
//! crate: `use ui_kit::*; use ui_kit::prelude::*;`. A module that derives without the
//! glob needs `use ui_kit::gpui;`: GPUI macros expand to `gpui::` paths.

pub use gpui_kit as gpui;
pub use gpui_kit::*;

pub use ::theme::{metrics, motion, snapped_code_size};

mod embedded;
mod motions;
mod palette;
mod popups;
mod rows;
mod script;
mod scrollbars;
#[cfg(any(test, feature = "test-support"))]
mod test_bounds;
mod text_field;
mod widgets;

pub use embedded::{Assets, app, init};
pub use motions::{Slide, smoothstep};
pub use palette::Theme;
pub use popups::*;
// GPUI has a `surface` element of its own on macOS; the popup surface is the one meant.
pub use popups::surface;
pub use rows::*;
pub use script::*;
pub use scrollbars::*;
#[cfg(any(test, feature = "test-support"))]
pub use test_bounds::*;
pub use text_field::{
    TextField, text_field_children, text_field_children_named, text_field_lines,
    text_field_lines_named,
};
pub use widgets::*;

#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static REVEALED: std::cell::RefCell<Vec<std::path::PathBuf>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Shows a file in the file manager through the platform. The test platform has no file
/// manager, so a test build records the call and starts nothing.
pub fn reveal_path(cx: &App, path: &std::path::Path) {
    #[cfg(any(test, feature = "test-support"))]
    {
        REVEALED.with(|paths| paths.borrow_mut().push(path.to_path_buf()));
        let _ = cx;
    }
    #[cfg(not(any(test, feature = "test-support")))]
    cx.reveal_path(path);
}

/// The paths a test build was asked to reveal, oldest first.
#[cfg(any(test, feature = "test-support"))]
pub fn revealed_paths() -> Vec<std::path::PathBuf> {
    REVEALED.with(|paths| paths.borrow().clone())
}

/// Forgets the paths a test build was asked to reveal.
#[cfg(any(test, feature = "test-support"))]
pub fn clear_revealed_paths() {
    REVEALED.with(|paths| paths.borrow_mut().clear());
}

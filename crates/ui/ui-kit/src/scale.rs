//! The interface scale: on Windows the platform multiplies the scale factor of every
//! window by it, so the text, the icons, the rows, the padding and the bars grow together
//! while the size of the window on screen stays the same. The platforms of macOS and
//! Linux are not ours to change, so there only the text follows the setting. The font
//! size of the editor is a setting of its own and does not take part.

#[cfg(windows)]
use std::cell::RefCell;
#[cfg(windows)]
use std::rc::Rc;

#[cfg(windows)]
use gpui_kit::Platform;
#[cfg(feature = "test-support")]
use gpui_kit::Window;

#[cfg(windows)]
thread_local! {
    /// The platform of the application, kept so that a change of the scale reaches every
    /// window. It lives on the thread that runs the UI.
    static PLATFORM: RefCell<Option<Rc<gpui_windows::WindowsPlatform>>> =
        const { RefCell::new(None) };
}

/// The platform of the application. On Windows it is built here rather than by
/// `gpui_kit::application`, because the scale of the interface has to reach the windows
/// the platform opens; elsewhere the platform of the framework is used as it is.
#[cfg(windows)]
pub(crate) fn platform() -> Rc<dyn Platform> {
    let platform = Rc::new(
        gpui_windows::WindowsPlatform::new(false).expect("failed to initialize Windows platform"),
    );
    PLATFORM.with(|slot| *slot.borrow_mut() = Some(platform.clone()));
    platform
}

/// How much the text grows by itself on top of the scale factor of the window. The
/// platform of Windows multiplies the scale factor of every window, and the test platform
/// does the same wherever it runs; the platforms of macOS and Linux are not ours, so
/// there only the text follows the setting.
pub fn text_scale(ui_scale: f32) -> f32 {
    if cfg!(any(windows, feature = "test-support")) {
        1.
    } else {
        ui_scale
    }
}

/// How much the logical pixels of a window shrink so that it keeps its size on screen
/// while every pixel grows by the interface scale. Only the platform of Windows grows
/// the pixels of the window; elsewhere the size on screen follows the logical size.
pub fn window_scale(ui_scale: f32) -> f32 {
    if cfg!(windows) { ui_scale } else { 1. }
}

/// How much every logical pixel of the interface grows: 1 is the size of the display.
/// The window itself keeps its size on screen. Only the platform of Windows applies it;
/// on macOS and Linux the call does nothing and `text_scale` carries the setting.
pub fn set_ui_scale(ui_scale: f32) {
    #[cfg(windows)]
    PLATFORM.with(|slot| {
        if let Some(platform) = slot.borrow().as_ref() {
            platform.set_ui_scale(ui_scale);
        }
    });
    #[cfg(not(windows))]
    let _ = ui_scale;
}

/// Applies the interface scale to a window of the test platform, which has no platform
/// of the application behind it. `base` is the scale the window had at the scale of 1.
#[cfg(feature = "test-support")]
pub fn scale_test_window(window: &mut Window, base: f32, ui_scale: f32) {
    window.set_scale_factor(base * ui_scale);
}

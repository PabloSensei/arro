//! The interface scale: the platform multiplies the scale factor of every window by it,
//! so the text, the icons, the rows, the padding and the bars grow together while the size
//! of the window on screen stays the same. The windows of the test platform do the same.
//! The font size of the editor is a setting of its own and does not take part.

#[cfg(any(
    windows,
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos"
))]
use std::cell::RefCell;
#[cfg(any(
    windows,
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos"
))]
use std::rc::Rc;

#[cfg(any(
    windows,
    target_os = "linux",
    target_os = "freebsd",
    target_os = "macos"
))]
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

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
thread_local! {
    /// The platform of the application, kept so that a change of the scale reaches every
    /// window. It lives on the thread that runs the UI.
    static PLATFORM: RefCell<Option<Rc<gpui_linux::LinuxPlatform>>> =
        const { RefCell::new(None) };
}

#[cfg(target_os = "macos")]
thread_local! {
    /// The platform of the application, kept so that a change of the scale reaches every
    /// window. It lives on the thread that runs the UI.
    static PLATFORM: RefCell<Option<Rc<gpui_macos::MacPlatform>>> =
        const { RefCell::new(None) };
}

/// The platform of the application. It is built here rather than by
/// `gpui_kit::application`, because the scale of the interface has to reach the windows
/// the platform opens.
#[cfg(windows)]
pub(crate) fn platform() -> Rc<dyn Platform> {
    let platform = Rc::new(
        gpui_windows::WindowsPlatform::new(false).expect("failed to initialize Windows platform"),
    );
    PLATFORM.with(|slot| *slot.borrow_mut() = Some(platform.clone()));
    platform
}

/// The platform of the application. It is built here rather than by
/// `gpui_kit::application`, because the scale of the interface has to reach the windows
/// the platform opens.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
pub(crate) fn platform() -> Rc<dyn Platform> {
    let platform = Rc::new(gpui_linux::LinuxPlatform::new(
        gpui_kit::WindowingModes::all(),
    ));
    PLATFORM.with(|slot| *slot.borrow_mut() = Some(platform.clone()));
    platform
}

/// The platform of the application. It is built here rather than by
/// `gpui_kit::application`, because the scale of the interface has to reach the windows
/// the platform opens.
#[cfg(target_os = "macos")]
pub(crate) fn platform() -> Rc<dyn Platform> {
    let platform = Rc::new(gpui_macos::MacPlatform::new(false));
    PLATFORM.with(|slot| *slot.borrow_mut() = Some(platform.clone()));
    platform
}

/// How much every logical pixel of the interface grows: 1 is the size of the display.
/// The window itself keeps its size on screen, and every platform applies it to all of
/// its windows.
pub fn set_ui_scale(ui_scale: f32) {
    #[cfg(windows)]
    PLATFORM.with(|slot| {
        if let Some(platform) = slot.borrow().as_ref() {
            platform.set_ui_scale(ui_scale);
        }
    });
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    PLATFORM.with(|slot| {
        if let Some(platform) = slot.borrow().as_ref() {
            platform.set_ui_scale(ui_scale);
        }
    });
    #[cfg(target_os = "macos")]
    PLATFORM.with(|slot| {
        if let Some(platform) = slot.borrow().as_ref() {
            platform.set_ui_scale(ui_scale);
        }
    });
    #[cfg(not(any(
        windows,
        target_os = "linux",
        target_os = "freebsd",
        target_os = "macos"
    )))]
    let _ = ui_scale;
}

/// How much the logical pixels of a window shrink so that it keeps its size on screen
/// while every pixel grows by the interface scale.
pub fn window_scale(ui_scale: f32) -> f32 {
    ui_scale
}

/// Applies the interface scale to a window of the test platform, which has no platform
/// of the application behind it. `base` is the scale the window had at the scale of 1.
#[cfg(feature = "test-support")]
pub fn scale_test_window(window: &mut Window, base: f32, ui_scale: f32) {
    window.set_scale_factor(base * ui_scale);
}

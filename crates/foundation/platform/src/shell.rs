//! The default browser and other shell targets.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows_sys::Win32::System::Com::{
    COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE, CoInitializeEx, CoUninitialize,
};
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

fn wide(text: &str) -> Vec<u16> {
    OsStr::new(text).encode_wide().chain([0]).collect()
}

/// Whether `url` is a web address. Only those reach the shell: a link from a language
/// server could otherwise name a local program, and the shell would start it.
pub fn is_web_url(url: &str) -> bool {
    let lower = url.trim_start().to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://"))
        && !url.chars().any(char::is_control)
}

/// With this variable set, links are recorded instead of opened: tests press every
/// button, and a real browser tab for each run is noise on the developer's desktop.
pub const NO_BROWSER_ENV: &str = "ARRO_NO_BROWSER";

static RECORDED: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

/// The links `open_url` was asked to open while `NO_BROWSER_ENV` was set, oldest first.
pub fn recorded_urls() -> Vec<String> {
    RECORDED.lock().map(|urls| urls.clone()).unwrap_or_default()
}

/// Opens `url` in the default browser. Returns whether the address was handed to the
/// shell; anything but an http or https address is refused.
///
/// The shell runs on a thread of its own: `ShellExecuteW` pumps window messages while it
/// works, and a message that reaches the window inside a click handler re-enters the
/// interface and ends the program.
pub fn open_url(url: &str) -> bool {
    if !is_web_url(url) {
        return false;
    }
    if std::env::var_os(NO_BROWSER_ENV).is_some() {
        if let Ok(mut urls) = RECORDED.lock() {
            urls.push(url.to_owned());
        }
        return true;
    }
    let url = url.to_owned();
    std::thread::Builder::new()
        .name("open-url".to_owned())
        .spawn(move || {
            if !shell_open(&url) {
                eprintln!("the browser did not open {url}");
            }
        })
        .is_ok()
}

fn shell_open(url: &str) -> bool {
    let operation = wide("open");
    let target = wide(url);
    // SAFETY: the shell asks for a single-threaded apartment without DDE on the thread
    // that calls it; this thread is ours and ends after the call.
    let initialized = unsafe {
        CoInitializeEx(
            std::ptr::null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        )
    } >= 0;
    // SAFETY: both strings are null terminated and live for the call.
    let started = unsafe {
        ShellExecuteW(
            std::ptr::null_mut(),
            operation.as_ptr(),
            target.as_ptr(),
            std::ptr::null(),
            std::ptr::null(),
            SW_SHOWNORMAL,
        )
    };
    if initialized {
        // SAFETY: pairs the successful initialization above on the same thread.
        unsafe { CoUninitialize() };
    }
    started as isize > 32
}

#[cfg(test)]
mod tests {
    use super::is_web_url;

    #[test]
    fn only_web_addresses_reach_the_shell() {
        assert!(is_web_url("https://docs.example.com/api/17/"));
        assert!(is_web_url("HTTP://example.com/a.html"));
        assert!(!is_web_url("file:///C:/Windows/System32/calc.exe"));
        assert!(!is_web_url(r"C:\Windows\System32\calc.exe"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("https://a.com/\nfoo"));
    }
}

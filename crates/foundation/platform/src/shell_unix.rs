//! The default browser and other shell targets.

use std::process::{Command, Stdio};

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
/// desktop; anything but an http or https address is refused.
///
/// The desktop runs on a thread of its own, so a slow launcher never blocks the caller.
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
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::is_web_url;

    #[test]
    fn only_web_addresses_reach_the_shell() {
        assert!(is_web_url("https://docs.example.com/api/17/"));
        assert!(is_web_url("HTTP://example.com/a.html"));
        assert!(!is_web_url("file:///usr/bin/sh"));
        assert!(!is_web_url("/usr/bin/sh"));
        assert!(!is_web_url("javascript:alert(1)"));
        assert!(!is_web_url("https://a.com/\nfoo"));
    }
}

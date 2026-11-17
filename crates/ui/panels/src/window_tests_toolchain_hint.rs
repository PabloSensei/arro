//! A missing JDK: the balloon behind the status line offers its download page and a
//! second search. No real process runs here: the status line names a fixture and the
//! check counts its calls.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;

use crate::window_tests::{click, draw, open_window, sample_project, wait_for_signal};
use crate::{LanguageSetup, SetupCheck, StartedServer, apply_setup};

const MISSING: &str = "Fixture: JDK not found";
const DETAILS: &str = "Install JDK 25 or newer. See README.txt next to the program.";
const DOWNLOAD: &str =
    "https://adoptium.net/temurin/releases/?version=25&os=windows&arch=x64&package=jdk";

struct CountingCheck(Arc<AtomicUsize>);

impl SetupCheck for CountingCheck {
    fn check(&self, _started: futures_channel::mpsc::UnboundedSender<Option<StartedServer>>) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[ui_kit::gpui::test]
fn a_missing_jdk_offers_its_page_and_a_second_search(cx: &mut TestAppContext) {
    // A real browser tab for every link is noise on the desktop.
    // SAFETY: tests of this binary run one at a time by default; the value stays set.
    unsafe {
        std::env::set_var(platform::NO_BROWSER_ENV, "1");
    }
    let root = sample_project("toolchain-hint");
    let check = Arc::new(AtomicUsize::new(0));
    let window = open_window(cx, root);
    // The helper starts with one status line of its own; it has to arrive first, or
    // its watcher replaces the text of this test.
    wait_for_signal(cx, "the first status line", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.language_status().is_some())
            .unwrap()
    });
    window
        .update(cx, |workspace, _, cx| {
            workspace.set_language_status(Some(format!("{MISSING}\n{DETAILS}").into()), cx);
            apply_setup(
                workspace,
                LanguageSetup {
                    download: Some(("Download JDK 25".into(), DOWNLOAD.to_owned())),
                    check: Some(Box::new(CountingCheck(check.clone()))),
                },
                cx,
            );
        })
        .unwrap();

    draw(cx, window);
    click(cx, window, "st-language");
    draw(cx, window);
    draw(cx, window);

    let (title, text) = window
        .read_with(cx, |workspace, _| workspace.last_notification())
        .unwrap()
        .expect("a click on the line explains the setup");
    assert_eq!(title, MISSING);
    assert_eq!(text, DETAILS);
    let buttons: Vec<String> = window
        .read_with(cx, |workspace, _| workspace.last_notification_buttons())
        .unwrap()
        .iter()
        .map(ToString::to_string)
        .collect();
    assert_eq!(buttons, ["Download JDK 25", "Check again", "README"]);

    click(cx, window, "notification-action");
    assert!(
        platform::recorded_urls().iter().any(|url| url == DOWNLOAD),
        "the download button did not ask for its page"
    );

    draw(cx, window);
    click(cx, window, "notification-action-check");
    assert_eq!(
        check.load(Ordering::SeqCst),
        1,
        "Check again did not start the search"
    );
}

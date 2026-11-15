//! T-0224: the table of a bundle edits the files that no tab shows through a buffer of
//! their own, without opening a tab for them; Ctrl+Z goes to that buffer as in the text.
//!
//! `cargo test -p panels window_tests_bundle_buffers`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{EditorView, Languages};
use lang_api::LanguageSupport;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::bundle_editor::{self, BundleEditorView};
use crate::index::SearchSetup;
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, press, scratch, type_text,
};
use crate::window_tests_real::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const BASE: &str = "src/main/resources/messages.properties";
const RU: &str = "src/main/resources/messages_ru.properties";
const DE: &str = "src/main/resources/messages_de.properties";

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    support: Arc<dyn LanguageSupport>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("bundle-buffers-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: file_syntax::language_for,
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server ready".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(support),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            crate::window_tests::project_setup(scratch("bundle-buffers-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = scratch(name);
    for (path, text) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, text).unwrap();
    }
    root
}

fn table(cx: &mut TestAppContext, path: &Path) -> gpui::Entity<BundleEditorView> {
    wait_for(cx, "the table of the set", Duration::from_secs(10), |cx| {
        cx.read(|cx| bundle_editor::view_of(path, cx).is_some())
    });
    cx.read(|cx| bundle_editor::view_of(path, cx).expect("the table was built"))
}

/// The buffer of a file of the set, opened by the table when a value changes.
fn buffer(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> gpui::Entity<EditorView> {
    wait_for(
        cx,
        "the buffer of a file of the set",
        Duration::from_secs(10),
        |cx| {
            window
                .read_with(cx, |workspace, _| workspace.view_of(path).is_some())
                .unwrap_or(false)
        },
    );
    editor_of(cx, window, path)
}

/// Opens the base file and shows its table.
fn table_of(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    base: &Path,
) -> gpui::Entity<BundleEditorView> {
    draw(cx, window);
    open_path(cx, window, base);
    draw(cx, window);
    click(cx, window, "bundle-mode-bundle");
    draw(cx, window);
    table(cx, base)
}

fn tabs(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<PathBuf> {
    window
        .read_with(cx, |workspace, _| {
            workspace.open_tabs().into_iter().flatten().collect()
        })
        .unwrap()
}

/// Clicks the element with a selector the test builds: the test platform looks selectors
/// up by a static name.
fn click_selector(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &str) {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let bounds = visual
        .debug_bounds(selector)
        .unwrap_or_else(|| panic!("no element {selector} on screen"))
        .center();
    visual.simulate_click(bounds, ui_kit::Modifiers::default());
    drop(visual);
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn a_key_added_to_closed_files_of_the_set_needs_no_tab(cx: &mut TestAppContext) {
    let root = project(
        "bundle-buffers-add",
        &[
            (BASE, "welcome=Welcome\n"),
            (RU, "welcome=Privet\n"),
            (DE, "welcome=Willkommen\n"),
        ],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    let base = root.join(BASE);
    let ru = root.join(RU);
    let de = root.join(DE);
    let table = table_of(cx, window, &base);
    wait_for(cx, "the key of the table", Duration::from_secs(10), |cx| {
        table.read_with(cx, |table, _| table.selected_key() == Some("welcome"))
    });

    click(cx, window, "bundle-add-key");
    type_text(cx, window, "farewell");
    press(cx, window, "enter");
    let ru_view = buffer(cx, window, &ru);
    let de_view = buffer(cx, window, &de);
    wait_for(
        cx,
        "the new key in every file",
        Duration::from_secs(10),
        |cx| {
            buffer_of(cx, &ru_view).contains("farewell=")
                && buffer_of(cx, &de_view).contains("farewell=")
        },
    );
    assert_eq!(
        tabs(cx, window),
        vec![base],
        "the files of the set keep one tab"
    );
}

#[ui_kit::gpui::test]
fn undo_in_the_table_takes_a_value_of_a_closed_file_back(cx: &mut TestAppContext) {
    let root = project(
        "bundle-buffers-undo",
        &[
            (BASE, "welcome=Welcome\n"),
            (RU, "welcome=Privet\n"),
            (DE, "welcome=Willkommen\n"),
        ],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    let base = root.join(BASE);
    let ru = root.join(RU);
    let table = table_of(cx, window, &base);
    wait_for(cx, "the key of the table", Duration::from_secs(10), |cx| {
        table.read_with(cx, |table, _| table.selected_key() == Some("welcome"))
    });

    let index = table.read_with(cx, |table, _| {
        table
            .locales()
            .iter()
            .position(|locale| locale.as_deref() == Some("ru"))
            .expect("the locale of the test is in the set")
    });
    click_selector(cx, window, &format!("bundle-value-{index}"));
    press(cx, window, "ctrl-a");
    type_text(cx, window, "Hola");
    let ru_view = buffer(cx, window, &ru);
    wait_for(cx, "the edited buffer", Duration::from_secs(10), |cx| {
        buffer_of(cx, &ru_view).starts_with("welcome=Hola")
    });

    press(cx, window, "ctrl-z");
    wait_for(cx, "the undo of the field", Duration::from_secs(10), |cx| {
        buffer_of(cx, &ru_view).starts_with("welcome=Privet")
    });
    assert_eq!(
        tabs(cx, window),
        vec![base],
        "the closed file got no tab of its own"
    );
}

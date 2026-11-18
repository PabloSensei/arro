//! T-0307: a value changed in the table of a message bundle edits the file of a locale
//! that no tab shows, and the common Save All writes it. The tab of the locale never
//! appears.
//!
//! `cargo test -p panels window_tests_bundle_save`

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
    buffer_of, click, draw, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_real::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const BASE: &str = "src/main/resources/messages.properties";
const RU: &str = "src/main/resources/messages_ru.properties";
const DE: &str = "src/main/resources/messages_de.properties";
const PRIVET: &str = "welcome=Privet\n";
const HOLA: &str = "welcome=Hola\n";

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    support: Arc<dyn LanguageSupport>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("bundle-save-state");
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
            project_setup(scratch("bundle-save-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn project(name: &str) -> PathBuf {
    let root = scratch(name);
    for (path, text) in [
        (BASE, "welcome=Welcome\n"),
        (RU, PRIVET),
        (DE, "welcome=Willkommen\n"),
    ] {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
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
fn saving_writes_the_closed_locale_without_a_tab_of_its_own(cx: &mut TestAppContext) {
    let root = project("bundle-save");
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.autosave = false, cx)
        })
        .unwrap();
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
    wait_for(
        cx,
        "the buffer of the closed locale",
        Duration::from_secs(10),
        |cx| {
            window
                .read_with(cx, |workspace, _| workspace.view_of(&ru).is_some())
                .unwrap_or(false)
        },
    );
    let view: gpui::Entity<EditorView> = window
        .read_with(cx, |workspace, _| workspace.view_of(&ru))
        .unwrap()
        .and_then(|view| view.downcast::<EditorView>().ok())
        .expect("the buffer of the closed file has an editor");
    wait_for(
        cx,
        "the text of the buffer",
        Duration::from_secs(10),
        |cx| buffer_of(cx, &view).starts_with("welcome=Hola"),
    );

    assert_eq!(
        fs::read_to_string(&ru).unwrap(),
        PRIVET,
        "nothing is written before Save"
    );
    assert_eq!(
        tabs(cx, window),
        vec![base.clone()],
        "the edited locale keeps no tab of its own"
    );

    press(cx, window, "ctrl-s");
    wait_for(cx, "the written locale", Duration::from_secs(10), |_| {
        fs::read_to_string(&ru).is_ok_and(|text| text == HOLA)
    });
    assert_eq!(
        fs::read_to_string(&ru).unwrap(),
        HOLA,
        "Save All writes the text of the closed buffer"
    );
    assert_eq!(tabs(cx, window), vec![base], "Save All adds no tab either");
}

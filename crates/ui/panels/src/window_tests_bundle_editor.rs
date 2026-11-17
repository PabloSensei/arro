//! The table of a localized set of `.properties` files: the two modes of its tabs, the
//! keys and the filter, the mark of a key a locale misses, a field that changes the
//! buffer of its file, and a key that is added to and removed from every file. The
//! language of the test project is the message bundle support, so no server is needed.
//!
//! The real project check needs `IDE_CORPUS`:
//!
//! `cargo test -p panels window_tests_bundle_editor -- --ignored --test-threads=1`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{BundleMode, EditorView, Languages};
use lang_api::LanguageSupport;
use text_core::Edit;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{BundleLayout, FileIcons, Launch, Layout, Settings, Workspace};

use crate::bundle_editor::{self, BundleEditorView};
use crate::index::SearchSetup;
use crate::window_tests::{
    active_path, buffer_of, click, draw, editor_of, open_path, press, project_setup, scratch,
    type_text,
};
use crate::window_tests_real::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    support: Arc<dyn LanguageSupport>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("bundle-editor-state");
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
            project_setup(scratch("bundle-editor-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// A project of the test: the files are written before the support reads them.
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

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn mode(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> Option<BundleMode> {
    view.read_with(cx, |view, cx| view.bundle_mode(cx))
}

fn switch_to_table(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    click(cx, window, "bundle-mode-bundle");
    draw(cx, window);
}

fn table(
    cx: &mut TestAppContext,
    _window: WindowHandle<Workspace>,
    path: &Path,
) -> gpui::Entity<BundleEditorView> {
    wait_for(cx, "the table of the set", Duration::from_secs(10), |cx| {
        cx.read(|cx| bundle_editor::view_of(path, cx).is_some())
    });
    cx.read(|cx| bundle_editor::view_of(path, cx).expect("the table was built"))
}

fn editor_when_open(
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

const BASE: &str = "src/main/resources/messages.properties";
const RU: &str = "src/main/resources/messages_ru.properties";

#[ui_kit::gpui::test]
fn the_tab_switches_between_the_text_and_the_table_and_keeps_the_choice(cx: &mut TestAppContext) {
    let root = project(
        "bundle-editor-modes",
        &[(BASE, "welcome=Welcome\n"), (RU, "welcome=Privet\n")],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let base = root.join(BASE);
    open_path(cx, window, &base);
    let view = editor_of(cx, window, &base);
    draw(cx, window);

    assert!(
        shown(cx, window, "bundle-mode-text"),
        "a file of a set offers the text mode"
    );
    assert!(
        shown(cx, window, "bundle-mode-bundle"),
        "a file of a set offers the table mode"
    );
    assert_eq!(mode(cx, &view), Some(BundleMode::Text));

    switch_to_table(cx, window);
    assert_eq!(mode(cx, &view), Some(BundleMode::Bundle));
    assert!(shown(cx, window, "bundle-editor"));
    assert!(!shown(cx, window, "editor-text"));
    let layout = window
        .read_with(cx, |workspace, cx| workspace.current_layout(cx))
        .unwrap();
    assert!(
        layout
            .bundle_modes
            .iter()
            .any(|(path, mode)| path == &base && *mode == BundleLayout::Bundle),
        "the choice is kept for the file: {:?}",
        layout.bundle_modes
    );

    click(cx, window, "bundle-mode-text");
    draw(cx, window);
    assert_eq!(mode(cx, &view), Some(BundleMode::Text));
    assert!(shown(cx, window, "editor-text"));
    assert!(!shown(cx, window, "bundle-editor"));
}

#[ui_kit::gpui::test]
fn the_table_lists_the_keys_in_alphabetical_order(cx: &mut TestAppContext) {
    let root = project(
        "bundle-editor-keys",
        &[
            (BASE, "gamma=Last\nalpha=First\nbeta=Release\n"),
            (RU, "gamma=Konec\nalpha=Pervyj\nbeta=Vypusk\n"),
        ],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let base = root.join(BASE);
    open_path(cx, window, &base);
    switch_to_table(cx, window);
    let table = table(cx, window, &base);

    wait_for(cx, "the keys of the set", Duration::from_secs(10), |cx| {
        table.read_with(cx, |table, _| {
            table.key_names() == ["alpha", "beta", "gamma"]
        })
    });
}

#[ui_kit::gpui::test]
fn the_filter_keeps_the_keys_and_values_that_match(cx: &mut TestAppContext) {
    let root = project(
        "bundle-editor-filter",
        &[
            (BASE, "alpha=First\nbeta=Release\ngamma=Last\n"),
            (RU, "alpha=Pervyj\nbeta=Vypusk\ngamma=Konec\n"),
        ],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let base = root.join(BASE);
    open_path(cx, window, &base);
    switch_to_table(cx, window);
    let table = table(cx, window, &base);

    click(cx, window, "bundle-filter");
    type_text(cx, window, "beta");
    wait_for(
        cx,
        "the filter of the keys",
        Duration::from_secs(10),
        |cx| table.read_with(cx, |table, _| table.key_names() == ["beta"]),
    );

    press(cx, window, "backspace backspace backspace backspace");
    type_text(cx, window, "Last");
    wait_for(
        cx,
        "the filter of the values",
        Duration::from_secs(10),
        |cx| table.read_with(cx, |table, _| table.key_names() == ["gamma"]),
    );
}

#[ui_kit::gpui::test]
fn a_key_a_locale_misses_is_marked(cx: &mut TestAppContext) {
    let root = project(
        "bundle-editor-missing",
        &[
            (BASE, "welcome=Welcome\norphan=Never\n"),
            (RU, "welcome=Privet\n"),
        ],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let base = root.join(BASE);
    open_path(cx, window, &base);
    switch_to_table(cx, window);
    let table = table(cx, window, &base);

    wait_for(cx, "the missing locale", Duration::from_secs(10), |cx| {
        table.read_with(cx, |table, _| table.missing_of("orphan") == ["ru"])
    });
    assert!(
        shown(cx, window, "bundle-missing-orphan"),
        "the key without a translation shows the warning"
    );
    assert!(
        !shown(cx, window, "bundle-missing-welcome"),
        "a translated key has no warning"
    );
}

#[ui_kit::gpui::test]
fn a_field_edit_changes_the_buffer_of_its_file_and_undo_takes_it_back(cx: &mut TestAppContext) {
    let root = project(
        "bundle-editor-field",
        &[(BASE, "welcome=Welcome\n"), (RU, "welcome=Privet\n")],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let base = root.join(BASE);
    open_path(cx, window, &base);
    let view = editor_of(cx, window, &base);
    switch_to_table(cx, window);
    let table = table(cx, window, &base);
    wait_for(cx, "the key of the table", Duration::from_secs(10), |cx| {
        table.read_with(cx, |table, _| table.selected_key() == Some("welcome"))
    });

    click(cx, window, "bundle-value-0");
    press(cx, window, "ctrl-a");
    type_text(cx, window, "Hello");
    wait_for(cx, "the edited buffer", Duration::from_secs(10), |cx| {
        buffer_of(cx, &view).starts_with("welcome=Hello")
    });

    press(cx, window, "ctrl-z");
    wait_for(cx, "the undo of the field", Duration::from_secs(10), |cx| {
        buffer_of(cx, &view).starts_with("welcome=Welcome")
    });
}

#[ui_kit::gpui::test]
fn a_field_of_a_closed_file_opens_its_buffer_and_changes_it(cx: &mut TestAppContext) {
    let root = project(
        "bundle-editor-closed",
        &[(BASE, "welcome=Welcome\n"), (RU, "welcome=Privet\n")],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let base = root.join(BASE);
    open_path(cx, window, &base);
    switch_to_table(cx, window);
    let table = table(cx, window, &base);
    wait_for(cx, "the key of the table", Duration::from_secs(10), |cx| {
        table.read_with(cx, |table, _| table.selected_key() == Some("welcome"))
    });

    draw(cx, window);
    click(cx, window, "bundle-value-1");
    press(cx, window, "ctrl-a");
    type_text(cx, window, "Hola");
    let ru = editor_when_open(cx, window, &root.join(RU));
    wait_for(
        cx,
        "the buffer of the locale",
        Duration::from_secs(10),
        |cx| buffer_of(cx, &ru).starts_with("welcome=Hola"),
    );
    assert_eq!(
        active_path(cx, window).as_deref(),
        Some(base.as_path()),
        "the table stays the active tab while the locale buffer opens behind it"
    );
}

#[ui_kit::gpui::test]
fn a_key_is_added_and_removed_in_every_file(cx: &mut TestAppContext) {
    let root = project(
        "bundle-editor-add-remove",
        &[(BASE, "welcome=Welcome\n"), (RU, "welcome=Privet\n")],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let base = root.join(BASE);
    let ru = root.join(RU);
    open_path(cx, window, &base);
    let base_view = editor_of(cx, window, &base);
    switch_to_table(cx, window);
    let table = table(cx, window, &base);
    draw(cx, window);

    click(cx, window, "bundle-add-key");
    type_text(cx, window, "farewell");
    press(cx, window, "enter");
    let ru_view = editor_when_open(cx, window, &ru);
    wait_for(
        cx,
        "the new key in every file",
        Duration::from_secs(10),
        |cx| {
            buffer_of(cx, &base_view).contains("farewell=")
                && buffer_of(cx, &ru_view).contains("farewell=")
        },
    );

    wait_for(
        cx,
        "the row of the new key",
        Duration::from_secs(10),
        |cx| {
            table.read_with(cx, |table, _| {
                table.key_names().iter().any(|key| key == "farewell")
            })
        },
    );
    click(cx, window, "bundle-key-farewell");
    click(cx, window, "bundle-remove-key");
    press(cx, window, "enter");
    wait_for(cx, "the removed key", Duration::from_secs(10), |cx| {
        !buffer_of(cx, &base_view).contains("farewell=")
            && !buffer_of(cx, &ru_view).contains("farewell=")
    });
}

#[ui_kit::gpui::test]
fn an_edit_of_the_text_updates_the_table(cx: &mut TestAppContext) {
    let root = project(
        "bundle-editor-text-edit",
        &[(BASE, "welcome=Welcome\n"), (RU, "welcome=Privet\n")],
    );
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let base = root.join(BASE);
    open_path(cx, window, &base);
    let view = editor_of(cx, window, &base);
    switch_to_table(cx, window);
    let table = table(cx, window, &base);
    wait_for(
        cx,
        "the first key of the table",
        Duration::from_secs(10),
        |cx| table.read_with(cx, |table, _| table.selected_key() == Some("welcome")),
    );

    let end = buffer_of(cx, &view).len();
    view.update(cx, |view, cx| {
        view.apply_edits(vec![Edit::insert(end, "delta=Fourth\n")], cx)
    });
    wait_for(
        cx,
        "the key typed in the text",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            table.read_with(cx, |table, _| {
                table.key_names().iter().any(|key| key == "delta")
            })
        },
    );
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS with the sample project"]
fn the_sample_project_opens_its_set_in_the_table(cx: &mut TestAppContext) {
    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("bundle-editor-sample").join("sample");
    window_language::prepare_project(&source, &root);
    let base = root.join("src/main/resources/messages/messages.properties");
    let support = window_language::message_bundles(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &base);
    let view = editor_of(cx, window, &base);
    switch_to_table(cx, window);
    assert_eq!(mode(cx, &view), Some(BundleMode::Bundle));
    let table = table(cx, window, &base);
    wait_for(
        cx,
        "the keys of the project",
        Duration::from_secs(10),
        |cx| table.read_with(cx, |table, _| !table.key_names().is_empty()),
    );
    let (keys, locales) =
        table.read_with(cx, |table, _| (table.key_names().len(), table.locales()));
    eprintln!(
        "sample: {keys} keys, {} files of the set, locales {:?}",
        locales.len(),
        locales
            .iter()
            .filter_map(|locale| locale.as_deref())
            .collect::<Vec<_>>()
    );
    assert!(keys > 0, "the set has keys");
    assert!(locales.len() >= 2, "the set has a base file and locales");
}

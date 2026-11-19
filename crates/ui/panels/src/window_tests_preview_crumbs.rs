//! The preview of Find in Files and of the palettes, and the list under a crumb of the
//! navigation bar, without a window. A file of the project lands in the list off the UI
//! thread; the tests wait for the row it shows, never for a clock.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use editor_view::Languages;
use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::finder::{ActiveFinder, Finder};
use crate::index::SearchSetup;
use crate::palette::Palette;
use crate::window_tests::{
    click, draw, editor_of, open_palette, open_path, open_window, press, project_setup, scratch,
    type_text, wait_for_file_index, wait_for_signal,
};
use crate::{AgentOptions, LanguageServer, connect, install};

/// The finder on screen, when its action has opened it.
fn finder(cx: &TestAppContext) -> Entity<Finder> {
    cx.read(|cx| {
        cx.global::<ActiveFinder>()
            .0
            .clone()
            .expect("the finder is open")
    })
}

/// The palette on screen, when its action has opened it.
fn palette(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Entity<Palette> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .overlay_view()
                .expect("the palette is open")
                .downcast::<Palette>()
                .expect("the popup is the palette")
        })
        .unwrap()
}

/// The window of a test with a state folder of the caller, so that a test opens the same
/// project twice and the second window finds what the first one saved.
fn open_window_in(
    cx: &mut TestAppContext,
    root: PathBuf,
    state_dir: PathBuf,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |_| None,
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
        .unbounded_send("No language server in the test".to_string())
        .ok();
    let server = LanguageServer {
        support: None,
        status,
    };
    let window = cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    });
    cx.update(|cx| {
        cx.on_app_quit(|cx| {
            for handle in cx.windows() {
                let Some(handle) = handle.downcast::<Workspace>() else {
                    continue;
                };
                handle
                    .update(cx, |_, window, _| window.remove_window())
                    .ok();
            }
            async {}
        })
        .detach();
    });
    window
}

/// The preview of the finder shows `path` with `text` painted over its row.
fn wait_for_preview(
    cx: &mut TestAppContext,
    preview: Entity<crate::preview::FilePreview>,
    what: &'static str,
    path: PathBuf,
    text: &'static str,
) {
    wait_for_signal(cx, what, |cx| {
        preview.read_with(cx, |preview, _| {
            preview.is_ready()
                && preview.path() == Some(path.as_path())
                && preview.highlight_text().as_deref() == Some(text)
        })
    });
}

/// The preview of a palette shows `path`, with no search match to paint.
fn wait_for_preview_file(
    cx: &mut TestAppContext,
    preview: Entity<crate::preview::FilePreview>,
    what: &'static str,
    path: PathBuf,
) {
    wait_for_signal(cx, what, |cx| {
        preview.read_with(cx, |preview, _| {
            preview.is_ready() && preview.path() == Some(path.as_path())
        })
    });
}

#[ui_kit::gpui::test]
fn find_in_files_shows_the_file_of_the_selected_row_in_the_preview(cx: &mut TestAppContext) {
    let root = scratch("preview-finder");
    let first = root.join("a.txt");
    let second = root.join("b.txt");
    fs::write(&first, "alpha one\n").unwrap();
    fs::write(&second, "beta alpha\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);

    press(cx, window, "ctrl-shift-f");
    wait_for_signal(cx, "the finder", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveFinder>()
                .is_some_and(|global| global.0.is_some())
        })
    });
    draw(cx, window);
    type_text(cx, window, "alpha");
    let found = finder(cx);
    wait_for_preview(
        cx,
        found.read_with(cx, |finder, _| finder.preview().clone()),
        "the preview of the first row",
        first.clone(),
        "alpha",
    );

    // The second hit is in the other file: its text, its row and the place of the caret.
    press(cx, window, "down");
    let preview = found.read_with(cx, |finder, _| finder.preview().clone());
    wait_for_preview(
        cx,
        preview.clone(),
        "the preview of the second row",
        second.clone(),
        "alpha",
    );
    let editor = preview
        .read_with(cx, |preview, _| preview.editor().cloned())
        .expect("the preview has an editor");
    assert_eq!(
        editor.read_with(cx, |editor, _| editor.caret()),
        (0, 5),
        "the preview is scrolled to the match"
    );
}

#[ui_kit::gpui::test]
fn the_preview_switch_of_recent_files_survives_a_new_window(cx: &mut TestAppContext) {
    let root = scratch("preview-recent");
    let main = root.join("main.txt");
    let notes = root.join("notes.txt");
    let readme = root.join("readme.txt");
    fs::write(&main, "main\n").unwrap();
    fs::write(&notes, "notes\n").unwrap();
    fs::write(&readme, "readme\n").unwrap();
    let state = scratch("preview-recent-state");
    let window = open_window_in(cx, root.clone(), state.clone());
    cx.run_until_parked();
    draw(cx, window);

    open_path(cx, window, &main);
    open_path(cx, window, &notes);
    open_path(cx, window, &readme);
    open_palette(cx, window, keymap::workspace::RECENT_FILES);
    let recent = palette(cx, window);
    assert!(
        !recent.read_with(cx, |palette, _| palette.preview_open()),
        "the preview starts off"
    );
    click(cx, window, "palette-preview");
    draw(cx, window);
    assert!(
        recent.read_with(cx, |palette, _| palette.preview_open()),
        "the button turns the preview on"
    );
    assert!(
        ui_kit::gpui::VisualTestContext::from_window(window.into(), cx)
            .debug_bounds("palette-preview-box")
            .is_some_and(|bounds| bounds.size.width > px(0.)),
        "the preview stands right of the list"
    );
    wait_for_preview_file(
        cx,
        recent.read_with(cx, |palette, _| palette.preview().clone()),
        "the preview of the selected recent file",
        notes.clone(),
    );

    // The same state folder, a new window: the switch is on and the preview is there.
    window
        .update(cx, |workspace, window, cx| {
            workspace.close_window(window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    let second = open_window_in(cx, root.clone(), state);
    cx.run_until_parked();
    draw(cx, second);
    open_palette(cx, second, keymap::workspace::RECENT_FILES);
    let recent = palette(cx, second);
    assert!(
        recent.read_with(cx, |palette, _| palette.preview_open()),
        "the switch is remembered"
    );
    wait_for_preview_file(
        cx,
        recent.read_with(cx, |palette, _| palette.preview().clone()),
        "the preview of the new window",
        notes,
    );
}

/// The project of the crumb test: one class with two methods and a field, and another
/// class, so that a level of the navigation bar has neighbours.
const SAMPLE: &str = "class Owner\n    method getOwner\n        body one\n    method setName\n        body two\n    field name\nclass Pet\n    method feed\n        body three\n";

#[ui_kit::gpui::test]
fn a_crumb_lists_its_neighbours_and_the_typing_narrows_them(cx: &mut TestAppContext) {
    let root = crate::window_tests_ui::navigation_project("crumb-menu");
    fs::write(root.join("src").join("Sample.txt"), SAMPLE).unwrap();
    let window = crate::window_tests_ui::open_test_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    let sample = root.join("src").join("Sample.txt");
    open_path(cx, window, &sample);
    crate::window_tests_ui::wait_for_outline(cx, window, &sample);
    let view = editor_of(cx, window, &sample);
    view.update(cx, |view, cx| view.restore_caret(1, 12, cx));
    draw(cx, window);
    let names: Vec<String> = view.read_with(cx, |view, _| {
        view.enclosing()
            .into_iter()
            .map(|item| item.name.clone())
            .collect()
    });
    assert_eq!(names, ["Owner", "getOwner"]);

    // The second crumb is the method: its list holds the members of the class.
    click(cx, window, "crumb-1");
    draw(cx, window);
    assert!(
        ui_kit::gpui::VisualTestContext::from_window(window.into(), cx)
            .debug_bounds("crumb-menu")
            .is_some(),
        "the click opens the list of the crumb"
    );
    let rows = view.read_with(cx, |view, _| view.crumb_menu_rows());
    assert_eq!(rows, ["getOwner", "setName", "name"]);
    press(cx, window, "down");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.crumb_menu_selected()),
        Some("setName".to_owned()),
        "the arrow moves the selection to the next member"
    );

    // Enter goes to the selected member and closes the list.
    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.caret().0), 3);
    assert!(
        view.read_with(cx, |view, _| view.crumb_menu_rows())
            .is_empty()
    );

    // Typing narrows the list of the level the caret is in.
    click(cx, window, "crumb-1");
    draw(cx, window);
    type_text(cx, window, "get");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.crumb_menu_rows()),
        ["getOwner"]
    );
    press(cx, window, "enter");
    draw(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.caret().0), 1);
    let names: Vec<String> = view.read_with(cx, |view, _| {
        view.enclosing()
            .into_iter()
            .map(|item| item.name.clone())
            .collect()
    });
    assert_eq!(names, ["Owner", "getOwner"]);
}

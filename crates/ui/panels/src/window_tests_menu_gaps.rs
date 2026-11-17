//! The File, Edit and Window rows of the main menu that the IDEA user looks for: the
//! command behind each one, what it does to the open files, the caches and the window.

use std::cell::RefCell;
use std::fs;
use std::path::PathBuf;
use std::rc::Rc;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{MenuAction, MenuEntry};
use workspace::{DockSide, Settings, Workspace};

use crate::choice::ChoiceDialog;
use crate::invalidate::CacheLocations;
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_sample_window, open_window,
    open_window_with, press, project_setup, sample_project, scratch, type_text, wait_for_signal,
};

fn sections(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> Vec<MenuEntry> {
    window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap()
}

/// The action of the row at a path of labels of the main menu.
fn row_action(entries: &[MenuEntry], labels: &[&str]) -> MenuAction {
    let mut level = entries;
    let mut found = None;
    for label in labels {
        let index = level
            .iter()
            .position(|entry| entry.label.as_ref() == *label)
            .unwrap_or_else(|| panic!("the main menu has no {label}"));
        found = Some(&level[index]);
        level = &level[index].submenu;
    }
    found
        .and_then(|entry| entry.on_click.clone())
        .unwrap_or_else(|| panic!("{labels:?} has no action"))
}

/// Runs the row of the main menu at a path of labels, as a click does.
fn invoke(cx: &mut TestAppContext, window: WindowHandle<Workspace>, labels: &[&str]) {
    let action = row_action(&sections(cx, window), labels);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.update(|window, cx| action(window, cx));
    cx.run_until_parked();
}

fn has_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn overlay_is_choice(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .overlay_view()
                .and_then(|view| view.downcast::<ChoiceDialog>().ok())
                .is_some()
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn sort_and_reverse_lines_are_one_undo_step(cx: &mut TestAppContext) {
    let root = sample_project("gaps-lines");
    let path = root.join("src").join("main.txt");
    fs::write(&path, "b\na\nc\n").unwrap();
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    press(cx, window, "ctrl-a");
    invoke(cx, window, &["Edit", "Sort Lines"]);
    assert_eq!(buffer_of(cx, &view), "a\nb\nc\n");

    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        "b\na\nc\n",
        "one undo brings the order back"
    );

    press(cx, window, "ctrl-a");
    invoke(cx, window, &["Edit", "Reverse Lines"]);
    assert_eq!(buffer_of(cx, &view), "c\na\nb\n");

    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &view), "b\na\nc\n");
}

#[ui_kit::gpui::test]
fn convert_indents_and_indent_selection_work_from_the_edit_menu(cx: &mut TestAppContext) {
    let root = sample_project("gaps-indents");
    let path = root.join("src").join("main.txt");
    fs::write(&path, "\tone\n\ttwo\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    invoke(cx, window, &["Edit", "Convert Indents", "To Spaces"]);
    assert_eq!(buffer_of(cx, &view), "    one\n    two\n");

    invoke(cx, window, &["Edit", "Convert Indents", "To Tabs"]);
    assert_eq!(buffer_of(cx, &view), "\tone\n\ttwo\n");

    let second = root.join("src").join("indent.txt");
    fs::write(&second, "one\ntwo\n").unwrap();
    open_path(cx, window, &second);
    let view = editor_of(cx, window, &second);
    press(cx, window, "ctrl-a");
    invoke(cx, window, &["Edit", "Indent Selection"]);
    assert_eq!(buffer_of(cx, &view), "    one\n    two\n");

    press(cx, window, "ctrl-z");
    assert_eq!(buffer_of(cx, &view), "one\ntwo\n");

    press(cx, window, "ctrl-a");
    invoke(cx, window, &["Edit", "Unindent Selection"]);
    assert_eq!(buffer_of(cx, &view), "one\ntwo\n");
}

#[ui_kit::gpui::test]
fn reload_all_from_disk_reads_the_disk_and_asks_about_unsaved_text(cx: &mut TestAppContext) {
    let root = sample_project("gaps-reload");
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    fs::write(&path, "changed\n").unwrap();
    invoke(cx, window, &["File", "Reload All from Disk"]);
    wait_for_signal(cx, "the text of the file", |cx| {
        buffer_of(cx, &view) == "changed\n"
    });

    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.autosave = false, cx)
        })
        .unwrap();
    type_text(cx, window, "unsaved");
    fs::write(&path, "changed again\n").unwrap();
    invoke(cx, window, &["File", "Reload All from Disk"]);
    assert!(
        overlay_is_choice(cx, window),
        "unsaved text must be asked about"
    );
    assert!(
        buffer_of(cx, &view).contains("unsaved"),
        "the unsaved text stays until the user answers"
    );

    click(cx, window, "choice-1");

    assert!(!overlay_is_choice(cx, window), "Cancel leaves the question");
}

#[ui_kit::gpui::test]
fn open_recent_opens_a_project_from_the_same_history(cx: &mut TestAppContext) {
    let other = sample_project("gaps-recent-other");
    let (_, window) = open_sample_window(cx, "gaps-recent");
    let opened: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let recorder = opened.clone();
    let expected = other.clone();
    window
        .update(cx, |workspace, _, cx| {
            workspace.set_project_opener(Rc::new(move |path, _| {
                *recorder.borrow_mut() = Some(path.to_path_buf());
            }));
            workspace.remember_recent_project(expected.clone(), cx);
        })
        .unwrap();
    draw(cx, window);

    let name = other.file_name().unwrap_or_default().to_string_lossy();
    let label = format!("{name}  {}", other.display());
    invoke(cx, window, &["File", "Open Recent", &label]);

    assert_eq!(*opened.borrow(), Some(other));
}

#[ui_kit::gpui::test]
fn invalidate_caches_removes_the_chosen_folders(cx: &mut TestAppContext) {
    let cache_root = scratch("gaps-model-cache");
    let root = sample_project("gaps-invalidate");
    let model = project_model::cache_file(&cache_root, &root);
    let model_folder = model.parent().unwrap().to_path_buf();
    fs::create_dir_all(&model_folder).unwrap();
    fs::write(&model, "{}").unwrap();
    let other_project = cache_root.join("other");
    fs::create_dir_all(&other_project).unwrap();
    let server = scratch("gaps-server-cache");
    fs::create_dir_all(server.join("workspace")).unwrap();
    let window = open_window_with(cx, root.clone(), project_setup(cache_root.clone()));
    let restarted: Rc<RefCell<Option<PathBuf>>> = Rc::new(RefCell::new(None));
    let recorder = restarted.clone();
    window
        .update(cx, |workspace, _, cx| {
            workspace.set_project_opener(Rc::new(move |path, _| {
                *recorder.borrow_mut() = Some(path.to_path_buf());
            }));
            cx.set_global(CacheLocations {
                server: Some(server.clone()),
            })
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    let state_dir = window
        .update(cx, |workspace, _, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let history = state_dir.join("history");

    invoke(cx, window, &["File", "Invalidate Caches..."]);
    draw(cx, window);
    assert!(
        has_selector(cx, window, "invalidate-caches"),
        "File > Invalidate Caches opens the dialog"
    );
    click(cx, window, "invalidate-restart");

    assert_eq!(*restarted.borrow(), Some(root), "the project restarts");
    assert!(model_folder.exists(), "nothing goes while the project runs");
    let pending = workspace::pending_invalidation(&state_dir);
    assert!(pending.contains(&model_folder), "{pending:?}");
    assert!(pending.contains(&server), "{pending:?}");
    assert!(
        !pending.contains(&history),
        "the history of all projects stays unless the user asks"
    );

    let failed = workspace::apply_pending_invalidation(&state_dir, &[&cache_root, &server]);
    assert!(failed.is_empty(), "{failed:?}");
    assert!(!model_folder.exists(), "the project model cache is gone");
    assert!(!server.exists(), "the language server workspace is gone");
    assert!(other_project.exists(), "the caches of other projects stay");
}

#[ui_kit::gpui::test]
fn window_editor_tabs_close_other_unmodified_and_pin(cx: &mut TestAppContext) {
    let root = sample_project("gaps-tabs");
    let main = root.join("src").join("main.txt");
    let notes = root.join("src").join("notes.txt");
    let docker = root.join("Dockerfile");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.autosave = false, cx)
        })
        .unwrap();
    open_path(cx, window, &main);
    open_path(cx, window, &notes);
    open_path(cx, window, &docker);
    open_path(cx, window, &notes);
    type_text(cx, window, "unsaved ");
    open_path(cx, window, &main);

    invoke(
        cx,
        window,
        &["Window", "Editor Tabs", "Close Unmodified Tabs"],
    );
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.open_editors().len())
            .unwrap(),
        1,
        "only the file with unsaved text stays"
    );
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.active_path())
            .unwrap(),
        Some(notes.clone())
    );

    open_path(cx, window, &main);
    invoke(cx, window, &["Window", "Editor Tabs", "Close Other Tabs"]);
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.open_editors().len())
            .unwrap(),
        1
    );

    invoke(cx, window, &["Window", "Editor Tabs", "Pin Tab"]);
    assert_eq!(
        window
            .read_with(cx, |workspace, _| workspace.pinned_tabs())
            .unwrap(),
        vec![main.clone()]
    );

    invoke(cx, window, &["Window", "Editor Tabs", "Pin Tab"]);
    invoke(cx, window, &["Window", "Editor Tabs", "Close All Tabs"]);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.open_editors().is_empty())
            .unwrap()
    );
}

#[ui_kit::gpui::test]
fn store_and_restore_the_default_layout(cx: &mut TestAppContext) {
    let (_, window) = open_sample_window(cx, "gaps-layout");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.dock_open(DockSide::Left))
            .unwrap()
    );

    invoke(cx, window, &["Window", "Store Current Layout as Default"]);
    assert!(cx.read(|cx| {
        cx.try_global::<Settings>()
            .is_some_and(|settings| settings.default_layout.is_some())
    }));

    window
        .update(cx, |workspace, window, cx| {
            workspace.toggle_all_windows(window, cx)
        })
        .unwrap();
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.all_windows_hidden())
            .unwrap()
    );

    invoke(cx, window, &["Window", "Restore Default Layout"]);
    let (hidden, left) = window
        .read_with(cx, |workspace, _| {
            (
                workspace.all_windows_hidden(),
                workspace.dock_open(DockSide::Left),
            )
        })
        .unwrap();
    assert!(!hidden, "the stored layout is back");
    assert!(left);
}

#[ui_kit::gpui::test]
fn exit_asks_about_unsaved_text(cx: &mut TestAppContext) {
    let root = sample_project("gaps-exit");
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(|settings| settings.autosave = false, cx)
        })
        .unwrap();
    type_text(cx, window, "unsaved ");

    invoke(cx, window, &["File", "Exit"]);
    assert!(
        overlay_is_choice(cx, window),
        "Exit must ask before it drops unsaved text"
    );
    let saved = window
        .read_with(cx, |workspace, _| workspace.open_editors().len())
        .unwrap();
    assert_eq!(saved, 1, "the window is still open");

    click(cx, window, "choice-2");
    assert!(!overlay_is_choice(cx, window));
}

#[ui_kit::gpui::test]
fn the_status_bar_lock_toggles_the_read_only_attribute(cx: &mut TestAppContext) {
    let root = sample_project("gaps-read-only");
    let path = root.join("src").join("main.txt");
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);

    invoke(cx, window, &["Edit", "Toggle Read-Only Attribute"]);
    assert!(fs::metadata(&path).unwrap().permissions().readonly());
    draw(cx, window);
    assert!(
        has_selector(cx, window, "st-readonly"),
        "the status bar shows the lock of a read-only file"
    );

    click(cx, window, "st-readonly");
    assert!(!fs::metadata(&path).unwrap().permissions().readonly());
    draw(cx, window);
    assert!(!has_selector(cx, window, "st-readonly"));
}

//! The New menu of the project tree, its submenu and the kinds of the New Class dialog,
//! without a real window.

use std::path::{Path, PathBuf};

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{PROJECT_TOOL, Workspace};

use crate::project::ActiveProjectPanel;
use crate::tree_ops::{Creates, FileTemplate};
use crate::window_tests::{
    draw, open_window_with, press, project_setup, sample_project, scratch, type_text,
    wait_for_signal, with_window,
};

fn class_text(_: &Path, _: &Path, name: &str) -> String {
    format!("public class {name} {{\n}}\n")
}

fn interface_text(_: &Path, _: &Path, name: &str) -> String {
    format!("public interface {name} {{\n}}\n")
}

fn enum_text(_: &Path, _: &Path, name: &str) -> String {
    format!("public enum {name} {{\n}}\n")
}

fn record_text(_: &Path, _: &Path, name: &str) -> String {
    format!("public record {name}() {{\n}}\n")
}

fn annotation_text(_: &Path, _: &Path, name: &str) -> String {
    format!("public @interface {name} {{\n}}\n")
}

fn package_info_text(_: &Path, _: &Path, _: &str) -> String {
    "package demo;\n".to_owned()
}

fn source_templates() -> Vec<FileTemplate> {
    let mut templates = Vec::new();
    for (label, icon, content) in [
        (
            "Class",
            "nodes/class",
            class_text as fn(&Path, &Path, &str) -> String,
        ),
        ("Interface", "nodes/interface", interface_text),
        ("Enum", "nodes/enum", enum_text),
        ("Record", "nodes/record", record_text),
        ("Annotation", "fileTypes/text", annotation_text),
    ] {
        templates.push(FileTemplate {
            label,
            icon,
            extension: "txt",
            creates: Creates::File,
            content,
        });
    }
    templates.push(FileTemplate {
        label: "Package",
        icon: "nodes/package",
        extension: "",
        creates: Creates::Folder { dotted: true },
        content: |_, _, _| String::new(),
    });
    templates.push(FileTemplate {
        label: "package-info",
        icon: "fileTypes/text",
        extension: "txt",
        creates: Creates::File,
        content: package_info_text,
    });
    templates
}

fn open_project(cx: &mut TestAppContext, name: &str) -> (PathBuf, WindowHandle<Workspace>) {
    let root = sample_project(name);
    let mut setup = project_setup(scratch("new-file-cache"));
    setup.templates = source_templates();
    let window = open_window_with(cx, root.clone(), setup);
    cx.run_until_parked();
    draw(cx, window);
    (root, window)
}

fn show_project(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROJECT_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
}

fn select(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &Path) {
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());
    let path = path.to_path_buf();
    with_window(cx, window, |_, cx| {
        panel.update(cx, |panel, cx| panel.select_path(&path, cx))
    });
    show_project(cx, window);
}

fn top_menu_labels(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Vec<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace
                .menu_entries()
                .map(|entries| {
                    entries
                        .iter()
                        .map(|entry| entry.label.to_string())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap()
}

#[ui_kit::gpui::test]
fn the_context_menu_has_one_new_row_with_the_whole_list(cx: &mut TestAppContext) {
    let (root, window) = open_project(cx, "new-menu-context");
    select(cx, window, &root);
    let panel = cx.update(|cx| cx.global::<ActiveProjectPanel>().0.clone());

    let (_, entries) = panel.update(cx, |panel, cx| panel.context_menu_entries(cx));
    let new = entries
        .iter()
        .find(|entry| entry.label.as_ref() == "New")
        .expect("the context menu has New");
    assert!(new.has_submenu());
    let labels: Vec<String> = new
        .submenu
        .iter()
        .map(|entry| entry.label.to_string())
        .collect();
    assert_eq!(labels[0], "New Class");
    for label in [
        "New File",
        "Scratch File",
        "New Directory",
        "New Package",
        "New package-info.txt",
    ] {
        assert!(
            labels.iter().any(|row| row == label),
            "the New list has {label}: {labels:?}"
        );
    }
    for label in ["New Interface", "New Enum", "New Record", "New Annotation"] {
        assert!(
            !labels.iter().any(|row| row == label),
            "the kind {label} lives in the New Class dialog: {labels:?}"
        );
    }
    assert_eq!(
        entries
            .iter()
            .filter(|entry| entry.label.as_ref() == "New")
            .count(),
        1,
        "the New row is one entry"
    );
    assert!(
        entries
            .iter()
            .all(|entry| !entry.label.as_ref().starts_with("New ")),
        "no separate New Item row is left at the top of the menu: {:?}",
        entries
            .iter()
            .map(|entry| entry.label.to_string())
            .collect::<Vec<_>>()
    );
}

#[ui_kit::gpui::test]
fn alt_insert_opens_the_new_list_itself(cx: &mut TestAppContext) {
    let (root, window) = open_project(cx, "new-menu-alt-insert");
    select(cx, window, &root);

    press(cx, window, "alt-insert");
    draw(cx, window);

    let labels = top_menu_labels(cx, window);
    assert_eq!(
        labels.first().map(String::as_str),
        Some("New Class"),
        "Alt+Insert opens the list of the New row: {labels:?}"
    );
    assert!(labels.iter().any(|label| label == "New File"));
    assert!(labels.iter().any(|label| label == "Scratch File"));
}

#[ui_kit::gpui::test]
fn the_kind_list_of_new_class_creates_the_chosen_kind(cx: &mut TestAppContext) {
    let (root, window) = open_project(cx, "new-file-kind");
    let src = root.join("src");
    select(cx, window, &src);

    press(cx, window, "alt-insert");
    draw(cx, window);
    crate::window_tests::click(cx, window, "menu-0");
    draw(cx, window);
    type_text(cx, window, "Payment");
    press(cx, window, "down");
    press(cx, window, "enter");
    draw(cx, window);

    let made = src.join("Payment.txt");
    wait_for_signal(cx, "the new interface", |_| made.is_file());
    let text = std::fs::read_to_string(&made).unwrap();
    assert!(
        text.contains("interface Payment"),
        "the chosen kind makes an interface: {text}"
    );

    // The dialog remembers Interface for the next new class.
    select(cx, window, &src);
    press(cx, window, "alt-insert");
    draw(cx, window);
    crate::window_tests::click(cx, window, "menu-0");
    draw(cx, window);
    type_text(cx, window, "Service");
    press(cx, window, "enter");
    draw(cx, window);

    let made = src.join("Service.txt");
    wait_for_signal(cx, "the second interface", |_| made.is_file());
    let text = std::fs::read_to_string(&made).unwrap();
    assert!(
        text.contains("interface Service"),
        "the kind of the last time is chosen again: {text}"
    );
}

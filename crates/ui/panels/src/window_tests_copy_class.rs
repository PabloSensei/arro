//! Copy Class in the project tree, on a language of the test: the window asks for the
//! name and the folder, the declaration and the package follow the new place, and the
//! unsaved text of an open buffer is the one copied. The last test runs the same keys
//! against the real server on a copy of PetClinic.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::Languages;
use lang_api::{LanguageFn, LanguageId, LanguageSupport, Queries, SyntaxProvider};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::project::ActiveProjectPanel;
use crate::window_tests::{
    draw, editor_of, open_path, press, project_setup, scratch, type_text, wait_for_signal,
};
use crate::{AgentOptions, LanguageServer, connect, install};

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

struct NoSyntax(Queries);

impl SyntaxProvider for NoSyntax {
    fn grammar(&self) -> LanguageFn {
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &self.0
    }
}

fn is_word(character: char) -> bool {
    character.is_alphanumeric() || character == '_' || character == '$'
}

/// Every whole-word `old` of `text` as `new`, as Copy Class renames a declaration.
fn rename_word(text: &str, old: &str, new: &str) -> String {
    let mut changed = String::with_capacity(text.len() + new.len());
    let mut rest = text;
    while let Some(found) = rest.find(old) {
        let before = rest[..found].chars().next_back();
        let after = rest[found + old.len()..].chars().next();
        changed.push_str(&rest[..found]);
        if before.is_some_and(is_word) || after.is_some_and(is_word) {
            changed.push_str(old);
        } else {
            changed.push_str(new);
        }
        rest = &rest[found + old.len()..];
    }
    changed.push_str(rest);
    changed
}

/// The package of a folder under the source root, as the language of this test sees it.
fn package_of(root: &Path, folder: &Path) -> String {
    let relative = folder.strip_prefix(root).unwrap_or(folder);
    let parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let source_root = parts.iter().rposition(|part| part == "source");
    source_root.map_or_else(String::new, |at| parts[at + 1..].join("."))
}

fn repackage(text: &str, package: &str) -> String {
    let newline = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_owned())
        .collect();
    let header = lines.iter().position(|line| {
        let trimmed = line.trim_start();
        trimmed.starts_with("package ") && trimmed.trim_end().ends_with(';')
    });
    match header {
        Some(index) if package.is_empty() => {
            lines.remove(index);
            if lines.get(index).is_some_and(|line| line.trim().is_empty()) {
                lines.remove(index);
            }
        }
        Some(index) => lines[index] = format!("package {package};"),
        None if !package.is_empty() => {
            lines.insert(0, format!("package {package};"));
            lines.insert(1, String::new());
        }
        None => {}
    }
    lines.join(newline)
}

/// The language of the window tests: a `class Name` declaration and a `package` line.
struct TestLanguage {
    syntax: NoSyntax,
}

impl TestLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
        })
    }
}

impl LanguageSupport for TestLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("copy-class-test")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("cls"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn copy_element_text(&self, text: &str, name: &str) -> Option<String> {
        let start = text.find("class ")? + "class ".len();
        let end = text[start..]
            .char_indices()
            .find(|(_, character)| !is_word(*character))
            .map_or(text.len(), |(offset, _)| start + offset);
        if start >= end || !text[start..end].chars().next()?.is_alphabetic() {
            return None;
        }
        Some(rename_word(text, &text[start..end], name))
    }

    fn copy_element_in_folder(
        &self,
        text: &str,
        name: &str,
        root: &Path,
        folder: &Path,
    ) -> Option<String> {
        let renamed = self.copy_element_text(text, name)?;
        Some(repackage(&renamed, &package_of(root, folder)))
    }
}

fn open_language_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<TestLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("copy-class-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |path| dockerfile_syntax::is_dockerfile(path).then(dockerfile_syntax::config),
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
    let setup = project_setup(scratch("copy-class-cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server ready".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(language as Arc<dyn LanguageSupport>),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            setup,
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
        fs::write(path, text).unwrap();
    }
    root
}

fn project_panel(cx: &TestAppContext) -> gpui::Entity<crate::project::ProjectPanel> {
    cx.read(|cx| cx.global::<ActiveProjectPanel>().0.clone())
}

fn focus_project(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    crate::window_tests_menus::show_project(cx, window);
    let panel = project_panel(cx);
    window
        .update(cx, |_, window, cx| {
            window.focus(&panel.read(cx).focus(), cx);
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
}

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

const THING: &str = "package a;

class Thing {
    Thing() {}
    Thing next;
    static Thing of() { return new Thing(); }
}
";

#[ui_kit::gpui::test]
fn f5_opens_copy_class_and_renames_the_class_its_constructors_and_its_own_uses(
    cx: &mut TestAppContext,
) {
    let source = "src/main/source/a/Thing.cls";
    let root = project("copy-class-f5", &[(source, THING)]);
    let language = TestLanguage::new();
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let from = root.join(source);
    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&from, cx));
    focus_project(cx, window);
    press(cx, window, "f5");
    draw(cx, window);
    assert!(
        shown(cx, window, "copy-class"),
        "F5 opens the Copy Class window"
    );

    // The same folder already has the class, so the copy takes the next number.
    press(cx, window, "enter");
    let copy = root.join("src/main/source/a/Thing2.cls");
    wait_for_signal(cx, "the copy", |_| copy.is_file());
    let text = fs::read_to_string(&copy).unwrap();
    assert!(text.contains("class Thing2 {"), "{text}");
    assert!(text.contains("Thing2() {}"), "{text}");
    assert!(text.contains("Thing2 next;"), "{text}");
    assert!(text.contains("static Thing2 of()"), "{text}");
    assert!(text.contains("new Thing2()"), "{text}");
    assert!(text.contains("package a;"), "the package stays: {text}");
    assert!(
        !text.contains("class Thing {"),
        "the old name is gone: {text}"
    );
}

#[ui_kit::gpui::test]
fn f5_of_a_class_at_the_project_root_offers_the_root_as_its_folder(cx: &mut TestAppContext) {
    let root = project("copy-class-root", &[("Thing.cls", THING)]);
    let language = TestLanguage::new();
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let from = root.join("Thing.cls");
    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&from, cx));
    focus_project(cx, window);
    press(cx, window, "f5");
    draw(cx, window);
    press(cx, window, "enter");
    let copy = root.join("Thing2.cls");
    wait_for_signal(cx, "the copy", |_| copy.is_file());
    let text = fs::read_to_string(&copy).unwrap();
    assert!(
        text.starts_with("class Thing2 {"),
        "the root of the sources has no package: {text}"
    );
}

#[ui_kit::gpui::test]
fn ctrl_c_ctrl_v_into_another_package_opens_the_same_window_and_writes_the_package(
    cx: &mut TestAppContext,
) {
    let source = "src/main/source/a/Thing.cls";
    let root = project(
        "copy-class-paste",
        &[(source, THING), ("src/main/source/b/.keep", "")],
    );
    let folder = root.join("src/main/source/b");
    let language = TestLanguage::new();
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let from = root.join(source);
    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&from, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-c");
    panel.update(cx, |panel, cx| panel.select_path(&folder, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-v");
    draw(cx, window);
    assert!(
        shown(cx, window, "copy-class"),
        "Ctrl+V of a class opens the Copy Class window"
    );
    press(cx, window, "enter");
    let copy = folder.join("Thing.cls");
    wait_for_signal(cx, "the copy", |_| copy.is_file());
    let text = fs::read_to_string(&copy).unwrap();
    assert!(text.contains("package b;"), "{text}");
    assert!(text.contains("class Thing {"), "{text}");

    // The next copy into the same folder takes `Thing2`, not another suffix.
    panel.update(cx, |panel, cx| panel.select_path(&folder, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-v");
    draw(cx, window);
    press(cx, window, "enter");
    let second = folder.join("Thing2.cls");
    wait_for_signal(cx, "the second copy", |_| second.is_file());
    assert!(
        fs::read_to_string(&second)
            .unwrap()
            .contains("class Thing2 {")
    );
}

#[ui_kit::gpui::test]
fn a_cut_class_moves_to_another_package_with_the_new_package_line(cx: &mut TestAppContext) {
    let source = "src/main/source/a/Thing.cls";
    let root = project(
        "copy-class-cut",
        &[(source, THING), ("src/main/source/b/.keep", "")],
    );
    let folder = root.join("src/main/source/b");
    let language = TestLanguage::new();
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let from = root.join(source);
    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&from, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-x");
    panel.update(cx, |panel, cx| panel.select_path(&folder, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-v");
    draw(cx, window);
    assert!(
        shown(cx, window, "copy-class"),
        "Ctrl+V of a cut class opens the same window"
    );
    press(cx, window, "enter");
    let moved = folder.join("Thing.cls");
    wait_for_signal(cx, "the moved class", |_| moved.is_file() && !from.exists());
    let text = fs::read_to_string(&moved).unwrap();
    assert!(text.contains("package b;"), "{text}");
    assert!(text.contains("class Thing {"), "{text}");
}

#[ui_kit::gpui::test]
fn the_copy_of_an_open_file_takes_the_unsaved_text_of_the_buffer(cx: &mut TestAppContext) {
    let source = "src/main/source/a/Thing.cls";
    let root = project(
        "copy-class-buffer",
        &[(source, THING), ("src/main/source/b/.keep", "")],
    );
    let folder = root.join("src/main/source/b");
    let language = TestLanguage::new();
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let from = root.join(source);
    open_path(cx, window, &from);
    let view = editor_of(cx, window, &from);
    view.update(cx, |view, cx| {
        view.replace_text(0..0, "// from the buffer\n", cx)
    });
    assert!(view.read_with(cx, |view, _| view.is_dirty()));

    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&from, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-c");
    panel.update(cx, |panel, cx| panel.select_path(&folder, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-v");
    draw(cx, window);
    press(cx, window, "enter");
    let copy = folder.join("Thing.cls");
    wait_for_signal(cx, "the copy", |_| copy.is_file());
    let text = fs::read_to_string(&copy).unwrap();
    assert!(
        text.starts_with("// from the buffer\n"),
        "the copy is the buffer, not the disk: {text}"
    );
    assert!(text.contains("package b;"), "{text}");
}

#[ui_kit::gpui::test]
fn copies_of_a_plain_file_grow_the_number_of_a_copy(cx: &mut TestAppContext) {
    let root = project(
        "copy-class-numbers",
        &[("src/Name_1.txt", "one\n"), ("src/Name_2.txt", "two\n")],
    );
    let language = TestLanguage::new();
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);

    let source = root.join("src").join("Name_1.txt");
    let panel = project_panel(cx);
    panel.update(cx, |panel, cx| panel.select_path(&source, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-c");
    press(cx, window, "ctrl-v");
    wait_for_signal(cx, "the next copy", |_| {
        root.join("src").join("Name_3.txt").is_file()
    });
    assert!(!root.join("src").join("Name_1_1.txt").exists());
}

// The same keys on the real server and a copy of PetClinic.
#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn a_class_copied_into_another_package_with_a_new_name_compiles(cx: &mut TestAppContext) {
    use crate::window_tests_real::{
        build_window, close, errors, finish, project, wait_for, wait_ready,
    };

    const LIMIT: Duration = Duration::from_secs(120);
    let started = std::time::Instant::now();
    let (root, language) = project("real-copy-class");
    let package = "org.springframework.samples.petclinic";
    let from = window_language::main_class(&root, &format!("{package}.owner"), "Owner");
    let to = window_language::main_class(&root, &format!("{package}.vet"), "OwnerCopy");
    let vet = to
        .parent()
        .expect("the target package folder")
        .to_path_buf();
    assert!(from.is_file(), "the corpus has the class to copy");

    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let panel = project_panel(cx);
    crate::window_tests_menus::show_project(cx, window);
    crate::window_tests_menus::reveal(cx, window, &from);
    crate::window_tests_menus::reveal(cx, window, &vet);
    panel.update(cx, |panel, cx| panel.select_path(&from, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-c");
    panel.update(cx, |panel, cx| panel.select_path(&vet, cx));
    focus_project(cx, window);
    press(cx, window, "ctrl-v");
    draw(cx, window);
    wait_for(cx, "the Copy Class window", LIMIT, |cx| {
        shown(cx, window, "copy-class")
    });
    press(cx, window, "ctrl-a");
    type_text(cx, window, "OwnerCopy");
    press(cx, window, "enter");
    wait_for(cx, "the copy", LIMIT, |_| to.is_file());
    let text = fs::read_to_string(&to).unwrap();
    assert!(text.contains("public class OwnerCopy"), "{text}");
    assert!(
        text.contains("package org.springframework.samples.petclinic.vet;"),
        "{text}"
    );

    open_path(cx, window, &to);
    let view = editor_of(cx, window, &to);
    draw(cx, window);
    wait_for(cx, "the copy to have no errors", LIMIT, |cx| {
        errors(cx, &view) == 0
    });

    close(cx, window, language, false);
    finish(
        "a_class_copied_into_another_package_with_a_new_name_compiles",
        started,
    );
}

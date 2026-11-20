//! The scope row of Find in Files: the search keeps to the project, to one folder or to
//! the open files, the file mask filters its list, and an empty query says how to start.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ui_kit::gpui;
use ui_kit::gpui::{Entity, TestAppContext, WindowHandle};
use workspace::Workspace;

use crate::finder::{ActiveFinder, Finder, Scope};
use crate::git::ActiveGit;
use crate::project_structure::ProjectEnvironmentGlobal;
use crate::window_tests::{
    bounds_of, click, draw, git_repository, open_window, press, scratch, type_text,
    wait_for_file_index, wait_for_signal,
};

pub(crate) fn finder(cx: &TestAppContext) -> Entity<Finder> {
    cx.read(|cx| {
        cx.try_global::<ActiveFinder>()
            .and_then(|finder| finder.0.clone())
            .expect("Find in Files is open")
    })
}

/// The text the empty state of the window shows in the last frame.
fn empty_hint() -> String {
    ui_kit::test_bounds()
        .into_iter()
        .find(|(name, _)| name == "finder-empty-hint")
        .and_then(|(_, bounds)| bounds.label)
        .map_or_else(String::new, |label| label.to_string())
}

/// The project, then the dialog with its own scope row.
pub(crate) fn open_finder(
    cx: &mut TestAppContext,
    name: &str,
    files: &[(&str, &str)],
) -> WindowHandle<Workspace> {
    let root = scratch(name);
    for (file, text) in files {
        let path = root.join(file);
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder).unwrap();
        }
        fs::write(path, text).unwrap();
    }
    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);
    let finder = wait_for_finder(cx, window);
    wait_for_signal(cx, "the file list of the dialog", |cx| {
        finder.read_with(cx, |finder, cx| {
            finder.files().read(cx).len() >= files.len()
        })
    });
    window
}

/// Ctrl+Shift+F and the entity of the dialog it opened.
pub(crate) fn wait_for_finder(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
) -> Entity<Finder> {
    press(cx, window, "ctrl-shift-f");
    wait_for_signal(cx, "the finder", |cx| {
        cx.read(|cx| {
            cx.try_global::<ActiveFinder>()
                .is_some_and(|global| global.0.is_some())
        })
    });
    draw(cx, window);
    finder(cx)
}

/// Runs a search and returns the text of its result after the answer.
pub(crate) fn search(cx: &mut TestAppContext, finder: &Entity<Finder>, query: &str) -> String {
    finder.update(cx, |finder, cx| finder.search(query, cx));
    let mut exported = String::new();
    wait_for_signal(cx, "the answer of the search", |cx| {
        let (searching, text) = finder.read_with(cx, |finder, _| {
            (finder.is_searching(), finder.export_text())
        });
        exported = text;
        !searching
    });
    exported
}

#[gpui::test]
fn the_scope_row_sits_under_the_query_and_an_empty_query_shows_a_hint(cx: &mut TestAppContext) {
    let window = open_finder(
        cx,
        "finder-scope-row",
        &[("a.txt", "needle\n"), ("b.md", "needle\n")],
    );
    draw(cx, window);

    let _ = bounds_of(cx, window, "finder-scope-row");
    let _ = bounds_of(cx, window, "finder-scope-project");
    let _ = bounds_of(cx, window, "finder-module");
    let _ = bounds_of(cx, window, "finder-scope-directory");
    let _ = bounds_of(cx, window, "finder-scope");
    let _ = bounds_of(cx, window, "finder-mask-check");
    let _ = bounds_of(cx, window, "finder-mask");
    assert_eq!(
        empty_hint(),
        "Type to search in the project",
        "an empty query says how to start"
    );
}

#[gpui::test]
fn the_directory_scope_keeps_the_search_to_one_folder(cx: &mut TestAppContext) {
    let _window = open_finder(
        cx,
        "finder-scope-directory",
        &[
            ("top.txt", "needle at the top\n"),
            ("nested/below.txt", "needle below\n"),
        ],
    );
    let finder = finder(cx);
    let nested = finder.read_with(cx, |finder, _| finder.root().join("nested"));

    finder.update(cx, |finder, cx| {
        finder.set_scope(Scope::Directory, Some(nested.clone()), cx)
    });
    let text = search(cx, &finder, "needle");
    assert!(
        text.contains("below.txt"),
        "the folder is searched: {text:?}"
    );
    assert!(
        !text.contains("top.txt"),
        "the rest of the project is not: {text:?}"
    );
}

#[gpui::test]
fn the_file_mask_drops_the_files_of_other_types(cx: &mut TestAppContext) {
    let _window = open_finder(
        cx,
        "finder-scope-mask",
        &[("notes.txt", "needle\n"), ("Owner.md", "needle\n")],
    );
    let finder = finder(cx);

    finder.update(cx, |finder, cx| finder.set_mask("*.txt", true, cx));
    let text = search(cx, &finder, "needle");
    assert!(text.contains("notes.txt"), "the mask matches: {text:?}");
    assert!(
        !text.contains("Owner.md"),
        "a file of another type is outside the mask: {text:?}"
    );

    finder.update(cx, |finder, cx| finder.set_scope(Scope::Project, None, cx));
    let text = search(cx, &finder, "needle");
    assert!(
        !text.contains("Owner.md"),
        "the mask still keeps the search: {text:?}"
    );
}

#[gpui::test]
fn the_scope_and_the_mask_are_remembered_between_openings(cx: &mut TestAppContext) {
    let window = open_finder(
        cx,
        "finder-scope-remembered",
        &[("notes.txt", "needle\n"), ("nested/inside.txt", "needle\n")],
    );
    let finder = finder(cx);
    let root = finder.read_with(cx, |finder, _| finder.root().clone());
    let directory = root.join("nested");
    finder.update(cx, |finder, cx| {
        finder.set_scope(Scope::Directory, Some(directory.clone()), cx);
        finder.set_mask("*.txt", true, cx);
    });

    // The same folder opens the dialog again, as Ctrl+Shift+F does in another window.
    let files = finder.read_with(cx, |finder, _| finder.files().clone());
    window
        .update(cx, |workspace, window, cx| {
            Finder::show(false, files, root, workspace, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    let reopened = crate::window_tests_finder_scope::finder(cx);

    assert_eq!(
        reopened.read_with(cx, |finder, _| finder.scope()),
        Scope::Directory,
        "the scope comes back"
    );
    assert_eq!(
        reopened.read_with(cx, |finder, _| finder.mask_text().to_owned()),
        "*.txt",
        "the mask comes back"
    );
    assert!(reopened.read_with(cx, |finder, _| finder.mask_on()));
}

#[gpui::test]
fn the_open_files_scope_searches_only_the_tabs(cx: &mut TestAppContext) {
    let root = scratch("finder-scope-open");
    fs::write(root.join("opened.txt"), "needle in the tab\n").unwrap();
    fs::write(root.join("closed.txt"), "needle on the disk\n").unwrap();
    let window = open_window(cx, root.clone());
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);
    crate::window_tests::open_path(cx, window, &root.join("opened.txt"));
    let finder = wait_for_finder(cx, window);
    wait_for_signal(cx, "the file list of the dialog", |cx| {
        finder.read_with(cx, |finder, cx| finder.files().read(cx).len() >= 2)
    });

    finder.update(cx, |finder, cx| {
        finder.set_scope(Scope::OpenFiles, None, cx)
    });
    let text = search(cx, &finder, "needle");
    assert!(text.contains("opened.txt"), "the tab is searched: {text:?}");
    assert!(
        !text.contains("closed.txt"),
        "a file without a tab is not: {text:?}"
    );
}

#[gpui::test]
fn typing_a_mask_turns_its_checkbox_on(cx: &mut TestAppContext) {
    let window = open_finder(
        cx,
        "finder-scope-mask-typing",
        &[("notes.txt", "needle\n"), ("Owner.md", "needle\n")],
    );
    let finder = finder(cx);
    type_text(cx, window, "needle");
    wait_for_signal(cx, "the first search", |cx| {
        !finder.read_with(cx, |finder, _| finder.is_searching())
    });

    // Ctrl+Alt+F puts the mask field under the keys, as its shortcut always did.
    press(cx, window, "ctrl-alt-f");
    draw(cx, window);
    type_text(cx, window, "*.txt");
    wait_for_signal(cx, "the search on the typed mask", |cx| {
        finder.read_with(cx, |finder, _| {
            finder.mask_text() == "*.txt"
                && finder.mask_on()
                && !finder.export_text().contains("Owner.md")
                && finder.export_text().contains("notes.txt")
        })
    });
}

/// The project model of these tests: the Module dropdown lists its modules, and the Module
/// scope searches their source and resource folders.
struct ModelProject {
    model: project_model::ProjectModel,
}

impl project_model::EnvironmentProvider for ModelProject {
    fn describe(&self) -> project_model::ProjectEnvironmentInfo {
        project_model::ProjectEnvironmentInfo::default()
    }

    fn model(&self) -> Option<Arc<project_model::ProjectModel>> {
        Some(Arc::new(self.model.clone()))
    }

    fn runtime_at(&self, _home: &Path) -> Result<project_model::RuntimeInfo, String> {
        Err("the test project has no runtimes".to_owned())
    }

    fn apply(
        &self,
        _selected: Option<&Path>,
        _added: &[PathBuf],
    ) -> Result<project_model::ProjectEnvironmentInfo, String> {
        Err("the test project is not changed".to_owned())
    }
}

/// A module of the test model whose sources are one folder and whose resources are another.
fn test_module(
    root: &Path,
    id: &str,
    sources: &str,
    resources: Option<&str>,
) -> project_model::Module {
    project_model::Module {
        id: id.to_owned(),
        name: id.to_owned(),
        dir: root.join(id),
        build_file: None,
        generated_roots: Vec::new(),
        excluded_roots: Vec::new(),
        units: vec![project_model::Unit {
            name: project_model::MAIN_UNIT.to_owned(),
            source_roots: vec![root.join(sources)],
            resource_roots: resources
                .map(|folder| root.join(folder))
                .into_iter()
                .collect(),
            ..project_model::Unit::default()
        }],
    }
}

#[gpui::test]
fn the_module_scope_keeps_the_search_to_the_folders_of_the_module(cx: &mut TestAppContext) {
    let _window = open_finder(
        cx,
        "finder-scope-module",
        &[
            ("core/src/one.txt", "needle\n"),
            ("core/resources/three.txt", "needle\n"),
            ("core/notes.txt", "needle\n"),
            ("app/src/two.txt", "needle\n"),
        ],
    );
    let finder = finder(cx);
    let root = finder.read_with(cx, |finder, _| finder.root().as_ref().clone());
    let model = project_model::ProjectModel {
        root: root.clone(),
        tool: "test-tool".to_owned(),
        modules: vec![
            test_module(&root, "core", "core/src", Some("core/resources")),
            test_module(&root, "app", "app/src", None),
        ],
        ..project_model::ProjectModel::default()
    };
    let environment: Arc<dyn project_model::EnvironmentProvider> = Arc::new(ModelProject { model });
    cx.update(|cx| cx.set_global(ProjectEnvironmentGlobal(Some(environment))));

    assert_eq!(
        finder.read_with(cx, |finder, cx| finder.module_names(cx)),
        vec!["core".to_owned(), "app".to_owned()],
        "the dropdown lists the modules of the model"
    );
    finder.update(cx, |finder, cx| finder.set_module("core", cx));
    let text = search(cx, &finder, "needle");
    assert!(
        text.contains("one.txt"),
        "the source folder is searched: {text:?}"
    );
    assert!(
        text.contains("three.txt"),
        "the resource folder is searched: {text:?}"
    );
    assert!(!text.contains("two.txt"), "another module is not: {text:?}");
    assert!(
        !text.contains("notes.txt"),
        "a file outside the folders of the module is not: {text:?}"
    );
    assert_eq!(
        finder.read_with(cx, |finder, _| finder.scope()),
        Scope::Module
    );
}

#[gpui::test]
fn the_changed_files_scope_searches_the_files_git_lists_as_changed(cx: &mut TestAppContext) {
    // The base commit holds main.txt; quiet.txt is committed too and stays unchanged.
    let root = git_repository("finder-scope-changed");
    let git = vcs::GitRunner::from_env().expect("git is on the PATH");
    fs::write(root.join("quiet.txt"), "needle at rest\n").unwrap();
    git.run(&root, ["add", "quiet.txt"]).unwrap();
    git.run(&root, ["commit", "-m", "quiet"]).unwrap();
    fs::write(root.join("main.txt"), "alpha\nneedle changed\n").unwrap();

    let window = open_window(cx, root);
    cx.run_until_parked();
    draw(cx, window);
    wait_for_file_index(cx);
    let finder = wait_for_finder(cx, window);
    wait_for_signal(cx, "the status of the repository", |cx| {
        cx.read(|cx| {
            cx.global::<ActiveGit>()
                .0
                .read(cx)
                .status()
                .files
                .iter()
                .any(|file| file.path == "main.txt")
        })
    });

    assert!(
        finder.read_with(cx, |finder, cx| finder
            .scope_available(Scope::ChangedFiles, cx)),
        "a repository lists its changes"
    );
    finder.update(cx, |finder, cx| {
        finder.set_scope(Scope::ChangedFiles, None, cx)
    });
    let text = search(cx, &finder, "needle");
    assert!(
        text.contains("main.txt"),
        "the changed file is searched: {text:?}"
    );
    assert!(
        !text.contains("quiet.txt"),
        "an unchanged file is not: {text:?}"
    );
}

#[gpui::test]
fn without_a_git_repository_changed_files_is_unavailable(cx: &mut TestAppContext) {
    let window = open_finder(cx, "finder-scope-no-git", &[("notes.txt", "needle\n")]);
    cx.run_until_parked();
    let finder = finder(cx);
    assert!(
        !finder.read_with(cx, |finder, cx| finder
            .scope_available(Scope::ChangedFiles, cx)),
        "a project outside Git has no list of changes"
    );

    // The Scope dropdown lists the item grey: a click on it does not choose it.
    click(cx, window, "finder-scope");
    draw(cx, window);
    click(cx, window, "finder-scope-item-changed-files");
    assert_eq!(
        finder.read_with(cx, |finder, _| finder.scope()),
        Scope::Project,
        "the grey item stays unchosen"
    );
}

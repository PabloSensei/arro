//! The Changed files tree of the Git Log without a window: every folder row has the folder
//! icon beside its chevron, and every file row the icon of its type, the one the tabs and
//! the project tree give. The Log opens on a commit that removed the files from the working
//! copy, so the project tree draws none of them and the icons found are the Git Log's.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::metrics::ICON;
use ui_kit::{Theme, icon_file};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::git_log::ActiveGitLog;
use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, click, draw, project_setup, scratch, wait_for_settled_bounds,
};
use crate::window_tests_git::{git, git_state, wait_for, wait_for_repository};
use crate::window_tests_git_window::{log, open_log};
use crate::{AgentOptions, LanguageServer, connect, install};

const FOLDER: &str = "nodes/folder";

/// The files of the first commit: a class file, an XML file and a text file, in folders.
const FILES: [(&str, &str); 3] = [
    ("src/main/kls/org/demo/App.kls", "class App {}\n"),
    ("src/main/resources/app.xml", "<app/>\n"),
    ("src/main/notes.txt", "notes\n"),
];

/// The icons the application gives by file name or extension, as its `file_icons` gives
/// them to the tabs and the project tree.
const APP_ICONS: [(&str, &str); 4] = [
    (".gitignore", "fileTypes/gitignore"),
    ("kls", "nodes/class"),
    ("xml", "fileTypes/xml"),
    ("iml", "fileTypes/ideaModule"),
];

/// A repository whose Log opens on a commit that removes the files of the first one from
/// the working copy.
fn repository(name: &str) -> PathBuf {
    let root = scratch(name);
    let git = git();
    git.run(&root, ["init"]).unwrap();
    git.run(&root, ["symbolic-ref", "HEAD", "refs/heads/main"])
        .unwrap();
    git.run(&root, ["config", "user.email", "window-test@example.com"])
        .unwrap();
    git.run(&root, ["config", "user.name", "Window Test"])
        .unwrap();
    for (path, text) in FILES {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
    git.run(&root, ["add", "."]).unwrap();
    git.run(&root, ["commit", "-m", "sources"]).unwrap();
    git.run(&root, ["rm", "-r", "-q", "src"]).unwrap();
    git.run(&root, ["commit", "-m", "remove sources"]).unwrap();
    root
}

/// A window whose file icons are the application's, so the class and XML files of the
/// project have the icons they have there.
fn open_window_with_icons(cx: &mut TestAppContext, root: PathBuf) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("log-icons-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(editor_view::Languages {
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
            by_name: APP_ICONS.to_vec(),
            fallback: "fileTypes/text",
        }
        .with_languages(),
        background: true,
        info: Default::default(),
    };
    let setup = project_setup(scratch("log-icons-cache"));
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

/// A debug selector built at run time. A test lives as long as the process, so a name
/// leaked here costs nothing that matters.
fn selector(name: String) -> &'static str {
    Box::leak(name.into_boxed_str())
}

/// The debug selector of the icon `name` in the theme of the window.
fn icon_selector(set: &str, name: &str, dark: bool) -> &'static str {
    selector(icon_file(set, name, dark, ICON).to_string())
}

/// The icon is drawn inside the row.
fn icon_in_row(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    row: &'static str,
    icon: &'static str,
) {
    let bounds = bounds_of(cx, window, row);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    let drawn = visual
        .debug_bounds(icon)
        .unwrap_or_else(|| panic!("the window draws no {icon}"));
    assert!(
        bounds.contains(&drawn.center()),
        "the icon {icon} is outside the row {row}: {drawn:?} and {bounds:?}"
    );
}

#[ui_kit::gpui::test]
fn the_changed_files_tree_gives_each_folder_and_each_file_type_its_icon(cx: &mut TestAppContext) {
    let root = repository("git-log-icons");
    let window = open_window_with_icons(cx, root);
    cx.run_until_parked();
    let state = git_state(cx);
    wait_for_repository(cx, &state);
    open_log(cx, window);
    wait_for_settled_bounds(cx, window, "log-commit-0");
    click(cx, window, "log-commit-0");
    wait_for(cx, "the files of the commit", |cx| {
        log(cx, |panel| panel.files().len() == FILES.len())
    });
    draw(cx, window);

    // The label and the icon of every row of the tree, in the order the rows are drawn.
    let rows = cx.read(|cx| cx.global::<ActiveGitLog>().0.read(cx).tree_icons(cx));
    let mut found = rows.clone();
    found.sort();
    let mut expected: Vec<(String, &'static str)> = vec![
        ("App.kls".to_owned(), "nodes/class"),
        ("app.xml".to_owned(), "fileTypes/xml"),
        ("demo".to_owned(), FOLDER),
        ("kls".to_owned(), FOLDER),
        ("main".to_owned(), FOLDER),
        ("notes.txt".to_owned(), "fileTypes/text"),
        ("org".to_owned(), FOLDER),
        ("resources".to_owned(), FOLDER),
        ("src".to_owned(), FOLDER),
    ];
    expected.sort();
    assert_eq!(
        found, expected,
        "every row of the Changed files tree has the icon of its folder or file type"
    );

    // What the window draws: the file icons sit inside their own rows, and a folder icon
    // sits inside one of the folder rows.
    let (set, dark) = cx.read(|cx| {
        let theme = cx.global::<Theme>();
        (theme.icon_set(), theme.dark)
    });
    let files = log(cx, |panel| panel.files());
    let file_row = |name: &str| {
        let index = files
            .iter()
            .position(|path| path.ends_with(name))
            .unwrap_or_else(|| panic!("the commit has no {name}"));
        selector(format!("log-file-{index}"))
    };
    icon_in_row(
        cx,
        window,
        file_row("App.kls"),
        icon_selector(set, "nodes/class", dark),
    );
    icon_in_row(
        cx,
        window,
        file_row("app.xml"),
        icon_selector(set, "fileTypes/xml", dark),
    );
    icon_in_row(
        cx,
        window,
        file_row("notes.txt"),
        icon_selector(set, "fileTypes/text", dark),
    );

    let folder_rows: Vec<&'static str> = rows
        .iter()
        .enumerate()
        .filter(|(_, (_, icon))| *icon == FOLDER)
        .map(|(row, _)| selector(format!("log-folder-{row}")))
        .collect();
    let folder_icon = icon_selector(set, FOLDER, dark);
    let drawn = {
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        visual
            .debug_bounds(folder_icon)
            .unwrap_or_else(|| panic!("the window draws no {folder_icon}"))
    };
    let in_a_folder_row = folder_rows
        .iter()
        .any(|&row| bounds_of(cx, window, row).contains(&drawn.center()));
    assert!(
        in_a_folder_row,
        "the folder icon {folder_icon} is drawn in no folder row: {drawn:?}"
    );
}

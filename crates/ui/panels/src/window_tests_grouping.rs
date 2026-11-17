//! The grouping, filters, masks, history and export of the Usages, TODO and Find in
//! Files windows, driven by a language of the test.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, Confidence, DocumentSpot, LanguageFn, LanguageId, LanguageSupport, Provider, Queries,
    SourceId, SourceSpan, SyntaxProvider, UsageEntry, UsageKind,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, TODO_TOOL, USAGES_TOOL, Workspace};

use crate::finder::ActiveFinder;
use crate::index::SearchSetup;
use crate::items::PanelRow;
use crate::todo::ActiveTodo;
use crate::usages::ActiveUsages;
use crate::window_tests::{
    bounds_of, click, draw, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_lang::{project, span_in, uri_of};
use crate::window_tests_ui::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("grouping-server");

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

/// The language of these tests: every usage, module and package is set by the test.
struct GroupingLanguage {
    syntax: NoSyntax,
    usages: Mutex<Vec<UsageEntry>>,
    modules: Mutex<HashMap<PathBuf, String>>,
    packages: Mutex<HashMap<PathBuf, String>>,
}

impl GroupingLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            usages: Mutex::new(Vec::new()),
            modules: Mutex::new(HashMap::new()),
            packages: Mutex::new(HashMap::new()),
        })
    }

    fn set_usages(&self, entries: Vec<UsageEntry>) {
        *self.usages.lock().unwrap() = entries;
    }

    fn set_module(&self, path: &Path, module: &str) {
        self.modules
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), module.to_owned());
    }

    fn set_package(&self, path: &Path, package: &str) {
        self.packages
            .lock()
            .unwrap()
            .insert(path.to_path_buf(), package.to_owned());
    }
}

impl Provider<DocumentSpot, Vec<UsageEntry>> for GroupingLanguage {
    fn start(&self, _: DocumentSpot, _: base::Cancel, mut reply: lang_api::Reply<Vec<UsageEntry>>) {
        let entries = self.usages.lock().unwrap().clone();
        let _ = reply.send(Answer::new(entries, Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for GroupingLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("grouping")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn usages(&self) -> Option<&lang_api::UsageProvider> {
        Some(self)
    }

    fn module_of(&self, path: &Path) -> Option<String> {
        self.modules.lock().unwrap().get(path).cloned()
    }

    fn package_of(&self, path: &Path) -> Option<String> {
        self.packages.lock().unwrap().get(path).cloned()
    }
}

fn open_language_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<GroupingLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
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
            project_setup(scratch("cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// The first file under `root` whose stem is `stem`.
fn file_by_stem(root: &Path, stem: &str) -> Option<PathBuf> {
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let entries = std::fs::read_dir(&folder).ok()?;
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                folders.push(path);
            } else if path.file_stem().is_some_and(|name| name == stem) {
                return Some(path);
            }
        }
    }
    None
}

fn usage(path: &Path, text: &str, start: usize, end: usize, kind: UsageKind) -> UsageEntry {
    UsageEntry {
        target: SourceSpan {
            uri: uri_of(path),
            span: span_in(text, start, end),
        },
        kind,
        container: Some("Main.run".to_owned()),
    }
}

fn usages_hub(cx: &TestAppContext) -> gpui::Entity<crate::usages::UsagesHub> {
    cx.read(|cx| cx.global::<ActiveUsages>().0.clone())
}

fn usages_labels(cx: &TestAppContext) -> Vec<String> {
    usages_hub(cx).read_with(cx, |hub, cx| hub.group_labels(cx))
}

fn usages_counts(cx: &TestAppContext) -> (usize, usize) {
    usages_hub(cx).read_with(cx, |hub, cx| hub.shown_and_excluded(cx))
}

#[ui_kit::gpui::test]
fn usages_are_grouped_by_type_filtered_and_excluded(cx: &mut TestAppContext) {
    let root = project(
        "usages-grouping",
        &[
            ("src/Main.txt", "field\n"),
            ("src/Other.txt", "field\n"),
            ("src/Import.txt", "field\n"),
            ("src/Call.txt", "field\n"),
            ("src/Decl.txt", "field\n"),
        ],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let imported = root.join("src/Import.txt");
    let called = root.join("src/Call.txt");
    let declaration = root.join("src/Decl.txt");
    let language = GroupingLanguage::new();
    language.set_usages(vec![
        usage(&main, "field\n", 0, 5, UsageKind::Read),
        usage(&other, "field\n", 0, 5, UsageKind::Write),
        usage(&imported, "field\n", 0, 5, UsageKind::Import),
        usage(&called, "field\n", 0, 5, UsageKind::Call),
        usage(&declaration, "field\n", 0, 5, UsageKind::Declaration),
    ]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "alt-f7");
    wait_for(cx, "the usages", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap()
            && usages_counts(cx).0 == 5
    });
    draw(cx, window);

    click(cx, window, "item-button-usage-group-type");
    draw(cx, window);
    assert!(
        cx.read(|cx| cx.global::<Settings>().usage_group_usage_type),
        "the button changes the setting"
    );
    let labels = usages_labels(cx);
    for expected in ["Read", "Write", "Import", "Call", "Declaration"] {
        assert!(
            labels.iter().any(|label| label == expected),
            "the group {expected} is there: {labels:?}"
        );
    }

    click(cx, window, "item-button-usage-filter-read");
    draw(cx, window);
    assert_eq!(usages_counts(cx).0, 4, "Ctrl+R hides the reads");
    assert!(!usages_labels(cx).iter().any(|label| label == "Read"));
    click(cx, window, "item-button-usage-filter-read");
    draw(cx, window);
    assert_eq!(usages_counts(cx).0, 5, "Ctrl+R brings the reads back");

    click(cx, window, "item-row-0");
    press(cx, window, "delete");
    draw(cx, window);
    assert_eq!(usages_counts(cx).1, 1, "Delete excludes the row");
    press(cx, window, "insert");
    draw(cx, window);
    assert_eq!(usages_counts(cx).1, 0, "Insert includes the row again");

    let title = usages_hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).title().to_owned());
    assert_eq!(title, "Usages (5)");
}

#[ui_kit::gpui::test]
fn todo_is_grouped_by_packages_and_modules(cx: &mut TestAppContext) {
    let root = project(
        "todo-grouping",
        &[
            ("src/com/example/a/A.txt", "// TODO one\n"),
            ("src/com/example/b/B.txt", "// TODO two\n"),
            ("src/org/other/C.txt", "// TODO three\n"),
        ],
    );
    let a = root.join("src/com/example/a/A.txt");
    let b = root.join("src/com/example/b/B.txt");
    let c = root.join("src/org/other/C.txt");
    let language = GroupingLanguage::new();
    language.set_package(&a, "com.example.a");
    language.set_package(&b, "com.example.b");
    language.set_package(&c, "org.other");
    language.set_module(&a, "app");
    language.set_module(&b, "app");
    language.set_module(&c, "lib");
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TODO_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    wait_for(cx, "the TODO scan", |cx| {
        cx.read(|cx| cx.global::<ActiveTodo>().0.read(cx).items().len() == 3)
    });

    press(cx, window, "ctrl-alt-p");
    draw(cx, window);
    let panel = cx.read(|cx| cx.global::<ActiveTodo>().0.clone());
    let labels: Vec<String> = panel.read_with(cx, |panel, _| {
        panel
            .rows()
            .iter()
            .filter_map(|row| match row {
                PanelRow::Group { label, .. } => Some(label.clone()),
                PanelRow::Item { .. } => None,
            })
            .collect()
    });
    assert!(labels.iter().any(|label| label == "com"), "{labels:?}");
    assert!(labels.iter().any(|label| label == "org"), "{labels:?}");

    press(cx, window, "ctrl-alt-m");
    draw(cx, window);
    let labels: Vec<String> = panel.read_with(cx, |panel, _| {
        panel
            .rows()
            .iter()
            .filter_map(|row| match row {
                PanelRow::Group { label, .. } => Some(label.clone()),
                PanelRow::Item { .. } => None,
            })
            .collect()
    });
    assert!(labels.iter().any(|label| label == "app"), "{labels:?}");
    assert!(labels.iter().any(|label| label == "lib"), "{labels:?}");
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(TODO_TOOL))
            .unwrap()
    );
}

#[ui_kit::gpui::test]
fn find_in_files_masks_history_and_selection(cx: &mut TestAppContext) {
    let root = project(
        "find-grouping",
        &[
            ("src/Main.txt", "needle one\nother\n"),
            ("src/MainTest.txt", "needle two\n"),
            ("src/Third.txt", "needle three\n"),
        ],
    );
    let main = root.join("src/Main.txt");
    let window = open_language_window(cx, root.clone(), GroupingLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-shift-f");
    draw(cx, window);
    type_text(cx, window, "needle");
    wait_for(cx, "the first search", |cx| {
        finder(cx).read_with(cx, |finder, _| finder.export_text().contains("Main.txt"))
    });

    press(cx, window, "ctrl-alt-f");
    draw(cx, window);
    assert!(
        finder(cx).read_with(cx, |finder, _| finder.mask_open()),
        "Ctrl+Alt+F opens the mask"
    );
    type_text(cx, window, "!*Test.txt");
    press(cx, window, "enter");
    wait_for(cx, "the masked search", |cx| {
        let text = finder(cx).read_with(cx, |finder, _| finder.export_text());
        text.contains("Main.txt") && !text.contains("MainTest.txt")
    });

    // A search that opens a place goes to the history of Alt+Down.
    press(cx, window, "enter");
    press(cx, window, "ctrl-shift-f");
    draw(cx, window);
    assert!(
        finder(cx).read_with(cx, |finder, _| finder.history_len()) > 0,
        "the executed search is in the history"
    );
    press(cx, window, "alt-down");
    draw(cx, window);
    assert!(
        finder(cx).read_with(cx, |finder, _| finder.history_open()),
        "Alt+Down opens the history"
    );
    click(cx, window, "find-history-0");
    draw(cx, window);
    let text = finder(cx).read_with(cx, |finder, _| finder.find_field().text.clone());
    assert_eq!(text, "needle", "the history brings the query back");

    press(cx, window, "ctrl-alt-e");
    draw(cx, window);
    assert!(
        finder(cx).read_with(cx, |finder, _| finder.in_selection()),
        "Ctrl+Alt+E searches only the selection"
    );
}

fn finder(cx: &TestAppContext) -> gpui::Entity<crate::finder::Finder> {
    cx.read(|cx| {
        cx.try_global::<ActiveFinder>()
            .and_then(|finder| finder.0.clone())
            .expect("the Find in Files window is open")
    })
}

#[ui_kit::gpui::test]
fn find_usages_settings_apply_to_the_next_alt_f7(cx: &mut TestAppContext) {
    let root = project(
        "find-usages-settings",
        &[("src/Main.txt", "field\n"), ("src/Other.txt", "field\n")],
    );
    let main = root.join("src/Main.txt");
    let other = root.join("src/Other.txt");
    let language = GroupingLanguage::new();
    language.set_usages(vec![
        usage(&main, "field\n", 0, 5, UsageKind::Read),
        usage(&other, "field\n", 0, 5, UsageKind::Write),
    ]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "ctrl-alt-shift-f7");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "find-usages-settings").size.height > px(0.),
        "Ctrl+Alt+Shift+F7 opens the dialog"
    );
    click(cx, window, "find-usages-scope-3");
    click(cx, window, "find-usages-find");
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_for(cx, "the narrowed usages", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap()
            && usages_counts(cx).0 == 1
    });
    assert_eq!(
        usages_hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).items()[0].path.clone()),
        main,
        "Current File keeps only the places of the open file"
    );
}

#[ui_kit::gpui::test]
fn export_writes_the_tree_of_the_usages_window(cx: &mut TestAppContext) {
    let root = project("export-usages", &[("src/Main.txt", "field\n")]);
    let main = root.join("src/Main.txt");
    let language = GroupingLanguage::new();
    language.set_usages(vec![usage(&main, "field\n", 0, 5, UsageKind::Read)]);
    let window = open_language_window(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "alt-f7");
    wait_for(cx, "the usages", |cx| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap()
            && usages_counts(cx).0 == 1
    });
    click(cx, window, "item-row-0");
    draw(cx, window);
    let target = root.join("usages.txt");
    press(cx, window, "alt-o");
    cx.simulate_new_path_selection({
        let target = target.clone();
        move |_| Some(target.clone())
    });
    wait_for(cx, "the exported file", |_| target.is_file());
    let text = std::fs::read_to_string(&target).unwrap();
    assert!(text.contains("field"), "{text:?}");
    assert!(text.contains("Main.txt"), "{text:?}");
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn a_field_of_petclinic_is_grouped_by_read_and_write_on_the_server(cx: &mut TestAppContext) {
    use std::time::Duration;

    use crate::window_tests::buffer_of;
    use crate::window_tests_real::{
        build_window, close, project as real_project, wait_for as wait_long, wait_ready,
    };

    let (root, language) = real_project("grouping-server");
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let owner = file_by_stem(&root, "Owner").expect("PetClinic has the owner source");
    open_path(cx, window, &owner);
    let view = crate::window_tests::editor_of(cx, window, &owner);
    let text = buffer_of(cx, &view);
    let line = text
        .lines()
        .position(|line| line.contains("private String address;"))
        .expect("Owner has the address field");
    let column = text
        .lines()
        .nth(line)
        .and_then(|line| line.find("address"))
        .expect("the field has a name");
    view.update(cx, |view, cx| view.restore_caret(line, column, cx));
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_long(
        cx,
        "the usages of the field",
        Duration::from_secs(120),
        |cx| {
            window
                .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
                .unwrap()
                && usages_counts(cx).0 > 0
        },
    );
    draw(cx, window);
    click(cx, window, "item-button-usage-group-type");
    draw(cx, window);
    let labels = usages_labels(cx);
    assert!(labels.iter().any(|label| label == "Read"), "{labels:?}");
    assert!(labels.iter().any(|label| label == "Write"), "{labels:?}");

    close(cx, window, language, true);
}

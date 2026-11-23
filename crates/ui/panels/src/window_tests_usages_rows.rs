//! T-0400: the rows of the Usages window show the line of the file and its number with
//! the match in the search color, the results group by file, and a late answer of a
//! search does not take the bottom area from a window opened after it. The call hierarchy
//! opens with its root expanded.
//!
//! `cargo test -p panels window_tests_usages_rows`

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    Answer, CallHierarchyItem, CallHierarchyProvider, CallHierarchyQuery, Confidence, DocumentSpot,
    FileSymbol, FileSymbolsProvider, FileSymbolsQuery, LanguageFn, LanguageId, LanguageSupport,
    Provider, Queries, Reply, SourceId, SourceSpan, SyntaxProvider, UsageEntry, UsageKind,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use ui_kit::px;
use workspace::{FileIcons, Launch, Layout, PROBLEMS_TOOL, Settings, USAGES_TOOL, Workspace};

use crate::call_hierarchy::ActiveCallHierarchy;
use crate::index::SearchSetup;
use crate::items::PanelRow;
use crate::usages::ActiveUsages;
use crate::window_tests::{bounds_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_lang::{project, span_in, uri_of};
use crate::window_tests_ui::wait_for;
use crate::window_tests_usages_tabs::{UsageLanguage, usage};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("usages-rows-server");
const CALLS_SOURCE: SourceId = SourceId::new("usages-rows-calls");

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

/// A language whose usages answer waits until the test sends it.
struct PendingUsages {
    syntax: NoSyntax,
    replies: Mutex<Vec<Reply<Vec<UsageEntry>>>>,
}

impl PendingUsages {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            replies: Mutex::new(Vec::new()),
        })
    }

    fn answer(&self, entries: Vec<UsageEntry>) {
        let reply = self.replies.lock().unwrap().pop();
        if let Some(mut reply) = reply {
            let _ = reply.send(Answer::new(entries, Confidence::Certain, SOURCE));
        }
    }

    fn pending(&self) -> bool {
        !self.replies.lock().unwrap().is_empty()
    }
}

impl Provider<DocumentSpot, Vec<UsageEntry>> for PendingUsages {
    fn start(&self, _: DocumentSpot, _: base::Cancel, reply: Reply<Vec<UsageEntry>>) {
        self.replies.lock().unwrap().push(reply);
    }
}

impl LanguageSupport for PendingUsages {
    fn id(&self) -> LanguageId {
        LanguageId("usages-rows-pending")
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

    fn file_symbols(&self) -> Option<&FileSymbolsProvider> {
        Some(self)
    }
}

impl Provider<FileSymbolsQuery, Vec<FileSymbol>> for PendingUsages {
    fn start(&self, _: FileSymbolsQuery, _: base::Cancel, mut reply: Reply<Vec<FileSymbol>>) {
        let _ = reply.send(Answer::new(Vec::new(), Confidence::Certain, SOURCE));
    }
}

/// A call hierarchy whose calls answer waits until the test sends it.
struct PendingCalls {
    syntax: NoSyntax,
    roots: Vec<CallHierarchyItem>,
    calls: Mutex<Option<Reply<Vec<CallHierarchyItem>>>>,
}

impl PendingCalls {
    fn new(roots: Vec<CallHierarchyItem>) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            roots,
            calls: Mutex::new(None),
        })
    }

    fn answer(&self, items: Vec<CallHierarchyItem>) {
        let reply = self.calls.lock().unwrap().take();
        if let Some(mut reply) = reply {
            let _ = reply.send(Answer::new(items, Confidence::Certain, CALLS_SOURCE));
        }
    }

    fn pending(&self) -> bool {
        self.calls.lock().unwrap().is_some()
    }
}

impl Provider<CallHierarchyQuery, Vec<CallHierarchyItem>> for PendingCalls {
    fn start(
        &self,
        query: CallHierarchyQuery,
        _: base::Cancel,
        mut reply: Reply<Vec<CallHierarchyItem>>,
    ) {
        match query {
            CallHierarchyQuery::Roots(_) => {
                let _ = reply.send(Answer::new(
                    self.roots.clone(),
                    Confidence::Certain,
                    CALLS_SOURCE,
                ));
            }
            CallHierarchyQuery::Calls { .. } => {
                *self.calls.lock().unwrap() = Some(reply);
            }
        }
    }
}

impl LanguageSupport for PendingCalls {
    fn id(&self) -> LanguageId {
        LanguageId("usages-rows-calls")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn call_hierarchy(&self) -> Option<&CallHierarchyProvider> {
        Some(self)
    }
}

/// A window with the language of the test.
fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<dyn LanguageSupport>,
    name: &str,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch(name);
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
        support: Some(language),
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

fn usages_open(cx: &TestAppContext, window: WindowHandle<Workspace>) -> bool {
    window
        .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
        .unwrap()
}

fn usages_count(cx: &TestAppContext) -> usize {
    cx.read(|cx| {
        cx.try_global::<ActiveUsages>()
            .map(|active| active.0.read(cx).panel().read(cx).items().len())
            .unwrap_or(0)
    })
}

fn code_lines(cx: &TestAppContext) -> Vec<crate::items::CodeLine> {
    cx.read(|cx| {
        let Some(active) = cx.try_global::<ActiveUsages>() else {
            return Vec::new();
        };
        let panel = active.0.read(cx).panel().clone();
        let panel = panel.read(cx);
        (0..panel.items().len())
            .filter_map(|index| panel.code_line(index).cloned())
            .collect()
    })
}

fn call_panel(
    cx: &TestAppContext,
) -> Option<gpui::Entity<crate::call_hierarchy::CallHierarchyPanel>> {
    cx.read(|cx| {
        cx.try_global::<ActiveCallHierarchy>()
            .map(|active| active.0.clone())
    })
}

#[ui_kit::gpui::test]
fn a_row_shows_the_line_of_the_file_with_its_number_and_the_match(cx: &mut TestAppContext) {
    let line = "    Page<Owner> ownersResults = findPaginatedForOwnersLastName(page, lastName);\n";
    let text = format!("class OwnerController {{\n{line}}}\n");
    let root = project("usages-rows-code", &[("src/Main.txt", &text)]);
    let main = root.join("src").join("Main.txt");
    let name = "findPaginatedForOwnersLastName";
    let start = text.find(name).expect("the call is in the file");
    let language = UsageLanguage::new();
    language.set_usages(vec![usage(
        &main,
        &text,
        start,
        start + name.len(),
        UsageKind::Call,
    )]);
    let window = open_window_with(cx, root.clone(), language, "usages-rows-code");
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_for(cx, "the usages of the call", |cx| {
        usages_open(cx, window) && usages_count(cx) == 1
    });
    wait_for(cx, "the line of the usage", |cx| !code_lines(cx).is_empty());
    draw(cx, window);

    let code = code_lines(cx).remove(0);
    let trimmed = line.trim();
    let found = trimmed.find(name).expect("the call is in the line");
    assert_eq!(code.number, 2, "the number of the line counts from one");
    assert_eq!(code.text, trimmed, "the indent of the line is dropped");
    assert_eq!(
        code.highlight,
        found..found + name.len(),
        "the match covers the call"
    );
    assert!(
        bounds_of(cx, window, "item-code-match-0").size.height > px(0.),
        "the match has a piece of its own, drawn in the search color"
    );
}

#[ui_kit::gpui::test]
fn the_line_of_an_open_buffer_wins_over_the_file(cx: &mut TestAppContext) {
    let text = "greet old\n";
    let root = project("usages-rows-buffer", &[("src/Main.txt", text)]);
    let main = root.join("src").join("Main.txt");
    let language = UsageLanguage::new();
    language.set_usages(vec![usage(&main, text, 0, 5, UsageKind::Read)]);
    let window = open_window_with(cx, root.clone(), language, "usages-rows-buffer");
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.replace_text(6..9, "new", cx));
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_for(cx, "the usages of the buffer", |cx| {
        usages_open(cx, window) && usages_count(cx) == 1
    });
    wait_for(cx, "the edited line", |cx| !code_lines(cx).is_empty());
    assert_eq!(
        code_lines(cx).remove(0).text,
        "greet new",
        "the unsaved buffer is what the row shows"
    );
}

#[ui_kit::gpui::test]
fn the_results_of_two_files_are_grouped_by_their_files(cx: &mut TestAppContext) {
    let root = project(
        "usages-rows-groups",
        &[("src/Main.txt", "alpha\n"), ("src/Other.txt", "alpha\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = UsageLanguage::new();
    language.set_usages(vec![
        usage(&other, "alpha\n", 0, 5, UsageKind::Read),
        usage(&main, "alpha\n", 0, 5, UsageKind::Read),
    ]);
    let window = open_window_with(cx, root.clone(), language, "usages-rows-groups");
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);

    press(cx, window, "alt-f7");
    wait_for(cx, "the usages of both files", |cx| {
        usages_open(cx, window) && usages_count(cx) == 2 && code_lines(cx).len() == 2
    });
    draw(cx, window);

    let rows = cx.read(|cx| {
        cx.global::<ActiveUsages>()
            .0
            .read(cx)
            .panel()
            .read(cx)
            .rows()
            .to_vec()
    });
    let groups: Vec<(String, Option<String>, usize)> = rows
        .iter()
        .filter_map(|row| match row {
            PanelRow::Group {
                label, hint, depth, ..
            } => Some((label.clone(), hint.clone(), *depth)),
            PanelRow::Item { .. } => None,
        })
        .collect();
    assert_eq!(
        groups,
        vec![
            ("Main.txt".to_owned(), Some("src (1)".to_owned()), 0),
            ("Other.txt".to_owned(), Some("src (1)".to_owned()), 0),
        ],
        "every file is a group with its count and the package in grey"
    );
    assert!(
        rows.iter()
            .any(|row| matches!(row, PanelRow::Item { depth: 1, .. })),
        "the rows of a file stand under its header: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn a_late_answer_does_not_take_the_area_from_a_window_opened_after_it(cx: &mut TestAppContext) {
    let root = project(
        "usages-rows-late",
        &[("src/Main.txt", "alpha\n"), ("src/Other.txt", "alpha\n")],
    );
    let main = root.join("src").join("Main.txt");
    let other = root.join("src").join("Other.txt");
    let language = PendingUsages::new();
    let window = open_window_with(cx, root.clone(), language.clone(), "usages-rows-late");
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    draw(cx, window);

    press(cx, window, "alt-f7");
    wait_for(cx, "the search that waits", |cx| {
        usages_open(cx, window) && language.pending()
    });

    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(PROBLEMS_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(PROBLEMS_TOOL))
            .unwrap(),
        "the other window took the bottom area"
    );

    language.answer(vec![usage(&other, "alpha\n", 0, 5, UsageKind::Read)]);
    wait_for(cx, "the late answer", |cx| usages_count(cx) == 1);
    draw(cx, window);

    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(PROBLEMS_TOOL))
            .unwrap(),
        "the late answer leaves the open window in front"
    );
    assert!(
        !usages_open(cx, window),
        "the late answer does not replace the window opened after the search"
    );
}

#[ui_kit::gpui::test]
fn the_root_of_the_call_hierarchy_is_open_and_shows_the_caller(cx: &mut TestAppContext) {
    let root_text = "callee\n";
    let root = project("usages-rows-calls", &[("src/Calls.txt", root_text)]);
    let root_path = root.join("src").join("Calls.txt");
    let callee = CallHierarchyItem {
        name: "callee".to_owned(),
        detail: Some("() : void".to_owned()),
        place: Some(SourceSpan {
            uri: uri_of(&root_path),
            span: span_in(root_text, 0, 6),
        }),
        handle: "callee".to_owned(),
    };
    let caller = CallHierarchyItem {
        name: "caller".to_owned(),
        detail: None,
        place: Some(SourceSpan {
            uri: uri_of(&root_path),
            span: span_in(root_text, 0, 6),
        }),
        handle: "caller".to_owned(),
    };
    let language = PendingCalls::new(vec![callee]);
    let window = open_window_with(cx, root.clone(), language.clone(), "usages-rows-calls");
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &root_path);
    draw(cx, window);

    press(cx, window, "ctrl-alt-h");
    wait_for(cx, "the root of the call hierarchy", |cx| {
        call_panel(cx).is_some_and(|panel| {
            panel.read_with(cx, |panel, _| panel.lines().len() == 1) && language.pending()
        })
    });
    let panel = call_panel(cx).expect("the call hierarchy is open");
    assert!(
        panel.read_with(cx, |panel, _| panel.root_expanded()),
        "the root is open at once, before its callers come"
    );

    language.answer(vec![caller]);
    wait_for(cx, "the caller under the root", |cx| {
        panel.read_with(cx, |panel, _| {
            let lines = panel.lines();
            lines.iter().any(|line| line == "  caller")
        })
    });
    let lines = panel.read_with(cx, |panel, _| panel.lines());
    assert!(
        lines.first().is_some_and(|line| line.starts_with("callee")),
        "{lines:?}"
    );
    assert!(
        lines.iter().any(|line| line == "  caller"),
        "the caller stands one level under the root: {lines:?}"
    );
}

//! Postfix templates of the language in the window: Tab after `.for`, and the same
//! template picked from the completion list.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::{EditorView, Languages, Spot};
use lang_api::{LanguageId, LanguageSupport, SyntaxProvider};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, buffer_of, draw, editor_of, open_path, press, project_setup, scratch, type_text,
};
use crate::window_tests_complete::CompleteLanguage;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: &str = "class Probe {\n    void run(List<String> list) {\n        list.\n    }\n}\n";

/// The test language owns the source files and supplies their syntax, and has no
/// completion of its own, so the postfix entries of the editor are the list.
struct SourceCompletion(Arc<CompleteLanguage>);

impl LanguageSupport for SourceCompletion {
    fn id(&self) -> LanguageId {
        LanguageId("source")
    }

    fn matches(&self, path: &Path) -> bool {
        window_language::language_for(path).is_some()
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self.0.syntax()
    }
}

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<CompleteLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("postfix-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: window_language::language_for,
            templates: window_language::templates_for,
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
        support: Some(Arc::new(SourceCompletion(language)) as Arc<dyn LanguageSupport>),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("postfix-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn project(name: &str) -> PathBuf {
    scratch(name)
}

/// Opens the source at the end of `list.` and returns its editor.
fn open_probe(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
) -> (PathBuf, gpui::Entity<EditorView>) {
    let path = window_language::write_test_source(root, "Probe", SOURCE);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| {
        view.go_to(
            Spot::Line {
                line: 2,
                column: "        list.".len(),
            },
            cx,
        )
    });
    cx.run_until_parked();
    (path, view)
}

fn selected(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> String {
    let (range, _) = view
        .read_with(cx, |view, _| view.selection())
        .expect("the editor has a selection");
    buffer_of(cx, view)[range].to_owned()
}

#[ui_kit::gpui::test]
fn tab_expands_the_for_postfix_of_a_list(cx: &mut TestAppContext) {
    let root = project("postfix-tab");
    let window = open_window(cx, root.clone(), CompleteLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_probe(cx, window, &root);

    type_text(cx, window, "for");
    press(cx, window, "tab");

    let text = buffer_of(cx, &view);
    assert!(
        text.contains("for (var item : list) {\n            \n        }"),
        "the template expands: {text:?}"
    );
    assert_eq!(
        selected(cx, &view),
        "item",
        "the loop starts on its variable"
    );
    type_text(cx, window, "element");
    assert!(
        buffer_of(cx, &view).contains("for (var element : list) {"),
        "the field takes the typed name: {:?}",
        buffer_of(cx, &view)
    );
}

#[ui_kit::gpui::test]
fn the_completion_list_offers_the_for_postfix_and_puts_it_in(cx: &mut TestAppContext) {
    let root = project("postfix-list");
    let window = open_window(cx, root.clone(), CompleteLanguage::new());
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_probe(cx, window, &root);

    type_text(cx, window, "for");
    press(cx, window, "ctrl-space");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "completion-popup").size.height > ui_kit::px(0.),
        "the completion list shows the postfix template"
    );
    press(cx, window, "enter");

    let text = buffer_of(cx, &view);
    assert!(
        text.contains("for (var item : list) {\n            \n        }"),
        "the picked template expands: {text:?}"
    );
    assert_eq!(selected(cx, &view), "item");
}

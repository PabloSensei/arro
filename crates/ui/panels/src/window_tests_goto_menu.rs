//! Go to Test with a heading and the folder of every test, and the note of Ctrl+U on a
//! method that overrides nothing. The language is a stub that answers no super method.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use editor_view::Spot;
use lang_api::{
    Answer, Confidence, LanguageFn, LanguageId, LanguageSupport, NavigationKind,
    NavigationProvider, NavigationQuery, Provider, Queries, Reply, SourceId, SyntaxProvider,
    TestFolder, TestForm, TestFormProvider, TestFormQuery, TestFramework,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    draw, editor_of, open_path, press, project_setup, scratch, wait_for_file_index,
};
use crate::window_tests_lang::{FakeLanguage, project};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("goto-menu-language");

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

/// The stub of the two tests: the files come from [`FakeLanguage`], the form of Create
/// Test is fixed, and Ctrl+U gets no place at all.
struct MenuLanguage {
    fake: Arc<FakeLanguage>,
    root: PathBuf,
    syntax: NoSyntax,
}

impl MenuLanguage {
    fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            fake: FakeLanguage::new(),
            root,
            syntax: NoSyntax(Queries::default()),
        })
    }
}

impl Provider<NavigationQuery, Vec<lang_api::SourceSpan>> for MenuLanguage {
    fn start(
        &self,
        query: NavigationQuery,
        cancel: base::Cancel,
        mut reply: Reply<Vec<lang_api::SourceSpan>>,
    ) {
        if query.kind == NavigationKind::SuperMethod {
            let _ = reply.send(Answer::new(Vec::new(), Confidence::Certain, SOURCE));
            return;
        }
        Provider::<NavigationQuery, Vec<lang_api::SourceSpan>>::start(
            self.fake.as_ref(),
            query,
            cancel,
            reply,
        );
    }
}

impl Provider<TestFormQuery, TestForm> for MenuLanguage {
    fn start(&self, _: TestFormQuery, _: base::Cancel, mut reply: Reply<TestForm>) {
        let form = TestForm {
            source_class: "Owner".to_owned(),
            frameworks: vec![TestFramework {
                handle: "unit".to_owned(),
                name: "Unit".to_owned(),
                available: true,
                note: None,
            }],
            framework: 0,
            class_name: "OwnerTest".to_owned(),
            superclass: String::new(),
            package: "com.shop".to_owned(),
            folders: vec![TestFolder {
                path: self.root.join("src").join("test"),
                name: "src/test".to_owned(),
            }],
            folder: 0,
            setup: false,
            teardown: false,
            methods: Vec::new(),
            can_fix: false,
            error: None,
        };
        let _ = reply.send(Answer::new(form, Confidence::Certain, SOURCE));
    }
}

impl LanguageSupport for MenuLanguage {
    fn id(&self) -> LanguageId {
        self.fake.id()
    }

    fn matches(&self, path: &Path) -> bool {
        self.fake.matches(path)
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn lang_api::SemanticServer> {
        self.fake.server()
    }

    fn navigation(&self) -> Option<&NavigationProvider> {
        Some(self)
    }

    fn test_form(&self) -> Option<&TestFormProvider> {
        Some(self)
    }
}

fn open_language_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<MenuLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("goto-menu-state");
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
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
    let setup = project_setup(scratch("goto-menu-cache"));
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

/// Go to Test names the class and the count in its heading, every row is the name of the
/// test and its folder with `/`, and Create New Test comes last after a separator.
#[ui_kit::gpui::test]
fn goto_test_menu_has_a_heading_names_and_folders(cx: &mut TestAppContext) {
    let root = project(
        "goto-menu",
        &[
            ("src/main/com/shop/Owner.txt", "class Owner {\n}\n"),
            ("src/test/com/shop/OwnerTest.txt", "class OwnerTest {\n}\n"),
            (
                "src/test/com/shop/OwnerTests.txt",
                "class OwnerTests {\n}\n",
            ),
        ],
    );
    let owner = root.join("src").join("main").join("com/shop/Owner.txt");
    let window = open_language_window(cx, root.clone(), MenuLanguage::new(root.clone()));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &owner);
    wait_for_file_index(cx);

    press(cx, window, "ctrl-shift-t");
    draw(cx, window);

    let (title, rows) = window
        .read_with(cx, |workspace, _| {
            let title = workspace.menu_title().map(str::to_owned);
            let rows = workspace
                .menu_entries()
                .unwrap_or_default()
                .iter()
                .map(|entry| {
                    (
                        entry.label.to_string(),
                        entry.detail.as_ref().map(ToString::to_string),
                        entry.icon,
                    )
                })
                .collect::<Vec<_>>();
            (title, rows)
        })
        .unwrap();
    assert_eq!(
        title.as_deref(),
        Some("Choose Test for Owner (2 found)"),
        "the heading names the class and the count"
    );
    assert_eq!(rows.len(), 4, "two tests, a separator and Create New Test");
    assert_eq!(rows[0].0, "OwnerTest");
    assert_eq!(rows[0].1.as_deref(), Some("src/test/com/shop"));
    assert_eq!(
        rows[0].2,
        Some("fileTypes/text"),
        "the icon is the one of the project tree"
    );
    assert_eq!(rows[1].0, "OwnerTests");
    assert_eq!(rows[1].1.as_deref(), Some("src/test/com/shop"));
    assert!(
        rows[2].0.is_empty(),
        "a separator stands before Create New Test"
    );
    assert_eq!(rows[3].0, "Create New Test...");
    assert_eq!(rows[3].1, None);

    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("menu-title").is_some(),
        "the heading is drawn"
    );
    assert!(
        visual.debug_bounds("menu-detail-0").is_some(),
        "the folder is drawn beside the name"
    );
}

/// Ctrl+U on a method that overrides nothing says there is no super method.
#[ui_kit::gpui::test]
fn ctrl_u_without_a_super_method_says_so(cx: &mut TestAppContext) {
    let root = project("goto-super", &[("src/Main.txt", "void find() {\n}\n")]);
    let main = root.join("src").join("Main.txt");
    let window = open_language_window(cx, root.clone(), MenuLanguage::new(root.clone()));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &main);
    let view = editor_of(cx, window, &main);
    view.update(cx, |view, cx| view.go_to(Spot::Offset(5), cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-u");
    draw(cx, window);
    assert_eq!(
        view.read_with(cx, |view, _| view.note_text()).as_deref(),
        Some("No super method found")
    );
}

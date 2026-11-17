//! Create Test in the window: Go to Test without a test offers the new row, Alt+Enter on
//! the name of a class opens the form, and the form writes the file the language makes.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use lang_api::{
    Answer, Confidence, DocumentEdit, LanguageFn, LanguageId, LanguageSupport, Provider, Queries,
    Reply, SourceEdit, SourceId, SyntaxProvider, TestChoice, TestCreateOutcome, TestCreateProvider,
    TestCreateRequest, TestFile, TestFixOutcome, TestFixProvider, TestFixRequest, TestFolder,
    TestForm, TestFormProvider, TestFormQuery, TestFramework, TextPoint, TextSpan, WorkspaceChange,
};
use platform::CanonPath;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::run::ProjectSetup;
use crate::window_tests::{
    active_path, buffer_of, click, draw, editor_of, open_path, press, project_setup, scratch,
};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("create-test-language");
const LIMIT: Duration = Duration::from_secs(20);

fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(
            started.elapsed() < LIMIT,
            "the test waited for {what} in vain"
        );
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
    }
}

fn visible(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &'static str,
) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

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

/// The language of the window tests: the form and the files come from the test, no
/// process runs.
struct TestLanguage {
    syntax: NoSyntax,
    root: PathBuf,
    form: Mutex<TestForm>,
}

impl TestLanguage {
    fn new(root: PathBuf, form: TestForm) -> std::sync::Arc<Self> {
        std::sync::Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            root,
            form: Mutex::new(form),
        })
    }

    fn answer<T>(value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }

    fn pom(&self) -> PathBuf {
        self.root.join("pom.xml")
    }
}

impl Provider<TestFormQuery, TestForm> for TestLanguage {
    fn start(&self, _: TestFormQuery, _: base::Cancel, mut reply: Reply<TestForm>) {
        let form = self.form.lock().unwrap().clone();
        let _ = reply.send(Self::answer(form));
    }
}

impl Provider<TestCreateRequest, TestCreateOutcome> for TestLanguage {
    fn start(
        &self,
        query: TestCreateRequest,
        _: base::Cancel,
        mut reply: Reply<TestCreateOutcome>,
    ) {
        let mut path = query.folder.clone();
        for part in query.package.split('.').filter(|part| !part.is_empty()) {
            path.push(part);
        }
        path.push(format!("{}.txt", query.class_name));
        let file = if path.exists() {
            TestFile {
                path,
                text: None,
                caret: None,
            }
        } else {
            let mut text = String::new();
            let mut line = 0;
            if !query.package.is_empty() {
                text.push_str(&format!("package {};\n\n", query.package));
                line += 2;
            }
            text.push_str(&format!("class {} {{\n", query.class_name));
            line += 1;
            for method in &query.methods {
                text.push_str(&format!("    @Test\n    void {method}() {{\n    }}\n\n"));
            }
            text.push_str("}\n");
            TestFile {
                path,
                text: Some(text),
                caret: Some(TextPoint {
                    line: line + 1,
                    character: 4,
                }),
            }
        };
        let _ = reply.send(Self::answer(TestCreateOutcome {
            file: Some(file),
            error: None,
        }));
    }
}

impl Provider<TestFixRequest, TestFixOutcome> for TestLanguage {
    fn start(&self, _: TestFixRequest, _: base::Cancel, mut reply: Reply<TestFixOutcome>) {
        let path = self.pom();
        let text = fs::read_to_string(&path).unwrap_or_default();
        let point = TextPoint {
            line: text.bytes().filter(|byte| *byte == b'\n').count() as u32,
            character: 0,
        };
        let edit = DocumentEdit {
            span: TextSpan {
                start: point,
                end: point,
            },
            text: "<!-- testing-library -->\n".to_owned(),
        };
        let _ = reply.send(Self::answer(TestFixOutcome {
            change: WorkspaceChange {
                edits: vec![SourceEdit {
                    uri: uri_of(&path),
                    edits: vec![edit],
                }],
                renames: Vec::new(),
            },
            error: None,
        }));
    }
}

impl LanguageSupport for TestLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("create-test")
    }

    fn matches(&self, path: &Path) -> bool {
        matches!(
            path.extension().and_then(|extension| extension.to_str()),
            Some("txt")
        )
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn test_form(&self) -> Option<&TestFormProvider> {
        Some(self)
    }

    fn create_test(&self) -> Option<&TestCreateProvider> {
        Some(self)
    }

    fn test_framework_fix(&self) -> Option<&TestFixProvider> {
        Some(self)
    }

    fn can_create_test(&self, text: &str, offset: usize) -> bool {
        let Some(start) = text.find("class ").map(|at| at + "class ".len()) else {
            return false;
        };
        let end = text[start..]
            .find(|character: char| !character.is_alphanumeric())
            .map_or(text.len(), |length| start + length);
        start <= offset && offset <= end
    }
}

fn uri_of(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
}

fn open_language_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: std::sync::Arc<TestLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("create-test-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(std::sync::Arc::new);
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
    let setup: ProjectSetup = project_setup(scratch("create-test-cache"));
    let agents = AgentOptions {
        state_dir: launch.state_dir.clone(),
        mock: false,
    };
    let (status_sender, status) = futures_channel::mpsc::unbounded();
    status_sender
        .unbounded_send("Language server ready".to_string())
        .ok();
    let server = LanguageServer {
        support: Some(language as std::sync::Arc<dyn LanguageSupport>),
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

fn framework(name: &str, available: bool) -> TestFramework {
    TestFramework {
        handle: name.to_lowercase().replace(' ', "-"),
        name: name.to_owned(),
        available,
        note: (!available).then(|| format!("{name} library not found in the module")),
    }
}

fn form(root: &Path, available: bool, can_fix: bool, methods: &[(&str, bool)]) -> TestForm {
    TestForm {
        source_class: "Owner".to_owned(),
        frameworks: vec![
            framework("Unit 5", available),
            framework("Unit 4", false),
            framework("Ng", false),
        ],
        framework: 0,
        class_name: "OwnerTest".to_owned(),
        superclass: String::new(),
        package: "com.shop".to_owned(),
        folders: vec![TestFolder {
            path: root.join("src").join("test"),
            name: "src/test".to_owned(),
        }],
        folder: 0,
        setup: false,
        teardown: false,
        methods: methods
            .iter()
            .map(|(name, checked)| TestChoice {
                name: (*name).to_owned(),
                detail: Some(format!("{name}()")),
                checked: *checked,
                inherited: false,
                handle: (*name).to_owned(),
            })
            .collect(),
        can_fix,
        error: None,
    }
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
    fs::write(root.join("pom.xml"), "<project>\n</project>\n").unwrap();
    root
}

/// The window of one test: the root, the class file and the window with its language.
struct Fixture {
    root: PathBuf,
    class: PathBuf,
    window: WindowHandle<Workspace>,
}

fn fixture(
    cx: &mut TestAppContext,
    name: &str,
    available: bool,
    can_fix: bool,
    methods: &[(&str, bool)],
) -> Fixture {
    let root = project(
        name,
        &[(
            "src/main/com/shop/Owner.txt",
            "class Owner {\n    void find() {\n    }\n}\n",
        )],
    );
    let language = TestLanguage::new(root.clone(), form(&root, available, can_fix, methods));
    let window = open_language_window(cx, root.clone(), language);
    let class = root.join("src").join("main").join("com/shop/Owner.txt");
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &class);
    Fixture {
        root,
        class,
        window,
    }
}

/// Go to Test with nothing to go to: the one row opens the form, and OK writes the file.
#[ui_kit::gpui::test]
fn goto_test_without_a_test_opens_create_test_and_writes_the_file(cx: &mut TestAppContext) {
    let fixture = fixture(cx, "create-test-goto", true, true, &[("find", true)]);
    let window = fixture.window;

    press(cx, window, "ctrl-shift-t");
    wait_for(cx, "the menu of Go to Test", |cx| {
        visible(cx, window, "menu-0")
    });
    press(cx, window, "enter");
    wait_for(cx, "the Create Test form", |cx| {
        visible(cx, window, "create-test")
    });
    assert!(visible(cx, window, "create-test-class-name"));

    press(cx, window, "enter");
    let test = fixture.root.join("src/test/com/shop/OwnerTest.txt");
    wait_for(cx, "the new test file", |cx| {
        active_path(cx, window).as_deref() == Some(test.as_path())
    });
    assert_eq!(active_path(cx, window), Some(test.clone()));
    let text = fs::read_to_string(&test).unwrap();
    assert!(text.contains("package com.shop;"));
    assert!(text.contains("void find() {"));
    let view = editor_of(cx, window, &test);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (4, 4),
        "the caret is in the first test method"
    );
}

/// Alt+Enter on the name of the class offers Create Test and opens the form.
#[ui_kit::gpui::test]
fn alt_enter_on_the_class_name_opens_the_create_test_form(cx: &mut TestAppContext) {
    let fixture = fixture(cx, "create-test-alt-enter", true, true, &[("find", true)]);
    let window = fixture.window;
    let view = editor_of(cx, window, &fixture.class);
    view.update(cx, |view, cx| view.restore_caret(0, 7, cx));
    cx.run_until_parked();
    draw(cx, window);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (0, 7));

    press(cx, window, "alt-enter");
    draw(cx, window);
    wait_for(cx, "the action list", |cx| {
        visible(cx, window, "actions-popup")
    });
    press(cx, window, "enter");
    wait_for(cx, "the Create Test form", |cx| {
        visible(cx, window, "create-test")
    });
}

/// A method that is not marked gets no test method, and the file follows the form.
#[ui_kit::gpui::test]
fn the_marked_methods_decide_the_new_test_methods(cx: &mut TestAppContext) {
    let fixture = fixture(
        cx,
        "create-test-methods",
        true,
        true,
        &[("find", true), ("save", true)],
    );
    let window = fixture.window;
    press(cx, window, "ctrl-shift-t");
    wait_for(cx, "the menu of Go to Test", |cx| {
        visible(cx, window, "menu-0")
    });
    press(cx, window, "enter");
    wait_for(cx, "the Create Test form", |cx| {
        visible(cx, window, "create-test-method-0")
    });
    click(cx, window, "create-test-method-0");
    press(cx, window, "enter");
    let test = fixture.root.join("src/test/com/shop/OwnerTest.txt");
    wait_for(cx, "the new test file", |cx| {
        active_path(cx, window).as_deref() == Some(test.as_path())
    });
    let text = fs::read_to_string(&test).unwrap();
    assert!(
        !text.contains("void find()"),
        "the unchecked method has no test"
    );
    assert!(text.contains("void save()"));
}

/// A test class that is already there is not written over: the window asks to open it.
#[ui_kit::gpui::test]
fn an_existing_test_class_is_asked_about_and_opens(cx: &mut TestAppContext) {
    let fixture = fixture(cx, "create-test-existing", true, true, &[("find", true)]);
    let window = fixture.window;
    let test = fixture.root.join("src/test/com/shop/OwnerTest.txt");
    fs::create_dir_all(test.parent().unwrap()).unwrap();
    fs::write(&test, "class OwnerTest {\n    // already there\n}\n").unwrap();

    press(cx, window, "ctrl-shift-t");
    wait_for(cx, "the menu of Go to Test", |cx| {
        visible(cx, window, "menu-0")
    });
    press(cx, window, "enter");
    wait_for(cx, "the Create Test form", |cx| {
        visible(cx, window, "create-test")
    });
    press(cx, window, "enter");
    wait_for(cx, "the question about the file", |cx| {
        visible(cx, window, "prompt")
    });
    press(cx, window, "enter");
    wait_for(cx, "the existing test", |cx| {
        active_path(cx, window).as_deref() == Some(test.as_path())
    });
    let opened = editor_of(cx, window, &test);
    assert!(buffer_of(cx, &opened).contains("already there"));
}

/// A library the module does not have gets a warning, and Fix adds it to the build file.
#[ui_kit::gpui::test]
fn a_missing_library_warns_and_fix_adds_it(cx: &mut TestAppContext) {
    let fixture = fixture(cx, "create-test-fix", false, true, &[("find", true)]);
    let window = fixture.window;
    press(cx, window, "ctrl-shift-t");
    wait_for(cx, "the menu of Go to Test", |cx| {
        visible(cx, window, "menu-0")
    });
    press(cx, window, "enter");
    wait_for(cx, "the warning of the missing library", |cx| {
        visible(cx, window, "create-test-warning")
    });
    assert!(
        visible(cx, window, "create-test-fix"),
        "the window offers Fix"
    );
    click(cx, window, "create-test-fix");
    wait_for(cx, "the library in the build file", |cx| {
        visible(cx, window, "create-test")
            && !visible(cx, window, "create-test-warning")
            && fs::read_to_string(fixture.root.join("pom.xml"))
                .is_ok_and(|text| text.contains("testing-library"))
    });
}

/// A project without a way to add the library only shows the warning.
#[ui_kit::gpui::test]
fn a_missing_library_without_fix_only_warns(cx: &mut TestAppContext) {
    let fixture = fixture(cx, "create-test-no-fix", false, false, &[("find", true)]);
    let window = fixture.window;
    press(cx, window, "ctrl-shift-t");
    wait_for(cx, "the menu of Go to Test", |cx| {
        visible(cx, window, "menu-0")
    });
    press(cx, window, "enter");
    wait_for(cx, "the warning of the missing library", |cx| {
        visible(cx, window, "create-test-warning")
    });
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("create-test-fix").is_none(),
        "there is no Fix button without a build file to change"
    );
}

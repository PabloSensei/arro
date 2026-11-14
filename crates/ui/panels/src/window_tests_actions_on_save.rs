//! Actions on Save with a language that answers from memory: the window formats, organizes
//! imports and cleans up before the text reaches the disk, in one undo step, and a slow
//! server never keeps the save waiting longer than its timeout.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use editor_view::Languages;
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, Confidence, DocumentEdit,
    FormatProvider, FormatQuery, LanguageFn, LanguageId, LanguageSupport, Provider, Queries,
    RangeFormatProvider, RangeFormatQuery, SourceId, SyntaxProvider, TextSpan,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, ReformatScope, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    buffer_of, draw, editor_of, git_repository, open_path, press, project_setup, scratch,
    type_text, wait_for_signal,
};
use crate::window_tests_lang::{edit_in, project, uri_of};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("save-server");

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

/// A language with a formatter for the whole file and for a range, an action provider,
/// and a switch that keeps the formatter's answer for the timeout test.
struct SaveLanguage {
    syntax: NoSyntax,
    whole: Mutex<Option<String>>,
    range: Mutex<Option<String>>,
    actions: Mutex<Vec<ActionEntry>>,
    slow: AtomicBool,
    /// A formatter that does not answer keeps its end of the channel here, so the reply
    /// stream stays open until the save gives up.
    pending: Mutex<Option<lang_api::Reply<Vec<DocumentEdit>>>>,
}

impl SaveLanguage {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            whole: Mutex::new(None),
            range: Mutex::new(None),
            actions: Mutex::new(Vec::new()),
            slow: AtomicBool::new(false),
            pending: Mutex::new(None),
        })
    }

    fn format_whole(&self, text: &str) {
        *self.whole.lock().unwrap() = Some(text.to_owned());
    }

    fn format_range(&self, text: &str) {
        *self.range.lock().unwrap() = Some(text.to_owned());
    }

    fn set_actions(&self, entries: Vec<ActionEntry>) {
        *self.actions.lock().unwrap() = entries;
    }

    fn slow(&self) {
        self.slow.store(true, Ordering::SeqCst);
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, Confidence::Certain, SOURCE)
    }
}

impl Provider<FormatQuery, Vec<DocumentEdit>> for SaveLanguage {
    fn start(
        &self,
        _: FormatQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<DocumentEdit>>,
    ) {
        if self.slow.load(Ordering::SeqCst) {
            *self.pending.lock().unwrap() = Some(reply);
            return;
        }
        let text = self.whole.lock().unwrap().clone().unwrap_or_default();
        let edit = DocumentEdit {
            span: TextSpan::default(),
            text,
        };
        let _ = reply.send(self.answer(vec![edit]));
    }
}

impl Provider<RangeFormatQuery, Vec<DocumentEdit>> for SaveLanguage {
    fn start(
        &self,
        query: RangeFormatQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<DocumentEdit>>,
    ) {
        if self.slow.load(Ordering::SeqCst) {
            return;
        }
        let Some(text) = self.range.lock().unwrap().clone() else {
            let _ = reply.send(self.answer(Vec::new()));
            return;
        };
        let edit = DocumentEdit {
            span: TextSpan {
                start: query.range.start,
                end: query.range.start,
            },
            text,
        };
        let _ = reply.send(self.answer(vec![edit]));
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for SaveLanguage {
    fn start(
        &self,
        query: ActionQuery,
        _: base::Cancel,
        mut reply: lang_api::Reply<Vec<ActionEntry>>,
    ) {
        let entries = self
            .actions
            .lock()
            .unwrap()
            .iter()
            .filter(|entry| query.only.is_none() || entry.kind == query.only)
            .cloned()
            .collect();
        let _ = reply.send(self.answer(entries));
    }
}

impl LanguageSupport for SaveLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("save")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn format(&self) -> Option<&FormatProvider> {
        Some(self)
    }

    fn range_format(&self) -> Option<&RangeFormatProvider> {
        Some(self)
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }
}

fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<SaveLanguage>,
    settings: Settings,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(settings);
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
    let setup = project_setup(scratch("cache"));
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

fn reformat_settings(changed_lines: bool) -> Settings {
    Settings {
        actions_on_save_reformat: true,
        actions_on_save_reformat_scope: if changed_lines {
            ReformatScope::ChangedLines
        } else {
            ReformatScope::WholeFile
        },
        ..Settings::default()
    }
}

fn action_of(kind: ActionKind, uri: &str, edits: Vec<DocumentEdit>) -> ActionEntry {
    ActionEntry {
        title: match kind {
            ActionKind::OrganizeImports => "Organize Imports".to_owned(),
            ActionKind::FixAll => "Code Cleanup".to_owned(),
            _ => "Action".to_owned(),
        },
        kind: Some(kind),
        code: None,
        preferred: true,
        edits: vec![lang_api::SourceEdit {
            uri: uri.to_owned(),
            edits,
        }],
        renames: Vec::new(),
        handle: None,
    }
}

#[gpui::test]
fn reformat_on_save_formats_before_the_text_reaches_the_disk(cx: &mut TestAppContext) {
    let root = project("save-reformat", &[("src/Main.txt", "value\n")]);
    let path = root.join("src").join("Main.txt");
    let language = SaveLanguage::new();
    language.format_whole("// formatted\n");
    let window = open_window_with(cx, root, language, reformat_settings(false));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, "!");

    press(cx, window, "ctrl-s");
    wait_for_signal(cx, "the formatted save", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "// formatted\nvalue!\n")
    });
    assert_eq!(buffer_of(cx, &view), "// formatted\nvalue!\n");
}

#[gpui::test]
fn changed_lines_format_only_the_lines_of_the_last_commit(cx: &mut TestAppContext) {
    let root = git_repository("save-changed");
    let path = root.join("main.txt");
    fs::write(&path, "alpha\nbeta\nGAMMA\n").unwrap();
    let language = SaveLanguage::new();
    language.format_range("// range\n");
    let window = open_window_with(cx, root, language, reformat_settings(true));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    let git = cx.update(|cx| cx.global::<crate::git::ActiveGit>().0.clone());
    git.update(cx, |git, cx| git.refresh(cx));
    cx.run_until_parked();
    assert!(
        !view.read_with(cx, |view, _| view.gutter_marks().is_empty()),
        "the third line differs from the commit"
    );
    view.update(cx, |view, cx| view.restore_caret(2, 5, cx));
    type_text(cx, window, "!");

    press(cx, window, "ctrl-s");
    wait_for_signal(cx, "the changed-lines save", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "alpha\nbeta\n// range\nGAMMA!\n")
    });
    assert_eq!(buffer_of(cx, &view), "alpha\nbeta\n// range\nGAMMA!\n");
}

#[gpui::test]
fn optimize_imports_runs_on_save(cx: &mut TestAppContext) {
    let root = project(
        "save-imports",
        &[("src/Main.txt", "import old;\nclass Main\n")],
    );
    let path = root.join("src").join("Main.txt");
    let uri = uri_of(&path);
    let language = SaveLanguage::new();
    language.set_actions(vec![action_of(
        ActionKind::OrganizeImports,
        &uri,
        vec![edit_in("import old;\nclass Main\n", 0, 11, "import new;")],
    )]);
    let settings = Settings {
        actions_on_save_optimize_imports: true,
        ..Settings::default()
    };
    let window = open_window_with(cx, root, language, settings);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.restore_caret(1, 10, cx));
    type_text(cx, window, "!");

    press(cx, window, "ctrl-s");
    wait_for_signal(cx, "the organized imports", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "import new;\nclass Main!\n")
    });
    assert_eq!(buffer_of(cx, &view), "import new;\nclass Main!\n");
}

#[gpui::test]
fn one_undo_takes_back_every_action_of_the_save(cx: &mut TestAppContext) {
    let root = project("save-undo", &[("src/Main.txt", "import old;\nvalue")]);
    let path = root.join("src").join("Main.txt");
    let uri = uri_of(&path);
    let language = SaveLanguage::new();
    language.format_whole("// formatted\n");
    language.set_actions(vec![action_of(
        ActionKind::OrganizeImports,
        &uri,
        vec![edit_in("import old;\nvalue", 0, 11, "import new;")],
    )]);
    let mut settings = reformat_settings(false);
    settings.actions_on_save_optimize_imports = true;
    settings.trim_trailing_whitespace = workspace::TrimTrailingWhitespace::All;
    settings.ensure_final_newline = true;
    let window = open_window_with(cx, root, language, settings);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| view.restore_caret(1, 5, cx));
    type_text(cx, window, "!");
    press(cx, window, "ctrl-s");
    wait_for_signal(cx, "the save with every action", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "// formatted\nimport new;\nvalue!\n")
    });
    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        "import old;\nvalue!",
        "one undo takes back the format, the imports and the save rules"
    );
}

#[gpui::test]
fn a_slow_server_is_left_behind_after_the_timeout(cx: &mut TestAppContext) {
    let root = project("save-slow", &[("src/Main.txt", "value\n")]);
    let path = root.join("src").join("Main.txt");
    let language = SaveLanguage::new();
    language.format_whole("// formatted\n");
    language.slow();
    let window = open_window_with(cx, root, language, reformat_settings(false));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, "!");
    let started = std::time::Instant::now();
    press(cx, window, "ctrl-s");
    wait_for_signal(cx, "the save without the answer", |cx| {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(250));
        fs::read_to_string(&path).is_ok_and(|text| text == "value!\n")
    });
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "the save waited for the slow server"
    );
    assert_eq!(buffer_of(cx, &view), "value!\n", "the typed text is kept");
    assert!(
        window
            .read_with(cx, |workspace, _| {
                workspace.last_notification().is_some_and(|(title, text)| {
                    title.contains("Actions on Save") && text.contains("did not answer")
                })
            })
            .unwrap(),
        "the window says the save went on without the language"
    );
}

#[gpui::test]
fn trailing_whitespace_and_the_final_newline_are_save_rules(cx: &mut TestAppContext) {
    let root = project("save-whitespace", &[("src/Main.txt", "one \ntwo")]);
    let path = root.join("src").join("Main.txt");
    let settings = Settings {
        trim_trailing_whitespace: workspace::TrimTrailingWhitespace::All,
        ensure_final_newline: true,
        ..Settings::default()
    };
    let window = open_window_with(cx, root, SaveLanguage::new(), settings);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| view.restore_caret(1, 3, cx));
    type_text(cx, window, "!");
    press(cx, window, "ctrl-s");
    wait_for_signal(cx, "the trimmed save", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "one\ntwo!\n")
    });
    assert_eq!(buffer_of(cx, &view), "one\ntwo!\n");
}

#[gpui::test]
fn disabled_actions_leave_the_text_alone(cx: &mut TestAppContext) {
    let root = project("save-disabled", &[("src/Main.txt", "value\n")]);
    let path = root.join("src").join("Main.txt");
    let language = SaveLanguage::new();
    language.format_whole("// formatted\n");
    let window = open_window_with(cx, root, language, Settings::default());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);

    view.update(cx, |view, cx| view.restore_caret(0, 5, cx));
    type_text(cx, window, "!");
    press(cx, window, "ctrl-s");
    wait_for_signal(cx, "the plain save", |_| {
        fs::read_to_string(&path).is_ok_and(|text| text == "value!\n")
    });
    assert_eq!(buffer_of(cx, &view), "value!\n");
}

#[gpui::test]
fn a_read_only_file_is_not_formatted(cx: &mut TestAppContext) {
    let root = project("save-read-only", &[("src/Main.txt", "value\n")]);
    let path = root.join("src").join("Main.txt");
    let language = SaveLanguage::new();
    language.format_whole("// formatted\n");
    let mut permissions = fs::metadata(&path).unwrap().permissions();
    permissions.set_readonly(true);
    fs::set_permissions(&path, permissions).unwrap();
    let window = open_window_with(cx, root, language, reformat_settings(false));
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    assert!(
        view.read_with(cx, |view, _| view.is_read_only()),
        "the attribute of the file reaches the editor"
    );

    view.update(cx, |view, cx| view.save(cx));
    cx.run_until_parked();
    assert_eq!(
        fs::read_to_string(&path).unwrap(),
        "value\n",
        "a read-only file keeps its text"
    );
}

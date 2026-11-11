//! The Change Signature window without a window: a language answers from memory the
//! way a server does, and the form drives the edits of the open and closed files.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use editor_view::{Languages, uri_path};
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, ChangeSignatureInfo,
    ChangeSignatureInfoProvider, ChangeSignatureOutcome, ChangeSignatureProvider,
    ChangeSignatureRequest, DocumentEdit, DocumentSpot, LanguageFn, LanguageId, LanguageSupport,
    MethodSignature, Provider, Queries, SemanticServer, SignatureParameter, SourceEdit, SourceId,
    SyntaxProvider, TextPoint, TextSpan, WorkspaceChange,
};
use platform::CanonPath;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use ui_kit::{Theme, px};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    bounds_of, click, draw, editor_of, open_path, press, scratch, type_text,
};
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("fake-server");

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

fn point_in(text: &str, offset: usize) -> TextPoint {
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    TextPoint { line, character }
}

fn span_in(text: &str, start: usize, end: usize) -> TextSpan {
    TextSpan {
        start: point_in(text, start),
        end: point_in(text, end),
    }
}

fn edit_in(text: &str, start: usize, end: usize, new_text: &str) -> DocumentEdit {
    DocumentEdit {
        span: span_in(text, start, end),
        text: new_text.to_owned(),
    }
}

fn uri_of(path: &Path) -> String {
    CanonPath::new(path).unwrap().to_uri()
}

struct FakeLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    info: Mutex<ChangeSignatureInfo>,
    outcome_error: Mutex<Option<String>>,
    requests: Mutex<Vec<ChangeSignatureRequest>>,
    closed: Mutex<Option<PathBuf>>,
    supported: Mutex<bool>,
}

impl FakeLanguage {
    fn new() -> Arc<Self> {
        let signature = MethodSignature {
            name: "combine".to_owned(),
            visibility: "public".to_owned(),
            return_type: "String".to_owned(),
            parameters: vec![
                SignatureParameter {
                    type_name: "String".to_owned(),
                    name: "left".to_owned(),
                    default_value: String::new(),
                    original_index: Some(0),
                },
                SignatureParameter {
                    type_name: "String".to_owned(),
                    name: "right".to_owned(),
                    default_value: String::new(),
                    original_index: Some(1),
                },
            ],
            exceptions: Vec::new(),
        };
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
            info: Mutex::new(ChangeSignatureInfo {
                signature: Some(signature),
                error: None,
            }),
            outcome_error: Mutex::new(None),
            requests: Mutex::new(Vec::new()),
            closed: Mutex::new(None),
            supported: Mutex::new(true),
        })
    }

    fn set_supported(&self, supported: bool) {
        *self.supported.lock().unwrap() = supported;
    }

    fn set_info_error(&self, text: &str) {
        *self.info.lock().unwrap() = ChangeSignatureInfo {
            signature: None,
            error: Some(text.to_owned()),
        };
    }

    fn set_outcome_error(&self, text: &str) {
        *self.outcome_error.lock().unwrap() = Some(text.to_owned());
    }

    fn set_closed(&self, path: PathBuf) {
        *self.closed.lock().unwrap() = Some(path);
    }

    fn requests(&self) -> Vec<ChangeSignatureRequest> {
        self.requests.lock().unwrap().clone()
    }

    fn text_of(&self, uri: &str) -> Option<String> {
        if let Some(text) = self.documents.lock().unwrap().get(uri) {
            return Some(text.to_string());
        }
        uri_path(uri).and_then(|path| fs::read_to_string(path).ok())
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, lang_api::Confidence::Certain, SOURCE)
    }
}

impl SemanticServer for FakeLanguage {
    fn open(&self, uri: &str, _: i32, text: Arc<str>) {
        self.documents.lock().unwrap().insert(uri.to_owned(), text);
    }

    fn change(&self, uri: &str, _: i32, text: Arc<str>, _: Vec<DocumentEdit>) {
        self.documents.lock().unwrap().insert(uri.to_owned(), text);
    }

    fn save(&self, _: &str) {}

    fn close(&self, uri: &str) {
        self.documents.lock().unwrap().remove(uri);
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.documents
            .lock()
            .unwrap()
            .contains_key(uri)
            .then_some(1)
    }
}

impl Provider<DocumentSpot, ChangeSignatureInfo> for FakeLanguage {
    fn start(
        &self,
        _: DocumentSpot,
        _: base::Cancel,
        mut reply: lang_api::Reply<ChangeSignatureInfo>,
    ) {
        let info = self.info.lock().unwrap().clone();
        let confidence = if info.error.is_some() {
            lang_api::Confidence::Unknown
        } else {
            lang_api::Confidence::Certain
        };
        let _ = reply.send(Answer::new(info, confidence, SOURCE));
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for FakeLanguage {
    fn start(&self, _: ActionQuery, _: base::Cancel, mut reply: lang_api::Reply<Vec<ActionEntry>>) {
        let _ = reply.send(self.answer(vec![ActionEntry {
            title: "Change Signature".to_owned(),
            kind: Some(ActionKind::Refactor),
            code: Some("refactor.change.signature".to_owned()),
            preferred: true,
            edits: Vec::new(),
            renames: Vec::new(),
        }]));
    }
}

impl Provider<ChangeSignatureRequest, ChangeSignatureOutcome> for FakeLanguage {
    fn start(
        &self,
        request: ChangeSignatureRequest,
        _: base::Cancel,
        mut reply: lang_api::Reply<ChangeSignatureOutcome>,
    ) {
        self.requests.lock().unwrap().push(request.clone());
        if let Some(error) = self.outcome_error.lock().unwrap().clone() {
            let _ = reply.send(self.answer(ChangeSignatureOutcome {
                change: WorkspaceChange::default(),
                error: Some(error),
            }));
            return;
        }
        let Some(text) = self.text_of(&request.spot.uri) else {
            let _ = reply.send(self.answer(ChangeSignatureOutcome {
                change: WorkspaceChange::default(),
                error: Some("The file is not open".to_owned()),
            }));
            return;
        };
        let signature = &request.signature;
        let parameters = signature
            .parameters
            .iter()
            .map(|parameter| format!("{} {}", parameter.type_name, parameter.name))
            .collect::<Vec<_>>()
            .join(", ");
        let declaration = format!(
            "{} {} {}({parameters})",
            signature.visibility, signature.return_type, signature.name
        );
        let old_declaration = "public String combine(String left, String right)";
        let start = text.find(old_declaration).unwrap();
        let mut edits = vec![edit_in(
            &text,
            start,
            start + old_declaration.len(),
            &declaration,
        )];
        let arguments = signature
            .parameters
            .iter()
            .map(|parameter| match parameter.original_index {
                Some(0) => "\"a\"".to_owned(),
                Some(1) => "\"b\"".to_owned(),
                _ => parameter.default_value.clone(),
            })
            .collect::<Vec<_>>()
            .join(", ");
        let old_call = "combine(\"a\", \"b\")";
        let call = format!("{}({arguments})", signature.name);
        let start = text.find(old_call).unwrap();
        edits.push(edit_in(&text, start, start + old_call.len(), &call));
        let mut change = WorkspaceChange {
            edits: vec![SourceEdit {
                uri: request.spot.uri.clone(),
                edits,
            }],
            renames: Vec::new(),
        };
        if let Some(path) = self.closed.lock().unwrap().clone()
            && let Ok(closed) = fs::read_to_string(&path)
            && let Some(start) = closed.find(old_call)
        {
            change.edits.push(SourceEdit {
                uri: uri_of(&path),
                edits: vec![edit_in(&closed, start, start + old_call.len(), &call)],
            });
        }
        let _ = reply.send(self.answer(ChangeSignatureOutcome {
            change,
            error: None,
        }));
    }
}

impl LanguageSupport for FakeLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn change_signature_info(&self) -> Option<&ChangeSignatureInfoProvider> {
        if *self.supported.lock().unwrap() {
            Some(self)
        } else {
            None
        }
    }

    fn change_signature(&self) -> Option<&ChangeSignatureProvider> {
        if *self.supported.lock().unwrap() {
            Some(self)
        } else {
            None
        }
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
    root
}

fn open_window_with(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: Arc<FakeLanguage>,
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
    let setup = crate::window_tests::project_setup(scratch("cache"));
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

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

const SIGNATURE: &str = "class Signature {\n    public String combine(String left, String right) {\n        return left + right;\n    }\n\n    public String use() {\n        return combine(\"a\", \"b\");\n    }\n}\n";

const CALLER: &str =
    "class Caller {\n    String call() {\n        return combine(\"a\", \"b\");\n    }\n}\n";

#[ui_kit::gpui::test]
fn the_form_edits_the_open_and_closed_files_and_undo_takes_it_back(cx: &mut TestAppContext) {
    let root = project(
        "signature",
        &[("src/Signature.txt", SIGNATURE), ("src/Caller.txt", CALLER)],
    );
    let open = root.join("src").join("Signature.txt");
    let closed = root.join("src").join("Caller.txt");
    let language = FakeLanguage::new();
    language.set_closed(closed.clone());
    let window = open_window_with(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    press(cx, window, "ctrl-f6");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "change-signature").size.height > px(0.),
        "Ctrl+F6 shows the form"
    );

    click(cx, window, "change-signature-add");
    draw(cx, window);
    click(cx, window, "change-signature-type-2");
    type_text(cx, window, "int");
    click(cx, window, "change-signature-name-2");
    type_text(cx, window, "times");
    click(cx, window, "change-signature-default-2");
    type_text(cx, window, "9");
    click(cx, window, "change-signature-up-2");
    draw(cx, window);

    click(cx, window, "change-signature-preview");
    draw(cx, window);
    assert!(
        shown(cx, window, "change-signature-status"),
        "Preview shows what the change would do"
    );
    assert!(
        crate::window_tests::buffer_of(cx, &view).contains("combine(\"a\", \"b\")"),
        "Preview does not edit the file"
    );
    let requests = language.requests();
    let parameters = &requests.last().unwrap().signature.parameters;
    assert_eq!(parameters.len(), 3);
    assert_eq!(parameters[0].name, "left");
    assert_eq!(parameters[1].name, "times");
    assert_eq!(parameters[1].original_index, None);
    assert_eq!(parameters[2].name, "right");

    click(cx, window, "change-signature-refactor");
    cx.run_until_parked();
    draw(cx, window);
    let text = crate::window_tests::buffer_of(cx, &view);
    assert!(
        text.contains("public String combine(String left, int times, String right)"),
        "{text}"
    );
    assert!(text.contains("combine(\"a\", 9, \"b\")"), "{text}");
    let closed_text = fs::read_to_string(&closed).unwrap();
    assert!(
        closed_text.contains("combine(\"a\", 9, \"b\")"),
        "the closed caller is changed on disk: {closed_text}"
    );

    press(cx, window, "ctrl-z");
    let text = crate::window_tests::buffer_of(cx, &view);
    assert!(
        text.contains("public String combine(String left, String right)"),
        "one Ctrl+Z takes the open file back: {text}"
    );
}

#[ui_kit::gpui::test]
fn escape_and_server_errors_leave_the_file_alone(cx: &mut TestAppContext) {
    let root = project("signature-errors", &[("src/Signature.txt", SIGNATURE)]);
    let open = root.join("src").join("Signature.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language.clone());
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);
    let view = editor_of(cx, window, &open);

    press(cx, window, "ctrl-f6");
    draw(cx, window);
    click(cx, window, "change-signature-add");
    type_text(cx, window, "value");
    press(cx, window, "escape");
    draw(cx, window);
    assert!(
        !shown(cx, window, "change-signature"),
        "Escape closes the form"
    );
    assert_eq!(crate::window_tests::buffer_of(cx, &view), SIGNATURE);

    language.set_info_error("Put the caret on a method");
    press(cx, window, "ctrl-f6");
    draw(cx, window);
    assert!(
        shown(cx, window, "change-signature-error"),
        "a server error is shown in the form"
    );
    assert_eq!(crate::window_tests::buffer_of(cx, &view), SIGNATURE);

    press(cx, window, "escape");
    draw(cx, window);
    let signature = MethodSignature {
        name: "combine".to_owned(),
        visibility: "public".to_owned(),
        return_type: "String".to_owned(),
        parameters: vec![
            SignatureParameter {
                type_name: "String".to_owned(),
                name: "left".to_owned(),
                default_value: String::new(),
                original_index: Some(0),
            },
            SignatureParameter {
                type_name: "String".to_owned(),
                name: "right".to_owned(),
                default_value: String::new(),
                original_index: Some(1),
            },
        ],
        exceptions: Vec::new(),
    };
    *language.info.lock().unwrap() = ChangeSignatureInfo {
        signature: Some(signature),
        error: None,
    };
    language.set_outcome_error("Refactoring failed");
    press(cx, window, "ctrl-f6");
    draw(cx, window);
    click(cx, window, "change-signature-refactor");
    draw(cx, window);
    assert!(
        shown(cx, window, "change-signature-error"),
        "the error of the refactoring reaches the form"
    );
    assert_eq!(crate::window_tests::buffer_of(cx, &view), SIGNATURE);

    press(cx, window, "escape");
    language.set_supported(false);
    press(cx, window, "ctrl-f6");
    draw(cx, window);
    assert!(
        shown(cx, window, "change-signature-error"),
        "a language without the capability says so in the form"
    );
    assert_eq!(crate::window_tests::buffer_of(cx, &view), SIGNATURE);
    press(cx, window, "escape");
}

#[ui_kit::gpui::test]
fn refactor_this_lists_change_signature(cx: &mut TestAppContext) {
    let root = project(
        "signature-refactor-this",
        &[("src/Signature.txt", SIGNATURE)],
    );
    let open = root.join("src").join("Signature.txt");
    let language = FakeLanguage::new();
    let window = open_window_with(cx, root.clone(), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &open);

    press(cx, window, "ctrl-alt-shift-t");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "actions-popup").size.height > px(0.),
        "Refactor This lists the refactorings"
    );
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        bounds_of(cx, window, "change-signature").size.height > px(0.),
        "the row opens Change Signature"
    );
}

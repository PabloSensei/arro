//! The language tools against a language that answers through `lang-api` like a server would.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lang_api::{
    Answer, Confidence, DiagnosticEntry, DiagnosticsQuery, DocumentEdit, DocumentSpot, HoverText,
    LanguageId, LanguageSupport, NavigationQuery, Provider, Queries, Reply, SemanticServer,
    Severity, SourceId, SourceSpan, SyntaxProvider, TextPoint, TextSpan,
};
use serde_json::json;
use tool_gateway::{AuditLog, LanguageTools, SessionScope, ToolError, ToolGateway};
use tree_sitter_language::LanguageFn;

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

/// Opens `.fake` files only, remembers the spots it was asked about.
struct FakeLanguage {
    syntax: NoSyntax,
    open: bool,
    confidence: Confidence,
    asked: Mutex<Vec<DocumentSpot>>,
}

impl FakeLanguage {
    fn new(open: bool, confidence: Confidence) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            open,
            confidence,
            asked: Mutex::new(Vec::new()),
        })
    }

    fn answer<T>(&self, value: T) -> Answer<T> {
        Answer::new(value, self.confidence, SOURCE)
    }
}

impl SemanticServer for FakeLanguage {
    fn open(&self, _: &str, _: i32, _: Arc<str>) {}
    fn change(&self, _: &str, _: i32, _: Arc<str>, _: Vec<DocumentEdit>) {}
    fn save(&self, _: &str) {}
    fn close(&self, _: &str) {}

    fn version(&self, _: &str) -> Option<i32> {
        self.open.then_some(7)
    }
}

impl Provider<DocumentSpot, HoverText> for FakeLanguage {
    fn start(&self, spot: DocumentSpot, _: base::Cancel, mut reply: Reply<HoverText>) {
        self.asked.lock().unwrap().push(spot);
        let text = "```fake\nvoid greet(String name)\n```\nSays hello.";
        let _ = reply.send(self.answer(HoverText(text.to_string())));
    }
}

impl Provider<NavigationQuery, Vec<SourceSpan>> for FakeLanguage {
    fn start(&self, query: NavigationQuery, _: base::Cancel, mut reply: Reply<Vec<SourceSpan>>) {
        let span = SourceSpan {
            uri: query.spot.uri.clone(),
            span: TextSpan {
                start: TextPoint {
                    line: 0,
                    character: 5,
                },
                end: TextPoint::default(),
            },
        };
        let _ = reply.send(self.answer(vec![span]));
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for FakeLanguage {
    fn start(&self, _: DiagnosticsQuery, _: base::Cancel, mut reply: Reply<Vec<DiagnosticEntry>>) {
        let at = |line, character| TextSpan {
            start: TextPoint { line, character },
            end: TextPoint::default(),
        };
        let entries = vec![
            DiagnosticEntry {
                span: at(1, 2),
                severity: Severity::Information,
                message: "just so you know".to_string(),
                unnecessary: false,
            },
            DiagnosticEntry {
                span: at(1, 4),
                severity: Severity::Error,
                message: "greet expects a name".to_string(),
                unnecessary: false,
            },
        ];
        // A live stream: the reply stays open, as with a real server.
        let _ = reply.send(self.answer(entries));
        std::mem::forget(reply);
    }
}

impl LanguageSupport for FakeLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension == "fake")
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn navigation(&self) -> Option<&lang_api::NavigationProvider> {
        Some(self)
    }

    fn hover(&self) -> Option<&lang_api::HoverProvider> {
        Some(self)
    }

    fn diagnostics(&self) -> Option<&lang_api::DiagnosticsProvider> {
        Some(self)
    }
}

struct Fixture {
    project: PathBuf,
    gateway: ToolGateway,
    scope: SessionScope,
}

impl Fixture {
    fn new(name: &str, language: Option<Arc<FakeLanguage>>) -> Self {
        let project = std::env::temp_dir().join(format!("ide-lang-{name}-{}", std::process::id()));
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("Hello.fake"),
            "void greet(String name) {}\n  😀greet();\n",
        )
        .unwrap();
        let tools = LanguageTools::with_wait(Duration::from_secs(2));
        if let Some(language) = language {
            tools.register(language);
        }
        let gateway = ToolGateway::new(Arc::new(tools), AuditLog::default());
        let scope = SessionScope {
            session: "s".to_string(),
            project: project.clone(),
            cwd: project.clone(),
        };
        Self {
            project,
            gateway,
            scope,
        }
    }

    fn call(&self, tool: &str, input: serde_json::Value) -> Result<String, ToolError> {
        self.gateway.call(&self.scope, tool, &input)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.project);
    }
}

#[test]
fn symbol_info_asks_the_server_in_utf16_and_shows_the_declaration() {
    let language = FakeLanguage::new(true, Confidence::Certain);
    let fixture = Fixture::new("info", Some(language.clone()));

    let text = fixture
        .call(
            "get_symbol_info",
            json!({"path": "Hello.fake", "line": 2, "column": 4}),
        )
        .expect("info");

    let asked = language.asked.lock().unwrap()[0].clone();
    assert_eq!(asked.version, 7);
    assert_eq!(
        asked.point,
        TextPoint {
            line: 1,
            character: 4
        }
    );
    assert!(asked.uri.starts_with("file:///"), "{}", asked.uri);
    assert!(text.starts_with("symbol greet\n"), "{text}");
    assert!(
        text.contains("source: fake-server, confidence: certain"),
        "{text}"
    );
    assert!(
        text.contains("signature: void greet(String name)"),
        "{text}"
    );
    assert!(
        text.contains("Hello.fake:1:void greet(String name) {}"),
        "{text}"
    );
}

#[test]
fn diagnostics_take_the_first_list_and_filter_by_severity() {
    let fixture = Fixture::new("diag", Some(FakeLanguage::new(true, Confidence::Certain)));

    let text = fixture
        .call("get_diagnostics", json!({"path": "Hello.fake"}))
        .expect("diagnostics");

    assert!(text.starts_with("1 diagnostics in "), "{text}");
    assert!(
        text.contains("Hello.fake:2:4: error: greet expects a name"),
        "{text}"
    );
    assert!(!text.contains("just so you know"), "{text}");
}

#[test]
fn an_unknown_answer_is_not_an_empty_one() {
    let fixture = Fixture::new(
        "unknown",
        Some(FakeLanguage::new(true, Confidence::Unknown)),
    );

    let error = fixture
        .call("get_diagnostics", json!({"path": "Hello.fake"}))
        .unwrap_err();

    assert!(
        error.to_string().contains("does not mean there is nothing"),
        "{error}"
    );
}

#[test]
fn closed_files_and_files_without_a_server_say_so() {
    let closed = Fixture::new(
        "closed",
        Some(FakeLanguage::new(false, Confidence::Certain)),
    );
    let none = Fixture::new("none", None);
    let at = json!({"path": "Hello.fake", "line": 1, "column": 6});

    let closed = closed.call("get_symbol_info", at.clone()).unwrap_err();
    let none = none.call("get_symbol_info", at).unwrap_err();

    assert!(
        closed.to_string().contains("not open in the editor"),
        "{closed}"
    );
    assert!(
        none.to_string().contains("no language server runs"),
        "{none}"
    );
}

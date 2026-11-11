//! Semantic tokens in the window: a stand-in language answers with tokens for a small
//! file, and the editor paints them over the syntax colours.
//!
//! The scenario with the real language server at the end is not part of a normal run:
//!
//! `cargo test -p panels window_tests_semantic_tokens -- --ignored --test-threads=1`

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use base::Cancel;
use editor_view::{Semantic, SemanticLook};
use lang_api::{
    Answer, Confidence, LanguageFn, LanguageId, LanguageSupport, Provider, Queries, Reply,
    SemanticToken, SemanticTokenKind, SemanticTokenModifiers, SemanticTokensProvider,
    SemanticTokensQuery, SourceId, SyntaxProvider,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::window_tests::{
    draw, editor_of, open_path, open_window_with, project_setup, scratch, wait_for_signal,
};
use crate::window_tests_lang::span_in;

const SOURCE: SourceId = SourceId::new("token-server");

const TEXT: &str = "class Holder {\n\
    String field = \"x\";\n\
    static String staticName() { return \"s\"; }\n\
    @Deprecated\n\
    void old() {}\n\
    void call(int param) {\n\
        String local = param;\n\
    }\n\
}\n";

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

/// Answers the tokens the test put in, whatever the document version.
struct TokenProvider(Mutex<Vec<SemanticToken>>);

impl Provider<SemanticTokensQuery, Vec<SemanticToken>> for TokenProvider {
    fn start(&self, _: SemanticTokensQuery, _: Cancel, mut reply: Reply<Vec<SemanticToken>>) {
        let tokens = self.0.lock().unwrap().clone();
        let _ = reply.send(Answer::new(tokens, Confidence::Certain, SOURCE));
    }
}

struct TokenLanguage {
    syntax: NoSyntax,
    tokens: TokenProvider,
}

impl TokenLanguage {
    fn new(text: &str) -> Arc<Self> {
        let mut tokens = Vec::new();
        for (name, kind, modifiers) in [
            (
                "field",
                SemanticTokenKind::Property,
                SemanticTokenModifiers::default(),
            ),
            (
                "staticName",
                SemanticTokenKind::Method,
                SemanticTokenModifiers {
                    static_member: true,
                    ..SemanticTokenModifiers::default()
                },
            ),
            (
                "param",
                SemanticTokenKind::Parameter,
                SemanticTokenModifiers::default(),
            ),
            (
                "local",
                SemanticTokenKind::Variable,
                SemanticTokenModifiers::default(),
            ),
        ] {
            if let Some(at) = text.find(name) {
                tokens.push(SemanticToken {
                    span: span_in(text, at, at + name.len()),
                    kind,
                    modifiers,
                });
            }
        }
        Arc::new(Self {
            syntax: NoSyntax(Queries::default()),
            tokens: TokenProvider(Mutex::new(tokens)),
        })
    }
}

impl LanguageSupport for TokenLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("tokens")
    }

    fn matches(&self, path: &std::path::Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn semantic_tokens(&self) -> Option<&SemanticTokensProvider> {
        Some(&self.tokens)
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

/// A window whose files get `language` as their server, without a server process.
fn window_with(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (WindowHandle<Workspace>, PathBuf, Arc<TokenLanguage>) {
    let root = project(name, &[("src/Holder.txt", text)]);
    let window = open_window_with(cx, root.clone(), project_setup(scratch("cache")));
    let language = TokenLanguage::new(text);
    cx.update(|cx| {
        cx.set_global(Semantic(language.clone()));
    });
    cx.run_until_parked();
    draw(cx, window);
    (window, root.join("src").join("Holder.txt"), language)
}

fn look(
    cx: &TestAppContext,
    view: &ui_kit::gpui::Entity<editor_view::EditorView>,
    offset: usize,
) -> Option<SemanticLook> {
    view.read_with(cx, |view, _| view.semantic_look_at(offset))
}

fn offset_of(text: &str, name: &str) -> usize {
    text.find(name).unwrap()
}

#[gpui::test]
fn a_field_and_a_parameter_get_their_own_colours_and_a_static_member_italics(
    cx: &mut TestAppContext,
) {
    let (window, path, _language) = window_with(cx, "semantic-tokens", TEXT);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_signal(cx, "the semantic tokens", |cx| {
        look(cx, &view, offset_of(TEXT, "field")).is_some()
    });
    draw(cx, window);

    let field = look(cx, &view, offset_of(TEXT, "field")).expect("a field token");
    let parameter = look(cx, &view, offset_of(TEXT, "param")).expect("a parameter token");
    let local = look(cx, &view, offset_of(TEXT, "local")).expect("a local token");
    let static_member = look(cx, &view, offset_of(TEXT, "staticName")).expect("a method token");
    assert_ne!(field.color, parameter.color, "a field is not a parameter");
    assert_ne!(local.color, parameter.color, "a local is not a parameter");
    assert!(static_member.italic, "a static member is italic");
    assert!(!field.italic, "a plain field is upright");
}

#[gpui::test]
fn the_setting_removes_the_tokens_from_the_open_editor(cx: &mut TestAppContext) {
    let (window, path, _language) = window_with(cx, "semantic-tokens-off", TEXT);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_signal(cx, "the semantic tokens", |cx| {
        look(cx, &view, offset_of(TEXT, "field")).is_some()
    });

    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(
                |settings: &mut Settings| settings.semantic_highlighting = false,
                cx,
            )
        })
        .unwrap();
    draw(cx, window);
    assert!(
        look(cx, &view, offset_of(TEXT, "field")).is_none(),
        "the tokens go away with the setting"
    );

    window
        .update(cx, |workspace, _, cx| {
            workspace.update_settings(
                |settings: &mut Settings| settings.semantic_highlighting = true,
                cx,
            )
        })
        .unwrap();
    wait_for_signal(cx, "the tokens again", |cx| {
        look(cx, &view, offset_of(TEXT, "field")).is_some()
    });
}

const PROBE: &str = r#"package probe;

public class SemanticProbe {

    private String name = "probe";

    static String staticName() {
        return "static";
    }

    @Deprecated
    public void old() {
    }

    public void use(int param) {
        String local = param;
        String text = name;
    }
}
"#;

/// The folder of the sources of the copied project, whose name this neutral crate does
/// not spell.
fn sources_folder(root: &std::path::Path) -> PathBuf {
    std::fs::read_dir(root.join("src").join("main"))
        .unwrap()
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .find(|path| path.is_dir())
        .expect("a sources folder")
}

/// The first source file under `folder`, whose extension the probe takes.
fn first_source(folder: &std::path::Path) -> Option<PathBuf> {
    for entry in std::fs::read_dir(folder).ok()?.filter_map(Result::ok) {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = first_source(&path) {
                return Some(found);
            }
        } else {
            return Some(path);
        }
    }
    None
}

/// The probe file of the copied project, with the extension its sources already use.
fn probe_file(root: &std::path::Path) -> PathBuf {
    let folder = sources_folder(root);
    let extension = first_source(&folder)
        .and_then(|path| path.extension().map(|extension| extension.to_os_string()))
        .expect("a source file");
    folder
        .join("probe")
        .join("SemanticProbe")
        .with_extension(extension)
}

fn real_project(name: &str) -> (PathBuf, window_language::RealLanguage) {
    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch(name).join("petclinic");
    window_language::prepare_project(&source, &root);
    let probe = probe_file(&root);
    std::fs::create_dir_all(probe.parent().unwrap()).unwrap();
    std::fs::write(&probe, PROBE).unwrap();
    let language =
        window_language::RealLanguage::open(root.clone(), scratch(&format!("{name}-state")))
            .expect("the language server starts");
    (root, language)
}

/// Not part of a normal run. The class has a field, a static method and a method marked
/// `@Deprecated`; the real server colours each of them and the editor paints the answers.
#[gpui::test]
#[ignore]
fn a_real_server_colours_a_field_a_static_method_and_a_deprecated_one(cx: &mut TestAppContext) {
    use std::time::Duration;

    use crate::window_tests_real::{build_window, wait_for, wait_ready};

    let (root, language) = real_project("semantic-tokens-real");
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let path = probe_file(&root);
    open_path(cx, window, &path);
    draw(cx, window);
    let view = editor_of(cx, window, &path);
    wait_for(cx, "the semantic tokens", Duration::from_secs(120), |cx| {
        look(cx, &view, offset_of(PROBE, "name")).is_some()
    });
    draw(cx, window);

    let field = look(cx, &view, offset_of(PROBE, "name")).expect("a field token");
    let parameter = look(cx, &view, offset_of(PROBE, "param")).expect("a parameter token");
    let static_member =
        look(cx, &view, offset_of(PROBE, "staticName")).expect("a static method token");
    let deprecated = look(cx, &view, offset_of(PROBE, "old")).expect("a deprecated method token");
    assert_ne!(field.color, parameter.color);
    assert!(static_member.italic);
    assert!(deprecated.strikethrough);

    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    drop(language);
    cx.update(|cx| cx.shutdown());
}

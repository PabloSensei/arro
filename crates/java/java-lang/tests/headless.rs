//! The whole path against a real jdtls, without a window, through the `lang-api` traits.
//!
//! cargo test -p java-lang --test headless -- --ignored --test-threads=1
//!
//! Needs `IDE_CORPUS`, the folder with `src`, `jdk` and `tools`, and
//! `MAVEN_USER_HOME` with a filled repository. Projects are copied, the corpus
//! stays untouched.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::str::FromStr;
use std::time::Duration;

use futures_util::StreamExt;
use java_lang::{JavaProject, JdtlsConfig, Runtime, State};
use lang_api::{
    ActionEntry, ActionKind, ActionQuery, Confidence, DiagnosticsQuery, DocumentEdit, DocumentSpot,
    FormatQuery, LanguageSupport, MoveKind, MoveQuery, MoveRequest, NavigationKind,
    NavigationQuery, RenameQuery, Replies, Severity, SourceSpan, SymbolEntry, SymbolQuery,
    TextPoint, TextSpan, WorkspaceChange, ask,
};
use lsp_client::lsp_types::Position;
use lsp_client::lsp_types::Uri;
use lsp_client::uri::{file_path, file_uri};
use lsp_client::utf16::{offset_at, position_at};
use platform::CanonPath;
use sidecar::{Status, Trust};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

const READY: Duration = Duration::from_secs(600);
const ANSWER: Duration = Duration::from_secs(120);

const SKIPPED: &[&str] = &[".git", "target", "build", "bin", ".gradle", "node_modules"];

const PROBE: &str = r#"package probe;

import java.util.ArrayList;
import java.util.List;
import org.springframework.util.StringUtils;

public class Probe {

    private final String name = "probe";

    public String greet(String who) {
        List<String> parts = new ArrayList<>();
        parts.add(who);
        String text = String.join(",", parts);
        return StringUtils.hasText(text) ? text.trim() : name;
    }

    public int unicode() {
        String smile = "😀😀"; int after = smile.length(); int again = after + 1;
        return again;
    }
}
"#;

fn corpus() -> PathBuf {
    PathBuf::from(std::env::var("IDE_CORPUS").expect("set IDE_CORPUS"))
}

fn copy_tree(from: &Path, to: &Path, inside_sources: bool) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let text = name.to_string_lossy();
        if !inside_sources && SKIPPED.contains(&text.as_ref()) {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_tree(
                &entry.path(),
                &to.join(&name),
                inside_sources || text == "src",
            );
        } else {
            fs::copy(entry.path(), to.join(&name)).unwrap();
        }
    }
}

struct Fixture {
    probe: PathBuf,
    config: JdtlsConfig,
}

/// A private copy of a project with the probe class, and a clean jdtls state.
fn fixture(project: &str, module: &str, drop: &[&str]) -> Fixture {
    let corpus = corpus();
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("headless")
        .join(project);
    let root = scratch.join("project");
    if !root.exists() {
        copy_tree(&corpus.join("src").join(project), &root, false);
    }
    for name in drop {
        let _ = fs::remove_file(root.join(name));
    }
    let probe = root.join(module).join("src/main/java/probe/Probe.java");
    fs::create_dir_all(probe.parent().unwrap()).unwrap();
    fs::write(&probe, PROBE).unwrap();
    let state_dir = scratch.join("state");
    let _ = fs::remove_dir_all(&state_dir);
    fs::create_dir_all(&state_dir).unwrap();

    let m2 = PathBuf::from(std::env::var("MAVEN_USER_HOME").expect("set MAVEN_USER_HOME"));
    let settings = scratch.join("maven-settings.xml");
    let repository = m2
        .join("repository")
        .display()
        .to_string()
        .replace('\\', "/");
    let xml = format!(
        "<settings><localRepository>{repository}</localRepository><offline>true</offline></settings>"
    );
    fs::write(&settings, xml).unwrap();

    let tools = corpus.join("tools");
    let runtime = |name: &str, version: &str, default| Runtime {
        name: name.to_owned(),
        home: fs::canonicalize(corpus.join("jdk").join(version)).unwrap(),
        default,
    };
    let config = JdtlsConfig {
        jdtls_home: tools.join("jdtls-1.63.0"),
        debug_plugin: tools.join(
            "vscode-java-debug-0.60.0/extension/server/com.microsoft.java.debug.plugin-0.53.2.jar",
        ),
        java_home: fs::canonicalize(corpus.join("jdk").join("25")).unwrap(),
        project_root: root.clone(),
        state_dir,
        runtimes: vec![
            runtime("JavaSE-17", "17", false),
            runtime("JavaSE-25", "25", true),
        ],
        maven_settings: Some(settings),
        gradle_home: None,
        gradle_user_home: None,
        gradle_java_home: None,
        offline: true,
        environment: HashMap::new(),
    };
    Fixture { probe, config }
}

async fn until(status: &mut watch::Receiver<Status>, test: impl Fn(&Status) -> bool) -> Status {
    let waited = timeout(READY, status.wait_for(test)).await;
    waited
        .expect("jdtls did not get there in time")
        .unwrap()
        .clone()
}

fn is_running(process_id: u32) -> bool {
    let output = Command::new("tasklist")
        .args(["/FI", &format!("PID eq {process_id}"), "/NH"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).contains(&process_id.to_string())
}

async fn assert_ends(process_id: u32) {
    timeout(Duration::from_secs(30), async {
        while is_running(process_id) {
            sleep(Duration::from_millis(200)).await;
        }
    })
    .await
    .expect("the process outlived the project");
}

/// The first answer, or `None` when the provider stays silent.
async fn first<T>(replies: Replies<T>) -> Option<T> {
    let mut replies = replies;
    let answer = timeout(ANSWER, replies.next())
        .await
        .expect("no answer in time");
    answer.map(|answer| answer.value)
}

/// A failed search answers `Unknown`, which on a fresh index is a timeout: ask again.
async fn workspace_symbols(project: &JavaProject, class: &str) -> Vec<SymbolEntry> {
    let provider = project.workspace_symbols().unwrap();
    for attempt in 1..=5 {
        let mut replies = ask(provider, SymbolQuery(class.to_owned()));
        let answer = timeout(ANSWER, replies.next())
            .await
            .expect("no answer in time");
        let answer = answer.expect("the search stayed silent");
        if answer.confidence != Confidence::Unknown {
            return answer.value;
        }
        eprintln!("{class}: no answer from the server, attempt {attempt}");
    }
    panic!("no answer for {class}");
}

struct Open<'a> {
    project: &'a JavaProject,
    uri: String,
    text: String,
    version: i32,
}

impl Open<'_> {
    fn point(&self, offset: usize) -> TextPoint {
        let position = position_at(&self.text, offset);
        TextPoint {
            line: position.line,
            character: position.character,
        }
    }

    fn spot(&self, needle: &str, shift: usize) -> DocumentSpot {
        let offset = self.text.find(needle).unwrap() + shift;
        DocumentSpot {
            uri: self.uri.clone(),
            version: self.version,
            point: self.point(offset),
        }
    }

    /// Asks again while the project settles: right after Ready a type may be unknown.
    async fn definition(&self, needle: &str, shift: usize) -> Vec<SourceSpan> {
        let navigation = self.project.navigation().unwrap();
        for _ in 0..40 {
            let query = NavigationQuery {
                kind: NavigationKind::Definition,
                spot: self.spot(needle, shift),
            };
            let found = first(ask(navigation, query)).await.unwrap_or_default();
            if !found.is_empty() {
                return found;
            }
            sleep(Duration::from_millis(250)).await;
        }
        panic!("no definition for {needle}");
    }

    fn offset(&self, point: TextPoint) -> usize {
        offset_at(&self.text, Position::new(point.line, point.character))
    }

    fn span(&self, needle: &str) -> TextSpan {
        let start = self.text.find(needle).unwrap();
        TextSpan {
            start: self.point(start),
            end: self.point(start + needle.len()),
        }
    }

    /// Server edits, all in the coordinates of the current text, applied as the editor
    /// does: from the last one back, one change each.
    fn apply(&mut self, mut edits: Vec<DocumentEdit>) {
        edits.sort_by_key(|edit| (edit.span.start.line, edit.span.start.character));
        let ranges: Vec<(usize, usize, String)> = edits
            .into_iter()
            .map(|edit| {
                (
                    self.offset(edit.span.start),
                    self.offset(edit.span.end),
                    edit.text,
                )
            })
            .collect();
        for (start, end, text) in ranges.into_iter().rev() {
            self.replace(start, end, &text);
        }
    }

    /// Code actions for `span`, asked again while the server has not seen the error yet.
    async fn actions(&self, span: TextSpan, only: Option<ActionKind>) -> Vec<ActionEntry> {
        let provider = self.project.actions().unwrap();
        for _ in 0..40 {
            let query = ActionQuery {
                uri: self.uri.clone(),
                version: self.version,
                span,
                only,
            };
            let found = first(ask(provider, query)).await.unwrap_or_default();
            if !found.is_empty() {
                return found;
            }
            sleep(Duration::from_millis(250)).await;
        }
        panic!("no actions for {span:?}");
    }

    fn mine(&self, entry: &ActionEntry) -> Vec<DocumentEdit> {
        let sources = entry.edits.iter().filter(|source| source.uri == self.uri);
        sources.flat_map(|source| source.edits.clone()).collect()
    }

    fn replace(&mut self, start: usize, end: usize, new_text: &str) {
        let span = TextSpan {
            start: self.point(start),
            end: self.point(end),
        };
        self.text.replace_range(start..end, new_text);
        self.version += 1;
        let edit = DocumentEdit {
            span,
            text: new_text.to_owned(),
        };
        self.project.server().unwrap().change(
            &self.uri,
            self.version,
            self.text.as_str().into(),
            vec![edit],
        );
    }
}

async fn wait_error(project: &JavaProject, uri: &str, wanted: bool) {
    let diagnostics = project.diagnostics().unwrap();
    let mut lists = ask(diagnostics, DiagnosticsQuery(uri.to_owned()));
    timeout(ANSWER, async {
        while let Some(list) = lists.next().await {
            let has_error = list
                .value
                .iter()
                .any(|entry| entry.severity == Severity::Error);
            if has_error == wanted {
                return;
            }
        }
        panic!("diagnostics ended");
    })
    .await
    .expect("diagnostics did not arrive");
}

async fn check_project(fixture: Fixture) {
    let Fixture { probe, config } = fixture;
    let project = JavaProject::open(config, Trust::Trusted).unwrap();
    let mut status = project.status();
    let ready = until(&mut status, |status| status.state == State::Ready).await;
    assert_eq!(ready.generation, 1);
    let process_id = ready.process_id.unwrap();

    let uri = file_uri(&CanonPath::new(&probe).unwrap())
        .unwrap()
        .as_str()
        .to_owned();
    let text = fs::read_to_string(&probe).unwrap();
    project
        .server()
        .unwrap()
        .open(&uri, 1, text.as_str().into());
    let mut open = Open {
        project: &project,
        uri: uri.clone(),
        text,
        version: 1,
    };

    // Autocomplete right after a dot.
    let dot = open.text.find("parts.add").unwrap() + "parts.".len();
    let spot = DocumentSpot {
        uri: uri.clone(),
        version: 1,
        point: open.point(dot),
    };
    let list = first(ask(project.completion().unwrap(), spot))
        .await
        .unwrap();
    assert!(
        list.entries
            .iter()
            .any(|entry| entry.label.starts_with("size")),
        "{} entries",
        list.entries.len()
    );

    // Ctrl+B in the same file.
    let local = open.definition("name;", 1).await;
    assert_eq!(local[0].uri, uri);
    assert_eq!(local[0].span.start.line, 8);

    // Ctrl+B into a JDK class, and the text behind the location.
    let jdk = open.definition("String.join", 1).await;
    assert!(jdk[0].uri.starts_with("jdt://"), "{}", jdk[0].uri);
    let class = Uri::from_str(&jdk[0].uri).unwrap();
    let source = timeout(ANSWER, project.class_file_contents(&class))
        .await
        .unwrap();
    assert!(source.unwrap().contains("class String"));

    // Ctrl+B into a dependency.
    let dependency = open.definition("StringUtils.hasText", 1).await;
    assert!(dependency[0].uri.starts_with("jdt://"));

    // UTF-16: the column after two surrogate pairs.
    let utf16 = open.definition("after + 1", 1).await;
    assert_eq!(utf16[0].span.start, open.spot("after = smile", 0).point);

    // Ctrl+N finds JDK and dependency classes, not only project ones.
    for class in ["ArrayList", "StringUtils"] {
        let found = workspace_symbols(&project, class).await;
        assert!(
            found.iter().any(|symbol| symbol.name.contains(class)),
            "{class}: {found:?}"
        );
    }

    // Ctrl+Q.
    let hover = first(ask(project.hover().unwrap(), open.spot("String.join", 8))).await;
    assert!(!hover.unwrap().0.is_empty());

    // An error appears and goes away with the edit.
    let bad = "int bad = \"x\"; ";
    let insert = open.text.find("parts.add").unwrap();
    open.replace(insert, insert, bad);
    wait_error(&project, &uri, true).await;
    open.replace(insert, insert + bad.len(), "");
    wait_error(&project, &uri, false).await;

    // Occurrences of `parts`: the declaration and its two uses.
    let highlights = project.highlights().unwrap();
    let spans = first(ask(highlights, open.spot("parts =", 1)))
        .await
        .unwrap();
    assert_eq!(spans.len(), 3, "{spans:?}");

    // Ctrl+Alt+L takes away indentation that does not belong.
    let line = open.text.find("parts.add").unwrap();
    open.replace(line, line, "      ");
    let query = FormatQuery {
        uri: uri.clone(),
        version: open.version,
        tab_size: 4,
        insert_spaces: true,
    };
    let edits = first(ask(project.format().unwrap(), query)).await.unwrap();
    assert!(!edits.is_empty());
    open.apply(edits);
    assert!(
        open.text.contains("\n        parts.add(who);"),
        "{}",
        open.text
    );

    // Alt+Enter on a class without its import offers the import.
    let line = open.text.find("parts.add").unwrap();
    open.replace(line, line, "LinkedList<String> more = null; ");
    wait_error(&project, &uri, true).await;
    let actions = open.actions(open.span("LinkedList"), None).await;
    let import = actions
        .iter()
        .find(|entry| entry.title.contains("LinkedList") && entry.title.contains("mport"))
        .unwrap_or_else(|| panic!("{:?}", actions.iter().map(|a| &a.title).collect::<Vec<_>>()));
    let edits = open.mine(import);
    open.apply(edits);
    assert!(
        open.text.contains("import java.util.LinkedList;"),
        "{}",
        open.text
    );
    wait_error(&project, &uri, false).await;

    // Ctrl+Alt+O removes the import once nothing uses it.
    let used = open.text.find("LinkedList<String> more").unwrap();
    let length = "LinkedList<String> more = null; ".len();
    open.replace(used, used + length, "");
    let whole = TextSpan {
        start: TextPoint::default(),
        end: open.point(open.text.len()),
    };
    let organize = open.actions(whole, Some(ActionKind::OrganizeImports)).await;
    let edits = open.mine(&organize[0]);
    open.apply(edits);
    assert!(!open.text.contains("LinkedList"), "{}", open.text);
    assert!(open.text.contains("import java.util.ArrayList;"));

    // A stale question gets no answer.
    let mut old = open.spot("name;", 1);
    old.version -= 1;
    assert!(first(ask(project.hover().unwrap(), old)).await.is_none());

    // Killing the server brings a new one, which receives the open document.
    let killed = Command::new("taskkill")
        .args(["/F", "/PID", &process_id.to_string()])
        .output()
        .unwrap();
    assert!(killed.status.success());
    assert_ends(process_id).await;
    let again = until(&mut status, |status| {
        status.state == State::Ready && status.generation == 2
    })
    .await;
    assert_eq!(again.crashes, 1);
    let second_id = again.process_id.unwrap();
    assert_ne!(second_id, process_id);
    // The edits above may have moved the declaration, so its line comes from the text.
    let local = open.definition("name;", 1).await;
    assert_eq!(local[0].span.start.line, open.spot("name =", 0).point.line);

    // A polite stop leaves nothing behind.
    project.stop().await;
    assert_ends(second_id).await;
}

const CALLER: &str = r#"package probe;

public class Caller {

    public String call() {
        return new Probe().greet("x");
    }
}
"#;

const MOVER: &str = r#"package probe;

public class Mover {

    public String value() {
        return "moved";
    }
}
"#;

const MOVER_USER: &str = r#"package probe;

public class MoverUser {

    public String use() {
        return new Mover().value();
    }
}
"#;

const PARAM: &str = r#"package probe;

public class Param {

    public int twice(int value) {
        return value * 2;
    }
}
"#;

fn uri_of(path: &Path) -> String {
    file_uri(&CanonPath::new(path).unwrap())
        .unwrap()
        .as_str()
        .to_owned()
}

/// Applies the edits of `change` to the open documents it names.
fn apply_change(docs: &mut [&mut Open<'_>], change: &WorkspaceChange) {
    for source in &change.edits {
        let doc = docs.iter_mut().find(|doc| doc.uri == source.uri);
        let doc = doc.unwrap_or_else(|| panic!("an edit of a closed {}", source.uri));
        doc.apply(source.edits.clone());
    }
}

/// Moves the files of `change` on disk and follows them with the server, as the window
/// does after a move refactoring.
fn apply_renames(docs: &mut [&mut Open<'_>], change: &WorkspaceChange) {
    for rename in &change.renames {
        let doc = docs
            .iter_mut()
            .find(|doc| doc.uri == rename.from)
            .unwrap_or_else(|| panic!("a rename of a closed {}", rename.from));
        let from = file_path(&Uri::from_str(&rename.from).unwrap()).unwrap();
        let to = file_path(&Uri::from_str(&rename.to).unwrap()).unwrap();
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::write(&to, &doc.text).unwrap();
        fs::remove_file(&from).unwrap();
        doc.project.server().unwrap().close(&doc.uri);
        doc.uri = rename.to.clone();
        doc.version += 1;
        doc.project
            .server()
            .unwrap()
            .open(&doc.uri, doc.version, doc.text.as_str().into());
    }
}

/// M5b: usages, rename of a method and of a class across files, extract method and
/// Generate, all on the code that the IDE would apply.
async fn check_refactorings(fixture: Fixture) {
    let Fixture { probe, config } = fixture;
    let caller_path = probe.with_file_name("Caller.java");
    let mover_path = probe.with_file_name("Mover.java");
    let mover_user_path = probe.with_file_name("MoverUser.java");
    let param_path = probe.with_file_name("Param.java");
    fs::write(&caller_path, CALLER).unwrap();
    fs::write(&mover_path, MOVER).unwrap();
    fs::write(&mover_user_path, MOVER_USER).unwrap();
    fs::write(&param_path, PARAM).unwrap();
    let project = JavaProject::open(config, Trust::Trusted).unwrap();
    let mut status = project.status();
    until(&mut status, |status| status.state == State::Ready).await;
    let server = project.server().unwrap();

    let mut mover = Open {
        project: &project,
        uri: uri_of(&mover_path),
        text: MOVER.to_owned(),
        version: 1,
    };
    let mut mover_user = Open {
        project: &project,
        uri: uri_of(&mover_user_path),
        text: MOVER_USER.to_owned(),
        version: 1,
    };
    server.open(&mover.uri, 1, mover.text.as_str().into());
    server.open(&mover_user.uri, 1, mover_user.text.as_str().into());
    wait_error(&project, &mover.uri, false).await;
    wait_error(&project, &mover_user.uri, false).await;

    // F6 on a class moves its file into another package, updates its package line and
    // adds the import to the file that uses it.
    let destinations = first(ask(
        project.move_destinations().unwrap(),
        MoveQuery {
            spot: mover.spot("class Mover", 6),
            kind: MoveKind::File,
        },
    ))
    .await
    .unwrap();
    let destination = destinations
        .iter()
        .find(|destination| destination.name == "org.springframework.samples.petclinic.owner")
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                destinations
                    .iter()
                    .map(|destination| &destination.name)
                    .collect::<Vec<_>>()
            )
        })
        .clone();
    let outcome = first(ask(
        project.move_file().unwrap(),
        MoveRequest {
            spot: mover.spot("class Mover", 6),
            kind: MoveKind::File,
            destination,
            update_references: true,
        },
    ))
    .await
    .unwrap();
    assert!(outcome.error.is_none(), "{outcome:?}");
    assert!(!outcome.change.is_empty(), "{outcome:?}");
    assert!(!outcome.change.renames.is_empty(), "{outcome:?}");
    apply_change(&mut [&mut mover, &mut mover_user], &outcome.change);
    apply_renames(&mut [&mut mover, &mut mover_user], &outcome.change);
    assert!(
        mover
            .text
            .contains("package org.springframework.samples.petclinic.owner;"),
        "{}",
        mover.text
    );
    assert!(
        mover_user
            .text
            .contains("import org.springframework.samples.petclinic.owner.Mover;"),
        "{}",
        mover_user.text
    );
    wait_error(&project, &mover.uri, false).await;
    wait_error(&project, &mover_user.uri, false).await;

    // Ctrl+Alt+P on an expression adds it as a parameter of the method.
    let mut param = Open {
        project: &project,
        uri: uri_of(&param_path),
        text: PARAM.to_owned(),
        version: 1,
    };
    server.open(&param.uri, 1, param.text.as_str().into());
    wait_error(&project, &param.uri, false).await;
    let expression = param.span("value * 2");
    let introduce = param.actions(expression, Some(ActionKind::Refactor)).await;
    let parameter = introduce
        .iter()
        .find(|entry| entry.code.as_deref() == Some("refactor.introduce.parameter"))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                introduce
                    .iter()
                    .map(|entry| &entry.title)
                    .collect::<Vec<_>>()
            )
        });
    let before = param.text.clone();
    let edits = param.mine(parameter);
    assert!(!edits.is_empty(), "{parameter:?}");
    param.apply(edits);
    assert_ne!(param.text, before);
    wait_error(&project, &param.uri, false).await;

    let mut open = Open {
        project: &project,
        uri: uri_of(&probe),
        text: fs::read_to_string(&probe).unwrap(),
        version: 1,
    };
    let mut caller = Open {
        project: &project,
        uri: uri_of(&caller_path),
        text: CALLER.to_owned(),
        version: 1,
    };
    server.open(&open.uri, 1, open.text.as_str().into());
    server.open(&caller.uri, 1, caller.text.as_str().into());
    wait_error(&project, &caller.uri, false).await;

    // Alt+F7 on greet finds the call in the other file.
    let references = project.references().unwrap();
    let mut usages = Vec::new();
    for _ in 0..40 {
        usages = first(ask(references, open.spot("greet(String", 1)))
            .await
            .unwrap_or_default();
        if usages.iter().any(|usage| usage.uri == caller.uri) {
            break;
        }
        sleep(Duration::from_millis(250)).await;
    }
    assert!(
        usages.iter().any(|usage| usage.uri == caller.uri),
        "{usages:?}"
    );

    // Shift+F6 on the method changes both files.
    let rename = project.rename().unwrap();
    let query = RenameQuery {
        spot: open.spot("greet(String", 1),
        new_name: "welcome".to_owned(),
    };
    let change = first(ask(rename, query)).await.unwrap();
    apply_change(&mut [&mut open, &mut caller], &change);
    assert!(caller.text.contains(".welcome(\"x\")"), "{}", caller.text);
    assert!(open.text.contains("String welcome(String who)"));
    wait_error(&project, &caller.uri, false).await;

    // Ctrl+Alt+M on a statement: the method that comes out compiles.
    let statement = open.span("parts.add(who);");
    let extract = open.actions(statement, Some(ActionKind::Refactor)).await;
    let method = extract
        .iter()
        .find(|entry| entry.code.as_deref() == Some("refactor.extract.function"))
        .unwrap_or_else(|| panic!("{:?}", extract.iter().map(|a| &a.title).collect::<Vec<_>>()));
    let edits = open.mine(method);
    open.apply(edits);
    assert!(
        open.text.matches("parts.add(who)").count() == 1,
        "{}",
        open.text
    );
    assert!(open.text.contains("private void "), "{}", open.text);
    wait_error(&project, &open.uri, false).await;

    // Alt+Insert: toString of the fields compiles.
    let generate = open
        .actions(open.span("class Probe"), Some(ActionKind::Source))
        .await;
    let to_string = generate
        .iter()
        .find(|entry| entry.code.as_deref() == Some("source.generate.toString"))
        .unwrap_or_else(|| {
            panic!(
                "{:?}",
                generate.iter().map(|a| &a.title).collect::<Vec<_>>()
            )
        });
    let edits = open.mine(to_string);
    open.apply(edits);
    assert!(
        open.text.contains("public String toString()"),
        "{}",
        open.text
    );
    wait_error(&project, &open.uri, false).await;

    // Shift+F6 on the class renames its file and its uses.
    let query = RenameQuery {
        spot: open.spot("class Probe", 6),
        new_name: "Sample".to_owned(),
    };
    let change = first(ask(rename, query)).await.unwrap();
    assert_eq!(change.renames.len(), 1, "{change:?}");
    assert!(change.renames[0].to.ends_with("Sample.java"));
    apply_change(&mut [&mut open, &mut caller], &change);
    assert!(caller.text.contains("new Sample()"), "{}", caller.text);
    assert!(open.text.contains("public class Sample"));
    // The IDE saves both and moves the file, the server opens it at its new place.
    fs::write(&caller_path, &caller.text).unwrap();
    server.close(&open.uri);
    let sample = probe.with_file_name("Sample.java");
    fs::write(&sample, &open.text).unwrap();
    fs::remove_file(&probe).unwrap();
    let sample_uri = uri_of(&sample);
    server.open(&sample_uri, 1, open.text.as_str().into());
    open.uri = sample_uri.clone();
    open.version = 1;
    wait_error(&project, &sample_uri, false).await;
    wait_error(&project, &caller.uri, false).await;

    let moved = Uri::from_str(&mover.uri)
        .ok()
        .and_then(|uri| file_path(&uri));
    project.stop().await;
    // The next run writes the probe again.
    let _ = fs::remove_file(&sample);
    let _ = fs::remove_file(&caller_path);
    let _ = fs::remove_file(&mover_path);
    let _ = fs::remove_file(&mover_user_path);
    let _ = fs::remove_file(&param_path);
    if let Some(path) = moved {
        let _ = fs::remove_file(path);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs jdtls on a corpus project"]
async fn refactorings() {
    check_refactorings(fixture(
        "petclinic",
        ".",
        &["build.gradle", "settings.gradle"],
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs jdtls on a corpus project"]
async fn petclinic() {
    check_project(fixture(
        "petclinic",
        ".",
        &["build.gradle", "settings.gradle"],
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs jdtls on a corpus project"]
async fn apollo() {
    check_project(fixture("apollo", "apollo-common", &[])).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs jdtls on a corpus project"]
async fn dropping_the_project_ends_jdtls() {
    let fixture = fixture("petclinic", ".", &["build.gradle", "settings.gradle"]);
    let project = JavaProject::open(fixture.config, Trust::Trusted).unwrap();
    let mut status = project.status();
    let started = until(&mut status, |status| status.process_id.is_some()).await;
    let process_id = started.process_id.unwrap();
    drop(project);
    assert_ends(process_id).await;
}

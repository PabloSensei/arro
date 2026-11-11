//! Refactorings across the modules of a real multi-module project, without a window.
//!
//! cargo test -p java-lang --test multimodule -- --ignored --test-threads=1 --nocapture
//!
//! Needs `IDE_CORPUS`, the folder with `src`, `jdk` and `tools`, and
//! `MAVEN_USER_HOME` with a filled repository. Projects are copied, the corpus
//! stays untouched. A Gradle user home is taken from `GRADLE_USER_HOME`, or from
//! the `gradle` folder next to the Maven one.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::future::join_all;
use java_lang::{JavaProject, JdtlsConfig, Runtime, State};
use lang_api::{
    DiagnosticEntry, DiagnosticsQuery, DocumentEdit, DocumentSpot, FileChange, LanguageSupport,
    MoveKind, MoveQuery, MoveRequest, RenameQuery, Replies, Severity, SymbolQuery, TextPoint,
    TextSpan, WorkspaceChange, ask,
};
use lsp_client::lsp_types::{CodeActionKind, Position, Range, Uri, WorkspaceEdit};
use lsp_client::uri::{file_path, file_uri};
use lsp_client::utf16::{offset_at, position_at};
use platform::CanonPath;
use semantic_router::{Place, Published, workspace_change};
use serde_json::{Value, json};
use sidecar::{Status, Trust};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

/// The import of a reactor with Kotlin modules may take minutes, not seconds.
const READY: Duration = Duration::from_secs(900);
const ANSWER: Duration = Duration::from_secs(240);
const SKIPPED: &[&str] = &[".git", "target", "build", "bin", ".gradle", "node_modules"];

const SHARED: &str = r#"package com.ctrip.framework.apollo.common.records;

public class ApolloShared {

    public String label() {
        return "shared";
    }
}
"#;

const SHARED_USER: &str = r#"package com.ctrip.framework.apollo.biz.records;

import com.ctrip.framework.apollo.common.records.ApolloShared;

public class ApolloSharedUser {

    public String use() {
        return new ApolloShared().label();
    }
}
"#;

const MOVED: &str = r#"package com.ctrip.framework.apollo.common.moved;

public class MovedRecord {

    public String value() {
        return "moved";
    }
}
"#;

const MOVED_USER: &str = r#"package com.ctrip.framework.apollo.biz.moved;

import com.ctrip.framework.apollo.common.moved.MovedRecord;

public class MovedRecordUser {

    public String use() {
        return new MovedRecord().value();
    }
}
"#;

const SIGNATURE_TARGET: &str = r#"package com.ctrip.framework.apollo.common.sign;

public class SignatureTarget {

    public String combine(String left, String right) {
        return left + right;
    }
}
"#;

const SIGNATURE_CALLER: &str = r#"package com.ctrip.framework.apollo.biz.sign;

import com.ctrip.framework.apollo.common.sign.SignatureTarget;

public class SignatureCaller {

    public String use() {
        return new SignatureTarget().combine("a", "b");
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

/// A Gradle user home that the environment prepared next to the Maven one.
fn gradle_home() -> Option<PathBuf> {
    if let Ok(home) = std::env::var("GRADLE_USER_HOME") {
        return Some(PathBuf::from(home));
    }
    let maven = PathBuf::from(std::env::var("MAVEN_USER_HOME").ok()?);
    let gradle = maven.parent()?.join("gradle");
    gradle.is_dir().then_some(gradle)
}

/// The root build applies `nebula.netflixoss` 13.0.0, whose publishing plugin
/// fails under the Gradle 9.7.1 that Buildship runs in this environment with
/// `Task with name 'postRelease' not found`. The copy drops only that publishing
/// plugin; the rest of the reactor, its Java source sets and dependencies stay.
fn patch_for_import(project: &str, root: &Path) {
    if project != "dgs-framework" {
        return;
    }
    let build = root.join("build.gradle.kts");
    let text = fs::read_to_string(&build).unwrap();
    let patched = text
        .replace("    id(\"nebula.netflixoss\") version \"13.0.0\"\n", "")
        .replace("    apply(plugin = \"nebula.netflixoss\")\n", "");
    assert_ne!(text, patched, "the nebula plugin lines moved");
    fs::write(&build, patched).unwrap();
}

struct Fixture {
    root: PathBuf,
    config: JdtlsConfig,
}

/// A private copy of a project with `extra` files, and a clean jdtls state.
fn fixture(project: &str, name: &str, extra: &[(&str, &str)]) -> Fixture {
    let corpus = corpus();
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("multimodule")
        .join(name);
    let root = scratch.join("project");
    // A previous run may have renamed or moved files; a copy is the only clean start.
    let _ = fs::remove_dir_all(&root);
    copy_tree(&corpus.join("src").join(project), &root, false);
    patch_for_import(project, &root);
    for (path, text) in extra {
        let path = root.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    }
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
    fs::write(
        &settings,
        format!("<settings><localRepository>{repository}</localRepository><offline>true</offline></settings>"),
    )
    .unwrap();

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
        gradle_user_home: gradle_home(),
        gradle_java_home: Some(fs::canonicalize(corpus.join("jdk").join("17")).unwrap()),
        // The Gradle reactor needs one plugin artifact that the prepared cache may not
        // hold yet; only the Maven reactor stays offline.
        offline: project == "apollo",
        environment: HashMap::new(),
    };
    Fixture { root, config }
}

async fn until(status: &mut watch::Receiver<Status>, test: impl Fn(&Status) -> bool) -> Status {
    let found = timeout(
        READY,
        status.wait_for(|status| {
            test(status) || matches!(status.state, State::Stopped | State::Degraded)
        }),
    )
    .await
    .expect("jdtls did not get there in time")
    .unwrap()
    .clone();
    assert!(
        test(&found),
        "jdtls stopped before it was ready: {:?} {:?}",
        found.state,
        found.last_error
    );
    found
}

/// The first answer, or `None` when the provider stays silent.
async fn first<T>(replies: Replies<T>) -> Option<T> {
    let mut replies = replies;
    timeout(ANSWER, replies.next())
        .await
        .expect("no answer in time")
        .map(|answer| answer.value)
}

fn uri_of(path: &Path) -> String {
    file_uri(&CanonPath::new(path).unwrap())
        .unwrap()
        .as_str()
        .to_owned()
}

fn path_of(uri: &str) -> PathBuf {
    file_path(&Uri::from_str(uri).unwrap()).unwrap()
}

struct Doc<'a> {
    project: &'a JavaProject,
    uri: String,
    text: String,
    version: i32,
}

impl<'a> Doc<'a> {
    fn open(project: &'a JavaProject, uri: String) -> Self {
        let path = path_of(&uri);
        let text = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("cannot read {}: {error}", path.display()));
        project
            .server()
            .unwrap()
            .open(&uri, 1, text.as_str().into());
        Self {
            project,
            uri,
            text,
            version: 1,
        }
    }

    fn point(&self, offset: usize) -> TextPoint {
        let position = position_at(&self.text, offset);
        TextPoint {
            line: position.line,
            character: position.character,
        }
    }

    fn offset(&self, point: TextPoint) -> usize {
        offset_at(&self.text, Position::new(point.line, point.character))
    }

    fn spot(&self, needle: &str, shift: usize) -> DocumentSpot {
        let offset = self.text.find(needle).unwrap() + shift;
        DocumentSpot {
            uri: self.uri.clone(),
            version: self.version,
            point: self.point(offset),
        }
    }

    fn span(&self, needle: &str) -> TextSpan {
        let start = self.text.find(needle).unwrap();
        TextSpan {
            start: self.point(start),
            end: self.point(start + needle.len()),
        }
    }

    /// Server edits, all in the coordinates of the current text, applied as the
    /// editor does: from the last one back, one change each.
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

/// The documents of a change, opened and kept in step with the server.
struct Docs<'a> {
    project: &'a JavaProject,
    docs: HashMap<String, Doc<'a>>,
}

impl<'a> Docs<'a> {
    fn new(project: &'a JavaProject) -> Self {
        Self {
            project,
            docs: HashMap::new(),
        }
    }

    fn open(&mut self, uri: &str) {
        if !self.docs.contains_key(uri) {
            let doc = Doc::open(self.project, uri.to_owned());
            self.docs.insert(uri.to_owned(), doc);
        }
    }

    fn doc(&self, uri: &str) -> &Doc<'a> {
        self.docs.get(uri).expect("the document was not opened")
    }

    fn text(&self, uri: &str) -> &str {
        &self.doc(uri).text
    }

    /// Applies the edits and the file moves, writes every touched file and tells the
    /// server about it. Returns the files in their places after the change.
    fn apply(&mut self, change: &WorkspaceChange) -> Vec<String> {
        let mut batches: Vec<(String, Vec<DocumentEdit>)> = Vec::new();
        for source in &change.edits {
            if source.edits.is_empty() {
                continue;
            }
            self.open(&source.uri);
            match batches.iter_mut().find(|(uri, _)| *uri == source.uri) {
                Some((_, edits)) => edits.extend(source.edits.iter().cloned()),
                None => batches.push((source.uri.clone(), source.edits.clone())),
            }
        }
        for (uri, edits) in batches {
            self.docs.get_mut(&uri).unwrap().apply(edits);
        }

        for rename in &change.renames {
            self.open(&rename.from);
            let from = path_of(&rename.from);
            let to = path_of(&rename.to);
            fs::create_dir_all(to.parent().unwrap()).unwrap();
            let mut doc = self.docs.remove(&rename.from).unwrap();
            fs::write(&to, &doc.text).unwrap();
            fs::remove_file(&from).unwrap();
            doc.project.server().unwrap().close(&doc.uri);
            doc.uri = rename.to.clone();
            doc.version += 1;
            doc.project
                .server()
                .unwrap()
                .open(&doc.uri, doc.version, doc.text.as_str().into());
            self.docs.insert(rename.to.clone(), doc);
        }

        for doc in self.docs.values() {
            fs::write(path_of(&doc.uri), &doc.text).unwrap();
            doc.project.server().unwrap().save(&doc.uri);
        }
        self.docs.keys().cloned().collect()
    }

    fn spot(&self, uri: &str, needle: &str, shift: usize) -> DocumentSpot {
        self.doc(uri).spot(needle, shift)
    }

    fn span(&self, uri: &str, needle: &str) -> TextSpan {
        self.doc(uri).span(needle)
    }
}

/// The latest diagnostics of every file for `window`.
async fn collect_diagnostics(
    project: &JavaProject,
    uris: Vec<String>,
    window: Duration,
) -> HashMap<String, Vec<DiagnosticEntry>> {
    let provider = project.diagnostics().unwrap();
    let tasks = uris.into_iter().map(|uri| {
        let mut replies = ask(provider, DiagnosticsQuery(uri.clone()));
        async move {
            let mut latest = Vec::new();
            let _ = timeout(window, async {
                while let Some(answer) = replies.next().await {
                    latest = answer.value;
                }
            })
            .await;
            (uri, latest)
        }
    });
    join_all(tasks).await.into_iter().collect()
}

fn errors(lists: &HashMap<String, Vec<DiagnosticEntry>>) -> Vec<String> {
    let mut errors = Vec::new();
    for (uri, list) in lists {
        for entry in list
            .iter()
            .filter(|entry| entry.severity == Severity::Error)
        {
            errors.push(format!("{uri}: {} {:?}", entry.message, entry.span));
        }
    }
    errors
}

/// Waits until the server has built the files and reports no errors. Diagnostics
/// arrive after the answer, so every round listens for a while and two clean rounds
/// in a row are needed.
async fn wait_clean(project: &JavaProject, uris: &[String], rounds: usize) {
    let mut last = HashMap::new();
    let mut clean = 0;
    for _ in 0..rounds {
        last = collect_diagnostics(project, uris.to_vec(), Duration::from_secs(3)).await;
        if errors(&last).is_empty() {
            clean += 1;
            if clean == 2 {
                return;
            }
        } else {
            clean = 0;
        }
        sleep(Duration::from_millis(500)).await;
    }
    panic!("errors after the change:\n{}", errors(&last).join("\n"));
}

/// Refactor code actions the server lists for `span`, as the window asks them.
async fn refactor_actions(
    project: &JavaProject,
    uri: &str,
    span: TextSpan,
    version: i32,
) -> Vec<lsp_client::lsp_types::CodeActionOrCommand> {
    let (published, _) = Published::new();
    let place = Place {
        uri: Uri::from_str(uri).unwrap(),
        version,
        position: Position::new(span.start.line, span.start.character),
    };
    let range = Range::new(
        Position::new(span.start.line, span.start.character),
        Position::new(span.end.line, span.end.character),
    );
    project
        .router()
        .code_actions(
            place,
            range,
            &published,
            Some(vec![CodeActionKind::REFACTOR]),
        )
        .await
        .ok()
        .flatten()
        .unwrap_or_default()
}

/// The code action context a `java/getChangeSignatureInfo` request takes.
fn code_action_context(uri: &str, span: TextSpan) -> Value {
    json!({
        "textDocument": {"uri": uri},
        "range": {
            "start": {"line": span.start.line, "character": span.start.character},
            "end": {"line": span.end.line, "character": span.end.character},
        },
        "context": {"diagnostics": []},
    })
}

/// Waits for the action of Change Signature and its info, then asks for the edits
/// that add an `int times` parameter with the default `1`.
async fn change_signature(
    project: &JavaProject,
    uri: &str,
    span: TextSpan,
    version: i32,
) -> WorkspaceChange {
    let mut offered = false;
    for _ in 0..40 {
        let actions = refactor_actions(project, uri, span, version).await;
        offered = actions.iter().any(|action| match action {
            lsp_client::lsp_types::CodeActionOrCommand::CodeAction(action) => action
                .kind
                .as_ref()
                .is_some_and(|kind| kind.as_str() == "refactor.change.signature"),
            lsp_client::lsp_types::CodeActionOrCommand::Command(_) => false,
        });
        if offered {
            break;
        }
        sleep(Duration::from_millis(250)).await;
    }
    assert!(offered, "jdtls offered no change signature action");

    let context = code_action_context(uri, span);
    let mut info = Value::Null;
    for _ in 0..40 {
        let reply = project
            .router()
            .extension("java/getChangeSignatureInfo", context.clone())
            .await
            .expect("jdtls answers the change signature info");
        if reply
            .get("methodIdentifier")
            .and_then(Value::as_str)
            .is_some()
        {
            info = reply;
            break;
        }
        sleep(Duration::from_millis(250)).await;
    }
    assert!(
        info.get("methodIdentifier")
            .and_then(Value::as_str)
            .is_some(),
        "no change signature info: {info}"
    );

    let mut parameters = info["parameters"].as_array().cloned().unwrap_or_default();
    let index = parameters.len();
    parameters.push(json!({
        "type": "int",
        "name": "times",
        "defaultValue": "1",
        "originalIndex": index,
    }));
    let arguments = json!([
        info["methodIdentifier"].clone(),
        false,
        info["methodName"].clone(),
        info["modifier"].clone(),
        info["returnType"].clone(),
        parameters,
        info["exceptions"].clone(),
        true
    ]);
    let params = json!({
        "command": "changeSignature",
        "commandArguments": arguments,
        "context": context,
        "options": {"tabSize": 4, "insertSpaces": true},
    });
    let reply = project
        .router()
        .extension("java/getRefactorEdit", params)
        .await
        .expect("jdtls answers the change signature refactoring");
    if let Some(error) = reply.get("errorMessage").and_then(Value::as_str) {
        panic!("change signature failed: {error}");
    }
    let edit: WorkspaceEdit = serde_json::from_value(reply["edit"].clone())
        .expect("the change signature refactoring carries an edit");
    workspace_change(edit)
}

async fn apollo() {
    let module = |path: &str| {
        format!("apollo-common/src/main/java/com/ctrip/framework/apollo/common/{path}")
    };
    let biz =
        |path: &str| format!("apollo-biz/src/main/java/com/ctrip/framework/apollo/biz/{path}");
    let fixture = fixture(
        "apollo",
        "apollo",
        &[
            (&module("records/ApolloShared.java"), SHARED),
            (&biz("records/ApolloSharedUser.java"), SHARED_USER),
            (&module("moved/MovedRecord.java"), MOVED),
            (&biz("moved/MovedRecordUser.java"), MOVED_USER),
            (&module("sign/SignatureTarget.java"), SIGNATURE_TARGET),
            (&biz("sign/SignatureCaller.java"), SIGNATURE_CALLER),
        ],
    );
    let root = fixture.root.clone();
    let started = Instant::now();
    let project = JavaProject::open(fixture.config, Trust::Trusted).unwrap();
    let mut status = project.status();
    let ready = until(&mut status, |status| status.state == State::Ready).await;
    assert_eq!(ready.generation, 1);
    eprintln!("apollo: ready in {:?}", started.elapsed());

    let shared = uri_of(&root.join(module("records/ApolloShared.java")));
    let shared_user = uri_of(&root.join(biz("records/ApolloSharedUser.java")));
    let moved = uri_of(&root.join(module("moved/MovedRecord.java")));
    let moved_new = root.join(biz("moved/MovedRecord.java"));
    let moved_user = uri_of(&root.join(biz("moved/MovedRecordUser.java")));
    let signature = uri_of(&root.join(module("sign/SignatureTarget.java")));
    let signature_caller = uri_of(&root.join(biz("sign/SignatureCaller.java")));

    let mut docs = Docs::new(&project);
    for uri in [
        &shared,
        &shared_user,
        &moved,
        &moved_user,
        &signature,
        &signature_caller,
    ] {
        docs.open(uri);
    }
    wait_clean(
        &project,
        &[
            shared.clone(),
            shared_user.clone(),
            moved.clone(),
            moved_user.clone(),
            signature.clone(),
            signature_caller.clone(),
        ],
        30,
    )
    .await;

    // Alt+F7 on a class of another module.
    let references = project.references().unwrap();
    let mut usages = Vec::new();
    for _ in 0..40 {
        usages = first(ask(references, docs.spot(&shared, "class ApolloShared", 6)))
            .await
            .unwrap_or_default();
        if usages.iter().any(|usage| usage.uri == shared_user) {
            break;
        }
        sleep(Duration::from_millis(250)).await;
    }
    assert!(
        usages.iter().any(|usage| usage.uri == shared_user),
        "no usage in another module: {usages:?}"
    );

    // Shift+F6 renames the class and its file, and the use in the other module.
    let rename = project.rename().unwrap();
    let change = first(ask(
        rename,
        RenameQuery {
            spot: docs.spot(&shared, "class ApolloShared", 6),
            new_name: "ApolloRecord".to_owned(),
        },
    ))
    .await
    .expect("a rename answer");
    assert_eq!(change.renames.len(), 1, "{change:?}");
    assert!(change.renames[0].to.ends_with("ApolloRecord.java"));
    let touched = docs.apply(&change);
    let shared_new = uri_of(&root.join(module("records/ApolloRecord.java")));
    assert!(root.join(module("records/ApolloRecord.java")).exists());
    assert!(!root.join(module("records/ApolloShared.java")).exists());
    assert!(docs.text(&shared_new).contains("public class ApolloRecord"));
    assert!(
        docs.text(&shared_user)
            .contains("import com.ctrip.framework.apollo.common.records.ApolloRecord;"),
        "{}",
        docs.text(&shared_user)
    );
    let shared_user_on_disk =
        fs::read_to_string(root.join(biz("records/ApolloSharedUser.java"))).unwrap();
    assert!(
        shared_user_on_disk
            .contains("import com.ctrip.framework.apollo.common.records.ApolloRecord;"),
        "{shared_user_on_disk}"
    );
    assert!(docs.text(&shared_user).contains("new ApolloRecord()"));
    wait_clean(&project, &touched, 30).await;

    // F6 moves the class into a package of the module that uses it.
    let destinations = first(ask(
        project.move_destinations().unwrap(),
        MoveQuery {
            spot: docs.spot(&moved, "class MovedRecord", 6),
            kind: MoveKind::File,
        },
    ))
    .await
    .expect("a move destination answer");
    let destination = destinations
        .iter()
        .find(|destination| {
            destination.name == "com.ctrip.framework.apollo.biz.moved"
                && destination
                    .detail
                    .as_deref()
                    .is_some_and(|detail| detail.contains("apollo-biz"))
        })
        .unwrap_or_else(|| {
            panic!(
                "jdtls listed no package of another module: {:?}",
                destinations
                    .iter()
                    .map(|destination| (&destination.name, &destination.detail))
                    .collect::<Vec<_>>()
            )
        })
        .clone();
    let outcome = first(ask(
        project.move_file().unwrap(),
        MoveRequest {
            spot: docs.spot(&moved, "class MovedRecord", 6),
            kind: MoveKind::File,
            destination,
            update_references: true,
        },
    ))
    .await
    .expect("a move answer");
    assert!(outcome.error.is_none(), "{outcome:?}");
    assert!(!outcome.change.is_empty(), "{outcome:?}");
    assert!(!outcome.change.renames.is_empty(), "{outcome:?}");
    let touched = docs.apply(&outcome.change);
    assert!(moved_new.exists(), "the moved file is not on disk");
    assert!(!root.join(module("moved/MovedRecord.java")).exists());
    assert!(
        !docs
            .text(&moved_user)
            .contains("com.ctrip.framework.apollo.common.moved"),
        "{}",
        docs.text(&moved_user)
    );
    assert!(docs.text(&moved_user).contains("new MovedRecord()"));
    let moved_user_on_disk =
        fs::read_to_string(root.join(biz("moved/MovedRecordUser.java"))).unwrap();
    assert!(
        !moved_user_on_disk.contains("com.ctrip.framework.apollo.common.moved"),
        "{moved_user_on_disk}"
    );
    wait_clean(&project, &touched, 30).await;

    // Change Signature adds a parameter to a method called from another module.
    // The window has no dialog for it yet, so the test sends the two requests a
    // client would send and applies the answer like any other workspace change.
    let span = docs.span(
        &signature,
        "public String combine(String left, String right)",
    );
    let change = change_signature(&project, &signature, span, docs.doc(&signature).version).await;
    let touched = docs.apply(&change);
    assert!(
        docs.text(&signature)
            .contains("combine(String left, String right, int times)"),
        "{}",
        docs.text(&signature)
    );
    assert!(
        docs.text(&signature_caller)
            .contains("combine(\"a\", \"b\", 1)"),
        "{}",
        docs.text(&signature_caller)
    );
    let signature_on_disk =
        fs::read_to_string(root.join(module("sign/SignatureTarget.java"))).unwrap();
    assert!(
        signature_on_disk.contains("combine(String left, String right, int times)"),
        "{signature_on_disk}"
    );
    wait_clean(&project, &touched, 30).await;

    drop(docs);
    project.stop().await;
    eprintln!("apollo: test took {:?}", started.elapsed());
}

const DGS_SHARED: &str = r#"package com.netflix.graphql.dgs.example.shared;

import com.netflix.graphql.dgs.DgsCodeRegistry;
import com.netflix.graphql.dgs.DgsComponent;

@DgsComponent
public class SharedCodeRegistryUser {

    @DgsCodeRegistry
    public Object registry() {
        return null;
    }
}
"#;

async fn dgs() {
    let fixture = fixture(
        "dgs-framework",
        "dgs",
        &[(
            "graphql-dgs-example-shared/src/main/java/com/netflix/graphql/dgs/example/shared/SharedCodeRegistryUser.java",
            DGS_SHARED,
        )],
    );
    let root = fixture.root.clone();
    let started = Instant::now();
    let project = JavaProject::open(fixture.config, Trust::Trusted).unwrap();
    let mut status = project.status();
    let ready = until(&mut status, |status| status.state == State::Ready).await;
    assert_eq!(ready.generation, 1);
    eprintln!("dgs: ready in {:?}", started.elapsed());

    let registry = uri_of(
        &root.join("graphql-dgs/src/main/java/com/netflix/graphql/dgs/DgsCodeRegistry.java"),
    );
    let user = uri_of(&root.join(
        "graphql-dgs-example-shared/src/main/java/com/netflix/graphql/dgs/example/shared/SharedCodeRegistryUser.java",
    ));
    let mut docs = Docs::new(&project);
    docs.open(&registry);
    docs.open(&user);
    wait_clean(&project, &[registry.clone(), user.clone()], 40).await;

    // Alt+F7 finds the annotation in another module.
    let references = project.references().unwrap();
    let mut usages = Vec::new();
    for _ in 0..40 {
        usages = first(ask(
            references,
            docs.spot(&registry, "interface DgsCodeRegistry", 10),
        ))
        .await
        .unwrap_or_default();
        if usages.iter().any(|usage| usage.uri == user) {
            break;
        }
        sleep(Duration::from_millis(250)).await;
    }
    assert!(
        usages.iter().any(|usage| usage.uri == user),
        "no usage in another module: {usages:?}"
    );

    // Shift+F6 renames the annotation and its uses in the example module.
    let change = first(ask(
        project.rename().unwrap(),
        RenameQuery {
            spot: docs.spot(&registry, "interface DgsCodeRegistry", 10),
            new_name: "DgsCodeRegistryScheme".to_owned(),
        },
    ))
    .await
    .expect("a rename answer");
    assert_eq!(change.renames.len(), 1, "{change:?}");
    let touched = docs.apply(&change);
    let registry_new = uri_of(
        &root.join("graphql-dgs/src/main/java/com/netflix/graphql/dgs/DgsCodeRegistryScheme.java"),
    );
    assert!(
        docs.text(&registry_new)
            .contains("public @interface DgsCodeRegistryScheme")
    );
    assert!(
        root.join("graphql-dgs/src/main/java/com/netflix/graphql/dgs/DgsCodeRegistryScheme.java")
            .exists()
    );
    assert!(
        docs.text(&user)
            .contains("import com.netflix.graphql.dgs.DgsCodeRegistryScheme;"),
        "{}",
        docs.text(&user)
    );
    assert!(docs.text(&user).contains("@DgsCodeRegistryScheme"));
    let user_on_disk = fs::read_to_string(root.join(
        "graphql-dgs-example-shared/src/main/java/com/netflix/graphql/dgs/example/shared/SharedCodeRegistryUser.java",
    ))
    .unwrap();
    assert!(
        user_on_disk.contains("import com.netflix.graphql.dgs.DgsCodeRegistryScheme;"),
        "{user_on_disk}"
    );
    wait_clean(&project, &touched, 40).await;

    drop(docs);
    project.stop().await;
    eprintln!("dgs: test took {:?}", started.elapsed());
}

/// A Maven reactor of `modules` modules with one empty class in each, in a scratch folder.
fn maven_reactor(name: &str, modules: &[&str]) -> Fixture {
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("multimodule")
        .join(name);
    let root = scratch.join("project");
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap();
    let listed: String = modules
        .iter()
        .map(|module| {
            format!(
                "        <module>{module}</module>
"
            )
        })
        .collect();
    fs::write(
        root.join("pom.xml"),
        format!(
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <groupId>demo</groupId>
    <artifactId>parent</artifactId>
    <version>1.0</version>
    <packaging>pom</packaging>
    <modules>
{listed}    </modules>
    <properties><maven.compiler.release>17</maven.compiler.release></properties>
</project>
"#
        ),
    )
    .unwrap();
    for module in modules {
        write_module(&root, module);
    }

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
    fs::write(
        &settings,
        format!("<settings><localRepository>{repository}</localRepository><offline>true</offline></settings>"),
    )
    .unwrap();
    let corpus = corpus();
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
            runtime("JavaSE-17", "17", true),
            runtime("JavaSE-25", "25", false),
        ],
        maven_settings: Some(settings),
        gradle_user_home: gradle_home(),
        gradle_java_home: Some(fs::canonicalize(corpus.join("jdk").join("17")).unwrap()),
        offline: true,
        environment: HashMap::new(),
    };
    Fixture { root, config }
}

/// One module of the scratch reactor: a POM that names the parent and a class of its own.
fn write_module(root: &Path, module: &str) {
    let dir = root.join(module);
    fs::create_dir_all(dir.join("src/main/java/demo").join(module)).unwrap();
    fs::write(
        dir.join("pom.xml"),
        format!(
            r#"<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <parent>
        <groupId>demo</groupId>
        <artifactId>parent</artifactId>
        <version>1.0</version>
        <relativePath>../pom.xml</relativePath>
    </parent>
    <artifactId>{module}</artifactId>
</project>
"#
        ),
    )
    .unwrap();
    let class = if module == "one" {
        "One"
    } else if module == "two" {
        "Two"
    } else {
        "Three"
    };
    fs::write(
        dir.join("src/main/java/demo")
            .join(module)
            .join(format!("{class}.java")),
        format!(
            "package demo.{module};

public class {class} {{
}}
"
        ),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs jdtls on a multi-module corpus project"]
async fn apollo_multimodule() {
    apollo().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs jdtls on a multi-module corpus project"]
async fn dgs_gradle_multimodule() {
    dgs().await;
}

/// A module added to a Maven reactor: the new POM and the changed root POM reach the
/// server as a created file and a changed one, and the class of the module is then
/// visible.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "runs jdtls on a scratch Maven reactor"]
async fn a_created_module_pom_is_imported() {
    let fixture = maven_reactor("maven-new-module", &["one", "two"]);
    let root = fixture.root.clone();
    let project = JavaProject::open(fixture.config, Trust::Trusted).unwrap();
    let mut status = project.status();
    let ready = until(&mut status, |status| status.state == State::Ready).await;
    assert_eq!(ready.generation, 1);

    // The new module appears on disk, and the root POM names it.
    write_module(&root, "three");
    let root_pom = root.join("pom.xml");
    let text = fs::read_to_string(&root_pom).unwrap();
    let text = text.replace(
        "        <module>two</module>
",
        "        <module>two</module>
        <module>three</module>
",
    );
    fs::write(&root_pom, text).unwrap();

    let server = project.server().unwrap();
    server.file_changed(&uri_of(&root_pom), FileChange::Changed);
    server.file_changed(&uri_of(&root.join("three/pom.xml")), FileChange::Created);
    let third = root.join("three/src/main/java/demo/three");
    let symbols = project.workspace_symbols().unwrap();
    let mut last = Vec::new();
    for _ in 0..90 {
        last = first(ask(symbols, SymbolQuery("Three".into())))
            .await
            .unwrap_or_default();
        if last.iter().any(|entry| {
            entry
                .place
                .as_ref()
                .is_some_and(|place| path_of(&place.uri).starts_with(&third))
        }) {
            break;
        }
        sleep(Duration::from_secs(1)).await;
    }
    assert!(
        last.iter().any(|entry| {
            entry
                .place
                .as_ref()
                .is_some_and(|place| path_of(&place.uri).starts_with(&third))
        }),
        "the class of the new module is not in the workspace: {last:?}"
    );

    project.stop().await;
}

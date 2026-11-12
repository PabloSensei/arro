//! Change Signature through the language capability, on a real server and a real
//! multi-module project.
//!
//! cargo test -p java-lang --test change_signature -- --ignored --test-threads=1 --nocapture
//!
//! Needs `IDE_CORPUS`, the folder with `src`, `jdk` and `tools`, and
//! `MAVEN_USER_HOME` with a filled repository. The project is copied, the corpus stays
//! untouched.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::future::join_all;
use java_lang::{JavaProject, JdtlsConfig, Runtime, State};
use lang_api::{
    ChangeSignatureRequest, Confidence, DiagnosticEntry, DiagnosticsQuery, DocumentEdit,
    DocumentSpot, LanguageSupport, MethodSignature, Replies, Severity, SignatureParameter,
    TextPoint, TextSpan, ask,
};
use lsp_client::lsp_types::Position;
use lsp_client::uri::{file_path, file_uri};
use lsp_client::utf16::{offset_at, position_at};
use platform::CanonPath;
use sidecar::{Status, Trust};
use tokio::sync::watch;
use tokio::time::{sleep, timeout};

const READY: Duration = Duration::from_secs(900);
const ANSWER: Duration = Duration::from_secs(240);
const SKIPPED: &[&str] = &[".git", "target", "build", "bin", ".gradle", "node_modules"];

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

struct Fixture {
    root: PathBuf,
    config: JdtlsConfig,
}

/// A private copy of a project with `extra` files, and a clean jdtls state.
fn fixture(project: &str, name: &str, extra: &[(&str, &str)]) -> Fixture {
    let corpus = corpus();
    let scratch = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("change_signature")
        .join(name);
    let root = scratch.join("project");
    let _ = fs::remove_dir_all(&root);
    copy_tree(&corpus.join("src").join(project), &root, false);
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
        format!(
            "<settings><localRepository>{repository}</localRepository><offline>true</offline></settings>"
        ),
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
        gradle_home: None,
        gradle_user_home: gradle_home(),
        gradle_java_home: Some(fs::canonicalize(corpus.join("jdk").join("17")).unwrap()),
        offline: true,
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

/// The first answer that carries a signature, or the last answer seen.
async fn first_signature(
    mut replies: Replies<lang_api::ChangeSignatureInfo>,
) -> lang_api::ChangeSignatureInfo {
    let mut last = lang_api::ChangeSignatureInfo::default();
    let deadline = Instant::now() + ANSWER;
    while Instant::now() < deadline {
        match timeout(Duration::from_secs(5), replies.next()).await {
            Ok(Some(answer)) => {
                last = answer.value;
                if last.signature.is_some() || last.error.is_some() {
                    return last;
                }
            }
            Ok(None) => return last,
            Err(_) => {}
        }
    }
    last
}

fn uri_of(path: &Path) -> String {
    file_uri(&CanonPath::new(path).unwrap())
        .unwrap()
        .as_str()
        .to_owned()
}

fn path_of(uri: &str) -> PathBuf {
    file_path(&lsp_client::lsp_types::Uri::from_str(uri).unwrap()).unwrap()
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

    fn apply(&mut self, change: &lang_api::WorkspaceChange) -> Vec<String> {
        for source in &change.edits {
            if source.edits.is_empty() {
                continue;
            }
            self.open(&source.uri);
            self.docs
                .get_mut(&source.uri)
                .unwrap()
                .apply(source.edits.clone());
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

/// Waits until the server reports no errors in two rounds in a row.
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

#[tokio::test]
#[ignore = "starts jdtls on a copy of a real project"]
async fn change_signature_adds_and_reorders_parameters() {
    let module = |path: &str| {
        format!("apollo-common/src/main/java/com/ctrip/framework/apollo/common/{path}")
    };
    let biz =
        |path: &str| format!("apollo-biz/src/main/java/com/ctrip/framework/apollo/biz/{path}");
    let fixture = fixture(
        "apollo",
        "apollo",
        &[
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

    let target = uri_of(&root.join(module("sign/SignatureTarget.java")));
    let caller = uri_of(&root.join(biz("sign/SignatureCaller.java")));
    let mut docs = Docs::new(&project);
    docs.open(&target);
    docs.open(&caller);
    wait_clean(&project, &[target.clone(), caller.clone()], 30).await;

    let spot = docs.spot(&target, "public String combine", 15);
    let info_provider = project
        .change_signature_info()
        .expect("the provider exists");
    let info = first_signature(ask(info_provider, spot.clone())).await;
    assert!(
        info.error.is_none(),
        "the server could not describe the signature: {:?}",
        info.error
    );
    let signature = info.signature.expect("a signature");
    assert_eq!(signature.name, "combine");
    assert_eq!(signature.return_type, "String");
    assert_eq!(signature.parameters.len(), 2, "{signature:?}");

    // Add `int times` with the default `1`, then move it before the old parameters.
    let mut parameters = signature.parameters.clone();
    parameters.insert(
        0,
        SignatureParameter {
            type_name: "int".to_owned(),
            name: "times".to_owned(),
            default_value: "1".to_owned(),
            original_index: None,
        },
    );
    let edited = MethodSignature {
        name: "combine".to_owned(),
        visibility: signature.visibility.clone(),
        return_type: "String".to_owned(),
        parameters,
        exceptions: signature.exceptions.clone(),
    };
    let provider = project.change_signature().expect("the provider exists");
    let mut replies = ask(
        provider,
        ChangeSignatureRequest {
            spot,
            signature: edited,
        },
    );
    let answer = timeout(ANSWER, replies.next())
        .await
        .expect("no answer in time")
        .expect("an answer");
    assert_eq!(answer.confidence, Confidence::Certain);
    assert!(
        answer.value.error.is_none(),
        "the server refused the change: {:?}",
        answer.value.error
    );
    let change = answer.value.change;
    assert!(!change.is_empty(), "the change has edits");
    let touched = docs.apply(&change);

    let target_text = &docs.doc(&target).text;
    assert!(
        target_text.contains("combine(int times, String left, String right)"),
        "{target_text}"
    );
    let caller_text = &docs.doc(&caller).text;
    assert!(
        caller_text.contains("combine(1, \"a\", \"b\")"),
        "the call gets the default of the added parameter: {caller_text}"
    );
    let target_on_disk =
        fs::read_to_string(root.join(module("sign/SignatureTarget.java"))).unwrap();
    assert!(
        target_on_disk.contains("combine(int times, String left, String right)"),
        "{target_on_disk}"
    );
    let caller_on_disk = fs::read_to_string(root.join(biz("sign/SignatureCaller.java"))).unwrap();
    assert!(
        caller_on_disk.contains("combine(1, \"a\", \"b\")"),
        "{caller_on_disk}"
    );
    wait_clean(&project, &touched, 30).await;

    drop(docs);
    project.stop().await;
    eprintln!("apollo: change signature test took {:?}", started.elapsed());
}

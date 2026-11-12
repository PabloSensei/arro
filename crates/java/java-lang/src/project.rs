use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use base::Cancel;
use futures_util::StreamExt;
use jdtls_adapter::{ImportFailure, Jdtls, JdtlsConfig};
use lang_api::{
    Answer, Confidence, DefinitionPreview, DefinitionPreviewQuery, DocumentSpot, ExternalDoc,
    FileSymbol, FileSymbolsQuery, GenerateApplyRequest, GenerateChoice, GenerateKind,
    GenerateQuery, GeneratedFile, Generation, HierarchyDirection, HierarchyQuery, HoverProvider,
    MethodHierarchyEntry, MethodHierarchyQuery, MethodRelation, PrepareDiagnosticsQuery,
    ProjectProblem, ProjectProblemsQuery, Provider, ReferenceQuery, Reply, Severity, SourceId,
    SourceSpan, SourceTextQuery, SymbolEntry, SymbolKind, TextSpan, ask, offset_in,
};
use lsp_client::ServerEvents;
use lsp_client::lsp_types::{
    DocumentSymbol, DocumentSymbolResponse, SymbolKind as LspSymbolKind, Uri,
};
use lsp_client::uri::file_path;
use semantic_router::{LspProviders, PromptSource, Published, RouteError, Router};
use serde_json::json;
use sidecar::{Launcher, Limits, NotTrusted, Sidecar, Status, SupervisedLauncher, Trust};
use tokio::runtime::Handle;
use tokio::sync::watch;
use tokio::time::sleep;

use crate::debug::DebugExtension;
use crate::language::{JavaSyntax, LANGUAGE_ID, is_import_line};
use crate::lombok::LombokSupport;

const SOURCE: SourceId = SourceId::new("jdtls");

const CLASS_FILE_CONTENTS: &str = "java/classFileContents";

#[derive(Debug)]
pub enum OpenError {
    /// Nothing was started.
    NotTrusted,
    Setup(io::Error),
}

impl fmt::Display for OpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotTrusted => NotTrusted.fmt(f),
            Self::Setup(error) => write!(f, "cannot prepare jdtls: {error}"),
        }
    }
}

impl std::error::Error for OpenError {}

/// The text of a `jdt://` class, as jdtls decompiles or attaches it.
pub(crate) struct ClassFiles {
    router: Arc<Router>,
    runtime: Handle,
}

impl ClassFiles {
    async fn contents(&self, uri: &str) -> Option<String> {
        let mut replies = lang_api::ask(self, SourceTextQuery(uri.to_owned()));
        let answer = replies.next().await?;
        (!answer.value.is_empty()).then_some(answer.value)
    }
}

impl Provider<SourceTextQuery, String> for ClassFiles {
    fn start(&self, query: SourceTextQuery, _: Cancel, mut reply: Reply<String>) {
        let router = self.router.clone();
        self.runtime.spawn(async move {
            let params = json!({"uri": query.0});
            let answer = match router.extension(CLASS_FILE_CONTENTS, params).await {
                Ok(text) => Answer::new(
                    text.as_str().unwrap_or_default().to_owned(),
                    Confidence::Certain,
                    SOURCE,
                ),
                Err(_) => Answer::new(String::new(), Confidence::Unknown, SOURCE),
            };
            let _ = reply.send(answer);
        });
    }
}

/// A project with its server, the language as the core sees it once a project is open.
/// Dropping it ends the server.
pub struct JavaProject {
    sidecar: Sidecar,
    /// The server with its settings. Project Structure changes the runtimes here and
    /// asks the sidecar for a new process.
    spec: Arc<Jdtls>,
    router: Arc<Router>,
    pub(crate) root: PathBuf,
    runtime: Handle,
    pub(crate) providers: Arc<LspProviders>,
    /// The same providers as the server the window talks to; it also reimports the
    /// project when a build file is added or removed.
    pub(crate) server: JavaServer,
    pub(crate) syntax: JavaSyntax,
    pub(crate) class_files: ClassFiles,
    pub(crate) problems: ProjectProblems,
    pub(crate) debug: DebugExtension,
    /// The Lombok agent: found in the project dependencies and applied with a restart.
    lombok: Arc<LombokSupport>,
}

/// The language server as the window sees it. A build file that appears or disappears can
/// change the set of modules, and jdtls imports the new set only on a full import.
pub(crate) struct JavaServer {
    router: Arc<Router>,
    providers: Arc<LspProviders>,
    runtime: Handle,
    /// A build file that changes can add or remove the Lombok dependency.
    lombok: Arc<LombokSupport>,
    /// Bumped on every added or removed build file: a branch switch brings many at
    /// once, and only the import after the last of them runs.
    imports: Arc<std::sync::atomic::AtomicU64>,
}

/// How long the build files have to stay quiet before the full import starts.
const IMPORT_QUIET: Duration = Duration::from_millis(500);

impl lang_api::SemanticServer for JavaServer {
    fn open(&self, uri: &str, version: i32, text: Arc<str>) {
        self.providers.open(uri, version, text);
    }

    fn change(&self, uri: &str, version: i32, text: Arc<str>, edits: Vec<lang_api::DocumentEdit>) {
        self.providers.change(uri, version, text, edits);
    }

    fn save(&self, uri: &str) {
        self.providers.save(uri);
    }

    fn close(&self, uri: &str) {
        self.providers.close(uri);
    }

    fn file_changed(&self, uri: &str, change: lang_api::FileChange) {
        self.providers.file_changed(uri, change);
        // The dependency may have appeared, disappeared or changed its version in this
        // file, and the project model can be older than the save.
        self.lombok.build_files_changed();
        if change == lang_api::FileChange::Changed {
            return;
        }
        let router = self.router.clone();
        let imports = self.imports.clone();
        let mine = imports.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
        self.runtime.spawn(async move {
            sleep(IMPORT_QUIET).await;
            if imports.load(std::sync::atomic::Ordering::SeqCst) != mine {
                return;
            }
            let _ = router
                .extension(
                    "workspace/executeCommand",
                    json!({"command": "java.project.import", "arguments": []}),
                )
                .await;
        });
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.providers.version(uri)
    }

    fn reimport(&self) {
        let router = self.router.clone();
        self.runtime.spawn(async move {
            let _ = router
                .extension(
                    "workspace/executeCommand",
                    json!({"command": "java.project.import", "arguments": []}),
                )
                .await;
        });
    }
}

/// The latest project problems of the server, as the window reads them.
pub(crate) struct ProjectProblems {
    failures: watch::Receiver<Option<ImportFailure>>,
    log: Option<PathBuf>,
    runtime: Handle,
}

impl ProjectProblems {
    fn list(
        failures: &watch::Receiver<Option<ImportFailure>>,
        log: Option<PathBuf>,
    ) -> Vec<ProjectProblem> {
        failures
            .borrow()
            .clone()
            .map(|failure| ProjectProblem {
                file: Some(failure.file),
                line: failure.line,
                severity: Severity::Error,
                message: failure.message,
                summary: failure.summary,
                log,
            })
            .into_iter()
            .collect()
    }
}

async fn cancelled(cancel: &Cancel) {
    while !cancel.is_cancelled() {
        sleep(Duration::from_millis(25)).await;
    }
}

impl Provider<ProjectProblemsQuery, Vec<ProjectProblem>> for ProjectProblems {
    fn start(
        &self,
        _: ProjectProblemsQuery,
        cancel: Cancel,
        mut reply: Reply<Vec<ProjectProblem>>,
    ) {
        let mut failures = self.failures.clone();
        let log = self.log.clone();
        self.runtime.spawn(async move {
            let source = SOURCE;
            loop {
                let list = ProjectProblems::list(&failures, log.clone());
                let answer = Answer::new(list, Confidence::Certain, source);
                if reply.send(answer).is_err() && reply.is_cancelled() {
                    return;
                }
                tokio::select! {
                    changed = failures.changed() => {
                        if changed.is_err() {
                            return;
                        }
                    }
                    () = cancelled(&cancel) => return,
                }
            }
        });
    }
}

/// Asks the server to build the workspace, so that the diagnostics of every file are
/// current. The answer comes when the build ends.
impl Provider<PrepareDiagnosticsQuery, ()> for JavaProject {
    fn start(&self, _: PrepareDiagnosticsQuery, _: Cancel, mut reply: Reply<()>) {
        let router = self.router.clone();
        self.runtime.spawn(async move {
            let _ = router
                .extension("java/buildWorkspace", serde_json::json!(false))
                .await;
            let _ = reply.send(Answer::new((), Confidence::Certain, SOURCE));
        });
    }
}

/// The page of the symbol at a place. The server names it in the documentation root
/// of the URI of a compiled class, so the type of the symbol becomes the page inside
/// that root.
impl Provider<DocumentSpot, ExternalDoc> for JavaProject {
    fn start(&self, spot: DocumentSpot, _: Cancel, mut reply: Reply<ExternalDoc>) {
        let providers = self.providers.clone();
        self.runtime.spawn(async move {
            let hover: &HoverProvider = providers.as_ref();
            let mut replies = ask(hover, spot);
            let url = replies
                .next()
                .await
                .map(|answer| answer.value)
                .and_then(|text| documentation_page(&text.0));
            let _ = reply.send(Answer::new(
                ExternalDoc { url },
                Confidence::Certain,
                SOURCE,
            ));
        });
    }
}

/// A web address in the documentation text, or the page built from the documentation
/// root the server puts into the URI of a compiled class.
fn documentation_page(text: &str) -> Option<String> {
    if let Some(url) = web_address(text) {
        return Some(url);
    }
    let name = doc_type_name(text)?;
    let root = documentation_root(text)?;
    let page = format!("{}.html", name.replace('.', "/"));
    Some(format!("{root}{page}"))
}

fn web_address(text: &str) -> Option<String> {
    let start = text.find("https://").or_else(|| text.find("http://"))?;
    let rest = &text[start..];
    let end = rest
        .find(|character: char| {
            character.is_whitespace() || matches!(character, ')' | ']' | '*' | '"')
        })
        .unwrap_or(rest.len());
    Some(rest[..end].trim_end_matches([',', '.', ';']).to_owned())
}

/// The fully qualified name in the first code block, the line the server puts above
/// the documentation of a symbol.
fn doc_type_name(text: &str) -> Option<String> {
    let fence = text.find("```")?;
    let after = &text[fence + 3..];
    let line = after.split_once('\n')?.1.lines().next()?.trim();
    let name = line.split(['<', ' ', '(']).next().unwrap_or(line).trim();
    (name.contains('.')
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '.' | '_' | '$')))
    .then(|| name.to_owned())
}

/// The documentation root from `javadoc_location=/...=/` in the URI of a class.
fn documentation_root(text: &str) -> Option<String> {
    let marker = "javadoc_location=/";
    let start = text.find(marker)? + marker.len();
    let rest = &text[start..];
    let end = rest.find("=/")?;
    let encoded = &rest[..end];
    let mut decoded = String::with_capacity(encoded.len());
    let bytes = encoded.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'%'
            && at + 2 < bytes.len()
            && let Ok(byte) = u8::from_str_radix(&encoded[at + 1..at + 3], 16)
        {
            decoded.push(byte as char);
            at += 3;
            continue;
        }
        decoded.push(bytes[at] as char);
        at += 1;
    }
    let root = decoded.replace('\\', "/");
    root.starts_with("http").then(|| {
        if root.ends_with('/') {
            root
        } else {
            format!("{root}/")
        }
    })
}

impl JavaProject {
    /// Starts jdtls for a trusted project and returns at once, on a tokio runtime.
    pub fn open(config: JdtlsConfig, trust: Trust) -> Result<Self, OpenError> {
        if trust == Trust::Untrusted {
            return Err(OpenError::NotTrusted);
        }
        let root = config.project_root.clone();
        let repositories = crate::lombok::repositories(
            config.maven_settings.as_deref(),
            config.gradle_user_home.as_deref(),
        );
        let spec = Arc::new(Jdtls::new(config).map_err(OpenError::Setup)?);
        Self::start(
            spec,
            Arc::new(SupervisedLauncher),
            trust,
            root,
            repositories,
        )
    }

    /// Applies the Inlay Hints settings of the window. jdtls reads them on its next
    /// answer, so the shown hints change without a restart of the server.
    pub(crate) fn apply_inlay_hint_settings(&self, settings: lang_api::InlayHintSettings) {
        let parameters = match settings.parameter_names {
            lang_api::InlayParameterNames::None => jdtls_adapter::InlayParameterNames::None,
            lang_api::InlayParameterNames::Literals => jdtls_adapter::InlayParameterNames::Literals,
            lang_api::InlayParameterNames::All => jdtls_adapter::InlayParameterNames::All,
        };
        if !self.spec.set_inlay_hints(parameters, settings.type_hints) {
            return;
        }
        let settings = self.spec.settings();
        self.router.notify(
            "workspace/didChangeConfiguration",
            serde_json::json!({"settings": settings}),
        );
    }

    /// Applies the Code Vision settings of the window. The server stops sending the
    /// labels that are off, and the editor removes the rows it already has.
    pub(crate) fn apply_code_vision_settings(&self, settings: lang_api::CodeVisionSettings) {
        if !self
            .spec
            .set_code_vision(settings.usages, settings.implementations)
        {
            return;
        }
        let settings = self.spec.settings();
        self.router.notify(
            "workspace/didChangeConfiguration",
            serde_json::json!({"settings": settings}),
        );
    }

    fn start(
        spec: Arc<Jdtls>,
        launcher: Arc<dyn Launcher>,
        trust: Trust,
        root: PathBuf,
        repositories: java_project::LocalRepositories,
    ) -> Result<Self, OpenError> {
        let problems = ProjectProblems {
            failures: spec.watch_import_failure(),
            log: Some(spec.log_file()),
            runtime: Handle::current(),
        };
        let (published, revisions) = Published::new();
        let consumer: Arc<dyn ServerEvents> = published.clone();
        let autobuild = spec.autobuild();
        // The build files name Lombok before the project model is read: the first start
        // takes the agent, so a usual open does not start the server twice.
        if let Some(found) = java_project::find_lombok_in_build_files(&root, &repositories)
            && let Err(error) = spec.set_lombok(Some(jdtls_adapter::Lombok {
                version: found.version,
                jar: found.jar,
            }))
        {
            eprintln!("the Lombok agent is not copied: {error}");
        }
        let sidecar = Sidecar::start(spec.clone(), launcher, consumer, Limits::default(), trust)
            .map_err(|NotTrusted| OpenError::NotTrusted)?;
        let lombok = Arc::new(LombokSupport::new(
            root.clone(),
            spec.clone(),
            sidecar.reload_handle(),
            repositories,
        ));
        let router = Router::new(sidecar.status());
        let providers = Arc::new(
            LspProviders::new(
                router.clone(),
                published,
                revisions,
                Handle::current(),
                LANGUAGE_ID.0,
                SOURCE,
            )
            .with_semantic_tokens(spec.semantic_tokens())
            .with_code_lens(spec.code_lens())
            .with_edit_command("java.apply.workspaceEdit")
            .with_super_request("java/findLinks")
            .with_refactor_action(
                "java.action.applyRefactoringCommand",
                "java/getRefactorEdit",
            )
            .with_move_requests("java/getMoveDestinations", "java/move")
            .with_import_line(is_import_line)
            .with_change_signature(
                "java/getChangeSignatureInfo",
                "java/getRefactorEdit",
                "changeSignature",
            )
            .with_override_methods("java/listOverridableMethods", "java/addOverridableMethods")
            // With the prompt support off, jdtls lists no toString action; the two
            // requests behind the prompt give the same edits without a dialog.
            .with_prompt_source(PromptSource {
                kind: "source.generate.toString",
                title: "Generate toString()",
                check: "java/checkToStringStatus",
                generate: "java/generateToString",
            })
            .with_generate(semantic_router::GenerateRequests {
                constructor_check: "java/checkConstructorsStatus",
                constructor_generate: "java/generateConstructors",
                accessor_check: "java/resolveUnimplementedAccessors",
                accessor_generate: "java/generateAccessors",
                equals_check: "java/checkHashCodeEqualsStatus",
                equals_generate: "java/generateHashCodeEquals",
                tostring_check: "java/checkToStringStatus",
                tostring_generate: "java/generateToString",
                delegate_check: "java/checkDelegateMethodsStatus",
                delegate_generate: "java/generateDelegateMethods",
            }),
        );
        let server = JavaServer {
            router: router.clone(),
            providers: providers.clone(),
            runtime: Handle::current(),
            lombok: lombok.clone(),
            imports: Arc::default(),
        };
        let class_files = ClassFiles {
            router: router.clone(),
            runtime: Handle::current(),
        };
        let debug = DebugExtension::with_router(router.clone(), autobuild);
        Ok(Self {
            sidecar,
            spec,
            router,
            root,
            runtime: Handle::current(),
            providers,
            server,
            syntax: JavaSyntax::new(),
            class_files,
            problems,
            debug,
            lombok,
        })
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.sidecar.status()
    }

    /// The latest import failure, for the line of the status bar.
    pub fn watch_import_failure(&self) -> watch::Receiver<Option<ImportFailure>> {
        self.problems.failures.clone()
    }

    /// Starts the server again after it stopped, for example for low memory.
    pub(crate) fn restart(&self) -> bool {
        self.sidecar.restart()
    }

    /// Project Structure chose another SDK: the server starts again with it. The state
    /// folder and the documents stay, so the window keeps working.
    pub fn set_project_jdk(&self, environment: &java_project::ResolvedEnvironment) -> bool {
        self.spec.set_runtimes(crate::session::runtimes_for(
            environment,
            self.spec.java_home(),
        ));
        self.sidecar.reload()
    }

    /// The model of the project arrived. A Lombok jar in its dependencies is copied and
    /// a running server starts again with it.
    pub(crate) fn set_project_model(&self, model: &Arc<project_model::ProjectModel>) {
        self.lombok.project_model(model.clone());
    }

    /// The version of Lombok the server runs with.
    pub(crate) fn lombok_version(&self) -> Option<String> {
        self.spec.lombok_version()
    }

    /// The Lombok jar the model names, with the repositories of this project.
    pub(crate) fn lombok_of(
        &self,
        model: &project_model::ProjectModel,
    ) -> Option<java_project::Lombok> {
        self.lombok.of(model)
    }

    pub(crate) fn lombok_repositories(&self) -> &java_project::LocalRepositories {
        self.lombok.repositories()
    }

    /// A line for the notification of the window, once.
    pub(crate) fn take_notice(&self) -> Option<String> {
        self.lombok.take_note()
    }

    pub fn router(&self) -> &Arc<Router> {
        &self.router
    }

    /// The requests and the state of debug sessions, shared by every JavaDebugger.
    pub(crate) fn debug_extension(&self) -> DebugExtension {
        self.debug.clone()
    }

    /// The text behind a `jdt://` location, for a read-only buffer.
    pub async fn class_file_contents(&self, uri: &Uri) -> Result<String, RouteError> {
        let text = self
            .router
            .extension(CLASS_FILE_CONTENTS, json!({"uri": uri.as_str()}))
            .await?;
        Ok(text.as_str().unwrap_or_default().to_owned())
    }

    /// Stops the server politely, then by force.
    pub async fn stop(self) {
        self.sidecar.stop().await;
    }
}

fn point_in_text(text: &str, offset: usize) -> lang_api::TextPoint {
    let offset = offset.min(text.len());
    let mut line = 0u32;
    let mut line_start = 0;
    for (at, byte) in text.bytes().enumerate() {
        if at >= offset {
            break;
        }
        if byte == b'\n' {
            line += 1;
            line_start = at + 1;
        }
    }
    let character = text[line_start..offset].encode_utf16().count() as u32;
    lang_api::TextPoint { line, character }
}

fn span_in_text(text: &str, start: usize, end: usize) -> TextSpan {
    TextSpan {
        start: point_in_text(text, start),
        end: point_in_text(text, end),
    }
}

fn type_symbol(text: &str, name: &str) -> Option<SourceSpan> {
    let offset = java_syntax::first_type_offset(text)?;
    Some(SourceSpan {
        uri: String::new(),
        span: span_in_text(text, offset, offset + name.len()),
    })
}

/// Reads the text of a hierarchy item, from the open documents or from disk.
fn hierarchy_text(router: &Router, uri: &str) -> Option<String> {
    let parsed = lsp_client::lsp_types::Uri::from_str(uri).ok()?;
    if let Some(text) = router.text(&parsed) {
        return Some(text.to_string());
    }
    let path = lsp_client::uri::file_path(&parsed)?;
    std::fs::read_to_string(path).ok()
}

/// The test source folder of the module that holds `path`, and the package below it.
fn test_folder(root: &Path, path: &Path) -> Option<(PathBuf, String)> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let source = parts.windows(3).position(|window| {
        window[0] == "src" && matches!(window[1].as_str(), "main" | "test") && window[2] == "java"
    })?;
    let mut folder = root.to_path_buf();
    for part in &parts[..source] {
        folder.push(part);
    }
    folder.push("src");
    folder.push("test");
    folder.push("java");
    let package_parts = &parts[source + 3..parts.len().saturating_sub(1)];
    for part in package_parts {
        folder.push(part);
    }
    Some((folder, package_parts.join(".")))
}

fn junit_version(root: &Path) -> Option<u8> {
    let script = ["pom.xml", "build.gradle", "build.gradle.kts"]
        .iter()
        .filter_map(|name| std::fs::read_to_string(root.join(name)).ok())
        .collect::<Vec<_>>()
        .join("\n")
        .to_lowercase();
    if script.contains("junit-jupiter") || script.contains("org.junit.jupiter") {
        return Some(5);
    }
    script.contains("junit").then_some(4)
}

fn test_class_source(package: &str, class: &str, methods: &[String], version: u8) -> String {
    let mut text = String::new();
    if !package.is_empty() {
        text.push_str(&format!("package {package};\n\n"));
    }
    if version >= 5 {
        text.push_str("import org.junit.jupiter.api.Test;\n\n");
        text.push_str(&format!("class {class}Test {{\n"));
        for method in methods {
            text.push_str(&format!("    @Test\n    void {method}() {{\n    }}\n\n"));
        }
    } else {
        text.push_str("import org.junit.Test;\n\n");
        text.push_str(&format!("public class {class}Test {{\n"));
        for method in methods {
            text.push_str(&format!(
                "    @Test\n    public void {method}() {{\n    }}\n\n"
            ));
        }
    }
    text.push_str("}\n");
    text
}

fn test_generation(
    root: &Path,
    text: &str,
    path: &Path,
    selected: &[String],
    choose: bool,
) -> Generation {
    let Some((folder, package)) = test_folder(root, path) else {
        return Generation {
            error: Some("The file is not under a source folder".to_owned()),
            ..Generation::default()
        };
    };
    let Some(version) = junit_version(root) else {
        return Generation {
            error: Some("The project has no test framework".to_owned()),
            ..Generation::default()
        };
    };
    let class = java_syntax::first_type_name(text)
        .or_else(|| {
            path.file_stem()
                .map(|stem| stem.to_string_lossy().into_owned())
        })
        .unwrap_or_default();
    let file = folder.join(format!("{class}Test.java"));
    if file.exists() {
        return Generation {
            error: Some("The test class already exists".to_owned()),
            ..Generation::default()
        };
    }
    let mut methods = java_syntax::testable_methods(text);
    let mut seen = std::collections::HashSet::new();
    methods.retain(|method| seen.insert(method.clone()));
    if choose {
        return Generation {
            choices: methods
                .into_iter()
                .map(|method| GenerateChoice {
                    label: method.clone(),
                    detail: None,
                    checked: false,
                    handle: method,
                })
                .collect(),
            ..Generation::default()
        };
    }
    Generation {
        files: vec![GeneratedFile {
            path: file,
            text: test_class_source(&package, &class, selected, version),
        }],
        ..Generation::default()
    }
}

impl Provider<GenerateQuery, Generation> for JavaProject {
    fn start(&self, query: GenerateQuery, cancel: Cancel, mut reply: Reply<Generation>) {
        if query.kind != GenerateKind::Test {
            let provider: &dyn Provider<GenerateQuery, Generation> = self.providers.as_ref();
            let mut replies = ask(provider, query);
            self.runtime.spawn(async move {
                if let Some(answer) = replies.next().await {
                    let _ = reply.send(answer);
                }
            });
            return;
        }
        let _ = cancel;
        let uri = lsp_client::lsp_types::Uri::from_str(&query.spot.uri).ok();
        let path = uri.as_ref().and_then(lsp_client::uri::file_path);
        let text = uri
            .as_ref()
            .and_then(|uri| self.router.text(uri))
            .map(|text| text.to_string());
        let outcome = match (path, text) {
            (Some(path), Some(text)) => test_generation(&self.root, &text, &path, &[], true),
            _ => Generation {
                error: Some("The file is not open".to_owned()),
                ..Generation::default()
            },
        };
        let _ = reply.send(Answer::new(outcome, Confidence::Certain, SOURCE));
    }
}

impl Provider<GenerateApplyRequest, Generation> for JavaProject {
    fn start(&self, query: GenerateApplyRequest, cancel: Cancel, mut reply: Reply<Generation>) {
        if query.kind != GenerateKind::Test {
            let provider: &dyn Provider<GenerateApplyRequest, Generation> = self.providers.as_ref();
            let mut replies = ask(provider, query);
            self.runtime.spawn(async move {
                if let Some(answer) = replies.next().await {
                    let _ = reply.send(answer);
                }
            });
            return;
        }
        let _ = cancel;
        let uri = lsp_client::lsp_types::Uri::from_str(&query.spot.uri).ok();
        let path = uri.as_ref().and_then(lsp_client::uri::file_path);
        let text = uri
            .as_ref()
            .and_then(|uri| self.router.text(uri))
            .map(|text| text.to_string());
        let outcome = match (path, text) {
            (Some(path), Some(text)) => {
                test_generation(&self.root, &text, &path, &query.handles, false)
            }
            _ => Generation {
                error: Some("The file is not open".to_owned()),
                ..Generation::default()
            },
        };
        let _ = reply.send(Answer::new(outcome, Confidence::Certain, SOURCE));
    }
}

impl Provider<MethodHierarchyQuery, Vec<MethodHierarchyEntry>> for JavaProject {
    fn start(
        &self,
        query: MethodHierarchyQuery,
        cancel: Cancel,
        mut reply: Reply<Vec<MethodHierarchyEntry>>,
    ) {
        let router = self.router.clone();
        let providers = self.providers.clone();
        self.runtime.spawn(async move {
            let Ok(uri) = lsp_client::lsp_types::Uri::from_str(&query.0.uri) else {
                return;
            };
            let Some(text) = router.text(&uri).map(|text| text.to_string()).or_else(|| {
                lsp_client::uri::file_path(&uri).and_then(|path| std::fs::read_to_string(path).ok())
            }) else {
                return;
            };
            let offset = offset_in(&text, query.0.point);
            let Some((method, _)) = java_syntax::method_at(&text, offset) else {
                let _ = reply.send(Answer::new(Vec::new(), Confidence::Certain, SOURCE));
                return;
            };
            let Some((type_name, type_name_range)) = java_syntax::enclosing_type_at(&text, offset)
            else {
                let _ = reply.send(Answer::new(Vec::new(), Confidence::Certain, SOURCE));
                return;
            };
            let type_spot = DocumentSpot {
                uri: query.0.uri.clone(),
                version: query.0.version,
                point: point_in_text(&text, type_name_range.start),
            };
            let provider: &dyn Provider<HierarchyQuery, Vec<SymbolEntry>> = providers.as_ref();
            let mut up = ask(
                provider,
                HierarchyQuery {
                    spot: type_spot.clone(),
                    direction: HierarchyDirection::Supertypes,
                },
            );
            let mut down = ask(
                provider,
                HierarchyQuery {
                    spot: type_spot,
                    direction: HierarchyDirection::Subtypes,
                },
            );
            let mut found: Vec<SymbolEntry> = Vec::new();
            if let Some(answer) = up.next().await
                && answer.confidence != Confidence::Unknown
            {
                found.extend(answer.value);
            }
            if let Some(answer) = down.next().await
                && answer.confidence != Confidence::Unknown
            {
                found.extend(answer.value);
            }
            if cancel.is_cancelled() {
                return;
            }
            let mut seen = std::collections::HashSet::new();
            let mut entries = Vec::new();
            let own = Some(type_name.clone()).map(|name| {
                let place = type_symbol(&text, &name).map(|mut span| {
                    span.uri = query.0.uri.clone();
                    span
                });
                (name, place)
            });
            let all = own
                .into_iter()
                .chain(found.into_iter().map(|symbol| (symbol.name, symbol.place)))
                .collect::<Vec<_>>();
            for (type_name, place) in all {
                if !seen.insert(type_name.clone()) {
                    continue;
                }
                let (declares, abstract_type) = place
                    .as_ref()
                    .and_then(|place| hierarchy_text(&router, &place.uri))
                    .map(|source| java_syntax::type_declares_method(&source, &method))
                    .unwrap_or((false, false));
                let relation = if declares {
                    MethodRelation::Defines
                } else if abstract_type {
                    MethodRelation::MustDefine
                } else {
                    MethodRelation::Inherits
                };
                entries.push(MethodHierarchyEntry {
                    type_name,
                    method: method.clone(),
                    relation,
                    place,
                });
            }
            let _ = reply.send(Answer::new(entries, Confidence::Certain, SOURCE));
        });
    }
}

fn text_span(range: lsp_client::lsp_types::Range) -> TextSpan {
    let point = |position: lsp_client::lsp_types::Position| lang_api::TextPoint {
        line: position.line,
        character: position.character,
    };
    TextSpan {
        start: point(range.start),
        end: point(range.end),
    }
}

fn kind_of(kind: LspSymbolKind) -> SymbolKind {
    match kind {
        LspSymbolKind::MODULE | LspSymbolKind::NAMESPACE | LspSymbolKind::PACKAGE => {
            SymbolKind::Module
        }
        LspSymbolKind::CLASS | LspSymbolKind::STRUCT | LspSymbolKind::OBJECT => SymbolKind::Class,
        LspSymbolKind::INTERFACE => SymbolKind::Interface,
        LspSymbolKind::ENUM => SymbolKind::Enum,
        LspSymbolKind::FUNCTION => SymbolKind::Function,
        LspSymbolKind::METHOD | LspSymbolKind::OPERATOR | LspSymbolKind::EVENT => {
            SymbolKind::Method
        }
        LspSymbolKind::CONSTRUCTOR => SymbolKind::Constructor,
        LspSymbolKind::CONSTANT => SymbolKind::Constant,
        _ => SymbolKind::Field,
    }
}

fn file_symbol(symbol: DocumentSymbol) -> FileSymbol {
    FileSymbol {
        name: symbol.name,
        detail: symbol.detail.filter(|detail| !detail.trim().is_empty()),
        kind: kind_of(symbol.kind),
        range: text_span(symbol.range),
        name_range: text_span(symbol.selection_range),
        children: symbol
            .children
            .unwrap_or_default()
            .into_iter()
            .map(file_symbol)
            .collect(),
    }
}

/// The type of a field from its declaration: the name of a field alone does not carry
/// it, unlike the detail of a method that the server sends.
fn field_type(text: &str, symbol: &FileSymbol) -> Option<String> {
    let start = offset_in(text, symbol.range.start);
    let end = offset_in(text, symbol.range.end).min(text.len());
    let declaration = text.get(start..end)?;
    let name_at = declaration.find(&symbol.name)?;
    let prefix = declaration[..name_at].trim_end();
    let end = prefix.len();
    let mut depth: i32 = 0;
    let mut begin = 0;
    for (index, character) in prefix[..end].char_indices().rev() {
        match character {
            '>' | ']' => depth += 1,
            '<' | '[' => depth -= 1,
            _ if depth > 0 => {}
            _ if character.is_alphanumeric() || matches!(character, '_' | '$' | '.') => {}
            _ => {
                begin = index + character.len_utf8();
                break;
            }
        }
    }
    let found = prefix[begin..].trim();
    (!found.is_empty()).then(|| found.to_owned())
}

fn enrich_fields(symbols: &mut [FileSymbol], text: &str) {
    for symbol in symbols {
        if symbol.detail.is_none()
            && matches!(symbol.kind, SymbolKind::Field | SymbolKind::Constant)
        {
            symbol.detail = field_type(text, symbol);
        }
        enrich_fields(&mut symbol.children, text);
    }
}

/// The declaration around a place: a method with its body, a field, a type.
fn declaration_span(text: &str, offset: usize) -> Option<std::ops::Range<usize>> {
    const DECLARATIONS: &[&str] = &[
        "method_declaration",
        "constructor_declaration",
        "compact_constructor_declaration",
        "field_declaration",
        "constant_declaration",
        "class_declaration",
        "interface_declaration",
        "enum_declaration",
        "record_declaration",
        "annotation_type_declaration",
        "enum_constant",
        "local_variable_declaration",
    ];
    let mut parser = tree_sitter::Parser::new();
    let config = java_syntax::config();
    parser.set_language(&config.grammar).ok()?;
    let tree = parser.parse(text, None)?;
    let offset = offset.min(text.len());
    let mut node = tree.root_node().descendant_for_byte_range(offset, offset)?;
    loop {
        if DECLARATIONS.contains(&node.kind()) {
            return Some(node.byte_range());
        }
        node = node.parent()?;
    }
}

fn first_location(
    response: lsp_client::lsp_types::GotoDefinitionResponse,
) -> Option<lsp_client::lsp_types::Location> {
    match response {
        lsp_client::lsp_types::GotoDefinitionResponse::Scalar(location) => Some(location),
        lsp_client::lsp_types::GotoDefinitionResponse::Array(mut locations) => {
            (!locations.is_empty()).then(|| locations.remove(0))
        }
        lsp_client::lsp_types::GotoDefinitionResponse::Link(links) => {
            links
                .into_iter()
                .next()
                .map(|link| lsp_client::lsp_types::Location {
                    uri: link.target_uri,
                    range: link.target_selection_range,
                })
        }
    }
}

fn read_source(uri: &Uri, router: &Router) -> Option<String> {
    router
        .text(uri)
        .map(|text| text.to_string())
        .or_else(|| file_path(uri).and_then(|path| std::fs::read_to_string(path).ok()))
}

fn contains(span: &TextSpan, point: lang_api::TextPoint) -> bool {
    (span.start.line, span.start.character) <= (point.line, point.character)
        && (point.line, point.character) <= (span.end.line, span.end.character)
}

fn package_of(text: &str) -> String {
    text.lines()
        .find_map(|line| line.trim().strip_prefix("package "))
        .map(|rest| rest.trim_end_matches(';').trim().to_owned())
        .unwrap_or_default()
}

fn type_name(package: &str, types: &[String]) -> String {
    let name = types.join(".");
    if package.is_empty() {
        name
    } else {
        format!("{package}.{name}")
    }
}

fn member_name(name: &str) -> &str {
    name.split('(').next().unwrap_or(name).trim()
}

fn reference_in(
    symbols: &[DocumentSymbol],
    point: lang_api::TextPoint,
    package: &str,
    types: &mut Vec<String>,
) -> Option<String> {
    for symbol in symbols {
        if !contains(&text_span(symbol.range), point) {
            continue;
        }
        let kind = kind_of(symbol.kind);
        if kind == SymbolKind::Module {
            if let Some(found) = reference_in(
                symbol.children.as_deref().unwrap_or_default(),
                point,
                package,
                types,
            ) {
                return Some(found);
            }
            continue;
        }
        if matches!(
            kind,
            SymbolKind::Class | SymbolKind::Interface | SymbolKind::Enum
        ) {
            types.push(symbol.name.clone());
            let children = symbol.children.as_deref().unwrap_or_default();
            if let Some(found) = reference_in(children, point, package, types) {
                types.pop();
                return Some(found);
            }
            let name = type_name(package, types);
            types.pop();
            return Some(name);
        }
        if matches!(
            kind,
            SymbolKind::Method | SymbolKind::Constructor | SymbolKind::Field | SymbolKind::Constant
        ) {
            let owner = type_name(package, types);
            return Some(format!("{owner}#{}", member_name(&symbol.name)));
        }
        if let Some(found) = reference_in(
            symbol.children.as_deref().unwrap_or_default(),
            point,
            package,
            types,
        ) {
            return Some(found);
        }
    }
    None
}

impl Provider<FileSymbolsQuery, Vec<FileSymbol>> for JavaProject {
    fn start(&self, query: FileSymbolsQuery, _: Cancel, mut reply: Reply<Vec<FileSymbol>>) {
        let router = self.router.clone();
        self.runtime.spawn(async move {
            let Ok(uri) = query.0.parse::<Uri>() else {
                return;
            };
            let place = semantic_router::Place {
                version: router.version(&uri).unwrap_or(0),
                uri: uri.clone(),
                position: lsp_client::lsp_types::Position::default(),
            };
            let Ok(answer) = router.document_symbol(place).await else {
                let _ = reply.send(Answer::new(Vec::new(), Confidence::Unknown, SOURCE));
                return;
            };
            let mut symbols = match answer {
                Some(DocumentSymbolResponse::Nested(symbols)) => {
                    symbols.into_iter().map(file_symbol).collect()
                }
                Some(DocumentSymbolResponse::Flat(symbols)) => symbols
                    .into_iter()
                    .map(|symbol| FileSymbol {
                        name: symbol.name,
                        detail: None,
                        kind: kind_of(symbol.kind),
                        range: text_span(symbol.location.range),
                        name_range: text_span(symbol.location.range),
                        children: Vec::new(),
                    })
                    .collect(),
                None => Vec::new(),
            };
            if let Some(text) = read_source(&uri, &router) {
                enrich_fields(&mut symbols, &text);
            }
            let _ = reply.send(Answer::new(symbols, Confidence::Certain, SOURCE));
        });
    }
}

impl Provider<DefinitionPreviewQuery, DefinitionPreview> for JavaProject {
    fn start(&self, query: DefinitionPreviewQuery, _: Cancel, mut reply: Reply<DefinitionPreview>) {
        let router = self.router.clone();
        let class_files = ClassFiles {
            router: router.clone(),
            runtime: self.runtime.clone(),
        };
        self.runtime.spawn(async move {
            let place = semantic_router::Place {
                version: query.0.version,
                uri: match query.0.uri.parse() {
                    Ok(uri) => uri,
                    Err(_) => return,
                },
                position: lsp_client::lsp_types::Position::new(
                    query.0.point.line,
                    query.0.point.character,
                ),
            };
            let Ok(response) = router.definition(place).await else {
                let _ = reply.send(Answer::new(
                    DefinitionPreview::default(),
                    Confidence::Unknown,
                    SOURCE,
                ));
                return;
            };
            let Some(location) = response.and_then(first_location) else {
                let _ = reply.send(Answer::new(
                    DefinitionPreview::default(),
                    Confidence::Certain,
                    SOURCE,
                ));
                return;
            };
            let text = match read_source(&location.uri, &router) {
                Some(text) => Some(text),
                None => class_files.contents(location.uri.as_str()).await,
            };
            let Some(text) = text else {
                let _ = reply.send(Answer::new(
                    DefinitionPreview::default(),
                    Confidence::Unknown,
                    SOURCE,
                ));
                return;
            };
            let offset = offset_in(
                &text,
                lang_api::TextPoint {
                    line: location.range.start.line,
                    character: location.range.start.character,
                },
            );
            let span = declaration_span(&text, offset).unwrap_or_else(|| {
                let start = text[..offset.min(text.len())]
                    .rfind('\n')
                    .map_or(0, |at| at + 1);
                let end = text[offset.min(text.len())..]
                    .find('\n')
                    .map_or(text.len(), |at| offset + at);
                start..end
            });
            let preview = DefinitionPreview {
                text: text[span].to_owned(),
                target: Some(SourceSpan {
                    uri: location.uri.as_str().to_owned(),
                    span: text_span(location.range),
                }),
            };
            let _ = reply.send(Answer::new(preview, Confidence::Certain, SOURCE));
        });
    }
}

impl Provider<ReferenceQuery, String> for JavaProject {
    fn start(&self, query: ReferenceQuery, _: Cancel, mut reply: Reply<String>) {
        let router = self.router.clone();
        let root = self.root.clone();
        self.runtime.spawn(async move {
            let fallback = |uri: &str, line: u32| {
                let mut text = uri
                    .parse::<Uri>()
                    .ok()
                    .and_then(|uri| file_path(&uri))
                    .map(|path| {
                        let relative = path.strip_prefix(&root).unwrap_or(&path);
                        relative.display().to_string()
                    })
                    .unwrap_or_else(|| uri.to_owned());
                text = text.replace('\\', "/");
                format!("{text}:{}", line + 1)
            };
            let Ok(uri) = query.0.uri.parse::<Uri>() else {
                return;
            };
            let place = semantic_router::Place {
                version: query.0.version,
                uri: uri.clone(),
                position: lsp_client::lsp_types::Position::new(
                    query.0.point.line,
                    query.0.point.character,
                ),
            };
            let fallback_line = query.0.point.line;
            let symbols = match router.document_symbol(place).await {
                Ok(Some(DocumentSymbolResponse::Nested(symbols))) => symbols,
                _ => {
                    let _ = reply.send(Answer::new(
                        fallback(uri.as_str(), fallback_line),
                        Confidence::Certain,
                        SOURCE,
                    ));
                    return;
                }
            };
            let text = read_source(&uri, &router).unwrap_or_default();
            let package = package_of(&text);
            let reference = reference_in(&symbols, query.0.point, &package, &mut Vec::new())
                .filter(|reference| !reference.ends_with('#') && !reference.starts_with('#'));
            let reference = reference.unwrap_or_else(|| fallback(uri.as_str(), fallback_line));
            let _ = reply.send(Answer::new(reference, Confidence::Certain, SOURCE));
        });
    }
}

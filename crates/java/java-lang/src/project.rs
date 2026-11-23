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
    Answer, CompletionList, Confidence, DefinitionPreview, DefinitionPreviewQuery,
    DisabledInspections, DocumentSpot, ExternalDoc, FileSymbol, FileSymbolsQuery,
    GenerateApplyRequest, GenerateChoice, GenerateKind, GenerateQuery, GeneratedFile, Generation,
    HierarchyDirection, HierarchyQuery, HoverProvider, MethodHierarchyEntry, MethodHierarchyQuery,
    MethodRelation, NavigationQuery, PrepareDiagnosticsQuery, ProjectProblem, ProjectProblemsQuery,
    Provider, ReferenceQuery, Reply, Severity, SourceId, SourceSpan, SourceTextQuery, SymbolEntry,
    SymbolKind, TestCreateOutcome, TestCreateRequest, TestFixOutcome, TestFixRequest, TestForm,
    TestFormQuery, TextSpan, TypeHierarchyItem, TypeHierarchyQuery, TypeKind, ask, offset_in,
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
use crate::messages::{JavaMessages, MessageBundles};
use crate::refactor::{MemberRefactorings, RefactorForms};
use crate::spring::SpringConfigSupport;
use crate::spring_data::{NativeCompletion, NativeNavigation, SpringDataIndex};

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
    /// Whether a Gradle import runs now, for the line of the status bar.
    importing: watch::Receiver<bool>,
    pub(crate) syntax: JavaSyntax,
    pub(crate) class_files: ClassFiles,
    /// The sources of library classes: the jar, the repository and the attachment.
    pub(crate) library_sources: crate::sources::LibrarySources,
    /// The model of the project, for the request that fetches every source at once.
    model: Arc<std::sync::Mutex<Option<Arc<project_model::ProjectModel>>>>,
    pub(crate) problems: ProjectProblems,
    pub(crate) debug: DebugExtension,
    /// The refactorings whose values the window asks for before the server edits.
    pub(crate) forms: RefactorForms,
    /// Pull Members Up, Push Members Down, Extract Superclass and Introduce Parameter
    /// Object, computed from the trees of the project.
    pub(crate) members: MemberRefactorings,
    /// Move Members: the members of a type and the types of the project that take them.
    pub(crate) move_members: crate::move_members::MoveMembers,
    /// A move to a parameter or a field of the type of a method.
    pub(crate) move_targets: crate::move_targets::MoveTargets,
    /// Change Signature: the server's signature with the levels the place allows.
    pub(crate) change_signature: crate::change_signature::ChangeSignatureSupport,
    /// The Lombok agent: found in the project dependencies and applied with a restart.
    lombok: Arc<LombokSupport>,
    /// Stack traces and thread dumps: the frames of a trace, and `jcmd` for a running
    /// program of the project.
    pub(crate) stack: crate::stack::StackService,
    /// The metadata of the configuration files of the project, which is read in the
    /// background from the class path of the model.
    pub(crate) spring: Arc<SpringConfigSupport>,
    /// The message bundles of the project and the places of the sources that use a key.
    pub(crate) messages: Arc<MessageBundles>,
    /// The versions of the dependencies in the build files: the hints on an old one, the
    /// fixes and the entries of a version being typed.
    pub(crate) build_scripts: crate::build_scripts::BuildScriptSupport,
    /// The changelogs of the schema migrations, for their completion and their checks.
    pub(crate) migration_files: Arc<crate::MigrationFiles>,
    /// The answers of the message bundles in front of the answers of the server.
    pub(crate) java_messages: JavaMessages,
    /// The beans and the points of an injection of the project, read in the background
    /// from its sources.
    pub(crate) beans: Arc<crate::BeanIndex>,
    /// The entities of the project and the queries of its data repositories, read in the
    /// background from its sources.
    pub(crate) data: Arc<SpringDataIndex>,
    /// The checks the user turned off, shared with the native providers: a check that is
    /// off is not computed at all.
    checks: DisabledInspections,
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
    /// The message bundles, which check the keys a Java source uses.
    messages: Arc<MessageBundles>,
    /// The text of an open source, so that the model of the beans follows an unsaved
    /// editor.
    beans: Arc<crate::BeanIndex>,
    /// The same text for the model of the entities and the queries.
    data: Arc<SpringDataIndex>,
}

/// How long the build files have to stay quiet before the full import starts.
const IMPORT_QUIET: Duration = Duration::from_millis(500);

impl lang_api::SemanticServer for JavaServer {
    fn open(&self, uri: &str, version: i32, text: Arc<str>) {
        self.data.document_uri_changed(uri, Some(text.clone()));
        self.messages.open(uri, version, text.clone());
        self.providers.open(uri, version, text);
    }

    fn change(&self, uri: &str, version: i32, text: Arc<str>, edits: Vec<lang_api::DocumentEdit>) {
        self.messages
            .change(uri, version, text.clone(), edits.clone());
        self.beans.document_uri_changed(uri, Some(text.clone()));
        self.data.document_uri_changed(uri, Some(text.clone()));
        self.providers.change(uri, version, text, edits);
    }

    fn save(&self, uri: &str) {
        self.messages.save(uri);
        self.providers.save(uri);
    }

    fn close(&self, uri: &str) {
        self.messages.close(uri);
        self.beans.document_uri_changed(uri, None);
        self.data.document_uri_changed(uri, None);
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
    spec: Arc<Jdtls>,
    runtime: Handle,
}

impl ProjectProblems {
    fn list(
        failures: &watch::Receiver<Option<ImportFailure>>,
        spec: &Jdtls,
    ) -> Vec<ProjectProblem> {
        let Some(failure) = failures.borrow().clone() else {
            return Vec::new();
        };
        // Show Log opens the log of the messages of the server. The JVM output that is
        // not a known warning joins it under its own heading before the window opens it.
        spec.append_jvm_output();
        vec![ProjectProblem {
            file: (!failure.file.as_os_str().is_empty()).then_some(failure.file),
            line: failure.line,
            severity: Severity::Error,
            message: failure.message,
            summary: failure.summary,
            log: Some(spec.log_file()),
            tool: failure.tool.map(|tool| match tool {
                jdtls_adapter::BuildTool::Maven => 0,
                jdtls_adapter::BuildTool::Gradle => 1,
            }),
            retry: failure.retry,
        }]
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
        let spec = self.spec.clone();
        self.runtime.spawn(async move {
            let source = SOURCE;
            loop {
                let list = ProjectProblems::list(&failures, &spec);
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
        let cache = config.state_dir.clone();
        let offline = config.offline;
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
            cache,
            offline,
        )
    }

    /// Applies the choices of the user for the checks. A check that is off reaches the
    /// native providers, which then skip it before their work; the compiler problems go
    /// to a `.prefs` file whose path changes with the choices, and the server reads it
    /// when `workspace/didChangeConfiguration` brings the new path, without a restart.
    /// False when no server setting changed.
    pub(crate) fn apply_inspection_settings(
        &self,
        settings: &lang_api::InspectionSettings,
    ) -> bool {
        self.checks.replace(settings);
        let prefs = crate::inspections::jdt_prefs(settings);
        if !self.spec.set_inspection_prefs(prefs) {
            return false;
        }
        let settings = self.spec.settings();
        self.router.notify(
            "workspace/didChangeConfiguration",
            serde_json::json!({"settings": settings}),
        );
        true
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
        cache: PathBuf,
        offline: bool,
    ) -> Result<Self, OpenError> {
        let failures = crate::session::follow_failures(
            spec.watch_import_failure(),
            spec.watch_import_starts(),
            Arc::new(crate::session::ImportRetries::new(&cache)),
            &Handle::current(),
        );
        let problems = ProjectProblems {
            failures,
            spec: spec.clone(),
            runtime: Handle::current(),
        };
        let importing = spec.watch_importing();
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
            repositories.clone(),
        ));
        let model: Arc<std::sync::Mutex<Option<Arc<project_model::ProjectModel>>>> = Arc::default();
        let router = Router::new(sidecar.status());
        let migration_files = crate::migrations_language(root.clone());
        let build_scripts = crate::build_scripts::BuildScriptSupport::new(
            root.clone(),
            cache.join(java_project::updates::CACHE_FOLDER),
            offline,
        );
        let library_sources = crate::sources::LibrarySources::new(
            router.clone(),
            Handle::current(),
            root.clone(),
            repositories,
            cache,
            model.clone(),
        );
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
            // The command of the server that points at the name an extract introduced.
            .with_refactor_rename("java.action.rename")
            // Extract interface and the two field refactorings need values the user
            // picks: what to extract, the name and the package, where a field starts.
            .with_refactor_steps(semantic_router::RefactorSteps {
                forms: &["extractInterface", "extractField", "convertVariableToField"],
                deferred: &[
                    "changeSignature",
                    "moveFile",
                    "moveType",
                    "moveInstanceMethod",
                    "moveStaticMember",
                ],
            })
            // The expressions under the caret that an extract may take, innermost first.
            .with_refactor_selections("java/inferSelection")
            .with_move_requests("java/getMoveDestinations", "java/move", "moveStaticMember")
            .with_move_targets_kind("moveInstanceMethod")
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
        let spring = crate::spring_config(root.clone());
        let messages = crate::message_bundles(root.clone());
        let beans = crate::bean_index(root.clone());
        let data = crate::spring_data_index(root.clone());
        let checks = DisabledInspections::default();
        spring.set_inspection_checks(checks.clone());
        messages.set_inspection_checks(checks.clone());
        data.set_inspection_checks(checks.clone());
        build_scripts.set_inspection_checks(checks.clone());
        migration_files.set_inspection_checks(checks.clone());
        let completion_fallback: Arc<dyn Provider<DocumentSpot, CompletionList>> =
            providers.clone();
        let navigation_fallback: Arc<dyn Provider<NavigationQuery, Vec<SourceSpan>>> =
            providers.clone();
        let completion = NativeCompletion {
            data: data.clone(),
            fallback: Some(completion_fallback),
        };
        let navigation = NativeNavigation {
            data: data.clone(),
            fallback: Some(navigation_fallback),
        };
        let native_completion = Arc::new(completion);
        let native_navigation = Arc::new(navigation);
        let server = JavaServer {
            router: router.clone(),
            providers: providers.clone(),
            runtime: Handle::current(),
            lombok: lombok.clone(),
            imports: Arc::default(),
            messages: messages.clone(),
            beans: beans.clone(),
            data: data.clone(),
        };
        let class_files = ClassFiles {
            router: router.clone(),
            runtime: Handle::current(),
        };
        let debug = DebugExtension::with_router(router.clone(), autobuild);
        let forms = RefactorForms::new(router.clone(), Handle::current());
        let members = MemberRefactorings::new(
            root.clone(),
            router.clone(),
            providers.clone(),
            Handle::current(),
        );
        let move_members =
            crate::move_members::MoveMembers::new(root.clone(), router.clone(), Handle::current());
        let move_targets = crate::move_targets::MoveTargets::new(
            router.clone(),
            providers.clone(),
            Handle::current(),
        );
        let change_signature = crate::change_signature::ChangeSignatureSupport::new(
            router.clone(),
            providers.clone(),
            Handle::current(),
        );
        let stack = crate::stack::StackService::new(
            root.clone(),
            router.clone(),
            Handle::current(),
            model.clone(),
        );
        let java_messages = JavaMessages::new(
            providers.clone(),
            router.clone(),
            messages.clone(),
            Handle::current(),
        )
        .with_data(native_completion, native_navigation)
        .with_config(spring.clone());
        Ok(Self {
            sidecar,
            spec,
            router,
            root,
            runtime: Handle::current(),
            providers,
            server,
            importing,
            syntax: JavaSyntax::new(),
            class_files,
            library_sources,
            model,
            problems,
            debug,
            forms,
            members,
            move_members,
            move_targets,
            change_signature,
            lombok,
            stack,
            spring,
            messages,
            build_scripts,
            migration_files,
            java_messages,
            beans,
            data,
            checks,
        })
    }

    pub fn status(&self) -> watch::Receiver<Status> {
        self.sidecar.status()
    }

    /// The latest import failure, for the line of the status bar.
    pub fn watch_import_failure(&self) -> watch::Receiver<Option<ImportFailure>> {
        self.problems.failures.clone()
    }

    /// Whether a Gradle import runs now, for the line of the status bar.
    pub fn watch_importing(&self) -> watch::Receiver<bool> {
        self.importing.clone()
    }

    /// Starts the server again after it stopped, for example for low memory.
    pub(crate) fn restart(&self) -> bool {
        self.sidecar.restart()
    }

    /// The most heap the server takes from its next start on. A running server keeps the
    /// heap it started with.
    pub(crate) fn set_max_heap_mb(&self, megabytes: Option<u32>) {
        self.spec.set_max_heap_mb(megabytes);
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
    /// a running server starts again with it, and the configuration metadata of its
    /// class path is read again. The test support sets the model the same way.
    pub fn set_project_model(&self, model: &Arc<project_model::ProjectModel>) {
        if let Ok(mut stored) = self.model.lock() {
            *stored = Some(model.clone());
        }
        self.lombok.project_model(model.clone());
        self.spring.set_model(model);
        self.messages.set_model(model);
        self.beans.set_model(model);
        self.data.set_model(model);
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

/// The text of the document of a place, from an open editor or from the disk.
fn spot_text(router: &Router, spot: &DocumentSpot) -> Option<(PathBuf, String)> {
    let uri = Uri::from_str(&spot.uri).ok()?;
    let path = file_path(&uri)?;
    let text = router
        .text(&uri)
        .map(|text| text.to_string())
        .or_else(|| std::fs::read_to_string(&path).ok())?;
    Some((path, text))
}

impl Provider<TestFormQuery, TestForm> for JavaProject {
    fn start(&self, query: TestFormQuery, cancel: Cancel, mut reply: Reply<TestForm>) {
        let root = self.root.clone();
        let model = self.model.lock().ok().and_then(|model| model.clone());
        let router = self.router.clone();
        self.runtime.spawn(async move {
            let form = if cancel.is_cancelled() {
                return;
            } else if let Some((path, text)) = spot_text(&router, &query.0) {
                crate::test_creation::form_for(&root, model.as_deref(), &path, &text)
            } else {
                TestForm {
                    error: Some("The file is not open".to_owned()),
                    ..TestForm::default()
                }
            };
            let _ = reply.send(Answer::new(form, Confidence::Certain, SOURCE));
        });
    }
}

impl Provider<TestCreateRequest, TestCreateOutcome> for JavaProject {
    fn start(&self, query: TestCreateRequest, cancel: Cancel, mut reply: Reply<TestCreateOutcome>) {
        self.runtime.spawn(async move {
            if cancel.is_cancelled() {
                return;
            }
            let outcome = crate::test_creation::create(&query);
            let _ = reply.send(Answer::new(outcome, Confidence::Certain, SOURCE));
        });
    }
}

impl Provider<TestFixRequest, TestFixOutcome> for JavaProject {
    fn start(&self, query: TestFixRequest, cancel: Cancel, mut reply: Reply<TestFixOutcome>) {
        let root = self.root.clone();
        let model = self.model.lock().ok().and_then(|model| model.clone());
        self.runtime.spawn(async move {
            let outcome = if cancel.is_cancelled() {
                return;
            } else if let Ok(uri) = Uri::from_str(&query.spot.uri)
                && let Some(path) = file_path(&uri)
            {
                crate::test_creation::fix(&root, model.as_deref(), &path, &query.framework)
            } else {
                TestFixOutcome {
                    error: Some("The file has no path".to_owned()),
                    ..TestFixOutcome::default()
                }
            };
            let _ = reply.send(Answer::new(outcome, Confidence::Certain, SOURCE));
        });
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

impl Provider<TypeHierarchyQuery, Vec<TypeHierarchyItem>> for JavaProject {
    fn start(
        &self,
        query: TypeHierarchyQuery,
        cancel: Cancel,
        mut reply: Reply<Vec<TypeHierarchyItem>>,
    ) {
        let providers = self.providers.clone();
        let router = self.router.clone();
        self.runtime.spawn(async move {
            let provider: &dyn Provider<TypeHierarchyQuery, Vec<TypeHierarchyItem>> =
                providers.as_ref();
            let mut replies = ask(provider, query);
            let Some(answer) = replies.next().await else {
                return;
            };
            if cancel.is_cancelled() {
                return;
            }
            let confidence = answer.confidence;
            let mut items = answer.value;
            // A server names only a class for a record or an annotation; the source
            // tells the kind and the `abstract` flag. One read serves every node of a
            // file.
            let mut texts: std::collections::HashMap<String, Option<String>> =
                std::collections::HashMap::new();
            for item in &mut items {
                let Some(place) = &item.place else {
                    continue;
                };
                let text = texts.entry(place.uri.clone()).or_insert_with(|| {
                    let uri = Uri::from_str(&place.uri).ok()?;
                    router.text(&uri).map(|text| text.to_string()).or_else(|| {
                        file_path(&uri).and_then(|path| std::fs::read_to_string(path).ok())
                    })
                });
                if let Some(text) = text.as_deref() {
                    refine_type_hierarchy_item(item, text);
                }
            }
            let _ = reply.send(Answer::new(items, confidence, SOURCE));
        });
    }
}

/// The kind and the `abstract` flag of a type from the modifiers of its declaration.
fn refine_type_hierarchy_item(item: &mut TypeHierarchyItem, text: &str) {
    let Some(place) = &item.place else {
        return;
    };
    let offset = offset_in(text, place.span.start);
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let head = &text[line_start..offset];
    let word = |name: &str| {
        head.split(|character: char| {
            !character.is_alphanumeric() && character != '@' && character != '_'
        })
        .any(|found| found == name)
    };
    if head.contains("@interface") {
        item.kind = TypeKind::Annotation;
    } else if word("record") {
        item.kind = TypeKind::Record;
    } else if word("enum") {
        item.kind = TypeKind::Enum;
    } else if word("interface") {
        item.kind = TypeKind::Interface;
    }
    item.abstract_type = item.kind == TypeKind::Class && word("abstract");
    // The server names the package and the type together; the window shows the package
    // beside the name.
    let owner = item
        .detail
        .as_deref()
        .and_then(|detail| detail.strip_suffix(&format!(".{}", item.name)))
        .map(str::to_owned);
    if let Some(owner) = owner {
        item.detail = Some(owner);
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
                // A server may list the types of a file without their package. The
                // debugger names a field or a method by the type that owns it, and that
                // name has the package: the outline puts the package around the types.
                let package = package_of(&text);
                if !package.is_empty()
                    && !symbols
                        .iter()
                        .any(|symbol| symbol.kind == SymbolKind::Module)
                {
                    symbols = vec![FileSymbol {
                        name: package,
                        detail: None,
                        kind: SymbolKind::Module,
                        range: TextSpan::default(),
                        name_range: TextSpan::default(),
                        children: symbols,
                    }];
                }
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

#[cfg(test)]
#[path = "project_log_tests.rs"]
mod project_log_tests;

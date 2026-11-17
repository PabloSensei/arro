//! Newer versions of the dependencies of a build file without a window: a stand-in
//! language answers with the versions of a repository, and the window shows the hint,
//! applies the fix, takes it back with one Ctrl+Z, lists the versions while one is typed
//! and marks a direct dependency of the analyzer. A language that has no repository to
//! ask shows nothing and says nothing.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use base::Cancel;
use editor_view::{Languages, Spot};
use lang_api::{
    ActionEntry, ActionKind, ActionProvider, ActionQuery, Answer, CompletionEntry, CompletionKind,
    CompletionList, CompletionProvider, Confidence, DependencyUpdatesProvider, DependencyVersion,
    DependencyVersionQuery, DiagnosticEntry, DiagnosticsProvider, DiagnosticsQuery, DocumentEdit,
    DocumentSpot, LanguageFn, LanguageId, LanguageSupport, Provider, Queries, SemanticServer,
    Severity, SourceEdit, SourceId, SyntaxProvider, TextPoint, TextSpan, offset_in,
};
use proc_sup::ProcessSpec;
use project_model::{
    BuildGroup, BuildModule, BuildSystem, BuildTask, Dependency, DependencyScope, Module,
    ModuleDependencies, ProjectModel, TaskOptions,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::{Settings, TASKS_TOOL, Workspace};

use crate::run::ProjectSetup;
use crate::script::ActiveBuildPanel;
use crate::window_tests::{buffer_of, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_run::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const SOURCE: SourceId = SourceId::new("stub-versions");

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

/// One version place of a build file: the bytes of the value and the value itself. A
/// value of `${name}` stands for the value of the property of that name.
fn places(text: &str) -> Vec<(usize, usize, String)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(open) = text[from..].find("<version>") {
        let value_start = from + open + "<version>".len();
        let Some(close) = text[value_start..]
            .find("</version>")
            .map(|at| value_start + at)
        else {
            break;
        };
        let raw = &text[value_start..close];
        let (start, value) = match raw
            .strip_prefix("${")
            .and_then(|rest| rest.strip_suffix('}'))
        {
            Some(name) => {
                let open_tag = format!("<{name}>");
                let close_tag = format!("</{name}>");
                match text.find(&open_tag) {
                    Some(at) => {
                        let start = at + open_tag.len();
                        let end = text[start..]
                            .find(&close_tag)
                            .map_or(start, |found| start + found);
                        (start, text[start..end].to_owned())
                    }
                    None => (value_start, raw.to_owned()),
                }
            }
            None => (value_start, raw.to_owned()),
        };
        let leading = value.len() - value.trim_start().len();
        let value = value.trim().to_owned();
        if !value.is_empty() {
            found.push((start + leading, start + leading + value.len(), value));
        }
        from = close + "</version>".len();
    }
    found
}

/// The numeric parts of a version, enough for the stand-in versions of these tests.
fn numbers(version: &str) -> Vec<u64> {
    version
        .split('.')
        .map_while(|part| part.parse().ok())
        .collect()
}

fn newer(version: &str, current: &str) -> bool {
    !version.contains('-') && numbers(version) > numbers(current)
}

fn newest_stable(versions: &[String], current: &str) -> Option<String> {
    versions
        .iter()
        .filter(|version| newer(version, current))
        .max_by_key(|version| numbers(version))
        .cloned()
}

fn patched(versions: &[String], current: &str) -> Option<String> {
    let current = numbers(current);
    if current.len() < 2 {
        return None;
    }
    versions
        .iter()
        .filter(|version| {
            let found = numbers(version);
            !version.contains('-')
                && found.len() >= 2
                && found[0] == current[0]
                && found[1] == current[1]
                && found > current
        })
        .max_by_key(|version| numbers(version))
        .cloned()
}

/// The stand-in language of these tests: it knows one repository and serves the list of
/// versions from memory. It never starts a process and never asks the network.
struct StubLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
    versions: Vec<String>,
    enabled: AtomicBool,
}

impl StubLanguage {
    fn new(versions: &[&str]) -> Arc<Self> {
        Arc::new(Self {
            syntax: NoSyntax(Queries {
                highlights: "",
                folds: "",
                indents: "",
                locals: "",
            }),
            documents: Mutex::new(HashMap::new()),
            versions: versions
                .iter()
                .map(|version| (*version).to_owned())
                .collect(),
            enabled: AtomicBool::new(true),
        })
    }

    fn text_of(&self, uri: &str) -> Option<String> {
        self.documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(uri)
            .map(|text| text.to_string())
    }

    fn place_at(&self, uri: &str, offset: usize) -> Option<(String, usize, usize, String)> {
        let text = self.text_of(uri)?;
        let (start, end, version) = places(&text)
            .into_iter()
            .find(|(start, end, _)| *start <= offset && offset <= *end)?;
        Some((text, start, end, version))
    }

    fn checks(&self, uri: &str) -> Vec<DiagnosticEntry> {
        if !self.enabled.load(Ordering::SeqCst) {
            return Vec::new();
        }
        let Some(text) = self.text_of(uri) else {
            return Vec::new();
        };
        places(&text)
            .into_iter()
            .filter_map(|(start, end, version)| {
                let latest = newest_stable(&self.versions, &version)?;
                Some(DiagnosticEntry {
                    span: span_in(&text, start, end),
                    severity: Severity::Information,
                    message: format!("Newer version available: {latest}"),
                    unnecessary: false,
                    code: None,
                })
            })
            .collect()
    }
}

impl LanguageSupport for StubLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("stub-versions")
    }

    fn matches(&self, path: &Path) -> bool {
        path.file_name().is_some_and(|name| {
            let name = name.to_string_lossy();
            name == "pom.xml" || name == "build.gradle" || name == "build.gradle.kts"
        })
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn diagnostics(&self) -> Option<&DiagnosticsProvider> {
        Some(self)
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self)
    }

    fn actions(&self) -> Option<&ActionProvider> {
        Some(self)
    }

    fn dependency_updates(&self) -> Option<&DependencyUpdatesProvider> {
        Some(self)
    }

    fn set_dependency_updates(&self, enabled: bool) {
        self.enabled.store(enabled, Ordering::SeqCst);
    }
}

impl SemanticServer for StubLanguage {
    fn open(&self, uri: &str, _version: i32, text: Arc<str>) {
        self.documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), text);
    }

    fn change(&self, uri: &str, _version: i32, text: Arc<str>, _edits: Vec<DocumentEdit>) {
        self.documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), text);
    }

    fn save(&self, _uri: &str) {}

    fn version(&self, uri: &str) -> Option<i32> {
        self.text_of(uri).map(|_| 0)
    }

    fn close(&self, uri: &str) {
        self.documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(uri);
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for StubLanguage {
    fn start(
        &self,
        query: DiagnosticsQuery,
        _: Cancel,
        mut reply: lang_api::Reply<Vec<DiagnosticEntry>>,
    ) {
        let entries = self.checks(&query.0);
        let _ = reply.send(Answer::new(entries, Confidence::Certain, SOURCE));
    }
}

impl Provider<DocumentSpot, CompletionList> for StubLanguage {
    fn start(&self, query: DocumentSpot, _: Cancel, mut reply: lang_api::Reply<CompletionList>) {
        let text = self.text_of(&query.uri).unwrap_or_default();
        let offset = offset_in(&text, query.point);
        let entries = self
            .place_at(&query.uri, offset)
            .map(|(text, start, end, _version)| {
                let prefix = &text[start..offset];
                let mut stable: Vec<&String> = self
                    .versions
                    .iter()
                    .filter(|version| !version.contains('-') && version.starts_with(prefix))
                    .collect();
                stable.sort_by_key(|version| std::cmp::Reverse(numbers(version)));
                let mut entries: Vec<CompletionEntry> = stable
                    .into_iter()
                    .enumerate()
                    .map(|(index, version)| CompletionEntry {
                        label: version.clone(),
                        detail: Some("stable".to_owned()),
                        insert_text: Some(version.clone()),
                        filter_text: Some(format!("{prefix}{version}")),
                        sort_text: Some(format!("0{index:04}")),
                        span: Some(span_in(&text, start, end)),
                        kind: CompletionKind::Value,
                        ..CompletionEntry::default()
                    })
                    .collect();
                let pre: Vec<&String> = self
                    .versions
                    .iter()
                    .filter(|version| version.contains('-') && version.starts_with(prefix))
                    .collect();
                entries.extend(pre.into_iter().map(|version| CompletionEntry {
                    label: version.clone(),
                    detail: Some("pre-release".to_owned()),
                    insert_text: Some(version.clone()),
                    filter_text: Some(format!("{prefix}{version}")),
                    span: Some(span_in(&text, start, end)),
                    kind: CompletionKind::Value,
                    deprecated: true,
                    ..CompletionEntry::default()
                }));
                entries
            })
            .unwrap_or_default();
        let _ = reply.send(Answer::new(
            CompletionList {
                entries,
                complete: true,
            },
            Confidence::Certain,
            SOURCE,
        ));
    }
}

impl Provider<ActionQuery, Vec<ActionEntry>> for StubLanguage {
    fn start(&self, query: ActionQuery, _: Cancel, mut reply: lang_api::Reply<Vec<ActionEntry>>) {
        let text = self.text_of(&query.uri).unwrap_or_default();
        let offset = offset_in(&text, query.span.start);
        let entries = self
            .place_at(&query.uri, offset)
            .map(|(text, start, end, current)| {
                let mut entries = Vec::new();
                if let Some(latest) = newest_stable(&self.versions, &current) {
                    entries.push(fix(&text, &query.uri, start, end, &latest, true));
                }
                if let Some(patch) = patched(&self.versions, &current)
                    && Some(&patch) != newest_stable(&self.versions, &current).as_ref()
                {
                    entries.push(fix(&text, &query.uri, start, end, &patch, false));
                }
                entries
            })
            .unwrap_or_default();
        let _ = reply.send(Answer::new(entries, Confidence::Certain, SOURCE));
    }
}

fn fix(
    text: &str,
    uri: &str,
    start: usize,
    end: usize,
    version: &str,
    preferred: bool,
) -> ActionEntry {
    let title = if preferred {
        format!("Update to {version}")
    } else {
        format!("Update to latest patch {version}")
    };
    ActionEntry {
        title,
        kind: Some(ActionKind::QuickFix),
        code: Some("stub.update".to_owned()),
        preferred,
        edits: vec![SourceEdit {
            uri: uri.to_owned(),
            edits: vec![DocumentEdit {
                span: span_in(text, start, end),
                text: version.to_owned(),
            }],
        }],
        renames: Vec::new(),
        handle: None,
        resolve: None,
    }
}

impl Provider<DependencyVersionQuery, Vec<DependencyVersion>> for StubLanguage {
    fn start(
        &self,
        query: DependencyVersionQuery,
        _: Cancel,
        mut reply: lang_api::Reply<Vec<DependencyVersion>>,
    ) {
        let enabled = self.enabled.load(Ordering::SeqCst);
        let found = query
            .dependencies
            .into_iter()
            .map(|(name, current)| DependencyVersion {
                latest: (enabled && name.contains("starter-web"))
                    .then(|| newest_stable(&self.versions, &current))
                    .flatten(),
                name,
                current,
            })
            .collect();
        let _ = reply.send(Answer::new(found, Confidence::Certain, SOURCE));
    }
}

/// The build tool of the analyzer test: one module with one direct dependency.
struct StubTool {
    model: ProjectModel,
}

impl StubTool {
    fn new(root: &Path) -> Self {
        let module = Module {
            id: "app".into(),
            name: "app".into(),
            dir: root.join("app"),
            ..Module::default()
        };
        Self {
            model: ProjectModel {
                root: root.to_path_buf(),
                tool: "stub".into(),
                resolved: true,
                modules: vec![module],
                dependencies: vec![ModuleDependencies {
                    module: "app".into(),
                    build_file: Some(root.join("app").join("pom.xml")),
                    scopes: vec![DependencyScope {
                        name: "compile".into(),
                        roots: vec![Dependency {
                            coordinates: "org.springframework.boot:spring-boot-starter-web:3.5.0"
                                .into(),
                            scope: "compile".into(),
                            ..Dependency::default()
                        }],
                    }],
                }],
                ..ProjectModel::default()
            },
        }
    }
}

impl BuildSystem for StubTool {
    fn detect(&self, _: &Path) -> bool {
        true
    }

    fn scan(&self, _: &Path) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn sync(&self, _: &Path, _: &Cancel) -> base::Result<ProjectModel> {
        Ok(self.model.clone())
    }

    fn assemble(&self, _: &Path) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "exit", "0"]))
    }

    fn task_tree(&self, _: &ProjectModel) -> Vec<BuildModule> {
        vec![BuildModule {
            id: "app".into(),
            name: "app".into(),
            groups: vec![BuildGroup {
                name: "Lifecycle".into(),
                tasks: vec![BuildTask {
                    name: "compile".into(),
                    program: false,
                }],
            }],
        }]
    }

    fn task_in(
        &self,
        _: &Path,
        _: Option<&Module>,
        _: &str,
        _: TaskOptions,
    ) -> base::Result<ProcessSpec> {
        Ok(ProcessSpec::new("cmd").args(["/d", "/c", "echo", "task"]))
    }
}

fn project(name: &str, text: &str) -> PathBuf {
    let root = scratch(name);
    fs::write(root.join("pom.xml"), text).unwrap();
    root
}

fn open(
    cx: &mut TestAppContext,
    root: PathBuf,
    setup: ProjectSetup,
    language: Arc<StubLanguage>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(ui_kit::Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: |_| None,
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = workspace::Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: workspace::FileIcons {
            by_name: Vec::new(),
            fallback: "fileTypes/text",
        },
        background: true,
        info: Default::default(),
    };
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
        let mut workspace = Workspace::new_for_test(
            launch,
            workspace::Layout::default(),
            store,
            Vec::new(),
            window,
            cx,
        );
        install(
            &mut workspace,
            crate::index::SearchSetup::default(),
            setup,
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

fn wait_for_hint(cx: &mut TestAppContext, window: WindowHandle<Workspace>, path: &Path) {
    let view = editor_of(cx, window, path);
    wait_for(cx, "the hint of the version", |cx| {
        draw(cx, window);
        view.read_with(cx, |view, _| !view.diagnostic_marks().is_empty())
    });
}

#[ui_kit::gpui::test]
fn a_hint_stands_on_the_old_version_and_alt_enter_updates_it(cx: &mut TestAppContext) {
    let text = "<project>\n    <dependencies>\n        <dependency>\n            \
                <groupId>org.example</groupId>\n            <artifactId>demo</artifactId>\n            \
                <version>1.0</version>\n        </dependency>\n    </dependencies>\n</project>\n";
    let root = project("updates-hint", text);
    let path = root.join("pom.xml");
    let language = StubLanguage::new(&["1.0", "1.5", "2.0-M1"]);
    let window = open(cx, root, project_setup(scratch("cache")), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_hint(cx, window, &path);

    let marks = view.read_with(cx, |view, _| view.diagnostic_marks());
    assert_eq!(marks.len(), 1, "{marks:?}");
    let (severity, range) = &marks[0];
    assert_eq!(*severity, Severity::Information, "a weak warning");
    assert_eq!(
        &buffer_of(cx, &view)[range.clone()],
        "1.0",
        "the hint stands on the version"
    );

    let offset = text.find("1.0").unwrap() + 1;
    view.update(cx, |view, cx| view.go_to(Spot::Offset(offset), cx));
    press(cx, window, "alt-enter");
    draw(cx, window);
    press(cx, window, "enter");
    assert!(
        buffer_of(cx, &view).contains("<version>1.5</version>"),
        "the fix writes the newest stable version"
    );

    press(cx, window, "ctrl-z");
    assert!(
        buffer_of(cx, &view).contains("<version>1.0</version>"),
        "one Ctrl+Z takes the fix back"
    );
}

#[ui_kit::gpui::test]
fn a_version_property_is_updated_in_the_properties_section(cx: &mut TestAppContext) {
    let text = "<project>\n    <properties>\n        <demo.version>1.0</demo.version>\n    \
                </properties>\n    <dependencies>\n        <dependency>\n            \
                <groupId>org.example</groupId>\n            <artifactId>demo</artifactId>\n            \
                <version>${demo.version}</version>\n        </dependency>\n    </dependencies>\n</project>\n";
    let root = project("updates-property", text);
    let path = root.join("pom.xml");
    let language = StubLanguage::new(&["1.0", "1.5"]);
    let window = open(cx, root, project_setup(scratch("cache")), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_hint(cx, window, &path);

    let offset = text.find("1.0").unwrap() + 1;
    view.update(cx, |view, cx| view.go_to(Spot::Offset(offset), cx));
    press(cx, window, "alt-enter");
    draw(cx, window);
    press(cx, window, "enter");

    let updated = buffer_of(cx, &view);
    assert!(
        updated.contains("<demo.version>1.5</demo.version>"),
        "the property holds the new version: {updated}"
    );
    assert!(
        updated.contains("<version>${demo.version}</version>"),
        "the dependency keeps its reference: {updated}"
    );
}

#[ui_kit::gpui::test]
fn completion_lists_the_versions_of_the_repository(cx: &mut TestAppContext) {
    let text = "<project>\n    <dependencies>\n        <dependency>\n            \
                <groupId>org.example</groupId>\n            <artifactId>demo</artifactId>\n            \
                <version>1.</version>\n        </dependency>\n    </dependencies>\n</project>\n";
    let root = project("updates-completion", text);
    let path = root.join("pom.xml");
    let language = StubLanguage::new(&["1.0", "1.5", "2.0-M1"]);
    let window = open(cx, root, project_setup(scratch("cache")), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_hint(cx, window, &path);

    let offset = text.find("1.<").unwrap() + 2;
    view.update(cx, |view, cx| view.go_to(Spot::Offset(offset), cx));
    press(cx, window, "ctrl-space");
    draw(cx, window);
    press(cx, window, "enter");

    let updated = buffer_of(cx, &view);
    assert!(
        updated.contains("<version>1.5</version>"),
        "the list offered the newest version of the prefix: {updated}"
    );
}

#[ui_kit::gpui::test]
fn an_offline_build_file_shows_no_hint_and_no_error(cx: &mut TestAppContext) {
    let text = "<project>\n    <dependencies>\n        <dependency>\n            \
                <groupId>org.example</groupId>\n            <artifactId>demo</artifactId>\n            \
                <version>1.0</version>\n        </dependency>\n    </dependencies>\n</project>\n";
    let root = project("updates-offline", text);
    let path = root.join("pom.xml");
    let language = StubLanguage::new(&[]);
    let window = open(cx, root, project_setup(scratch("cache")), language);
    cx.run_until_parked();
    draw(cx, window);
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    for _ in 0..20 {
        cx.executor()
            .advance_clock(std::time::Duration::from_millis(20));
        cx.run_until_parked();
        draw(cx, window);
    }

    assert!(
        view.read_with(cx, |view, _| view.diagnostic_marks().is_empty()),
        "no repository means no hint"
    );
    let offset = text.find("1.0").unwrap() + 1;
    view.update(cx, |view, cx| view.go_to(Spot::Offset(offset), cx));
    let before = buffer_of(cx, &view);
    press(cx, window, "alt-enter");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.debug_bounds("actions-popup").is_none(),
        "there is nothing to fix, so no list opens"
    );
    assert_eq!(buffer_of(cx, &view), before, "Alt+Enter changes nothing");
}

#[ui_kit::gpui::test]
fn the_analyzer_marks_a_direct_dependency_with_a_newer_version(cx: &mut TestAppContext) {
    let root = scratch("updates-analyzer");
    fs::create_dir_all(root.join("app")).unwrap();
    fs::write(root.join("app").join("pom.xml"), "<project/>\n").unwrap();
    let language = StubLanguage::new(&["3.5.0", "4.0.0"]);
    let setup = ProjectSetup {
        tool: Some(Arc::new(StubTool::new(&root))),
        providers: Vec::new(),
        tests: None,
        test_sites: |_, _| Vec::new(),
        cache_root: scratch("updates-analyzer-cache"),
        templates: Vec::new(),
        skipped_folders: Vec::new(),
        environment: None,
        debug: None,
    };
    let window = open(cx, root, setup, language);
    let panel = cx.read(|cx| cx.global::<ActiveBuildPanel>().0.clone());
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(TASKS_TOOL, window, cx)
        })
        .unwrap();
    draw(cx, window);
    wait_for(cx, "the task tree", |cx| {
        !panel.read_with(cx, |panel, cx| panel.labels(cx)).is_empty()
    });
    panel.update(cx, |panel, cx| panel.show_analyzer_now(cx));

    wait_for(cx, "the update mark", |cx| {
        draw(cx, window);
        !panel
            .update(cx, |panel, _| panel.analyzer_marks())
            .is_empty()
    });
    let marks = panel.update(cx, |panel, _| panel.analyzer_marks());
    assert_eq!(
        marks,
        [(
            "org.springframework.boot:spring-boot-starter-web:3.5.0".to_owned(),
            "4.0.0".to_owned()
        )],
        "the direct dependency says which version is newer"
    );
}

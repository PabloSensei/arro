//! The window on a Quarkus or Micronaut project: the badge of a CDI bean and the jump
//! from an injection, the keys of Quarkus in a property file, and the handlers of JAX-RS,
//! Micronaut and Quarkus Reactive Routes in the Endpoints window with the port of the
//! module. The language of the test reads the real sources through `window_language`, so
//! the annotations of the test files are the ones the IDE reads.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base::Cancel;
use lang_api::{
    Answer, BeanModel, BeanProvider, BeanQuery, Confidence, EndpointProvider, EndpointScan,
    LanguageFn, LanguageId, LanguageSupport, Provider, Queries, SourceId, SyntaxProvider,
};
use project_model::{Dependency, DependencyScope, Module, ModuleDependencies, ProjectModel, Unit};
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use ui_kit::gpui::VisualTestContext;
use ui_kit::gpui::WindowHandle;
use workspace::{ENDPOINTS_TOOL, Workspace};

use crate::beans::ActiveBeans;
use crate::endpoints::ActiveEndpoints;
use crate::window_tests::{
    active_path, buffer_of, click_gutter, draw, editor_of, open_path, open_window, press, scratch,
    wait_for_signal,
};

const SOURCE: SourceId = SourceId::new("jakarta-language");
/// The hint of a badge that opens the beans a point receives.
const POINT_HINT: &str = "Navigate to autowired candidates";

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

/// The language of a source folder: the model of the beans read once from its files, the
/// handlers read from each text, and the configuration files it brings for the project.
struct JakartaLanguage {
    syntax: NoSyntax,
    source_root: PathBuf,
    model: BeanModel,
    config: Arc<dyn LanguageSupport>,
}

impl JakartaLanguage {
    fn new(source_root: PathBuf, model: BeanModel, config: Arc<dyn LanguageSupport>) -> Self {
        Self {
            syntax: NoSyntax(Queries::default()),
            source_root,
            model,
            config,
        }
    }
}

impl Provider<BeanQuery, BeanModel> for JakartaLanguage {
    fn start(&self, _: BeanQuery, _: Cancel, mut reply: lang_api::Reply<BeanModel>) {
        let _ = reply.send(Answer::new(self.model.clone(), Confidence::Certain, SOURCE));
    }
}

impl BeanProvider for JakartaLanguage {
    fn sources_changed(&self) {}
}

impl EndpointProvider for JakartaLanguage {
    fn file_endpoints(&self, path: &Path, text: &str) -> EndpointScan {
        window_language::endpoint_scan(path, text)
    }
}

impl LanguageSupport for JakartaLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("jakarta-window")
    }

    fn matches(&self, path: &Path) -> bool {
        path.starts_with(&self.source_root)
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn module_of(&self, _: &Path) -> Option<String> {
        Some("shop".to_owned())
    }

    fn beans(&self) -> Option<&dyn BeanProvider> {
        Some(self)
    }

    fn endpoints(&self) -> Option<&dyn EndpointProvider> {
        Some(self)
    }

    fn embedded_languages(&self) -> Vec<Arc<dyn LanguageSupport>> {
        vec![self.config.clone()]
    }
}

/// The model of a one module project whose dependency names the framework.
fn project_model(root: &Path, coordinates: &str) -> ProjectModel {
    let source_root = window_language::spring_source_root(root);
    let resources = root.join("src").join("main").join("resources");
    ProjectModel {
        root: root.to_path_buf(),
        tool: "pom".to_owned(),
        resolved: true,
        modules: vec![Module {
            id: "app".to_owned(),
            name: "shop".to_owned(),
            dir: root.to_path_buf(),
            units: vec![Unit {
                name: "main".to_owned(),
                source_roots: vec![source_root],
                resource_roots: vec![resources],
                ..Unit::default()
            }],
        }],
        dependencies: vec![ModuleDependencies {
            module: "app".to_owned(),
            scopes: vec![DependencyScope {
                name: "compile".to_owned(),
                roots: vec![Dependency {
                    coordinates: coordinates.to_owned(),
                    ..Dependency::default()
                }],
            }],
            ..ModuleDependencies::default()
        }],
        ..ProjectModel::default()
    }
}

fn source(root: &Path, name: &str, text: &str) -> PathBuf {
    window_language::write_test_source(root, name, text)
}

fn resource(root: &Path, name: &str, text: &str) -> PathBuf {
    let path = root.join("src").join("main").join("resources").join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}

/// The build file of the project, whose dependency names the framework together with the
/// model the test builds.
fn project_file(root: &Path, group: &str) {
    let path = root.join("pom.xml");
    fs::write(
        &path,
        format!(
            "<project><modelVersion>4.0.0</modelVersion>\
             <groupId>{group}</groupId><artifactId>shop</artifactId><version>1.0</version>\
             </project>"
        ),
    )
    .unwrap();
}

fn open(
    cx: &mut TestAppContext,
    root: PathBuf,
    model: BeanModel,
    config: Arc<dyn LanguageSupport>,
) -> WindowHandle<Workspace> {
    let window = open_window(cx, root.clone());
    let language = Arc::new(JakartaLanguage::new(
        window_language::spring_source_root(&root),
        model,
        config,
    ));
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(
                language.clone() as Arc<dyn LanguageSupport>
            ));
            if let Some(panel) = cx.try_global::<ActiveBeans>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(language, cx));
            }
            if let Some(panel) = cx.try_global::<ActiveEndpoints>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(cx));
            }
        })
        .unwrap();
    cx.run_until_parked();
    window
}

fn marks(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
) -> Vec<(u32, String)> {
    let view = editor_of(cx, window, path);
    view.read_with(cx, |view, _| {
        view.place_marks()
            .iter()
            .map(|mark| (mark.line, mark.hint.to_string()))
            .collect()
    })
}

fn line_of(text: &str, needle: &str) -> u32 {
    text.lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("no line with {needle}")) as u32
}

fn popup_shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds("completion-popup").is_some()
}

fn panel_rows(cx: &mut TestAppContext) -> Vec<String> {
    cx.update(|cx| {
        cx.try_global::<ActiveEndpoints>()
            .map(|panel| {
                panel
                    .0
                    .read(cx)
                    .rows()
                    .iter()
                    .map(|row| row.label.clone())
                    .collect()
            })
            .unwrap_or_default()
    })
}

fn row_of(cx: &mut TestAppContext, label: &str) -> usize {
    cx.update(|cx| {
        cx.global::<ActiveEndpoints>()
            .0
            .read(cx)
            .rows()
            .iter()
            .position(|row| row.label == label)
            .unwrap_or_else(|| panic!("the window has no row {label}"))
    })
}

const SERVICE: &str = r#"package com.example;

import jakarta.enterprise.context.ApplicationScoped;

@ApplicationScoped
public class OwnerService {
}
"#;

const CONTROLLER: &str = r#"package com.example;

import jakarta.enterprise.context.ApplicationScoped;
import jakarta.inject.Inject;

@ApplicationScoped
public class OwnerController {
    @Inject
    OwnerService service;
}
"#;

const RESOURCE: &str = r#"package com.example;

import jakarta.ws.rs.GET;
import jakarta.ws.rs.Path;

@Path("/owners")
public class OwnerResource {
    @GET
    @Path("/{id}")
    public String find() {
        return "";
    }
}
"#;

const MICRONAUT: &str = r#"package com.example;

import io.micronaut.http.annotation.Controller;
import io.micronaut.http.annotation.Get;

@Controller("/owners")
public class OwnerController {
    @Get
    public String index() {
        return "";
    }
}
"#;

#[ui_kit::gpui::test]
fn a_quarkus_bean_gets_a_badge_and_an_injection_jumps_to_it(cx: &mut TestAppContext) {
    let root = scratch("jakarta-beans");
    project_file(&root, "io.quarkus");
    let service = source(&root, "OwnerService", SERVICE);
    let controller = source(&root, "OwnerController", CONTROLLER);
    let model = window_language::bean_model(&project_model(&root, "io.quarkus:quarkus-arc:3.9.0"));
    let config = window_language::spring_config(&root, Vec::new());
    let window = open(cx, root.clone(), model, config);

    open_path(cx, window, &service);
    let service_line = line_of(SERVICE, "class OwnerService");
    wait_for_signal(cx, "the badge of the bean", |cx| {
        marks(cx, window, &service)
            .iter()
            .any(|(line, _)| *line == service_line)
    });

    open_path(cx, window, &controller);
    let point_line = line_of(CONTROLLER, "OwnerService service");
    wait_for_signal(cx, "the badge of the injection", |cx| {
        marks(cx, window, &controller)
            .iter()
            .any(|(line, hint)| *line == point_line && hint == POINT_HINT)
    });
    draw(cx, window);
    click_gutter(cx, window, 0, point_line as usize);
    assert_eq!(active_path(cx, window), Some(service.clone()));
    let view = editor_of(cx, window, &service);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret_line()),
        Some(service_line as usize)
    );
}

#[ui_kit::gpui::test]
fn the_keys_of_quarkus_are_offered_in_a_property_file(cx: &mut TestAppContext) {
    let root = scratch("jakarta-quarkus-keys");
    project_file(&root, "io.quarkus");
    let file = resource(&root, "application.properties", "quarkus.http.p");
    let jar = window_language::write_quarkus_metadata_jar(&root);
    let config = window_language::spring_config(&root, vec![jar]);
    let model = window_language::bean_model(&project_model(&root, "io.quarkus:quarkus-arc:3.9.0"));
    let window = open(cx, root.clone(), model, config);
    open_path(cx, window, &file);
    let view = editor_of(cx, window, &file);
    view.update(cx, |view, cx| {
        view.go_to(
            editor_view::Spot::Line {
                line: 0,
                column: "quarkus.http.p".len(),
            },
            cx,
        )
    });
    cx.run_until_parked();

    press(cx, window, "ctrl-space");
    wait_for_signal(cx, "the offered keys", |cx| {
        draw(cx, window);
        popup_shown(cx, window)
    });
    press(cx, window, "enter");
    assert_eq!(buffer_of(cx, &view), "quarkus.http.port");
}

#[ui_kit::gpui::test]
fn a_jax_rs_handler_of_a_quarkus_module_is_listed_with_its_method(cx: &mut TestAppContext) {
    let root = scratch("jakarta-endpoints");
    project_file(&root, "io.quarkus");
    let _ = source(&root, "OwnerResource", RESOURCE);
    let model = window_language::bean_model(&project_model(
        &root,
        "io.quarkus:quarkus-resteasy-reactive:3.9.0",
    ));
    let config = window_language::spring_config(&root, Vec::new());
    let window = open(cx, root.clone(), model, config);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(ENDPOINTS_TOOL, window, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the handler of the resource", |cx| {
        panel_rows(cx).iter().any(|row| row == "GET /owners/{id}")
    });
    let rows = panel_rows(cx);
    assert!(rows.contains(&"OwnerResource".to_owned()), "{rows:?}");
}

#[ui_kit::gpui::test]
fn a_micronaut_controller_is_listed_and_its_port_comes_from_the_config(cx: &mut TestAppContext) {
    let root = scratch("jakarta-micronaut");
    project_file(&root, "io.micronaut");
    let _ = source(&root, "OwnerController", MICRONAUT);
    resource(
        &root,
        "application.yml",
        "micronaut:\n  server:\n    port: 8081\n",
    );
    let model = window_language::bean_model(&project_model(
        &root,
        "io.micronaut:micronaut-http-server:4.2.0",
    ));
    let config = window_language::spring_config(&root, Vec::new());
    let window = open(cx, root.clone(), model, config);
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(ENDPOINTS_TOOL, window, cx)
        })
        .unwrap();
    wait_for_signal(cx, "the handler of the controller", |cx| {
        panel_rows(cx).iter().any(|row| row == "GET /owners")
    });
    let state = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let request = state.join("scratches").join("generated-requests.http");
    fs::remove_file(&request).ok();
    let row = row_of(cx, "GET /owners");
    window
        .update(cx, |_, window, cx| {
            let panel = cx.global::<ActiveEndpoints>().0.clone();
            panel.update(cx, |panel, cx| panel.generate_row(row, window, cx));
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    let text = fs::read_to_string(&request).expect("the request file is written");
    assert!(
        text.contains("GET http://localhost:8081/owners\n"),
        "{text:?}"
    );
}

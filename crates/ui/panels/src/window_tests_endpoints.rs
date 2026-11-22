//! The Endpoints window without a real language: a support that reads a small marker
//! format from the sample files. The tests check the tree, the filters, the jump and the
//! request the window writes into the HTTP client.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang_api::{
    Endpoint, EndpointParameter, EndpointParameterKind, EndpointProvider, EndpointScan, HttpMethod,
    LanguageFn, LanguageId, LanguageSupport, Queries, SyntaxProvider,
};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{ENDPOINTS_TOOL, Workspace};

use crate::endpoints::ActiveEndpoints;
use crate::window_tests::{
    active_path, click, draw, editor_of, open_path, open_window, press, scratch, type_text,
    wait_for_signal,
};

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

/// The language of the tests: every line that starts with `@METHOD "path" name` is a
/// handler. The rest of the line names a query or a body parameter.
struct EndpointLanguage {
    syntax: NoSyntax,
}

impl EndpointLanguage {
    fn new() -> Self {
        Self {
            syntax: NoSyntax(Queries::default()),
        }
    }
}

fn parameter(part: &str) -> Option<EndpointParameter> {
    if let Some(name) = part.strip_prefix("query=") {
        return Some(EndpointParameter {
            name: name.to_owned(),
            kind: EndpointParameterKind::Query,
            type_name: None,
        });
    }
    let name = part.strip_prefix("body=")?;
    Some(EndpointParameter {
        name: "body".to_owned(),
        kind: EndpointParameterKind::Body,
        type_name: Some(name.to_owned()),
    })
}

fn scan(file: &Path, text: &str) -> Vec<Endpoint> {
    let class = text
        .lines()
        .find_map(|line| {
            let rest = line.trim().strip_prefix("class ")?;
            Some(
                rest.split_whitespace()
                    .next()
                    .unwrap_or("Sample")
                    .trim_end_matches('{')
                    .to_owned(),
            )
        })
        .unwrap_or_else(|| "Sample".to_owned());
    let class = format!("demo.{class}");
    let mut found = Vec::new();
    let mut offset = 0;
    for (line_index, line) in text.split_inclusive('\n').enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix('@') {
            let mut parts = rest.split_whitespace();
            if let Some(method) = parts.next().and_then(HttpMethod::parse) {
                let path = parts.next().unwrap_or("/").trim_matches('"').to_owned();
                let name = parts.next().unwrap_or("handle");
                let name_at = line.find(name).unwrap_or(0);
                found.push(Endpoint {
                    method,
                    path,
                    class: class.clone(),
                    file: file.to_path_buf(),
                    line: line_index as u32,
                    name_range: offset + name_at..offset + name_at + name.len(),
                    parameters: parts.filter_map(parameter).collect(),
                });
            }
        }
        offset += line.len();
    }
    found
}

impl EndpointProvider for EndpointLanguage {
    fn file_endpoints(&self, path: &Path, text: &str) -> EndpointScan {
        EndpointScan {
            port: Some(8081),
            endpoints: scan(path, text),
        }
    }
}

impl LanguageSupport for EndpointLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("fake-endpoints")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("txt"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn module_of(&self, _: &Path) -> Option<String> {
        Some("demo".to_owned())
    }

    fn endpoints(&self) -> Option<&dyn EndpointProvider> {
        Some(self)
    }
}

fn project(name: &str) -> PathBuf {
    let root = scratch(name);
    let owners = root.join("src").join("Owners.txt");
    std::fs::create_dir_all(owners.parent().unwrap()).unwrap();
    std::fs::write(
        &owners,
        "class Owners {\n@GET \"/owners/{ownerId}\" show\n@POST \"/owners\" save query=name body=Owner\n}\n",
    )
    .unwrap();
    std::fs::write(
        root.join("src").join("Vets.txt"),
        "class Vets {\n@GET \"/vets.html\" list\n}\n",
    )
    .unwrap();
    root
}

pub(crate) fn open(cx: &mut TestAppContext, name: &str) -> (WindowHandle<Workspace>, PathBuf) {
    let root = project(name);
    let window = open_window(cx, root.clone());
    let language = Arc::new(EndpointLanguage::new());
    window
        .update(cx, |_, _, cx| {
            cx.set_global(editor_view::Semantic(language as Arc<dyn LanguageSupport>));
            if let Some(panel) = cx.try_global::<ActiveEndpoints>() {
                let panel = panel.0.clone();
                panel.update(cx, |panel, cx| panel.language_ready(cx));
            }
        })
        .unwrap();
    window
        .update(cx, |workspace, window, cx| {
            workspace.show_tool(ENDPOINTS_TOOL, window, cx)
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    wait_for_signal(cx, "the handlers", |cx| panel_rows(cx).len() > 3);
    (window, root)
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

#[ui_kit::gpui::test]
fn the_window_lists_the_modules_types_and_handlers_of_the_project(cx: &mut TestAppContext) {
    let (_window, _root) = open(cx, "endpoints-tree");
    let rows = panel_rows(cx);
    assert!(
        rows.contains(&"demo".to_owned()),
        "the module row: {rows:?}"
    );
    assert!(
        rows.contains(&"Owners".to_owned()),
        "the type row: {rows:?}"
    );
    assert!(rows.contains(&"Vets".to_owned()), "the type row: {rows:?}");
    assert!(
        rows.contains(&"GET /owners/{ownerId}".to_owned()),
        "the handler row: {rows:?}"
    );
    assert!(
        rows.contains(&"POST /owners".to_owned()),
        "the handler row: {rows:?}"
    );
    assert!(
        rows.contains(&"GET /vets.html".to_owned()),
        "the handler row: {rows:?}"
    );
}

#[ui_kit::gpui::test]
fn typing_a_filter_keeps_the_matching_handlers(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "endpoints-filter");
    click(cx, window, "endpoints-filter");
    type_text(cx, window, "vets");
    let rows = panel_rows(cx);
    assert!(rows.contains(&"GET /vets.html".to_owned()), "{rows:?}");
    assert!(
        !rows.contains(&"GET /owners/{ownerId}".to_owned()),
        "{rows:?}"
    );
    assert!(!rows.contains(&"Owners".to_owned()), "{rows:?}");
}

#[ui_kit::gpui::test]
fn the_method_filter_keeps_one_method(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "endpoints-method");
    click(cx, window, "endpoints-method-GET");
    let rows = panel_rows(cx);
    assert!(
        rows.contains(&"GET /owners/{ownerId}".to_owned()),
        "{rows:?}"
    );
    assert!(rows.contains(&"GET /vets.html".to_owned()), "{rows:?}");
    assert!(!rows.contains(&"POST /owners".to_owned()), "{rows:?}");
    click(cx, window, "endpoints-method-All");
    let rows = panel_rows(cx);
    assert!(rows.contains(&"POST /owners".to_owned()), "{rows:?}");
}

#[ui_kit::gpui::test]
fn enter_opens_the_method_of_the_selected_handler(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "endpoints-enter");
    window
        .update(cx, |_, _, cx| {
            let panel = cx.global::<ActiveEndpoints>().0.clone();
            panel.update(cx, |panel, cx| panel.set_query("vets", cx));
        })
        .unwrap();
    cx.run_until_parked();
    press(cx, window, "enter");
    draw(cx, window);
    let path = active_path(cx, window).expect("a tab is open");
    assert!(path.ends_with("Vets.txt"), "{path:?}");
    let view = editor_of(cx, window, &path);
    assert_eq!(view.read_with(cx, |view, _| view.caret_line()), Some(1));
}

#[ui_kit::gpui::test]
fn generate_request_writes_the_method_the_path_and_the_variables(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "endpoints-generate");
    let state = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let request = state.join("scratches").join("generated-requests.http");
    std::fs::remove_file(&request).ok();
    let row = row_of(cx, "POST /owners");
    window
        .update(cx, |_, window, cx| {
            let panel = cx.global::<ActiveEndpoints>().0.clone();
            panel.update(cx, |panel, cx| panel.generate_row(row, window, cx));
        })
        .unwrap();
    cx.run_until_parked();
    draw(cx, window);
    let text = std::fs::read_to_string(&request).expect("the request file is written");
    assert!(
        text.contains("POST http://localhost:8081/owners?name={{name}}\n"),
        "{text:?}"
    );
    assert!(
        text.contains("Content-Type: application/json\n\n{}\n"),
        "{text:?}"
    );
    let path = active_path(cx, window).expect("the request file is open");
    assert_eq!(path, request);
    let view = editor_of(cx, window, &path);
    assert_eq!(view.read_with(cx, |view, _| view.caret_line()), Some(1));
}

#[ui_kit::gpui::test]
fn a_path_part_becomes_a_request_variable(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "endpoints-path-variable");
    let state = window
        .read_with(cx, |workspace, _| workspace.state_dir().to_path_buf())
        .unwrap();
    let request = state.join("scratches").join("generated-requests.http");
    std::fs::remove_file(&request).ok();
    let row = row_of(cx, "GET /owners/{ownerId}");
    window
        .update(cx, |_, window, cx| {
            let panel = cx.global::<ActiveEndpoints>().0.clone();
            panel.update(cx, |panel, cx| panel.generate_row(row, window, cx));
        })
        .unwrap();
    cx.run_until_parked();
    let text = std::fs::read_to_string(&request).expect("the request file is written");
    assert!(
        text.contains("GET http://localhost:8081/owners/{{ownerId}}\n"),
        "{text:?}"
    );
}

#[ui_kit::gpui::test]
fn an_edit_of_a_file_adds_its_handler(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "endpoints-edit");
    let path = root.join("src").join("Vets.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_signal(cx, "the open file", |cx| {
        view.read_with(cx, |view, _| view.text().is_some())
    });
    view.update(cx, |view, cx| {
        view.replace_text(0..0, "@GET \"/vets/{id}\" one\n", cx)
    });
    cx.run_until_parked();
    wait_for_signal(cx, "the edited handler", |cx| {
        panel_rows(cx).iter().any(|row| row == "GET /vets/{id}")
    });
    let rows = panel_rows(cx);
    assert!(rows.contains(&"GET /vets/{id}".to_owned()), "{rows:?}");
}

#[ui_kit::gpui::test]
fn the_gutter_marks_the_lines_of_the_handlers(cx: &mut TestAppContext) {
    let (window, root) = open(cx, "endpoints-gutter");
    let path = root.join("src").join("Vets.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    wait_for_signal(cx, "the gutter mark", |cx| {
        view.read_with(cx, |view, _| {
            view.endpoint_marks().iter().any(|mark| mark.line == 1)
        })
    });
}

#[ui_kit::gpui::test]
fn the_find_action_command_shows_the_window(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "endpoints-command");
    let ran = window
        .update(cx, |workspace, window, cx| {
            workspace.run_command("window.show_endpoints", window, cx)
        })
        .unwrap();
    assert!(ran, "the window lists the command for Find Action");
    let open = window
        .read_with(cx, |workspace, _| workspace.tool_open(ENDPOINTS_TOOL))
        .unwrap();
    assert!(open, "the command shows the Endpoints window");
}

#[ui_kit::gpui::test]
fn the_theme_of_the_window_is_installed(cx: &mut TestAppContext) {
    let (window, _root) = open(cx, "endpoints-theme");
    window
        .update(cx, |_, _, cx| {
            assert!(cx.try_global::<Theme>().is_some());
        })
        .unwrap();
}

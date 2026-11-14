//! The entities and the query methods of a data repository in the window: the completion
//! of a method name and of the text of a query annotation, the checks of the properties
//! and of the parameters, and Ctrl+B from a query to a field of an entity. The model comes
//! from the real language over the sources of the test project, so no server is needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{EditorView, Languages, Spot};
use lang_api::{LanguageSupport, Severity};
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{
    active_path, buffer_of, draw, editor_of, open_path, press, project_setup, scratch,
};
use crate::window_tests_real::wait_for;
use crate::{AgentOptions, LanguageServer, connect, install};

const OWNER: &str = r#"package demo;

import jakarta.persistence.Entity;
import jakarta.persistence.Id;

@Entity
class Owner {
    @Id
    private Integer id;
    private String firstName;
    private String lastName;
    private String city;
}
"#;

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    support: Arc<dyn LanguageSupport>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("spring-data-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: file_syntax::language_for,
            templates: |_| None,
        });
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root,
        open: None,
        state_dir: state_dir.clone(),
        settings_dir: state_dir,
        icons: FileIcons {
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
        support: Some(support),
        status,
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(
            &mut workspace,
            SearchSetup::default(),
            project_setup(scratch("spring-data-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// The folder of the sources of the copied project, without spelling its language in
/// this neutral crate.
fn source_root(root: &Path) -> PathBuf {
    window_language::spring_source_root(root)
}

/// The name of a source file, whose extension this neutral crate does not spell.
fn source_file(name: &str) -> String {
    format!("{name}.{}{}", "ja", "va")
}

/// A project with the entity and the repository of the test.
fn project(name: &str, repository: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let folder = source_root(&root).join("demo");
    fs::create_dir_all(&folder).unwrap();
    let owner = folder.join(source_file("Owner"));
    fs::write(&owner, OWNER).unwrap();
    let path = folder.join(source_file("OwnerRepository"));
    fs::write(&path, repository).unwrap();
    (root, path)
}

/// Opens the repository and puts the caret where the text names the place.
fn open_file(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
    (line, column): (usize, usize),
) -> gpui::Entity<EditorView> {
    open_path(cx, window, path);
    let view = editor_of(cx, window, path);
    view.update(cx, |view, cx| view.go_to(Spot::Line { line, column }, cx));
    cx.run_until_parked();
    view
}

/// The line and the column of the end of `needle` in `text`.
fn caret_after(text: &str, needle: &str) -> (usize, usize) {
    caret_at(text, needle, needle.len())
}

/// The line and the column of `extra` bytes after the start of `needle` in `text`.
fn caret_at(text: &str, needle: &str, extra: usize) -> (usize, usize) {
    let offset = text.find(needle).expect("the text holds the needle") + extra;
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count();
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let column = before[line_start..].encode_utf16().count();
    (line, column)
}

fn popup_shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>) -> bool {
    let mut visual = ui_kit::gpui::VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds("completion-popup").is_some()
}

fn marks(
    view: &gpui::Entity<EditorView>,
    cx: &TestAppContext,
) -> Vec<(Severity, std::ops::Range<usize>)> {
    view.read_with(cx, |view, _| view.diagnostic_marks())
}

fn marked_text(view: &gpui::Entity<EditorView>, cx: &TestAppContext) -> Vec<String> {
    let text = buffer_of(cx, view);
    marks(view, cx)
        .into_iter()
        .map(|(_, range)| text[range].to_owned())
        .collect()
}

const COMPLETE: &str = r#"package demo;

import org.springframework.data.jpa.repository.JpaRepository;

interface OwnerRepository extends JpaRepository<Owner, Integer> {
    List<Owner> findByLastNa(String name);
    List<Owner> findByLastName(String name);
}
"#;

#[ui_kit::gpui::test]
fn the_properties_and_the_operators_of_a_method_name_are_offered(cx: &mut TestAppContext) {
    let (root, path) = project("spring-data-method", COMPLETE);
    let support = window_language::spring_data(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(cx, window, &path, caret_after(COMPLETE, "findByLastNa"));

    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the properties of the entity",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window)
        },
    );
    press(cx, window, "enter");
    assert!(
        buffer_of(cx, &view).contains("findByLastName(String name)"),
        "the property of the entity is written: {}",
        buffer_of(cx, &view)
    );

    // The whole property is typed: the name continues with an operator.
    let text = COMPLETE.replace("findByLastNa", "findByLastName");
    let length = view.read_with(cx, |view, _| view.text().map_or(0, |text| text.len()));
    view.update(cx, |view, cx| view.replace_text(0..length, &text, cx));
    let (line, column) = caret_after(&text, "findByLastName");
    view.update(cx, |view, cx| view.go_to(Spot::Line { line, column }, cx));
    cx.run_until_parked();

    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the operators of the property",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window)
        },
    );
    press(cx, window, "enter");
    assert!(
        buffer_of(cx, &view).contains("findByLastNameAfter"),
        "an operator continues the name: {}",
        buffer_of(cx, &view)
    );
}

const UNKNOWN: &str = r#"package demo;

import org.springframework.data.jpa.repository.JpaRepository;

interface OwnerRepository extends JpaRepository<Owner, Integer> {
    List<Owner> findByNickname(String nickname);
    List<Owner> findByCity(String city, String other);
}
"#;

#[ui_kit::gpui::test]
fn an_unknown_property_and_a_wrong_number_of_parameters_are_marked(cx: &mut TestAppContext) {
    let (root, path) = project("spring-data-check", UNKNOWN);
    let support = window_language::spring_data(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(cx, window, &path, caret_after(UNKNOWN, "findByNickname"));

    wait_for(
        cx,
        "the checks of the name",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            !marks(&view, cx).is_empty()
        },
    );
    let found = marked_text(&view, cx);
    assert_eq!(found, ["Nickname", "findByCity"], "{found:?}");
    let severities: Vec<Severity> = marks(&view, cx).into_iter().map(|(kind, _)| kind).collect();
    assert_eq!(severities, [Severity::Error, Severity::Warning]);
}

const QUERY: &str = r#"package demo;

import org.springframework.data.jpa.repository.JpaRepository;
import org.springframework.data.jpa.repository.Query;
import org.springframework.data.repository.query.Param;

interface OwnerRepository extends JpaRepository<Owner, Integer> {
    @Query("select o from Owner o where o.nickname = :name")
    List<Owner> broken(String other);

    @Query("select o from Nope n")
    List<Owner> lost();

    @Query("select o from Own")
    List<Owner> all();

    @Query("select o from Owner o where o.ci")
    List<Owner> byCity(String city);

    @Query("select o from Owner o where o.firstName = :firstName")
    List<Owner> byName(String firstName);
}
"#;

#[ui_kit::gpui::test]
fn a_query_is_checked_for_its_entity_its_properties_and_its_parameters(cx: &mut TestAppContext) {
    let (root, path) = project("spring-data-query", QUERY);
    let support = window_language::spring_data(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(
        cx,
        window,
        &path,
        caret_at(QUERY, "from Own\")", "from Own".len()),
    );

    wait_for(
        cx,
        "the checks of the queries",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            marks(&view, cx).len() >= 3
        },
    );
    let found = marked_text(&view, cx);
    assert_eq!(&found[..3], ["nickname", "name", "Nope"], "{found:?}");
    let severities: Vec<Severity> = marks(&view, cx).into_iter().map(|(kind, _)| kind).collect();
    assert!(
        severities.iter().all(|kind| *kind == Severity::Error),
        "{severities:?}"
    );
}

#[ui_kit::gpui::test]
fn a_query_is_completed_with_its_entities_and_the_properties_of_an_alias(cx: &mut TestAppContext) {
    let (root, path) = project("spring-data-query-complete", QUERY);
    let support = window_language::spring_data(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(
        cx,
        window,
        &path,
        caret_at(QUERY, "from Own\")", "from Own".len()),
    );

    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the entities of the query",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window)
        },
    );
    press(cx, window, "enter");
    assert!(
        buffer_of(cx, &view).contains("from Owner\""),
        "the entity is written: {}",
        buffer_of(cx, &view)
    );

    let length = view.read_with(cx, |view, _| view.text().map_or(0, |text| text.len()));
    view.update(cx, |view, cx| view.replace_text(0..length, QUERY, cx));
    let (line, column) = caret_after(QUERY, "where o.ci");
    view.update(cx, |view, cx| view.go_to(Spot::Line { line, column }, cx));
    cx.run_until_parked();
    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the properties of the alias",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window)
        },
    );
    press(cx, window, "enter");
    assert!(
        buffer_of(cx, &view).contains("where o.city\""),
        "the property of the entity is written: {}",
        buffer_of(cx, &view)
    );
}

#[ui_kit::gpui::test]
fn ctrl_b_on_a_property_of_a_query_opens_the_field_of_the_entity(cx: &mut TestAppContext) {
    let (root, path) = project("spring-data-nav", QUERY);
    let owner = source_root(&root).join("demo").join(source_file("Owner"));
    let support = window_language::spring_data(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    open_file(cx, window, &path, caret_after(QUERY, "where o.firstName"));

    press(cx, window, "ctrl-b");
    wait_for(
        cx,
        "the field of the entity",
        Duration::from_secs(10),
        |cx| active_path(cx, window).as_deref() == Some(owner.as_path()),
    );
    let view = editor_of(cx, window, &owner);
    assert_eq!(
        view.read_with(cx, |view, _| view.caret()),
        (9, 19),
        "the caret stands on the name of the first name"
    );
}

#[ui_kit::gpui::test]
fn ctrl_b_on_an_entity_of_a_query_opens_its_class(cx: &mut TestAppContext) {
    let (root, path) = project("spring-data-nav-entity", QUERY);
    let owner = source_root(&root).join("demo").join(source_file("Owner"));
    let support = window_language::spring_data(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    open_file(cx, window, &path, caret_after(QUERY, "select o from Owner"));

    press(cx, window, "ctrl-b");
    wait_for(
        cx,
        "the class of the entity",
        Duration::from_secs(10),
        |cx| active_path(cx, window).as_deref() == Some(owner.as_path()),
    );
    let view = editor_of(cx, window, &owner);
    assert_eq!(view.read_with(cx, |view, _| view.caret()), (6, 6));
}

const BASE_REPOSITORY: &str = r#"package demo;

import org.springframework.data.jpa.repository.JpaRepository;

interface BaseRepo<T, ID> extends JpaRepository<T, ID> {}
"#;

const INHERITED: &str = r#"package demo;

interface OwnerRepository extends BaseRepo<Owner, Integer> {
    List<Owner> findByLastNa(String name);
}
"#;

#[ui_kit::gpui::test]
fn a_repository_that_inherits_a_project_interface_finds_the_properties_of_its_entity(
    cx: &mut TestAppContext,
) {
    let (root, path) = project("spring-data-inherited", INHERITED);
    fs::write(
        source_root(&root)
            .join("demo")
            .join(source_file("BaseRepo")),
        BASE_REPOSITORY,
    )
    .unwrap();
    let support = window_language::spring_data(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(cx, window, &path, caret_after(INHERITED, "findByLastNa"));

    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the properties of the entity of the base interface",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window)
        },
    );
    press(cx, window, "enter");
    assert!(
        buffer_of(cx, &view).contains("findByLastName(String name)"),
        "the property of the entity is written: {}",
        buffer_of(cx, &view)
    );
}

const BLOCK_QUERY: &str = concat!(
    "package demo;\n",
    "\n",
    "import org.springframework.data.jpa.repository.JpaRepository;\n",
    "import org.springframework.data.jpa.repository.Query;\n",
    "\n",
    "interface OwnerRepository extends JpaRepository<Owner, Integer> {\n",
    "    @Query(\"\"\"\n",
    "        select o from Owner o\n",
    "        where o.nickname = :name\n",
    "        \"\"\")\n",
    "    List<Owner> broken(String other);\n",
    "}\n",
);

#[ui_kit::gpui::test]
fn a_text_block_of_a_query_is_checked_on_the_lines_of_its_text(cx: &mut TestAppContext) {
    let (root, path) = project("spring-data-block", BLOCK_QUERY);
    let support = window_language::spring_data(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(
        cx,
        window,
        &path,
        caret_after(BLOCK_QUERY, "where o.nickname"),
    );

    wait_for(
        cx,
        "the checks of the text block",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            !marks(&view, cx).is_empty()
        },
    );
    let found = marked_text(&view, cx);
    assert_eq!(found, ["nickname", "name"], "{found:?}");
}

#[ui_kit::gpui::test]
fn the_keywords_of_a_query_are_coloured(cx: &mut TestAppContext) {
    let (root, path) = project("spring-data-colour", QUERY);
    let support = window_language::spring_data(&root);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_file(cx, window, &path, caret_after(QUERY, "where o.ci"));

    wait_for(
        cx,
        "the colours of the query",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            view.read_with(cx, |view, _| {
                let text = view.text().unwrap_or_default();
                text.find("select")
                    .is_some_and(|at| view.semantic_look_at(at).is_some())
            })
        },
    );
    view.read_with(cx, |view, _| {
        let text = view.text().unwrap_or_default();
        let keyword = text.find("where").expect("the keyword of the test");
        assert!(view.semantic_look_at(keyword).is_some());
        let name = text.find("o.ci").expect("the property of the test") + 2;
        assert!(
            view.semantic_look_at(name).is_none(),
            "a name keeps its own colour"
        );
    });
}

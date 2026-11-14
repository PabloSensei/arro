//! Spring Boot configuration files: the keys of the metadata in completion, the value
//! choices of a key, the documentation of a key and the checks of a property file. The
//! metadata comes from a jar the test builds, so no outside project is needed.
//!
//! The real PetClinic check needs `IDE_CORPUS`, the filled package cache of the
//! environment and the helpers of the test build:
//!
//! `cargo test -p panels window_tests_spring_config -- --ignored --test-threads=1`

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{EditorView, Languages, Spot};
use lang_api::{DiagnosticsQuery, DocumentSpot, LanguageSupport, Severity, TextPoint, ask};
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

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    support: Arc<dyn LanguageSupport>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("spring-state");
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
            project_setup(scratch("spring-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// A project with a jar of metadata and one configuration file.
fn project(name: &str, file: &str, text: &str) -> (PathBuf, PathBuf) {
    let root = scratch(name);
    let jar = window_language::write_spring_metadata_jar(&root);
    let path = root.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    (root, jar)
}

fn open_file(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    root: &Path,
    file: &str,
    text: &str,
    line: usize,
    column: usize,
) -> (PathBuf, gpui::Entity<EditorView>) {
    let path = root.join(file);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(&path, text).unwrap();
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    view.update(cx, |view, cx| view.go_to(Spot::Line { line, column }, cx));
    cx.run_until_parked();
    (path, view)
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

/// Replaces the whole text of an open view and puts the caret at the end of `line`.
fn reset(
    view: &gpui::Entity<EditorView>,
    cx: &mut TestAppContext,
    text: &str,
    line: usize,
    column: usize,
) {
    let length = view.read_with(cx, |view, _| view.text().map_or(0, |text| text.len()));
    view.update(cx, |view, cx| {
        view.replace_text(0..length, text, cx);
        view.go_to(Spot::Line { line, column }, cx);
    });
    cx.run_until_parked();
}

#[ui_kit::gpui::test]
fn keys_of_the_metadata_are_offered_and_inserted(cx: &mut TestAppContext) {
    let (root, jar) = project(
        "spring-keys",
        "src/main/resources/application.properties",
        "spring.jpa.",
    );
    let support = window_language::spring_config(&root, vec![jar]);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(
        cx,
        window,
        &root,
        "src/main/resources/application.properties",
        "spring.jpa.",
        0,
        11,
    );

    press(cx, window, "ctrl-space");
    wait_for(cx, "the offered keys", Duration::from_secs(10), |cx| {
        draw(cx, window);
        popup_shown(cx, window)
    });
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "spring.jpa.database-platform",
        "the chosen key is written from its first letter"
    );

    reset(&view, cx, "spring.jpa.o", 0, 12);
    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the offered keys of the shorter prefix",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window)
        },
    );
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "spring.jpa.open-in-view",
        "a deprecated key of the same prefix comes after the live one"
    );
}

#[ui_kit::gpui::test]
fn a_dotted_key_in_yaml_breaks_into_its_levels(cx: &mut TestAppContext) {
    let (root, jar) = project(
        "spring-yaml",
        "src/main/resources/application.yml",
        "spring.jpa.open",
    );
    let support = window_language::spring_config(&root, vec![jar]);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(
        cx,
        window,
        &root,
        "src/main/resources/application.yml",
        "spring.jpa.open",
        0,
        15,
    );

    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the offered key in YAML",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window)
        },
    );
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "spring:\n  jpa:\n    open-in-view: ",
        "the typed key becomes its levels"
    );

    reset(&view, cx, "spring:\n  jpa.op", 1, 8);
    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the offered key inside a block",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window)
        },
    );
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "spring:\n  jpa:\n    open-in-view: ",
        "a key in an existing block fills only the levels it misses"
    );
}

#[ui_kit::gpui::test]
fn boolean_and_enum_values_are_offered(cx: &mut TestAppContext) {
    let (root, jar) = project(
        "spring-values",
        "src/main/resources/application-values.properties",
        "spring.jpa.open-in-view=",
    );
    let support = window_language::spring_config(&root, vec![jar]);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(
        cx,
        window,
        &root,
        "src/main/resources/application-values.properties",
        "spring.jpa.open-in-view=",
        0,
        24,
    );

    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the values of the boolean",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window)
        },
    );
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "spring.jpa.open-in-view=false",
        "the value is written after the separator"
    );

    reset(&view, cx, "spring.jpa.hibernate.ddl-auto=", 0, 32);
    press(cx, window, "ctrl-space");
    wait_for(
        cx,
        "the values of the enum",
        Duration::from_secs(10),
        |cx| {
            draw(cx, window);
            popup_shown(cx, window)
        },
    );
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "spring.jpa.hibernate.ddl-auto=create",
        "an enum offers the values of its hints"
    );
}

#[ui_kit::gpui::test]
fn the_documentation_of_a_key_shows_its_type_and_source(cx: &mut TestAppContext) {
    let (root, jar) = project(
        "spring-doc",
        "src/main/resources/application-doc.properties",
        "spring.jpa.open-in-view=false\n",
    );
    let support = window_language::spring_config(&root, vec![jar]);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(
        cx,
        window,
        &root,
        "src/main/resources/application-doc.properties",
        "spring.jpa.open-in-view=false\n",
        0,
        5,
    );

    press(cx, window, "ctrl-q");
    draw(cx, window);
    wait_for(
        cx,
        "the documentation of the key",
        Duration::from_secs(10),
        |cx| view.read_with(cx, |view, _| view.quick_doc_text().is_some()),
    );
    let text = view
        .read_with(cx, |view, _| view.quick_doc_text())
        .unwrap_or_default();
    assert!(text.contains("Boolean"), "the type is shown: {text}");
    assert!(
        text.contains("interceptor"),
        "the description is shown: {text}"
    );
    assert!(
        text.contains("com.example.JpaProperties"),
        "the source is shown: {text}"
    );
}

#[ui_kit::gpui::test]
fn ctrl_b_on_a_key_opens_the_class_of_its_source_type(cx: &mut TestAppContext) {
    let (root, jar) = project(
        "spring-nav",
        "src/main/resources/application-nav.properties",
        "spring.jpa.open-in-view=false\n",
    );
    let source = window_language::write_spring_source(&root);
    let support = window_language::spring_config_with_sources(
        &root,
        vec![jar],
        vec![window_language::spring_source_root(&root)],
    );
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    open_file(
        cx,
        window,
        &root,
        "src/main/resources/application-nav.properties",
        "spring.jpa.open-in-view=false\n",
        0,
        5,
    );

    press(cx, window, "ctrl-b");
    wait_for(
        cx,
        "the class of the source type",
        Duration::from_secs(10),
        |cx| active_path(cx, window).as_deref() == Some(source.as_path()),
    );
}

#[ui_kit::gpui::test]
fn ctrl_b_on_a_key_without_a_source_shows_its_description(cx: &mut TestAppContext) {
    let text = "spring.jpa.show-sql=true\n";
    let (root, jar) = project(
        "spring-nav-fallback",
        "src/main/resources/application-fallback.properties",
        text,
    );
    let support = window_language::spring_config(&root, vec![jar]);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(
        cx,
        window,
        &root,
        "src/main/resources/application-fallback.properties",
        text,
        0,
        5,
    );

    press(cx, window, "ctrl-b");
    wait_for(
        cx,
        "the description of the key",
        Duration::from_secs(10),
        |cx| {
            view.read_with(cx, |view, _| {
                view.quick_doc_text()
                    .is_some_and(|text| text.contains("statements"))
            })
        },
    );
}

#[ui_kit::gpui::test]
fn an_unknown_key_warns_and_a_map_level_does_not(cx: &mut TestAppContext) {
    let text = "spring.jpa.unknown-key=true\n";
    let (root, jar) = project(
        "spring-checks",
        "src/main/resources/application-checks.properties",
        text,
    );
    let support = window_language::spring_config(&root, vec![jar]);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let (_, view) = open_file(
        cx,
        window,
        &root,
        "src/main/resources/application-checks.properties",
        text,
        0,
        0,
    );

    wait_for(
        cx,
        "the warning of the unknown key",
        Duration::from_secs(10),
        |cx| {
            marks(&view, cx)
                .iter()
                .any(|(severity, _)| *severity == Severity::Warning)
        },
    );
    let warnings: Vec<_> = marks(&view, cx)
        .into_iter()
        .filter(|(severity, _)| *severity == Severity::Warning)
        .collect();
    assert_eq!(warnings.len(), 1, "one key is unknown: {warnings:?}");
    assert_eq!(warnings[0].1.start, 0);

    let text = "logging.level.com.x=DEBUG\nspring.jpa.old-key=x\n";
    let (_, map_view) = open_file(
        cx,
        window,
        &root,
        "src/main/resources/application-map.properties",
        text,
        0,
        0,
    );
    wait_for(
        cx,
        "the checks of the map key",
        Duration::from_secs(10),
        |cx| marks(&map_view, cx).len() == 1,
    );
    let warnings = marks(&map_view, cx);
    let deprecated = text.find("spring.jpa.old-key").unwrap();
    assert_eq!(
        warnings[0].1.start, deprecated,
        "only the deprecated key warns, a map level does not: {warnings:?}"
    );

    reset(&view, cx, "spring.jpa.open-in-view=maybe\n", 0, 0);
    wait_for(
        cx,
        "the error of the bad boolean",
        Duration::from_secs(10),
        |cx| {
            marks(&view, cx)
                .iter()
                .any(|(severity, _)| *severity == Severity::Error)
        },
    );
}

#[tokio::test]
#[ignore = "needs IDE_CORPUS, the filled package cache and the helpers"]
async fn petclinic_keys_are_known_and_database_has_no_warning() {
    use futures_util::StreamExt;

    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("spring-petclinic").join("petclinic");
    window_language::prepare_project(&source, &root);
    let settings = window_language::offline_build_settings(&scratch("spring-petclinic-state"));
    let model =
        window_language::project_model(&root, &scratch("spring-petclinic-model"), &settings)
            .expect("the model of PetClinic is read");
    let support = window_language::spring_config_with_model(&root, &model);
    let path = root.join("src/main/resources/application.properties");
    let text = fs::read_to_string(&path).expect("PetClinic has its properties");
    let uri = platform::CanonPath::new(&path).unwrap().to_uri();
    support
        .server()
        .expect("the configuration support opens documents")
        .open(&uri, 1, Arc::from(text.as_str()));

    // The key of the file is known and shows its documentation.
    let offset = text.find("spring.jpa.open-in-view").unwrap();
    let line = text[..offset].bytes().filter(|byte| *byte == b'\n').count() as u32;
    let spot = DocumentSpot {
        uri: uri.clone(),
        version: 1,
        point: TextPoint { line, character: 0 },
    };
    let hover = support.hover().expect("the support documents keys");
    let mut replies = ask(hover, spot);
    let answer = replies.next().await.expect("the key has documentation");
    assert!(
        answer.value.0.contains("Boolean"),
        "the type of open-in-view: {}",
        answer.value.0
    );
    assert!(
        answer.value.0.contains("JpaProperties"),
        "the source of open-in-view: {}",
        answer.value.0
    );

    // `database=h2` is read through `${database}` and is no unknown key.
    let diagnostics = support.diagnostics().expect("the support checks files");
    let mut replies = ask(diagnostics, DiagnosticsQuery(uri.clone()));
    let answer = replies.next().await.expect("the checks of the file");
    let unknown: Vec<&str> = answer
        .value
        .iter()
        .filter(|entry| entry.severity == Severity::Warning)
        .map(|entry| entry.message.as_str())
        .filter(|message| message.contains("database") || message.contains("open-in-view"))
        .collect();
    assert!(unknown.is_empty(), "{unknown:?}");
}

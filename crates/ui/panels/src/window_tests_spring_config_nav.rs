//! Ctrl+B from a Spring configuration key to its field and from a `@Value` placeholder
//! to the key of a configuration file. The metadata comes from a jar the test builds and
//! the sources from files the test writes, so no outside project is needed.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{EditorView, Languages, Spot};
use lang_api::LanguageSupport;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{FileIcons, Launch, Layout, Settings, USAGES_TOOL, Workspace};

use crate::index::SearchSetup;
use crate::window_tests::{active_path, draw, editor_of, open_path, press, project_setup, scratch};
use crate::window_tests_real::wait_for;
use crate::window_tests_usages_tabs::{counts, hub};
use crate::{AgentOptions, LanguageServer, connect, install};

fn open_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    support: Arc<dyn LanguageSupport>,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("spring-nav-state");
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
            project_setup(scratch("spring-nav-cache")),
            agents,
            window,
            cx,
        );
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// Writes a file of the project and returns its path.
fn write(root: &Path, file: &str, text: &str) -> PathBuf {
    let path = root.join(file);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, text).unwrap();
    path
}

/// Opens a file and puts the caret on a place of it.
fn open_at(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    path: &Path,
    line: usize,
    column: usize,
) -> gpui::Entity<EditorView> {
    open_path(cx, window, path);
    let view = editor_of(cx, window, path);
    view.update(cx, |view, cx| view.go_to(Spot::Line { line, column }, cx));
    cx.run_until_parked();
    view
}

/// The column of the `needle` of a line, with the caret just inside it.
fn inside(text: &str, line: usize, needle: &str) -> usize {
    text.lines()
        .nth(line)
        .and_then(|line| line.find(needle))
        .expect("the place of the test")
        + 2
}

#[ui_kit::gpui::test]
fn ctrl_b_on_a_key_puts_the_caret_on_the_field(cx: &mut TestAppContext) {
    let root = scratch("spring-nav-field");
    let jar = window_language::write_spring_metadata_jar(&root);
    let sources = window_language::spring_source_root(&root);
    let source_text =
        "package com.example;\n\nclass JpaProperties {\n    private Boolean openInView;\n}\n";
    let source = window_language::write_test_source(&root, "JpaProperties", source_text);
    let support = window_language::spring_config_with_sources(&root, vec![jar], vec![sources]);
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let config = write(
        &root,
        "src/main/resources/application-nav.properties",
        "spring.jpa.open-in-view=false\n",
    );
    open_at(cx, window, &config, 0, 5);

    press(cx, window, "ctrl-b");
    wait_for(cx, "the field of the key", Duration::from_secs(10), |cx| {
        active_path(cx, window).as_deref() == Some(source.as_path())
            && editor_of(cx, window, &source).read_with(cx, |view, _| view.caret_line()) == Some(3)
    });
}

#[ui_kit::gpui::test]
fn ctrl_b_in_a_value_placeholder_opens_the_key_file(cx: &mut TestAppContext) {
    let root = scratch("spring-nav-value");
    let config = write(
        &root,
        "src/main/resources/application.properties",
        "server.port=8080\n",
    );
    let source_text =
        "package com.example;\n\nclass Shop {\n    @Value(\"${server.port}\")\n    int port;\n}\n";
    let column = inside(source_text, 3, "server.port");
    let source = window_language::write_test_source(&root, "Shop", source_text);
    let support = window_language::spring_source_with_config(
        &root,
        Vec::new(),
        vec![window_language::spring_source_root(&root)],
        vec![root.join("src/main/resources")],
    );
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    open_at(cx, window, &source, 3, column);

    press(cx, window, "ctrl-b");
    wait_for(
        cx,
        "the key of the placeholder",
        Duration::from_secs(10),
        |cx| {
            active_path(cx, window).as_deref() == Some(config.as_path())
                && editor_of(cx, window, &config).read_with(cx, |view, _| view.caret_line())
                    == Some(0)
        },
    );
}

#[ui_kit::gpui::test]
fn a_key_in_two_profile_files_gives_a_list_of_two(cx: &mut TestAppContext) {
    let root = scratch("spring-nav-profiles");
    let _ = write(
        &root,
        "src/main/resources/application.properties",
        "spring.application.name=shop\n",
    );
    let dev = write(
        &root,
        "src/main/resources/application-dev.properties",
        "spring.jpa.open-in-view=true\n",
    );
    let prod = write(
        &root,
        "src/main/resources/application-prod.properties",
        "spring.jpa.open-in-view=false\n",
    );
    let source_text = "package com.example;\n\nclass Shop {\n    @Value(\"${spring.jpa.open-in-view}\")\n    boolean open;\n}\n";
    let column = inside(source_text, 3, "spring.jpa.open-in-view");
    let source = window_language::write_test_source(&root, "Shop", source_text);
    let support = window_language::spring_source_with_config(
        &root,
        Vec::new(),
        vec![window_language::spring_source_root(&root)],
        vec![root.join("src/main/resources")],
    );
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    let view = open_at(cx, window, &source, 3, column);

    press(cx, window, "ctrl-b");
    wait_for(
        cx,
        "the list of the key files",
        Duration::from_secs(10),
        |cx| view.read_with(cx, |view, _| view.navigation_targets()) == Some(2),
    );
    draw(cx, window);
    {
        let mut visual = ui_kit::gpui::VisualTestContext::from_window(window.into(), cx);
        assert!(
            visual.debug_bounds("navigation-row-1").is_some(),
            "the second profile file is a row of the list"
        );
    }

    press(cx, window, "down enter");
    wait_for(
        cx,
        "the second profile file",
        Duration::from_secs(10),
        |cx| active_path(cx, window).as_deref() == Some(prod.as_path()),
    );
    assert!(
        dev.exists() && active_path(cx, window).as_deref() != Some(dev.as_path()),
        "the list picks the second file"
    );
}

#[ui_kit::gpui::test]
fn alt_f7_on_a_key_lists_its_value_placeholder(cx: &mut TestAppContext) {
    let root = scratch("spring-nav-usages");
    let property = write(
        &root,
        "src/main/resources/application.properties",
        "server.port=8080\n",
    );
    let source_text =
        "package com.example;\n\nclass Shop {\n    @Value(\"${server.port}\")\n    int port;\n}\n";
    let source = window_language::write_test_source(&root, "Shop", source_text);
    let support = window_language::spring_source_with_config(
        &root,
        Vec::new(),
        vec![window_language::spring_source_root(&root)],
        vec![root.join("src/main/resources")],
    );
    let window = open_window(cx, root.clone(), support);
    cx.run_until_parked();
    draw(cx, window);
    open_at(cx, window, &property, 0, 2);

    press(cx, window, "alt-f7");
    wait_for(
        cx,
        "the placeholder of the key",
        Duration::from_secs(10),
        |cx| {
            window
                .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
                .unwrap()
                && counts(cx) == 1
        },
    );
    let items = hub(cx).read_with(cx, |hub, cx| hub.panel().read(cx).items().to_vec());
    assert!(
        items.iter().any(|item| item.path == source),
        "the usage is in the source: {items:?}"
    );
}

//! The window with the real language server and debugger on a copy of PetClinic.
//!
//! The corpus and the package cache are the only outside world; the project is copied,
//! so nothing changes in the corpus. The module is not part of a normal test run:
//!
//! `cargo test -p panels window_tests_real -- --ignored --test-threads=1`
//!
//! The real language and debugger come from the `window-language` test support crate,
//! which is the only place of this crate that knows them.

use std::fs;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dap_client::DebugState;
use editor_view::{EditorView, Languages};
use lang_api::Severity;
use ui_kit::Theme;
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use window_language::RealLanguage;
use workspace::{FileIcons, Launch, Layout, PROBLEMS_TOOL, Settings, USAGES_TOOL, Workspace};

use crate::debug::ActiveDebug;
use crate::index::SearchSetup;
use crate::problems::ActiveProblems;
use crate::run::{ProjectSetup, RunPanel};
use crate::script::ActiveRunPanel;
use crate::usages::ActiveUsages;
use crate::window_tests::{
    active_path, buffer_of, click_gutter, draw, editor_of, open_path, press, project_setup,
    scratch, type_text,
};
use crate::{AgentOptions, LanguageServer, connect, install};

/// The language server imports PetClinic for a long time on a cold state folder.
const READY_LIMIT: Duration = Duration::from_secs(600);
/// One answer of the server or one debug stop.
const LIMIT: Duration = Duration::from_secs(120);
const TICK: Duration = Duration::from_millis(50);

pub(crate) fn wait_for(
    cx: &mut TestAppContext,
    what: &str,
    limit: Duration,
    mut check: impl FnMut(&mut TestAppContext) -> bool,
) {
    let started = Instant::now();
    loop {
        if check(cx) {
            return;
        }
        assert!(
            started.elapsed() < limit,
            "the test waited for {what} in vain"
        );
        std::thread::sleep(Duration::from_millis(20));
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
    }
}

fn status(cx: &TestAppContext, window: WindowHandle<Workspace>) -> Option<String> {
    window
        .read_with(cx, |workspace, _| {
            workspace.language_status().map(ToString::to_string)
        })
        .unwrap()
}

pub(crate) fn wait_ready(cx: &mut TestAppContext, window: WindowHandle<Workspace>) {
    wait_for(cx, "the server to be ready", READY_LIMIT, |cx| {
        status(cx, window).is_some_and(|text| window_language::status_is_ready(&text))
    });
}

fn shown(cx: &mut TestAppContext, window: WindowHandle<Workspace>, selector: &'static str) -> bool {
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    visual.debug_bounds(selector).is_some()
}

fn run_panel(cx: &TestAppContext) -> gpui::Entity<RunPanel> {
    cx.read(|cx| cx.global::<ActiveRunPanel>().0.clone())
}

fn debug_panel(cx: &TestAppContext) -> gpui::Entity<crate::debug::DebugPanel> {
    cx.read(|cx| cx.global::<ActiveDebug>().0.clone())
}

pub(crate) fn errors(cx: &TestAppContext, view: &gpui::Entity<EditorView>) -> usize {
    view.read_with(cx, |view, _| {
        view.diagnostic_marks()
            .iter()
            .filter(|(severity, _)| *severity == Severity::Error)
            .count()
    })
}

/// The corpus copy and one session of the real server.
pub(crate) fn project(name: &str) -> (PathBuf, RealLanguage) {
    // Public to the sibling import test, which reuses the copy for its control case.
    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch(name).join("petclinic");
    window_language::prepare_project(&source, &root);
    let language = RealLanguage::open(root.clone(), scratch(&format!("{name}-state")))
        .expect("the language server starts");
    (root, language)
}

pub(crate) fn build_window(
    cx: &mut TestAppContext,
    root: PathBuf,
    language: &RealLanguage,
    setup: ProjectSetup,
) -> WindowHandle<Workspace> {
    cx.executor().allow_parking();
    let state_dir = scratch("window-state");
    let store = store::Store::open(&state_dir.join("state.sqlite"))
        .ok()
        .map(Arc::new);
    cx.update(|cx| {
        cx.set_global(Theme::dark());
        cx.set_global(Settings::default());
        cx.set_global(Languages {
            resolve: window_language::language_for,
            templates: window_language::templates_for,
        });
        workspace::bind_keys(cx);
    });
    let launch = Launch {
        root: root.clone(),
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
    language.watch_status(move |text| {
        status_sender.unbounded_send(text).ok();
    });
    let server = LanguageServer {
        support: Some(language.support()),
        status,
    };
    let search = SearchSetup {
        excludes: window_language::search_excludes(&root),
        symbols_for: window_language::symbols_for,
        extra_sources: Vec::new(),
    };
    cx.add_window(move |window, cx| {
        let mut workspace =
            Workspace::new_for_test(launch, Layout::default(), store, Vec::new(), window, cx);
        install(&mut workspace, search, setup, agents, window, cx);
        connect(&mut workspace, server, cx);
        workspace
    })
}

/// Closes the window the way the application does: the language state goes with it.
pub(crate) fn close(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    language: RealLanguage,
    check: bool,
) {
    let marker = language.process_marker();
    assert!(
        !window_language::processes_with(&marker).is_empty(),
        "the server was not running before the close"
    );
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    // The application ends with its last window: the entities and the language state go,
    // then the session that started the server.
    cx.update(|cx| {
        cx.shutdown();
        cx.clear_globals();
    });
    drop(language);
    let started = Instant::now();
    while !window_language::processes_with(&marker).is_empty() {
        assert!(
            started.elapsed() < Duration::from_secs(60),
            "the server outlived the window: {:?}",
            window_language::processes_with(&marker)
        );
        std::thread::sleep(Duration::from_millis(200));
    }
    if check {
        assert!(
            window_language::processes_with(&marker).is_empty(),
            "the server outlived the window: {:?}",
            window_language::processes_with(&marker)
        );
    }
}

pub(crate) fn finish(name: &str, started: Instant) {
    println!("{name}: {:.1} s", started.elapsed().as_secs_f64());
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn errors_reach_the_text_and_problems_and_completion_works(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-errors");
    let check = window_language::write_error_sample(&root);
    let complete = window_language::write_completion_sample(&root);
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    // A broken return type appears in the text and in Problems, then goes away.
    open_path(cx, window, &check);
    let view = editor_of(cx, window, &check);
    wait_for(cx, "the sample to have no errors", LIMIT, |cx| {
        errors(cx, &view) == 0
    });
    let text = buffer_of(cx, &view);
    let from = text.find("return 1;").expect("the sample has a return");
    view.update(cx, |view, cx| {
        view.replace_text(from..from + "return 1;".len(), "return \"bad\";", cx)
    });
    wait_for(cx, "the error to reach the text", LIMIT, |cx| {
        errors(cx, &view) > 0
    });
    press(cx, window, "alt-6");
    draw(cx, window);
    assert!(
        window
            .read_with(cx, |workspace, _| workspace.tool_open(PROBLEMS_TOOL))
            .unwrap(),
        "Alt+6 opens Problems"
    );
    let problems = cx.update(|cx| cx.global::<ActiveProblems>().0.clone());
    wait_for(cx, "the error to reach Problems", LIMIT, |cx| {
        !problems.read_with(cx, |hub, cx| hub.panel().read(cx).items().is_empty())
    });

    let text = buffer_of(cx, &view);
    let from = text.find("\"bad\"").expect("the bad return is there");
    view.update(cx, |view, cx| view.replace_text(from..from + 5, "1", cx));
    wait_for(cx, "the error to go away", LIMIT, |cx| {
        errors(cx, &view) == 0
            && problems.read_with(cx, |hub, cx| hub.panel().read(cx).items().is_empty())
    });

    // Typing the start of a type shows the completion of the server and inserts it.
    open_path(cx, window, &complete);
    let view = editor_of(cx, window, &complete);
    view.update(cx, |view, cx| view.restore_caret(5, 8, cx));
    draw(cx, window);
    type_text(cx, window, "Syste");
    draw(cx, window);
    wait_for(cx, "the completion popup", LIMIT, |cx| {
        shown(cx, window, "completion-popup")
    });
    press(cx, window, "enter");
    let text = buffer_of(cx, &view);
    assert!(
        text.contains("System"),
        "the completion was not inserted: {text:?}"
    );

    close(cx, window, language, false);
    finish(
        "errors_reach_the_text_and_problems_and_completion_works",
        started,
    );
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn navigation_usages_and_refactorings_work(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-refactors");
    let (sample, other) = window_language::write_refactor_samples(&root);
    let extract = window_language::write_extract_sample(&root);
    let messy = window_language::write_format_sample(&root);
    let (movable, moved) = window_language::write_move_samples(&root);
    let window = build_window(cx, root.clone(), &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    // Ctrl+B from a call site to another class, and back.
    open_path(cx, window, &sample);
    open_path(cx, window, &other);
    let view = editor_of(cx, window, &other);
    view.update(cx, |view, cx| view.restore_caret(4, 13, cx));
    draw(cx, window);
    press(cx, window, "ctrl-b");
    wait_for(cx, "the definition", LIMIT, |cx| {
        active_path(cx, window) == Some(sample.clone())
    });
    press(cx, window, "ctrl-alt-left");
    wait_for(cx, "the way back", LIMIT, |cx| {
        active_path(cx, window) == Some(other.clone())
    });

    // Alt+F7 lists the uses of the method.
    let view = editor_of(cx, window, &other);
    view.update(cx, |view, cx| view.restore_caret(5, 22, cx));
    draw(cx, window);
    press(cx, window, "alt-f7");
    wait_for(cx, "the Usages window", LIMIT, |cx| {
        window
            .read_with(cx, |workspace, _| workspace.tool_open(USAGES_TOOL))
            .unwrap()
            && !cx
                .update(|cx| cx.global::<ActiveUsages>().0.clone())
                .read_with(cx, |hub, cx| hub.panel().read(cx).items().is_empty())
    });

    // Shift+F6 renames the method in the open and the other open file, in one undo step.
    open_path(cx, window, &sample);
    let sample_view = editor_of(cx, window, &sample);
    sample_view.update(cx, |view, cx| view.restore_caret(6, 8, cx));
    draw(cx, window);
    press(cx, window, "shift-f6");
    draw(cx, window);
    assert!(
        shown(cx, window, "rename-popup"),
        "Shift+F6 asks for the new name"
    );
    press(cx, window, "backspace backspace backspace");
    type_text(cx, window, "sum");
    press(cx, window, "enter");
    draw(cx, window);
    assert!(
        buffer_of(cx, &sample_view).contains("int add("),
        "the first Enter only previews"
    );
    // The preview comes from the server asynchronously; an Enter that arrives before it
    // asks again instead of applying, so keep asking until the text changes.
    let rename_started = Instant::now();
    while !buffer_of(cx, &sample_view).contains("int sum(") {
        assert!(rename_started.elapsed() < LIMIT, "the rename did not apply");
        press(cx, window, "enter");
        draw(cx, window);
        std::thread::sleep(Duration::from_millis(500));
        cx.executor().advance_clock(TICK);
        cx.run_until_parked();
    }
    wait_for(cx, "the other file to change", LIMIT, |cx| {
        buffer_of(cx, &editor_of(cx, window, &other)).contains(".sum(")
    });
    press(cx, window, "ctrl-z");
    assert!(
        buffer_of(cx, &sample_view).contains("int add("),
        "Ctrl+Z takes the rename back in one step"
    );
    open_path(cx, window, &other);
    press(cx, window, "ctrl-z");
    assert!(
        buffer_of(cx, &editor_of(cx, window, &other)).contains(".add("),
        "Ctrl+Z takes the other file back too"
    );

    // F6 moves the class to the package picked from the list.
    open_path(cx, window, &movable);
    let view = editor_of(cx, window, &movable);
    view.update(cx, |view, cx| view.restore_caret(2, 13, cx));
    draw(cx, window);
    press(cx, window, "f6");
    draw(cx, window);
    wait_for(cx, "the package list", LIMIT, |cx| {
        shown(cx, window, "move-popup")
    });
    type_text(cx, window, "dest");
    draw(cx, window);
    press(cx, window, "enter");
    wait_for(cx, "the moved class", LIMIT, |_| {
        !movable.exists() && moved.is_file()
    });
    let text = fs::read_to_string(&moved).unwrap();
    assert!(text.starts_with("package probe.dest;"), "{text}");

    // Ctrl+Alt+M extracts the statement under the selection, and Ctrl+Z takes it back.
    open_path(cx, window, &extract);
    let view = editor_of(cx, window, &extract);
    let before = buffer_of(cx, &view);
    view.update(cx, |view, cx| view.restore_caret(5, 0, cx));
    draw(cx, window);
    press(cx, window, "home shift-end");
    draw(cx, window);
    press(cx, window, "ctrl-alt-m");
    wait_for(cx, "the extracted method", LIMIT, |cx| {
        buffer_of(cx, &view) != before
    });
    press(cx, window, "ctrl-z");
    assert_eq!(
        buffer_of(cx, &view),
        before,
        "Ctrl+Z takes the extraction back in one step"
    );

    // Ctrl+Alt+L formats the file, Ctrl+Alt+O removes the unused import.
    open_path(cx, window, &messy);
    let view = editor_of(cx, window, &messy);
    let before = buffer_of(cx, &view);
    press(cx, window, "ctrl-alt-l");
    wait_for(cx, "the format", LIMIT, |cx| buffer_of(cx, &view) != before);
    assert!(
        buffer_of(cx, &view).lines().count() > before.lines().count(),
        "the formatter put the members on lines of their own"
    );

    open_path(cx, window, &sample);
    let view = editor_of(cx, window, &sample);
    assert!(
        window_language::has_unused_import(&buffer_of(cx, &view)),
        "the sample starts with the unused import"
    );
    press(cx, window, "ctrl-alt-o");
    wait_for(cx, "the organized imports", LIMIT, |cx| {
        window_language::unused_import_is_gone(&buffer_of(cx, &view))
    });

    close(cx, window, language, false);
    finish("navigation_usages_and_refactorings_work", started);
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn a_breakpoint_stops_and_the_session_runs_to_the_end(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-debug");
    let probe = language.install_probe().expect("the probe compiles");
    let setup = ProjectSetup {
        tool: Some(language.probe_build()),
        providers: language.probe_providers(&probe),
        tests: None,
        test_sites: window_language::test_sites,
        cache_root: scratch("probe-cache"),
        templates: Vec::new(),
        skipped_folders: window_language::skipped_folders(),
        environment: None,
        debug: Some(language.debugger()),
    };
    let window = build_window(cx, root.clone(), &language, setup);
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);

    let panel = run_panel(cx);
    wait_for(cx, "the run configuration", LIMIT, |cx| {
        panel
            .read_with(cx, |panel, _| panel.state().configs.clone())
            .iter()
            .any(|name| name == "DebugProbe")
    });
    assert!(
        panel.update(cx, |panel, cx| panel.select_named("DebugProbe", cx)),
        "the probe configuration is selected"
    );

    open_path(cx, window, &probe.path);
    let line = probe.line as usize - 1;
    click_gutter(cx, window, 0, line);
    let debug = debug_panel(cx);
    wait_for(cx, "the breakpoint", LIMIT, |cx| {
        debug.read_with(cx, |panel, _| panel.breakpoint_lines(&probe.path)) == vec![probe.line]
    });

    press(cx, window, "shift-f9");
    wait_for(cx, "the stop at the breakpoint", LIMIT, |cx| {
        matches!(
            debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { .. }
        )
    });
    wait_for(cx, "the stack and the variables", LIMIT, |cx| {
        let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
        snapshot
            .frames
            .iter()
            .any(|frame| frame.source.as_deref() == Some(probe.path.as_path()))
            && snapshot.variables.values().flatten().next().is_some()
    });
    let snapshot = debug.read_with(cx, |panel, _| panel.snapshot().clone());
    assert!(
        snapshot
            .frames
            .iter()
            .any(|frame| frame.source.as_deref() == Some(probe.path.as_path())),
        "the stack has a frame of the probe: {:?}",
        snapshot.frames
    );
    assert!(
        snapshot.variables.values().flatten().next().is_some(),
        "the panel has the variables of the frame"
    );
    assert!(
        snapshot
            .console
            .iter()
            .any(|line| line.text.contains("probe-stdout")),
        "the console shows the output of the program: {:?}",
        snapshot.console
    );
    let process_id = snapshot.process_id.expect("the debuggee process id");

    draw(cx, window);
    let frame = snapshot.frames[0].id;
    assert!(
        shown_selector(cx, window, &format!("frame-{frame}")),
        "the stack row is on screen"
    );

    press(cx, window, "f8");
    wait_for(cx, "the step to stop", LIMIT, |cx| {
        matches!(
            &debug.read_with(cx, |panel, _| panel.snapshot().state.clone()),
            DebugState::Stopped { reason } if reason == "step"
        )
    });

    // The probe loops over the same method: take the breakpoint away and run to the end.
    debug.update(cx, |panel, cx| {
        panel.toggle_breakpoint(probe.path.clone(), probe.line, cx)
    });
    press(cx, window, "f9");
    wait_for(cx, "the end of the program", LIMIT, |cx| {
        debug.read_with(cx, |panel, _| panel.snapshot().state.clone()) == DebugState::Terminated
    });
    wait_for(cx, "the debuggee to end", Duration::from_secs(60), |_| {
        !window_language::is_process_running(process_id)
            && window_language::processes_with(&probe.classes().display().to_string()).is_empty()
    });

    close(cx, window, language, true);
    finish(
        "a_breakpoint_stops_and_the_session_runs_to_the_end",
        started,
    );
}

fn shown_selector(
    cx: &mut TestAppContext,
    window: WindowHandle<Workspace>,
    selector: &str,
) -> bool {
    let selector: &'static str = Box::leak(selector.to_owned().into_boxed_str());
    shown(cx, window, selector)
}

#[ui_kit::gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn closing_the_window_ends_the_server(cx: &mut TestAppContext) {
    let started = Instant::now();
    let (root, language) = project("real-close");
    let window = build_window(cx, root, &language, project_setup(scratch("cache")));
    cx.run_until_parked();
    draw(cx, window);
    wait_ready(cx, window);
    let marker = language.process_marker();
    assert!(
        !window_language::processes_with(&marker).is_empty(),
        "the server runs while the window is open"
    );

    close(cx, window, language, true);
    finish("closing_the_window_ends_the_server", started);
}

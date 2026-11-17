//! T-0215: the console of a project without a window. Ctrl+Enter runs the selection or
//! the whole file, the answers and the errors reach the window, Stop and Reset reach the
//! helper, completion comes from the helper, and closing the window closes it.
//!
//! `cargo test -p panels window_tests_jshell`

#![cfg_attr(not(windows), allow(dead_code, unused_imports))]

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use repl::{
    Command, CompletionItem, CompletionSource, ConsoleSession, Diagnostic, Event, FragmentKind,
};
use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, VisualTestContext, WindowHandle};
use workspace::Workspace;

use crate::jshell_panel::{ActiveJShell, JShellPanel};
use crate::window_tests::{draw, open_path, open_window, press, sample_project, wait_for_signal};

const MODULE: &str = "app";

#[derive(Default)]
struct ScriptedState {
    commands: Mutex<Vec<Command>>,
    events: Mutex<VecDeque<Event>>,
    completions: Mutex<Vec<String>>,
    killed: AtomicBool,
    ids: AtomicU64,
}

struct Scripted {
    state: Arc<ScriptedState>,
}

impl Scripted {
    fn new() -> (Self, Arc<ScriptedState>) {
        let state = Arc::new(ScriptedState::default());
        (
            Self {
                state: state.clone(),
            },
            state,
        )
    }
}

impl ConsoleSession for Scripted {
    fn send(&self, command: Command) -> io::Result<()> {
        let id = match &command {
            Command::Eval { id, .. } | Command::Reset { id } => *id,
            _ => 0,
        };
        {
            let mut commands = self.state.commands.lock().unwrap();
            commands.push(command.clone());
        }
        if let Command::Eval { text, .. } = &command {
            let mut events = self.state.events.lock().unwrap();
            if text.contains("while") {
                // The fragment runs until Stop.
            } else if text.contains("int y = ;") {
                events.push_back(Event::Fragment {
                    id,
                    index: 1,
                    kind: FragmentKind::Error,
                    name: "1".into(),
                    value: None,
                    type_name: None,
                    status: "REJECTED".into(),
                    signature: None,
                    source: "int y = ;".into(),
                    diagnostics: vec![Diagnostic {
                        start: 8,
                        end: 8,
                        line: 1,
                        column: 9,
                        message: "illegal start of expression".into(),
                        code: "compiler.err".into(),
                    }],
                    exception: None,
                    truncated: false,
                });
                events.push_back(Event::Done {
                    id,
                    cancelled: false,
                });
            } else if text.contains("println") {
                events.push_back(Event::Output {
                    id,
                    text: "hello\n".into(),
                    error: false,
                });
                events.push_back(Event::Fragment {
                    id,
                    index: 1,
                    kind: FragmentKind::Statement,
                    name: "1".into(),
                    value: None,
                    type_name: None,
                    status: "VALID".into(),
                    signature: None,
                    source: text.trim().to_owned(),
                    diagnostics: Vec::new(),
                    exception: None,
                    truncated: false,
                });
                events.push_back(Event::Done {
                    id,
                    cancelled: false,
                });
            } else {
                events.push_back(Event::Fragment {
                    id,
                    index: 1,
                    kind: FragmentKind::Expression,
                    name: "1".into(),
                    value: Some("42".into()),
                    type_name: Some("int".into()),
                    status: "VALID".into(),
                    signature: None,
                    source: text.clone(),
                    diagnostics: Vec::new(),
                    exception: None,
                    truncated: false,
                });
                events.push_back(Event::Done {
                    id,
                    cancelled: false,
                });
            }
        }
        if let Command::Reset { id } = command {
            self.state.events.lock().unwrap().push_back(Event::Done {
                id,
                cancelled: false,
            });
        }
        Ok(())
    }

    fn poll(&self) -> Vec<Event> {
        self.state.events.lock().unwrap().drain(..).collect()
    }

    fn next_id(&self) -> u64 {
        self.state.ids.fetch_add(1, Ordering::Relaxed)
    }

    fn kill(&self) {
        self.state.killed.store(true, Ordering::Relaxed);
    }

    fn completion_source(&self) -> Arc<dyn CompletionSource> {
        Arc::new(ScriptedCompletion {
            state: self.state.clone(),
        })
    }
}

struct ScriptedCompletion {
    state: Arc<ScriptedState>,
}

impl CompletionSource for ScriptedCompletion {
    fn complete(&self, text: &str, _: usize, _: Duration) -> Result<Vec<CompletionItem>, String> {
        self.state.completions.lock().unwrap().push(text.to_owned());
        Ok(vec![CompletionItem {
            text: "substring(".into(),
            matches_type: true,
        }])
    }
}

fn panel(cx: &TestAppContext) -> gpui::Entity<JShellPanel> {
    cx.update(|cx| cx.global::<ActiveJShell>().0.clone())
}

/// Opens the console of the module with a scripted helper: no process starts.
fn console(
    cx: &mut TestAppContext,
    name: &str,
    text: &str,
) -> (
    WindowHandle<Workspace>,
    gpui::Entity<JShellPanel>,
    PathBuf,
    Arc<ScriptedState>,
) {
    let root = sample_project(name);
    let window = open_window(cx, root);
    let panel = panel(cx);
    let path = panel.read_with(cx, |panel, _| panel.console_path(MODULE));
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory).unwrap();
    }
    fs::write(&path, text).unwrap();
    let (session, state) = Scripted::new();
    panel.update(cx, |panel, cx| {
        panel.connect_for_test(MODULE, Box::new(session), cx);
    });
    open_path(cx, window, &path);
    (window, panel, path, state)
}

fn evals(state: &ScriptedState) -> Vec<String> {
    state
        .commands
        .lock()
        .unwrap()
        .iter()
        .filter_map(|command| match command {
            Command::Eval { text, .. } => Some(text.clone()),
            _ => None,
        })
        .collect()
}

#[ui_kit::gpui::test]
fn ctrl_enter_runs_the_whole_file_and_the_selection(cx: &mut TestAppContext) {
    let (window, _, _, state) = console(cx, "jshell-whole", "int x = 2;\nx * 21\n");
    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the whole file runs", |_| !evals(&state).is_empty());
    assert_eq!(evals(&state)[0], "int x = 2;\nx * 21\n");

    window
        .update(cx, |workspace, window, cx| {
            workspace.focus_editor(window, cx)
        })
        .unwrap();
    press(cx, window, "shift-down");
    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the selection runs", |_| evals(&state).len() > 1);
    assert_eq!(evals(&state)[1], "int x = 2;\n");
}

#[ui_kit::gpui::test]
fn a_result_line_reaches_the_window(cx: &mut TestAppContext) {
    let (window, panel, _, _) = console(cx, "jshell-answer", "x * 21\n");
    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the result line", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .lines_of(MODULE)
                .iter()
                .any(|line| line.contains("$1 ==> 42"))
        })
    });
}

#[ui_kit::gpui::test]
fn a_compile_error_reaches_the_window(cx: &mut TestAppContext) {
    let (window, panel, _, _) = console(cx, "jshell-error", "int y = ;\n");
    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the error line", |cx| {
        panel.read_with(cx, |panel, _| {
            panel
                .lines_of(MODULE)
                .iter()
                .any(|line| line.contains("1:9 illegal start of expression"))
        })
    });
    assert!(
        panel
            .read_with(cx, |panel, _| panel.lines_of(MODULE))
            .iter()
            .any(|line| line.trim() == "int y = ;"),
        "the rejected fragment is shown with its source"
    );
}

#[ui_kit::gpui::test]
fn stop_and_reset_reach_the_session(cx: &mut TestAppContext) {
    let (window, panel, _, state) = console(cx, "jshell-stop", "while(true){}\n");
    press(cx, window, "ctrl-enter");
    wait_for_signal(cx, "the fragment starts", |_| !evals(&state).is_empty());
    panel.update(cx, |panel, cx| panel.stop(cx));
    wait_for_signal(cx, "the stop reaches the helper", |_| {
        state
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| matches!(command, Command::Stop))
    });
    panel.update(cx, |panel, cx| panel.reset(cx));
    wait_for_signal(cx, "the reset reaches the helper", |_| {
        state
            .commands
            .lock()
            .unwrap()
            .iter()
            .any(|command| matches!(command, Command::Reset { .. }))
    });
}

#[ui_kit::gpui::test]
fn closing_the_window_closes_the_session(cx: &mut TestAppContext) {
    let (window, _, _, state) = console(cx, "jshell-close", "x * 21\n");
    draw(cx, window);
    let mut visual = VisualTestContext::from_window(window.into(), cx);
    assert!(
        visual.simulate_close(),
        "the cross asks to close the window"
    );
    drop(visual);
    window
        .update(cx, |_, window, _| window.remove_window())
        .unwrap();
    cx.run_until_parked();
    wait_for_signal(cx, "the closed helper", |_| {
        state.killed.load(Ordering::Relaxed)
    });
}

#[ui_kit::gpui::test]
fn completion_of_a_console_file_comes_from_the_session(cx: &mut TestAppContext) {
    let (window, _, _, state) = console(cx, "jshell-complete", "\"abc\".subs\n");
    press(cx, window, "ctrl-end");
    press(cx, window, "ctrl-space");
    wait_for_signal(cx, "the console is asked", |_| {
        !state.completions.lock().unwrap().is_empty()
    });
    let asked = state.completions.lock().unwrap().clone();
    assert!(
        asked[0].contains("\"abc\".subs"),
        "the helper sees the text of the file: {asked:?}"
    );
    let mut shown = false;
    for _ in 0..100 {
        cx.run_until_parked();
        let mut visual = VisualTestContext::from_window(window.into(), cx);
        shown = visual.debug_bounds("completion-popup").is_some();
        drop(visual);
        if shown {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
        cx.executor().advance_clock(Duration::from_millis(50));
    }
    assert!(shown, "the list of the helper is shown");
}

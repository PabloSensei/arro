//! The terminal tool window: the shell of the user in the project folder.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use project_model::EnvironmentProvider;
use terminal::{ShiftEnter, SpawnSpec, TERM_VARS, TerminalSession};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{Settings, ToggleTerminal};

use crate::term_grid::{every_frame, grid, scroll_lines, send_key};

const GROUP: &str = "bottom-panel";

/// The panel of the window, for scripts.
pub struct ActiveTerminal(pub Entity<TerminalPanel>);

impl Global for ActiveTerminal {}

enum State {
    Starting,
    Running(Arc<TerminalSession>),
    Failed(SharedString),
}

pub struct TerminalPanel {
    root: PathBuf,
    environment: Option<Arc<dyn EnvironmentProvider>>,
    state: State,
    focus: FocusHandle,
    version: u64,
}

/// The shell sees what the user sees in any console, plus the terminal variables and the
/// ones the project decides, so that its SDK is the SDK of the build.
fn shell_spec(root: PathBuf, variables: Vec<(String, String)>, settings: &Settings) -> SpawnSpec {
    let shell = match settings.terminal_shell.trim() {
        "" => terminal::default_shell()
            .unwrap_or_else(|| std::env::var("ComSpec").unwrap_or_else(|_| "cmd.exe".to_string())),
        configured => configured.to_owned(),
    };
    let cwd = start_directory(settings, &root);
    let mut env: Vec<(String, String)> = std::env::vars().collect();
    for (key, value) in TERM_VARS {
        env.retain(|(known, _)| !known.eq_ignore_ascii_case(key));
        env.push((key.to_string(), value.to_string()));
    }
    for (key, value) in variables {
        env.retain(|(known, _)| !known.eq_ignore_ascii_case(&key));
        env.push((key, value));
    }
    for (key, value) in terminal_environment(&settings.terminal_environment) {
        env.retain(|(known, _)| !known.eq_ignore_ascii_case(&key));
        env.push((key, value));
    }
    SpawnSpec {
        command_line: shell,
        cwd: Some(cwd),
        env,
        cols: 120,
        rows: 12,
        shift_enter: ShiftEnter::default(),
    }
}

/// Where a new terminal starts: the setting, the root of the project when it is empty
/// or gone. A relative path counts from the root.
fn start_directory(settings: &Settings, root: &Path) -> PathBuf {
    let chosen = settings.terminal_start_directory.trim();
    if chosen.is_empty() {
        return root.to_path_buf();
    }
    let path = PathBuf::from(chosen);
    let path = if path.is_absolute() {
        path
    } else {
        root.join(path)
    };
    if path.is_dir() {
        path
    } else {
        root.to_path_buf()
    }
}

/// The `NAME=value` lines of the setting, in their order. A line without `=` is skipped.
pub(crate) fn terminal_environment(text: &str) -> Vec<(String, String)> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .filter_map(|line| {
            let (name, value) = line.split_once('=')?;
            let name = name.trim();
            (!name.is_empty()).then(|| (name.to_owned(), value.trim().to_owned()))
        })
        .collect()
}

impl TerminalPanel {
    pub fn create(
        root: PathBuf,
        environment: Option<Arc<dyn EnvironmentProvider>>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let mut panel = Self {
                root,
                environment,
                state: State::Starting,
                focus: cx.focus_handle(),
                version: 0,
            };
            panel.start(cx);
            every_frame(cx, Self::refresh);
            panel
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn session(&self) -> Option<&Arc<TerminalSession>> {
        match &self.state {
            State::Running(session) => Some(session),
            _ => None,
        }
    }

    /// What the panel shows now, for tests: the shell, its start or the reason it failed.
    #[cfg(test)]
    pub fn report(&self) -> String {
        match &self.state {
            State::Starting => "starting".to_owned(),
            State::Running(_) => "running".to_owned(),
            State::Failed(text) => text.to_string(),
        }
    }

    /// Types a command line into the shell and runs it, the way the user would.
    pub fn run_command(&self, line: &str) -> Result<(), String> {
        let session = self.session().ok_or("The shell is not running")?;
        session
            .send(format!("{line}\r"))
            .map_err(|error| format!("The shell did not take the command: {error:?}"))
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        self.state = State::Starting;
        let root = self.root.clone();
        let environment = self.environment.clone();
        let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let spawn = cx.background_executor().spawn(async move {
            let variables = environment
                .map(|environment| environment.describe().variables)
                .unwrap_or_default();
            TerminalSession::spawn(&shell_spec(root, variables, &settings))
        });
        cx.spawn(async move |this, cx| {
            let result = spawn.await;
            this.update(cx, |this, cx| {
                this.state = match result {
                    Ok(session) => {
                        ::platform::label_process(session.pid(), "Terminal");
                        State::Running(Arc::new(session))
                    }
                    Err(error) => State::Failed(format!("The shell did not start: {error}").into()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(session) = self.session() {
            let version = session.screen().version;
            if version != self.version {
                self.version = version;
                cx.notify();
            }
        }
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        let Some(session) = self.session().cloned() else {
            return;
        };
        // A finished shell starts again on Enter.
        if session.is_closed() {
            if event.keystroke.key == "enter" {
                cx.stop_propagation();
                self.start(cx);
            }
            return;
        }
        if send_key(&session, &event.keystroke, cx) {
            cx.stop_propagation();
        }
    }
}

impl Render for TerminalPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let hide = |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            window.dispatch_action(Box::new(ToggleTerminal), cx);
        };
        let focused = self.focus.is_focused(window);
        let body = div()
            .id("terminal")
            .key_context("Terminal ToolWindow")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            );
        let body = match &self.state {
            State::Running(session) => {
                let scrolled = session.clone();
                let mut body = body
                    .on_scroll(move |event, _, _| scrolled.scroll(scroll_lines(event)))
                    .child(grid(session.clone(), focused, theme));
                if let Some(code) = session.exit_code().filter(|_| session.is_closed()) {
                    body = body.child(
                        div()
                            .flex_none()
                            .px(px(SPACE_M))
                            .pb(px(SPACE_M))
                            .text_size(px(UI_SIZE))
                            .text_color(theme.text_secondary)
                            .child(format!(
                                "Process finished with exit code {code}. Press Enter to start again"
                            )),
                    );
                }
                body
            }
            State::Starting => body.child(message("Starting the shell", &theme)),
            State::Failed(text) => body.child(message(text.clone(), &theme)),
        };
        island(&theme)
            .group(GROUP)
            .h_full()
            .child(panel_header("Terminal", GROUP, hide, &theme))
            .child(body)
    }
}

pub(crate) fn message(text: impl Into<SharedString>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_1()
        .items_center()
        .justify_center()
        .text_size(px(UI_SIZE))
        .text_color(theme.text_secondary)
        .child(text.into())
}

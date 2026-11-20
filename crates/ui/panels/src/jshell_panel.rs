//! The console of a project: a scratch file per module, the tool window of its answers
//! and the helper process that runs what the file says.
//!
//! The window knows no language and no helper: [`repl::Support`] from the application
//! says which modules exist and how to start the helper of one. The protocol and the
//! process live in `services/repl`.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ::base::Cancel;
use ::platform::CanonPath;
use editor_view::{EditorView, FileBar, FileLanguages, RunFiles};
use lang_api::{
    Answer, CompletionEntry, CompletionList, CompletionProvider, Confidence, DocumentEdit,
    DocumentSpot, LanguageFn, LanguageId, LanguageSupport, Provider, Queries, SemanticServer,
    SourceId, SyntaxProvider, offset_in,
};
use repl::{COMPLETION_TIMEOUT, Command, ConsoleSession, Event, FragmentKind, Session, Support};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{DockSide, JSHELL_TOOL, Workspace};

/// The tool window the answers go to.
pub const GROUP: &str = "bottom-panel";
/// The folder of the console files inside the state folder of the project.
const FOLDER: &str = "jshell";
/// The file of one module's console.
const CONSOLE_FILE: &str = "jshell-console.jsh";
/// The command of the menu, of Find Action and of the Tools row.
pub const OPEN: &str = "jshell.open";
const POLL: Duration = Duration::from_millis(50);

/// The panel of the window, for scripts and for tests.
pub struct ActiveJShell(pub Entity<JShellPanel>);

impl Global for ActiveJShell {}

/// The state folder of the project, for the editor that asks whether a file is a console
/// and may run it.
#[derive(Clone)]
pub struct JShellRoot(pub PathBuf);

impl Global for JShellRoot {}

/// The support the application gave before the window was built.
static SUPPORT: Mutex<Option<Arc<dyn Support>>> = Mutex::new(None);

/// Puts the language side of the window in place. The application calls it before it
/// opens the project, so no file of the project or of the language is read here.
pub fn set_jshell_support(support: Arc<dyn Support>) {
    if let Ok(mut slot) = SUPPORT.lock() {
        *slot = Some(support);
    }
}

fn support() -> Option<Arc<dyn Support>> {
    SUPPORT.lock().ok().and_then(|slot| slot.clone())
}

/// One line of the window.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Line {
    text: String,
    kind: LineKind,
}

impl Line {
    fn normal(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: LineKind::Normal,
        }
    }

    fn muted(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: LineKind::Muted,
        }
    }

    fn error(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            kind: LineKind::Error,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LineKind {
    Normal,
    Muted,
    Error,
    Output,
}

/// The helper of one module's console and its unfinished output.
struct OpenConsole {
    session: Box<dyn ConsoleSession>,
    uri: Option<String>,
    /// The request that runs now, or the one Reset is answering.
    busy: Option<u64>,
    /// Set by Stop until the answer of the helper comes.
    stopping: Option<Instant>,
    /// The output of the running fragment that has no line break yet.
    output: String,
}

pub struct JShellPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    state_dir: PathBuf,
    modules: Vec<String>,
    selected: Option<String>,
    consoles: HashMap<String, OpenConsole>,
    lines: HashMap<String, Vec<Line>>,
    status: Option<String>,
    scroll: ScrollHandle,
    /// Set while the task that reads the answers of the helpers runs: it runs only while
    /// at least one console is open.
    polling: bool,
    /// Set when the window goes away, so the helpers do not outlive it.
    window_closed: Arc<AtomicBool>,
    /// How many times the task read the answers, for the window tests.
    #[cfg(test)]
    poll_ticks: usize,
}

impl JShellPanel {
    pub fn create(
        workspace: WeakEntity<Workspace>,
        state_dir: PathBuf,
        cx: &mut App,
    ) -> Entity<Self> {
        let panel = cx.new(|cx| Self {
            workspace,
            focus: cx.focus_handle(),
            state_dir,
            modules: support()
                .map(|support| support.modules())
                .unwrap_or_default(),
            selected: None,
            consoles: HashMap::new(),
            lines: HashMap::new(),
            status: None,
            scroll: ScrollHandle::new(),
            polling: false,
            window_closed: Arc::new(AtomicBool::new(false)),
            #[cfg(test)]
            poll_ticks: 0,
        });
        panel.update(cx, |panel, cx| {
            let workspace = panel.workspace.clone();
            let closed = panel.window_closed.clone();
            let this = cx.weak_entity();
            // The panel is built while the window is being updated; the hooks are added
            // after that update is over.
            cx.defer(move |cx| {
                if let Some(workspace) = workspace.upgrade() {
                    let tabs = this.clone();
                    workspace.update(cx, |workspace, _cx| {
                        workspace.on_close(move || closed.store(true, Ordering::Relaxed));
                        workspace.on_tab_closed(move |path, cx| {
                            tabs.update(cx, |panel, cx| panel.tab_closed(path, cx)).ok();
                        });
                    });
                }
            });
        });
        panel
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The modules the selector offers.
    pub fn modules(&self) -> Vec<String> {
        self.modules.clone()
    }

    /// The module whose answers the window shows now.
    pub fn selected(&self) -> Option<String> {
        self.selected.clone()
    }

    /// The lines of one module's console, as the window draws them.
    pub fn lines_of(&self, module: &str) -> Vec<String> {
        self.lines
            .get(module)
            .map(|lines| lines.iter().map(|line| line.text.clone()).collect())
            .unwrap_or_default()
    }

    /// The file of a module's console, whether or not it exists yet.
    pub fn console_path(&self, module: &str) -> PathBuf {
        console_path(&self.state_dir, module)
    }

    /// The module whose console is `path`, when it is one.
    pub fn module_of(&self, path: &Path) -> Option<String> {
        console_module(&self.state_dir, path)
    }

    /// Starts the task that reads the answers of the helpers, when a console needs it.
    fn ensure_poll(&mut self, cx: &mut Context<Self>) {
        if self.polling {
            return;
        }
        self.polling = true;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(POLL).await;
                let more = this.update(cx, |this, cx| {
                    #[cfg(test)]
                    {
                        this.poll_ticks += 1;
                    }
                    this.poll(cx);
                    // Nothing to read: the task ends and the next console starts it again.
                    let more = !this.consoles.is_empty();
                    if !more {
                        this.polling = false;
                    }
                    more
                });
                if !matches!(more, Ok(true)) {
                    break;
                }
            }
        })
        .detach();
    }

    /// Runs the answers of every helper. The task that calls it runs only while a console
    /// is open.
    fn poll(&mut self, cx: &mut Context<Self>) {
        if self.window_closed.load(Ordering::Relaxed) && !self.consoles.is_empty() {
            let modules: Vec<String> = self.consoles.keys().cloned().collect();
            for module in modules {
                self.remove(&module);
            }
            self.selected = None;
            cx.notify();
            return;
        }
        let modules: Vec<String> = self.consoles.keys().cloned().collect();
        let mut changed = false;
        for module in modules {
            let Some(console) = self.consoles.get_mut(&module) else {
                continue;
            };
            let events = console.session.poll();
            if events.is_empty() {
                continue;
            }
            for event in events {
                self.handle(&module, event);
                changed = true;
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// The tab of a console file closed: its helper ends. The workspace runs this as the
    /// tab goes, so there is no list of the open tabs to compare on every answer.
    fn tab_closed(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(module) = self.module_of(path) else {
            return;
        };
        if !self.consoles.contains_key(&module) {
            return;
        }
        self.push_line(&module, Line::muted("The console was closed with its tab"));
        self.remove(&module);
        cx.notify();
    }

    fn handle(&mut self, module: &str, event: Event) {
        match event {
            Event::Ready {
                runtime, engine, ..
            } => {
                self.status = Some(format!("{runtime} ({engine})"));
                self.push_line(
                    module,
                    Line::muted(format!("Console ready: {runtime} ({engine})")),
                );
            }
            Event::Fragment {
                id,
                index,
                kind,
                name,
                value,
                status,
                signature,
                source,
                diagnostics,
                exception,
                truncated,
                ..
            } => {
                let _ = id;
                let mut lines = Vec::new();
                match kind {
                    FragmentKind::Expression => {
                        let value = value.unwrap_or_else(|| "null".to_owned());
                        let value = if truncated {
                            format!("{value}...")
                        } else {
                            value
                        };
                        lines.push(Line::normal(format!("${index} ==> {value}")));
                    }
                    FragmentKind::Variable => {
                        let name = if name.is_empty() {
                            format!("${index}")
                        } else {
                            name
                        };
                        match value {
                            Some(value) if !value.is_empty() => {
                                lines.push(Line::normal(format!("{name} ==> {value}")));
                            }
                            _ => lines.push(Line::muted(format!("{name} declared"))),
                        }
                    }
                    FragmentKind::Method => {
                        let shown = signature.unwrap_or(name);
                        lines.push(Line::normal(format!("created method {shown}")));
                    }
                    FragmentKind::Class => {
                        lines.push(Line::normal(format!("created class {name}")));
                    }
                    FragmentKind::Import => {
                        let shown = signature.unwrap_or_else(|| source.clone());
                        lines.push(Line::muted(shown.trim().to_owned()));
                    }
                    FragmentKind::Error => {
                        lines.push(Line::error(source.trim().to_owned()));
                    }
                    FragmentKind::Statement => {
                        if status == "VALID" {
                            lines.push(Line::muted(format!("--> {}", source.trim())));
                        } else {
                            lines.push(Line::error(source.trim().to_owned()));
                        }
                    }
                    FragmentKind::Other => {
                        lines.push(Line::muted(source.trim().to_owned()));
                    }
                }
                if status != "VALID"
                    && !matches!(kind, FragmentKind::Error)
                    && !matches!(kind, FragmentKind::Statement)
                {
                    lines.push(Line::error(format!("{status}: {}", source.trim())));
                }
                for diagnostic in diagnostics {
                    lines.push(Line::error(format!(
                        "  {}:{} {}",
                        diagnostic.line, diagnostic.column, diagnostic.message
                    )));
                }
                if let Some(exception) = exception {
                    lines.push(Line::error(format!("Exception: {}", exception.message)));
                    for frame in exception.stack.lines() {
                        lines.push(Line::error(frame.to_owned()));
                    }
                }
                for line in lines {
                    self.push_line(module, line);
                }
            }
            Event::Output { text, error, .. } => {
                if let Some(console) = self.consoles.get_mut(module) {
                    console.output.push_str(&text);
                }
                self.drain_output(module, false);
                if error
                    && let Some(lines) = self.lines.get_mut(module)
                    && let Some(last) = lines.last_mut()
                {
                    last.kind = LineKind::Error;
                }
            }
            Event::Done { cancelled, .. } => {
                self.drain_output(module, true);
                if let Some(console) = self.consoles.get_mut(module) {
                    console.busy = None;
                    console.stopping = None;
                }
                if cancelled {
                    self.push_line(module, Line::muted("Stopped"));
                }
            }
            Event::Completion { .. } => {}
            Event::Error { message, .. } => {
                if let Some(console) = self.consoles.get_mut(module) {
                    console.busy = None;
                    console.stopping = None;
                }
                self.status = Some(message.clone());
                self.push_line(module, Line::error(message));
            }
            Event::Exited { code } => {
                self.drain_output(module, true);
                let text = match code {
                    Some(code) => format!("The console ended with code {code}"),
                    None => "The console ended".to_owned(),
                };
                self.status = Some(text.clone());
                self.push_line(module, Line::error(text));
                self.remove(module);
            }
        }
    }

    /// Moves the complete lines of the unfinished output into the window.
    fn drain_output(&mut self, module: &str, all: bool) {
        let Some(console) = self.consoles.get_mut(module) else {
            return;
        };
        let mut ready = Vec::new();
        while let Some(at) = console.output.find('\n') {
            let mut line: String = console.output.drain(..=at).collect();
            line.pop();
            if line.ends_with('\r') {
                line.pop();
            }
            ready.push(line);
        }
        if all && !console.output.is_empty() {
            let mut line = std::mem::take(&mut console.output);
            if line.ends_with('\r') {
                line.pop();
            }
            ready.push(line);
        }
        for line in ready {
            self.push_line(
                module,
                Line {
                    text: line,
                    kind: LineKind::Output,
                },
            );
        }
    }

    fn push_line(&mut self, module: &str, line: Line) {
        self.lines.entry(module.to_owned()).or_default().push(line);
    }

    fn remove(&mut self, module: &str) {
        if let Some(console) = self.consoles.remove(module) {
            if let Some(uri) = &console.uri {
                repl::hub().unregister(uri);
            }
            console.session.kill();
        }
    }

    /// Opens the console of a module in a tab and shows the window below.
    pub(crate) fn open_console(
        &mut self,
        module: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.selected = Some(module.clone());
        let file = console_path(&self.state_dir, &module);
        let directory = file.parent().map(Path::to_path_buf);
        let job = cx.background_executor().spawn(async move {
            if let Some(directory) = directory {
                fs::create_dir_all(&directory).ok()?;
            }
            if !file.exists() {
                fs::write(&file, "").ok()?;
            }
            Some(file)
        });
        let Some(handle) = window.window_handle().downcast::<Workspace>() else {
            return;
        };
        cx.spawn(async move |_, cx| {
            let Some(path) = job.await else {
                return;
            };
            handle
                .update(cx, |workspace, window, cx| {
                    workspace.open_file(path, window, cx);
                    workspace.show_tool(JSHELL_TOOL, window, cx);
                })
                .ok();
        })
        .detach();
        cx.notify();
    }

    fn open_default(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.modules = support()
            .map(|support| support.modules())
            .unwrap_or_default();
        let module = self
            .selected
            .clone()
            .or_else(|| self.modules.first().cloned())
            .unwrap_or_else(default_module);
        self.open_console(module, window, cx);
    }

    /// A tab of a console file was opened: its helper starts before the first keystroke,
    /// so completion has a console to ask.
    pub(crate) fn opened(&mut self, path: &Path, cx: &mut Context<Self>) {
        let Some(module) = console_module(&self.state_dir, path) else {
            return;
        };
        self.modules = support()
            .map(|support| support.modules())
            .unwrap_or_default();
        self.selected = Some(module.clone());
        self.ensure(&module, cx);
        cx.notify();
    }

    fn ensure(&mut self, module: &str, cx: &mut Context<Self>) -> bool {
        if self.consoles.contains_key(module) {
            return true;
        }
        let path = console_path(&self.state_dir, module);
        let Some(support) = support() else {
            self.push_line(module, Line::error("The project has no console support"));
            cx.notify();
            return false;
        };
        match support.launch(module) {
            Ok(launch) => match Session::start(launch) {
                Ok(session) => {
                    self.register(module, path, Box::new(session), cx);
                    true
                }
                Err(error) => {
                    let text = format!("The console did not start: {error}");
                    self.status = Some(text.clone());
                    self.push_line(module, Line::error(text));
                    cx.notify();
                    false
                }
            },
            Err(error) => {
                self.status = Some(error.clone());
                self.push_line(module, Line::error(error));
                cx.notify();
                false
            }
        }
    }

    fn register(
        &mut self,
        module: &str,
        path: PathBuf,
        session: Box<dyn ConsoleSession>,
        cx: &mut Context<Self>,
    ) {
        let uri = CanonPath::new(&path).ok().map(|path| path.to_uri());
        if let Some(uri) = &uri {
            repl::hub().register(uri.clone(), session.completion_source());
        }
        self.consoles.insert(
            module.to_owned(),
            OpenConsole {
                session,
                uri,
                busy: None,
                stopping: None,
                output: String::new(),
            },
        );
        self.ensure_poll(cx);
        cx.notify();
    }

    /// How many times the task that reads the answers has run, for the window tests.
    #[cfg(test)]
    pub(crate) fn poll_ticks_for_test(&self) -> usize {
        self.poll_ticks
    }

    /// The session of a module, for a window test that answers without a process.
    #[cfg(test)]
    pub(crate) fn connect_for_test(
        &mut self,
        module: &str,
        session: Box<dyn ConsoleSession>,
        cx: &mut Context<Self>,
    ) {
        let path = console_path(&self.state_dir, module);
        self.register(module, path, session, cx);
    }

    /// Runs a text in the console of `module`, or the module of the current tab.
    pub(crate) fn run(&mut self, module: &str, text: String, cx: &mut Context<Self>) {
        if !self.ensure(module, cx) {
            return;
        }
        let Some(console) = self.consoles.get_mut(module) else {
            return;
        };
        let id = console.session.next_id();
        if let Err(error) = console.session.send(Command::Eval { id, text }) {
            let text = format!("The console did not take the text: {error}");
            self.status = Some(text.clone());
            self.push_line(module, Line::error(text));
            self.remove(module);
            cx.notify();
            return;
        }
        console.busy = Some(id);
        console.stopping = None;
        self.selected = Some(module.to_owned());
        cx.notify();
    }

    pub(crate) fn stop(&mut self, cx: &mut Context<Self>) {
        let Some(module) = self.selected.clone() else {
            return;
        };
        let mut kill = false;
        let mut send = false;
        if let Some(console) = self.consoles.get_mut(&module)
            && console.busy.is_some()
        {
            if console.stopping.is_some() {
                kill = true;
            } else {
                console.stopping = Some(Instant::now());
                send = true;
            }
        }
        if kill {
            self.remove(&module);
            self.push_line(
                &module,
                Line::error("The console did not answer and was stopped"),
            );
        } else if send {
            let Some(console) = self.consoles.get(&module) else {
                return;
            };
            let _ = console.session.send(Command::Stop);
            self.push_line(&module, Line::muted("Stopping..."));
        }
        cx.notify();
    }

    pub(crate) fn reset(&mut self, cx: &mut Context<Self>) {
        let Some(module) = self.selected.clone() else {
            return;
        };
        let Some(console) = self.consoles.get_mut(&module) else {
            return;
        };
        let id = console.session.next_id();
        let _ = console.session.send(Command::Reset { id });
        console.busy = Some(id);
        console.stopping = None;
        self.push_line(&module, Line::muted("Console reset"));
        cx.notify();
    }

    fn clear(&mut self, cx: &mut Context<Self>) {
        let Some(module) = self.selected.clone() else {
            return;
        };
        self.lines.remove(&module);
        cx.notify();
    }

    fn show_module_menu(&mut self, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let current = console_module(&self.state_dir, &path);
        let names = if self.modules.is_empty() {
            vec![default_module()]
        } else {
            self.modules.clone()
        };
        let entries: Vec<MenuEntry> = names
            .into_iter()
            .map(|name| {
                let panel = cx.weak_entity();
                let chosen = name.clone();
                let picked = current.as_deref() == Some(name.as_str());
                let entry = MenuEntry::new(name, move |window, cx| {
                    let _ = panel.update(cx, |panel, cx| {
                        panel.select_module(chosen.clone(), window, cx);
                    });
                });
                if picked {
                    entry.icon("general/check")
                } else {
                    entry
                }
            })
            .collect();
        let position = window.mouse_position();
        if let Some(workspace) = self.workspace.upgrade() {
            workspace.update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            });
        }
    }

    fn select_module(&mut self, module: String, window: &mut Window, cx: &mut Context<Self>) {
        self.open_console(module, window, cx);
    }

    fn toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let stop = toolbar_button("jshell-stop", "run/stop", "Stop", None, theme)
            .debug_selector(|| "jshell-stop".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.stop(cx)));
        let reset = panel_button("jshell-reset", "Reset", theme)
            .on_click(cx.listener(|this, _, _, cx| this.reset(cx)));
        let clear = panel_button("jshell-clear", "Clear", theme)
            .on_click(cx.listener(|this, _, _, cx| this.clear(cx)));
        let status = self.status.clone().unwrap_or_default();
        div()
            .id("jshell-toolbar")
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .border_b_1()
            .border_color(theme.border)
            .child(stop)
            .child(reset)
            .child(clear)
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_muted)
                    .child(status),
            )
            .into_any_element()
    }
}

impl Drop for JShellPanel {
    fn drop(&mut self) {
        let consoles: Vec<OpenConsole> =
            self.consoles.drain().map(|(_, console)| console).collect();
        for console in consoles {
            if let Some(uri) = &console.uri {
                repl::hub().unregister(uri);
            }
            console.session.kill();
        }
    }
}

impl Render for JShellPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let options =
            crate::tool_menu::options_menu(workspace.clone(), workspace::JSHELL_TOOL, |_| {
                Vec::new()
            });
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(JSHELL_TOOL, window, cx)
                })
                .ok();
        };
        let toolbar = self.toolbar(&theme, cx);
        let module = self.selected.clone();
        let lines = module
            .as_deref()
            .map(|module| self.lines_of(module))
            .unwrap_or_default();
        let rows = lines
            .into_iter()
            .map(|line| div().whitespace_nowrap().text_color(theme.text).child(line));
        let scroll = self.scroll.clone();
        let body = scrollable(
            "jshell-output-scroll",
            &self.scroll,
            div()
                .id("jshell-output")
                .flex()
                .flex_col()
                .w_full()
                .p(px(SPACE_S))
                .font_family("monospace")
                .text_size(px(TEXT_SMALL))
                .overflow_y_scroll()
                .track_scroll(&scroll)
                .children(rows),
            &theme,
            window,
            cx,
        );
        let hint = if module.is_none() {
            Some(
                div()
                    .p(px(SPACE_M))
                    .text_color(theme.text_secondary)
                    .child("Open the console with Tools > JShell Console."),
            )
        } else {
            None
        };
        island(&theme)
            .id("jshell-panel")
            .debug_selector(|| "jshell-panel".to_owned())
            .flex()
            .flex_col()
            .h_full()
            .child(panel_header(
                "JShell", GROUP, "jshell", options, hide, &theme,
            ))
            .child(toolbar)
            .children(hint)
            .child(body.flex_1().min_h_0())
    }
}

/// Puts the window in the dock of the workspace and registers the command that opens it.
pub(crate) fn install(
    workspace: &mut Workspace,
    weak: WeakEntity<Workspace>,
    cx: &mut Context<Workspace>,
) {
    let state_dir = workspace.state_dir().to_path_buf();
    cx.set_global(JShellRoot(state_dir.clone()));
    let panel = JShellPanel::create(weak, state_dir, cx);
    let focus = panel.read(cx).focus();
    workspace.set_dock(
        DockSide::Bottom,
        JSHELL_TOOL,
        panel.clone().into(),
        Some(focus),
        cx,
    );
    cx.set_global(ActiveJShell(panel.clone()));
    workspace.register_command(OPEN, "JShell Console", move |workspace, window, cx| {
        workspace.show_tool(JSHELL_TOOL, window, cx);
        if let Some(panel) = cx.try_global::<ActiveJShell>() {
            let panel = panel.0.clone();
            panel.update(cx, |panel, cx| panel.open_default(window, cx));
        }
    });
    install_globals(cx);
}

/// The plain files of the console: Ctrl+Enter runs them, the bar above them selects the
/// module, and completion comes from the console of the module.
fn install_globals(cx: &mut App) {
    struct Installed;
    impl Global for Installed {}
    if cx.try_global::<Installed>().is_some() {
        return;
    }
    let Some(root) = cx.try_global::<JShellRoot>().map(|root| root.0.clone()) else {
        return;
    };
    let previous = cx.try_global::<RunFiles>().map(|files| files.0.clone());
    cx.set_global(RunFiles(Arc::new(move |path: &Path| {
        console_module(&root, path).is_some()
            || previous.as_ref().is_some_and(|previous| previous(path))
    })));
    let bar = cx.try_global::<FileBar>().map(|bar| bar.0.clone());
    cx.set_global(FileBar(Rc::new(
        move |path: &Path, window: &mut Window, cx: &mut Context<EditorView>| {
            file_bar(path, window, cx)
                .or_else(|| bar.as_ref().and_then(|bar| bar(path, window, cx)))
        },
    )));
    let mut languages = cx
        .try_global::<FileLanguages>()
        .map(|languages| languages.0.clone())
        .unwrap_or_default();
    if !languages
        .iter()
        .any(|language| language.id() == LanguageId("console"))
    {
        languages.push(Arc::new(ConsoleLanguage::new()));
    }
    cx.set_global(FileLanguages(languages));
    cx.set_global(Installed);
}

/// The row above a console file: the module whose console it is.
fn file_bar(path: &Path, _window: &mut Window, cx: &mut Context<EditorView>) -> Option<AnyElement> {
    let panel = cx.try_global::<ActiveJShell>()?.0.clone();
    let state_dir = panel.read_with(cx, |panel, _| panel.state_dir.clone());
    let module = console_module(&state_dir, path)?;
    let theme = *cx.global::<Theme>();
    let target = path.to_path_buf();
    let click = panel.clone();
    let button = toolbar_dropdown("jshell-module", None, module.clone(), &theme)
        .debug_selector(|| "jshell-module".to_owned())
        .on_click(move |_, window, cx| {
            click.update(cx, |panel, cx| {
                panel.show_module_menu(target.clone(), window, cx)
            });
        });
    Some(
        div()
            .id("jshell-file-bar")
            .flex()
            .items_center()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .border_b_1()
            .border_color(theme.border)
            .child(button)
            .into_any_element(),
    )
}

/// Runs the selection or the whole console file in the console of its module. The answer
/// is `false` when the file is not a console, so another tool may take the key.
pub(crate) fn run_line(
    view: &Entity<EditorView>,
    _line: usize,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let path = view.read(cx).path().to_path_buf();
    let Some(root) = cx.try_global::<JShellRoot>().map(|root| root.0.clone()) else {
        return false;
    };
    let Some(module) = console_module(&root, &path) else {
        return false;
    };
    let selection = view.read(cx).selected_text();
    let text = selection.or_else(|| view.read(cx).text());
    let Some(panel) = cx.try_global::<ActiveJShell>().map(|panel| panel.0.clone()) else {
        return false;
    };
    workspace.show_tool(JSHELL_TOOL, window, cx);
    if let Some(text) = text {
        panel.update(cx, |panel, cx| panel.run(&module, text, cx));
    }
    true
}

/// A tab of a console file was opened.
pub(crate) fn tab_opened(path: &Path, cx: &mut App) {
    let Some(panel) = cx.try_global::<ActiveJShell>().map(|panel| panel.0.clone()) else {
        return;
    };
    panel.update(cx, |panel, cx| panel.opened(path, cx));
}

fn console_path(state_dir: &Path, module: &str) -> PathBuf {
    state_dir
        .join(FOLDER)
        .join(repl::folder_name(module))
        .join(CONSOLE_FILE)
}

fn console_module(state_dir: &Path, path: &Path) -> Option<String> {
    repl::module_of_folder(&state_dir.join(FOLDER), path, CONSOLE_FILE)
}

fn default_module() -> String {
    "default".to_owned()
}

fn panel_button(id: impl Into<ElementId>, label: &str, theme: &Theme) -> Stateful<Div> {
    let hover = theme.hover;
    let pressed = theme.pressed;
    div()
        .id(id)
        .debug_selector({
            let name = label.to_owned();
            move || format!("jshell-{}", name.to_lowercase())
        })
        .flex()
        .items_center()
        .justify_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.control_border)
        .text_color(theme.text)
        .cursor_pointer()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .child(label.to_owned())
}

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

/// The language of the console files: the project does not own them, and completion comes
/// from the helper of the module whose console the file is.
struct ConsoleLanguage {
    syntax: NoSyntax,
    documents: Mutex<HashMap<String, Arc<str>>>,
}

impl ConsoleLanguage {
    fn new() -> Self {
        Self {
            syntax: NoSyntax(Queries::default()),
            documents: Mutex::new(HashMap::new()),
        }
    }

    fn text(&self, uri: &str) -> Option<Arc<str>> {
        self.documents
            .lock()
            .ok()
            .and_then(|documents| documents.get(uri).cloned())
    }
}

impl SemanticServer for ConsoleLanguage {
    fn open(&self, uri: &str, _: i32, text: Arc<str>) {
        if let Ok(mut documents) = self.documents.lock() {
            documents.insert(uri.to_owned(), text);
        }
    }

    fn change(&self, uri: &str, _: i32, text: Arc<str>, _: Vec<DocumentEdit>) {
        if let Ok(mut documents) = self.documents.lock() {
            documents.insert(uri.to_owned(), text);
        }
    }

    fn save(&self, _: &str) {}

    fn close(&self, uri: &str) {
        if let Ok(mut documents) = self.documents.lock() {
            documents.remove(uri);
        }
    }

    fn version(&self, uri: &str) -> Option<i32> {
        self.documents
            .lock()
            .ok()
            .is_some_and(|documents| documents.contains_key(uri))
            .then_some(1)
    }
}

impl Provider<DocumentSpot, CompletionList> for ConsoleLanguage {
    fn start(
        &self,
        query: DocumentSpot,
        cancel: Cancel,
        mut reply: lang_api::Reply<CompletionList>,
    ) {
        let Some(text) = self.text(&query.uri) else {
            let _ = reply.send(Answer::new(
                CompletionList {
                    entries: Vec::new(),
                    complete: true,
                },
                Confidence::Certain,
                SourceId::new("console"),
            ));
            return;
        };
        let cursor = offset_in(&text, query.point);
        // The console counts the characters of its own text, one UTF-16 unit each.
        let cursor = text[..cursor].encode_utf16().count();
        let uri = query.uri.clone();
        std::thread::spawn(move || {
            if cancel.is_cancelled() {
                return;
            }
            let items = repl::hub()
                .complete(&uri, &text, cursor, COMPLETION_TIMEOUT)
                .unwrap_or_else(|| Ok(Vec::new()))
                .unwrap_or_default();
            if cancel.is_cancelled() {
                return;
            }
            let mut entries = Vec::new();
            for item in items {
                if entries
                    .iter()
                    .any(|entry: &CompletionEntry| entry.label == item.text)
                {
                    continue;
                }
                entries.push(CompletionEntry {
                    label: item.text.clone(),
                    insert_text: Some(item.text),
                    detail: Some("value from the console".to_owned()),
                    ..CompletionEntry::default()
                });
            }
            let _ = reply.send(Answer::new(
                CompletionList {
                    entries,
                    complete: true,
                },
                Confidence::Certain,
                SourceId::new("console"),
            ));
        });
    }
}

impl LanguageSupport for ConsoleLanguage {
    fn id(&self) -> LanguageId {
        LanguageId("console")
    }

    fn matches(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("jsh"))
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        &self.syntax
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self)
    }
}

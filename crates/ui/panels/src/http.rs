//! The HTTP Response tool window: the gutter marks of the request lines of a `.http`
//! file, the environment selector above its text, the request at the caret and the
//! answer of the last one. The request itself runs away from the window.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use ::http_client::{
    Cancel, Environments, Error, Handler, HistoryEntry, PreparedRequest, Response, ScriptError,
    ScriptOutput, Shell, Span, TestResult, TokenKind, Variables, execute, from_curl, highlight,
    highlight_json, is_request_file, limits, load_environments, load_history, parse, prepare,
    pretty_json, read_body, run_script, save_response, to_curl,
};
use editor_view::{
    EditorStyle, EditorView, FileBar, HighlightKind, HighlightSpan, PasteText, PlainSyntax,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{HTTP_TOOL, Settings, Workspace};

const GROUP: &str = "bottom-panel";
/// The lines of headers the window shows at once.
const SHOWN_HEADERS: usize = 200;

/// The answer on screen, with the text the window paints.
struct Shown {
    status: u16,
    reason: String,
    headers: Vec<(String, String)>,
    url: String,
    elapsed_ms: u64,
    /// The body as the window shows it: indented when it is JSON.
    #[cfg_attr(not(test), allow(dead_code))]
    body: String,
    /// The body as it came, even when only the beginning is on screen.
    raw: String,
    lines: Vec<String>,
    /// The byte offset of every line of `body`.
    line_starts: Vec<usize>,
    spans: Vec<Span>,
    size: usize,
    truncated: bool,
    #[cfg_attr(not(test), allow(dead_code))]
    pretty: bool,
    /// The file of the body in the history, for Open in Editor.
    body_file: Option<PathBuf>,
    /// The results of the script of the answer.
    tests: Vec<TestResult>,
    /// The lines of `client.log` of the script.
    logs: Vec<String>,
    /// The place of the script that stopped it.
    script_error: Option<ScriptError>,
}

impl Shown {
    fn new(
        request: &PreparedRequest,
        response: &Response,
        entry: Option<HistoryEntry>,
        show_all: bool,
    ) -> Self {
        let raw = response.text();
        let raw_text = raw.clone();
        let pretty = pretty_json(&raw);
        let mut body = pretty.clone().unwrap_or(raw);
        let spans = if pretty.is_some() {
            highlight_json(&body)
        } else {
            Vec::new()
        };
        let mut truncated = false;
        if !show_all && body.len() > limits::SHOWN_BODY_BYTES {
            let mut cut = limits::SHOWN_BODY_BYTES;
            while cut > 0 && !body.is_char_boundary(cut) {
                cut -= 1;
            }
            body.truncate(cut);
            truncated = true;
        }
        let lines: Vec<String> = body.split('\n').map(str::to_owned).collect();
        let mut line_starts = Vec::with_capacity(lines.len());
        let mut start = 0;
        for line in &lines {
            line_starts.push(start);
            start += line.len() + 1;
        }
        Self {
            status: response.status,
            reason: response.reason.clone(),
            headers: response.headers.clone(),
            url: request.url.clone(),
            elapsed_ms: response.elapsed.as_millis() as u64,
            body,
            raw: raw_text,
            lines,
            line_starts,
            spans,
            size: response.size(),
            truncated,
            pretty: pretty.is_some(),
            body_file: entry.map(|entry| history_body_file(&entry)),
            tests: Vec::new(),
            logs: Vec::new(),
            script_error: None,
        }
    }

    /// The tests and the logs the script of this answer wrote.
    fn with_script(mut self, output: ScriptOutput) -> Self {
        self.tests = output.tests;
        self.logs = output.logs;
        self.script_error = output.error;
        self
    }

    fn status_line(&self) -> String {
        if self.reason.is_empty() {
            self.status.to_string()
        } else {
            format!("{} {}", self.status, self.reason)
        }
    }
}

/// Where the body of a history entry of this window lives, relative to the history
/// folder.
fn history_body_file(entry: &HistoryEntry) -> PathBuf {
    PathBuf::from(format!("{}.body", entry.id))
}

/// The tabs of the answer: the body and its headers, or the tests of its script.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HttpTab {
    Response,
    Tests,
}

/// What one press of Run brings back from the requests and their scripts.
struct RunOutcome {
    last: Option<(PreparedRequest, Response)>,
    entries: Vec<HistoryEntry>,
    failure: Option<Error>,
    /// The variables the scripts set and the next requests use.
    globals: BTreeMap<String, String>,
    script: ScriptOutput,
}

/// The source of the handler of a request: the text between `{%` and `%}`, or the file
/// named after `>` relative to the request file.
fn handler_source(handler: &Handler, base: &Path) -> Result<String, String> {
    match handler {
        Handler::Inline(source) => Ok(source.clone()),
        Handler::File(path) => {
            let path = PathBuf::from(path);
            let path = if path.is_absolute() {
                path
            } else {
                base.join(path)
            };
            std::fs::read_to_string(&path)
                .map_err(|error| format!("Cannot read the script {}: {error}", path.display()))
        }
    }
}

pub struct HttpPanel {
    workspace: WeakEntity<Workspace>,
    state_dir: PathBuf,
    focus: FocusHandle,
    /// The request file whose environment the bar above the text shows.
    file: Option<PathBuf>,
    environments: HashMap<PathBuf, Environments>,
    selected: HashMap<PathBuf, String>,
    /// The variables a script of an answer set; they live until the window closes.
    globals: BTreeMap<String, String>,
    shown: Option<Shown>,
    tab: HttpTab,
    error: Option<String>,
    history: Vec<HistoryEntry>,
    headers_open: bool,
    show_all: bool,
    running: bool,
    cancel: Option<Cancel>,
    /// Grows with every run, so that only the last answer reaches the window.
    serial: u64,
    scroll: UniformListScrollHandle,
}

/// Lets the window and the editor reach the response panel.
pub struct ActiveHttp(pub Entity<HttpPanel>);

impl Global for ActiveHttp {}

impl HttpPanel {
    pub fn create(
        workspace: WeakEntity<Workspace>,
        state_dir: PathBuf,
        cx: &mut App,
    ) -> Entity<Self> {
        let panel = cx.new(|cx| Self {
            workspace,
            state_dir,
            focus: cx.focus_handle(),
            file: None,
            environments: HashMap::new(),
            selected: HashMap::new(),
            globals: BTreeMap::new(),
            shown: None,
            tab: HttpTab::Response,
            error: None,
            history: Vec::new(),
            headers_open: false,
            show_all: false,
            running: false,
            cancel: None,
            serial: 0,
            scroll: UniformListScrollHandle::new(),
        });
        panel.update(cx, |panel, cx| panel.load_history(cx));
        panel
    }

    /// Reads the answers kept in the state folder in the background.
    fn load_history(&mut self, cx: &mut Context<Self>) {
        let dir = self.state_dir.clone();
        let job = cx
            .background_executor()
            .spawn(async move { load_history(&dir) });
        cx.spawn(async move |this, cx| {
            let history = job.await;
            this.update(cx, |this, cx| {
                this.history.extend(history);
                this.history
                    .sort_by_key(|entry| std::cmp::Reverse(entry.id));
                this.history.dedup_by_key(|entry| entry.id);
                this.history.truncate(limits::HISTORY);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The panel follows the request file of the active editor: its environments are
    /// read in the background the first time it is seen.
    pub fn follow(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        if !is_request_file(&path) {
            return;
        }
        self.file = Some(path.clone());
        if self.environments.contains_key(&path) {
            cx.notify();
            return;
        }
        let request = path.clone();
        let job = cx
            .background_executor()
            .spawn(async move { load_environments(&request) });
        cx.spawn(async move |this, cx| {
            let environments = job.await;
            this.update(cx, |this, cx| {
                this.environments.insert(path, environments);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The environment names of a request file, empty until its files are read.
    pub fn environment_names_for(&self, path: &Path) -> Vec<String> {
        self.environments
            .get(path)
            .map(|environments| environments.names().to_vec())
            .unwrap_or_default()
    }

    /// The environment a request of `path` uses: the one picked, else the first one.
    pub fn selected_for(&self, path: &Path) -> Option<String> {
        let picked = self.selected.get(path).cloned();
        if picked.as_deref() == Some("") {
            return None;
        }
        let environments = self.environments.get(path)?;
        environments.resolved_name(picked.as_deref())
    }

    /// Picks the environment of a request file. The next request of the file uses it.
    pub fn select_environment(&mut self, path: PathBuf, name: String, cx: &mut Context<Self>) {
        self.selected.insert(path, name);
        cx.notify();
    }

    /// The values a request of `path` substitutes: the environment in force over the
    /// variables the scripts set.
    pub(crate) fn variables_for(&self, path: &Path) -> BTreeMap<String, String> {
        let environments = self
            .environments
            .get(path)
            .cloned()
            .unwrap_or_else(|| load_environments(path));
        let picked = self.selected.get(path).cloned();
        let name = if picked.as_deref() == Some("") {
            None
        } else {
            environments.resolved_name(picked.as_deref())
        };
        let mut values = name
            .as_deref()
            .map(|name| environments.values(name))
            .unwrap_or_default();
        values.extend(self.globals.clone());
        values
    }

    /// Opens the picker of two answers of the history to compare.
    pub(crate) fn show_compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries: Vec<HistoryEntry> =
            self.history.iter().take(limits::HISTORY).cloned().collect();
        if entries.len() < 2 {
            return;
        }
        let panel = cx.weak_entity();
        let workspace = self.workspace.clone();
        let view = cx.new(|cx| CompareResponses {
            workspace,
            panel,
            focus: cx.focus_handle(),
            entries,
            selected: Vec::new(),
        });
        let focus = view.read(cx).focus.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_overlay(view.into(), focus, window, cx)
            })
            .ok();
    }

    /// Opens the bodies of two answers in the diff window, the JSON ones indented.
    pub(crate) fn compare_entries(
        &mut self,
        left: HistoryEntry,
        right: HistoryEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(git) = cx
            .try_global::<crate::git::ActiveGit>()
            .map(|git| git.0.clone())
        else {
            return;
        };
        let dir = self.state_dir.clone();
        let path = self
            .file
            .clone()
            .unwrap_or_else(|| self.state_dir.join("http-history").join("responses.http"));
        let workspace = self.workspace.clone();
        let job = cx
            .background_executor()
            .spawn(async move { (read_body(&dir, &left), read_body(&dir, &right)) });
        window
            .spawn(cx, async move |cx| {
                let (left, right) = job.await;
                let left = left.unwrap_or_default();
                let right = right.unwrap_or_default();
                let left = pretty_json(&left).unwrap_or(left);
                let right = pretty_json(&right).unwrap_or(right);
                let weak = workspace.clone();
                cx.update(|window, cx| {
                    workspace
                        .update(cx, |_, cx| {
                            crate::diff_view::DiffView::compare_texts(
                                git,
                                weak,
                                path.clone(),
                                left,
                                right,
                                window,
                                cx,
                            );
                        })
                        .ok();
                })
                .ok();
            })
            .detach();
    }

    /// The names of every environment of the file on screen.
    #[cfg(test)]
    pub fn environment_names(&self) -> Vec<String> {
        self.file
            .as_deref()
            .map(|path| self.environment_names_for(path))
            .unwrap_or_default()
    }

    /// The environment in force for the file on screen.
    #[cfg(test)]
    pub fn selected_environment(&self) -> Option<String> {
        self.file
            .as_deref()
            .and_then(|path| self.selected_for(path))
    }

    /// The status line of the answer on screen, `200 OK` for one.
    #[cfg(test)]
    pub fn status_text(&self) -> Option<String> {
        self.shown.as_ref().map(Shown::status_line)
    }

    /// The body on screen, for the tests.
    #[cfg(test)]
    pub fn body_text(&self) -> Option<&str> {
        self.shown.as_ref().map(|shown| shown.body.as_str())
    }

    /// Whether the body is JSON, so it is painted with its colours.
    #[cfg(test)]
    pub fn body_is_pretty(&self) -> bool {
        self.shown.as_ref().is_some_and(|shown| shown.pretty)
    }

    #[cfg(test)]
    pub fn history(&self) -> &[HistoryEntry] {
        &self.history
    }

    /// The URL of the answer on screen.
    #[cfg(test)]
    pub fn shown_url(&self) -> Option<&str> {
        self.shown.as_ref().map(|shown| shown.url.as_str())
    }

    /// The tests the script of the answer on screen ran.
    #[cfg(test)]
    pub fn tests(&self) -> &[TestResult] {
        self.shown
            .as_ref()
            .map_or(&[], |shown| shown.tests.as_slice())
    }

    /// The lines of `client.log` of the answer on screen.
    #[cfg(test)]
    pub fn test_logs(&self) -> &[String] {
        self.shown
            .as_ref()
            .map_or(&[], |shown| shown.logs.as_slice())
    }

    /// The place of the script of the answer on screen that stopped it.
    #[cfg(test)]
    pub fn script_error(&self) -> Option<&ScriptError> {
        self.shown
            .as_ref()
            .and_then(|shown| shown.script_error.as_ref())
    }

    /// The variables the scripts set, which the next requests use.
    #[cfg(test)]
    pub fn globals(&self) -> &BTreeMap<String, String> {
        &self.globals
    }

    /// Shows the tests of the answer, as a click on the tab does.
    #[cfg(test)]
    pub fn choose_tests(&mut self, cx: &mut Context<Self>) {
        self.tab = HttpTab::Tests;
        cx.notify();
    }

    /// Sends the request at `line`, or every request of the file when `line` is `None`.
    pub fn run(
        &mut self,
        path: PathBuf,
        text: String,
        line: Option<usize>,
        cx: &mut Context<Self>,
    ) {
        if !is_request_file(&path) {
            return;
        }
        let parsed = parse(&text);
        let requests = match line {
            Some(line) => parsed.at_line(line).cloned().into_iter().collect(),
            None => parsed.requests.clone(),
        };
        if requests.is_empty() {
            self.error = Some(match line {
                Some(_) => "The caret is not in a request".to_owned(),
                None => "The file has no requests".to_owned(),
            });
            cx.notify();
            return;
        }
        let selected = self.selected.get(&path).cloned();
        let known = self.environments.get(&path).cloned();
        let globals = self.globals.clone();
        let timeout = timeout_of(cx);
        let cancel = Cancel::new();
        self.cancel = Some(cancel.clone());
        self.running = true;
        self.error = None;
        self.serial += 1;
        let serial = self.serial;
        let base = path
            .parent()
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
        let state_dir = self.state_dir.clone();
        let job = cx.background_executor().spawn(async move {
            let environments = known.unwrap_or_else(|| load_environments(&path));
            let name = match selected.as_deref() {
                Some("") => None,
                Some(name) => Some(name.to_owned()),
                None => environments.default_name().map(str::to_owned),
            };
            let values = name
                .as_deref()
                .map(|name| environments.values(name))
                .unwrap_or_default();
            let mut globals = globals;
            let mut last = None;
            let mut entries = Vec::new();
            let mut failure = None;
            let mut tests = Vec::new();
            let mut logs = Vec::new();
            let mut script_error = None;
            for request in requests {
                if cancel.is_cancelled() {
                    failure = Some(Error::Cancelled);
                    break;
                }
                let mut known = values.clone();
                known.extend(globals.clone());
                let variables = Variables::for_request(known);
                let outcome = prepare(&request, &base, &variables).and_then(|prepared| {
                    execute(&prepared, timeout, &cancel).map(|response| (prepared, response))
                });
                match outcome {
                    Ok((prepared, response)) => {
                        if let Ok(entry) = save_response(&state_dir, &prepared, &response) {
                            entries.push(entry);
                        }
                        if let Some(handler) = &request.handler {
                            match handler_source(handler, &base) {
                                Ok(source) => {
                                    let output = run_script(&source, &response, &mut globals);
                                    tests.extend(output.tests);
                                    logs.extend(output.logs);
                                    if output.error.is_some() {
                                        script_error = output.error;
                                    }
                                }
                                Err(message) => {
                                    script_error = Some(ScriptError { line: 1, message });
                                }
                            }
                        }
                        last = Some((prepared, response));
                    }
                    Err(error) => {
                        failure = Some(error);
                        break;
                    }
                }
            }
            RunOutcome {
                last,
                entries,
                failure,
                globals,
                script: ScriptOutput {
                    tests,
                    logs,
                    error: script_error,
                },
            }
        });
        cx.spawn(async move |this, cx| {
            let outcome = job.await;
            this.update(cx, |this, cx| this.finish(serial, outcome, cx))
                .ok();
        })
        .detach();
        cx.notify();
    }

    /// Takes the answer of a run, unless a newer run started meanwhile.
    fn finish(&mut self, serial: u64, outcome: RunOutcome, cx: &mut Context<Self>) {
        if serial != self.serial {
            return;
        }
        let RunOutcome {
            last,
            entries,
            failure,
            globals,
            script,
        } = outcome;
        self.running = false;
        self.cancel = None;
        self.show_all = false;
        self.globals = globals;
        for entry in entries.into_iter().rev() {
            self.history.insert(0, entry);
        }
        self.history
            .sort_by_key(|entry| std::cmp::Reverse(entry.id));
        self.history.dedup_by_key(|entry| entry.id);
        self.history.truncate(limits::HISTORY);
        match last {
            Some((request, response)) => {
                let entry = self.history.first().cloned();
                self.shown =
                    Some(Shown::new(&request, &response, entry, false).with_script(script));
                self.error = failure.map(|error| error.to_string());
            }
            None => {
                self.error = Some(
                    failure
                        .map(|error| error.to_string())
                        .unwrap_or_else(|| "The request was stopped".to_owned()),
                );
            }
        }
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.cancel {
            cancel.cancel();
        }
        cx.notify();
    }

    fn show_all(&mut self, cx: &mut Context<Self>) {
        let Some(shown) = &self.shown else {
            return;
        };
        let request = PreparedRequest {
            name: None,
            method: ::http_client::Method::Get,
            url: shown.url.clone(),
            headers: Vec::new(),
            body: None,
            line: 0,
        };
        let response = Response {
            status: shown.status,
            reason: shown.reason.clone(),
            headers: shown.headers.clone(),
            body: shown.raw.as_bytes().to_vec(),
            elapsed: Duration::from_millis(shown.elapsed_ms),
        };
        let body_file = shown.body_file.clone();
        let tests = shown.tests.clone();
        let logs = shown.logs.clone();
        let script_error = shown.script_error.clone();
        let mut all = Shown::new(&request, &response, None, true);
        all.body_file = body_file;
        all.tests = tests;
        all.logs = logs;
        all.script_error = script_error;
        self.shown = Some(all);
        self.show_all = true;
        cx.notify();
    }

    fn show_history_entry(&mut self, entry: HistoryEntry, cx: &mut Context<Self>) {
        let dir = self.state_dir.clone();
        let name = history_body_file(&entry);
        let stored = entry.clone();
        let job = cx
            .background_executor()
            .spawn(async move { read_body(&dir, &stored) });
        let entry_for_update = entry.clone();
        cx.spawn(async move |this, cx| {
            let body = job.await.unwrap_or_default();
            this.update(cx, |this, cx| {
                let request = PreparedRequest {
                    name: entry_for_update.name.clone(),
                    method: ::http_client::Method::parse(&entry_for_update.method)
                        .unwrap_or(::http_client::Method::Get),
                    url: entry_for_update.url.clone(),
                    headers: Vec::new(),
                    body: None,
                    line: 0,
                };
                let response = Response {
                    status: entry_for_update.status,
                    reason: entry_for_update.reason.clone(),
                    headers: Vec::new(),
                    body: body.into_bytes(),
                    elapsed: Duration::from_millis(entry_for_update.elapsed_ms),
                };
                let body_file = name;
                let mut shown = Shown::new(&request, &response, None, true);
                shown.body_file = Some(body_file);
                this.shown = Some(shown);
                this.error = None;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn show_environment_menu(
        &mut self,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let names = self.environment_names_for(&path);
        let current = self.selected_for(&path);
        let entries: Vec<MenuEntry> = names
            .into_iter()
            .map(|name| {
                let picked = current.as_deref() == Some(name.as_str());
                let panel = cx.weak_entity();
                let path = path.clone();
                let chosen = name.clone();
                let entry = MenuEntry::new(name, move |_, cx| {
                    panel
                        .update(cx, |panel, cx| {
                            panel.select_environment(path.clone(), chosen.clone(), cx)
                        })
                        .ok();
                });
                if picked {
                    entry.icon("general/check")
                } else {
                    entry
                }
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        let position = window.mouse_position();
        let entries = [MenuEntry::new("No Environment", {
            let panel = cx.weak_entity();
            let path = path.clone();
            move |_, cx| {
                panel
                    .update(cx, |panel, cx| {
                        panel.select_environment(path.clone(), String::new(), cx)
                    })
                    .ok();
            }
        })]
        .into_iter()
        .chain(std::iter::once(MenuEntry::separator()))
        .chain(entries)
        .collect();
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn show_history_menu(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let entries: Vec<MenuEntry> = self
            .history
            .iter()
            .take(limits::HISTORY)
            .map(|entry| {
                let panel = cx.weak_entity();
                let chosen = entry.clone();
                let label = format!("{}  {}", entry.status_line(), entry.label());
                MenuEntry::new(label, move |_, cx| {
                    panel
                        .update(cx, |panel, cx| panel.show_history_entry(chosen.clone(), cx))
                        .ok();
                })
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        let mut entries = entries;
        if self.history.len() >= 2 {
            entries.push(MenuEntry::separator());
            let panel = cx.weak_entity();
            entries.push(MenuEntry::new("Compare Responses...", move |window, cx| {
                panel
                    .update(cx, |panel, cx| panel.show_compare(window, cx))
                    .ok();
            }));
        }
        let position = window.mouse_position();
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn open_in_editor(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self
            .shown
            .as_ref()
            .and_then(|shown| shown.body_file.clone())
        else {
            return;
        };
        let file = self.state_dir.join("http-history").join(file);
        self.workspace
            .update(cx, |workspace, cx| workspace.open_file(file, window, cx))
            .ok();
    }

    fn toolbar(&self, window: &mut Window, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let mut toolbar = div()
            .id("http-toolbar")
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .border_b_1()
            .border_color(theme.border);
        if self.running {
            toolbar = toolbar.child(
                icon_button("http-stop", "run/stop", "Stop Request", None, theme)
                    .on_click(cx.listener(|this, _, _, cx| this.stop(cx))),
            );
        }
        if !self.history.is_empty() {
            let history = toolbar_dropdown("http-history", None, "History", theme)
                .debug_selector(|| "http-history".to_owned())
                .on_click(cx.listener(|this, _, window, cx| this.show_history_menu(window, cx)));
            toolbar = toolbar.child(history);
        }
        if self.history.len() >= 2 {
            toolbar = toolbar.child(
                icon_button(
                    "http-compare",
                    "general/editorAndPreview",
                    "Compare Responses",
                    None,
                    theme,
                )
                .debug_selector(|| "http-compare".to_owned())
                .on_click(cx.listener(|this, _, window, cx| this.show_compare(window, cx))),
            );
        }
        if self
            .shown
            .as_ref()
            .is_some_and(|shown| shown.body_file.is_some())
        {
            toolbar = toolbar.child(
                icon_button("http-open", "general/locate", "Open in Editor", None, theme)
                    .on_click(cx.listener(|this, _, window, cx| this.open_in_editor(window, cx))),
            );
        }
        if let Some(shown) = &self.shown {
            toolbar = toolbar.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child(shown.status_line())
                    .child(format!("{} ms", shown.elapsed_ms))
                    .child(size_text(shown.size)),
            );
        }
        let _ = window;
        toolbar
    }

    fn tabs(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .child(tab_button(
                "http-tab-response",
                "Response",
                self.tab == HttpTab::Response,
                theme,
                cx.listener(|this, _, _, cx| {
                    this.tab = HttpTab::Response;
                    cx.notify();
                }),
            ))
            .child(tab_button(
                "http-tab-tests",
                "Tests",
                self.tab == HttpTab::Tests,
                theme,
                cx.listener(|this, _, _, cx| {
                    this.tab = HttpTab::Tests;
                    cx.notify();
                }),
            ))
    }

    /// The tests and the logs of the script under the answer.
    fn tests_body(&self, theme: &Theme) -> Stateful<Div> {
        let mut list = div()
            .id("http-tests")
            .debug_selector(|| "http-tests".to_owned())
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .gap(px(SPACE_XS))
            .p(px(SPACE_M));
        let Some(shown) = &self.shown else {
            return list.child(
                div()
                    .text_color(theme.text_secondary)
                    .child("Run a request with a response handler to see its tests."),
            );
        };
        if let Some(error) = &shown.script_error {
            list = list.child(
                div()
                    .debug_selector(|| "http-test-script-error".to_owned())
                    .text_color(theme.stripe_error)
                    .child(error.to_string()),
            );
        }
        for (index, test) in shown.tests.iter().enumerate() {
            let colour = if test.passed {
                theme.console_input
            } else {
                theme.stripe_error
            };
            list = list.child(
                div()
                    .debug_selector(move || format!("http-test-row-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .text_color(colour)
                    .child(icon_sized(
                        if test.passed {
                            "general/check"
                        } else {
                            "general/close"
                        },
                        TEXT_SMALL,
                        theme,
                    ))
                    .child(test.name.clone())
                    .children(test.message.clone().map(|message| {
                        div()
                            .text_color(theme.text_secondary)
                            .child(format!("— {message}"))
                    })),
            );
        }
        for log in &shown.logs {
            list = list.child(
                div()
                    .text_color(theme.text_secondary)
                    .child(format!("log: {log}")),
            );
        }
        if shown.tests.is_empty() && shown.script_error.is_none() && shown.logs.is_empty() {
            list = list.child(
                div()
                    .text_color(theme.text_secondary)
                    .child("The request has no response handler."),
            );
        }
        list
    }

    fn headers_block(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        let shown = self.shown.as_ref()?;
        let label = div()
            .id("http-headers-toggle")
            .debug_selector(|| "http-headers-toggle".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .text_color(theme.text_secondary)
            .on_click(cx.listener(|this, _, _, cx| {
                this.headers_open = !this.headers_open;
                cx.notify();
            }))
            .child(icon_sized(
                if self.headers_open {
                    "general/chevronDown"
                } else {
                    "general/chevronRight"
                },
                TEXT_SMALL,
                theme,
            ))
            .child(format!("Headers ({})", shown.headers.len()));
        let block = self.headers_open.then(|| {
            let rows: Vec<Div> = shown
                .headers
                .iter()
                .take(SHOWN_HEADERS)
                .map(|(name, value)| {
                    div()
                        .whitespace_normal()
                        .text_color(theme.text)
                        .child(format!("{name}: {value}"))
                })
                .collect();
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .px(px(SPACE_M))
                .pb(px(SPACE_S))
                .children(rows)
                .into_any_element()
        });
        Some(
            div()
                .flex()
                .flex_col()
                .flex_none()
                .child(label)
                .children(block)
                .into_any_element(),
        )
    }
}

/// The picker of two answers of the history: their bodies go to the diff window.
struct CompareResponses {
    workspace: WeakEntity<Workspace>,
    panel: WeakEntity<HttpPanel>,
    focus: FocusHandle,
    entries: Vec<HistoryEntry>,
    /// The rows picked, at most two and in the order they were picked.
    selected: Vec<usize>,
}

impl CompareResponses {
    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(at) = self.selected.iter().position(|picked| *picked == index) {
            self.selected.remove(at);
        } else {
            if self.selected.len() == 2 {
                self.selected.remove(0);
            }
            self.selected.push(index);
        }
        cx.notify();
    }

    fn compare(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.len() != 2 {
            return;
        }
        let entries: Vec<HistoryEntry> = self
            .selected
            .iter()
            .filter_map(|index| self.entries.get(*index).cloned())
            .collect();
        if entries.len() != 2 {
            return;
        }
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
        self.panel
            .update(cx, |panel, cx| {
                panel.compare_entries(entries[0].clone(), entries[1].clone(), window, cx)
            })
            .ok();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }
}

impl Render for CompareResponses {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(520.).min(viewport.width - px(40.));
        let height =
            (px(120.) + px(24.) * self.entries.len() as f32).min(viewport.height - px(40.));
        let rows: Vec<AnyElement> = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let picked = self.selected.contains(&index);
                div()
                    .id(("http-compare-row", index))
                    .debug_selector(move || format!("http-compare-row-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .h(px(24.))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .when(picked, |this| this.bg(theme.selection))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
                    .child(icon_sized(
                        if picked {
                            "general/check"
                        } else {
                            "general/chevronRight"
                        },
                        TEXT_SMALL,
                        &theme,
                    ))
                    .child(format!("{}  {}", entry.status_line(), entry.label()))
                    .into_any_element()
            })
            .collect();
        let enabled = self.selected.len() == 2;
        surface(&theme)
            .id("http-compare-dialog")
            .debug_selector(|| "http-compare-dialog".to_owned())
            .key_context("HttpCompare")
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .h(height)
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .flex_none()
                    .text_color(theme.text)
                    .child("Compare two responses"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .gap(px(SPACE_XS))
                    .overflow_hidden()
                    .children(rows),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .justify_end()
                    .gap(px(SPACE_S))
                    .child(
                        crate::project_structure::dialog_button(
                            "http-compare-cancel",
                            "http-compare-cancel".to_owned(),
                            "Cancel",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        crate::project_structure::dialog_button(
                            "http-compare-run",
                            "http-compare-run".to_owned(),
                            "Compare",
                            true,
                            enabled,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.compare(window, cx))),
                    ),
            )
    }
}

fn tab_button(
    id: &'static str,
    label: &'static str,
    active: bool,
    theme: &Theme,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .items_center()
        .flex_none()
        .h(px(24.))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .text_color(if active {
            theme.text
        } else {
            theme.text_secondary
        })
        .when(active, |this| this.bg(theme.selection_inactive))
        .when(!active, |this| {
            let hover = theme.hover;
            this.hover(move |style| style.bg(hover))
        })
        .on_click(on_click)
        .child(label)
}

fn timeout_of(cx: &App) -> Duration {
    let seconds = cx
        .try_global::<Settings>()
        .map_or(workspace::DEFAULT_HTTP_TIMEOUT_SECONDS, |settings| {
            settings.http_timeout_seconds
        });
    Duration::from_secs(seconds.max(1) as u64)
}

fn size_text(size: usize) -> String {
    if size < 1024 {
        format!("{size} B")
    } else if size < 1024 * 1024 {
        format!("{:.1} KB", size as f64 / 1024.)
    } else {
        format!("{:.1} MB", size as f64 / (1024. * 1024.))
    }
}

fn colour_of(kind: TokenKind, style: &EditorStyle) -> Hsla {
    match kind {
        TokenKind::Keyword => style.keyword,
        TokenKind::String => style.string,
        TokenKind::Field => style.field,
        TokenKind::Constant => style.field,
        TokenKind::Comment => style.comment,
        TokenKind::Number => style.number,
    }
}

fn body_row(text: &str, start: usize, spans: &[Span], style: &EditorStyle) -> AnyElement {
    let end = start + text.len();
    let highlights = spans
        .iter()
        .filter(|span| span.end as usize > start && (span.start as usize) < end)
        .map(|span| {
            let from = (span.start as usize).saturating_sub(start);
            let to = (span.end as usize).min(end) - start;
            (
                from..to,
                HighlightStyle {
                    color: Some(colour_of(span.kind, style)),
                    ..HighlightStyle::default()
                },
            )
        });
    div()
        .h(px(CODE_LINE_HEIGHT))
        .px(px(SPACE_S))
        .whitespace_nowrap()
        .child(StyledText::new(text.to_owned()).with_highlights(highlights))
        .into_any_element()
}

impl Render for HttpPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(HTTP_TOOL, window, cx)
                })
                .ok();
        };
        let toolbar = self.toolbar(window, &theme, cx);
        let tabs = self.tabs(&theme, cx);
        let response_tab = self.tab == HttpTab::Response;
        let headers = response_tab
            .then(|| self.headers_block(&theme, cx))
            .flatten();
        let hint = (response_tab && self.shown.is_none()).then(|| {
            div()
                .p(px(SPACE_M))
                .text_color(theme.text_secondary)
                .child(match &self.error {
                Some(error) => error.clone(),
                None => {
                    "Run a request from an .http file with the icon in the gutter or Ctrl+Enter."
                        .to_owned()
                }
            })
        });
        let body = (response_tab && self.shown.is_some()).then(|| {
            let shown = self.shown.as_ref()?;
            let style = EditorStyle::from_theme(&theme);
            let lines = shown.lines.clone();
            let starts = shown.line_starts.clone();
            let spans = shown.spans.clone();
            let count = lines.len().max(1);
            let truncated = shown.truncated;
            let rows = cx.processor(move |_this, range: std::ops::Range<usize>, _, _| {
                range
                    .filter_map(|index| {
                        let line = lines.get(index)?;
                        let start = starts.get(index).copied().unwrap_or(0);
                        Some(body_row(line, start, &spans, &style))
                    })
                    .collect::<Vec<_>>()
            });
            let list = scrollable(
                "http-body",
                &self.scroll,
                div().size_full().child(
                    uniform_list("http-body", count, rows)
                        .track_scroll(&self.scroll)
                        .size_full(),
                ),
                &theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0();
            let mut area = div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(list)
                .into_any_element();
            if truncated || self.show_all {
                let show_all = self.show_all;
                let more = div()
                    .id("http-show-all")
                    .debug_selector(|| "http-show-all".to_owned())
                    .flex()
                    .flex_none()
                    .items_center()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .text_color(theme.accent)
                    .on_click(cx.listener(|this, _, _, cx| {
                        if this.show_all {
                            return;
                        }
                        this.show_all(cx);
                    }))
                    .child(if show_all {
                        "The whole body is shown"
                    } else {
                        "Show all"
                    });
                area = div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .child(area)
                    .child(more)
                    .into_any_element();
            }
            Some(area)
        });
        let body = body.flatten();
        let tests = (self.tab == HttpTab::Tests).then(|| self.tests_body(&theme));

        island(&theme)
            .id("http-panel")
            .debug_selector(|| "http-panel".to_owned())
            .key_context("HttpPanel ToolWindow")
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .child(panel_header("HTTP Response", GROUP, hide, &theme))
            .child(toolbar)
            .child(tabs)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .children(headers)
                    .children(hint)
                    .children(body)
                    .children(tests),
            )
    }
}

/// The gutter marks of the requests of a request file.
pub(crate) fn request_lines(path: &Path, text: &str) -> Vec<u32> {
    if !is_request_file(path) {
        return Vec::new();
    }
    parse(text)
        .requests
        .iter()
        .map(|request| request.line as u32)
        .collect()
}

/// Sends the request on `line`, from zero, and shows the window.
pub(crate) fn run_line(
    view: &Entity<EditorView>,
    line: usize,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let path = view.read(cx).path().to_path_buf();
    if !is_request_file(&path) {
        return;
    }
    let Some(text) = view.read(cx).text() else {
        return;
    };
    let Some(panel) = cx.try_global::<ActiveHttp>().map(|panel| panel.0.clone()) else {
        return;
    };
    workspace.show_tool(HTTP_TOOL, window, cx);
    panel.update(cx, |panel, cx| panel.run(path, text, Some(line), cx));
}

/// Sends every request of the file of the view, one after another.
pub(crate) fn run_all(
    view: &Entity<EditorView>,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let path = view.read(cx).path().to_path_buf();
    if !is_request_file(&path) {
        return;
    }
    let Some(text) = view.read(cx).text() else {
        return;
    };
    let Some(panel) = cx.try_global::<ActiveHttp>().map(|panel| panel.0.clone()) else {
        return;
    };
    workspace.show_tool(HTTP_TOOL, window, cx);
    panel.update(cx, |panel, cx| panel.run(path, text, None, cx));
}

/// Copy as cURL: the request at the caret of the editor, with the variables of the
/// environment and of the scripts in place.
pub(crate) fn copy_as_curl(
    view: &Entity<EditorView>,
    shell: Shell,
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) {
    let path = view.read(cx).path().to_path_buf();
    if !is_request_file(&path) {
        workspace.notify_user("Copy as cURL", "The file is not a request file", cx);
        return;
    }
    let Some(text) = view.read(cx).text() else {
        return;
    };
    let line = view.read(cx).caret().0;
    let parsed = parse(&text);
    let Some(request) = parsed
        .at_line(line)
        .or_else(|| parsed.requests.first())
        .cloned()
    else {
        workspace.notify_user("Copy as cURL", "The file has no request", cx);
        return;
    };
    let values = cx
        .try_global::<ActiveHttp>()
        .map(|panel| panel.0.read(cx).variables_for(&path))
        .unwrap_or_default();
    let variables = Variables::for_request(values);
    let base = path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    match prepare(&request, &base, &variables) {
        Ok(prepared) => {
            cx.write_to_clipboard(ClipboardItem::new_string(to_curl(&prepared, shell)));
        }
        Err(error) => workspace.notify_user("Copy as cURL", error.to_string(), cx),
    }
}

/// Copy as cURL of Find Action and the editor menu: the active editor, if it is a
/// request file.
pub(crate) fn copy_as_curl_active(
    shell: Shell,
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<EditorView>().ok())
    else {
        workspace.notify_user("Copy as cURL", "No file is open", cx);
        return;
    };
    copy_as_curl(&view, shell, workspace, cx);
}

/// The request file of the active editor changed its environment list.
pub(crate) fn file_opened(view: &Entity<EditorView>, cx: &mut App) {
    let path = view.read(cx).path().to_path_buf();
    if !is_request_file(&path) {
        return;
    }
    let Some(panel) = cx.try_global::<ActiveHttp>().map(|panel| panel.0.clone()) else {
        return;
    };
    panel.update(cx, |panel, cx| panel.follow(path, cx));
}

/// The colours of a request file, from its lines.
fn spans_for(path: &Path, text: &str) -> Option<Vec<HighlightSpan>> {
    if !is_request_file(path) {
        return None;
    }
    Some(
        highlight(text)
            .into_iter()
            .map(|span| HighlightSpan {
                start: span.start,
                end: span.end,
                kind: match span.kind {
                    TokenKind::Keyword => HighlightKind::Keyword,
                    TokenKind::String => HighlightKind::String,
                    TokenKind::Field => HighlightKind::Field,
                    TokenKind::Constant => HighlightKind::Constant,
                    TokenKind::Comment => HighlightKind::Comment,
                    TokenKind::Number => HighlightKind::Number,
                },
            })
            .collect(),
    )
}

/// The row above the text of a request file: its environment.
fn file_bar(path: &Path, _window: &mut Window, cx: &mut Context<EditorView>) -> Option<AnyElement> {
    if !is_request_file(path) {
        return None;
    }
    let panel = cx.try_global::<ActiveHttp>()?.0.clone();
    let selected = panel.read_with(cx, |panel, _| panel.selected_for(path));
    let theme = *cx.global::<Theme>();
    let label = selected.unwrap_or_else(|| "No Environment".to_owned());
    let target = path.to_path_buf();
    let click_panel = panel.clone();
    let button = toolbar_dropdown("http-environment", None, label, &theme)
        .debug_selector(|| "http-environment".to_owned())
        .on_click(move |_, window, cx| {
            click_panel.update(cx, |panel, cx| {
                panel.show_environment_menu(target.clone(), window, cx)
            });
        });
    Some(
        div()
            .id("http-file-bar")
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

/// Puts the plain syntax of the request files and the bar above their text in place.
pub(crate) fn install(cx: &mut App) {
    cx.set_global(PlainSyntax {
        resolve: spans_for,
        runs_on_enter: is_request_file,
    });
    cx.set_global(FileBar(Rc::new(
        file_bar as fn(&Path, &mut Window, &mut Context<EditorView>) -> Option<AnyElement>,
    )));
    // A pasted command line becomes the request it describes.
    cx.set_global(PasteText(Rc::new(|path, text| {
        if !is_request_file(path) {
            return None;
        }
        match from_curl(text) {
            Some(Ok(imported)) => Some(imported.text),
            Some(Err(_)) | None => None,
        }
    })));
}

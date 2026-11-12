//! `--script` drives the window with the same calls as the mouse and keyboard, so a
//! scenario runs without taking the real pointer or the focus of the user.
//!
//! Steps are separated by `;`, paths are relative to the project root:
//! `reveal:src/main`, `open:pom.xml`, `key:ctrl-s`, `type:text`, `theme`, `split`,
//! `close-tab`, `next-tab`, `dock:left|right|bottom`, `search`, `dismiss`, `wait:MS`,
//! `report:FILE`, `quit`. `config:NAME` waits for a run configuration and selects it,
//! `show:TOOL` opens a tool window, `task:NAME` runs a task of the build tool, `until:TEXT`
//! waits for the run console to show the text. `goto:LINE` moves the caret of the active
//! editor, `mark:LINE` clicks the run mark on a line, `tests:CLASS[#METHOD]` runs tests,
//! `retry` runs the failed ones again, `select:TEST` shows the failure of a test,
//! `frame` opens the first line of that failure that is in the project, `stop` stops the
//! run and `tests-idle` waits until nothing runs.
//! `docker:ACTION:NAME` waits for an image (`run`) or a container (`start`, `stop`,
//! `restart`, `remove`, `logs`, `exec`) of that name in the Services panel and acts on it,
//! `docker-until:TEXT` waits for the panel to show the text. `wait-ready` waits until the
//! language server reports the ready state, so a scenario needs no guessed timeout.
//! `reopen-tab`, `pin`, `split-down`, `move-group`, `group:next|prev`, `previous-tab`, `save-all`,
//! `format:CRLF|LF|UTF-8...`, `caret:LINE:COLUMN` (from 1, as the status bar shows it) work on the
//! tabs; `new:LABEL:FOLDER:NAME`, `rename:PATH:NEW`, `delete:PATH` do what the project tree does.
//! `problem:PATH:LINE:COLUMN:error|warning:TEXT` gives the Problems window a diagnostic (line and
//! column from 1), `problem-clear:PATH` takes the file's diagnostics away, `todo-scan` reads the
//! project again.
//! `settings` opens the Settings window, `setting:NAME:VALUE` changes one setting
//! (`font_size`, `soft_wrap`, `show_whitespace`, `autosave`, `runtime_home`).
//! `history:PATH` opens Local History of a file or folder, `history-revert` reverts the file to
//! the version picked there, `label:TEXT` puts a label over the project.
//! `agent:claude|codex`, `agent-input:MS`. `agent-input` waits until the
//! active session shows its input box and closes the window when it does not, so a script never
//! types into a login or permission screen. `type` and `key` go to whatever has the focus,
//! the terminal of a dock included.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use agent_core::Harness;
use ui_kit::*;
use workspace::{DockSide, Workspace};

use crate::agents::ActiveAgents;
use crate::build::BuildPanel;
use crate::commit::CommitPanel;
use crate::diff_view::DiffView;
use crate::docker::ActiveDocker;
use crate::git::ActiveGit;
use crate::marks;
use crate::plugins::ActivePlugins;
use crate::problems::ActiveProblems;
use crate::project::ActiveProjectPanel;
use crate::run::RunPanel;
use crate::structure::ActiveStructure;
use crate::terminal_panel::ActiveTerminal;
use crate::todo::ActiveTodo;

/// The run panel of the window, for scripts.
pub struct ActiveRunPanel(pub Entity<RunPanel>);

impl Global for ActiveRunPanel {}

/// The build panel of the window, for scripts.
pub struct ActiveBuildPanel(pub Entity<BuildPanel>);

impl Global for ActiveBuildPanel {}

/// The commit panel of the window, for scripts.
pub struct ActiveCommitPanel(pub Entity<CommitPanel>);

impl Global for ActiveCommitPanel {}

/// A build and a server start take a while; past this a step gives up.
const PATIENCE: Duration = Duration::from_secs(600);
const RECHECK: Duration = Duration::from_millis(250);
/// Console lines a report keeps from the end.
const REPORT_LINES: usize = 400;

const INPUT_POLL: Duration = Duration::from_millis(200);

#[derive(Debug, PartialEq)]
pub enum Step {
    Reveal(PathBuf),
    Open(PathBuf),
    Key(String),
    Type(String),
    Theme,
    Split,
    SplitDown,
    CloseTab,
    ReopenTab,
    NextTab,
    PreviousTab,
    Pin,
    MoveGroup,
    Group(isize),
    SaveAll,
    Format(String),
    Caret(usize, usize),
    New {
        label: String,
        folder: PathBuf,
        name: String,
    },
    Rename(PathBuf, String),
    Delete(PathBuf),
    Problem {
        path: PathBuf,
        problem: crate::problems::Problem,
    },
    ProblemClear(PathBuf),
    TodoScan,
    Settings,
    Setting(String, String),
    History(PathBuf),
    HistoryRevert,
    Label(String),
    Dock(DockSide),
    Search,
    Dismiss,
    Agent(Harness),
    AgentInput(u64),
    Wait(u64),
    WaitReady,
    Config(String),
    Task(String),
    Show(String),
    Message(String),
    Commit(bool),
    Branch(String),
    NewBranch(String),
    Diff(PathBuf),
    Idle,
    Until(String),
    Goto(usize),
    Mark(usize),
    Tests(String),
    Retry,
    SelectTest(String),
    Frame,
    Stop,
    TestsIdle,
    Docker(String, String),
    DockerUntil(String),
    Report(PathBuf),
    Quit,
}

pub fn parse_script(script: &str) -> Result<Vec<Step>, String> {
    let steps = script
        .split(';')
        .map(str::trim)
        .filter(|step| !step.is_empty());
    steps.map(parse_step).collect()
}

fn parse_step(step: &str) -> Result<Step, String> {
    let (name, value) = step.split_once(':').unwrap_or((step, ""));
    Ok(match name {
        "reveal" => Step::Reveal(value.into()),
        "open" => Step::Open(value.into()),
        "key" => Step::Key(value.into()),
        "type" => Step::Type(value.into()),
        "theme" => Step::Theme,
        "split" => Step::Split,
        "split-down" => Step::SplitDown,
        "close-tab" => Step::CloseTab,
        "reopen-tab" => Step::ReopenTab,
        "next-tab" => Step::NextTab,
        "previous-tab" => Step::PreviousTab,
        "pin" => Step::Pin,
        "move-group" => Step::MoveGroup,
        "group" => Step::Group(match value {
            "next" => 1,
            "prev" => -1,
            other => return Err(format!("unknown group {other}")),
        }),
        "save-all" => Step::SaveAll,
        "format" => Step::Format(value.into()),
        "caret" => {
            let (line, column) = value.split_once(':').ok_or("caret needs LINE:COLUMN")?;
            let number = |text: &str| text.parse().map_err(|_| format!("bad number {text}"));
            Step::Caret(number(line)?, number(column)?)
        }
        "new" => {
            let mut parts = value.splitn(3, ':');
            let (label, folder, name) = (parts.next(), parts.next(), parts.next());
            let (Some(label), Some(folder), Some(name)) = (label, folder, name) else {
                return Err("new needs LABEL:FOLDER:NAME".into());
            };
            Step::New {
                label: label.into(),
                folder: folder.into(),
                name: name.into(),
            }
        }
        "rename" => {
            let (path, name) = value.split_once(':').ok_or("rename needs PATH:NEW")?;
            Step::Rename(path.into(), name.into())
        }
        "delete" => Step::Delete(value.into()),
        "todo-scan" => Step::TodoScan,
        "settings" => Step::Settings,
        "setting" => {
            let (name, value) = value.split_once(':').ok_or("setting needs NAME:VALUE")?;
            Step::Setting(name.into(), value.into())
        }
        "problem-clear" => Step::ProblemClear(value.into()),
        "problem" => {
            let mut parts = value.splitn(5, ':');
            let mut next = |what: &str| parts.next().ok_or(format!("problem needs {what}"));
            let path = next("PATH")?;
            let number = |text: &str| {
                text.parse::<usize>()
                    .map_err(|_| format!("bad number {text}"))
            };
            let line = number(next("LINE")?)?;
            let column = number(next("COLUMN")?)?;
            let severity = match next("SEVERITY")? {
                "error" => lang_api::Severity::Error,
                "warning" => lang_api::Severity::Warning,
                other => return Err(format!("unknown severity {other}")),
            };
            let message = next("TEXT")?.to_string();
            Step::Problem {
                path: path.into(),
                problem: crate::problems::Problem {
                    line: line.saturating_sub(1),
                    column: column.saturating_sub(1),
                    severity,
                    message,
                },
            }
        }
        "history" => Step::History(value.into()),
        "history-revert" => Step::HistoryRevert,
        "label" => Step::Label(value.into()),
        "dock" => Step::Dock(match value {
            "left" => DockSide::Left,
            "right" => DockSide::Right,
            "bottom" => DockSide::Bottom,
            other => return Err(format!("unknown dock {other}")),
        }),
        "search" => Step::Search,
        "dismiss" => Step::Dismiss,
        "agent" => {
            let harness = Harness::ALL.iter().find(|harness| harness.stem() == value);
            Step::Agent(*harness.ok_or(format!("unknown harness {value}"))?)
        }
        "wait" => Step::Wait(value.parse().map_err(|_| format!("bad wait {value}"))?),
        "wait-ready" => Step::WaitReady,
        "config" => Step::Config(value.into()),
        "show" => Step::Show(value.into()),
        "message" => Step::Message(value.into()),
        "commit" => Step::Commit(false),
        "commit-push" => Step::Commit(true),
        "branch" => Step::Branch(value.into()),
        "new-branch" => Step::NewBranch(value.into()),
        "diff" => Step::Diff(value.into()),
        "git-idle" => Step::Idle,
        "task" => Step::Task(value.into()),
        "until" => Step::Until(value.into()),
        "docker" => {
            let (action, name) = value.split_once(':').ok_or(format!("bad docker {value}"))?;
            Step::Docker(action.into(), name.into())
        }
        "docker-until" => Step::DockerUntil(value.into()),
        "agent-input" => Step::AgentInput(value.parse().map_err(|_| format!("bad wait {value}"))?),
        "goto" => Step::Goto(value.parse().map_err(|_| format!("bad line {value}"))?),
        "mark" => Step::Mark(value.parse().map_err(|_| format!("bad line {value}"))?),
        "tests" => Step::Tests(value.into()),
        "retry" => Step::Retry,
        "select" => Step::SelectTest(value.into()),
        "frame" => Step::Frame,
        "stop" => Step::Stop,
        "tests-idle" => Step::TestsIdle,
        "report" => Step::Report(value.into()),
        "quit" => Step::Quit,
        other => return Err(format!("unknown step {other}")),
    })
}

/// A text snapshot of the window state, for checks from outside.
fn report(workspace: &Workspace, cx: &App) -> String {
    let theme = cx.global::<Theme>();
    let mut lines = vec![
        format!("theme {}", if theme.dark { "dark" } else { "light" }),
        format!("dock left {}", workspace.dock_open(DockSide::Left)),
        format!("dock right {}", workspace.dock_open(DockSide::Right)),
        format!("dock bottom {}", workspace.dock_open(DockSide::Bottom)),
        format!(
            "tool project {}",
            workspace.tool_open(workspace::PROJECT_TOOL)
        ),
        format!(
            "tool commit {}",
            workspace.tool_open(workspace::COMMIT_TOOL)
        ),
    ];
    if let Some(language) = workspace.language_status() {
        lines.push(format!("language {language}"));
    }
    let active = workspace.active_path();
    let active = active
        .as_deref()
        .map_or("none".to_owned(), |path| relative(workspace.root(), path));
    lines.push(format!("active {active}"));
    let view = workspace.active_view();
    if let Some(view) = view.and_then(|view| view.downcast::<editor_view::EditorView>().ok()) {
        let bytes = view.read(cx).text().map_or(0, |text| text.len());
        lines.push(format!("bytes {bytes}"));
    }
    if let Some(status) = workspace.editor_status(cx) {
        lines.push(format!("caret {} dirty {}", status.caret, status.dirty));
    }
    lines.push(format!(
        "group active {} down {}",
        workspace.active_group(),
        workspace.split_is_down()
    ));
    let named = |paths: Vec<PathBuf>| {
        let names: Vec<String> = paths
            .iter()
            .map(|path| relative(workspace.root(), path))
            .collect();
        names.join(" ")
    };
    let list = |label: &str, panel: &Entity<crate::items::ItemsPanel>, lines: &mut Vec<String>| {
        for item in panel.read(cx).items() {
            lines.push(format!("{label} {} @ {}", item.label, item.hint));
        }
    };
    let problems = cx.global::<ActiveProblems>().0.read(cx);
    list("problem", problems.panel(), &mut lines);
    list("todo", &cx.global::<ActiveTodo>().0, &mut lines);
    let settings = cx.global::<workspace::Settings>();
    lines.push(format!(
        "setting font_size {} autosave {} soft_wrap {} show_whitespace {} runtime_home {}",
        settings.font_size,
        settings.autosave,
        settings.soft_wrap,
        settings.show_whitespace,
        settings.runtime_home
    ));
    lines.push(format!("pinned {}", named(workspace.pinned_tabs())));
    lines.push(format!(
        "closed {}",
        named(workspace.closed_tabs().to_vec())
    ));
    lines.push(format!("recent {}", workspace.recent_projects().len()));
    for (index, tabs) in workspace.open_tabs().iter().enumerate() {
        let tabs = tabs.iter().map(|path| relative(workspace.root(), path));
        lines.push(format!(
            "pane {index} {}",
            tabs.collect::<Vec<_>>().join(" ")
        ));
    }
    let panel = &cx.global::<ActiveProjectPanel>().0;
    for row in panel.read(cx).tree().rows() {
        lines.push(format!("row {} {}", row.depth, row.name));
    }
    let first_line = workspace.active_editor().and_then(|(_, view)| {
        let view = view.downcast::<editor_view::EditorView>().ok()?;
        let text = view.read(cx).text()?;
        text.lines().next().map(str::to_string)
    });
    if let Some(line) = first_line {
        lines.push(format!("editor first {line}"));
    }
    if let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<editor_view::EditorView>().ok())
    {
        let view = view.read(cx);
        lines.push(format!(
            "editor wrap {} whitespace {}",
            view.soft_wrap(),
            view.show_whitespace()
        ));
        let crumbs: Vec<_> = view
            .enclosing()
            .into_iter()
            .map(|item| item.name.as_str())
            .collect();
        lines.push(format!("editor crumbs {}", crumbs.join(" > ")));
        lines.push(format!("editor outline {}", view.outline().len()));
        let sticky: Vec<_> = view.sticky_lines().iter().map(|line| line + 1).collect();
        lines.push(format!("editor sticky {sticky:?}"));
    }
    for mark in editor_view::bookmarks(cx) {
        lines.push(format!(
            "bookmark {} {} {}",
            relative(workspace.root(), &mark.path),
            mark.line + 1,
            mark.mnemonic.map_or("-".to_owned(), String::from)
        ));
    }
    lines.push(format!(
        "tool structure {}",
        workspace.tool_open(workspace::STRUCTURE_TOOL)
    ));
    for row in cx.global::<ActiveStructure>().0.read(cx).lines(cx) {
        lines.push(format!("structure {row}"));
    }
    let git = cx.global::<ActiveGit>().0.read(cx);
    if let Some(branch) = &git.status().branch {
        lines.push(format!("git branch {branch}"));
    }
    lines.push(format!("git busy {}", git.busy().unwrap_or("no")));
    if let Some(error) = git.last_error() {
        lines.push(format!("git error {}", error.replace('\n', " | ")));
    }
    for file in &git.status().files {
        let side = |change: vcs::Change| format!("{change:?}");
        lines.push(format!(
            "git file {} index {} worktree {}",
            file.path,
            side(file.index),
            side(file.worktree)
        ));
    }
    let commit = cx.global::<ActiveCommitPanel>().0.read(cx);
    lines.push(format!(
        "commit message {}",
        commit.message().replace('\n', " | ")
    ));
    let run = cx.global::<ActiveRunPanel>().0.read(cx);
    let state = run.state();
    lines.push(format!("run {} running {}", state.label(), state.running));
    for config in &state.configs {
        lines.push(format!("config {config}"));
    }
    for problem in run.problems() {
        lines.push(format!("problem {}", problem.replace('\n', " | ")));
    }
    lines.extend(run.test_report());
    let view = workspace.active_view();
    if let Some(view) = view.and_then(|view| view.downcast::<editor_view::EditorView>().ok()) {
        for mark in view.read(cx).run_marks() {
            lines.push(format!("mark {} {:?}", mark.line + 1, mark.state));
        }
    }
    let text = run.text();
    let skip = text.len().saturating_sub(REPORT_LINES);
    for line in &text[skip..] {
        lines.push(format!("console {line}"));
    }
    let terminal = cx.global::<ActiveTerminal>().0.read(cx);
    if let Some(session) = terminal.session() {
        let screen = session.screen();
        let shown = screen.lines().into_iter().filter(|line| !line.is_empty());
        lines.extend(shown.map(|line| format!("terminal {line}")));
    }
    lines.extend(cx.global::<ActiveAgents>().0.read(cx).report());
    lines.extend(cx.global::<ActivePlugins>().0.read(cx).report());
    lines.extend(cx.global::<ActiveDocker>().0.read(cx).report());
    lines.extend(crate::history_view::report(cx));
    lines.join("\n") + "\n"
}

fn relative(root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(root).unwrap_or(path);
    relative.display().to_string().replace('\\', "/")
}

fn apply(step: &Step, workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let root = workspace.root().to_path_buf();
    match step {
        Step::Open(path) => workspace.open_file(root.join(path), window, cx),
        Step::History(path) => {
            crate::history_view::HistoryView::show(root.join(path), workspace, window, cx)
        }
        Step::HistoryRevert => crate::history_view::revert_shown(cx),
        Step::Label(text) => crate::history::put_label(&root, text.clone(), cx),
        Step::Theme => workspace.toggle_theme(cx),
        Step::Split => workspace.split_right(window, cx),
        Step::SplitDown => workspace.split_down(window, cx),
        Step::CloseTab => window.dispatch_action(Box::new(workspace::CloseTab), cx),
        Step::ReopenTab => workspace.reopen_closed_tab(window, cx),
        Step::NextTab => workspace.next_tab(window, cx),
        Step::PreviousTab => workspace.previous_tab(window, cx),
        Step::Pin => workspace.pin_active_tab(window, cx),
        Step::MoveGroup => workspace.move_to_other_group(window, cx),
        Step::Group(step) => workspace.focus_group(*step, window, cx),
        Step::SaveAll => workspace.save_all(cx),
        Step::Format(name) => workspace.set_active_format(name, cx),
        Step::Caret(line, column) => {
            let view = workspace
                .active_editor()
                .and_then(|(_, view)| view.downcast::<editor_view::EditorView>().ok());
            if let Some(view) = view {
                let (line, column) = (line.saturating_sub(1), column.saturating_sub(1));
                view.update(cx, |view, cx| view.restore_caret(line, column, cx));
            }
        }
        Step::Dock(side) => workspace.toggle_panel(*side, window, cx),
        Step::Search => workspace.show_search(window, cx),
        Step::Dismiss => workspace.dismiss(window, cx),
        Step::Show(tool) => workspace.show_tool(tool, window, cx),
        Step::Report(file) => {
            if let Err(error) = std::fs::write(file, report(workspace, cx)) {
                eprintln!("report is not written: {error}");
            }
        }
        Step::Goto(line) => {
            let view = workspace.active_view();
            if let Some(view) =
                view.and_then(|view| view.downcast::<editor_view::EditorView>().ok())
            {
                let spot = editor_view::Spot::Line {
                    line: line.saturating_sub(1),
                    column: 0,
                };
                view.update(cx, |view, cx| view.go_to(spot, cx));
            }
        }
        Step::Mark(line) => {
            let view = workspace.active_view();
            let view = view.and_then(|view| view.downcast::<editor_view::EditorView>().ok());
            let request = view.and_then(|view| marks::request_at_line(view.read(cx), line - 1, cx));
            match request {
                Some(request) => marks::run_request(request, workspace, window, cx),
                None => eprintln!("no run mark on line {line}"),
            }
        }
        Step::Tests(spec) => {
            let (suite, case) = match spec.split_once('#') {
                Some((suite, case)) => (suite, Some(case.to_owned())),
                None => (spec.as_str(), None),
            };
            let request = runner::TestRequest {
                suite: Some(suite.to_owned()),
                case,
                ..runner::TestRequest::default()
            };
            marks::run_request(request, workspace, window, cx);
        }
        Step::Retry => marks::rerun_failed(workspace, window, cx),
        Step::SelectTest(name) => {
            let panel = cx.global::<ActiveRunPanel>().0.clone();
            if !panel.update(cx, |panel, cx| panel.select_test(name, cx)) {
                eprintln!("no test {name}");
            }
        }
        Step::Frame => {
            let panel = cx.global::<ActiveRunPanel>().0.clone();
            panel.update(cx, |panel, cx| match panel.first_frame() {
                Some(index) => panel.open_frame(index, window, cx),
                None => eprintln!("the failure has no line in the project"),
            });
        }
        Step::Stop => {
            let panel = cx.global::<ActiveRunPanel>().0.clone();
            panel.update(cx, |panel, cx| panel.stop(cx));
        }
        Step::Quit => workspace.close_window(window, cx),
        Step::TestsIdle => {}
        Step::Reveal(_)
        | Step::Key(_)
        | Step::Type(_)
        | Step::Wait(_)
        | Step::WaitReady
        | Step::Config(_)
        | Step::Task(_)
        | Step::Message(_)
        | Step::Commit(_)
        | Step::Branch(_)
        | Step::NewBranch(_)
        | Step::Diff(_)
        | Step::New { .. }
        | Step::Rename(..)
        | Step::Delete(_)
        | Step::Idle
        | Step::Until(_)
        | Step::Docker(..)
        | Step::DockerUntil(_) => {}
        Step::Problem { path, problem } => {
            let path = root.join(path);
            let hub = cx.global::<ActiveProblems>().0.clone();
            let mut all = hub.read(cx).state().of_file(&path).to_vec();
            all.push(problem.clone());
            hub.update(cx, |hub, cx| hub.set_file_problems(path, all, cx));
        }
        Step::ProblemClear(path) => {
            let path = root.join(path);
            let hub = cx.global::<ActiveProblems>().0.clone();
            hub.update(cx, |hub, cx| hub.set_file_problems(path, Vec::new(), cx));
        }
        Step::Settings => crate::settings::SettingsView::show(workspace, window, cx),
        Step::Setting(name, value) => {
            let (name, value) = (name.clone(), value.clone());
            workspace.update_settings(|settings| apply_setting(settings, &name, &value), cx);
        }
        Step::TodoScan => {
            let panel = cx.global::<ActiveTodo>().0.clone();
            crate::todo::rescan(&panel, root, cx);
        }
        Step::Agent(_) | Step::AgentInput(_) => {}
    }
}

fn typed(character: char) -> Keystroke {
    Keystroke {
        key: character.to_string(),
        key_char: Some(character.to_string()),
        ..Keystroke::default()
    }
}

/// The panel updates the workspace, so this step runs outside of it.
fn reveal(path: &Path, cx: &mut App) {
    let panel = cx.global::<ActiveProjectPanel>().0.clone();
    panel.update(cx, |panel, cx| {
        let path = panel.tree().rows()[0].path.join(path);
        panel.reveal(&path, cx);
    });
}

/// Whether a waiting step is through. The run panel updates the workspace, so this runs
/// outside of it.
/// The steps that talk to the git windows. They run outside of the workspace update because
/// the panels update the workspace themselves.
fn git_step(step: &Step, window: &mut Window, cx: &mut App) {
    let git = cx.global::<ActiveGit>().0.clone();
    let commit = cx.global::<ActiveCommitPanel>().0.clone();
    match step {
        Step::Message(text) => commit.update(cx, |panel, cx| panel.set_message(text, cx)),
        Step::Commit(push) => commit.update(cx, |panel, cx| panel.commit(*push, cx)),
        Step::Branch(name) => git.update(cx, |git, cx| git.switch(name.clone(), cx)),
        Step::NewBranch(name) => git.update(cx, |git, cx| git.create_branch(name.clone(), cx)),
        Step::Diff(relative) => {
            let (workspace, path) = {
                let panel = cx.global::<ActiveProjectPanel>().0.clone();
                let root = panel.read(cx).tree().rows()[0].path.clone();
                (commit.read(cx).workspace(), root.join(relative))
            };
            DiffView::open(git, workspace, path, window, cx);
        }
        _ => {}
    }
}

/// A step sets one named setting; a value that does not fit leaves it as it was.
fn apply_setting(settings: &mut workspace::Settings, name: &str, value: &str) {
    let flag = value == "true" || value == "on";
    match name {
        "font_size" => {
            if let Ok(size) = value.parse::<f32>() {
                settings.font_size = size;
                *settings = settings.with_font_step(0.);
            }
        }
        "soft_wrap" => settings.soft_wrap = flag,
        "show_whitespace" => settings.show_whitespace = flag,
        "autosave" => settings.autosave = flag,
        "runtime_home" => settings.runtime_home = value.into(),
        other => eprintln!("unknown setting {other}"),
    }
}

/// The steps that do what the project tree does. The tree updates the workspace itself.
fn tree_step(step: &Step, window: &mut Window, cx: &mut App) {
    let panel = cx.global::<ActiveProjectPanel>().0.clone();
    panel.update(cx, |panel, cx| match step {
        Step::New {
            label,
            folder,
            name,
        } => panel.script_new(label, folder, name.clone(), window, cx),
        Step::Rename(path, name) => panel.script_rename(path, name.clone(), window, cx),
        Step::Delete(path) => panel.script_delete(path, window, cx),
        _ => {}
    });
}

/// Whether a status line of the language says that its server is ready.
fn language_ready(status: Option<&str>) -> bool {
    // The line reads `Name: state` or `Name: state: error`, and the state is its last
    // word before any error, so an error that mentions the word does not pass for it.
    status.is_some_and(|text| {
        let head = text.splitn(3, ':').take(2).collect::<Vec<_>>().join(":");
        head.split(|character: char| !character.is_alphanumeric())
            .rfind(|word| !word.is_empty())
            .is_some_and(|word| word.eq_ignore_ascii_case("ready"))
    })
}

/// A step that works on the text of the open file. The file is read in the background, so
/// the step waits until the tab shows it: a key sent before that would go nowhere.
fn waits_for_the_text(step: &Step) -> bool {
    matches!(
        step,
        Step::Goto(_)
            | Step::Caret(..)
            | Step::Mark(_)
            | Step::Key(_)
            | Step::Type(_)
            | Step::Format(_)
    )
}

/// Whether the tab in front holds its text, or is a view that takes no text at all.
fn editor_ready(workspace: &Workspace, cx: &App) -> bool {
    workspace
        .active_view()
        .and_then(|view| view.downcast::<editor_view::EditorView>().ok())
        .is_none_or(|view| view.read(cx).is_ready())
}

fn check(step: &Step, cx: &mut App) -> bool {
    let panel = cx.global::<ActiveRunPanel>().0.clone();
    match step {
        Step::Config(name) => panel.update(cx, |panel, cx| panel.select_named(name, cx)),
        Step::Task(name) => {
            let panel = cx.global::<ActiveBuildPanel>().0.clone();
            panel.update(cx, |panel, cx| panel.run_named(name, cx))
        }
        // Git settles when nothing is running and the status was read after the last action.
        Step::Idle => {
            let git = cx.global::<ActiveGit>().0.read(cx);
            git.busy().is_none()
        }
        Step::Until(text) => panel.read(cx).text().iter().any(|line| line.contains(text)),
        Step::TestsIdle => panel.read(cx).idle(),
        Step::Docker(action, name) => {
            let docker = cx.global::<ActiveDocker>().0.clone();
            docker.update(cx, |docker, cx| docker.act_named(action, name, cx))
        }
        Step::DockerUntil(text) => {
            let docker = cx.global::<ActiveDocker>().0.read(cx);
            docker.report().iter().any(|line| line.contains(text))
        }
        _ => true,
    }
}

pub fn start(script: &str, window: &mut Window, cx: &mut Context<Workspace>) {
    let steps = match parse_script(script) {
        Ok(steps) => steps,
        Err(error) => {
            eprintln!("bad script: {error}");
            return;
        }
    };
    cx.spawn_in(window, async move |this, cx| {
        for step in steps {
            // A tab shows its text a moment after it opens: a key or a jump sent before
            // that would go nowhere, and the caret would stay where the file starts.
            if waits_for_the_text(&step) {
                let started = Instant::now();
                loop {
                    let ready = this
                        .update_in(cx, |workspace, _, cx| editor_ready(workspace, cx))
                        .unwrap_or(false);
                    if ready || started.elapsed() > PATIENCE {
                        break;
                    }
                    cx.background_executor().timer(RECHECK).await;
                }
            }
            match &step {
                Step::Wait(ms) => {
                    let delay = Duration::from_millis(*ms);
                    cx.background_executor().timer(delay).await;
                    continue;
                }
                Step::WaitReady => {
                    let started = Instant::now();
                    loop {
                        let ready = this
                            .update_in(cx, |workspace, _, _| {
                                language_ready(
                                    workspace.language_status().map(|text| text.as_ref()),
                                )
                            })
                            .unwrap_or(false);
                        if ready || started.elapsed() > PATIENCE {
                            break;
                        }
                        cx.background_executor().timer(RECHECK).await;
                    }
                    continue;
                }
                Step::Config(_)
                | Step::Task(_)
                | Step::Until(_)
                | Step::Idle
                | Step::TestsIdle
                | Step::Docker(..)
                | Step::DockerUntil(_) => {
                    let started = Instant::now();
                    loop {
                        let Ok(done) = cx.update(|_, cx| check(&step, cx)) else {
                            return;
                        };
                        if done || started.elapsed() > PATIENCE {
                            break;
                        }
                        cx.background_executor().timer(RECHECK).await;
                    }
                    continue;
                }
                Step::Message(_)
                | Step::Commit(_)
                | Step::Branch(_)
                | Step::NewBranch(_)
                | Step::Diff(_) => {
                    let work = cx.update(|window, cx| git_step(&step, window, cx));
                    if work.is_err() {
                        break;
                    }
                    continue;
                }
                Step::New { .. } | Step::Rename(..) | Step::Delete(_) => {
                    let work = cx.update(|window, cx| tree_step(&step, window, cx));
                    if work.is_err() {
                        break;
                    }
                    continue;
                }
                Step::Reveal(path) => {
                    if cx.update(|_, cx| reveal(path, cx)).is_err() {
                        break;
                    }
                    continue;
                }
                Step::AgentInput(ms) => {
                    let deadline = std::time::Instant::now() + Duration::from_millis(*ms);
                    let ready = loop {
                        let ready =
                            cx.update(|_, cx| cx.global::<ActiveAgents>().0.read(cx).input_ready());
                        match ready {
                            Ok(true) => break true,
                            Ok(false) if std::time::Instant::now() < deadline => {}
                            _ => break false,
                        }
                        cx.background_executor().timer(INPUT_POLL).await;
                    };
                    if !ready {
                        eprintln!("the agent shows no input box, the script stops here");
                        this.update_in(cx, |workspace, window, cx| {
                            workspace.close_window(window, cx)
                        })
                        .ok();
                        break;
                    }
                    continue;
                }
                // The panel may open the trust dialog, which updates the workspace.
                Step::Agent(harness) => {
                    let harness = *harness;
                    let started = cx.update(|window, cx| {
                        let panel = cx.global::<ActiveAgents>().0.clone();
                        panel.update(cx, |panel, cx| panel.request(harness, window, cx));
                    });
                    if started.is_err() {
                        break;
                    }
                    continue;
                }
                // Key handlers update the workspace themselves.
                Step::Key(key) => {
                    let parsed = Keystroke::parse_with_os(key, OperatingSystem::Windows);
                    let Ok(stroke) = parsed else {
                        eprintln!("bad key {key}");
                        break;
                    };
                    cx.update(|window, cx| window.dispatch_keystroke(stroke, cx))
                        .ok();
                    continue;
                }
                Step::Type(text) => {
                    for character in text.chars() {
                        let stroke = typed(character);
                        cx.update(|window, cx| window.dispatch_keystroke(stroke, cx))
                            .ok();
                    }
                    continue;
                }
                _ => {}
            }
            let applied = this.update_in(cx, |workspace, window, cx| {
                apply(&step, workspace, window, cx)
            });
            if applied.is_err() {
                break;
            }
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::{DockSide, Harness, Step, parse_script};

    #[test]
    fn parses_every_kind_of_step() {
        let steps = parse_script(
            "reveal:src; open:a b.txt;theme;dock:bottom;wait:50;key:ctrl-s;type:x;config:App;task:package;until:Started;docker:stop:web;docker-until:tick;agent:codex;settings;setting:font_size:15;problem:a.txt:3:7:error:bad thing;problem-clear:a.txt;todo-scan;caret:3:7;group:prev;new:Class:src/a:Foo;rename:a.txt:b.txt;delete:b.txt;format:CRLF;quit;",
        )
        .unwrap();
        assert_eq!(
            steps,
            [
                Step::Reveal("src".into()),
                Step::Open("a b.txt".into()),
                Step::Theme,
                Step::Dock(DockSide::Bottom),
                Step::Wait(50),
                Step::Key("ctrl-s".into()),
                Step::Type("x".into()),
                Step::Config("App".into()),
                Step::Task("package".into()),
                Step::Until("Started".into()),
                Step::Docker("stop".into(), "web".into()),
                Step::DockerUntil("tick".into()),
                Step::Agent(Harness::Codex),
                Step::Settings,
                Step::Setting("font_size".into(), "15".into()),
                Step::Problem {
                    path: "a.txt".into(),
                    problem: crate::problems::Problem {
                        line: 2,
                        column: 6,
                        severity: lang_api::Severity::Error,
                        message: "bad thing".into()
                    }
                },
                Step::ProblemClear("a.txt".into()),
                Step::TodoScan,
                Step::Caret(3, 7),
                Step::Group(-1),
                Step::New {
                    label: "Class".into(),
                    folder: "src/a".into(),
                    name: "Foo".into()
                },
                Step::Rename("a.txt".into(), "b.txt".into()),
                Step::Delete("b.txt".into()),
                Step::Format("CRLF".into()),
                Step::Quit,
            ]
        );
    }

    #[test]
    fn settings_steps_set_what_fits_and_clamp_the_size() {
        let mut settings = workspace::Settings::default();
        super::apply_setting(&mut settings, "font_size", "99");
        assert_eq!(settings.font_size, workspace::MAX_FONT_SIZE);
        super::apply_setting(&mut settings, "font_size", "big");
        assert_eq!(
            settings.font_size,
            workspace::MAX_FONT_SIZE,
            "a bad value changes nothing"
        );
        super::apply_setting(&mut settings, "autosave", "off");
        assert!(!settings.autosave);
        super::apply_setting(&mut settings, "runtime_home", "C:/rt");
        assert_eq!(settings.runtime_home, "C:/rt");
    }

    #[test]
    fn refuses_unknown_steps() {
        assert!(parse_script("fly").is_err());
        assert!(parse_script("wait:soon").is_err());
        assert!(parse_script("dock:top").is_err());
        assert!(parse_script("agent:vim").is_err());
        assert!(parse_script("caret:3").is_err());
        assert!(parse_script("new:Class:src").is_err());
        assert!(parse_script("group:up").is_err());
        assert!(parse_script("setting:font_size").is_err());
        assert!(parse_script("problem:a.txt:1:1:fatal:x").is_err());
        assert!(parse_script("problem:a.txt:1").is_err());
    }
}

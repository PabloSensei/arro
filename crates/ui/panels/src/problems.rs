//! The Problems tool window: errors and warnings of the files, from the diagnostics of the
//! language. Whoever receives diagnostics calls [`ProblemsHub::set_file_problems`]; F2 and
//! Shift+F2 walk through the problems of the file in the editor.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use editor_view::{EditorView, Semantic};
use keymap::editor as actions;
use lang_api::{DiagnosticEntry, ProjectProblem, Severity};
use ui_kit::*;
use workspace::{PROBLEMS_TOOL, Workspace};

use crate::items::{Item, ItemsPanel, open_at};

/// One diagnostic. Line and column count from zero.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Problem {
    pub line: usize,
    pub column: usize,
    pub severity: Severity,
    pub message: String,
}

impl Problem {
    pub fn from_entry(entry: &DiagnosticEntry) -> Self {
        Self {
            line: entry.span.start.line as usize,
            column: entry.span.start.character as usize,
            severity: entry.severity,
            message: entry.message.clone(),
        }
    }

    /// A problem of the project itself, such as a build script that could not be
    /// imported. Its reason is the line the user needs first.
    pub fn from_project(problem: &ProjectProblem) -> Self {
        Self {
            line: problem.line as usize,
            column: 0,
            severity: problem.severity,
            message: problem.summary.clone(),
        }
    }

    /// Errors and warnings are what F2 stops at.
    fn counts(&self) -> bool {
        matches!(self.severity, Severity::Error | Severity::Warning)
    }
}

/// The problems of every file that has some.
#[derive(Default)]
pub struct ProblemsState {
    files: BTreeMap<PathBuf, Vec<Problem>>,
}

impl ProblemsState {
    /// Replaces what is known of a file; an empty list forgets it.
    pub fn set_file(&mut self, path: PathBuf, mut problems: Vec<Problem>) {
        problems.sort_by_key(|problem| (problem.line, problem.column));
        if problems.is_empty() {
            self.files.remove(&path);
        } else {
            self.files.insert(path, problems);
        }
    }

    /// Replaces what is known of a file from the diagnostics of the language server.
    pub fn set_file_diagnostics(&mut self, path: PathBuf, diagnostics: &[DiagnosticEntry]) {
        self.set_file(path, diagnostics.iter().map(Problem::from_entry).collect());
    }

    /// Errors and warnings of all files.
    pub fn totals(&self) -> (usize, usize) {
        let all = self.files.values().flatten();
        let count = |severity| all.clone().filter(|p| p.severity == severity).count();
        (count(Severity::Error), count(Severity::Warning))
    }

    pub fn of_file(&self, path: &Path) -> &[Problem] {
        self.files.get(path).map_or(&[], Vec::as_slice)
    }

    /// The error or warning after (or before) a place of the file, going round at the end.
    pub fn step(&self, path: &Path, line: usize, column: usize, forward: bool) -> Option<&Problem> {
        let mut stops = self.of_file(path).iter().filter(|problem| problem.counts());
        let here = (line, column);
        let place = |problem: &Problem| (problem.line, problem.column);
        if forward {
            let first = stops.clone().next();
            stops.find(|problem| place(problem) > here).or(first)
        } else {
            let last = stops.clone().next_back();
            stops.rfind(|problem| place(problem) < here).or(last)
        }
    }

    /// The list for the window: errors first, then warnings and the rest, each by place.
    pub fn items(&self, root: &Path) -> Vec<Item> {
        let mut entries: Vec<(&PathBuf, &Problem)> = self
            .files
            .iter()
            .flat_map(|(path, problems)| problems.iter().map(move |problem| (path, problem)))
            .collect();
        entries.sort_by_key(|(path, problem)| (problem.severity, (*path).clone(), problem.line));
        entries
            .into_iter()
            .map(|(path, problem)| {
                let relative = path.strip_prefix(root).unwrap_or(path);
                let hint = format!("{}:{}", relative.display(), problem.line + 1);
                let icon = match problem.severity {
                    Severity::Error => "status/error",
                    Severity::Warning => "status/warning",
                    Severity::Information | Severity::Hint => "status/info",
                };
                Item {
                    path: path.clone(),
                    line: problem.line,
                    column: problem.column,
                    label: problem.message.lines().next().unwrap_or("").to_string(),
                    hint: hint.replace('\\', "/"),
                    icon,
                }
            })
            .collect()
    }
}

/// Whether the language of the window can offer a fix at a problem.
fn can_show_fixes(cx: &App) -> bool {
    cx.try_global::<Semantic>()
        .is_some_and(|semantic| semantic.0.actions().is_some())
}

/// Runs `action` once the editor of the file has its text, so a quick fix is asked of a
/// loaded document rather than of an empty one.
fn run_after_load(view: Entity<EditorView>, action: &'static str, cx: &mut App) {
    cx.spawn(async move |cx| {
        for _ in 0..40 {
            let loaded = view.read_with(cx, |view, _| view.text().is_some());
            if loaded {
                break;
            }
            cx.background_executor()
                .timer(Duration::from_millis(25))
                .await;
        }
        view.update(cx, |view, cx| view.run(action, cx));
    })
    .detach();
}

/// The rows of the Problems window for one problem. The panel adds Copy Path.
fn problem_menu(item: &Item, workspace: &WeakEntity<Workspace>, cx: &App) -> Vec<MenuEntry> {
    let jump = {
        let workspace = workspace.clone();
        let path = item.path.clone();
        let (line, column) = (item.line, item.column);
        MenuEntry::new("Jump to Source", move |window, cx| {
            let path = path.clone();
            workspace
                .update(cx, |workspace, cx| {
                    open_at(workspace, path, line, column, window, cx)
                })
                .ok();
        })
        .shortcut("Enter")
    };
    let quick = if can_show_fixes(cx) {
        let workspace = workspace.clone();
        let path = item.path.clone();
        let (line, column) = (item.line, item.column);
        MenuEntry::new("Show Quick Fixes", move |window, cx| {
            let path = path.clone();
            let view = workspace
                .update(cx, |workspace, cx| {
                    open_at(workspace, path, line, column, window, cx);
                    workspace.active_view()
                })
                .ok()
                .flatten();
            if let Some(view) = view.and_then(|view| view.downcast::<EditorView>().ok()) {
                run_after_load(view, actions::SHOW_ACTIONS, cx);
            }
        })
        .shortcut("Alt+Enter")
    } else {
        MenuEntry::disabled("Show Quick Fixes").shortcut("Alt+Enter")
    };
    let message = item.label.clone();
    let copy = MenuEntry::new("Copy Problem Description", move |_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(message.clone()))
    });
    vec![jump, quick, copy]
}

/// Keeps the problems and the window that lists them.
pub struct ProblemsHub {
    state: ProblemsState,
    root: PathBuf,
    panel: Entity<ItemsPanel>,
    /// Files that carry problems of the project itself, replaced as a group.
    project_files: Vec<PathBuf>,
}

/// The hub of the window, for scripts and for whoever receives diagnostics.
pub struct ActiveProblems(pub Entity<ProblemsHub>);

impl Global for ActiveProblems {}

impl ProblemsHub {
    pub fn create(root: PathBuf, workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        let panel = ItemsPanel::create(
            "Problems",
            PROBLEMS_TOOL,
            "No problems found.",
            workspace.clone(),
            cx,
        );
        let weak = workspace;
        panel.update(cx, |panel, _| {
            panel.set_row_menu(Rc::new(move |item, _, cx| problem_menu(item, &weak, cx)));
        });
        cx.new(|_| Self {
            state: ProblemsState::default(),
            root,
            panel,
            project_files: Vec::new(),
        })
    }

    pub fn panel(&self) -> &Entity<ItemsPanel> {
        &self.panel
    }

    pub fn state(&self) -> &ProblemsState {
        &self.state
    }

    /// Takes the latest diagnostics of a file.
    pub fn set_file_problems(
        &mut self,
        path: PathBuf,
        problems: Vec<Problem>,
        cx: &mut Context<Self>,
    ) {
        self.state.set_file(path, problems);
        self.refresh(cx);
    }

    /// Replaces the problems of the project itself, such as a failed build import.
    /// A file that the list no longer names loses its problem.
    pub fn set_project_problems(&mut self, problems: &[ProjectProblem], cx: &mut Context<Self>) {
        for path in std::mem::take(&mut self.project_files) {
            self.state.set_file(path, Vec::new());
        }
        let mut grouped: BTreeMap<PathBuf, Vec<Problem>> = BTreeMap::new();
        for problem in problems {
            let Some(path) = &problem.file else {
                continue;
            };
            grouped
                .entry(path.clone())
                .or_default()
                .push(Problem::from_project(problem));
        }
        for (path, problems) in &grouped {
            self.state.set_file(path.clone(), problems.clone());
        }
        self.project_files = grouped.into_keys().collect();
        self.refresh(cx);
    }

    /// Takes the latest diagnostics of a file from the language server.
    pub fn set_file_diagnostics(
        &mut self,
        path: PathBuf,
        diagnostics: &[DiagnosticEntry],
        cx: &mut Context<Self>,
    ) {
        self.state.set_file_diagnostics(path, diagnostics);
        self.refresh(cx);
    }

    /// Shows the current problems in the window.
    fn refresh(&mut self, cx: &mut Context<Self>) {
        let (errors, warnings) = self.state.totals();
        let items = self.state.items(&self.root);
        let title = format!("Problems ({errors} errors, {warnings} warnings)");
        self.panel.update(cx, |panel, cx| {
            panel.set_title(title, cx);
            panel.set_items(items, cx);
        });
    }
}

/// F2 and Shift+F2 in the editor: the caret goes to the next or the previous problem.
pub fn bind_actions(
    hub: Entity<ProblemsHub>,
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
    cx: &mut App,
) {
    for forward in [true, false] {
        let (hub, workspace) = (hub.clone(), workspace.clone());
        let go = move |cx: &mut App| {
            let (hub, workspace) = (hub.clone(), workspace.clone());
            // The window is being updated while its action runs, so the rest waits.
            cx.defer(move |cx| {
                let jump = move |_: AnyView, window: &mut Window, cx: &mut App| {
                    let Ok(Some((path, view))) =
                        workspace.update(cx, |this, _| this.active_editor())
                    else {
                        return;
                    };
                    let Ok(view) = view.downcast::<editor_view::EditorView>() else {
                        return;
                    };
                    let (line, column) = view.read(cx).caret();
                    let target = hub
                        .read(cx)
                        .state()
                        .step(&path, line, column, forward)
                        .map(|problem| (problem.line, problem.column));
                    if let Some((line, column)) = target {
                        let go = |this: &mut Workspace, cx: &mut Context<Workspace>| {
                            open_at(this, path, line, column, window, cx)
                        };
                        workspace.update(cx, go).ok();
                    }
                };
                window.update(cx, jump).ok();
            });
        };
        if forward {
            cx.on_action(move |_: &workspace::NextProblem, cx| go(cx));
        } else {
            cx.on_action(move |_: &workspace::PreviousProblem, cx| go(cx));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use lang_api::{DiagnosticEntry, Severity, TextPoint, TextSpan};

    use super::{Problem, ProblemsState};

    fn problem(line: usize, column: usize, severity: Severity, message: &str) -> Problem {
        Problem {
            line,
            column,
            severity,
            message: message.into(),
        }
    }

    fn state() -> ProblemsState {
        let mut state = ProblemsState::default();
        state.set_file(
            PathBuf::from("/p/A.txt"),
            vec![
                problem(9, 0, Severity::Warning, "unused"),
                problem(2, 4, Severity::Error, "missing semicolon\nmore"),
                problem(5, 0, Severity::Hint, "style"),
            ],
        );
        state.set_file(
            PathBuf::from("/p/B.txt"),
            vec![problem(1, 1, Severity::Error, "bad")],
        );
        state
    }

    #[test]
    fn totals_count_errors_and_warnings_of_all_files() {
        assert_eq!(state().totals(), (2, 1));
    }

    #[test]
    fn a_file_with_no_problems_is_forgotten() {
        let mut state = state();
        state.set_file(PathBuf::from("/p/B.txt"), Vec::new());
        assert_eq!(state.totals(), (1, 1));
        assert!(state.of_file(Path::new("/p/B.txt")).is_empty());
    }

    #[test]
    fn diagnostics_of_the_editor_reach_the_hub_and_an_empty_list_clears_them() {
        let path = PathBuf::from("/p/A.txt");
        let diagnostic = DiagnosticEntry {
            span: TextSpan {
                start: TextPoint {
                    line: 4,
                    character: 2,
                },
                end: TextPoint {
                    line: 4,
                    character: 9,
                },
            },
            severity: Severity::Error,
            message: "cannot resolve".into(),
            unnecessary: false,
        };
        let mut state = ProblemsState::default();
        state.set_file_diagnostics(path.clone(), std::slice::from_ref(&diagnostic));
        let problems = state.of_file(&path);
        assert_eq!(problems.len(), 1);
        assert_eq!((problems[0].line, problems[0].column), (4, 2));
        assert_eq!(problems[0].severity, Severity::Error);
        assert_eq!(problems[0].message, "cannot resolve");
        assert_eq!(state.items(Path::new("/p")).len(), 1);

        state.set_file_diagnostics(path.clone(), &[]);
        assert!(state.of_file(&path).is_empty());
        assert!(state.items(Path::new("/p")).is_empty());
    }

    #[test]
    fn f2_walks_errors_and_warnings_forward_and_back_and_goes_round() {
        let state = state();
        let a = Path::new("/p/A.txt");
        let at = |line, column, forward| {
            let found = state.step(a, line, column, forward);
            found.map(|problem| (problem.line, problem.column))
        };
        assert_eq!(at(0, 0, true), Some((2, 4)));
        assert_eq!(at(2, 4, true), Some((9, 0)), "the hint is skipped");
        assert_eq!(at(9, 0, true), Some((2, 4)), "round to the first");
        assert_eq!(at(9, 0, false), Some((2, 4)));
        assert_eq!(at(2, 4, false), Some((9, 0)), "round to the last");
        assert_eq!(state.step(Path::new("/p/none"), 0, 0, true), None);
    }

    #[test]
    fn the_list_has_errors_first_with_the_first_line_and_the_place() {
        let items = state().items(Path::new("/p"));
        let labels: Vec<&str> = items.iter().map(|item| item.label.as_str()).collect();
        assert_eq!(labels, ["missing semicolon", "bad", "unused", "style"]);
        assert_eq!(items[0].hint, "A.txt:3");
        assert_eq!(items[1].hint, "B.txt:2");
        assert_eq!(items[1].icon, "status/error");
        assert_eq!(items[2].icon, "status/warning");
    }
}

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
use workspace::{FileIcons, PROBLEMS_TOOL, Workspace};

use crate::items::{Item, ItemsPanel, PanelRow, PanelTab, open_at};

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

    /// A misspelled word of the spelling check, which the summary of the window names
    /// apart from the weak warnings.
    fn is_typo(&self) -> bool {
        self.severity == Severity::Hint
            && self
                .message
                .strip_prefix(lang_api::SPELLING_TITLE)
                .is_some_and(|rest| rest.starts_with(':'))
    }
}

/// The number of problems of each kind, as the summary of the window names them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProblemCounts {
    pub errors: usize,
    pub warnings: usize,
    pub weak_warnings: usize,
    pub infos: usize,
    pub typos: usize,
}

impl ProblemCounts {
    pub fn total(&self) -> usize {
        self.errors + self.warnings + self.weak_warnings + self.infos + self.typos
    }

    /// The title of the window, such as "Problems (2 errors, 3 typos)". A list with
    /// nothing in it says so, rather than counting the kinds that are all zero.
    pub fn title(&self) -> String {
        if self.total() == 0 {
            return "No problems".to_owned();
        }
        let mut parts = Vec::new();
        let mut add = |count: usize, one: &str, many: &str| {
            if count > 0 {
                parts.push(format!("{count} {}", if count == 1 { one } else { many }));
            }
        };
        add(self.errors, "error", "errors");
        add(self.warnings, "warning", "warnings");
        add(self.weak_warnings, "weak warning", "weak warnings");
        add(self.infos, "info", "infos");
        add(self.typos, "typo", "typos");
        format!("Problems ({})", parts.join(", "))
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

    /// The problems of every file counted by kind, for the summary of the window.
    pub fn counts(&self) -> ProblemCounts {
        let mut counts = ProblemCounts::default();
        for problem in self.files.values().flatten() {
            match problem.severity {
                Severity::Error => counts.errors += 1,
                Severity::Warning => counts.warnings += 1,
                Severity::Information => counts.infos += 1,
                Severity::Hint if problem.is_typo() => counts.typos += 1,
                Severity::Hint => counts.weak_warnings += 1,
            }
        }
        counts
    }

    pub fn of_file(&self, path: &Path) -> &[Problem] {
        if let Some(problems) = self.files.get(path) {
            return problems.as_slice();
        }
        // The file of the editor and the file of the diagnostics may come from
        // different spellings of the same place, such as a long Windows path.
        self.files
            .iter()
            .find(|(known, _)| same_path(known, path))
            .map_or(&[], |(_, problems)| problems.as_slice())
    }

    /// Every file that has problems, for the IDE protocol.
    pub fn files(&self) -> impl Iterator<Item = (&PathBuf, &[Problem])> {
        self.files
            .iter()
            .map(|(path, problems)| (path, problems.as_slice()))
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
            .map(|(path, problem)| item_of(root, path, problem))
            .collect()
    }

    /// The current file once, with its problems under it, in the order of the text. A row
    /// of a problem carries the line at its end rather than the path of the file.
    pub fn file_view(
        &self,
        root: &Path,
        path: &Path,
        icons: &FileIcons,
    ) -> (Vec<Item>, Vec<PanelRow>) {
        let problems = self.of_file(path);
        if problems.is_empty() {
            return (Vec::new(), Vec::new());
        }
        let items: Vec<Item> = problems
            .iter()
            .map(|problem| item_in_file(path, problem))
            .collect();
        let relative = path.strip_prefix(root).unwrap_or(path);
        let name = relative.file_name().map_or_else(
            || relative.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let folder = relative
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(|parent| parent.display().to_string().replace('\\', "/"));
        let hint = match folder {
            Some(folder) => format!("{folder} ({})", items.len()),
            None => items.len().to_string(),
        };
        let mut rows = Vec::with_capacity(items.len() + 1);
        rows.push(PanelRow::Group {
            label: name,
            depth: 0,
            icon: icons.icon_for(path),
            hint: Some(hint),
        });
        rows.extend((0..items.len()).map(|index| PanelRow::Item {
            index,
            depth: 1,
            excluded: false,
        }));
        (items, rows)
    }

    /// The list of the whole project, with a row per file and its problems under it.
    pub fn project_view(&self, root: &Path, icons: &FileIcons) -> (Vec<Item>, Vec<PanelRow>) {
        let mut items = Vec::new();
        let mut rows = Vec::new();
        for (path, problems) in &self.files {
            let relative = path.strip_prefix(root).unwrap_or(path);
            rows.push(PanelRow::Group {
                label: relative.display().to_string().replace('\\', "/"),
                depth: 0,
                icon: icons.icon_for(path),
                hint: None,
            });
            for problem in problems {
                let index = items.len();
                rows.push(PanelRow::Item {
                    index,
                    depth: 1,
                    excluded: false,
                });
                items.push(item_of(root, path, problem));
            }
        }
        (items, rows)
    }

    /// Every problem of every file, for the counters of the tabs.
    pub fn total(&self) -> usize {
        self.files.values().map(Vec::len).sum()
    }
}

/// Whether two paths name the same file. Windows knows several spellings of one path:
/// the long form, either slash and either case.
fn same_path(left: &Path, right: &Path) -> bool {
    if left == right {
        return true;
    }
    #[cfg(windows)]
    {
        let normalize = |path: &Path| {
            path.to_string_lossy()
                .replace('/', "\\")
                .trim_start_matches(r"\\?\")
                .to_lowercase()
        };
        normalize(left) == normalize(right)
    }
    #[cfg(not(windows))]
    {
        false
    }
}

/// One row of the list for a problem of `path`.
fn item_of(root: &Path, path: &Path, problem: &Problem) -> Item {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let hint = format!("{}:{}", relative.display(), problem.line + 1);
    Item {
        path: path.to_path_buf(),
        line: problem.line,
        column: problem.column,
        label: problem.message.lines().next().unwrap_or("").to_string(),
        hint: hint.replace('\\', "/"),
        icon: severity_icon(problem.severity),
    }
}

/// One row of the Current File tab: the problem without its path, which the row of the
/// file above already shows, and with the line at the end.
fn item_in_file(path: &Path, problem: &Problem) -> Item {
    Item {
        path: path.to_path_buf(),
        line: problem.line,
        column: problem.column,
        label: problem.message.lines().next().unwrap_or("").to_string(),
        hint: format!(":{}", problem.line + 1),
        icon: severity_icon(problem.severity),
    }
}

/// The icon of the severity of a problem.
fn severity_icon(severity: Severity) -> &'static str {
    match severity {
        Severity::Error => "status/error",
        Severity::Warning => "status/warning",
        Severity::Information | Severity::Hint => "status/info",
    }
}

/// Which problems the window lists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scope {
    /// The problems of the file of the active tab.
    CurrentFile,
    /// Every file of the project, grouped by file.
    Project,
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
        .shortcut(keymap::shortcut_label("enter", keymap::platform()))
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
        .shortcut(keymap::shortcut_label("alt-enter", keymap::platform()))
    } else {
        MenuEntry::disabled("Show Quick Fixes")
            .shortcut(keymap::shortcut_label("alt-enter", keymap::platform()))
    };
    let message = item.label.clone();
    let copy = MenuEntry::new("Copy Problem Description", move |_, cx| {
        cx.write_to_clipboard(ClipboardItem::new_string(message.clone()))
    });
    vec![jump, quick, copy]
}

/// The rows at the end of the menu of a problem: Fix with AI, which sends the problem
/// to the agent with the code around it.
fn problem_ai_menu(item: &Item, workspace: &WeakEntity<Workspace>, cx: &App) -> Vec<MenuEntry> {
    if !crate::fix_with_ai::enabled(cx) {
        return Vec::new();
    }
    let Some(workspace) = workspace.upgrade() else {
        return Vec::new();
    };
    let root = workspace.read(cx).root().to_path_buf();
    let problem = cx.try_global::<ActiveProblems>().and_then(|hub| {
        let hub = hub.0.read(cx);
        hub.state()
            .of_file(&item.path)
            .iter()
            .find(|problem| problem.line == item.line)
            .cloned()
    });
    let kind = match problem.as_ref().map(|problem| problem.severity) {
        Some(Severity::Warning) => "warning",
        Some(_) => "error",
        None if item.icon == "status/warning" => "warning",
        None => "error",
    };
    let message = problem.map_or_else(|| item.label.clone(), |problem| problem.message);
    let text = crate::fix_with_ai::file_text(&item.path, cx);
    let message = crate::fix_with_ai::diagnostic_at(
        &root,
        &item.path,
        item.line,
        kind,
        &message,
        None,
        text.as_deref(),
    );
    vec![
        MenuEntry::separator(),
        MenuEntry::new("Fix with AI", move |window, cx| {
            workspace.update(cx, |workspace, cx| {
                crate::fix_with_ai::ask(workspace, message.clone(), window, cx)
            });
        }),
    ]
}

/// Keeps the problems and the window that lists them.
pub struct ProblemsHub {
    state: ProblemsState,
    root: PathBuf,
    /// The window, for the icons of the file types.
    workspace: WeakEntity<Workspace>,
    panel: Entity<ItemsPanel>,
    /// Files that carry problems of the project itself, replaced as a group.
    project_files: Vec<PathBuf>,
    /// Which problems the window lists.
    scope: Scope,
    /// The file of the active tab, when there is one.
    active_file: Option<PathBuf>,
}

/// The hub of the window, for scripts and for whoever receives diagnostics.
pub struct ActiveProblems(pub Entity<ProblemsHub>);

impl Global for ActiveProblems {}

impl ProblemsHub {
    pub fn create(root: PathBuf, workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        let panel = ItemsPanel::create(
            &ProblemCounts::default().title(),
            PROBLEMS_TOOL,
            "No problems found",
            workspace.clone(),
            cx,
        );
        let weak = workspace.clone();
        panel.update(cx, |panel, _| {
            panel.set_row_menu(Rc::new(move |item, _, cx| {
                let mut entries = problem_menu(item, &weak, cx);
                entries.extend(problem_ai_menu(item, &weak, cx));
                entries
            }));
        });
        cx.new(|_| Self {
            state: ProblemsState::default(),
            root,
            workspace,
            panel,
            project_files: Vec::new(),
            scope: Scope::CurrentFile,
            active_file: None,
        })
    }

    pub fn panel(&self) -> &Entity<ItemsPanel> {
        &self.panel
    }

    pub fn state(&self) -> &ProblemsState {
        &self.state
    }

    /// A tab became active: the problems of Current File follow it.
    pub fn set_active_file(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
        if self.active_file == path {
            return;
        }
        self.active_file = path;
        if self.scope == Scope::CurrentFile {
            self.refresh(cx);
        } else {
            self.refresh_tabs(cx);
        }
    }

    fn set_scope(&mut self, scope: Scope, cx: &mut Context<Self>) {
        if self.scope == scope {
            return;
        }
        self.scope = scope;
        self.refresh(cx);
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
        let files = self
            .state
            .files()
            .map(|(path, problems)| (path.clone(), problems.to_vec()));
        crate::claude_ide::publish_problems(cx, files);
        let title = self.state.counts().title();
        let icons = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).icons().clone())
            .unwrap_or_default();
        let (items, rows, empty) = match self.scope {
            Scope::Project => {
                let (items, rows) = self.state.project_view(&self.root, &icons);
                (items, rows, "No problems found")
            }
            // With no file open there is nothing to scope the tab to, so it lists the
            // project rather than an empty window.
            Scope::CurrentFile => match &self.active_file {
                Some(path) => {
                    let (items, rows) = self.state.file_view(&self.root, path, &icons);
                    (items, rows, "No problems in the current file")
                }
                _ => {
                    let (items, rows) = self.state.project_view(&self.root, &icons);
                    (items, rows, "No problems found")
                }
            },
        };
        let current = self.current_count();
        let total = self.state.total();
        let tabs = self.tabs(current, total, cx);
        self.panel.update(cx, |panel, cx| {
            panel.set_title(title, cx);
            panel.set_empty(empty, cx);
            panel.set_tabs(tabs, cx);
            panel.set_view(items, rows, cx);
        });
    }

    /// Updates the counters of the tabs without building the list again.
    fn refresh_tabs(&mut self, cx: &mut Context<Self>) {
        let (current, total) = (self.current_count(), self.state.total());
        let tabs = self.tabs(current, total, cx);
        self.panel.update(cx, |panel, cx| panel.set_tabs(tabs, cx));
    }

    /// The problems the Current File tab counts.
    fn current_count(&self) -> usize {
        match &self.active_file {
            Some(path) => self.state.of_file(path).len(),
            None => self.state.total(),
        }
    }

    /// The two tabs of the window, each with the number of the problems it lists.
    fn tabs(&self, current: usize, total: usize, cx: &mut Context<Self>) -> Vec<PanelTab> {
        let weak = cx.entity().downgrade();
        let tab = |scope: Scope, id: &'static str, label: String| {
            let weak = weak.clone();
            PanelTab {
                id,
                label,
                active: self.scope == scope,
                run: Rc::new(move |_window, cx| {
                    let weak = weak.clone();
                    // The tab is clicked inside an update of its own panel, so the hub
                    // builds the list after that update ends.
                    cx.defer(move |cx| {
                        weak.update(cx, |hub, cx| hub.set_scope(scope, cx)).ok();
                    });
                }),
            }
        };
        vec![
            tab(
                Scope::CurrentFile,
                "current-file",
                format!("Current File ({current})"),
            ),
            tab(
                Scope::Project,
                "project-errors",
                format!("Project Errors ({total})"),
            ),
        ]
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
            code: None,
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

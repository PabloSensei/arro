//! The tool window of the build tool: the modules of the project, their groups of tasks
//! and the buttons that read the model again, run a line of the user or leave the tests
//! out. A double click or Enter runs a task in the console of the Run window.
//!
//! Under every module the window shows the tree of its dependencies, one scope at a time,
//! and the Dependency Analyzer lists every artifact of the project with the paths that
//! bring it in. A double click on a dependency goes to its declaration in the build file;
//! the analyzer can copy the coordinates of an artifact or add an exclusion to the
//! first-level dependency it comes through.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{EditorView, Semantic, Spot};
use futures_util::StreamExt;
use lang_api::{DependencyVersion, DependencyVersionQuery, ask};
use project_model::{
    BuildGroup, BuildModule, BuildProfile, BuildSelection, BuildSystem, BuildTask, Coordinates,
    Dependency, Exclusion, Module, ProfileChoice, ProjectModel,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{RUN_TOOL, TASKS_TOOL, Workspace};

use crate::prompt::TextField;
use crate::run::RunPanel;

#[path = "dependencies.rs"]
mod dependencies;
pub(crate) use dependencies::ANALYZE_COMMAND;

use dependencies::Artifact;

pub(crate) use dependencies::{declaration_offset, short_name};

const GROUP: &str = "right-panel";
const ROW_MARGIN: f32 = SPACE_L;
/// Lines of the Execute field that stay offered.
const HISTORY: usize = 10;
const POPUP_WIDTH: f32 = 250.;
/// The part of the analyzer the list of artifacts takes.
const LIST_WIDTH: f32 = 260.;
/// How deep the tree of one scope may go before the window stops drawing it.
const MAX_TREE_DEPTH: usize = 32;
/// Why a check box of a profile is shown.
const PROFILE_HINT: &str = "Turn the profile on or off for the build";
/// What the values of a build are edited as.
const PROPERTIES_HINT: &str = "Values for the build, as name=value; ...; Enter saves";

/// The buttons of the window and the hint each of them shows. The tests read the same
/// table the render does.
pub(crate) const TOOLTIPS: &[(&str, &str)] = &[
    ("build-reload", "Reload All Projects"),
    ("build-execute", "Execute Goal/Task"),
    ("build-skip-tests", "Skip Tests"),
    ("build-expand-all", "Expand All"),
    ("build-collapse-all", "Collapse All"),
    ("build-analyze", "Analyze Dependencies"),
    ("build-download-sources", "Download Sources for All"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    /// The node of the project that holds the named settings of its build.
    Profiles,
    /// One named setting of the build, with its check box.
    Profile,
    Module,
    Group,
    Task,
    /// The node of a module that holds its dependencies.
    Dependencies,
    /// One scope of the dependencies: compile, runtime or test.
    Scope,
    /// One edge of a scope tree.
    Dependency,
}

/// A named setting of the build as the tree shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ProfileRow {
    name: String,
    /// The build turns the setting on with the answers of the user.
    active: bool,
    /// The answer of the user: the file, on, or off.
    choice: ProfileChoice,
}

/// A dependency row names the artifact its declaration is searched by.
#[derive(Clone, Debug, PartialEq, Eq)]
struct DependencyRow {
    coordinates: String,
}

/// What an exclusion needs once the editor of the build file holds its text.
struct ExcludeJob {
    tool: Arc<dyn BuildSystem>,
    module: Module,
    root: PathBuf,
    path: PathBuf,
    request: Exclusion,
}

/// One line of the visible tree.
#[derive(Clone, Debug)]
struct Row {
    kind: RowKind,
    depth: usize,
    label: String,
    expandable: bool,
    open: bool,
    module: String,
    /// The name of the module as the tree shows it, for the labels of the menu.
    module_name: String,
    task: Option<String>,
    program: bool,
    /// The tree node that can be closed: a group, the Dependencies node or a scope.
    key: Option<String>,
    /// The grey text after the label, such as the version a conflict dropped.
    hint: Option<String>,
    dependency: Option<DependencyRow>,
    profile: Option<ProfileRow>,
}

pub struct BuildPanel {
    run: Entity<RunPanel>,
    workspace: WeakEntity<Workspace>,
    this: WeakEntity<Self>,
    focus: FocusHandle,
    /// Modules and groups the user closed; everything is open until then.
    closed_modules: HashSet<String>,
    closed_groups: HashSet<String>,
    selected: Option<usize>,
    scroll: ScrollHandle,
    execute_open: bool,
    execute: TextField,
    history: Vec<String>,
    skip_tests: bool,
    /// The analyzer replaced the task tree.
    analyzer: bool,
    search: TextField,
    /// The search field takes the keys: a click in it turns the mode on.
    search_active: bool,
    only_conflicts: bool,
    /// Every artifact of the model, with the paths that bring it in.
    artifacts: Vec<Artifact>,
    /// The model the artifacts were built from.
    analyzed: Option<Arc<ProjectModel>>,
    /// The newest stable version of an artifact that is newer than the one the model
    /// names, by `group:artifact`, as the language found it.
    updates: HashMap<String, String>,
    /// The model generation the updates belong to.
    updates_generation: usize,
    /// The generation the language was asked for.
    updates_asked: usize,
    analyzer_selected: Option<usize>,
    analyzer_list_scroll: ScrollHandle,
    analyzer_details_scroll: ScrollHandle,
    /// Why the last command of the analyzer did nothing.
    message: Option<String>,
    /// The named values of the build, as the window edits them.
    properties: TextField,
    /// The values take the keys: a click in them turns the mode on.
    properties_active: bool,
    _run: Subscription,
}

impl BuildPanel {
    pub fn create(
        run: Entity<RunPanel>,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let properties = TextField::from_text(run.read(cx).build_selection().properties_text());
            let _run = cx.observe(&run, |_, _, cx| cx.notify());
            let this = cx.entity().downgrade();
            Self {
                run,
                workspace,
                this,
                focus: cx.focus_handle(),
                closed_modules: HashSet::new(),
                closed_groups: HashSet::new(),
                selected: None,
                scroll: ScrollHandle::new(),
                execute_open: false,
                execute: TextField::default(),
                history: Vec::new(),
                skip_tests: false,
                analyzer: false,
                search: TextField::default(),
                search_active: false,
                only_conflicts: false,
                artifacts: Vec::new(),
                analyzed: None,
                updates: HashMap::new(),
                updates_generation: 0,
                updates_asked: 0,
                analyzer_selected: None,
                analyzer_list_scroll: ScrollHandle::new(),
                analyzer_details_scroll: ScrollHandle::new(),
                message: None,
                properties,
                properties_active: false,
                _run,
            }
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Runs a task by name, for scripts.
    pub fn run_named(&mut self, task: &str, cx: &mut Context<Self>) -> bool {
        let known = self.run.read(cx).tasks().iter().any(|name| name == task);
        if known {
            self.run.update(cx, |run, cx| run.run_task(task, cx));
        }
        known
    }

    /// The lines of the visible tree, for tests.
    #[cfg(test)]
    pub(crate) fn labels(&self, cx: &App) -> Vec<String> {
        self.rows(cx)
            .into_iter()
            .map(|row| format!("{}{}", "  ".repeat(row.depth), row.label))
            .collect()
    }

    /// The grey marks of the visible tree, next to the row that has them, for tests.
    #[cfg(test)]
    pub(crate) fn hints(&self, cx: &App) -> Vec<(String, String)> {
        self.rows(cx)
            .into_iter()
            .filter_map(|row| row.hint.map(|hint| (row.label, hint)))
            .collect()
    }

    /// Selects a line as a click does, for tests.
    #[cfg(test)]
    pub(crate) fn select_row(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.rows(cx).len() {
            self.selected = Some(index);
            cx.notify();
        }
    }

    /// Runs or toggles a line as a double click or Enter does, for tests.
    #[cfg(test)]
    pub(crate) fn activate_row(
        &mut self,
        index: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.activate(index, window, cx);
    }

    #[cfg(test)]
    pub(crate) fn skip_tests(&self) -> bool {
        self.skip_tests
    }

    #[cfg(test)]
    pub(crate) fn execute_open(&self) -> bool {
        self.execute_open
    }

    /// Opens the Execute popup as the toolbar button does, for tests.
    #[cfg(test)]
    pub(crate) fn open_execute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_execute(window, cx);
    }

    /// Flips Skip Tests as the toolbar button does, for tests.
    #[cfg(test)]
    pub(crate) fn toggle_skip_tests(&mut self, cx: &mut Context<Self>) {
        self.skip_tests = !self.skip_tests;
        cx.notify();
    }

    /// Reloads the model as the toolbar button does, for tests.
    #[cfg(test)]
    pub(crate) fn reload_now(&mut self, cx: &mut Context<Self>) {
        self.reload(cx);
    }

    /// Opens the analyzer as the toolbar button and Find Action do, for tests.
    #[cfg(test)]
    pub(crate) fn show_analyzer_now(&mut self, cx: &mut Context<Self>) {
        self.open_analyzer(cx);
    }

    /// The artifacts the analyzer lists, as the left list shows them, for tests.
    #[cfg(test)]
    pub(crate) fn analyzer_labels(&mut self, _cx: &mut Context<Self>) -> Vec<String> {
        let indices = self.filtered();
        indices
            .into_iter()
            .map(|index| {
                let artifact = &self.artifacts[index];
                artifact_label(artifact)
            })
            .collect()
    }

    /// The lines of the right pane of the analyzer, for tests.
    #[cfg(test)]
    pub(crate) fn analyzer_details(&mut self, _cx: &mut Context<Self>) -> Vec<String> {
        self.details()
    }

    /// The artifacts with a newer stable version, as the analyzer marks them, for tests.
    #[cfg(test)]
    pub(crate) fn analyzer_marks(&mut self) -> Vec<(String, String)> {
        let indices = self.filtered();
        indices
            .into_iter()
            .filter_map(|index| {
                let artifact = &self.artifacts[index];
                let latest = self.updates.get(&artifact.name)?;
                Some((artifact_label(artifact), latest.clone()))
            })
            .collect()
    }

    /// Types into the search field of the analyzer, for tests.
    #[cfg(test)]
    pub(crate) fn search_for(&mut self, text: &str, cx: &mut Context<Self>) {
        self.search = TextField::from_text(text);
        self.search_active = true;
        self.analyzer_selected = None;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn analyzer_open(&self) -> bool {
        self.analyzer
    }

    #[cfg(test)]
    pub(crate) fn only_conflicts(&self) -> bool {
        self.only_conflicts
    }

    #[cfg(test)]
    pub(crate) fn set_only_conflicts(&mut self, on: bool, cx: &mut Context<Self>) {
        self.only_conflicts = on;
        cx.notify();
    }

    #[cfg(test)]
    pub(crate) fn select_artifact(&mut self, index: usize, cx: &mut Context<Self>) {
        let filtered = self.filtered();
        if let Some(&artifact) = filtered.get(index) {
            self.analyzer_selected = Some(artifact);
            cx.notify();
        }
    }

    #[cfg(test)]
    pub(crate) fn message(&self) -> Option<String> {
        self.message.clone()
    }

    /// The named settings of the build as the tree lists them, for tests.
    #[cfg(test)]
    pub(crate) fn profile_rows(&self, cx: &App) -> Vec<(String, bool)> {
        self.rows(cx)
            .into_iter()
            .filter_map(|row| row.profile.map(|profile| (profile.name, profile.active)))
            .collect()
    }

    /// The answers of the user for the named settings, for tests.
    #[cfg(test)]
    pub(crate) fn profile_choices(&self, cx: &App) -> Vec<(String, ProfileChoice)> {
        self.rows(cx)
            .into_iter()
            .filter_map(|row| row.profile.map(|profile| (profile.name, profile.choice)))
            .collect()
    }

    /// The line of one named setting, for tests.
    #[cfg(test)]
    pub(crate) fn profile_row(&self, name: &str, cx: &App) -> Option<usize> {
        self.rows(cx).iter().position(|row| {
            row.profile
                .as_ref()
                .is_some_and(|profile| profile.name == name)
        })
    }

    /// The values of the build as the field shows them, for tests.
    #[cfg(test)]
    pub(crate) fn properties_text(&self) -> String {
        self.properties.text.clone()
    }

    /// Edits the values and saves them as Enter does, for tests.
    #[cfg(test)]
    pub(crate) fn set_properties(&mut self, text: &str, cx: &mut Context<Self>) {
        self.properties = TextField::from_text(text);
        self.commit_properties(cx);
    }

    /// The model the panel shows, for tests.
    fn model(&self, cx: &App) -> Option<Arc<ProjectModel>> {
        self.run.read(cx).model()
    }

    fn rows(&self, cx: &App) -> Vec<Row> {
        let tree = self.run.read(cx).build_tree();
        let model = self.model(cx);
        let mut rows = Vec::new();
        let profiles: &[BuildProfile] = model
            .as_deref()
            .map_or(&[], |model| model.profiles.as_slice());
        if !profiles.is_empty() {
            let selection = self.run.read(cx).build_selection().clone();
            let key = profiles_key();
            let open = !self.closed_groups.contains(&key);
            rows.push(profiles_row(open));
            if open {
                for profile in profiles {
                    rows.push(profile_row(profile, &selection));
                }
            }
        }
        for module in &tree {
            let found = model
                .as_deref()
                .and_then(|model| model.dependencies_of(&module.id));
            let has_dependencies = found.is_some_and(|found| !found.scopes.is_empty());
            rows.push(module_row(module, has_dependencies, &self.closed_modules));
            if self.closed_modules.contains(&module.id) {
                continue;
            }
            for group in &module.groups {
                let key = group_key(&module.id, &group.name);
                let open = !self.closed_groups.contains(&key);
                rows.push(group_row(module, group, open));
                if !open {
                    continue;
                }
                for task in &group.tasks {
                    rows.push(task_row(module, task));
                }
            }
            let Some(found) = found.filter(|_| has_dependencies) else {
                continue;
            };
            let key = dependencies_key(&module.id);
            let open = !self.closed_groups.contains(&key);
            rows.push(dependencies_row(module, open));
            if !open {
                continue;
            }
            for scope in &found.scopes {
                let key = scope_key(&module.id, &scope.name);
                let open = !self.closed_groups.contains(&key);
                rows.push(scope_row(module, &scope.name, open));
                if !open {
                    continue;
                }
                for root in &scope.roots {
                    let mut chain = Vec::new();
                    push_dependency_rows(
                        module,
                        &scope.name,
                        root,
                        &mut chain,
                        3,
                        &self.closed_groups,
                        &mut rows,
                    );
                }
            }
        }
        rows
    }

    fn activate(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows(cx).into_iter().nth(index) else {
            return;
        };
        self.selected = Some(index);
        match row.kind {
            RowKind::Task => self.run_row(&row, window, cx),
            RowKind::Profile => {
                if let Some(profile) = &row.profile {
                    let name = profile.name.clone();
                    self.run.update(cx, |run, cx| run.cycle_profile(&name, cx));
                }
            }
            RowKind::Module => {
                toggle(&mut self.closed_modules, &row.module);
                cx.notify();
            }
            RowKind::Profiles | RowKind::Group | RowKind::Dependencies | RowKind::Scope => {
                if let Some(key) = &row.key {
                    toggle(&mut self.closed_groups, key);
                }
                cx.notify();
            }
            RowKind::Dependency => self.go_to_dependency(&row, window, cx),
        }
    }

    fn run_row(&mut self, row: &Row, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(task), module) = (row.task.as_deref(), row.module.clone()) else {
            return;
        };
        let skip_tests = self.skip_tests;
        self.run.update(cx, |run, cx| {
            run.run_task_in(Some(&module), task, skip_tests, cx)
        });
        self.show_run(window, cx);
    }

    fn show_run(&self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        cx.defer_in(window, move |_, window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.show_tool(RUN_TOOL, window, cx)
                })
                .ok();
        });
    }

    fn click(
        &mut self,
        index: usize,
        event: &ClickEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        window.focus(&self.focus, cx);
        self.properties_active = false;
        let profile = self
            .rows(cx)
            .into_iter()
            .nth(index)
            .is_some_and(|row| row.kind == RowKind::Profile);
        if profile {
            // A check box answers on one click; a task needs a double click.
            self.activate(index, window, cx);
            return;
        }
        if event.click_count() >= 2 {
            let second = self
                .rows(cx)
                .into_iter()
                .nth(index)
                .is_some_and(|row| matches!(row.kind, RowKind::Task | RowKind::Dependency));
            if second {
                self.activate(index, window, cx);
                return;
            }
        }
        self.selected = Some(index);
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.execute_open {
            match event.keystroke.key.as_str() {
                "escape" => {
                    self.execute_open = false;
                    cx.notify();
                }
                "enter" => {
                    let line = self.execute.text.trim().to_owned();
                    if !line.is_empty() {
                        self.submit(line, window, cx);
                    }
                }
                _ => {
                    if self.execute.key_with_clipboard(event, false, cx) {
                        cx.notify();
                    }
                }
            }
            cx.stop_propagation();
            return;
        }
        if self.properties_active {
            match event.keystroke.key.as_str() {
                "escape" => {
                    self.properties_active = false;
                    self.reset_properties(cx);
                }
                "enter" => self.commit_properties(cx),
                _ => {
                    if self.properties.key_with_clipboard(event, false, cx) {
                        cx.notify();
                    }
                }
            }
            cx.stop_propagation();
            return;
        }
        if self.analyzer {
            match event.keystroke.key.as_str() {
                "escape" => {
                    self.search_active = false;
                    cx.stop_propagation();
                    cx.notify();
                    return;
                }
                "up" | "down" => {
                    cx.stop_propagation();
                    self.move_analyzer_selection(event.keystroke.key == "down", cx);
                    return;
                }
                _ => {}
            }
            if self.search_active {
                if self.search.key_with_clipboard(event, false, cx) {
                    self.analyzer_selected = None;
                    cx.notify();
                }
                cx.stop_propagation();
                return;
            }
        }
        match event.keystroke.key.as_str() {
            "up" => {
                cx.stop_propagation();
                self.move_selection(-1, cx);
            }
            "down" => {
                cx.stop_propagation();
                self.move_selection(1, cx);
            }
            "left" => {
                cx.stop_propagation();
                self.collapse_or_parent(cx);
            }
            "right" => {
                cx.stop_propagation();
                self.expand_or_child(cx);
            }
            "enter" => {
                cx.stop_propagation();
                if let Some(index) = self.selected {
                    self.activate(index, window, cx);
                }
            }
            _ => {}
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.rows(cx).len();
        if count == 0 {
            return;
        }
        let last = count as isize - 1;
        let next = match self.selected {
            Some(index) => (index as isize + delta).clamp(0, last),
            None if delta < 0 => last,
            None => 0,
        };
        self.selected = Some(next as usize);
        cx.notify();
    }

    fn move_analyzer_selection(&mut self, forward: bool, cx: &mut Context<Self>) {
        let filtered = self.filtered();
        if filtered.is_empty() {
            return;
        }
        let at = self
            .analyzer_selected
            .and_then(|selected| filtered.iter().position(|index| *index == selected));
        let next = match (at, forward) {
            (Some(at), true) => (at + 1).min(filtered.len() - 1),
            (Some(at), false) => at.saturating_sub(1),
            (None, true) => 0,
            (None, false) => filtered.len() - 1,
        };
        self.analyzer_selected = Some(filtered[next]);
        cx.notify();
    }

    fn collapse_or_parent(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let rows = self.rows(cx);
        let Some(row) = rows.get(index).cloned() else {
            return;
        };
        if row.expandable && row.open {
            match row.kind {
                RowKind::Module => {
                    self.closed_modules.insert(row.module);
                }
                _ => {
                    if let Some(key) = &row.key {
                        self.closed_groups.insert(key.clone());
                    }
                }
            }
            cx.notify();
            return;
        }
        if let Some(parent) = rows[..index]
            .iter()
            .rposition(|candidate| candidate.depth < row.depth)
        {
            self.selected = Some(parent);
            cx.notify();
        }
    }

    fn expand_or_child(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected else {
            return;
        };
        let rows = self.rows(cx);
        let Some(row) = rows.get(index).cloned() else {
            return;
        };
        if row.expandable && !row.open {
            match row.kind {
                RowKind::Module => {
                    self.closed_modules.remove(&row.module);
                }
                _ => {
                    if let Some(key) = &row.key {
                        self.closed_groups.remove(key);
                    }
                }
            }
            cx.notify();
            return;
        }
        if rows
            .get(index + 1)
            .is_some_and(|child| child.depth > row.depth)
        {
            self.selected = Some(index + 1);
            cx.notify();
        }
    }

    fn toggle_execute(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.execute_open = !self.execute_open;
        if self.execute_open {
            self.execute = TextField::default();
        }
        cx.notify();
    }

    fn submit(&mut self, line: String, window: &mut Window, cx: &mut Context<Self>) {
        self.execute_open = false;
        if self.history.last() != Some(&line) {
            self.history.push(line.clone());
            if self.history.len() > HISTORY {
                self.history.remove(0);
            }
        }
        self.run.update(cx, |run, cx| run.run_goal(&line, cx));
        self.show_run(window, cx);
    }

    /// Saves the values of the build as Enter does and shows them as the build tool kept
    /// them.
    fn commit_properties(&mut self, cx: &mut Context<Self>) {
        let text = self.properties.text.clone();
        self.properties_active = false;
        self.run
            .update(cx, |run, cx| run.set_build_properties(&text, cx));
        self.reset_properties(cx);
    }

    /// Puts the values of the build back as they are kept.
    fn reset_properties(&mut self, cx: &mut Context<Self>) {
        let text = self.run.read(cx).build_selection().properties_text();
        self.properties = TextField::from_text(&text);
        cx.notify();
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        self.run.update(cx, |run, cx| run.reload_project(cx));
        if let Some(semantic) = cx.try_global::<Semantic>()
            && let Some(server) = semantic.0.server()
        {
            server.reimport();
        }
        cx.notify();
    }

    fn expand_all(&mut self, cx: &mut Context<Self>) {
        self.closed_modules.clear();
        self.closed_groups.clear();
        cx.notify();
    }

    fn collapse_all(&mut self, cx: &mut Context<Self>) {
        let model = self.model(cx);
        if model
            .as_deref()
            .is_some_and(|model| !model.profiles.is_empty())
        {
            self.closed_groups.insert(profiles_key());
        }
        for module in self.run.read(cx).build_tree() {
            for group in &module.groups {
                self.closed_groups
                    .insert(group_key(&module.id, &group.name));
            }
            if model
                .as_deref()
                .and_then(|model| model.dependencies_of(&module.id))
                .is_some_and(|found| !found.scopes.is_empty())
            {
                self.closed_groups.insert(dependencies_key(&module.id));
            }
            self.closed_modules.insert(module.id);
        }
        cx.notify();
    }

    pub(crate) fn menu_entries(&self, index: usize, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let Some(row) = self.rows(cx).into_iter().nth(index) else {
            return Vec::new();
        };
        let (Some(task), module) = (row.task.clone(), row.module.clone()) else {
            return Vec::new();
        };
        let run = self.run.clone();
        let workspace = self.workspace.clone();
        let (run_task, run_module) = (task.clone(), module.clone());
        let skip_tests = self.skip_tests;
        let mut entries = vec![
            MenuEntry::new("Run", move |window, cx| {
                run.update(cx, |run, cx| {
                    run.run_task_in(Some(&run_module), &run_task, skip_tests, cx)
                });
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.show_tool(RUN_TOOL, window, cx)
                    })
                    .ok();
            })
            .icon("run/run"),
        ];
        if row.program {
            let (debug_task, debug_module) = (task.clone(), module.clone());
            let debug_run = self.run.clone();
            entries.push(
                MenuEntry::new("Debug", move |window, cx| {
                    debug_run.update(cx, |run, cx| {
                        run.debug_task_in(Some(&debug_module), &debug_task, window, cx)
                    });
                })
                .icon("run/debug"),
            );
        }
        let create_run = self.run.clone();
        let create_workspace = self.workspace.clone();
        let create_module = module;
        let create_name = row.module_name;
        let create_task = task;
        entries.push(
            MenuEntry::new(
                format!("Create Run Configuration '{create_name} [{create_task}]'"),
                move |window, cx| {
                    let panel = create_run.clone();
                    let created = panel.update(cx, |run, cx| {
                        run.create_task_configuration(&create_module, &create_task, cx)
                    });
                    if !created {
                        return;
                    }
                    create_workspace
                        .update(cx, |workspace, cx| {
                            crate::run_configs::RunConfigDialog::show(panel, workspace, window, cx)
                        })
                        .ok();
                },
            )
            .icon("run/run"),
        );
        entries
    }

    pub(crate) fn show_menu(
        &mut self,
        index: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let entries = self.menu_entries(index, cx);
        if entries.is_empty() {
            return;
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    /// Opens the analyzer in place of the task tree.
    pub(crate) fn open_analyzer(&mut self, cx: &mut Context<Self>) {
        self.analyzer = true;
        self.search = TextField::default();
        self.search_active = true;
        self.message = None;
        self.refresh_artifacts(cx);
        cx.notify();
    }

    fn close_analyzer(&mut self, cx: &mut Context<Self>) {
        self.analyzer = false;
        self.search_active = false;
        self.message = None;
        cx.notify();
    }

    fn toggle_analyzer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        if self.analyzer {
            self.close_analyzer(cx);
        } else {
            self.open_analyzer(cx);
        }
    }

    /// Rebuilds the artifact list when the model changed.
    fn refresh_artifacts(&mut self, cx: &App) {
        let model = self.model(cx);
        let same = match (&self.analyzed, &model) {
            (Some(cached), Some(current)) => Arc::ptr_eq(cached, current),
            (None, None) => true,
            _ => false,
        };
        if same {
            return;
        }
        self.artifacts = model
            .as_deref()
            .map(dependencies::analyze)
            .unwrap_or_default();
        self.analyzed = model;
        self.analyzer_selected = None;
        self.updates.clear();
        self.updates_generation += 1;
    }

    /// Asks the language for the newest stable version of the direct artifacts once per
    /// model. The answers come from the cache first and from the repositories later, so
    /// the window draws the list at once and adds a mark when one arrives.
    fn request_updates(&mut self, cx: &mut Context<Self>) {
        if self.updates_asked == self.updates_generation || self.artifacts.is_empty() {
            return;
        }
        self.updates_asked = self.updates_generation;
        let dependencies: Vec<(String, String)> = self
            .artifacts
            .iter()
            .filter(|artifact| artifact.direct() && !artifact.selected.is_empty())
            .map(|artifact| (artifact.name.clone(), artifact.selected.clone()))
            .collect();
        if dependencies.is_empty() {
            return;
        }
        let Some(semantic) = cx.try_global::<Semantic>() else {
            return;
        };
        let support = semantic.0.clone();
        let Some(provider) = support.dependency_updates() else {
            return;
        };
        let replies = ask(provider, DependencyVersionQuery { dependencies });
        let generation = self.updates_generation;
        cx.spawn(async move |this, cx| {
            let _support = support;
            let mut replies = replies;
            while let Some(answer) = replies.next().await {
                let versions: Vec<DependencyVersion> = answer.value;
                this.update(cx, |panel, cx| {
                    if panel.updates_generation != generation {
                        return;
                    }
                    panel.updates = versions
                        .into_iter()
                        .filter_map(|version| {
                            version.latest.map(|latest| (version.name.clone(), latest))
                        })
                        .collect();
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    /// The indices of the artifacts the filters keep.
    fn filtered(&mut self) -> Vec<usize> {
        let query = self.search.text.trim().to_lowercase();
        self.artifacts
            .iter()
            .enumerate()
            .filter(|(_, artifact)| !self.only_conflicts || artifact.conflict())
            .filter(|(_, artifact)| {
                query.is_empty() || artifact.name.to_lowercase().contains(&query)
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn selected_artifact(&self) -> Option<&Artifact> {
        self.analyzer_selected
            .and_then(|index| self.artifacts.get(index))
    }

    /// The lines of the right pane: every path of the selected artifact and the versions
    /// that lost.
    fn details(&self) -> Vec<String> {
        let Some(artifact) = self.selected_artifact() else {
            return Vec::new();
        };
        let mut details = vec![artifact.coordinates()];
        let losing: Vec<&String> = artifact.losing().collect();
        if !losing.is_empty() {
            let losing: Vec<&str> = losing.iter().map(|version| version.as_str()).collect();
            details.push(format!("Omitted for conflict: {}", losing.join(", ")));
        }
        for usage in &artifact.usages {
            details.push(usage.path());
            if let Some(mark) = usage.mark() {
                details.push(format!("    {mark}"));
            }
            details.push(format!("    {} [{}]", usage.module_name, usage.scope));
        }
        details
    }

    /// A double click or Enter on a dependency: its declaration in the build file of the
    /// module, when that file is the one that declares it.
    fn go_to_dependency(&mut self, row: &Row, window: &mut Window, cx: &mut Context<Self>) {
        let Some(dependency) = &row.dependency else {
            return;
        };
        let Some(found) = self
            .model(cx)
            .as_deref()
            .and_then(|model| model.dependencies_of(&row.module))
            .cloned()
        else {
            return;
        };
        let Some(path) = found.build_file.clone() else {
            self.message = Some("The module does not name the file it is declared in".into());
            cx.notify();
            return;
        };
        let coordinates = dependency.coordinates.clone();
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        // The editor reads the file in the background: the caret moves once its text is
        // there. The window is not held meanwhile.
        cx.spawn(async move |this, cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.open_file(path.clone(), window, cx)
                        })
                        .ok();
                })
                .ok();
            for _ in 0..200 {
                let done = this
                    .update(cx, |panel, cx| {
                        panel.show_declaration(&path, &coordinates, cx)
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
        })
        .detach();
    }

    /// One attempt to put the caret on the declaration: `false` while the editor of the
    /// build file still has no text.
    fn show_declaration(&mut self, path: &Path, coordinates: &str, cx: &mut Context<Self>) -> bool {
        let editor = self
            .workspace
            .read_with(cx, |workspace, _| workspace.view_of(path))
            .ok()
            .flatten()
            .and_then(|view| view.downcast::<EditorView>().ok());
        let Some(editor) = editor else {
            return false;
        };
        let Some(text) = editor.read(cx).text() else {
            return false;
        };
        if let Some(offset) = dependencies::declaration_offset(&text, coordinates) {
            editor.update(cx, |editor, cx| editor.go_to(Spot::Offset(offset), cx));
        }
        true
    }

    /// Go to Declaration of the analyzer: the build file of the first path of the
    /// selected artifact.
    pub(crate) fn go_to_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(artifact) = self.selected_artifact() else {
            return;
        };
        let Some(usage) = artifact.usages.first() else {
            return;
        };
        let row = Row {
            kind: RowKind::Dependency,
            depth: 0,
            label: String::new(),
            expandable: false,
            open: false,
            module: usage.module.clone(),
            module_name: usage.module_name.clone(),
            task: None,
            program: false,
            key: None,
            hint: None,
            dependency: Some(DependencyRow {
                coordinates: artifact.coordinates(),
            }),
            profile: None,
        };
        self.go_to_dependency(&row, window, cx);
    }

    /// Copy Coordinates of the analyzer.
    pub(crate) fn copy_selected(&mut self, cx: &mut Context<Self>) {
        let Some(artifact) = self.selected_artifact() else {
            return;
        };
        let text = artifact.coordinates();
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }

    /// Exclude of the analyzer: an exclusion in the first-level dependency the selected
    /// path comes through, made in the editor of the build file and saved as a user edit.
    pub(crate) fn exclude_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.message = None;
        let Some(artifact) = self.selected_artifact().cloned() else {
            return;
        };
        // A first-level dependency has nothing to be excluded from.
        let artifact_name = artifact.name.clone();
        let usage = artifact.usages.iter().find(|usage| {
            usage
                .declared()
                .and_then(Coordinates::parse)
                .map(|declared| declared.artifact_name())
                .is_some_and(|name| name != artifact_name)
        });
        let Some(usage) = usage.cloned() else {
            self.message = Some(
                "The artifact is a first-level dependency, so there is nothing to exclude it from"
                    .into(),
            );
            cx.notify();
            return;
        };
        let Some(declared) = usage.declared().map(str::to_owned) else {
            return;
        };
        let Some(coordinates) = Coordinates::parse(&artifact.coordinates()) else {
            return;
        };
        let Some(module) = self
            .model(cx)
            .as_deref()
            .and_then(|model| model.module(&usage.module))
            .cloned()
        else {
            return;
        };
        let Some(found) = self
            .model(cx)
            .as_deref()
            .and_then(|model| model.dependencies_of(&usage.module))
            .cloned()
        else {
            return;
        };
        let Some(tool) = self.run.read(cx).build_system() else {
            self.message = Some("The project has no build tool".into());
            cx.notify();
            return;
        };
        let Some(path) = found.build_file.clone() else {
            self.message = Some("The module does not name the file it is declared in".into());
            cx.notify();
            return;
        };
        let request = Exclusion {
            group: coordinates.group,
            artifact: coordinates.artifact,
            declared,
        };
        let job = ExcludeJob {
            tool,
            module,
            root: self
                .model(cx)
                .map_or_else(PathBuf::new, |model| model.root.clone()),
            path,
            request,
        };
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        // The editor of the build file reads the file in the background: the edit waits
        // for its text instead of holding the window.
        cx.spawn(async move |this, cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.open_file(job.path.clone(), window, cx)
                        })
                        .ok();
                })
                .ok();
            for _ in 0..200 {
                let done = this
                    .update(cx, |panel, cx| panel.apply_exclusion(&job, cx))
                    .unwrap_or(true);
                if done {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(10))
                    .await;
            }
        })
        .detach();
    }

    /// One attempt of an exclusion: `false` while the editor of the build file still has
    /// no text, `true` when the edit was made or refused.
    fn apply_exclusion(&mut self, job: &ExcludeJob, cx: &mut Context<Self>) -> bool {
        let editor = self
            .workspace
            .read_with(cx, |workspace, _| workspace.view_of(&job.path))
            .ok()
            .flatten()
            .and_then(|view| view.downcast::<EditorView>().ok());
        let Some(editor) = editor else {
            return false;
        };
        let Some(text) = editor.read(cx).text() else {
            return false;
        };
        let edit = match job
            .tool
            .exclude(&job.root, &job.module, &job.path, &job.request, &text)
        {
            Ok(Some(edit)) => edit,
            Ok(None) => {
                self.message = Some(format!(
                    "{} does not declare {} with a form this window can change",
                    job.path.file_name().unwrap_or_default().to_string_lossy(),
                    job.request.declared
                ));
                cx.notify();
                return true;
            }
            Err(error) => {
                self.message = Some(error.to_string());
                cx.notify();
                return true;
            }
        };
        editor.update(cx, |editor, cx| {
            editor.apply_edits(
                vec![text_core::Edit::new(edit.start..edit.end, edit.text)],
                cx,
            )
        });
        self.workspace
            .update(cx, |workspace, cx| workspace.save_editor(&job.path, cx))
            .ok();
        self.run.update(cx, |run, cx| run.model_file_changed(cx));
        true
    }

    fn show_analyzer_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        if self.selected_artifact().is_none() {
            return;
        }
        let this = self.this.clone();
        let go_to = this.clone();
        let copy = this.clone();
        let exclude = this;
        let entries = vec![
            MenuEntry::new("Go to Declaration", move |window, cx| {
                go_to
                    .update(cx, |panel, cx| panel.go_to_selected(window, cx))
                    .ok();
            })
            .icon("general/locate"),
            MenuEntry::new("Copy Coordinates", move |_, cx| {
                copy.update(cx, |panel, cx| panel.copy_selected(cx)).ok();
            })
            .icon("general/copy"),
            MenuEntry::new("Exclude", move |window, cx| {
                exclude
                    .update(cx, |panel, cx| panel.exclude_selected(window, cx))
                    .ok();
            })
            .icon("general/minus"),
        ];
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn toolbar(
        &self,
        theme: &Theme,
        loading: bool,
        skip_supported: bool,
        cx: &mut Context<Self>,
    ) -> Div {
        let reload = tool_button("build-reload", "general/refresh", 0, theme);
        let reload = if loading {
            reload.opacity(0.4)
        } else {
            reload.on_click(cx.listener(|this, _, _, cx| this.reload(cx)))
        };
        let mut bar = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_XS))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .child(reload)
            .child(
                tool_button("build-execute", "general/menu", 1, theme)
                    .on_click(cx.listener(|this, _, window, cx| this.toggle_execute(window, cx))),
            );
        if skip_supported {
            bar = bar.child(
                skip_button(self.skip_tests, theme)
                    .debug_selector(|| "build-skip-tests".to_owned())
                    .tooltip(move |_, cx| tooltip(TOOLTIPS[2].1, None, cx))
                    .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.skip_tests = !this.skip_tests;
                        cx.notify();
                    })),
            );
        }
        bar.child(
            tool_button("build-expand-all", "general/expandAll", 3, theme)
                .on_click(cx.listener(|this, _, _, cx| this.expand_all(cx))),
        )
        .child(
            tool_button("build-collapse-all", "general/collapseAll", 4, theme)
                .on_click(cx.listener(|this, _, _, cx| this.collapse_all(cx))),
        )
        .child(
            tool_button("build-analyze", "nodes/library", 5, theme)
                .when(self.analyzer, |this| this.bg(theme.pressed))
                .on_click(cx.listener(|this, _, window, cx| this.toggle_analyzer(window, cx))),
        )
        .child(
            tool_button("build-download-sources", "general/download", 6, theme).on_click(
                cx.listener(|_, _, window, cx| {
                    crate::library_sources::download_all(window, cx);
                }),
            ),
        )
    }

    /// The row of the named values of the build: a field edited as `name=value; ...`, and
    /// Enter keeps it for the commands that follow.
    fn properties_bar(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let empty = self.properties.text.is_empty();
        let color = if empty {
            theme.text_secondary
        } else {
            theme.text
        };
        let active = self.properties_active;
        div()
            .id("build-properties")
            .debug_selector(|| "build-properties".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .px(px(ROW_MARGIN))
            .tooltip(move |_, cx| tooltip(PROPERTIES_HINT, None, cx))
            .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
            .child(
                div()
                    .flex_none()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child("Properties"),
            )
            .child(
                div()
                    .id("build-properties-field")
                    .debug_selector(|| "build-properties-field".to_owned())
                    .flex()
                    .items_center()
                    .flex_1()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(if active { theme.accent } else { theme.border })
                    .text_color(color)
                    .children(text_field_children_named(
                        "build-properties-text",
                        &self.properties,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        16.,
                        active,
                    ))
                    .when(empty, |this| this.child("name=value; ...")),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.properties_active = true;
                cx.notify();
            }))
            .into_any_element()
    }

    fn execute_popup(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let empty = self.execute.text.is_empty();
        let color = if empty {
            theme.text_secondary
        } else {
            theme.text
        };
        let field = div()
            .flex()
            .items_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.accent)
            .text_color(color)
            .children(text_field_children_named(
                "build-execute-field",
                &self.execute,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                16.,
                true,
            ))
            .when(empty, |this| this.child("clean install"));
        let history = self.history.iter().rev().enumerate().map(|(index, line)| {
            let line = line.clone();
            let shown = line.clone();
            div()
                .id(("build-history", index))
                .flex()
                .items_center()
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_S))
                .rounded(px(CONTROL_RADIUS))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_secondary)
                .hover(move |style| style.bg(theme.hover))
                .child(shown)
                .on_click(
                    cx.listener(move |this, _, window, cx| this.submit(line.clone(), window, cx)),
                )
        });
        popup_frame(theme)
            .absolute()
            .left(px(SPACE_S))
            .top(px(CONTROL_HEIGHT * 2. + SPACE_XS))
            .w(px(POPUP_WIDTH))
            .p(px(SPACE_S))
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .occlude()
            .child(field)
            .children(history)
    }

    /// The analyzer in place of the task tree: the list of the artifacts on the left, the
    /// paths of the selected one on the right.
    fn analyzer_body(&mut self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> Div {
        self.refresh_artifacts(cx);
        self.request_updates(cx);
        let filtered = self.filtered();
        let selected = self.analyzer_selected;
        let rows = filtered.iter().enumerate().map(|(position, index)| {
            let artifact_index = *index;
            let artifact = &self.artifacts[artifact_index];
            let label = artifact_label(artifact);
            let conflict = artifact.conflict();
            let latest = self.updates.get(&artifact.name).cloned();
            let chosen = selected == Some(artifact_index);
            div()
                .id(("dependencies-artifact", position))
                .debug_selector(move || format!("dependencies-artifact-{position}"))
                .flex()
                .items_center()
                .gap(px(SPACE_S))
                .h(px(ROW_HEIGHT))
                .px(px(SPACE_S))
                .rounded(px(ROW_RADIUS))
                .text_size(px(TEXT_SMALL))
                .when(chosen, |this| this.bg(theme.selection_inactive))
                .when(!chosen, |this| {
                    this.hover(move |style| style.bg(theme.hover))
                })
                .child(div().flex_1().overflow_hidden().child(label))
                .when(conflict, |this| {
                    this.child(div().text_color(theme.text_secondary).child("conflict"))
                })
                .when_some(latest, |this, latest| {
                    this.child(
                        div()
                            .debug_selector(move || format!("dependencies-update-{position}"))
                            .text_color(theme.accent)
                            .child(format!("update available: {latest}")),
                    )
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.analyzer_selected = Some(artifact_index);
                    this.search_active = false;
                    cx.notify();
                }))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                        this.analyzer_selected = Some(artifact_index);
                        this.show_analyzer_menu(event.position, cx);
                        cx.notify();
                    }),
                )
        });
        let empty = filtered.is_empty();
        let details = self.details();
        let message = self.message.clone();
        let search_empty = self.search.text.is_empty();
        let search_color = if search_empty {
            theme.text_secondary
        } else {
            theme.text
        };
        let search = div()
            .id("dependencies-search-frame")
            .debug_selector(|| "dependencies-search-frame".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if self.search_active {
                theme.accent
            } else {
                theme.border
            })
            .text_color(search_color)
            .children(text_field_children_named(
                "dependencies-search",
                &self.search,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                16.,
                self.search_active,
            ))
            .when(search_empty, |this| this.child("Search artifacts"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.search_active = true;
                cx.notify();
            }));
        let only_conflicts =
            filter_button(self.only_conflicts, theme).on_click(cx.listener(|this, _, _, cx| {
                this.only_conflicts = !this.only_conflicts;
                this.analyzer_selected = None;
                cx.notify();
            }));
        let details_rows: Vec<Div> = details
            .iter()
            .map(|line| {
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text)
                    .child(line.clone())
            })
            .collect();
        let list = scrollable(
            "dependencies-list",
            &self.analyzer_list_scroll,
            div()
                .id("dependencies-list")
                .flex()
                .flex_col()
                .py(px(SPACE_XS))
                .when(empty, |this| {
                    this.child(
                        div()
                            .px(px(SPACE_S))
                            .py(px(SPACE_S))
                            .text_color(theme.text_secondary)
                            .child("No artifacts"),
                    )
                })
                .children(rows),
            theme,
            window,
            cx,
        );
        let details_pane = scrollable(
            "dependencies-details",
            &self.analyzer_details_scroll,
            div()
                .id("dependencies-details")
                .flex()
                .flex_col()
                .gap(px(SPACE_XS))
                .px(px(SPACE_M))
                .py(px(SPACE_S))
                .children(details_rows)
                .when_some(message, |this, message| {
                    this.child(div().text_color(theme.text_secondary).child(message))
                }),
            theme,
            window,
            cx,
        );
        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_S))
                    .px(px(SPACE_S))
                    .py(px(SPACE_XS))
                    .child(search.flex_1())
                    .child(only_conflicts),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .w(px(LIST_WIDTH))
                            .flex_none()
                            .border_r_1()
                            .border_color(theme.border)
                            .child(list),
                    )
                    .child(div().flex_1().min_w_0().child(details_pane)),
            )
            .child(
                div()
                    .px(px(SPACE_S))
                    .py(px(SPACE_XS))
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child("Right-click a path for Go to Declaration, Copy Coordinates or Exclude"),
            )
    }
}

fn artifact_label(artifact: &Artifact) -> String {
    if artifact.selected.is_empty() {
        artifact.name.clone()
    } else {
        format!("{}:{}", artifact.name, artifact.selected)
    }
}

/// `Profiles` of the project: the node the named settings of the build hang under.
fn profiles_row(open: bool) -> Row {
    Row {
        kind: RowKind::Profiles,
        depth: 0,
        label: "Profiles".into(),
        expandable: true,
        open,
        module: String::new(),
        module_name: String::new(),
        task: None,
        program: false,
        key: Some(profiles_key()),
        hint: None,
        dependency: None,
        profile: None,
    }
}

/// One named setting of the build with its check box.
fn profile_row(profile: &BuildProfile, selection: &BuildSelection) -> Row {
    Row {
        kind: RowKind::Profile,
        depth: 1,
        label: profile.name.clone(),
        expandable: false,
        open: false,
        module: String::new(),
        module_name: String::new(),
        task: None,
        program: false,
        key: None,
        hint: profile.condition.clone(),
        dependency: None,
        profile: Some(ProfileRow {
            name: profile.name.clone(),
            active: selection.active(profile),
            choice: selection.choice(&profile.name),
        }),
    }
}

fn module_row(module: &BuildModule, has_dependencies: bool, closed: &HashSet<String>) -> Row {
    Row {
        kind: RowKind::Module,
        depth: 0,
        label: module.name.clone(),
        expandable: !module.groups.is_empty() || has_dependencies,
        open: !closed.contains(&module.id),
        module: module.id.clone(),
        module_name: module.name.clone(),
        task: None,
        program: false,
        key: None,
        hint: None,
        dependency: None,
        profile: None,
    }
}

fn group_row(module: &BuildModule, group: &BuildGroup, open: bool) -> Row {
    Row {
        kind: RowKind::Group,
        depth: 1,
        label: group.name.clone(),
        expandable: !group.tasks.is_empty(),
        open,
        module: module.id.clone(),
        module_name: module.name.clone(),
        task: None,
        program: false,
        key: Some(group_key(&module.id, &group.name)),
        hint: None,
        dependency: None,
        profile: None,
    }
}

fn task_row(module: &BuildModule, task: &BuildTask) -> Row {
    Row {
        kind: RowKind::Task,
        depth: 2,
        label: task.name.clone(),
        expandable: false,
        open: false,
        module: module.id.clone(),
        module_name: module.name.clone(),
        task: Some(task.name.clone()),
        program: task.program,
        key: None,
        hint: None,
        dependency: None,
        profile: None,
    }
}

/// `Dependencies` of a module: the node the scopes hang under.
fn dependencies_row(module: &BuildModule, open: bool) -> Row {
    Row {
        kind: RowKind::Dependencies,
        depth: 1,
        label: "Dependencies".into(),
        expandable: true,
        open,
        module: module.id.clone(),
        module_name: module.name.clone(),
        task: None,
        program: false,
        key: Some(dependencies_key(&module.id)),
        hint: None,
        dependency: None,
        profile: None,
    }
}

fn scope_row(module: &BuildModule, scope: &str, open: bool) -> Row {
    Row {
        kind: RowKind::Scope,
        depth: 2,
        label: scope_name(scope),
        expandable: true,
        open,
        module: module.id.clone(),
        module_name: module.name.clone(),
        task: None,
        program: false,
        key: Some(scope_key(&module.id, scope)),
        hint: None,
        dependency: None,
        profile: None,
    }
}

fn push_dependency_rows(
    module: &BuildModule,
    scope: &str,
    node: &Dependency,
    chain: &mut Vec<String>,
    depth: usize,
    closed: &HashSet<String>,
    rows: &mut Vec<Row>,
) {
    if depth > MAX_TREE_DEPTH {
        return;
    }
    chain.push(node.coordinates.clone());
    let key = dependency_key(&module.id, scope, chain);
    let open = !closed.contains(&key);
    rows.push(Row {
        kind: RowKind::Dependency,
        depth,
        label: dependencies::short_name(&node.coordinates),
        expandable: !node.children.is_empty(),
        open,
        module: module.id.clone(),
        module_name: module.name.clone(),
        task: None,
        program: false,
        key: Some(key),
        hint: dependencies::conflict_mark(node),
        dependency: Some(DependencyRow {
            coordinates: node.coordinates.clone(),
        }),
        profile: None,
    });
    if open {
        for child in &node.children {
            push_dependency_rows(module, scope, child, chain, depth + 1, closed, rows);
        }
    }
    chain.pop();
}

/// `compile` as the tree shows it: the tool writes the name in its own case.
fn scope_name(scope: &str) -> String {
    let mut characters = scope.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().collect::<String>() + characters.as_str(),
        None => String::new(),
    }
}

fn group_key(module: &str, group: &str) -> String {
    format!("{module}\u{0}{group}")
}

fn profiles_key() -> String {
    "profiles".to_owned()
}

fn dependencies_key(module: &str) -> String {
    format!("{module}\u{0}dependencies")
}

fn scope_key(module: &str, scope: &str) -> String {
    format!("{module}\u{0}dependencies\u{0}{scope}")
}

fn dependency_key(module: &str, scope: &str, chain: &[String]) -> String {
    format!(
        "{module}\u{0}dependencies\u{0}{scope}\u{0}{}",
        chain.join("\u{1}")
    )
}

fn toggle(set: &mut HashSet<String>, value: &str) {
    if !set.remove(value) {
        set.insert(value.to_owned());
    }
}

/// An icon button of the toolbar with the hint from [`TOOLTIPS`].
fn tool_button(
    id: &'static str,
    icon: &'static str,
    tooltip_index: usize,
    theme: &Theme,
) -> Stateful<Div> {
    icon_button(id, icon, TOOLTIPS[tooltip_index].1, None, theme)
        .debug_selector(move || id.to_owned())
}

fn skip_button(on: bool, theme: &Theme) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    div()
        .id("build-skip-tests")
        .flex()
        .items_center()
        .gap(px(SPACE_XS))
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .text_size(px(TEXT_SMALL))
        .occlude()
        .when(on, |this| this.bg(pressed))
        .when(!on, |this| this.hover(move |style| style.bg(hover)))
        .child(checkbox(on, theme))
        .child("Skip Tests")
}

/// The `Only conflicts` switch of the analyzer.
fn filter_button(on: bool, theme: &Theme) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    div()
        .id("dependencies-only-conflicts")
        .debug_selector(|| "dependencies-only-conflicts".to_owned())
        .flex()
        .items_center()
        .gap(px(SPACE_XS))
        .flex_none()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .text_size(px(TEXT_SMALL))
        .when(on, |this| this.bg(pressed))
        .when(!on, |this| this.hover(move |style| style.bg(hover)))
        .child(checkbox(on, theme))
        .child("Only conflicts")
}

/// One named setting of the build: the check box, the name, the condition of the build
/// file and the answer of the user.
fn profile_element(index: usize, row: &Row, theme: &Theme) -> Stateful<Div> {
    let active = row.profile.as_ref().is_some_and(|profile| profile.active);
    let mark = match row.profile.as_ref().map(|profile| profile.choice) {
        Some(ProfileChoice::On) => Some("on"),
        Some(ProfileChoice::Off) => Some("off"),
        _ => None,
    };
    let hover = theme.selection_hover;
    div()
        .id(("build-row", index))
        .flex()
        .items_center()
        .flex_none()
        .w_full()
        .h(px(ROW_HEIGHT))
        .pl(px(row.depth as f32 * 19. + ROW_PADDING_X))
        .pr(px(ROW_PADDING_X))
        .rounded(px(ROW_RADIUS))
        .hover(move |style| style.bg(hover))
        .child(div().size(px(16.)).flex_none())
        .child(checkbox(active, theme))
        .child(div().ml(px(SPACE_S)).flex_none().child(row.label.clone()))
        .children(row.hint.clone().map(|hint| {
            div()
                .ml(px(SPACE_M))
                .text_color(theme.text_secondary)
                .flex_none()
                .child(hint)
        }))
        .children(mark.map(|mark| {
            div()
                .ml(px(SPACE_M))
                .text_color(theme.text_secondary)
                .flex_none()
                .child(mark)
        }))
}

impl Render for BuildPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let title = self.run.read(cx).tool_title();
        let loading = self.run.read(cx).is_loading();
        let skip_supported = self.run.read(cx).supports_skip_tests();
        let workspace = self.workspace.clone();
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(TASKS_TOOL, window, cx)
                })
                .ok();
        };
        let rows = self.rows(cx);
        let selected = self.selected;
        let properties_supported = self.run.read(cx).takes_properties();
        let rows: Vec<_> =
            rows.into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let icon = match row.kind {
                        RowKind::Profiles => "nodes/folder",
                        RowKind::Profile => "general/check",
                        RowKind::Module => "nodes/module",
                        RowKind::Group => "nodes/folder",
                        RowKind::Task => "run/run",
                        RowKind::Dependencies => "nodes/library",
                        RowKind::Scope => "nodes/folder",
                        RowKind::Dependency => "nodes/library",
                    };
                    let style = TreeRow {
                        id: ("build-row", index).into(),
                        depth: row.depth,
                        label: row.label.clone().into(),
                        hint: row.hint.clone().map(Into::into),
                        icon,
                        expandable: row.expandable,
                        open: row.open,
                        selected: selected == Some(index),
                    };
                    let actionable = matches!(
                        row.kind,
                        RowKind::Task | RowKind::Dependency | RowKind::Profile
                    );
                    let element = match row.kind {
                        RowKind::Profile => profile_element(index, &row, &theme),
                        _ => tree_row(style, &theme),
                    };
                    let element = element.on_click(cx.listener(move |this, event, window, cx| {
                        this.click(index, event, window, cx)
                    }));
                    let element = element.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.selected = Some(index);
                            this.show_menu(index, event.position, cx);
                            cx.notify();
                        }),
                    );
                    let element = if row.kind == RowKind::Profile {
                        element
                            .tooltip(move |_, cx| tooltip(PROFILE_HINT, None, cx))
                            .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
                    } else {
                        element
                    };
                    let element = if actionable {
                        element.cursor_pointer()
                    } else {
                        element
                    };
                    div().w_full().px(px(ROW_MARGIN)).child(element)
                })
                .collect();
        let empty = rows.is_empty();
        let hint = if loading {
            "Loading project"
        } else if empty {
            "No tasks"
        } else {
            ""
        };
        let properties =
            (properties_supported && !self.analyzer).then(|| self.properties_bar(&theme, cx));
        let body = if self.analyzer {
            self.analyzer_body(&theme, window, cx).into_any_element()
        } else {
            scrollable(
                "build-rows",
                &self.scroll,
                div()
                    .id("build-rows")
                    .flex()
                    .flex_col()
                    .pt(px(SPACE_S))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows)
                    .when(!hint.is_empty(), |this| {
                        this.child(
                            div()
                                .px(px(ROW_MARGIN + SPACE_M))
                                .text_color(theme.text_secondary)
                                .child(hint),
                        )
                    }),
                &theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0()
            .into_any_element()
        };
        island(&theme)
            .id("build-panel")
            .key_context("BuildPanel ToolWindow")
            .relative()
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(panel_header(title, GROUP, hide, &theme))
            .child(self.toolbar(&theme, loading, skip_supported, cx))
            .children(properties)
            .child(body)
            .when(self.execute_open, |this| {
                this.child(self.execute_popup(&theme, cx))
            })
    }
}

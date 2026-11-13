//! The tool window of the build tool: the modules of the project, their groups of tasks
//! and the buttons that read the model again, run a line of the user or leave the tests
//! out. A double click or Enter runs a task in the console of the Run window.

use std::collections::HashSet;

use editor_view::Semantic;
use project_model::{BuildGroup, BuildModule, BuildTask};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{RUN_TOOL, TASKS_TOOL, Workspace};

use crate::prompt::TextField;
use crate::run::RunPanel;

const GROUP: &str = "right-panel";
const ROW_MARGIN: f32 = SPACE_L;
/// Lines of the Execute field that stay offered.
const HISTORY: usize = 10;
const POPUP_WIDTH: f32 = 250.;

/// The buttons of the window and the hint each of them shows. The tests read the same
/// table the render does.
pub(crate) const TOOLTIPS: &[(&str, &str)] = &[
    ("build-reload", "Reload All Projects"),
    ("build-execute", "Execute Goal/Task"),
    ("build-skip-tests", "Skip Tests"),
    ("build-expand-all", "Expand All"),
    ("build-collapse-all", "Collapse All"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowKind {
    Module,
    Group,
    Task,
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
    group: Option<String>,
    task: Option<String>,
    program: bool,
}

impl Row {
    fn group_key(&self) -> Option<String> {
        let group = self.group.as_deref()?;
        Some(format!("{}\u{0}{group}", self.module))
    }
}

pub struct BuildPanel {
    run: Entity<RunPanel>,
    workspace: WeakEntity<Workspace>,
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
    _run: Subscription,
}

impl BuildPanel {
    pub fn create(
        run: Entity<RunPanel>,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let _run = cx.observe(&run, |_, _, cx| cx.notify());
            Self {
                run,
                workspace,
                focus: cx.focus_handle(),
                closed_modules: HashSet::new(),
                closed_groups: HashSet::new(),
                selected: None,
                scroll: ScrollHandle::new(),
                execute_open: false,
                execute: TextField::default(),
                history: Vec::new(),
                skip_tests: false,
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

    fn rows(&self, cx: &App) -> Vec<Row> {
        let tree = self.run.read(cx).build_tree();
        let mut rows = Vec::new();
        for module in &tree {
            rows.push(module_row(module, &self.closed_modules));
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
                    rows.push(task_row(module, group, task));
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
            RowKind::Module => {
                toggle(&mut self.closed_modules, &row.module);
                cx.notify();
            }
            RowKind::Group => {
                if let Some(key) = row.group_key() {
                    toggle(&mut self.closed_groups, &key);
                }
                cx.notify();
            }
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
        if event.click_count() >= 2 {
            let second = self
                .rows(cx)
                .into_iter()
                .nth(index)
                .is_some_and(|row| row.kind == RowKind::Task);
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
                RowKind::Group => {
                    if let Some(key) = row.group_key() {
                        self.closed_groups.insert(key);
                    }
                }
                RowKind::Task => {}
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
                RowKind::Group => {
                    if let Some(key) = row.group_key() {
                        self.closed_groups.remove(&key);
                    }
                }
                RowKind::Task => {}
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
        for module in self.run.read(cx).build_tree() {
            for group in &module.groups {
                self.closed_groups
                    .insert(group_key(&module.id, &group.name));
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
}

fn module_row(module: &BuildModule, closed: &HashSet<String>) -> Row {
    Row {
        kind: RowKind::Module,
        depth: 0,
        label: module.name.clone(),
        expandable: !module.groups.is_empty(),
        open: !closed.contains(&module.id),
        module: module.id.clone(),
        module_name: module.name.clone(),
        group: None,
        task: None,
        program: false,
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
        group: Some(group.name.clone()),
        task: None,
        program: false,
    }
}

fn task_row(module: &BuildModule, group: &BuildGroup, task: &BuildTask) -> Row {
    Row {
        kind: RowKind::Task,
        depth: 2,
        label: task.name.clone(),
        expandable: false,
        open: false,
        module: module.id.clone(),
        module_name: module.name.clone(),
        group: Some(group.name.clone()),
        task: Some(task.name.clone()),
        program: task.program,
    }
}

fn group_key(module: &str, group: &str) -> String {
    format!("{module}\u{0}{group}")
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
        let rows: Vec<_> =
            rows.into_iter()
                .enumerate()
                .map(|(index, row)| {
                    let icon = match row.kind {
                        RowKind::Module => "nodes/module",
                        RowKind::Group => "nodes/folder",
                        RowKind::Task => "run/run",
                    };
                    let style = TreeRow {
                        id: ("build-row", index).into(),
                        depth: row.depth,
                        label: row.label.clone().into(),
                        hint: None,
                        icon,
                        expandable: row.expandable,
                        open: row.open,
                        selected: selected == Some(index),
                    };
                    let task = row.kind == RowKind::Task;
                    let element = tree_row(style, &theme).on_click(cx.listener(
                        move |this, event, window, cx| this.click(index, event, window, cx),
                    ));
                    let element = element.on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.selected = Some(index);
                            this.show_menu(index, event.position, cx);
                            cx.notify();
                        }),
                    );
                    let element = if task {
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
            .child(
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
                .min_h_0(),
            )
            .when(self.execute_open, |this| {
                this.child(self.execute_popup(&theme, cx))
            })
    }
}

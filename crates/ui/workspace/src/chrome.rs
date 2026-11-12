//! What the main window looks like: the layout of the S10 mock on real state.

use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use ::platform::open_url;
use keymap::{editor as editor_keys, workspace as workspace_keys};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::editor::{ENCODINGS, IndentChoice, LINE_ENDS};
use crate::settings::Settings;
use crate::window::{
    AI_TOOL, BalloonAction, BuildProject, COMMIT_TOOL, ChooseDebugConfiguration,
    ChooseRunConfiguration, CloseTab, Compile, CopyElement, DEBUG_TOOL, Debug, Dismiss, DockSide,
    EditBreakpoint, Evaluate, FocusNextGroup, FocusPreviousGroup, ForceRunToCursor, ForceStepInto,
    ForceStepOver, GotoRelated, HideToolWindow, LOG_TOOL, MoveToOtherGroup, NewElementSamePlace,
    NextProblem, NextProjectWindow, NextTab, OpenFolder, Overlay, PROBLEMS_TOOL, PROJECT_TOOL,
    PinTab, PreviousProblem, PreviousProjectWindow, ProjectStructure, PushChanges, QuickEvaluate,
    RUN_TOOL, ReopenTab, Rerun, RerunTests, Resume, Run, RunToCursor, SERVICES_TOOL,
    STRUCTURE_TOOL, SaveAll, ShowBranches, ShowCommit, ShowExecutionPoint, ShowSearch,
    ShowSettings, SmartStepInto, SplitDown, SplitRight, StepInto, StepOut, StepOver, Stop,
    StopBackgroundProcesses, Synchronize, TASKS_TOOL, TERMINAL_TOOL, TabLeft, TabRight, ToggleAi,
    ToggleBreakpoint, ToggleCommit, ToggleDebug, ToggleLog, ToggleProject, ToggleRun,
    ToggleServices, ToggleStructure, ToggleTemporaryLineBreakpoint, ToggleTerminal, ToggleTheme,
    ToggleUsages, USAGES_TOOL, UpdateProject, VcsQuickList, ViewBreakpoints, Workspace,
};
use crate::window::{TODO_TOOL, ToggleProblems, ToggleTodo};

/// Wider than the gap itself so that the edge is easy to catch.
const EDGE_GRIP: f32 = 6.;
const TAB_ID_SHIFT: usize = 16;
/// The padding of both sides of a section of the menu bar.
const MENU_ITEM_PADDING: f32 = SPACE_L;
/// The hairline between two sections of the menu bar.
const MENU_ITEM_GAP: f32 = 1.;
/// What the overflow row of the menu bar keeps for its ellipsis and padding.
const MENU_OVERFLOW_WIDTH: f32 = SPACE_L * 2. + SPACE_XS;
/// What the configuration dropdown keeps apart from its label.
const TOOLBAR_CONFIG_FIXED: f32 = SPACE_S * 2. + SPACE_XS + TEXT_SMALL;
/// The space before the window buttons while Stop is in the run group.
const TOOLBAR_WINDOW_GAP: f32 = SPACE_M;
/// The same space while nothing runs: it keeps the width Stop takes, so that adding
/// Stop never moves Run or Debug.
const TOOLBAR_WINDOW_GAP_IDLE: f32 = TOOLBAR_WINDOW_GAP + ICON_BUTTON + SPACE_XS;
/// What the right side of the toolbar keeps, apart from the run configuration name:
/// the run group, the divider, the tools, the idle space before the window buttons and
/// the three window buttons themselves.
const TOOLBAR_RIGHT_FIXED: f32 = TOOLBAR_CONFIG_FIXED
    + SPACE_XS * 2.
    + ICON_BUTTON * 2.
    + SPACE_L
    + ICON_BUTTON * 2.
    + SPACE_XS
    + TOOLBAR_WINDOW_GAP_IDLE
    + WINDOW_BUTTON_WIDTH * 3.;
/// What the left side keeps for its padding and gaps, apart from the labels.
const TOOLBAR_LEFT_FIXED: f32 = SPACE_M + SPACE_L + SPACE_XS + SPACE_L;
/// What the project button keeps apart from its name and badge.
const TOOLBAR_PROJECT_FIXED: f32 = SPACE_S * 2. + PROJECT_BADGE + SPACE_XS * 2. + TEXT_SMALL;
/// What the branch button keeps apart from its name.
const TOOLBAR_BRANCH_FIXED: f32 = SPACE_S * 2. + ICON + SPACE_XS * 2. + TEXT_SMALL;
/// What the channel label keeps apart from its text.
const TOOLBAR_CHANNEL_FIXED: f32 = SPACE_S * 2.;
/// Space left free before the section that does not fit goes to the overflow row.
const TOOLBAR_SLACK: f32 = SPACE_M;
/// The mnemonics of the menu bar, in the order of `main_menu`.
const MENU_MNEMONICS: [char; 11] = ['F', 'E', 'V', 'N', 'C', 'R', 'B', 'U', 'G', 'W', 'H'];
/// The bar item of the sections that did not fit, in `MenuEntry` paths.
const MENU_OVERFLOW: usize = usize::MAX;

#[derive(Clone, Copy)]
struct DraggedEdge(DockSide);

/// Where a menu that a button opens goes: under the button while its area is known,
/// otherwise at the pointer of the click.
#[derive(Clone, Copy)]
struct MenuPlacement {
    anchor: Option<Bounds<Pixels>>,
    position: Point<Pixels>,
}

struct NoDragPreview;

impl Render for NoDragPreview {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        Empty
    }
}

/// A tab on its way to another place, by the pointer.
#[derive(Clone)]
struct DraggedTab {
    pane: usize,
    index: usize,
    title: SharedString,
}

struct TabDragPreview(SharedString);

impl Render for TabDragPreview {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        div()
            .flex()
            .items_center()
            .h(px(26.))
            .px(px(SPACE_M))
            .rounded(px(TAB_RADIUS))
            .border_1()
            .border_color(theme.tab_selected_border)
            .bg(theme.tab_selected)
            .text_color(theme.text)
            .child(self.0.clone())
    }
}

/// How a project in the recent list is shown: its name and, dimmed by the menu, the folder.
fn recent_label(project: &Path) -> String {
    let name = project.file_name().unwrap_or_default().to_string_lossy();
    format!("{name}  {}", project.display())
}

fn tab_id(pane: usize, index: usize) -> usize {
    (pane << TAB_ID_SHIFT) | index
}

/// The label of a bar section, with the letter of its mnemonic underlined while Alt is
/// held.
fn menu_bar_label(label: &SharedString, index: usize, alt: bool) -> Div {
    let Some(mnemonic) = MENU_MNEMONICS.get(index).copied() else {
        return div().child(label.clone());
    };
    if !alt {
        return div().child(label.clone());
    }
    let Some((position, _)) = label
        .char_indices()
        .find(|(_, letter)| letter.eq_ignore_ascii_case(&mnemonic))
    else {
        return div().child(label.clone());
    };
    let (before, rest) = label.split_at(position);
    let len = rest.chars().next().map_or(0, |letter| letter.len_utf8());
    let (letter, after) = rest.split_at(len);
    div()
        .flex()
        .items_center()
        .children((!before.is_empty()).then(|| div().child(before.to_owned())))
        .child(div().underline().child(letter.to_owned()))
        .children((!after.is_empty()).then(|| div().child(after.to_owned())))
}

/// Folders of `path` under `root`, then the file name.
fn path_parts(root: &Path, path: &Path) -> Vec<String> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let parts = relative.components();
    parts
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect()
}

/// A menu row that runs on the workspace.
fn workspace_entry(
    weak: &WeakEntity<Workspace>,
    label: &'static str,
    run: impl Fn(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) -> MenuEntry {
    let weak = weak.clone();
    MenuEntry::new(label, move |window, cx| {
        weak.update(cx, |this, cx| run(this, window, cx)).ok();
    })
}

/// A row that runs the command the keymap and Find Action know by `id`.
fn command_entry(weak: &WeakEntity<Workspace>, id: &'static str, label: &'static str) -> MenuEntry {
    let weak = weak.clone();
    MenuEntry::new(label, move |window, cx| {
        weak.update(cx, |this, cx| {
            this.run_command(id, window, cx);
        })
        .ok();
    })
    .id(id)
}

/// A row that dispatches a window action, as its key does.
fn action_entry<A: Action + Clone + 'static>(label: &'static str, action: A) -> MenuEntry {
    let id = A::name_for_type();
    MenuEntry::new(label, move |window, cx| {
        window.dispatch_action(Box::new(action.clone()), cx);
    })
    .id(id)
}

impl Workspace {
    fn stripe_button(
        &self,
        id: &'static str,
        icon_name: &'static str,
        label: &'static str,
        key: Option<&'static str>,
        selected: bool,
        theme: &Theme,
    ) -> Stateful<Div> {
        stripe_button(
            id,
            icon_name,
            label.into(),
            key.and_then(|key| self.shortcut_of_key(key))
                .map(Into::into),
            selected,
            theme,
        )
        .debug_selector(move || format!("stripe-{id}"))
    }

    fn left_stripe(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let toggle = |id: &'static str| {
            cx.listener(move |this: &mut Self, _: &ClickEvent, window, cx| {
                this.toggle_tool(id, window, cx)
            })
        };
        let separator = || div().w(px(ICON)).h(px(DIVIDER_WIDTH)).bg(theme.border);
        let project_open = self.tool_open(PROJECT_TOOL);
        let run_open = self.tool_open(RUN_TOOL);
        let commit_open = self.tool_open(COMMIT_TOOL);
        let log_open = self.tool_open(LOG_TOOL);
        let structure_open = self.tool_open(STRUCTURE_TOOL);
        let terminal_open = self.tool_open(TERMINAL_TOOL);
        let services_open = self.tool_open(SERVICES_TOOL);
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_between()
            .flex_none()
            .w(px(STRIPE_WIDTH))
            .py(px(SPACE_S))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .child(
                        self.stripe_button(
                            "s-project",
                            "toolwindows/project",
                            "Project",
                            Some("workspace::ToggleProject"),
                            project_open,
                            theme,
                        )
                        .on_click(toggle(PROJECT_TOOL)),
                    )
                    .child(
                        self.stripe_button(
                            "s-commit",
                            "toolwindows/commit",
                            "Commit",
                            Some("workspace::ToggleCommit"),
                            commit_open,
                            theme,
                        )
                        .on_click(toggle(COMMIT_TOOL)),
                    )
                    .child(
                        self.stripe_button(
                            "s-log",
                            "general/vcs",
                            "Git Log",
                            Some("workspace::ToggleLog"),
                            log_open,
                            theme,
                        )
                        .on_click(toggle(LOG_TOOL)),
                    )
                    .child(separator())
                    .child(
                        self.stripe_button(
                            "s-structure",
                            "toolwindows/structure",
                            "Structure",
                            Some("workspace::ToggleStructure"),
                            structure_open,
                            theme,
                        )
                        .on_click(toggle(STRUCTURE_TOOL)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .child(separator())
                    .child(
                        self.stripe_button(
                            "s-run",
                            "toolwindows/run",
                            "Run",
                            Some("workspace::ToggleRun"),
                            run_open,
                            theme,
                        )
                        .on_click(toggle(RUN_TOOL)),
                    )
                    .child(
                        self.stripe_button(
                            "s-debug",
                            "toolwindows/debug",
                            "Debug",
                            Some("workspace::ToggleDebug"),
                            self.tool_open(DEBUG_TOOL),
                            theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.toggle_tool(DEBUG_TOOL, window, cx)
                        })),
                    )
                    .child(
                        self.stripe_button(
                            "s-terminal",
                            "toolwindows/terminal",
                            "Terminal",
                            Some("workspace::ToggleTerminal"),
                            terminal_open,
                            theme,
                        )
                        .on_click(toggle(TERMINAL_TOOL)),
                    )
                    .child(
                        self.stripe_button(
                            "s-services",
                            "toolwindows/services",
                            "Services",
                            Some("workspace::ToggleServices"),
                            services_open,
                            theme,
                        )
                        .on_click(toggle(SERVICES_TOOL)),
                    )
                    .child(
                        self.stripe_button(
                            "s-todo",
                            "status/info",
                            "TODO",
                            Some("workspace::ToggleTodo"),
                            self.tool_open(TODO_TOOL),
                            theme,
                        )
                        .on_click(toggle(TODO_TOOL)),
                    )
                    .child(
                        self.stripe_button(
                            "s-problems",
                            "toolwindows/problems",
                            "Problems",
                            Some("workspace::ToggleProblems"),
                            self.tool_open(PROBLEMS_TOOL),
                            theme,
                        )
                        .on_click(toggle(PROBLEMS_TOOL)),
                    )
                    .child(
                        self.stripe_button(
                            "s-usages",
                            "codeInsight/search",
                            "Usages",
                            Some("workspace::ToggleUsages"),
                            self.tool_open(USAGES_TOOL),
                            theme,
                        )
                        .on_click(toggle(USAGES_TOOL)),
                    ),
            )
    }

    fn right_stripe(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let separator = div().w(px(ICON)).h(px(DIVIDER_WIDTH)).bg(theme.border);
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .w(px(RIGHT_STRIPE_WIDTH))
            .py(px(SPACE_S))
            .child(
                self.stripe_button(
                    "s-notifications",
                    "toolwindows/notifications",
                    "Notifications",
                    None,
                    self.balloon_visible,
                    theme,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_balloon(cx))),
            )
            .child(
                self.stripe_button(
                    "s-ai",
                    "own/aiSessions",
                    "AI Sessions",
                    Some("workspace::ToggleAi"),
                    self.dock_open(DockSide::Right),
                    theme,
                )
                .on_click(
                    cx.listener(|this, _, window, cx| {
                        this.toggle_panel(DockSide::Right, window, cx)
                    }),
                ),
            )
            .child(separator)
            .child(
                self.stripe_button(
                    "s-tasks",
                    "toolwindows/tasks",
                    "Tasks",
                    None,
                    self.tool_open(TASKS_TOOL),
                    theme,
                )
                .on_click(
                    cx.listener(|this, _, window, cx| this.toggle_tool(TASKS_TOOL, window, cx)),
                ),
            )
            .child(
                self.stripe_button(
                    "s-bookmarks",
                    "toolwindows/bookmarks",
                    "Bookmarks",
                    None,
                    false,
                    theme,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.run_command("window.show_bookmarks", window, cx);
                })),
            )
    }

    fn toolbar(&self, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let initial: String = self
            .name
            .chars()
            .take(1)
            .flat_map(char::to_uppercase)
            .collect();
        let badge = div()
            .flex()
            .items_center()
            .justify_center()
            .size(px(PROJECT_BADGE))
            .rounded(px(PROJECT_BADGE_RADIUS))
            .bg(theme.accent)
            .text_size(px(TEXT_SMALL))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(theme.over_accent)
            .child(initial);
        let project = toolbar_dropdown(
            "t-project",
            Some(badge.into_any_element()),
            self.name.clone(),
            theme,
        )
        .debug_selector(|| "t-project".to_owned())
        .relative()
        .child(self.t_project_anchor.canvas())
        .on_click(cx.listener(|this, _, window, cx| {
            let anchor = this.t_project_anchor.bounds();
            this.show_project_menu(anchor, window, cx)
        }));
        let channel = div()
            .id("app-channel")
            .debug_selector(|| "app-channel".to_owned())
            .flex()
            .items_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_S))
            .rounded(px(CONTROL_RADIUS))
            .text_size(px(TEXT_SMALL))
            .font_weight(FontWeight::MEDIUM)
            .text_color(theme.accent)
            .hover(move |style| style.bg(theme.hover))
            .child(self.info().channel_label())
            .on_click(cx.listener(|this, _, window, cx| this.show_about(window, cx)));
        let branch = self.branch.clone().map(|branch| {
            let leading = icon("general/vcs", theme).into_any_element();
            toolbar_dropdown("t-branch", Some(leading), branch, theme)
                .debug_selector(|| "t-branch".to_owned())
                .relative()
                .child(self.t_branch_anchor.canvas())
                .on_click(cx.listener(|this, _, window, cx| {
                    if let Some(anchor) = this.t_branch_anchor.bounds() {
                        this.set_action_menu_anchor(anchor);
                    }
                    window.dispatch_action(Box::new(ShowBranches), cx);
                }))
        });
        let search = toolbar_button(
            "t-search",
            "general/search",
            "Search Everywhere",
            Some("Double Shift".into()),
            theme,
        )
        .debug_selector(|| "t-search".to_owned())
        .on_click(cx.listener(|this, _, window, cx| this.show_search(window, cx)));
        let settings = toolbar_button(
            "t-settings",
            "general/settings",
            "Settings",
            self.shortcut_of_key("workspace::ShowSettings")
                .map(Into::into),
            theme,
        )
        .debug_selector(|| "t-settings".to_owned())
        .on_click(
            cx.listener(|_, _, window, cx| window.dispatch_action(Box::new(ShowSettings), cx)),
        );
        let maximize = if window.is_maximized() {
            "windows/restoreSmall"
        } else {
            "windows/maximizeSmall"
        };
        // Stop is drawn in the run group. The gap before the window buttons keeps the
        // width it takes, so that adding it never moves Run or Debug: the free space is
        // given back on the right, where nothing else sits.
        let active = self.run_state(cx).running || self.debug_state(cx).active;
        let window_gap = if active {
            TOOLBAR_WINDOW_GAP
        } else {
            TOOLBAR_WINDOW_GAP_IDLE
        };
        let window_buttons = div()
            .flex()
            .items_center()
            .ml(px(window_gap))
            .child(window_button(
                "w-min",
                "windows/minimizeSmall",
                "Minimize",
                WindowControlArea::Min,
                false,
                theme,
            ))
            .child(window_button(
                "w-max",
                maximize,
                if window.is_maximized() {
                    "Restore"
                } else {
                    "Maximize"
                },
                WindowControlArea::Max,
                false,
                theme,
            ))
            .child(window_button(
                "w-close",
                "windows/closeSmall",
                "Close",
                WindowControlArea::Close,
                true,
                theme,
            ));
        let tools = div()
            .id("tools-group")
            .debug_selector(|| "tools-group".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .child(search)
            .child(settings);
        let right = div()
            .relative()
            .flex()
            .items_center()
            .child(self.run_widget(theme, cx))
            .child(
                div()
                    .w(px(DIVIDER_WIDTH))
                    .h(px(DIVIDER))
                    .mx(px((SPACE_L - DIVIDER_WIDTH) / 2.))
                    .bg(theme.border),
            )
            .child(tools)
            .child(window_buttons);

        div()
            .id("toolbar")
            .debug_selector(|| "toolbar".to_owned())
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT))
            .window_control_area(WindowControlArea::Drag)
            .child(
                div()
                    .id("left-group")
                    .debug_selector(|| "left-group".to_owned())
                    .flex()
                    .items_center()
                    .h_full()
                    .pl(px(SPACE_M))
                    .child(self.menu_bar(window, theme, cx))
                    .child(project.ml(px(SPACE_L)))
                    .child(channel.ml(px(SPACE_XS)))
                    .children(branch.map(|branch| branch.ml(px(SPACE_L)))),
            )
            .child(right)
    }

    fn tab_bar(
        &self,
        pane: usize,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let active = self.panes[pane].active;
        let selection = theme.selection;
        let tabs = self.panes[pane]
            .tabs
            .iter()
            .enumerate()
            .map(|(index, item)| {
                let id = tab_id(pane, index);
                let dirty = (item.editor.status)(cx).dirty;
                let pinned = item.pinned;
                let style = TabStyle {
                    id: ("tab", id).into(),
                    close_id: ("tab-close", id).into(),
                    group: format!("tab-{id}").into(),
                    label: item.title.clone(),
                    icon: item.icon,
                    selected: index == active,
                    dirty,
                    pinned,
                };
                // The dot of a pinned tab unpins it, the cross of another one closes it.
                let close = cx.listener(move |this, _: &ClickEvent, window, cx| {
                    cx.stop_propagation();
                    if pinned {
                        this.toggle_pin(pane, index, window, cx);
                    } else {
                        this.close_tab(pane, index, window, cx);
                    }
                });
                let dragged = DraggedTab {
                    pane,
                    index,
                    title: item.title.clone(),
                };
                tab(style, close, theme)
                    .debug_selector(move || format!("tab-{pane}-{index}"))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.select_tab(pane, index, window, cx)
                    }))
                    .on_mouse_down(
                        MouseButton::Middle,
                        cx.listener(move |this, _, window, cx| {
                            this.close_tab(pane, index, window, cx)
                        }),
                    )
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                            this.show_tab_menu(pane, index, event.position, cx)
                        }),
                    )
                    .on_drag(dragged, |dragged, _, _, cx| {
                        let title = dragged.title.clone();
                        cx.new(|_| TabDragPreview(title))
                    })
                    .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(selection))
                    .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                        let from = (dragged.pane, dragged.index);
                        this.move_tab(from, (pane, index), window, cx)
                    }))
            });
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .px(px(SPACE_M))
            .border_b_1()
            .border_color(theme.border)
            .child(
                scrollable(
                    ("tabs", pane),
                    &self.panes[pane].scroll,
                    div()
                        .id(("tabs", pane))
                        .debug_selector(move || format!("tabs-{pane}"))
                        .flex()
                        .items_center()
                        .gap(px(2.))
                        .overflow_x_scroll()
                        .track_scroll(&self.panes[pane].scroll)
                        .children(tabs)
                        .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(selection))
                        .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                            let end = this.panes[pane].tabs.len();
                            let from = (dragged.pane, dragged.index);
                            this.move_tab(from, (pane, end), window, cx)
                        })),
                    theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_w_0()
                .h_full()
                .items_center(),
            )
    }

    /// The configuration list, Run, Debug, and Stop while something runs.
    fn run_widget(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let state = self.run_state(cx);
        let configs = toolbar_dropdown("t-config", None, state.label(), theme)
            .debug_selector(|| "t-config".to_owned())
            .relative()
            .child(self.t_config_anchor.canvas())
            .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                let anchor = this.t_config_anchor.bounds();
                this.show_run_menu(anchor, event.position(), window, cx)
            }));
        let run_icon = icon_sized("run/run", ICON, theme);
        let run_icon = if state.can_run() {
            run_icon
        } else {
            run_icon.grayscale(true).opacity(0.4)
        };
        let run = button_frame(
            "t-run",
            "Run",
            self.shortcut_of_key("workspace::Run").map(Into::into),
            theme,
        )
        .debug_selector(|| "t-run".to_owned())
        .on_click(cx.listener(|this, _, window, cx| this.run_selected(window, cx)))
        .child(run_icon);
        let debug = button_frame(
            "t-debug",
            "Debug",
            self.shortcut_of_key("workspace::Debug").map(Into::into),
            theme,
        )
        .debug_selector(|| "t-debug".to_owned())
        .on_click(cx.listener(|this, _, window, cx| this.debug_selected(window, cx)))
        .child(icon_sized("run/debug", ICON, theme));
        let active = state.running || self.debug_state(cx).active;
        let stop = active.then(|| {
            button_frame(
                "t-stop",
                "Stop",
                self.shortcut_of_key("workspace::Stop").map(Into::into),
                theme,
            )
            .debug_selector(|| "t-stop".to_owned())
            .on_click(cx.listener(|this, _, window, cx| this.stop_run(window, cx)))
            .child(icon_sized("run/stop", ICON, theme))
        });
        div()
            .id("run-group")
            .debug_selector(|| "run-group".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .child(configs)
            .child(run)
            .child(debug)
            .children(stop)
    }

    /// The entries of the configuration menu: the configurations of the project and the
    /// build command under them. A pick selects the configuration; with `run_on_pick`
    /// (Alt+Shift+F10) it also runs it, while the dropdown of the button only selects.
    fn run_menu_entries(&self, run_on_pick: bool, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let state = self.run_state(cx);
        let weak = cx.weak_entity();
        let mut entries: Vec<MenuEntry> = state
            .configs
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let weak = weak.clone();
                let temporary = state.is_temporary(index);
                let entry = MenuEntry::new(name.clone(), move |window, cx| {
                    weak.update(cx, |this, cx| {
                        this.select_run(index, window, cx);
                        if run_on_pick {
                            this.run_selected(window, cx);
                        }
                    })
                    .ok();
                })
                .icon("run/run");
                if temporary { entry.muted() } else { entry }
            })
            .collect();
        if let Some(index) = state.selected.filter(|index| state.is_temporary(*index))
            && let Some(name) = state.configs.get(index)
        {
            let save = weak.clone();
            entries.push(
                MenuEntry::new(format!("Save '{name}' Configuration"), move |window, cx| {
                    save.update(cx, |this, cx| this.save_run(window, cx)).ok();
                })
                .icon("general/check"),
            );
        }
        let build = weak.clone();
        entries.push(MenuEntry::separator());
        entries.push(
            MenuEntry::new("Build Project", move |window, cx| {
                build
                    .update(cx, |this, cx| this.build_project(window, cx))
                    .ok();
            })
            .shortcut("Ctrl+F9"),
        );
        entries.push(MenuEntry::separator());
        entries.push(self.command_row(&weak, "run.edit_configurations", "Edit Configurations..."));
        entries
    }

    fn show_run_menu(
        &mut self,
        anchor: Option<Bounds<Pixels>>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let entries = self.run_menu_entries(false, cx);
        match anchor {
            Some(anchor) => self.show_menu_under(anchor, entries, window, cx),
            None => self.show_menu(position, entries, cx),
        }
    }

    /// Alt+Shift+F10: the menu of the configuration button, under it, with its first row
    /// highlighted so the arrows and Enter work at once. False when the button is not on
    /// screen, and the caller opens the palette instead.
    pub(crate) fn show_run_menu_under_button(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(anchor) = self.t_config_anchor.bounds() else {
            return false;
        };
        let entries = self.run_menu_entries(true, cx);
        self.show_menu_under(anchor, entries, window, cx);
        if let Some(Overlay::Menu { entries, state, .. }) = &mut self.overlay {
            state.move_down(entries);
        }
        true
    }

    fn show_tab_menu(
        &mut self,
        pane: usize,
        index: usize,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.weak_entity();
        let path = self.panes[pane].tabs[index].path.clone();
        let text = path.display().to_string();
        let pin = if self.panes[pane].tabs[index].pinned {
            "Unpin Tab"
        } else {
            "Pin Tab"
        };
        let entries = vec![
            workspace_entry(&weak, "Close", move |this, window, cx| {
                this.close_tab(pane, index, window, cx)
            })
            .shortcut("Ctrl+F4"),
            workspace_entry(&weak, "Close Other Tabs", move |this, window, cx| {
                this.close_other_tabs(pane, index, window, cx)
            }),
            workspace_entry(&weak, "Close All Tabs", move |this, window, cx| {
                this.close_all_tabs(pane, window, cx)
            }),
            MenuEntry::separator(),
            workspace_entry(&weak, pin, move |this, window, cx| {
                this.toggle_pin(pane, index, window, cx)
            }),
            MenuEntry::separator(),
            workspace_entry(&weak, "Split Right", move |this, window, cx| {
                this.select_tab(pane, index, window, cx);
                this.split_right(window, cx);
            }),
            workspace_entry(&weak, "Split Down", move |this, window, cx| {
                this.select_tab(pane, index, window, cx);
                this.split_down(window, cx);
            }),
            workspace_entry(&weak, "Move to Other Group", move |this, window, cx| {
                this.select_tab(pane, index, window, cx);
                this.move_to_other_group(window, cx);
            }),
            MenuEntry::separator(),
            workspace_entry(&weak, "Reload from Disk", move |this, _, cx| {
                this.reload_tab(pane, index, cx)
            }),
            MenuEntry::new("Copy Path", move |_, cx| {
                cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
            })
            .icon("general/copy"),
            MenuEntry::new("Reveal in Explorer", move |_, cx| {
                ui_kit::reveal_path(cx, &path)
            }),
            workspace_entry(&weak, "Show History", move |this, window, cx| {
                this.select_tab(pane, index, window, cx);
                this.run_command("git.show_file_history", window, cx);
            }),
        ];
        self.show_menu(position, entries, cx);
    }

    /// A command row with its key from the keymap. A command that nothing registered is
    /// shown, but grey: the row of a feature that is not here yet does not run or panic.
    fn command_row(
        &self,
        weak: &WeakEntity<Workspace>,
        id: &'static str,
        label: &'static str,
    ) -> MenuEntry {
        let mut entry = if self.commands().get(id).is_some() {
            command_entry(weak, id, label)
        } else {
            MenuEntry::disabled(label).id(id)
        };
        if let Some(shortcut) = self.shortcut_of_key(id) {
            entry = entry.shortcut(shortcut);
        }
        entry
    }

    /// A window action row with its key from the window bindings.
    fn action_row<A: Action + Clone + 'static>(&self, label: &'static str, action: A) -> MenuEntry {
        let entry = action_entry(label, action);
        match self.shortcut_of_key(A::name_for_type()) {
            Some(shortcut) => entry.shortcut(shortcut),
            None => entry,
        }
    }

    /// The rows of the projects opened before, the latest last.
    fn recent_project_entries(&self, weak: &WeakEntity<Workspace>) -> Vec<MenuEntry> {
        let others: Vec<&PathBuf> = self
            .recent
            .iter()
            .filter(|path| **path != self.root)
            .collect();
        let mut entries = if others.is_empty() {
            vec![MenuEntry::disabled("No Recent Projects")]
        } else {
            others
                .into_iter()
                .map(|project| {
                    let weak = weak.clone();
                    let project = project.clone();
                    let label = recent_label(&project);
                    MenuEntry::new(label, move |_, cx| {
                        let project = project.clone();
                        weak.update(cx, |this, cx| this.open_project(project, cx))
                            .ok();
                    })
                    .icon("nodes/module")
                })
                .collect::<Vec<_>>()
        };
        entries.push(MenuEntry::separator());
        entries.push(if self.recent.is_empty() {
            MenuEntry::disabled("Clear List")
        } else {
            workspace_entry(weak, "Clear List", |this, _, cx| {
                this.clear_recent_projects(cx)
            })
        });
        entries
    }

    /// The project dropdown of the title bar: the actions of the project, under the button.
    fn show_project_menu(
        &mut self,
        anchor: Option<Bounds<Pixels>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.weak_entity();
        let mut entries = vec![
            self.action_row("Open Folder...", OpenFolder),
            MenuEntry::separator(),
        ];
        entries.extend(self.recent_project_entries(&weak));
        entries.push(MenuEntry::separator());
        entries.push(workspace_entry(
            &weak,
            "Close Project",
            |this, window, cx| this.close_window(window, cx),
        ));
        match anchor {
            Some(anchor) => self.show_menu_under(anchor, entries, window, cx),
            None => self.show_menu(window.mouse_position(), entries, cx),
        }
    }

    /// The sections of the main menu, in IDEA's order, with the rows the window can run.
    pub fn main_menu(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let weak = cx.weak_entity();
        let command = |id: &'static str, label: &'static str| self.command_row(&weak, id, label);

        let recent = self.recent_project_entries(&weak);

        let file = MenuEntry::submenu(
            "File",
            vec![
                self.action_row("Open Folder...", OpenFolder),
                command("file.open", "Open..."),
                self.action_row("Save All", SaveAll),
                command(workspace_keys::SAVE_AS, "Save As..."),
                MenuEntry::separator(),
                MenuEntry::submenu("Open Recent", recent),
                MenuEntry::separator(),
                self.command_row(&weak, "file.reload_all", "Reload All from Disk"),
                command("file.invalidate_caches", "Invalidate Caches..."),
                MenuEntry::separator(),
                MenuEntry::submenu(
                    "Local History",
                    vec![
                        command("local_history.show", "Show History"),
                        command("local_history.show_project", "Show Project History"),
                        command("local_history.put_label", "Put Label..."),
                    ],
                ),
                MenuEntry::separator(),
                self.action_row("Settings...", ShowSettings),
                self.action_row("Project Structure...", ProjectStructure),
                {
                    let weak = weak.clone();
                    MenuEntry::new("Close Project", move |window, cx| {
                        weak.update(cx, |this, cx| this.close_window(window, cx))
                            .ok();
                    })
                },
                command("app.exit", "Exit"),
            ],
        );

        let edit = MenuEntry::submenu(
            "Edit",
            vec![
                command(editor_keys::UNDO, "Undo"),
                command(editor_keys::REDO, "Redo"),
                MenuEntry::separator(),
                command(editor_keys::CUT, "Cut"),
                command(editor_keys::COPY, "Copy"),
                command(editor_keys::PASTE, "Paste"),
                command(editor_keys::PASTE_HISTORY, "Paste History..."),
                command(editor_keys::DELETE, "Delete"),
                MenuEntry::separator(),
                command(editor_keys::FIND, "Find..."),
                command(editor_keys::FIND_NEXT, "Find Next"),
                command(editor_keys::FIND_PREVIOUS, "Find Previous"),
                command(editor_keys::REPLACE, "Replace..."),
                command(workspace_keys::FIND_IN_FILES, "Find in Files..."),
                command(workspace_keys::REPLACE_IN_FILES, "Replace in Files..."),
                MenuEntry::separator(),
                command(editor_keys::SELECT_ALL, "Select All"),
                command(editor_keys::EXTEND_SELECTION, "Extend Selection"),
                command(editor_keys::SHRINK_SELECTION, "Shrink Selection"),
                command(
                    editor_keys::SELECT_NEXT_OCCURRENCE,
                    "Add Selection for Next Occurrence",
                ),
                command(
                    editor_keys::SELECT_ALL_OCCURRENCES,
                    "Select All Occurrences",
                ),
                command(editor_keys::COLUMN_MODE, "Column Selection Mode"),
                MenuEntry::separator(),
                command(editor_keys::DUPLICATE, "Duplicate Line"),
                command(editor_keys::DELETE_LINE, "Delete Line"),
                command(editor_keys::MOVE_LINE_UP, "Move Line Up"),
                command(editor_keys::MOVE_LINE_DOWN, "Move Line Down"),
                command(editor_keys::JOIN_LINES, "Join Lines"),
                command(editor_keys::TOGGLE_COMMENT, "Comment Line"),
                command(editor_keys::BLOCK_COMMENT, "Comment Block"),
                command(editor_keys::COMPLETE_STATEMENT, "Complete Statement"),
                MenuEntry::separator(),
                command(editor_keys::REFORMAT, "Reformat Code"),
                command(editor_keys::OPTIMIZE_IMPORTS, "Optimize Imports"),
                command(editor_keys::AUTO_INDENT, "Auto-Indent Lines"),
                command(editor_keys::TOGGLE_CASE, "Toggle Case"),
                command(editor_keys::INDENT_SELECTION, "Indent Selection"),
                command(editor_keys::UNINDENT, "Unindent Selection"),
                MenuEntry::separator(),
                command(editor_keys::SORT_LINES, "Sort Lines"),
                command(editor_keys::REVERSE_LINES, "Reverse Lines"),
                MenuEntry::submenu(
                    "Convert Indents",
                    vec![
                        command(editor_keys::CONVERT_INDENTS_TO_SPACES, "To Spaces"),
                        command(editor_keys::CONVERT_INDENTS_TO_TABS, "To Tabs"),
                    ],
                ),
                command("editor.toggle_read_only", "Toggle Read-Only Attribute"),
            ],
        );

        let tool_windows = MenuEntry::submenu(
            "Tool Windows",
            vec![
                self.action_row("Project", ToggleProject),
                self.action_row("Commit", ToggleCommit),
                self.action_row("Structure", ToggleStructure),
                self.action_row("Problems", ToggleProblems),
                self.action_row("Services", ToggleServices),
                self.action_row("Usages", ToggleUsages),
                self.action_row("TODO", ToggleTodo),
                self.action_row("Git", ToggleLog),
                MenuEntry::separator(),
                self.action_row("Run", ToggleRun),
                self.action_row("Debug", ToggleDebug),
                self.action_row("Terminal", ToggleTerminal),
                self.action_row("AI Assistant", ToggleAi),
                command("window.show_documentation", "Documentation"),
            ],
        );

        let view = MenuEntry::submenu(
            "View",
            vec![
                tool_windows,
                MenuEntry::submenu(
                    "Appearance",
                    vec![
                        command(editor_keys::TOGGLE_WHITESPACE, "Show Whitespaces"),
                        command(editor_keys::TOGGLE_SOFT_WRAP, "Soft-Wrap"),
                    ],
                ),
                MenuEntry::separator(),
                command(workspace_keys::RECENT_FILES, "Recent Files"),
                command(workspace_keys::RECENT_LOCATIONS, "Recent Locations"),
                command(workspace_keys::BACK, "Back"),
                command(workspace_keys::FORWARD, "Forward"),
                MenuEntry::separator(),
                command(editor_keys::QUICK_DOC, "Quick Documentation"),
                command(editor_keys::PARAMETER_INFO, "Parameter Info"),
                command(editor_keys::EXPRESSION_TYPE, "Type Info"),
                command(editor_keys::ERROR_DESCRIPTION, "Error Description"),
                MenuEntry::separator(),
                self.action_row("Hide Active Tool Window", HideToolWindow),
                self.action_row("Next Tab", TabRight),
                self.action_row("Previous Tab", TabLeft),
            ],
        );

        let bookmarks = MenuEntry::submenu(
            "Bookmarks",
            vec![
                command(editor_keys::TOGGLE_BOOKMARK, "Toggle Bookmark"),
                command(
                    editor_keys::BOOKMARK_MNEMONIC,
                    "Toggle Bookmark with Mnemonic",
                ),
                command(editor_keys::SHOW_BOOKMARKS, "Show Bookmarks"),
            ],
        );

        let navigate = MenuEntry::submenu(
            "Navigate",
            vec![
                command(workspace_keys::GOTO_CLASS, "Class..."),
                command(workspace_keys::GOTO_FILE, "File..."),
                command(workspace_keys::GOTO_SYMBOL, "Symbol..."),
                command(editor_keys::GOTO_LINE, "Line..."),
                MenuEntry::separator(),
                command(workspace_keys::SEARCH_EVERYWHERE, "Search Everywhere"),
                command(workspace_keys::FILE_STRUCTURE, "File Structure"),
                MenuEntry::separator(),
                command(workspace_keys::BACK, "Back"),
                command(workspace_keys::FORWARD, "Forward"),
                command(workspace_keys::RECENT_FILES, "Recent Files"),
                command(workspace_keys::RECENT_LOCATIONS, "Recent Locations"),
                command(editor_keys::LAST_EDIT, "Last Edit Location"),
                MenuEntry::separator(),
                command(editor_keys::GOTO_DECLARATION, "Declaration or Usages"),
                command(editor_keys::GOTO_IMPLEMENTATION, "Implementation(s)"),
                command(editor_keys::GOTO_SUPER, "Super Method"),
                command(editor_keys::TYPE_HIERARCHY, "Type Hierarchy"),
                command(editor_keys::FIND_USAGES, "Find Usages"),
                command(editor_keys::SHOW_USAGES, "Show Usages"),
                MenuEntry::separator(),
                self.action_row("Next Problem", NextProblem),
                self.action_row("Previous Problem", PreviousProblem),
                bookmarks,
                MenuEntry::separator(),
                command(editor_keys::GOTO_TYPE_DECLARATION, "Type Declaration"),
                command(editor_keys::CALL_HIERARCHY, "Call Hierarchy"),
                command(editor_keys::QUICK_DEFINITION, "Quick Definition"),
                command(editor_keys::COPY_REFERENCE, "Copy Reference"),
                command(editor_keys::OVERRIDE_METHODS, "Override Methods..."),
                command(editor_keys::IMPLEMENT_METHODS, "Implement Methods..."),
                command(editor_keys::METHOD_HIERARCHY, "Method Hierarchy"),
            ],
        );

        let code = MenuEntry::submenu(
            "Code",
            vec![
                command(editor_keys::SHOW_ACTIONS, "Show Context Actions"),
                command(editor_keys::GENERATE, "Generate..."),
                command(editor_keys::INSERT_TEMPLATE, "Insert Live Template..."),
                command(editor_keys::SURROUND_WITH, "Surround With..."),
                MenuEntry::submenu(
                    "Code Completion",
                    vec![command(editor_keys::COMPLETE, "Basic")],
                ),
                MenuEntry::separator(),
                command(editor_keys::TOGGLE_COMMENT, "Comment Line"),
                command(editor_keys::BLOCK_COMMENT, "Comment Block"),
                command(editor_keys::REFORMAT, "Reformat Code"),
                command(workspace_keys::SHOW_REFORMAT_DIALOG, "Reformat File..."),
                command(editor_keys::OPTIMIZE_IMPORTS, "Optimize Imports"),
                command(editor_keys::AUTO_INDENT, "Auto-Indent Lines"),
                MenuEntry::separator(),
                command(workspace_keys::INSPECT_CODE, "Inspect Code..."),
                command(workspace_keys::RUN_INSPECTION, "Run Inspection by Name..."),
                command(workspace_keys::CODE_CLEANUP, "Code Cleanup..."),
                MenuEntry::separator(),
                MenuEntry::submenu(
                    "Folding",
                    vec![
                        command(editor_keys::FOLD, "Fold Selection"),
                        command(editor_keys::UNFOLD, "Unfold"),
                        command(editor_keys::FOLD_ALL, "Fold All"),
                        command(editor_keys::UNFOLD_ALL, "Unfold All"),
                    ],
                ),
            ],
        );

        let refactor = MenuEntry::submenu(
            "Refactor",
            vec![
                command(editor_keys::REFACTOR_THIS, "Refactor This..."),
                MenuEntry::separator(),
                command(editor_keys::RENAME, "Rename..."),
                command(editor_keys::CHANGE_SIGNATURE, "Change Signature..."),
                command(editor_keys::MOVE, "Move..."),
                command(editor_keys::INLINE, "Inline..."),
                MenuEntry::separator(),
                MenuEntry::submenu(
                    "Extract",
                    vec![
                        command(editor_keys::EXTRACT_VARIABLE, "Variable..."),
                        command(editor_keys::EXTRACT_METHOD, "Method..."),
                        command(editor_keys::EXTRACT_FIELD, "Field..."),
                        command(editor_keys::EXTRACT_CONSTANT, "Constant..."),
                        command(editor_keys::INTRODUCE_PARAMETER, "Parameter..."),
                    ],
                ),
                command(editor_keys::SAFE_DELETE, "Safe Delete..."),
                command(editor_keys::UNWRAP, "Unwrap/Remove..."),
            ],
        );

        let build = MenuEntry::submenu(
            "Build",
            vec![
                self.action_row("Build Project", BuildProject),
                self.action_row("Compile", Compile),
            ],
        );

        let run = MenuEntry::submenu(
            "Run",
            vec![
                self.action_row("Run", Run),
                self.action_row("Debug", Debug),
                command(workspace_keys::RUN_ANYTHING, "Run Anything"),
                command(workspace_keys::RUN_CLASS, "Run Test at Caret"),
                self.action_row("Rerun Tests", RerunTests),
                command("window.rerun_failed_tests", "Rerun Failed Tests"),
                MenuEntry::separator(),
                self.action_row("Stop", Stop),
                self.action_row("Stop Background Processes", StopBackgroundProcesses),
                MenuEntry::separator(),
                self.action_row("Step Over", StepOver),
                self.action_row("Force Step Over", ForceStepOver),
                self.action_row("Step Into", StepInto),
                self.action_row("Step Out", StepOut),
                self.action_row("Resume", Resume),
                self.action_row("Evaluate Expression...", Evaluate),
                self.action_row("Toggle Breakpoint", ToggleBreakpoint),
                self.action_row("Toggle Temporary Breakpoint", ToggleTemporaryLineBreakpoint),
                self.action_row("View Breakpoints...", ViewBreakpoints),
                MenuEntry::separator(),
                command("debug.attach_jvm", "Attach to JVM..."),
                MenuEntry::separator(),
                command("run.edit_configurations", "Edit Configurations..."),
            ],
        );

        let git = MenuEntry::submenu(
            "Git",
            vec![
                self.action_row("VCS Operations...", VcsQuickList),
                MenuEntry::separator(),
                self.action_row("Commit...", ShowCommit),
                self.action_row("Push...", PushChanges),
                self.action_row("Update Project...", UpdateProject),
                command("git.fetch", "Fetch"),
                self.action_row("Branches...", ShowBranches),
                MenuEntry::separator(),
                command("git.stash", "Stash Changes..."),
                command("git.unstash", "Unstash Changes..."),
                command("git.conflicts", "Conflicts..."),
                command("git.abort", "Abort Merge or Rebase"),
                MenuEntry::separator(),
                command("git.show_file_history", "Show History"),
                command("git.show_log", "Git Log"),
                command(workspace_keys::ROLLBACK_CHANGED_LINES, "Revert Change"),
            ],
        );

        let editor_tabs = MenuEntry::submenu(
            "Editor Tabs",
            vec![
                self.action_row("Next Tab", TabRight),
                self.action_row("Previous Tab", TabLeft),
                self.action_row("Close Tab", CloseTab),
                command("window.close_other_tabs", "Close Other Tabs"),
                command("window.close_all_tabs", "Close All Tabs"),
                command("window.close_unmodified_tabs", "Close Unmodified Tabs"),
                command("window.close_tabs_to_left", "Close Tabs to the Left"),
                command("window.close_tabs_to_right", "Close Tabs to the Right"),
                self.action_row("Pin Tab", PinTab),
                MenuEntry::separator(),
                self.action_row("Split Right", SplitRight),
                self.action_row("Split Down", SplitDown),
                self.action_row("Move to Other Group", MoveToOtherGroup),
                self.action_row("Focus Next Group", FocusNextGroup),
                self.action_row("Focus Previous Group", FocusPreviousGroup),
            ],
        );

        let active_tool = MenuEntry::submenu(
            "Active Tool Window",
            vec![
                self.action_row("Hide", HideToolWindow),
                command("workspace.hide_all_windows", "Hide All"),
            ],
        );

        let window = MenuEntry::submenu(
            "Window",
            vec![
                editor_tabs,
                MenuEntry::separator(),
                command("window.reopen_closed_tab", "Reopen Closed Tab"),
                MenuEntry::separator(),
                active_tool,
                command(
                    "workspace.store_default_layout",
                    "Store Current Layout as Default",
                ),
                self.command_row(
                    &weak,
                    workspace_keys::RESTORE_DEFAULT_LAYOUT,
                    "Restore Default Layout",
                ),
                MenuEntry::separator(),
                command("window.show_performance", "Show Performance Monitor"),
                command("window.toggle_theme", "Toggle Theme"),
            ],
        );

        let help = MenuEntry::submenu(
            "Help",
            vec![
                command(workspace_keys::FIND_ACTION, "Find Action..."),
                MenuEntry::separator(),
                command("app.about", "About Arro"),
                command("app.check_updates", "Check for Updates…"),
                command("app.support", "Support Arro…"),
                command("app.report_problem", "Report a Problem…"),
            ],
        );

        vec![
            file, edit, view, navigate, code, refactor, build, run, git, window, help,
        ]
    }

    /// The width of `text` in the toolbar font.
    fn text_width(window: &Window, text: &str, font_size: Pixels, color: Hsla) -> f32 {
        let run = TextRun {
            len: text.len(),
            font: font(UI_FONT),
            color,
            background_color: None,
            underline: None,
            strikethrough: None,
        };
        let line = window.text_system().shape_line(
            text.to_owned().into(),
            font_size,
            std::slice::from_ref(&run),
            None,
        );
        f32::from(line.width())
    }

    /// How many sections fit next to the project, the branch and the run widget. The
    /// sections that do not fit move to the overflow row at the end of the bar.
    fn menu_bar_visible_count(
        &self,
        window: &Window,
        theme: &Theme,
        sections: &[MenuEntry],
        cx: &mut Context<Self>,
    ) -> usize {
        let scale = cx
            .try_global::<Settings>()
            .map_or(1., |settings| settings.ui_scale);
        let font_size = px(UI_SIZE * scale);
        let widths: Vec<f32> = sections
            .iter()
            .map(|section| {
                Self::text_width(window, &section.label, font_size, theme.text) + MENU_ITEM_PADDING
            })
            .collect();
        let total =
            widths.iter().sum::<f32>() + MENU_ITEM_GAP * widths.len().saturating_sub(1) as f32;
        let state = self.run_state(cx);
        let right =
            TOOLBAR_RIGHT_FIXED + Self::text_width(window, &state.label(), font_size, theme.text);
        let project =
            Self::text_width(window, &self.name, font_size, theme.text) + TOOLBAR_PROJECT_FIXED;
        let channel = Self::text_width(window, self.info().channel_label(), font_size, theme.text)
            + TOOLBAR_CHANNEL_FIXED;
        let branch = self.branch.as_ref().map_or(0., |branch| {
            Self::text_width(window, branch, font_size, theme.text) + TOOLBAR_BRANCH_FIXED
        });
        let available = f32::from(window.viewport_size().width)
            - right
            - project
            - channel
            - branch
            - TOOLBAR_LEFT_FIXED
            - TOOLBAR_SLACK;
        if total <= available {
            return sections.len();
        }
        let mut used = 0.;
        let mut count = 0;
        for width in &widths {
            let next = *width + if count == 0 { 0. } else { MENU_ITEM_GAP };
            if used + next + MENU_ITEM_GAP + MENU_OVERFLOW_WIDTH > available {
                break;
            }
            used += next;
            count += 1;
        }
        count
    }

    /// The labels of the menu bar, with the underlined mnemonic while Alt is held.
    fn menu_bar(&self, window: &Window, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let sections = self.main_menu(cx);
        let visible = self.menu_bar_visible_count(window, theme, &sections, cx);
        self.menu_bar_visible.set(visible);
        let open = self.menu_bar_section();
        let focus = self.menu_bar_focus.get();
        self.menu_bar_bounds.borrow_mut().clear();
        self.menu_bar_overflow.set(None);
        let items: Vec<_> = sections
            .iter()
            .take(visible)
            .enumerate()
            .map(|(index, section)| {
                let selected = open == Some(index) || (open.is_none() && focus == Some(index));
                self.menu_bar_item(index, &section.label, selected, theme, cx)
            })
            .collect();
        let overflow = (visible < sections.len()).then(|| self.menu_bar_overflow_item(theme, cx));
        div()
            .flex()
            .items_center()
            .gap(px(MENU_ITEM_GAP))
            .children(items)
            .children(overflow)
    }

    /// One section of the menu bar: a click opens its submenu, a hover switches to it
    /// while another submenu is open.
    fn menu_bar_item(
        &self,
        index: usize,
        label: &SharedString,
        selected: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let bounds = self.menu_bar_bounds.clone();
        let measure = canvas(
            move |row, _, _| {
                bounds.borrow_mut().insert(index, row);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let hover = theme.hover;
        let label = menu_bar_label(label, index, self.menu_bar_alt.get());
        div()
            .id(ElementId::named_usize("menu-bar", index))
            .debug_selector(move || format!("menu-bar-{index}"))
            .relative()
            .flex()
            .items_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(MENU_ITEM_PADDING / 2.))
            .rounded(px(CONTROL_RADIUS))
            .occlude()
            .when(selected, |this| this.bg(theme.selection))
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(move |this, _: &ClickEvent, window, cx| {
                this.menu_bar_clicked(index, window, cx)
            }))
            .on_hover(cx.listener(move |this, entered, window, cx| {
                this.menu_bar_hovered(index, *entered, window, cx)
            }))
            .child(measure)
            .child(label)
    }

    /// The "…" row of the menu bar with the sections that did not fit.
    fn menu_bar_overflow_item(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let bounds = self.menu_bar_overflow.clone();
        let measure = canvas(move |row, _, _| bounds.set(Some(row)), |_, _, _, _| {})
            .absolute()
            .top_0()
            .left_0()
            .size_full();
        let hover = theme.hover;
        let selected = self.menu_bar_section() == Some(MENU_OVERFLOW);
        div()
            .id("menu-bar-overflow")
            .debug_selector(|| "menu-bar-overflow".to_owned())
            .relative()
            .flex()
            .items_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(MENU_ITEM_PADDING / 2.))
            .rounded(px(CONTROL_RADIUS))
            .occlude()
            .when(selected, |this| this.bg(theme.selection))
            .hover(move |style| style.bg(hover))
            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                this.menu_bar_overflow_clicked(window, cx)
            }))
            .on_hover(cx.listener(|this, entered, window, cx| {
                this.menu_bar_overflow_hovered(*entered, window, cx)
            }))
            .child(measure)
            .child(div().child("…"))
    }

    fn menu_bar_clicked(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu_bar_section() == Some(index) {
            self.close_menu(window, cx);
        } else {
            self.open_menu_bar_section(index, window, cx);
        }
    }

    fn menu_bar_hovered(
        &mut self,
        index: usize,
        entered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if entered && self.menu_bar_section().is_some_and(|open| open != index) {
            self.open_menu_bar_section(index, window, cx);
        }
    }

    fn menu_bar_overflow_clicked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.menu_bar_section() == Some(MENU_OVERFLOW) {
            self.close_menu(window, cx);
        } else {
            self.show_menu_bar_overflow(window, cx);
        }
    }

    fn menu_bar_overflow_hovered(
        &mut self,
        entered: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if entered
            && self
                .menu_bar_section()
                .is_some_and(|open| open != MENU_OVERFLOW)
        {
            self.show_menu_bar_overflow(window, cx);
        }
    }

    /// Where the submenu of a bar item opens: under the item.
    fn menu_bar_position(&self, index: usize) -> Point<Pixels> {
        let under = |bounds: Bounds<Pixels>| {
            let bottom = bounds.bottom();
            let bottom = if bottom < px(TOOLBAR_HEIGHT) {
                px(TOOLBAR_HEIGHT)
            } else {
                bottom
            };
            point(bounds.left(), bottom)
        };
        self.menu_bar_bounds
            .borrow()
            .get(&index)
            .map(|bounds| under(*bounds))
            .or_else(|| self.menu_bar_overflow.get().map(under))
            .unwrap_or_else(|| point(px(8.), px(TOOLBAR_HEIGHT)))
    }

    /// Opens the submenu of a section of the menu bar, under its item when it fitted.
    fn open_menu_bar_section(
        &mut self,
        index: usize,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sections = self.main_menu(cx);
        let Some(section) = sections.get(index) else {
            return;
        };
        let bar = (index < self.menu_bar_visible.get()).then_some(index);
        let position = self.menu_bar_position(index);
        self.show_menu_bar_section(bar, position, section.submenu.clone(), cx);
        self.menu_bar_focus.set(Some(index));
    }

    fn show_menu_bar_overflow(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        let sections = self.main_menu(cx);
        let visible = self.menu_bar_visible.get();
        let hidden: Vec<MenuEntry> = sections
            .iter()
            .skip(visible)
            .map(|section| MenuEntry::submenu(section.label.clone(), section.submenu.clone()))
            .collect();
        let position = self.menu_bar_position(MENU_OVERFLOW);
        self.show_menu_bar_section(Some(MENU_OVERFLOW), position, hidden, cx);
    }

    /// Alt+letter of a section, when the keymap does not take the combination first.
    fn menu_bar_mnemonic(&self, key: &str, alt: bool) -> Option<usize> {
        if !alt {
            return None;
        }
        let key = key.to_ascii_lowercase();
        let index = MENU_MNEMONICS
            .iter()
            .position(|letter| letter.to_ascii_lowercase().to_string() == key)?;
        (!self.alt_letter_is_bound(&key)).then_some(index)
    }

    /// The arrows of the menu bar, both with a highlighted item and with an open
    /// submenu. Returns whether the key belonged to the bar.
    pub(crate) fn menu_bar_key(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        // GPUI reports a lone modifier that went down and up as a key of its own.
        // That is the Alt tap that picks the first section of the bar.
        if key == "alt" {
            if modifiers.alt {
                return false;
            }
            self.menu_bar_alt.set(false);
            if !modifiers.control
                && !modifiers.shift
                && !modifiers.platform
                && self.overlay.is_none()
            {
                self.menu_bar_focus.set(Some(0));
                window.focus(&self.focus_handle, cx);
                cx.notify();
            }
            return true;
        }
        if key == "control"
            || key == "shift"
            || key == "platform"
            || modifiers.platform
            || modifiers.control
            || modifiers.shift
        {
            return false;
        }
        if (self.overlay.is_none() || self.menu_bar_section().is_some())
            && let Some(index) = self.menu_bar_mnemonic(key, modifiers.alt)
        {
            self.open_menu_bar_section(index, window, cx);
            return true;
        }
        if modifiers.alt {
            return false;
        }
        if let Some(open) = self.menu_bar_section() {
            if !self.menu_at_top() {
                return false;
            }
            match key {
                "left" => self.menu_bar_step(open, -1, window, cx),
                "right" => self.menu_bar_step(open, 1, window, cx),
                _ => return false,
            }
            return true;
        }
        if self.overlay.is_some() {
            return false;
        }
        let Some(focus) = self.menu_bar_focus.get() else {
            return false;
        };
        match key {
            "left" => self.menu_bar_move(-1, cx),
            "right" => self.menu_bar_move(1, cx),
            "down" | "enter" => self.open_menu_bar_section(focus, window, cx),
            "escape" => {
                self.menu_bar_focus.set(None);
                self.focus_active(window, cx);
                cx.notify();
            }
            _ => {
                self.menu_bar_focus.set(None);
                self.focus_active(window, cx);
                cx.notify();
                return false;
            }
        }
        true
    }

    fn menu_at_top(&self) -> bool {
        matches!(&self.overlay, Some(Overlay::Menu { state, .. }) if state.open_path().is_empty())
    }

    fn menu_bar_move(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.menu_bar_visible.get();
        if count == 0 {
            return;
        }
        let current = self.menu_bar_focus.get().unwrap_or(0).min(count - 1);
        let next = (current as isize + delta).rem_euclid(count as isize) as usize;
        self.menu_bar_focus.set(Some(next));
        cx.notify();
    }

    fn menu_bar_step(
        &mut self,
        from: usize,
        delta: isize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let count = self.menu_bar_visible.get();
        if count == 0 {
            return;
        }
        let next = if from == MENU_OVERFLOW {
            if delta > 0 { 0 } else { count - 1 }
        } else {
            (from as isize + delta).rem_euclid(count as isize) as usize
        };
        self.open_menu_bar_section(next, window, cx);
    }

    /// Opens the first section, as Find Action and the old main menu command do.
    pub fn show_main_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let sections = self.main_menu(cx);
        let Some(first) = sections.into_iter().next() else {
            return;
        };
        self.show_menu_bar_section(Some(0), position, first.submenu, cx);
    }

    fn show_format_menu(
        &mut self,
        names: &'static [&'static str],
        anchor: Option<Bounds<Pixels>>,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.weak_entity();
        let entries = names.iter().map(|name| {
            let weak = weak.clone();
            MenuEntry::new(*name, move |_, cx| {
                weak.update(cx, |this, cx| this.set_active_format(name, cx))
                    .ok();
            })
        });
        let entries = entries.collect();
        match anchor {
            Some(anchor) => self.show_menu_under(anchor, entries, window, cx),
            None => self.show_menu(position, entries, cx),
        }
    }

    /// The menu of the indentation in the status bar: the settings of the project, the
    /// unit of this file and its size until the tab closes.
    fn show_indent_menu(
        &mut self,
        placement: MenuPlacement,
        tabs: bool,
        size: usize,
        language: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let weak = cx.weak_entity();
        let checked = |on: bool, label: &str| {
            if on {
                format!("✓ {label}")
            } else {
                label.to_owned()
            }
        };
        let configure = match language.filter(|name| !name.is_empty()) {
            Some(name) => format!("Configure Indents for {name}"),
            None => "Configure Indents...".to_owned(),
        };
        let rows = vec![
            MenuEntry::new(configure, |window, cx| {
                window.dispatch_action(Box::new(ShowSettings), cx)
            }),
            MenuEntry::separator(),
            MenuEntry::new(checked(tabs, "Indent with Tab"), {
                let weak = weak.clone();
                move |_, cx| {
                    weak.update(cx, |this, cx| {
                        this.set_active_indent(IndentChoice::Tabs, cx)
                    })
                    .ok();
                }
            }),
            MenuEntry::new(checked(!tabs, "Indent with Spaces"), {
                let weak = weak.clone();
                move |_, cx| {
                    weak.update(cx, |this, cx| {
                        this.set_active_indent(IndentChoice::Spaces, cx)
                    })
                    .ok();
                }
            }),
            MenuEntry::separator(),
            MenuEntry::new(checked(size == 2, "Size 2"), {
                let weak = weak.clone();
                move |_, cx| {
                    weak.update(cx, |this, cx| {
                        this.set_active_indent(IndentChoice::Size(2), cx)
                    })
                    .ok();
                }
            }),
            MenuEntry::new(checked(size == 4, "Size 4"), {
                let weak = weak.clone();
                move |_, cx| {
                    weak.update(cx, |this, cx| {
                        this.set_active_indent(IndentChoice::Size(4), cx)
                    })
                    .ok();
                }
            }),
            MenuEntry::new(checked(size == 8, "Size 8"), {
                let weak = weak.clone();
                move |_, cx| {
                    weak.update(cx, |this, cx| {
                        this.set_active_indent(IndentChoice::Size(8), cx)
                    })
                    .ok();
                }
            }),
        ];
        match placement.anchor {
            Some(anchor) => self.show_menu_under(anchor, rows, window, cx),
            None => self.show_menu(placement.position, rows, cx),
        }
    }

    fn welcome(&self, theme: &Theme) -> impl IntoElement + use<> {
        let line = |label: &'static str, shortcut: &'static str| {
            div()
                .flex()
                .justify_between()
                .w(px(260.))
                .child(label)
                .child(div().text_color(theme.link).child(shortcut))
        };
        div()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap(px(8.))
            .size_full()
            .text_color(theme.text_secondary)
            .child(line("Search Everywhere", "Double Shift"))
            .child(line("Project View", "Alt+1"))
            .child(line("Terminal", "Alt+F12"))
    }

    fn pane_view(
        &self,
        pane: usize,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let tab = self.panes[pane].active_tab();
        let selection = theme.selection;
        let body = div()
            .id(("editor-body", pane))
            .relative()
            .flex_1()
            .min_h_0()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if this.active_pane != pane {
                        this.active_pane = pane;
                        cx.notify();
                    }
                }),
            )
            .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(selection))
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                let end = this.panes[pane].tabs.len();
                let from = (dragged.pane, dragged.index);
                this.move_tab(from, (pane, end), window, cx)
            }));
        let body = match tab {
            Some(tab) => body
                .child(
                    div()
                        .id(("editor-view", pane))
                        .debug_selector(move || format!("editor-body-{pane}"))
                        .size_full()
                        .child(tab.editor.view.clone())
                        .with_animation(
                            ("tab-fade", self.tab_serial),
                            Animation::new(Duration::from_millis(motion::TAB))
                                .with_easing(ease_in_out),
                            |this, delta| this.opacity(0.4 + 0.6 * delta),
                        ),
                )
                .into_any_element(),
            None => body.child(self.welcome(theme)).into_any_element(),
        };
        let down = self.split_down;
        island(theme)
            .flex_1()
            .min_w_0()
            .min_h_0()
            .when(down, |this| this.w_full())
            .when(!down, |this| this.h_full())
            .bg(theme.editor_bg)
            .child(self.tab_bar(pane, theme, window, cx))
            .child(body)
    }

    fn editor_area(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let panes: Vec<_> = (0..self.panes.len())
            .map(|pane| self.pane_view(pane, theme, window, cx))
            .collect();
        div()
            .flex()
            .when(self.split_down, |this| this.flex_col())
            .flex_1()
            .min_w_0()
            .h_full()
            .gap(px(ISLAND_GAP))
            .children(panes)
    }

    /// Thin area over the gap next to a dock, dragging it resizes the dock.
    fn edge_grip(&self, side: DockSide) -> impl IntoElement {
        let id: (&'static str, usize) = ("dock-edge", side as usize);
        let grip = div()
            .id(id)
            .absolute()
            .occlude()
            .on_drag(DraggedEdge(side), |_, _, _, cx| cx.new(|_| NoDragPreview));
        match side {
            DockSide::Left => grip
                .top_0()
                .bottom_0()
                .right_0()
                .w(px(EDGE_GRIP))
                .cursor_col_resize(),
            DockSide::Right => grip
                .top_0()
                .bottom_0()
                .left_0()
                .w(px(EDGE_GRIP))
                .cursor_col_resize(),
            DockSide::Bottom => grip
                .left_0()
                .right_0()
                .top_0()
                .h(px(EDGE_GRIP))
                .cursor_row_resize(),
        }
    }

    fn dock_body(&self, side: DockSide, theme: &Theme) -> AnyElement {
        match self.dock(side).active_tool() {
            Some(tool) => tool.view.clone().into_any_element(),
            None => island(theme).size_full().into_any_element(),
        }
    }

    fn status_bar(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut path = div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_1()
            .min_w_0()
            .overflow_hidden();
        path = path.child(crumb(
            0usize,
            Some("nodes/module"),
            self.name.clone(),
            theme,
        ));
        if let Some(tab) = self.active_tab() {
            let parts = path_parts(&self.root, &tab.path);
            let count = parts.len();
            for (index, part) in parts.into_iter().enumerate() {
                let leading = (index + 1 == count).then_some(tab.icon);
                path = path.child(crumb_separator(theme)).child(crumb(
                    index + 1,
                    leading,
                    part,
                    theme,
                ));
            }
        }
        let language = self.language_status.clone().map(|text| {
            let widget = status_widget("st-language", text, theme)
                .debug_selector(|| "st-language".to_owned());
            if self.language_details.is_some() {
                widget
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| this.show_language_help(cx)))
            } else {
                widget
            }
        });
        let branch = self.branch.clone().map(|branch| {
            status_widget("st-branch", branch, theme)
                .relative()
                .child(self.st_branch_anchor.canvas())
                .on_click(cx.listener(|this, _, window, cx| {
                    if let Some(anchor) = this.st_branch_anchor.bounds() {
                        this.set_action_menu_anchor(anchor);
                    }
                    window.dispatch_action(Box::new(ShowBranches), cx);
                }))
                .tooltip(|_, cx| tooltip("Git Branches", None, cx))
                .tooltip_show_delay(ui_kit::tooltip_show_delay())
        });
        let editor = self.active_status(cx).map(|status| {
            let (indent_tabs, indent_size) = (status.indent_tabs, status.indent_size);
            let language_name = status.language.clone();
            let read_only = status.read_only;
            let eol = status_widget("st-eol", status.eol, theme)
                .debug_selector(|| "st-eol".to_owned())
                .relative()
                .child(self.st_eol_anchor.canvas())
                .tooltip(|_, cx| tooltip("Line Separator", None, cx))
                .tooltip_show_delay(ui_kit::tooltip_show_delay())
                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                    let anchor = this.st_eol_anchor.bounds();
                    this.show_format_menu(&LINE_ENDS, anchor, event.position(), window, cx)
                }));
            let encoding = status_widget("st-enc", status.encoding, theme)
                .debug_selector(|| "st-enc".to_owned())
                .relative()
                .child(self.st_enc_anchor.canvas())
                .tooltip(|_, cx| tooltip("File Encoding", None, cx))
                .tooltip_show_delay(ui_kit::tooltip_show_delay())
                .on_click(cx.listener(|this, event: &ClickEvent, window, cx| {
                    let anchor = this.st_enc_anchor.bounds();
                    this.show_format_menu(&ENCODINGS, anchor, event.position(), window, cx)
                }));
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_L))
                .child(
                    status_widget("st-caret", status.caret, theme)
                        .debug_selector(|| "st-caret".to_owned()),
                )
                .child(eol)
                .child(encoding)
                .child(
                    status_widget("st-indent", status.indent, theme)
                        .debug_selector(|| "st-indent".to_owned())
                        .relative()
                        .child(self.st_indent_anchor.canvas())
                        .tooltip(|_, cx| tooltip("Indent", None, cx))
                        .tooltip_show_delay(ui_kit::tooltip_show_delay())
                        .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                            let placement = MenuPlacement {
                                anchor: this.st_indent_anchor.bounds(),
                                position: event.position(),
                            };
                            this.show_indent_menu(
                                placement,
                                indent_tabs,
                                indent_size,
                                language_name.as_ref().map(|name| name.to_string()),
                                window,
                                cx,
                            )
                        })),
                )
                .child(
                    status_widget("st-insert", status.insert, theme)
                        .debug_selector(|| "st-insert".to_owned()),
                )
                .children(read_only.then(|| {
                    status_widget("st-readonly", "Read-only", theme)
                        .debug_selector(|| "st-readonly".to_owned())
                        .cursor_pointer()
                        .tooltip(|_, cx| tooltip("Read-Only Attribute", None, cx))
                        .tooltip_show_delay(ui_kit::tooltip_show_delay())
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.run_command("editor.toggle_read_only", window, cx);
                        }))
                }))
        });
        let widgets = div()
            .flex()
            .items_center()
            .gap(px(SPACE_L))
            .children(language)
            .children(editor);
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(STATUS_HEIGHT))
            .gap(px(SPACE_L))
            .px(px(SPACE_M))
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_muted)
            .child(path)
            .children(branch)
            .child(widgets)
    }

    fn overlay_element(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        match self.overlay.as_ref()? {
            Overlay::View { view, .. } => Some(
                deferred(view.clone().into_any_element())
                    .with_priority(1)
                    .into_any_element(),
            ),
            Overlay::Menu {
                position,
                entries,
                state,
                geometry,
                bar,
                ..
            } => {
                let weak = cx.weak_entity();
                let close_weak = weak.clone();
                let close = move |window: &mut Window, cx: &mut App| {
                    close_weak
                        .update(cx, |this, cx| this.close_menu(window, cx))
                        .ok();
                };
                let hover_weak = weak.clone();
                let on_hover: MenuHover = Rc::new(move |path, entered, _, cx| {
                    hover_weak
                        .update(cx, |this, cx| this.menu_hover(path, entered, cx))
                        .ok();
                });
                let open_weak = weak;
                let on_open: MenuOpen = Rc::new(move |path, _, cx| {
                    open_weak
                        .update(cx, |this, cx| this.menu_open(path, cx))
                        .ok();
                });
                let callbacks = MenuCallbacks {
                    on_pick: Rc::new(close),
                    on_hover,
                    on_open,
                };
                let body = menu(
                    entries,
                    state,
                    geometry,
                    &callbacks,
                    window.viewport_size(),
                    theme,
                );
                let backdrop = deferred(
                    div()
                        .id("menu-backdrop")
                        .absolute()
                        .top(if bar.is_some() {
                            px(TOOLBAR_HEIGHT)
                        } else {
                            px(0.)
                        })
                        .left_0()
                        .right_0()
                        .bottom_0()
                        .occlude()
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
                        )
                        .on_mouse_down(
                            MouseButton::Right,
                            cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
                        )
                        .on_mouse_down(
                            MouseButton::Middle,
                            cx.listener(|this, _, window, cx| this.close_menu(window, cx)),
                        ),
                )
                .with_priority(1);
                let popup = deferred(
                    anchored()
                        .position(*position)
                        .snap_to_window_with_margin(px(SPACE_M))
                        .child(enter(("menu-enter", self.overlay_serial), body)),
                )
                .with_priority(2);
                Some(
                    div()
                        .absolute()
                        .size_full()
                        .child(backdrop)
                        .child(popup)
                        .into_any_element(),
                )
            }
        }
    }

    fn balloon_element(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.balloon_visible {
            return None;
        }
        let (title, text) = match &self.balloon {
            Some(balloon) => (balloon.title.clone(), balloon.text.clone()),
            None => ("Notifications".into(), "Nothing new.".into()),
        };
        let close = cx.listener(|this, _: &ClickEvent, _, cx| this.toggle_balloon(cx));
        let mut actions = Vec::new();
        if let Some((label, path)) = self
            .balloon
            .as_ref()
            .and_then(|balloon| balloon.action.clone())
        {
            actions.push(NotificationAction {
                id: "file",
                label,
                on_click: Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.balloon_visible = false;
                    cx.notify();
                    this.open_file(path.clone(), window, cx);
                })),
            });
        }
        if let Some(balloon) = self.balloon.as_ref() {
            for button in &balloon.buttons {
                let id = button.id;
                let label = button.label.clone();
                let on_click: NotificationClick = match button.action.clone() {
                    BalloonAction::Url(url) => Box::new(cx.listener(move |_, _, _, _| {
                        open_url(&url);
                    })),
                    BalloonAction::InstallUpdate => {
                        Box::new(cx.listener(|this, _: &ClickEvent, _, cx| {
                            this.balloon_visible = false;
                            cx.notify();
                            this.install_update(cx);
                        }))
                    }
                    BalloonAction::SkipUpdate => {
                        Box::new(cx.listener(|this, _: &ClickEvent, _, cx| this.skip_update(cx)))
                    }
                    BalloonAction::Command(id) => {
                        Box::new(cx.listener(move |this, _: &ClickEvent, window, cx| {
                            this.balloon_visible = false;
                            cx.notify();
                            this.run_command(id, window, cx);
                        }))
                    }
                };
                actions.push(NotificationAction {
                    id,
                    label,
                    on_click,
                });
            }
        }
        let body = notification(title, text, actions, close, theme);
        Some(
            deferred(
                div()
                    .absolute()
                    .right(px(STRIPE_WIDTH + SPACE_L))
                    .bottom(px(STATUS_HEIGHT + SPACE_L))
                    .child(enter(("notification-enter", self.balloon_serial), body)),
            )
            .into_any_element(),
        )
    }
}

impl Render for Workspace {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.step_animations(cx) {
            window.request_animation_frame();
        }
        let theme = *cx.global::<Theme>();
        let gap = px(ISLAND_GAP);
        let scale = cx
            .try_global::<Settings>()
            .map_or(1., |settings| settings.ui_scale);
        window.set_rem_size(px(16. * scale));
        let viewport = window.viewport_size();
        let (left_panel, right_panel, bottom_panel) = match self.maximized_tool_window() {
            Some(DockSide::Left) => (
                f32::from(viewport.width) - STRIPE_WIDTH - RIGHT_STRIPE_WIDTH,
                0.,
                0.,
            ),
            Some(DockSide::Right) => (
                0.,
                f32::from(viewport.width) - STRIPE_WIDTH - RIGHT_STRIPE_WIDTH,
                0.,
            ),
            Some(DockSide::Bottom) => (
                0.,
                0.,
                f32::from(viewport.height) - TOOLBAR_HEIGHT - STATUS_HEIGHT,
            ),
            None => (self.left.size, self.right.size, self.bottom.size),
        };
        let upper = div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .w(px(left_panel * self.left.slide.value))
                    .overflow_hidden()
                    .child(
                        div()
                            .w(px(left_panel - ISLAND_GAP))
                            .h_full()
                            .child(self.dock_body(DockSide::Left, &theme)),
                    )
                    .child(self.edge_grip(DockSide::Left)),
            )
            .child(self.editor_area(&theme, window, cx))
            .child(
                div()
                    .relative()
                    .flex_none()
                    .h_full()
                    .w(px(right_panel * self.right.slide.value))
                    .overflow_hidden()
                    .child(
                        div()
                            .w(px(right_panel))
                            .h_full()
                            .pl(gap)
                            .child(self.dock_body(DockSide::Right, &theme)),
                    )
                    .child(self.edge_grip(DockSide::Right)),
            );
        let lower = div()
            .relative()
            .flex_none()
            .w_full()
            .h(px(bottom_panel * self.bottom.slide.value))
            .overflow_hidden()
            .child(
                div()
                    .h(px(bottom_panel))
                    .pt(gap)
                    .child(self.dock_body(DockSide::Bottom, &theme)),
            )
            .child(self.edge_grip(DockSide::Bottom));

        let key_context = if matches!(self.overlay, Some(Overlay::Menu { .. })) {
            "Menu"
        } else {
            "Workspace"
        };
        div()
            .id("workspace")
            .key_context(key_context)
            .track_focus(&self.focus_handle)
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.main_bg)
            .text_color(theme.text)
            .font_family(UI_FONT)
            .text_size(px(UI_SIZE * scale))
            .on_action(cx.listener(|this, _: &ToggleTheme, _, cx| this.toggle_theme(cx)))
            .on_action(cx.listener(|this, _: &ToggleProject, window, cx| {
                this.toggle_tool(PROJECT_TOOL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleTerminal, window, cx| {
                this.toggle_tool(TERMINAL_TOOL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleServices, window, cx| {
                this.toggle_tool(SERVICES_TOOL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleCommit, window, cx| {
                this.toggle_tool(COMMIT_TOOL, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleLog, window, cx| {
                    this.toggle_tool(LOG_TOOL, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ToggleStructure, window, cx| {
                this.toggle_tool(STRUCTURE_TOOL, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ToggleRun, window, cx| {
                    this.toggle_tool(RUN_TOOL, window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &ToggleDebug, window, cx| {
                this.toggle_tool(DEBUG_TOOL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &Debug, window, cx| this.debug_selected(window, cx)))
            .on_action(cx.listener(|this, _: &StepOver, window, cx| this.step_over(window, cx)))
            .on_action(cx.listener(|this, _: &StepInto, window, cx| this.step_into(window, cx)))
            .on_action(
                cx.listener(|this, _: &ForceStepInto, window, cx| this.force_step_into(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &SmartStepInto, window, cx| this.smart_step_into(window, cx)),
            )
            .on_action(cx.listener(|this, _: &StepOut, window, cx| this.step_out(window, cx)))
            .on_action(cx.listener(|this, _: &Resume, window, cx| this.resume_debug(window, cx)))
            .on_action(cx.listener(|this, _: &RunToCursor, window, cx| {
                this.run_to_cursor(false, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ForceRunToCursor, window, cx| {
                this.run_to_cursor(true, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ShowExecutionPoint, window, cx| {
                this.show_execution_point(window, cx)
            }))
            .on_action(cx.listener(|this, _: &Rerun, window, cx| this.rerun(window, cx)))
            .on_action(cx.listener(|this, _: &ChooseRunConfiguration, window, cx| {
                this.choose_config(false, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ChooseDebugConfiguration, window, cx| {
                    this.choose_config(true, window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &Evaluate, window, cx| this.evaluate_debug(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &QuickEvaluate, window, cx| this.quick_evaluate(window, cx)),
            )
            .on_action(cx.listener(|this, _: &ToggleBreakpoint, window, cx| {
                this.toggle_breakpoint(window, cx)
            }))
            .on_action(cx.listener(|this, _: &ViewBreakpoints, window, cx| {
                this.view_breakpoints(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &EditBreakpoint, window, cx| {
                    this.edit_breakpoint(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &ForceStepOver, window, cx| this.force_step_over(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleTemporaryLineBreakpoint, window, cx| {
                    this.toggle_temporary_breakpoint(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &Run, window, cx| this.run_selected(window, cx)))
            .on_action(
                cx.listener(|this, _: &BuildProject, window, cx| this.build_project(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Compile, window, cx| {
                this.run_command("workspace.compile", window, cx);
            }))
            .on_action(cx.listener(|this, _: &RerunTests, window, cx| this.rerun_tests(window, cx)))
            .on_action(
                cx.listener(|this, _: &StopBackgroundProcesses, window, cx| {
                    this.stop_background(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &Synchronize, window, cx| {
                this.run_command("file.reload_all", window, cx);
            }))
            .on_action(cx.listener(|this, _: &Stop, window, cx| this.stop_run(window, cx)))
            .on_action(
                cx.listener(|this, _: &ToggleAi, window, cx| this.toggle_tool(AI_TOOL, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowSearch, window, cx| this.show_search(window, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(
                cx.listener(|this, _: &HideToolWindow, window, cx| {
                    this.hide_active_tool(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &NextTab, window, cx| this.next_tab(window, cx)))
            .on_action(
                cx.listener(|this, _: &CloseTab, window, cx| this.close_active_tab(window, cx)),
            )
            .on_action(cx.listener(|this, _: &SplitRight, window, cx| this.split_right(window, cx)))
            .on_action(cx.listener(|this, _: &SplitDown, window, cx| this.split_down(window, cx)))
            .on_action(cx.listener(|this, _: &ToggleProblems, window, cx| {
                this.toggle_tool(PROBLEMS_TOOL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleUsages, window, cx| {
                this.toggle_tool(USAGES_TOOL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleTodo, window, cx| {
                this.toggle_tool(TODO_TOOL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &MoveToOtherGroup, window, cx| {
                this.move_to_other_group(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &FocusNextGroup, window, cx| this.focus_group(1, window, cx)),
            )
            .on_action(cx.listener(|this, _: &FocusPreviousGroup, window, cx| {
                this.focus_group(-1, window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &ReopenTab, window, cx| this.reopen_closed_tab(window, cx)),
            )
            .on_action(cx.listener(|this, _: &TabLeft, window, cx| this.previous_tab(window, cx)))
            .on_action(cx.listener(|this, _: &TabRight, window, cx| this.next_tab(window, cx)))
            .on_action(cx.listener(|this, _: &PinTab, window, cx| this.pin_active_tab(window, cx)))
            .on_action(cx.listener(|this, _: &SaveAll, _, cx| this.save_all(cx)))
            .on_action(
                cx.listener(|this, _: &OpenFolder, window, cx| this.choose_project(window, cx)),
            )
            .on_action(cx.listener(|this, _: &NewElementSamePlace, window, cx| {
                this.run_command("workspace.new_element_same_place", window, cx);
            }))
            .on_action(cx.listener(|this, _: &CopyElement, window, cx| {
                this.run_command("workspace.copy_element", window, cx);
            }))
            .on_action(cx.listener(|this, _: &GotoRelated, window, cx| {
                this.run_command("workspace.goto_related", window, cx);
            }))
            .on_action(cx.listener(|this, _: &NextProjectWindow, window, cx| {
                this.activate_project_window(1, window, cx)
            }))
            .on_action(cx.listener(|this, _: &PreviousProjectWindow, window, cx| {
                this.activate_project_window(-1, window, cx)
            }))
            .on_modifiers_changed(
                cx.listener(|this, event: &ModifiersChangedEvent, window, cx| {
                    this.modifiers_changed(event.modifiers, window, cx)
                }),
            )
            .capture_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.note_key(event);
                if this.menu_bar_key(event, window, cx) || this.menu_key(event, window, cx) {
                    cx.stop_propagation();
                }
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, window, cx| {
                this.key_down(event, window, cx)
            }))
            .on_drag_move(
                cx.listener(|this, event: &DragMoveEvent<DraggedEdge>, window, cx| {
                    let side = event.drag(cx).0;
                    this.resize_dock(side, event.event.position, window, cx);
                }),
            )
            .child(self.toolbar(window, &theme, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(self.left_stripe(&theme, cx))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(upper)
                            .child(lower),
                    )
                    .child(self.right_stripe(&theme, cx)),
            )
            .child(self.status_bar(&theme, cx))
            .children(self.overlay_element(&theme, window, cx))
            .children(self.balloon_element(&theme, cx))
    }
}

#[cfg(test)]
mod tests {
    use super::path_parts;
    use std::path::Path;

    #[test]
    fn a_recent_project_shows_its_name_and_folder() {
        let label = super::recent_label(Path::new("C:/work/shop"));
        assert_eq!(label, "shop  C:/work/shop");
    }

    #[test]
    fn splits_a_path_under_the_root() {
        let root = Path::new("C:/project");
        let path = Path::new("C:/project/src/App.txt");
        assert_eq!(path_parts(root, path), ["src", "App.txt"]);
    }
}

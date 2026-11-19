//! What the main window looks like: the layout of the S10 mock on real state.

use std::cell::RefCell;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::Duration;

use ::platform::open_url;
use keymap::{editor as editor_keys, workspace as workspace_keys};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::appearance::{
    TOGGLE_BREADCRUMBS, TOGGLE_DISTRACTION_FREE, TOGGLE_FULL_SCREEN, TOGGLE_GUTTER_ICONS,
    TOGGLE_INDENT_GUIDES, TOGGLE_LINE_NUMBERS, TOGGLE_NAVIGATION_BAR, TOGGLE_PRESENTATION_MODE,
    TOGGLE_STATUS_BAR, TOGGLE_STICKY_LINES, TOGGLE_TOOL_WINDOW_BARS, TOGGLE_TOOLBAR,
    TOGGLE_ZEN_MODE, WindowMode, checked_label,
};
use crate::editor::{ENCODINGS, IndentChoice, LINE_ENDS};
use crate::settings::Settings;
use crate::split::SplitNode;
use crate::window::{
    AI_CLIENTS, AI_TOOL, AboutApp, BOOKMARKS_POPUP, BalloonAction, BringAllToFront, BuildProject,
    COMMIT_TOOL, CheckForUpdatesApp, ChooseDebugConfiguration, ChooseRunConfiguration, CloseTab,
    Compile, CopyElement, DATABASE_TOOL, DEBUG_TOOL, Debug, Dismiss, DockSide, EditBreakpoint,
    Evaluate, FocusNextGroup, FocusPreviousGroup, ForceRunToCursor, ForceStepInto, ForceStepOver,
    FullyExpandTreeNode, GotoRelated, HTTP_TOOL, HideApp, HideCoverage, HideOtherApps,
    HideToolWindow, LOG_TOOL, MenuRow, MinimizeWindow, MoveToOtherGroup, NewElementSamePlace,
    NextProblem, NextProjectWindow, NextTab, OpenFolder, Overlay, PROBLEMS_TOOL, PROJECT_TOOL,
    PinTab, PreviousProblem, PreviousProjectWindow, ProjectStructure, PushChanges, QuickEvaluate,
    QuitApp, RUN_TOOL, ReopenTab, Rerun, RerunTests, ResizeStep, ResizeToolWindowDown,
    ResizeToolWindowLeft, ResizeToolWindowRight, ResizeToolWindowUp, Resume, Run,
    RunTestsWithCoverage, RunTestsWithProfiler, RunToCursor, RunWithCoverage, SERVICES_TOOL,
    STRUCTURE_TOOL, SaveAll, ShowAllApps, ShowBranches, ShowCommit, ShowExecutionPoint, ShowSearch,
    ShowSettings, SmartStepInto, SplitDown, SplitRight, StepInto, StepOut, StepOver, Stop,
    StopBackgroundProcesses, Synchronize, TASKS_TOOL, TERMINAL_TOOL, TabLeft, TabRight, ToggleAi,
    ToggleBreakpoint, ToggleCommit, ToggleCoverage, ToggleDatabase, ToggleDebug, ToggleFullScreen,
    ToggleLog, ToggleProject, ToggleRun, ToggleServices, ToggleStructure,
    ToggleTemporaryLineBreakpoint, ToggleTerminal, ToggleTheme, ToggleUsages, USAGES_TOOL,
    UpdateProject, VcsQuickList, ViewBreakpoints, Workspace, ZoomWindow,
};
use crate::window::{
    ENDPOINTS_TOOL, RunWithProfiler, TODO_TOOL, ToggleEndpoints, ToggleHttp, ToggleProblems,
    ToggleTodo,
};

/// Wider than the gap itself so that the edge is easy to catch.
const EDGE_GRIP: f32 = 6.;
/// The smallest share of a split that a drag leaves to either part.
const SPLIT_LIMIT: f32 = 0.1;
/// The border of a window that draws its own title bar, where a press resizes the
/// window: the width of the border of the system windows of Ubuntu.
const WINDOW_RESIZE_BORDER: f32 = 6.;
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
/// What the left side of the toolbar keeps for the traffic lights of a macOS window.
const TRAFFIC_LIGHTS_ROOM: f32 = 62.;

/// Whether the window draws the menu bar and the window buttons itself. macOS keeps both in
/// the system: the menu in the menu bar of the screen, the buttons as the traffic lights.
/// The test platform of GPUI has neither, so a test window keeps the in-window chrome and
/// can press it.
fn own_chrome() -> bool {
    !cfg!(target_os = "macos") || cfg!(feature = "test-support")
}

/// The edge of the window a press at `position` resizes: the border that a window drawing
/// its own title bar gives to the edges and corners of its frame. A maximized window has
/// no border.
fn resize_edge_at(
    position: Point<Pixels>,
    size: Size<Pixels>,
    maximized: bool,
) -> Option<ResizeEdge> {
    if maximized {
        return None;
    }
    let border = px(WINDOW_RESIZE_BORDER);
    let left = position.x < border;
    let right = position.x >= size.width - border;
    let top = position.y < border;
    let bottom = position.y >= size.height - border;
    match (top, bottom, left, right) {
        (true, _, true, _) => Some(ResizeEdge::TopLeft),
        (true, _, _, true) => Some(ResizeEdge::TopRight),
        (true, _, _, _) => Some(ResizeEdge::Top),
        (_, true, true, _) => Some(ResizeEdge::BottomLeft),
        (_, true, _, true) => Some(ResizeEdge::BottomRight),
        (_, true, _, _) => Some(ResizeEdge::Bottom),
        (_, _, true, _) => Some(ResizeEdge::Left),
        (_, _, _, true) => Some(ResizeEdge::Right),
        _ => None,
    }
}

/// Maximizes the window, or gives back the size it had before. macOS follows the system
/// setting of a double click on the title bar, so a window there can minimize instead or
/// do nothing. The test platform changes no window and only records the request.
#[cfg(any(test, feature = "test-support"))]
fn toggle_window_zoom(_window: &mut Window, cx: &mut App) {
    let count = cx.default_global::<crate::window::WindowZoomRequests>();
    count.0.set(count.0.get() + 1);
}

#[cfg(not(any(test, feature = "test-support")))]
fn toggle_window_zoom(window: &mut Window, _cx: &mut App) {
    #[cfg(target_os = "macos")]
    window.titlebar_double_click();
    #[cfg(not(target_os = "macos"))]
    window.zoom_window();
}

#[derive(Clone, Copy)]
struct DraggedEdge(DockSide);

/// The divider of a split of the editor area, which drags the share of its two parts.
/// `path` names the split: every step is the child taken at the split above it.
struct DraggedSplit {
    path: Vec<usize>,
}

/// The path of the child `child` of the split at `path`.
fn child_path(path: &[usize], child: usize) -> Vec<usize> {
    let mut child_path = path.to_vec();
    child_path.push(child);
    child_path
}

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

/// Windows answers the hit test of the window buttons itself; a Unix window with client
/// decorations needs a handler of its own for each control.
#[cfg(unix)]
fn minimize_button(button: Stateful<Div>, _cx: &mut Context<Workspace>) -> Stateful<Div> {
    button.on_click(|_: &ClickEvent, window: &mut Window, _: &mut App| window.minimize_window())
}

/// The area of the platform is what the system finds in the frame it drew last. A
/// press that reaches the client area instead, as after a resize a stale area can, is
/// taken by the button itself; stopping the event keeps the system from doing the work
/// a second time when it did find the area.
#[cfg(windows)]
fn minimize_button(button: Stateful<Div>, _cx: &mut Context<Workspace>) -> Stateful<Div> {
    button.on_click(|_: &ClickEvent, window: &mut Window, cx: &mut App| {
        cx.stop_propagation();
        window.minimize_window();
    })
}

#[cfg(unix)]
fn maximize_button(button: Stateful<Div>, _cx: &mut Context<Workspace>) -> Stateful<Div> {
    button.on_click(|_: &ClickEvent, window: &mut Window, _: &mut App| window.zoom_window())
}

#[cfg(windows)]
fn maximize_button(button: Stateful<Div>, _cx: &mut Context<Workspace>) -> Stateful<Div> {
    button.on_click(|_: &ClickEvent, window: &mut Window, cx: &mut App| {
        cx.stop_propagation();
        toggle_window_zoom(window, cx);
    })
}

#[cfg(unix)]
fn close_button(button: Stateful<Div>, cx: &mut Context<Workspace>) -> Stateful<Div> {
    button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
        this.close_window(window, cx);
    }))
}

/// The close area of the caption closes the window from the system, but the platform
/// reads that area from the frame it drew last: on a frame that only the editor redrew
/// the toolbar has no area, the press reaches the client area and nothing closes the
/// window. The button takes its own click, as on Unix. Stopping the event keeps the
/// system from closing the window a second time when it did find the area.
#[cfg(windows)]
fn close_button(button: Stateful<Div>, cx: &mut Context<Workspace>) -> Stateful<Div> {
    button.on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
        cx.stop_propagation();
        this.close_window(window, cx);
    }))
}

/// The areas the caption drew last: the three window buttons and the empty space that
/// moves the window. The platform asks for them in the frame it drew last, and the title
/// bar is a cached view, so most frames have none of them. The window root draws every
/// frame and gives the platform the areas the caption left here.
#[derive(Clone, Default)]
pub(crate) struct WindowControlAreas {
    bounds: Rc<RefCell<WindowControlBounds>>,
}

#[derive(Clone, Copy, Default)]
struct WindowControlBounds {
    /// The buttons of the caption, in the order Min, Max, Close.
    buttons: [Option<Bounds<Pixels>>; 3],
    /// The empty part of the toolbar between its groups.
    drag: Option<Bounds<Pixels>>,
}

impl WindowControlAreas {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Keeps the areas of the three buttons of the caption, in the order Min, Max, Close.
    fn record_buttons(&self, bounds: &[Bounds<Pixels>]) {
        let mut buttons = [None; 3];
        for (slot, bounds) in buttons.iter_mut().zip(bounds) {
            *slot = Some(*bounds);
        }
        self.bounds.borrow_mut().buttons = buttons;
    }

    fn record_drag(&self, bounds: Bounds<Pixels>) {
        self.bounds.borrow_mut().drag = Some(bounds);
    }

    /// Forgets the areas of the caption: the window draws no title bar of its own, so the
    /// platform must not find a control where there is no button.
    pub(crate) fn clear(&self) {
        *self.bounds.borrow_mut() = WindowControlBounds::default();
    }

    /// The hitboxes the frame gives the platform, one for each area of the caption. The
    /// bounds are read while the frame is prepared, after the title bar has recorded its
    /// own, so a frame that moves the buttons moves the areas with them.
    #[cfg(windows)]
    fn prepaint(&self, window: &mut Window) -> Vec<(WindowControlArea, Hitbox)> {
        let bounds = *self.bounds.borrow();
        let mut areas = Vec::with_capacity(4);
        for (area, bounds) in [
            (WindowControlArea::Min, bounds.buttons[0]),
            (WindowControlArea::Max, bounds.buttons[1]),
            (WindowControlArea::Close, bounds.buttons[2]),
            (WindowControlArea::Drag, bounds.drag),
        ] {
            if let Some(bounds) = bounds {
                areas.push((area, window.insert_hitbox(bounds, HitboxBehavior::Normal)));
            }
        }
        areas
    }

    /// The areas of the last frame the caption drew, by the name a test knows them by.
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn drawn(&self) -> Vec<(&'static str, Bounds<Pixels>)> {
        let bounds = *self.bounds.borrow();
        let mut areas = Vec::with_capacity(4);
        for (name, bounds) in [
            ("w-area-min", bounds.buttons[0]),
            ("w-area-max", bounds.buttons[1]),
            ("w-area-close", bounds.buttons[2]),
            ("w-area-drag", bounds.drag),
        ] {
            if let Some(bounds) = bounds {
                areas.push((name, bounds));
            }
        }
        areas
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

/// One row of the menu a path crumb of the status bar opens.
struct FolderEntry {
    label: String,
    path: PathBuf,
    folder: bool,
    icon: &'static str,
}

/// The files and folders directly under `folder`, from the list of the files of the
/// project: the folders first, then the files, each in the order of their names.
fn folder_entries(
    folder: &Path,
    files: &[PathBuf],
    icons: &crate::editor::FileIcons,
) -> Vec<FolderEntry> {
    let mut entries: Vec<FolderEntry> = Vec::new();
    let mut seen: HashSet<(String, bool)> = HashSet::new();
    for file in files {
        let Ok(relative) = file.strip_prefix(folder) else {
            continue;
        };
        let mut parts = relative.components();
        let Some(name) = parts.next() else {
            continue;
        };
        let label = name.as_os_str().to_string_lossy().into_owned();
        let is_folder = parts.next().is_some();
        if !seen.insert((label.clone(), is_folder)) {
            continue;
        }
        let path = if is_folder {
            folder.join(name.as_os_str())
        } else {
            file.clone()
        };
        let icon = if is_folder {
            "nodes/folder"
        } else {
            icons.icon_for(&path)
        };
        entries.push(FolderEntry {
            label,
            path,
            folder: is_folder,
            icon,
        });
    }
    entries.sort_by_key(|entry| (!entry.folder, entry.label.to_lowercase()));
    entries
}

/// The row a path of the menu of the screen names: the section, then the row, then the row
/// of its submenu, and so on.
fn row_at<'a>(entries: &'a mut [MenuEntry], path: &[usize]) -> Option<&'a MenuEntry> {
    let (index, rest) = path.split_first()?;
    let entry = entries.get_mut(*index)?;
    if rest.is_empty() {
        Some(entry)
    } else {
        row_at(&mut entry.submenu, rest)
    }
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
fn command_entry(
    weak: &WeakEntity<Workspace>,
    id: &'static str,
    label: impl Into<SharedString>,
) -> MenuEntry {
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
        let view = "left-stripe";
        let scripted = cx.has_global::<ScriptSpots>();
        // The stripe is a cached view, so its spots are kept until it draws again.
        let button = |id: &'static str,
                      icon_name: &'static str,
                      label: &'static str,
                      key: Option<&'static str>,
                      selected: bool,
                      theme: &Theme| {
            self.stripe_button(id, icon_name, label, key, selected, theme)
                .relative()
                .child(kept_spot(scripted, view, format!("stripe-{id}")))
        };
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
            // A cached view of its own: no flex parent stretches it any more.
            .h_full()
            .py(px(SPACE_S))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .child(
                        button(
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
                        button(
                            "s-commit",
                            "toolwindows/commit",
                            "Commit",
                            Some("workspace::ToggleCommit"),
                            commit_open,
                            theme,
                        )
                        .on_click(toggle(COMMIT_TOOL)),
                    )
                    .child(separator())
                    .child(
                        button(
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
                        button(
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
                        button(
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
                        button(
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
                        button(
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
                        button(
                            "s-database",
                            "toolwindows/database",
                            "Database",
                            Some("workspace::ToggleDatabase"),
                            self.tool_open(DATABASE_TOOL),
                            theme,
                        )
                        .on_click(toggle(DATABASE_TOOL)),
                    )
                    .child(
                        button(
                            "s-todo",
                            "toolwindows/todo",
                            "TODO",
                            Some("workspace::ToggleTodo"),
                            self.tool_open(TODO_TOOL),
                            theme,
                        )
                        .on_click(toggle(TODO_TOOL)),
                    )
                    .child(
                        button(
                            "s-log",
                            "general/vcs",
                            "Git Log",
                            Some("workspace::ToggleLog"),
                            log_open,
                            theme,
                        )
                        .on_click(toggle(LOG_TOOL)),
                    )
                    .child(
                        button(
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
                        button(
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
        let view = "right-stripe";
        let scripted = cx.has_global::<ScriptSpots>();
        // The stripe is a cached view, so its spots are kept until it draws again.
        let button = |id: &'static str,
                      icon_name: &'static str,
                      label: &'static str,
                      key: Option<&'static str>,
                      selected: bool,
                      theme: &Theme| {
            self.stripe_button(id, icon_name, label, key, selected, theme)
                .relative()
                .child(kept_spot(scripted, view, format!("stripe-{id}")))
        };
        let separator = div().w(px(ICON)).h(px(DIVIDER_WIDTH)).bg(theme.border);
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_none()
            .w(px(RIGHT_STRIPE_WIDTH))
            .h_full()
            .py(px(SPACE_S))
            .child(
                button(
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
                button(
                    "s-ai",
                    "own/aiSessions",
                    "AI Sessions",
                    Some("workspace::ToggleAi"),
                    self.tool_open(AI_TOOL),
                    theme,
                )
                // The AI sessions, not whichever tool window the right dock shows last.
                .on_click(cx.listener(|this, _, window, cx| this.toggle_tool(AI_TOOL, window, cx))),
            )
            .child(separator)
            .child(
                button(
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
                button(
                    "s-bookmarks",
                    "toolwindows/bookmarks",
                    "Bookmarks",
                    None,
                    self.overlay_tag() == Some(BOOKMARKS_POPUP),
                    theme,
                )
                // The open popup hears this press too and closes itself on it, so the
                // button reads the popup before the pointer does. A second press then
                // closes the popup instead of opening it again.
                .capture_any_mouse_down(cx.listener(
                    |this, event: &MouseDownEvent, window, cx| {
                        if event.button == MouseButton::Left {
                            this.toggle_bookmarks(window, cx);
                        }
                    },
                )),
            )
    }

    /// The Bookmarks button of the right stripe: the popup opens, and the same button
    /// closes it while it is open.
    fn toggle_bookmarks(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.overlay_tag() == Some(BOOKMARKS_POPUP) {
            self.close_overlay(window, cx);
            return;
        }
        // A popup that is open now closes on this same press and would take the new one
        // with it, so the bookmarks open after the press is through.
        cx.defer_in(window, |this, window, cx| {
            this.run_command("window.show_bookmarks", window, cx);
        });
    }

    /// The areas of the window controls that the platform reads for hit testing. The title
    /// bar is cached, so most frames do not carry them on their own; the root gives the
    /// platform the bounds the bar left when it drew last, on every frame. The canvas
    /// stands after the title bar in the tree, so a frame that draws the bar moves the
    /// areas with it, and a window that draws no caption forgets them.
    #[cfg(windows)]
    fn caption_areas_canvas(&self) -> Option<AnyElement> {
        if !own_chrome() {
            return None;
        }
        let areas = self.window_control_areas.clone();
        Some(
            canvas(
                move |_, window, _| areas.prepaint(window),
                |_, hitboxes, window, _| {
                    for (area, hitbox) in hitboxes {
                        window.insert_window_control_hitbox(area, hitbox);
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .into_any_element(),
        )
    }

    /// The platforms with a title bar of their own take the controls from the elements
    /// themselves, and the root adds no areas.
    #[cfg(not(windows))]
    fn caption_areas_canvas(&self) -> Option<AnyElement> {
        None
    }

    /// The border of a window that draws its own title bar: a press on the border resizes
    /// the window, and the cursor of the border says so. Windows and macOS decorate their
    /// windows from the system, and a compositor that draws the frame of a Linux window
    /// needs no border of ours either; a maximized window has no border at all.
    fn window_resize_zones(&self, window: &Window) -> Vec<AnyElement> {
        let Decorations::Client { .. } = window.window_decorations() else {
            return Vec::new();
        };
        if !own_chrome() || window.is_maximized() || window.is_fullscreen() {
            return Vec::new();
        }
        let size = window.viewport_size();
        let border = px(WINDOW_RESIZE_BORDER);
        let double = px(WINDOW_RESIZE_BORDER * 2.);
        if size.width <= double || size.height <= double {
            return Vec::new();
        }
        // The press: one area covers the window and reads the point of the press, so the
        // border takes it anywhere on the border, as the frame of a system window does. A
        // press off the border does nothing and goes on to the element below.
        let press = div()
            .id("window-resize")
            .debug_selector(|| "window-resize".to_owned())
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .on_mouse_down(
                MouseButton::Left,
                |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                    let Some(edge) = resize_edge_at(
                        event.position,
                        window.viewport_size(),
                        window.is_maximized(),
                    ) else {
                        return;
                    };
                    cx.stop_propagation();
                    window.start_window_resize(edge);
                },
            )
            .into_any_element();
        // The cursor comes from the element under the pointer, so every side and corner
        // of the border is an element of its own. They take no press.
        let middle_width = size.width - double;
        let middle_height = size.height - double;
        let right = size.width - border;
        let bottom = size.height - border;
        let mut zones = Vec::with_capacity(9);
        zones.push(press);
        for (id, x, y, width, height, cursor) in [
            (
                "window-resize-top",
                border,
                px(0.),
                middle_width,
                border,
                CursorStyle::ResizeUp,
            ),
            (
                "window-resize-bottom",
                border,
                bottom,
                middle_width,
                border,
                CursorStyle::ResizeDown,
            ),
            (
                "window-resize-left",
                px(0.),
                border,
                border,
                middle_height,
                CursorStyle::ResizeLeft,
            ),
            (
                "window-resize-right",
                right,
                border,
                border,
                middle_height,
                CursorStyle::ResizeRight,
            ),
            (
                "window-resize-top-left",
                px(0.),
                px(0.),
                border,
                border,
                CursorStyle::ResizeUpLeftDownRight,
            ),
            (
                "window-resize-top-right",
                right,
                px(0.),
                border,
                border,
                CursorStyle::ResizeUpRightDownLeft,
            ),
            (
                "window-resize-bottom-left",
                px(0.),
                bottom,
                border,
                border,
                CursorStyle::ResizeUpRightDownLeft,
            ),
            (
                "window-resize-bottom-right",
                right,
                bottom,
                border,
                border,
                CursorStyle::ResizeUpLeftDownRight,
            ),
        ] {
            zones.push(
                div()
                    .id(id)
                    .absolute()
                    .left(x)
                    .top(y)
                    .w(width)
                    .h(height)
                    .cursor(cursor)
                    .into_any_element(),
            );
        }
        zones
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
                .on_click(
                    cx.listener(|this, _, window, cx| this.show_branches_under_title(window, cx)),
                )
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
        // The platform reads a caption area in the frame it drew last, and this title bar
        // is a cached view. The buttons leave their areas here so that the window root can
        // put them back on the frames that keep this one.
        let caption_space = self.window_control_areas.clone();
        let window_buttons = div()
            .flex()
            .items_center()
            .ml(px(window_gap))
            .on_children_prepainted(move |bounds, _, _| caption_space.record_buttons(&bounds))
            .child(minimize_button(
                window_button(
                    "w-min",
                    "windows/minimizeSmall",
                    "Minimize",
                    WindowControlArea::Min,
                    false,
                    theme,
                ),
                cx,
            ))
            .child(maximize_button(
                window_button(
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
                ),
                cx,
            ))
            .child(close_button(
                window_button(
                    "w-close",
                    "windows/closeSmall",
                    "Close",
                    WindowControlArea::Close,
                    true,
                    theme,
                ),
                cx,
            ));
        let tools = div()
            .id("tools-group")
            .debug_selector(|| "tools-group".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .child(search)
            .child(settings);
        let controls = cx
            .try_global::<Settings>()
            .is_none_or(|settings| settings.toolbar_visible);
        let right = div()
            .relative()
            .flex()
            .items_center()
            .children(controls.then(|| self.run_widget(theme, cx)))
            .children(controls.then(|| {
                div()
                    .w(px(DIVIDER_WIDTH))
                    .h(px(DIVIDER))
                    .mx(px((SPACE_L - DIVIDER_WIDTH) / 2.))
                    .bg(theme.border)
            }))
            .children(controls.then_some(tools))
            .children(own_chrome().then_some(window_buttons));

        // The empty space of the toolbar moves the window. Interactive elements stand
        // beside it rather than over it, so a press on a menu or a button never starts a
        // move. The area is recorded here and put back by the window root every frame.
        let drag_space = self.window_control_areas.clone();
        let drag_probe = canvas(
            move |bounds, _, _| drag_space.record_drag(bounds),
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        let titlebar_drag = div()
            .id("titlebar-drag")
            .debug_selector(|| "w-drag-space".to_owned())
            .flex_1()
            .h_full()
            .child(drag_probe);
        // A second press on the empty part of the title bar maximizes the window, or gives
        // back the size it had. The first press moves the window, as the caption of a
        // system window does. Taking the second press keeps the platform from handling the
        // double click of the caption itself, as the default handler of a Windows window
        // would.
        #[cfg(unix)]
        let titlebar_drag = titlebar_drag.on_mouse_down(
            MouseButton::Left,
            |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                if event.click_count >= 2 {
                    cx.stop_propagation();
                    toggle_window_zoom(window, cx);
                } else {
                    window.start_window_move()
                }
            },
        );
        #[cfg(windows)]
        let titlebar_drag = titlebar_drag.on_mouse_down(
            MouseButton::Left,
            |event: &MouseDownEvent, window: &mut Window, cx: &mut App| {
                if event.click_count >= 2 {
                    cx.stop_propagation();
                    toggle_window_zoom(window, cx);
                }
            },
        );

        let toolbar = div()
            .id("toolbar")
            .debug_selector(|| "toolbar".to_owned())
            .relative()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(TOOLBAR_HEIGHT));
        // Unix client decorations move the window from this area themselves; Windows gets
        // it from the root each frame, where a cached frame cannot lose it.
        #[cfg(not(windows))]
        let toolbar = toolbar.window_control_area(WindowControlArea::Drag);
        toolbar
            .child(
                div()
                    .id("left-group")
                    .debug_selector(|| "left-group".to_owned())
                    .flex()
                    .items_center()
                    .h_full()
                    .pl(px(if own_chrome() {
                        SPACE_M
                    } else {
                        TRAFFIC_LIGHTS_ROOM + SPACE_M
                    }))
                    .children(own_chrome().then(|| self.menu_bar(window, theme, cx)))
                    .children(controls.then(|| project.ml(px(SPACE_L))))
                    .children(controls.then(|| channel.ml(px(SPACE_XS))))
                    .children(
                        controls
                            .then(|| branch.map(|branch| branch.ml(px(SPACE_L))))
                            .flatten(),
                    ),
            )
            .child(titlebar_drag)
            .child(right)
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
        let coverage = button_frame(
            "t-coverage",
            "Run with Coverage",
            self.shortcut_of_key("workspace::RunWithCoverage")
                .map(Into::into),
            theme,
        )
        .debug_selector(|| "t-coverage".to_owned())
        .on_click(cx.listener(|this, _, window, cx| this.run_with_coverage(window, cx)))
        .child(icon_sized("run/coverage", ICON, theme));
        let profile = button_frame("t-profile", "Profile", None, theme)
            .debug_selector(|| "t-profile".to_owned())
            .on_click(cx.listener(|this, _, window, cx| this.run_with_profiler(window, cx)))
            .child(icon_sized("run/profiler", ICON, theme));
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
        // The coverage button first of the run group, so that Stop and the tools keep
        // the places the toolbar tests measure.
        div()
            .id("run-group")
            .debug_selector(|| "run-group".to_owned())
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .child(coverage)
            .child(profile)
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
            .shortcut(
                self.shortcut_of_key("workspace::BuildProject")
                    .unwrap_or_else(|| "Ctrl+F9".into()),
            ),
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
        let close_key = self
            .shortcut_of_key("workspace::CloseTab")
            .unwrap_or_else(|| "Ctrl+F4".into());
        let entries = vec![
            workspace_entry(&weak, "Close", move |this, window, cx| {
                this.close_tab(pane, index, window, cx)
            })
            .shortcut(close_key),
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
            MenuEntry::separator(),
            workspace_entry(&weak, "Unsplit", move |this, window, cx| {
                this.select_tab(pane, index, window, cx);
                this.unsplit(window, cx);
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
        self.command_row_named(weak, id, label)
    }

    /// The same row with a label the window builds, such as a switch with its check.
    fn command_row_named(
        &self,
        weak: &WeakEntity<Workspace>,
        id: &'static str,
        label: impl Into<SharedString>,
    ) -> MenuEntry {
        let mut entry = if self.commands().get(id).is_some() {
            command_entry(weak, id, label)
        } else {
            MenuEntry::disabled(label).id(id)
        };
        if let (Some(keys), Some(shortcut)) = (self.key_of_key(id), self.shortcut_of_key(id)) {
            entry = entry.shortcut(shortcut).keys(keys);
        }
        entry
    }

    /// A window action row with its key from the window bindings.
    fn action_row<A: Action + Clone + 'static>(&self, label: &'static str, action: A) -> MenuEntry {
        let entry = action_entry(label, action);
        let name = A::name_for_type();
        if let (Some(keys), Some(shortcut)) = (self.key_of_key(name), self.shortcut_of_key(name)) {
            entry.shortcut(shortcut).keys(keys)
        } else {
            entry
        }
    }

    /// The same row with a label that names the selected configuration.
    fn action_row_named<A: Action + Clone + 'static>(
        &self,
        label: impl Into<SharedString>,
        action: A,
    ) -> MenuEntry {
        let name = A::name_for_type();
        let entry = MenuEntry::new(label, move |window, cx| {
            window.dispatch_action(Box::new(action.clone()), cx);
        })
        .id(name);
        if let (Some(keys), Some(shortcut)) = (self.key_of_key(name), self.shortcut_of_key(name)) {
            entry.shortcut(shortcut).keys(keys)
        } else {
            entry
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

    /// Runs the row the menu of the screen names by its path. The menu of the window may
    /// have changed since it was built, so a path that no longer names a row does nothing.
    pub(crate) fn run_menu_row(
        &mut self,
        row: &MenuRow,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut entries = self.main_menu(cx);
        let run = row_at(&mut entries, &row.path).and_then(|entry| entry.on_click.clone());
        if let Some(run) = run {
            run(window, cx);
        }
    }

    /// The menu bar of the screen, which is the menu of the window on macOS. The other
    /// systems have none: their window draws the bar.
    pub(crate) fn refresh_platform_menus(&self, cx: &mut Context<Self>) {
        #[cfg(target_os = "macos")]
        {
            let screen = self.screen_menus(cx);
            // The hints come before the menus: the platform reads the keymap when it
            // builds them and makes the key it finds the key equivalent of the row.
            crate::native_menu::add_hints(cx, &screen.hints);
            cx.set_menus(screen.menus);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = cx;
    }

    /// The menus the platform shows on macOS: the menu of the application and the sections
    /// of the menu of the window. The window list is added to Window by the system.
    pub fn screen_menus(&self, cx: &mut Context<Self>) -> crate::native_menu::ScreenMenus {
        let sections = self.main_menu(cx);
        crate::native_menu::menus(self.info().name, &sections)
    }

    /// The sections of the main menu, in IDEA's order, with the rows the window can run.
    pub fn main_menu(&self, cx: &mut Context<Self>) -> Vec<MenuEntry> {
        let weak = cx.weak_entity();
        let command = |id: &'static str, label: &'static str| self.command_row(&weak, id, label);

        let recent = self.recent_project_entries(&weak);

        let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
        let mode = self.window_mode();
        let full_screen = if self.full_screen() {
            "Exit Full Screen"
        } else {
            "Enter Full Screen"
        };
        let presentation = if mode == WindowMode::Presentation {
            "Exit Presentation Mode"
        } else {
            "Enter Presentation Mode"
        };
        let distraction_free = if mode == WindowMode::DistractionFree {
            "Exit Distraction Free Mode"
        } else {
            "Enter Distraction Free Mode"
        };
        let zen = if mode == WindowMode::Zen {
            "Exit Zen Mode"
        } else {
            "Enter Zen Mode"
        };
        let appearance = MenuEntry::submenu(
            "Appearance",
            vec![
                self.command_row_named(&weak, TOGGLE_PRESENTATION_MODE, presentation),
                self.command_row_named(&weak, TOGGLE_DISTRACTION_FREE, distraction_free),
                self.command_row_named(&weak, TOGGLE_FULL_SCREEN, full_screen),
                self.command_row_named(&weak, TOGGLE_ZEN_MODE, zen),
                MenuEntry::separator(),
                self.command_row_named(
                    &weak,
                    editor_keys::TOGGLE_WHITESPACE,
                    checked_label("Show Whitespaces", settings.show_whitespace),
                ),
                self.command_row_named(
                    &weak,
                    editor_keys::TOGGLE_SOFT_WRAP,
                    checked_label("Soft-Wrap", settings.soft_wrap),
                ),
                MenuEntry::submenu(
                    "Active Editor",
                    vec![
                        self.command_row_named(
                            &weak,
                            "editor.soft_wrap_in_editor",
                            "Soft-Wrap in This Editor",
                        ),
                        self.command_row_named(
                            &weak,
                            "editor.show_whitespace_in_editor",
                            "Show Whitespaces in This Editor",
                        ),
                    ],
                ),
                MenuEntry::separator(),
                self.command_row_named(
                    &weak,
                    TOGGLE_TOOLBAR,
                    checked_label("Toolbar", settings.toolbar_visible),
                ),
                self.command_row_named(
                    &weak,
                    TOGGLE_TOOL_WINDOW_BARS,
                    checked_label("Tool Window Bars", settings.tool_window_bars_visible),
                ),
                self.command_row_named(
                    &weak,
                    TOGGLE_STATUS_BAR,
                    checked_label("Status Bar", settings.status_bar_visible),
                ),
                self.command_row_named(
                    &weak,
                    TOGGLE_NAVIGATION_BAR,
                    checked_label("Navigation Bar", settings.navigation_bar_visible),
                ),
                MenuEntry::separator(),
                self.command_row_named(
                    &weak,
                    TOGGLE_LINE_NUMBERS,
                    checked_label("Line Numbers", settings.show_line_numbers),
                ),
                self.command_row_named(
                    &weak,
                    TOGGLE_GUTTER_ICONS,
                    checked_label("Gutter Icons", settings.show_gutter_icons),
                ),
                self.command_row_named(
                    &weak,
                    TOGGLE_INDENT_GUIDES,
                    checked_label("Indent Guides", settings.show_indent_guides),
                ),
                self.command_row_named(
                    &weak,
                    TOGGLE_BREADCRUMBS,
                    checked_label("Breadcrumbs", settings.show_breadcrumbs),
                ),
                self.command_row_named(
                    &weak,
                    TOGGLE_STICKY_LINES,
                    checked_label("Sticky Lines", settings.show_sticky_lines),
                ),
            ],
        );

        let file = MenuEntry::submenu(
            "File",
            vec![
                self.action_row("Open Folder...", OpenFolder),
                MenuEntry::submenu("New", vec![command("file.new_project", "Project...")]),
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
                MenuEntry::separator(),
                MenuEntry::submenu(
                    "Manage IDE Settings",
                    vec![
                        command("settings.export", "Export Settings..."),
                        command("settings.import", "Import Settings..."),
                    ],
                ),
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
                command("workspace.search_structurally", "Search Structurally..."),
                command("workspace.replace_structurally", "Replace Structurally..."),
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
                self.action_row("Database", ToggleDatabase),
                command("jshell.open", "JShell Console"),
                self.action_row("Endpoints", ToggleEndpoints),
                self.action_row("Usages", ToggleUsages),
                self.action_row("TODO", ToggleTodo),
                self.action_row("Git", ToggleLog),
                MenuEntry::separator(),
                self.action_row("Run", ToggleRun),
                self.action_row("Debug", ToggleDebug),
                self.action_row("Terminal", ToggleTerminal),
                self.action_row("AI Assistant", ToggleAi),
                command("window.show_documentation", "Documentation"),
                self.action_row("HTTP Response", ToggleHttp),
            ],
        );

        let view = MenuEntry::submenu(
            "View",
            vec![
                tool_windows,
                appearance,
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
                command("workspace.analyze_stack_trace", "Analyze Stack Trace..."),
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
                command("workspace.copy_element", "Copy..."),
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

        let run_state = self.run_state(cx);
        let profile_label = match run_state.selected {
            Some(_) => format!("Profile '{}'", run_state.label()),
            None => "Profile".to_owned(),
        };
        let run = MenuEntry::submenu(
            "Run",
            vec![
                self.action_row("Run", Run),
                self.action_row("Debug", Debug),
                self.action_row("Run with Coverage", RunWithCoverage),
                self.action_row_named(profile_label, RunWithProfiler),
                self.action_row("Run Tests with Coverage", RunTestsWithCoverage),
                self.action_row("Run Tests with Profiler", RunTestsWithProfiler),
                self.action_row("Hide Coverage", HideCoverage),
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
                command("debug.mute_breakpoints", "Mute Breakpoints"),
                MenuEntry::separator(),
                command("debug.attach_jvm", "Attach to Process..."),
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
                command("git.show_selection_history", "Show History for Selection"),
                command("git.show_method_history", "Show History for Method"),
                command("git.show_log", "Git Log"),
                command(workspace_keys::ROLLBACK_CHANGED_LINES, "Rollback Lines"),
                command(workspace_keys::NEXT_CHANGE, "Show Next Change"),
                command(workspace_keys::PREVIOUS_CHANGE, "Show Previous Change"),
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
        let font_size = px(UI_SIZE);
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
        // macOS has no mnemonics in its menu bar of the screen.
        if !own_chrome() {
            return false;
        }
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

    /// The menu of a path crumb of the status bar: the folders and files of that level
    /// of the project. A folder opens its own level, a file opens in the editor.
    pub(crate) fn show_folder_menu(
        &mut self,
        folder: PathBuf,
        position: Point<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(files) = self.project_files().map(|files| files.read(cx).clone()) else {
            return;
        };
        let icons = self.icons().clone();
        let entries: Vec<MenuEntry> = folder_entries(&folder, &files, &icons)
            .into_iter()
            .map(|entry| {
                let weak = cx.weak_entity();
                let path = entry.path;
                let is_folder = entry.folder;
                MenuEntry::new(entry.label, move |window, cx| {
                    weak.update(cx, |workspace, cx| {
                        if is_folder {
                            workspace.show_folder_menu(path.clone(), position, cx);
                        } else {
                            workspace.open_file(path.clone(), window, cx);
                        }
                    })
                    .ok();
                })
                .icon(entry.icon)
            })
            .collect();
        if entries.is_empty() {
            return;
        }
        self.show_menu(position, entries, cx);
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
        let bar = if self.distraction_free() {
            div().into_any_element()
        } else {
            self.panes[pane].bar.clone().map_or_else(
                || div().into_any_element(),
                |bar| {
                    bar.cached(
                        StyleRefinement::default()
                            .w_full()
                            .h(px(HEADER_HEIGHT))
                            .flex_none(),
                    )
                    .into_any_element()
                },
            )
        };
        island(theme)
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .bg(theme.editor_bg)
            .child(bar)
            .child(body)
    }

    /// The splits and the groups of the editor area, the nested splits inside the parts
    /// of the ones above them.
    fn split_view(
        &self,
        node: &SplitNode,
        path: Vec<usize>,
        divider: &mut usize,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let SplitNode::Split {
            down,
            ratio,
            first,
            second,
        } = node
        else {
            let SplitNode::Group(pane) = node else {
                return div().into_any_element();
            };
            return div()
                .flex()
                .flex_1()
                .min_w_0()
                .min_h_0()
                .child(self.pane_view(*pane, theme, cx))
                .into_any_element();
        };
        let index = *divider;
        *divider += 1;
        let first = self.split_view(first, child_path(&path, 0), divider, theme, cx);
        let second = self.split_view(second, child_path(&path, 1), divider, theme, cx);
        let grab = path.clone();
        let wanted = path.clone();
        let ratio = ratio.clamp(SPLIT_LIMIT, 1. - SPLIT_LIMIT);
        div()
            .id(("split", index))
            .flex()
            .when(*down, |this| this.flex_col())
            .flex_1()
            .min_w_0()
            .min_h_0()
            .on_drag_move(
                cx.listener(move |this, event: &DragMoveEvent<DraggedSplit>, _, cx| {
                    if event.drag(cx).path == wanted {
                        this.drag_split(&wanted, event.event.position, event.bounds, cx);
                    }
                }),
            )
            .child(
                div()
                    .flex()
                    .flex_grow(ratio)
                    .min_w_0()
                    .min_h_0()
                    .child(first),
            )
            .child(self.split_divider(grab, *down, index, theme))
            .child(
                div()
                    .flex()
                    .flex_grow(1. - ratio)
                    .min_w_0()
                    .min_h_0()
                    .child(second),
            )
            .into_any_element()
    }

    /// The bar between the two parts of a split: a drag moves the share they take.
    fn split_divider(
        &self,
        path: Vec<usize>,
        down: bool,
        index: usize,
        theme: &Theme,
    ) -> Stateful<Div> {
        div()
            .id(("split-divider", index))
            .debug_selector(move || format!("split-divider-{index}"))
            .flex_none()
            .when(down, |this| {
                this.w_full().h(px(EDGE_GRIP)).cursor_row_resize()
            })
            .when(!down, |this| {
                this.h_full().w(px(EDGE_GRIP)).cursor_col_resize()
            })
            .bg(theme.border)
            .on_drag(DraggedSplit { path }, |_, _, _, cx| {
                cx.new(|_| NoDragPreview)
            })
    }

    /// Moves the share of the split at `path` to the place of the pointer in it.
    fn drag_split(
        &mut self,
        path: &[usize],
        position: Point<Pixels>,
        bounds: Bounds<Pixels>,
        cx: &mut Context<Self>,
    ) {
        let Some(down) = self.splits.split_direction(path) else {
            return;
        };
        let (moved, size) = if down {
            (position.y - bounds.origin.y, bounds.size.height)
        } else {
            (position.x - bounds.origin.x, bounds.size.width)
        };
        if size <= px(0.) {
            return;
        }
        if self
            .splits
            .set_ratio(path, f32::from(moved) / f32::from(size))
        {
            self.layout_changed(cx);
            cx.notify();
        }
    }

    fn editor_area(&self, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement {
        let mut divider = 0;
        let tree = self.split_view(self.splits.root(), Vec::new(), &mut divider, theme, cx);
        div()
            .flex()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .child(tree)
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
            Some(tool) => tool
                .view
                .clone()
                .cached(StyleRefinement::default().size_full())
                .into_any_element(),
            None => island(theme).size_full().into_any_element(),
        }
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
                let body = menu(entries, state, geometry, &callbacks, theme, window, cx);
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
        let body = notification(
            title,
            text,
            actions,
            close,
            cx.has_global::<ScriptSpots>(),
            theme,
        );
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
        // The spots a script names are recorded for this frame only; a view the window
        // caches keeps its own until it draws again. A window no script drives has no
        // table at all.
        if cx.has_global::<ScriptSpots>() {
            cx.global_mut::<ScriptSpots>().begin_frame();
        }
        #[cfg(any(test, feature = "test-support"))]
        if self.state_revision != self.counted_revision {
            self.counted_revision = self.state_revision;
            let count = cx.default_global::<crate::window::WindowRenderCount>();
            count.0.set(count.0.get() + 1);
        }
        if self.step_animations(cx) {
            window.request_animation_frame();
        }
        let theme = *cx.global::<Theme>();
        let gap = px(ISLAND_GAP);
        #[cfg(feature = "test-support")]
        self.apply_test_ui_scale(window);
        window.set_rem_size(px(16.));
        // The platform looks for the areas of the caption in the frame it drew last, and
        // the title bar is a cached view: the root gives them back on every frame. A
        // window without a caption of its own has none, and the areas of an older frame
        // must not stay where no button is drawn.
        if !own_chrome() {
            self.window_control_areas.clear();
        }
        let viewport = window.viewport_size();
        let (shows_stripes, shows_status) = {
            let settings = cx.try_global::<Settings>();
            let distraction = self.distraction_free();
            (
                !distraction && settings.is_none_or(|settings| settings.tool_window_bars_visible),
                !distraction && settings.is_none_or(|settings| settings.status_bar_visible),
            )
        };
        let distraction = self.distraction_free();
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
        let (left_panel, right_panel, bottom_panel) = if distraction {
            (0., 0., 0.)
        } else {
            (left_panel, right_panel, bottom_panel)
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
            .child(self.editor_area(&theme, cx))
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
        let navigation = cx.weak_entity();
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
            .text_size(px(UI_SIZE))
            .child(
                // The side buttons of the mouse are the keys of Back and Forward:
                // pressing one runs the same command wherever the pointer is, even over
                // a field of a dialog or of a tool window that holds the focus. The
                // listener is registered by the first child, so a view with a history of
                // its own, such as the Documentation window, sees the press first and may
                // take it for itself.
                canvas(
                    |_, _, _| (),
                    move |_, _, window, _| {
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            let id = match event.button {
                                MouseButton::Navigate(NavigationDirection::Back) => {
                                    workspace_keys::BACK
                                }
                                MouseButton::Navigate(NavigationDirection::Forward) => {
                                    workspace_keys::FORWARD
                                }
                                _ => return,
                            };
                            if let Some(workspace) = navigation.upgrade() {
                                workspace.update(cx, |workspace, cx| {
                                    workspace.run_command(id, window, cx);
                                });
                            }
                        });
                    },
                )
                .absolute()
                .size_full(),
            )
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
            .on_action(cx.listener(|this, _: &ToggleDatabase, window, cx| {
                this.toggle_tool(DATABASE_TOOL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleEndpoints, window, cx| {
                this.toggle_tool(ENDPOINTS_TOOL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleCommit, window, cx| {
                this.toggle_tool(COMMIT_TOOL, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ToggleLog, window, cx| {
                let moved = this.activate_tool(LOG_TOOL, cx);
                if this.tool_open(LOG_TOOL) && !moved {
                    this.toggle_tool(LOG_TOOL, window, cx);
                } else {
                    this.show_tool(LOG_TOOL, window, cx);
                }
            }))
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
            .on_action(cx.listener(|this, _: &RunWithCoverage, window, cx| {
                this.run_with_coverage(window, cx)
            }))
            .on_action(cx.listener(|this, _: &RunWithProfiler, window, cx| {
                this.run_with_profiler(window, cx)
            }))
            .on_action(cx.listener(|this, _: &RunTestsWithCoverage, window, cx| {
                this.run_tests_with_coverage(window, cx)
            }))
            .on_action(cx.listener(|this, _: &RunTestsWithProfiler, window, cx| {
                this.run_tests_with_profiler(window, cx)
            }))
            .on_action(
                cx.listener(|this, _: &HideCoverage, window, cx| this.hide_coverage(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ToggleCoverage, window, cx| {
                    this.toggle_coverage(window, cx)
                }),
            )
            .on_action(
                cx.listener(|this, _: &StopBackgroundProcesses, window, cx| {
                    this.stop_background(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &Synchronize, window, cx| {
                this.run_command("file.reload_all", window, cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleFullScreen, window, cx| {
                this.toggle_full_screen(window, cx)
            }))
            .on_action(cx.listener(|this, _: &Stop, window, cx| this.stop_run(window, cx)))
            .on_action(
                cx.listener(|this, _: &ToggleAi, window, cx| this.toggle_tool(AI_TOOL, window, cx)),
            )
            .on_action(cx.listener(|this, _: &ShowSearch, window, cx| this.show_search(window, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| this.dismiss(window, cx)))
            .on_action(
                cx.listener(|this, row: &MenuRow, window, cx| this.run_menu_row(row, window, cx)),
            )
            .on_action(cx.listener(|_, _: &QuitApp, _, cx| cx.quit()))
            .on_action(cx.listener(|this, _: &AboutApp, window, cx| this.show_about(window, cx)))
            .on_action(cx.listener(|this, _: &CheckForUpdatesApp, _, cx| this.check_updates(cx)))
            .on_action(cx.listener(|_, _: &HideApp, _, cx| cx.hide()))
            .on_action(cx.listener(|_, _: &HideOtherApps, _, cx| cx.hide_other_apps()))
            .on_action(cx.listener(|_, _: &ShowAllApps, _, cx| cx.unhide_other_apps()))
            .on_action(cx.listener(|_, _: &MinimizeWindow, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &ZoomWindow, window, _| window.zoom_window()))
            .on_action(cx.listener(|_, _: &BringAllToFront, window, cx| {
                cx.activate(true);
                window.activate_window();
            }))
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
            .on_action(cx.listener(|this, _: &ToggleHttp, window, cx| {
                this.toggle_tool(HTTP_TOOL, window, cx)
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
            .on_action(cx.listener(|this, _: &ResizeToolWindowLeft, window, cx| {
                this.resize_focused_dock(ResizeStep::Left, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ResizeToolWindowRight, window, cx| {
                this.resize_focused_dock(ResizeStep::Right, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ResizeToolWindowUp, window, cx| {
                this.resize_focused_dock(ResizeStep::Up, window, cx)
            }))
            .on_action(cx.listener(|this, _: &ResizeToolWindowDown, window, cx| {
                this.resize_focused_dock(ResizeStep::Down, window, cx)
            }))
            .on_action(cx.listener(|this, _: &FullyExpandTreeNode, window, cx| {
                this.run_command("workspace.fully_expand_tree_node", window, cx);
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
            .child(
                self.title_view
                    .clone()
                    .cached(
                        StyleRefinement::default()
                            .w_full()
                            .h(px(TOOLBAR_HEIGHT))
                            .flex_none(),
                    )
                    .into_any_element(),
            )
            // After the title bar: a frame that draws the bar records the bounds of its
            // buttons first, and the areas of the platform follow it in the same frame.
            // Every other frame keeps the cached bar and gets the areas it left behind.
            .children(self.caption_areas_canvas())
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .children(shows_stripes.then(|| {
                        self.left_stripe_view
                            .clone()
                            .cached(
                                StyleRefinement::default()
                                    .w(px(STRIPE_WIDTH))
                                    .h_full()
                                    .flex_none(),
                            )
                            .into_any_element()
                    }))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(upper)
                            .child(lower),
                    )
                    .children((shows_stripes).then(|| {
                        self.right_stripe_view
                            .clone()
                            .cached(
                                StyleRefinement::default()
                                    .w(px(RIGHT_STRIPE_WIDTH))
                                    .h_full()
                                    .flex_none(),
                            )
                            .into_any_element()
                    })),
            )
            .children(shows_status.then(|| {
                self.status_view
                    .clone()
                    .cached(
                        StyleRefinement::default()
                            .w_full()
                            .h(px(STATUS_HEIGHT))
                            .flex_none(),
                    )
                    .into_any_element()
            }))
            .children(self.overlay_element(&theme, window, cx))
            .children(self.balloon_element(&theme, cx))
            // The border stands above the window: a press on it resizes the window and
            // never reaches the scrollbar or a button under the pointer.
            .children(self.window_resize_zones(window))
    }
}

/// A piece of the window chrome or a tool window that draws itself apart from the
/// window. The window embeds it as a cached view, so a keystroke in the editor only
/// rebuilds the editor: the piece redraws when its own view changes, when the window
/// changes what it shows, or when the theme or the interface scale changes.
pub(crate) enum ChromePart {
    TitleBar,
    LeftStripe,
    RightStripe,
    /// A tool window, drawn by its own entity.
    Tool(AnyView),
}

pub(crate) struct ChromeView {
    /// The window that the chrome of this piece reads; `None` for a tool window.
    workspace: Option<WeakEntity<Workspace>>,
    part: ChromePart,
}

impl ChromeView {
    fn create(
        workspace: Option<WeakEntity<Workspace>>,
        part: ChromePart,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            // A theme fade changes the global on every frame of the animation, and the
            // scale changes every size of the chrome: both have to redraw the cache.
            cx.observe_global::<Theme>(|_, cx| cx.notify()).detach();
            cx.observe_global::<Settings>(|_, cx| cx.notify()).detach();
            Self { workspace, part }
        })
    }

    pub(crate) fn title_bar(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        Self::create(Some(workspace), ChromePart::TitleBar, cx)
    }

    pub(crate) fn left_stripe(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        Self::create(Some(workspace), ChromePart::LeftStripe, cx)
    }

    pub(crate) fn right_stripe(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        Self::create(Some(workspace), ChromePart::RightStripe, cx)
    }

    /// Wraps a tool window of the docks, so that the window can keep its last frame
    /// while the tool window itself did not change.
    pub(crate) fn tool(view: AnyView, cx: &mut App) -> AnyView {
        Self::create(None, ChromePart::Tool(view), cx).into()
    }
}

/// The name a cached piece of the window is known by in the table of `--script` spots.
fn chrome_view_name(part: &ChromePart) -> &'static str {
    match part {
        ChromePart::TitleBar => "title",
        ChromePart::LeftStripe => "left-stripe",
        ChromePart::RightStripe => "right-stripe",
        ChromePart::Tool(_) => "tool",
    }
}

impl Render for ChromeView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // A cached piece drops the spots of the frame it drew last: what it no longer
        // shows leaves the table, and what it shows is recorded again below.
        if cx.has_global::<ScriptSpots>() {
            cx.global_mut::<ScriptSpots>()
                .begin_view(chrome_view_name(&self.part));
        }
        let theme = *cx.global::<Theme>();
        let content = match &self.part {
            ChromePart::Tool(view) => view.clone().into_any_element(),
            _ => {
                let Some(workspace) = self.workspace.as_ref().and_then(|weak| weak.upgrade())
                else {
                    return div().size_full().into_any_element();
                };
                match &self.part {
                    ChromePart::TitleBar => {
                        #[cfg(any(test, feature = "test-support"))]
                        crate::window::note_view_render(cx, "title");
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.toolbar(window, &theme, cx).into_any_element()
                            })
                            .into_any_element()
                    }
                    ChromePart::LeftStripe => workspace
                        .update(cx, |workspace, cx| {
                            workspace.left_stripe(&theme, cx).into_any_element()
                        })
                        .into_any_element(),
                    ChromePart::RightStripe => workspace
                        .update(cx, |workspace, cx| {
                            workspace.right_stripe(&theme, cx).into_any_element()
                        })
                        .into_any_element(),
                    ChromePart::Tool(_) => div().into_any_element(),
                }
            }
        };
        // A cached view is laid out as a root at the size the window gave it, so its
        // content has to fill that size instead of measuring itself.
        div().size_full().child(content).into_any_element()
    }
}

/// Told that the text, the caret or the dirty state of an editor changed. The views that
/// show them listen here, so a keystroke redraws only what it changes.
#[derive(Clone, Copy)]
pub(crate) struct EditorChromeChanged;

/// The signal that the editor events of the window go through.
pub(crate) struct EditorChromeSignal;

impl EventEmitter<EditorChromeChanged> for EditorChromeSignal {}

impl EditorChromeSignal {
    pub(crate) fn changed(&mut self, cx: &mut Context<Self>) {
        cx.emit(EditorChromeChanged);
    }
}

/// The tabs of one group. It follows the active editor and the state of the group
/// without redrawing the window.
pub(crate) struct TabBarView {
    workspace: WeakEntity<Workspace>,
    pane: usize,
    _changed: Subscription,
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) shown_dirty: Vec<bool>,
}

impl TabBarView {
    pub(crate) fn create(
        workspace: WeakEntity<Workspace>,
        signal: Entity<EditorChromeSignal>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let changed = cx.subscribe(&signal, |_, _, _: &EditorChromeChanged, cx| cx.notify());
            cx.observe_global::<Theme>(|_, cx| cx.notify()).detach();
            Self {
                workspace,
                pane: 0,
                _changed: changed,
                #[cfg(any(test, feature = "test-support"))]
                shown_dirty: Vec::new(),
            }
        })
    }

    pub(crate) fn set_pane(&mut self, pane: usize, cx: &mut Context<Self>) {
        if self.pane != pane {
            self.pane = pane;
            cx.notify();
        }
    }

    /// Runs a change of the workspace once this bar is free. A change that takes a tab
    /// out of its group may drop the group and renumbers every strip, which updates this
    /// bar too: from a handler of the bar itself that would update it while it is already
    /// being updated.
    fn defer_change(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        change: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
    ) {
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| change(workspace, window, cx))
                .ok();
        });
    }
}

impl Render for TabBarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let selection = theme.selection;
        let Some(workspace) = self.workspace.upgrade() else {
            return div().into_any_element();
        };
        let (active, scroll, tabs) = {
            let workspace = workspace.read(cx);
            let Some(pane) = workspace.panes.get(self.pane) else {
                return div().into_any_element();
            };
            let tabs = pane
                .tabs
                .iter()
                .enumerate()
                .map(|(index, item)| {
                    (
                        index,
                        (item.editor.status)(cx).dirty,
                        item.pinned,
                        item.title.clone(),
                        item.icon,
                    )
                })
                .collect::<Vec<_>>();
            (pane.active, pane.scroll.clone(), tabs)
        };
        #[cfg(any(test, feature = "test-support"))]
        {
            self.shown_dirty = tabs.iter().map(|tab| tab.1).collect();
        }
        let pane = self.pane;
        let items = tabs.into_iter().map(|(index, dirty, pinned, title, icon)| {
            let id = tab_id(pane, index);
            let style = TabStyle {
                id: ("tab", id).into(),
                close_id: ("tab-close", id).into(),
                group: format!("tab-{id}").into(),
                label: title.clone(),
                icon,
                selected: index == active,
                dirty,
                pinned,
            };
            // The dot of a pinned tab unpins it, the cross of another one closes it.
            let close = cx.listener(move |this, _: &ClickEvent, window, cx| {
                cx.stop_propagation();
                this.defer_change(window, cx, move |workspace, window, cx| {
                    if pinned {
                        workspace.toggle_pin(pane, index, window, cx);
                    } else {
                        workspace.close_tab(pane, index, window, cx);
                    }
                });
            });
            let dragged = DraggedTab { pane, index, title };
            let select_workspace = self.workspace.clone();
            let menu_workspace = self.workspace.clone();
            tab(style, close, &theme)
                .debug_selector(move || format!("tab-{pane}-{index}"))
                .on_click(cx.listener(move |_, _, window, cx| {
                    select_workspace
                        .update(cx, |workspace, cx| {
                            workspace.select_tab(pane, index, window, cx)
                        })
                        .ok();
                }))
                .on_mouse_down(
                    MouseButton::Middle,
                    cx.listener(move |this, _, window, cx| {
                        this.defer_change(window, cx, move |workspace, window, cx| {
                            workspace.close_tab(pane, index, window, cx)
                        });
                    }),
                )
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                        menu_workspace
                            .update(cx, |workspace, cx| {
                                workspace.show_tab_menu(pane, index, event.position, cx)
                            })
                            .ok();
                    }),
                )
                .on_drag(dragged, |dragged, _, _, cx| {
                    let title = dragged.title.clone();
                    cx.new(|_| TabDragPreview(title))
                })
                .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(selection))
                .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                    let from = (dragged.pane, dragged.index);
                    this.defer_change(window, cx, move |workspace, window, cx| {
                        workspace.move_tab(from, (pane, index), window, cx)
                    });
                }))
        });
        let strip = div()
            .id(("tabs", pane))
            .debug_selector(move || format!("tabs-{pane}"))
            .flex()
            .items_center()
            .gap(px(2.))
            // scrollbar-exempt: the strip is the content of ui_kit::scrollable just below
            .overflow_x_scroll()
            .track_scroll(&scroll)
            .children(items)
            .drag_over::<DraggedTab>(move |style, _, _, _| style.bg(selection))
            .on_drop(cx.listener(move |this, dragged: &DraggedTab, window, cx| {
                let from = (dragged.pane, dragged.index);
                this.defer_change(window, cx, move |workspace, window, cx| {
                    let end = workspace.panes.get(pane).map_or(0, |pane| pane.tabs.len());
                    workspace.move_tab(from, (pane, end), window, cx);
                });
            }));
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .w_full()
            .h(px(HEADER_HEIGHT))
            .px(px(SPACE_M))
            .border_b_1()
            .border_color(theme.border)
            .child(
                scrollable(("tabs", pane), &scroll, strip, &theme, window, cx)
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .items_center(),
            )
            .into_any_element()
    }
}

/// The status bar of the window. It shows the active tab, the branch and the state of the
/// editor, and follows them without redrawing the window.
pub(crate) struct StatusBarView {
    workspace: WeakEntity<Workspace>,
    _changed: Subscription,
    #[cfg(any(test, feature = "test-support"))]
    pub(crate) shown_caret: Option<String>,
}

impl StatusBarView {
    pub(crate) fn create(
        workspace: WeakEntity<Workspace>,
        signal: Entity<EditorChromeSignal>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let changed = cx.subscribe(&signal, |_, _, _: &EditorChromeChanged, cx| cx.notify());
            cx.observe_global::<Theme>(|_, cx| cx.notify()).detach();
            Self {
                workspace,
                _changed: changed,
                #[cfg(any(test, feature = "test-support"))]
                shown_caret: None,
            }
        })
    }
}

impl Render for StatusBarView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let Some(workspace) = self.workspace.upgrade() else {
            return div().into_any_element();
        };
        let (
            name,
            root,
            active,
            language_status,
            language_details,
            branch,
            status,
            branch_anchor,
            eol_anchor,
            enc_anchor,
            indent_anchor,
            zoom_notice,
            agent_clients,
            agent_busy,
        ) = {
            let workspace = workspace.read(cx);
            (
                workspace.name.clone(),
                workspace.root.clone(),
                workspace
                    .active_tab()
                    .map(|tab| (tab.path.clone(), tab.icon)),
                workspace.language_status.clone(),
                workspace.language_details.is_some(),
                workspace.branch.clone(),
                workspace.active_status(cx),
                workspace.st_branch_anchor.clone(),
                workspace.st_eol_anchor.clone(),
                workspace.st_enc_anchor.clone(),
                workspace.st_indent_anchor.clone(),
                workspace.zoom_notice().cloned(),
                workspace.agent_clients,
                workspace.agent_busy,
            )
        };
        #[cfg(any(test, feature = "test-support"))]
        {
            self.shown_caret = status.as_ref().map(|status| status.caret.clone());
        }
        let mut path = div()
            .flex()
            .items_center()
            .gap(px(SPACE_XS))
            .flex_1()
            .min_w_0()
            .overflow_hidden();
        let root_menu = root.clone();
        path = path.child(
            crumb(0usize, Some("nodes/module"), name, &theme).on_click(cx.listener(
                move |this, event: &ClickEvent, _window, cx| {
                    let folder = root_menu.clone();
                    let position = event.position();
                    this.workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_folder_menu(folder, position, cx)
                        })
                        .ok();
                },
            )),
        );
        if let Some((file, icon)) = &active {
            let parts = path_parts(&root, file);
            let count = parts.len();
            let mut folder = root.clone();
            for (index, part) in parts.into_iter().enumerate() {
                let leading = (index + 1 == count).then_some(*icon);
                let is_file = index + 1 == count;
                let mut element = crumb(index + 1, leading, part.clone(), &theme);
                if !is_file {
                    folder = folder.join(&part);
                    let child = folder.clone();
                    element = element.cursor_pointer().on_click(cx.listener(
                        move |this, event: &ClickEvent, _window, cx| {
                            let folder = child.clone();
                            let position = event.position();
                            this.workspace
                                .update(cx, |workspace, cx| {
                                    workspace.show_folder_menu(folder, position, cx)
                                })
                                .ok();
                        },
                    ));
                }
                path = path.child(crumb_separator(&theme)).child(element);
            }
        }
        let zoom = zoom_notice.map(|text| {
            status_widget("st-zoom", text, &theme).debug_selector(|| "st-zoom".to_owned())
        });
        let language = language_status.map(|text| {
            // A server that is still starting or importing cannot answer: its line is
            // shown in the warning colour, so the user sees why Ctrl+B waits.
            let ready = text.ends_with("ready");
            let widget = status_widget("st-language", text, &theme)
                .debug_selector(|| "st-language".to_owned())
                .when(!ready, |widget| widget.text_color(theme.warning_mark));
            if language_details {
                let workspace = self.workspace.clone();
                widget
                    .cursor_pointer()
                    .on_click(cx.listener(move |_, _, _, cx| {
                        workspace
                            .update(cx, |workspace, cx| workspace.show_language_help(cx))
                            .ok();
                    }))
            } else {
                widget
            }
        });
        let branch = branch.map(|branch| {
            let workspace = self.workspace.clone();
            let anchor = branch_anchor.clone();
            status_widget("st-branch", branch, &theme)
                .relative()
                .child(anchor.canvas())
                .on_click(cx.listener(move |_, _, window, cx| {
                    if let Some(bounds) = anchor.bounds() {
                        workspace
                            .update(cx, |workspace, _| workspace.set_action_menu_anchor(bounds))
                            .ok();
                    }
                    window.dispatch_action(Box::new(ShowBranches), cx);
                }))
                .tooltip(|_, cx| tooltip("Git Branches", None, cx))
                .tooltip_show_delay(ui_kit::tooltip_show_delay())
        });
        let editor = status.map(|status| {
            let (indent_tabs, indent_size) = (status.indent_tabs, status.indent_size);
            let language_name = status.language.clone();
            let read_only = status.read_only;
            let eol_workspace = self.workspace.clone();
            let eol_anchor = eol_anchor.clone();
            let eol = status_widget("st-eol", status.eol, &theme)
                .debug_selector(|| "st-eol".to_owned())
                .relative()
                .child(eol_anchor.canvas())
                .tooltip(|_, cx| tooltip("Line Separator", None, cx))
                .tooltip_show_delay(ui_kit::tooltip_show_delay())
                .on_click(cx.listener(move |_, event: &ClickEvent, window, cx| {
                    let anchor = eol_anchor.bounds();
                    eol_workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_format_menu(
                                &LINE_ENDS,
                                anchor,
                                event.position(),
                                window,
                                cx,
                            )
                        })
                        .ok();
                }));
            let enc_workspace = self.workspace.clone();
            let enc_anchor = enc_anchor.clone();
            let encoding = status_widget("st-enc", status.encoding, &theme)
                .debug_selector(|| "st-enc".to_owned())
                .relative()
                .child(enc_anchor.canvas())
                .tooltip(|_, cx| tooltip("File Encoding", None, cx))
                .tooltip_show_delay(ui_kit::tooltip_show_delay())
                .on_click(cx.listener(move |_, event: &ClickEvent, window, cx| {
                    let anchor = enc_anchor.bounds();
                    enc_workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_format_menu(
                                &ENCODINGS,
                                anchor,
                                event.position(),
                                window,
                                cx,
                            )
                        })
                        .ok();
                }));
            let indent_workspace = self.workspace.clone();
            let indent_anchor = indent_anchor.clone();
            let indent = status_widget("st-indent", status.indent, &theme)
                .debug_selector(|| "st-indent".to_owned())
                .relative()
                .child(indent_anchor.canvas())
                .tooltip(|_, cx| tooltip("Indent", None, cx))
                .tooltip_show_delay(ui_kit::tooltip_show_delay())
                .on_click(cx.listener(move |_, event: &ClickEvent, window, cx| {
                    let placement = MenuPlacement {
                        anchor: indent_anchor.bounds(),
                        position: event.position(),
                    };
                    let language = language_name.as_ref().map(|name| name.to_string());
                    indent_workspace
                        .update(cx, |workspace, cx| {
                            workspace.show_indent_menu(
                                placement,
                                indent_tabs,
                                indent_size,
                                language,
                                window,
                                cx,
                            )
                        })
                        .ok();
                }));
            let read_only_workspace = self.workspace.clone();
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_L))
                .child(
                    status_widget("st-caret", status.caret, &theme)
                        .debug_selector(|| "st-caret".to_owned()),
                )
                .child(eol)
                .child(encoding)
                .child(indent)
                .child(
                    status_widget("st-insert", status.insert, &theme)
                        .debug_selector(|| "st-insert".to_owned()),
                )
                .children(read_only.then(|| {
                    status_widget("st-readonly", "Read-only", &theme)
                        .debug_selector(|| "st-readonly".to_owned())
                        .cursor_pointer()
                        .tooltip(|_, cx| tooltip("Read-Only Attribute", None, cx))
                        .tooltip_show_delay(ui_kit::tooltip_show_delay())
                        .on_click(cx.listener(move |_, _, window, cx| {
                            read_only_workspace
                                .update(cx, |workspace, cx| {
                                    workspace.run_command("editor.toggle_read_only", window, cx);
                                })
                                .ok();
                        }))
                }))
        });
        let agents = (agent_clients > 0).then(|| {
            let workspace = self.workspace.clone();
            let label = if agent_busy {
                "An agent is working"
            } else {
                "Connected agents"
            };
            div()
                .id("st-agents")
                .debug_selector(|| "st-agents".to_owned())
                .flex()
                .items_center()
                .gap(px(SPACE_XS))
                .flex_none()
                .cursor_pointer()
                .tooltip(move |_, cx| tooltip(label, None, cx))
                .tooltip_show_delay(ui_kit::tooltip_show_delay())
                .on_click(cx.listener(move |_, _, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            workspace.run_command(AI_CLIENTS, window, cx);
                        })
                        .ok();
                }))
                .child(icon("own/aiSessions", &theme))
                .child(agent_clients.to_string())
                .when(agent_busy, |this| {
                    this.child(
                        div()
                            .debug_selector(|| "st-agents-busy".to_owned())
                            .size(px(6.))
                            .rounded_full()
                            .bg(theme.accent),
                    )
                })
        });
        let widgets = div()
            .flex()
            .items_center()
            .gap(px(SPACE_L))
            .children(zoom)
            .children(agents)
            .children(language)
            .children(editor);
        div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .w_full()
            .h(px(STATUS_HEIGHT))
            .gap(px(SPACE_L))
            .px(px(SPACE_M))
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_muted)
            .child(path)
            .children(branch)
            .child(widgets)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::path_parts;
    use std::path::Path;
    use ui_kit::gpui::{Bounds, Pixels, ResizeEdge, point, px, size};

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

    /// An area of a caption, ten pixels high and as wide as the test names.
    fn area(left: f32, width: f32) -> Bounds<Pixels> {
        Bounds {
            origin: point(px(left), px(0.)),
            size: size(px(width), px(10.)),
        }
    }

    #[test]
    fn the_areas_of_the_caption_are_its_buttons_and_its_empty_space() {
        let areas = super::WindowControlAreas::new();
        assert!(
            areas.drawn().is_empty(),
            "a caption that did not draw has no areas"
        );

        areas.record_buttons(&[area(0., 10.), area(10., 10.), area(20., 10.)]);
        areas.record_drag(area(30., 40.));

        assert_eq!(
            areas.drawn(),
            vec![
                ("w-area-min", area(0., 10.)),
                ("w-area-max", area(10., 10.)),
                ("w-area-close", area(20., 10.)),
                ("w-area-drag", area(30., 40.)),
            ],
            "the areas are the buttons of the caption and the space that moves the window"
        );
    }

    #[test]
    fn a_window_without_the_caption_forgets_its_areas() {
        let areas = super::WindowControlAreas::new();
        areas.record_buttons(&[area(0., 10.), area(10., 10.), area(20., 10.)]);
        areas.record_drag(area(30., 40.));

        areas.clear();

        assert!(
            areas.drawn().is_empty(),
            "no area of an older frame stays where no button is drawn"
        );
    }

    #[test]
    fn the_border_of_the_window_gives_the_edge_that_resizes_it() {
        let window = size(px(800.), px(600.));
        for (position, edge) in [
            (point(px(400.), px(1.)), ResizeEdge::Top),
            (point(px(400.), px(599.)), ResizeEdge::Bottom),
            (point(px(1.), px(300.)), ResizeEdge::Left),
            (point(px(799.), px(300.)), ResizeEdge::Right),
            (point(px(1.), px(1.)), ResizeEdge::TopLeft),
            (point(px(799.), px(1.)), ResizeEdge::TopRight),
            (point(px(1.), px(599.)), ResizeEdge::BottomLeft),
            (point(px(799.), px(599.)), ResizeEdge::BottomRight),
        ] {
            assert_eq!(
                super::resize_edge_at(position, window, false),
                Some(edge),
                "a press at {position:?} resizes from {edge:?}"
            );
        }
    }

    #[test]
    fn a_press_off_the_border_resizes_nothing() {
        let window = size(px(800.), px(600.));
        let border = super::WINDOW_RESIZE_BORDER;
        for position in [
            point(px(border), px(border)),
            point(px(400.), px(300.)),
            point(px(800. - border - 1.), px(600. - border - 1.)),
            point(px(80.), px(300.)),
        ] {
            assert_eq!(
                super::resize_edge_at(position, window, false),
                None,
                "a press at {position:?} is inside the window and resizes nothing"
            );
        }
    }

    #[test]
    fn a_maximized_window_has_no_border() {
        let window = size(px(800.), px(600.));
        for position in [
            point(px(0.), px(0.)),
            point(px(1.), px(300.)),
            point(px(799.), px(1.)),
            point(px(799.), px(599.)),
            point(px(400.), px(300.)),
        ] {
            assert_eq!(
                super::resize_edge_at(position, window, true),
                None,
                "a maximized window at {position:?} has no edge to resize"
            );
        }
    }
}

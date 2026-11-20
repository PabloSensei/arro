//! The Profiler tool window: the folded stacks of one recording as a flame graph, a call
//! tree and a list of methods, by samples of the CPU or by the bytes the run allocated.
//! A timeline shows the samples of every thread over the recording. The numbers come from
//! the run panel, which gets them from the provider of the language; the window only
//! folds and draws them.

use std::cell::Cell;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;

use editor_view::Spot;
use lang_api::{
    ProfileFrame, ProfileMethod, ProfileNode, ProfileRecording, ProfileSamples, ProfileSnapshot,
    bytes_text,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{PROFILER_TOOL, Workspace};

use crate::navigation;
use crate::prompt::TextField;
use crate::run::{ProfileEntry, RunPanel};
use crate::script::ActiveRunPanel;

const GROUP: &str = "bottom-panel";
const CONTEXT: &str = "ProfilerPanel";
/// The height of one bar of the flame graph.
const FLAME_HEIGHT: f32 = 20.;
/// The width of the own and total columns of the Method List.
const COLUMN_WIDTH: f32 = 72.;
/// The width of the name of a thread in the timeline.
const LANE_WIDTH: f32 = 160.;
/// The width of the plot of the timeline before it is measured.
const TIMELINE_WIDTH: f32 = 600.;

/// Whether the window counts samples of the CPU or the bytes a run allocated.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ProfileMode {
    #[default]
    Cpu,
    Allocations,
}

impl ProfileMode {
    fn label(self) -> &'static str {
        match self {
            Self::Cpu => "CPU",
            Self::Allocations => "Allocations",
        }
    }
}

/// The tab the window shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfilerTab {
    FlameGraph,
    CallTree,
    MethodList,
    AllocatedTypes,
    Timeline,
}

impl ProfilerTab {
    fn label(self) -> &'static str {
        match self {
            Self::FlameGraph => "Flame Graph",
            Self::CallTree => "Call Tree",
            Self::MethodList => "Method List",
            Self::AllocatedTypes => "Allocated Types",
            Self::Timeline => "Timeline",
        }
    }

    /// The tabs of a mode: the timeline counts samples, the allocated types weight bytes.
    fn of(mode: ProfileMode) -> [Self; 4] {
        match mode {
            ProfileMode::Cpu => [
                Self::FlameGraph,
                Self::CallTree,
                Self::MethodList,
                Self::Timeline,
            ],
            ProfileMode::Allocations => [
                Self::FlameGraph,
                Self::CallTree,
                Self::MethodList,
                Self::AllocatedTypes,
            ],
        }
    }
}

/// The column of the Method List a click sorts by.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProfileColumn {
    Own,
    Total,
}

impl ProfileColumn {
    fn label(self) -> &'static str {
        match self {
            Self::Own => "Own",
            Self::Total => "Total",
        }
    }
}

/// One bar of the flame graph, as the window draws and a test reads it.
#[derive(Clone, Debug, PartialEq)]
pub struct FlameBar {
    /// `flame-0-1`: the path of the node in the tree.
    pub selector: String,
    pub label: String,
    /// The index of each level below the roots.
    pub path: Vec<usize>,
    /// The share of every sample of the recording, `0..=1`.
    pub share: f32,
    pub own: bool,
    /// The weight of the subtree: samples or allocated bytes.
    pub samples: u64,
    pub percent: u32,
}

/// One visible row of the call tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CallRow {
    pub label: String,
    pub path: Vec<usize>,
    pub depth: usize,
    pub own: u32,
    pub total: u32,
    pub own_samples: u64,
    pub total_samples: u64,
    pub expanded: bool,
    pub has_children: bool,
    pub source: Option<PathBuf>,
    pub line: Option<u32>,
}

/// One row of the Method List.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MethodRow {
    pub label: String,
    pub own: u32,
    pub total: u32,
    pub own_samples: u64,
    pub total_samples: u64,
    pub source: Option<PathBuf>,
    pub line: Option<u32>,
}

/// One class of the Allocated Types tab.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AllocationRow {
    pub class: String,
    /// The bytes of every allocation of the class.
    pub weight: u64,
    /// How many samples allocated it.
    pub count: u64,
    pub weight_text: String,
    pub selected: bool,
    pub source: Option<PathBuf>,
}

/// One column of the timeline: how many samples of a thread fall into it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TimelineBucket {
    /// The index of the column from the left.
    pub column: usize,
    pub count: u64,
    /// The share of the lane, `0..=1`.
    pub share: f32,
}

/// The columns of one lane of the timeline: every sample falls into the column of its
/// moment, so a long recording draws at most `width` marks and not one per sample.
pub fn timeline_buckets(samples: &ProfileSamples, lane: u32, width: usize) -> Vec<TimelineBucket> {
    let width = width.max(1);
    let mut counts = vec![0u64; width];
    let duration = samples.duration().max(1);
    let mut total = 0u64;
    for sample in &samples.samples {
        if sample.lane != lane {
            continue;
        }
        let column = ((u128::from(sample.at) * width as u128) / u128::from(duration)) as usize;
        counts[column.min(width - 1)] += 1;
        total += 1;
    }
    counts
        .into_iter()
        .enumerate()
        .filter(|(_, count)| *count > 0)
        .map(|(column, count)| TimelineBucket {
            column,
            count,
            share: match total {
                0 => 0.,
                _ => count as f32 / total as f32,
            },
        })
        .collect()
}

/// A drag in the timeline: the thread, the moment it started and the moment now.
#[derive(Clone, Copy)]
struct LaneDrag {
    lane: u32,
    from: u64,
    to: u64,
}

/// The panel of the window, for the tests.
pub struct ActiveProfiler(pub Entity<ProfilerPanel>);

impl Global for ActiveProfiler {}

pub struct ProfilerPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    mode: ProfileMode,
    tab: ProfilerTab,
    /// The recordings of the last run, one per JVM when the runtime wrote several.
    reports: Vec<ProfileEntry>,
    /// The recording the window shows.
    choice: usize,
    snapshot: Option<Arc<ProfileSnapshot>>,
    /// The allocated class the trees show, empty for every allocation.
    selected_type: Option<String>,
    /// The interval of the timeline the trees show, milliseconds from the first sample.
    range: Option<(u64, u64)>,
    /// The thread the trees show.
    lane: Option<u32>,
    drag: Option<LaneDrag>,
    /// The measured width of the plot of the timeline.
    timeline_width: Rc<Cell<f32>>,
    /// The node the flame graph shows, empty for the roots.
    zoom: Vec<usize>,
    /// The call tree nodes the user opened.
    expanded: std::collections::HashSet<Vec<usize>>,
    search: TextField,
    search_focus: FocusHandle,
    column: ProfileColumn,
    descending: bool,
    /// What the run panel says about the last recording.
    error: Option<String>,
    recording: Option<PathBuf>,
    /// A profile run records now.
    running: bool,
    /// The revision of the state that came from the run panel.
    revision: u64,
    recordings: Vec<ProfileRecording>,
    scroll_flame: ScrollHandle,
    scroll_call: UniformListScrollHandle,
    scroll_methods: UniformListScrollHandle,
    scroll_types: UniformListScrollHandle,
    scroll_timeline: UniformListScrollHandle,
}

impl ProfilerPanel {
    pub fn create(
        workspace: WeakEntity<Workspace>,
        runs: Entity<RunPanel>,
        cx: &mut App,
    ) -> Entity<Self> {
        let panel = cx.new(|cx| {
            cx.subscribe(
                &runs,
                |panel: &mut Self, runs, _: &crate::run::ProfileRecordingsChanged, cx| {
                    panel.recordings = runs.read(cx).profile_recordings().to_vec();
                    cx.notify();
                },
            )
            .detach();
            cx.subscribe(
                &runs,
                |panel: &mut Self, runs, _: &crate::run::ProfileChanged, cx| {
                    let (error, recording, running, recordings, revision, reports) = {
                        let runs = runs.read(cx);
                        (
                            runs.profile_error(),
                            runs.profile_recording().map(Path::to_path_buf),
                            runs.profile_running(),
                            runs.profile_recordings().to_vec(),
                            runs.profile_revision(),
                            runs.profile_entries().to_vec(),
                        )
                    };
                    panel.recordings = recordings;
                    panel.error = error;
                    panel.recording = recording;
                    panel.running = running;
                    if panel.revision != revision {
                        panel.revision = revision;
                        panel.reports = reports;
                        panel.choice = 0;
                        panel.selected_type = None;
                        panel.range = None;
                        panel.lane = None;
                        panel.refresh_view();
                    }
                    cx.notify();
                },
            )
            .detach();
            let (error, recording, running, recordings, revision, reports) = {
                let runs = runs.read(cx);
                (
                    runs.profile_error(),
                    runs.profile_recording().map(Path::to_path_buf),
                    runs.profile_running(),
                    runs.profile_recordings().to_vec(),
                    runs.profile_revision(),
                    runs.profile_entries().to_vec(),
                )
            };
            let mut panel = Self {
                workspace,
                focus: cx.focus_handle(),
                mode: ProfileMode::Cpu,
                tab: ProfilerTab::FlameGraph,
                reports,
                choice: 0,
                snapshot: None,
                selected_type: None,
                range: None,
                lane: None,
                drag: None,
                timeline_width: Rc::new(Cell::new(TIMELINE_WIDTH)),
                zoom: Vec::new(),
                expanded: std::collections::HashSet::new(),
                search: TextField::default(),
                search_focus: cx.focus_handle(),
                column: ProfileColumn::Total,
                descending: true,
                error,
                recording,
                running,
                revision,
                recordings,
                scroll_flame: ScrollHandle::new(),
                scroll_call: UniformListScrollHandle::new(),
                scroll_methods: UniformListScrollHandle::new(),
                scroll_types: UniformListScrollHandle::new(),
                scroll_timeline: UniformListScrollHandle::new(),
            };
            panel.refresh_view();
            panel
        });
        runs.update(cx, |runs, cx| runs.refresh_profile_recordings(cx));
        panel
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn tab(&self) -> ProfilerTab {
        self.tab
    }

    pub fn mode(&self) -> ProfileMode {
        self.mode
    }

    /// The recordings of the last run the window can choose from.
    pub fn report_count(&self) -> usize {
        self.reports.len()
    }

    /// Shows a snapshot the window did not get from a run, as a test does.
    pub fn show_snapshot(&mut self, snapshot: ProfileSnapshot, cx: &mut Context<Self>) {
        self.show_reports(
            vec![ProfileEntry {
                label: String::new(),
                report: Arc::new(lang_api::ProfileReport::of(snapshot)),
            }],
            cx,
        );
    }

    /// Shows the recordings of one run, as the list above the tabs does when a runtime
    /// wrote several files.
    pub fn show_reports(&mut self, reports: Vec<ProfileEntry>, cx: &mut Context<Self>) {
        self.reports = reports;
        self.choice = 0;
        self.error = None;
        self.selected_type = None;
        self.range = None;
        self.lane = None;
        self.refresh_view();
        cx.notify();
    }

    /// Shows another recording of the last run.
    pub fn choose_report(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.reports.len() || index == self.choice {
            return;
        }
        self.choice = index;
        self.selected_type = None;
        self.range = None;
        self.lane = None;
        self.refresh_view();
        cx.notify();
    }

    pub fn set_mode(&mut self, mode: ProfileMode, cx: &mut Context<Self>) {
        if self.mode == mode {
            return;
        }
        self.mode = mode;
        match mode {
            ProfileMode::Cpu if self.tab == ProfilerTab::AllocatedTypes => {
                self.tab = ProfilerTab::FlameGraph;
            }
            ProfileMode::Allocations if self.tab == ProfilerTab::Timeline => {
                self.tab = ProfilerTab::FlameGraph;
            }
            _ => {}
        }
        self.selected_type = None;
        self.refresh_view();
        cx.notify();
    }

    pub fn set_tab(&mut self, tab: ProfilerTab, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        self.tab = tab;
        cx.notify();
    }

    /// The node the flame graph is zoomed to.
    pub fn zoom(&self) -> &[usize] {
        &self.zoom
    }

    /// A click on a bar: the graph shows that subtree only.
    pub fn zoom_to(&mut self, path: Vec<usize>, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        if snapshot.node(&path).is_none() {
            return;
        }
        self.zoom = path;
        cx.notify();
    }

    /// Escape or the Back button: one level up of the flame graph.
    pub fn back(&mut self, cx: &mut Context<Self>) {
        if self.zoom.pop().is_some() {
            cx.notify();
        }
    }

    /// The allocation samples the Allocated Types tab lists, the heaviest first.
    pub fn allocation_rows(&self) -> Vec<AllocationRow> {
        let Some(report) = self.report() else {
            return Vec::new();
        };
        report
            .types
            .iter()
            .map(|entry| AllocationRow {
                class: entry.allocated.class.clone(),
                weight: entry.allocated.weight,
                count: entry.allocated.count,
                weight_text: bytes_text(entry.allocated.weight),
                selected: self.selected_type.as_deref() == Some(entry.allocated.class.as_str()),
                source: entry.allocated.source.clone(),
            })
            .collect()
    }

    /// A click on an allocated class: the trees show the stacks that allocated it. `None`
    /// shows every allocation again.
    pub fn choose_type(&mut self, class: Option<&str>, cx: &mut Context<Self>) {
        let class = class.map(str::to_owned);
        if self.selected_type == class {
            return;
        }
        self.selected_type = class;
        self.refresh_view();
        cx.notify();
    }

    /// The moment the trees are limited to, in milliseconds from the first sample.
    pub fn timeline_range(&self) -> Option<(u64, u64)> {
        self.range
    }

    /// The thread the trees are limited to.
    pub fn timeline_lane(&self) -> Option<u32> {
        self.lane
    }

    /// What a drag in the timeline does: the trees keep the samples of the thread in the
    /// interval. A drag of no width selects the thread alone.
    pub fn select_timeline(&mut self, lane: u32, from: u64, to: u64, cx: &mut Context<Self>) {
        let (from, to) = match from <= to {
            true => (from, to),
            false => (to, from),
        };
        self.lane = Some(lane);
        self.range = (to > from).then_some((from, to));
        self.refresh_view();
        cx.notify();
    }

    /// The button of the timeline and of the note: the trees show every sample again.
    pub fn reset_timeline(&mut self, cx: &mut Context<Self>) {
        if self.range.is_none() && self.lane.is_none() {
            return;
        }
        self.range = None;
        self.lane = None;
        self.refresh_view();
        cx.notify();
    }

    /// The columns of one thread of the timeline at `width` pixels.
    pub fn timeline_buckets(&self, lane: u32, width: usize) -> Vec<TimelineBucket> {
        match self.report() {
            Some(report) => timeline_buckets(&report.cpu, lane, width),
            None => Vec::new(),
        }
    }

    /// The names of the threads of the timeline.
    pub fn timeline_lanes(&self) -> Vec<String> {
        self.report()
            .map(|report| report.cpu.lanes.clone())
            .unwrap_or_default()
    }

    fn report(&self) -> Option<&lang_api::ProfileReport> {
        self.reports
            .get(self.choice)
            .map(|entry| entry.report.as_ref())
    }

    /// Folds the samples the filters keep into the tree the tabs show, and drops the
    /// zoom and the opened nodes, whose paths mean nothing in another tree.
    fn refresh_view(&mut self) {
        let snapshot = match self.mode {
            ProfileMode::Cpu => {
                self.report()
                    .map(|report| match self.range.is_some() || self.lane.is_some() {
                        true => report.cpu.snapshot_between(self.range, self.lane),
                        false => report.cpu_tree.clone(),
                    })
            }
            ProfileMode::Allocations => match self.report() {
                Some(report) => match &self.selected_type {
                    Some(class) => report.type_tree(class).cloned(),
                    None => report.allocations.clone(),
                },
                None => None,
            },
        };
        self.snapshot = snapshot.map(Arc::new);
        self.zoom.clear();
        self.expanded.clear();
    }

    /// The bars of the flame graph in the order it draws them.
    pub fn flame_bars(&self) -> Vec<FlameBar> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        if snapshot.samples == 0 {
            return Vec::new();
        }
        let mut bars = Vec::new();
        match self.zoom.is_empty() {
            true => {
                for (index, node) in snapshot.roots.iter().enumerate() {
                    collect_bars(node, vec![index], snapshot.samples, &mut bars);
                }
            }
            false => {
                if let Some(node) = snapshot.node(&self.zoom) {
                    collect_bars(node, self.zoom.clone(), snapshot.samples, &mut bars);
                }
            }
        }
        bars
    }

    /// The rows of the call tree: the roots, then the children of every node the user
    /// opened. A search keeps the matching frames and the frames above them.
    pub fn call_rows(&self) -> Vec<CallRow> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let query = self.search.text.trim().to_lowercase();
        let mut rows = Vec::new();
        for (index, node) in snapshot.roots.iter().enumerate() {
            let path = vec![index];
            if !query.is_empty() && !branch_matches(node, &query) {
                continue;
            }
            collect_calls(
                node,
                path,
                0,
                snapshot.samples,
                &query,
                &self.expanded,
                &mut rows,
            );
        }
        rows
    }

    pub fn toggle_call(&mut self, path: &[usize], cx: &mut Context<Self>) {
        let key = path.to_vec();
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
        cx.notify();
    }

    /// The call tree search as typed; an empty text shows the whole tree again.
    pub fn set_search(&mut self, text: &str, cx: &mut Context<Self>) {
        self.search = TextField::from_text(text);
        cx.notify();
    }

    /// Sorts the Method List by a column, the way a click on its header does.
    pub fn sort_methods(&mut self, column: ProfileColumn, cx: &mut Context<Self>) {
        if self.column == column {
            self.descending = !self.descending;
        } else {
            self.column = column;
            self.descending = true;
        }
        cx.notify();
    }

    /// The rows of the Method List in the order the window shows them.
    pub fn method_rows(&self) -> Vec<MethodRow> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let mut methods: Vec<ProfileMethod> = snapshot.methods();
        methods.sort_by(|left, right| {
            let order = match self.column {
                ProfileColumn::Own => right.own.cmp(&left.own),
                ProfileColumn::Total => right.total.cmp(&left.total),
            };
            let order = match self.descending {
                true => order,
                false => order.reverse(),
            };
            order.then_with(|| left.frame.name().cmp(&right.frame.name()))
        });
        methods
            .into_iter()
            .map(|method| MethodRow {
                label: method.frame.name(),
                own: percent(method.own, snapshot.samples),
                total: percent(method.total, snapshot.samples),
                own_samples: method.own,
                total_samples: method.total,
                source: method.frame.source,
                line: method.frame.line,
            })
            .collect()
    }

    /// The weight of a row as the window writes it: a share for samples, bytes for
    /// allocations.
    fn weight_text(&self, weight: u64, total: u64) -> String {
        match self.mode {
            ProfileMode::Cpu => format!("{}%", percent(weight, total)),
            ProfileMode::Allocations => bytes_text(weight),
        }
    }

    /// A double click on a method of the project opens its source.
    pub fn open_method(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.method_rows().get(index).cloned() else {
            return;
        };
        self.go_to(row.source, row.line, window, cx);
    }

    /// A double click on a row of the call tree opens the frame of the project.
    pub fn open_call(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.call_rows().get(index).cloned() else {
            return;
        };
        self.go_to(row.source, row.line, window, cx);
    }

    fn go_to(
        &mut self,
        source: Option<PathBuf>,
        line: Option<u32>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(path) = source else {
            return;
        };
        let spot = line.map(|line| Spot::Line {
            line: line.saturating_sub(1) as usize,
            column: 0,
        });
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    navigation::go_to(workspace, path.clone(), spot, true, window, cx)
                })
                .ok();
        });
    }

    /// The button of the header: the recordings on disk, the newest first.
    fn show_snapshots(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(runs) = cx
            .try_global::<ActiveRunPanel>()
            .map(|panel| panel.0.clone())
        else {
            return;
        };
        runs.update(cx, |runs, cx| runs.refresh_profile_recordings(cx));
        let recordings = self.recordings.clone();
        let entries: Vec<MenuEntry> = match recordings.is_empty() {
            true => vec![MenuEntry::disabled("No snapshots")],
            false => recordings
                .into_iter()
                .map(|recording| {
                    let runs = runs.clone();
                    let path = recording.path;
                    MenuEntry::new(recording.label, move |_, cx| {
                        runs.update(cx, |runs, cx| runs.open_profile_file(path.clone(), cx));
                    })
                })
                .collect(),
        };
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.show_menu(position, entries, cx)
            })
            .ok();
    }

    fn prompt_for_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open Snapshot".into()),
        });
        cx.spawn_in(window, async move |this, cx| {
            let chosen = paths.await.ok().and_then(Result::ok).flatten();
            let Some(path) = chosen.and_then(|paths| paths.into_iter().next()) else {
                return;
            };
            this.update(cx, |_, cx| {
                if let Some(runs) = cx.try_global::<ActiveRunPanel>() {
                    let runs = runs.0.clone();
                    runs.update(cx, |runs, cx| runs.open_profile_file(path, cx));
                }
            })
            .ok();
        })
        .detach();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_focus.is_focused(window) && self.search.key_with_clipboard(event, false, cx)
        {
            cx.stop_propagation();
            cx.notify();
            return;
        }
        if event.keystroke.key == "escape" && !self.zoom.is_empty() {
            cx.stop_propagation();
            self.back(cx);
        }
    }

    fn mode_button(
        &self,
        mode: ProfileMode,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.mode == mode;
        div()
            .id(("profiler-mode", mode as usize))
            .debug_selector(move || format!("profiler-mode-{}", mode.label()))
            .flex()
            .items_center()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .text_color(if selected {
                theme.text
            } else {
                theme.text_secondary
            })
            .when(selected, |this| this.bg(theme.selection))
            .when(!selected, |this| {
                this.hover(move |style| style.bg(theme.selection_hover))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.set_mode(mode, cx)))
            .child(mode.label())
    }

    fn modes(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_XS))
            .px(px(SPACE_S))
            .child(self.mode_button(ProfileMode::Cpu, theme, cx))
            .child(self.mode_button(ProfileMode::Allocations, theme, cx))
    }

    fn tab_button(&self, tab: ProfilerTab, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let selected = self.tab == tab;
        div()
            .id(("profiler-tab", tab as usize))
            .debug_selector(move || format!("profiler-tab-{}", tab.label()))
            .flex()
            .items_center()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .text_color(if selected {
                theme.text
            } else {
                theme.text_secondary
            })
            .when(selected, |this| this.bg(theme.selection))
            .when(!selected, |this| {
                this.hover(move |style| style.bg(theme.selection_hover))
            })
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.set_tab(tab, cx)))
            .child(tab.label())
    }

    /// The recordings of the last run, when the runtime wrote more than one.
    fn choices(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<Div> {
        if self.reports.len() <= 1 {
            return None;
        }
        let buttons: Vec<Stateful<Div>> = self
            .reports
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let selected = index == self.choice;
                div()
                    .id(("profiler-recording", index))
                    .debug_selector(move || format!("profiler-recording-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .text_color(if selected {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .when(selected, |this| this.bg(theme.selection))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_report(index, cx)))
                    .child(entry.label.clone())
            })
            .collect();
        Some(
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap(px(SPACE_XS))
                .px(px(SPACE_S))
                .child(
                    div()
                        .flex_none()
                        .pr(px(SPACE_XS))
                        .text_color(theme.text_secondary)
                        .child("Recordings"),
                )
                .children(buttons),
        )
    }

    fn tabs(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let back = (!self.zoom.is_empty()).then(|| {
            icon_button(
                "profiler-back",
                "general/arrowLeft",
                "Back",
                Some("Escape".into()),
                theme,
            )
            .debug_selector(|| "profiler-back".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.back(cx)))
        });
        div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_XS))
            .px(px(SPACE_S))
            .children(
                ProfilerTab::of(self.mode)
                    .into_iter()
                    .map(|tab| self.tab_button(tab, theme, cx)),
            )
            .children(back)
            .child(div().flex_1())
            .child(
                icon_button(
                    "profiler-heap",
                    "general/logs",
                    "Capture Memory Snapshot",
                    None,
                    theme,
                )
                .debug_selector(|| "profiler-heap".to_owned())
                .on_click(cx.listener(|this, _, window, cx| {
                    let workspace = this.workspace.clone();
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::heap_dump::capture(workspace, window, cx)
                        })
                        .ok();
                })),
            )
            .child(
                icon_button(
                    "profiler-snapshots",
                    "general/logs",
                    "Snapshots",
                    None,
                    theme,
                )
                .debug_selector(|| "profiler-snapshots".to_owned())
                .on_click(cx.listener(|this, event: &ClickEvent, _, cx| {
                    this.show_snapshots(event.position(), cx)
                })),
            )
            .child(
                icon_button(
                    "profiler-open",
                    "general/openInToolWindow",
                    "Open Snapshot...",
                    None,
                    theme,
                )
                .debug_selector(|| "profiler-open".to_owned())
                .on_click(cx.listener(|this, _, window, cx| this.prompt_for_file(window, cx))),
            )
    }

    fn note_text(&self) -> Option<(String, bool)> {
        if let Some(error) = &self.error {
            return Some((format!("No profile data: {error}"), true));
        }
        if self.mode == ProfileMode::Allocations
            && !self
                .report()
                .is_some_and(lang_api::ProfileReport::has_allocations)
        {
            return Some(("The recording has no allocation samples".to_owned(), false));
        }
        if let Some(snapshot) = &self.snapshot
            && snapshot.truncated
        {
            return Some((
                format!(
                    "The recording is long: the window shows the first {} {}",
                    snapshot.samples,
                    match self.mode {
                        ProfileMode::Cpu => "samples",
                        ProfileMode::Allocations => "bytes",
                    }
                ),
                false,
            ));
        }
        if let Some(lane) = self.lane
            && let Some(name) = self.timeline_lanes().get(lane as usize)
        {
            return Some((
                match self.range {
                    Some((from, to)) => {
                        format!("Timeline: {name}, {from}–{to} ms of the recording")
                    }
                    None => format!("Timeline: {name}"),
                },
                false,
            ));
        }
        if self.running
            && let Some(recording) = &self.recording
        {
            let name = recording
                .file_name()
                .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
            return Some((format!("Recording {name}"), false));
        }
        None
    }

    fn flame_body(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let Some(snapshot) = &self.snapshot else {
            return match self.mode {
                ProfileMode::Cpu => empty("No profile data", theme),
                ProfileMode::Allocations => empty("The recording has no allocation samples", theme),
            }
            .into_any_element();
        };
        if snapshot.samples == 0 {
            return empty("The recording has no samples", theme).into_any_element();
        }
        let nodes: Vec<Div> = match self.zoom.is_empty() {
            true => snapshot
                .roots
                .iter()
                .enumerate()
                .map(|(index, node)| {
                    self.flame_node(node, vec![index], snapshot.samples, theme, cx)
                })
                .collect(),
            false => match snapshot.node(&self.zoom) {
                Some(node) => {
                    vec![self.flame_node(node, self.zoom.clone(), node.total.max(1), theme, cx)]
                }
                None => Vec::new(),
            },
        };
        div()
            .flex()
            .w_full()
            .p(px(SPACE_M))
            .children(nodes)
            .into_any_element()
    }

    fn flame_node(
        &self,
        node: &ProfileNode,
        path: Vec<usize>,
        denominator: u64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let share = share(node.total, denominator);
        let selector_path = path.clone();
        let zoom_path = path.clone();
        let label = node.frame.name();
        let samples = node.total;
        let percent = self
            .snapshot
            .as_ref()
            .map_or(0, |snapshot| percent(samples, snapshot.samples));
        let hint = match self.mode {
            ProfileMode::Cpu => format!("{label}\n{samples} samples · {percent}%"),
            ProfileMode::Allocations => format!("{label}\n{} allocated", bytes_text(samples)),
        };
        let color = frame_color(&node.frame, theme.dark);
        let bar = div()
            .id(("profiler-flame-bar", path_hash(&path)))
            .debug_selector(move || {
                let path: Vec<String> = selector_path.iter().map(usize::to_string).collect();
                format!("flame-{}", path.join("-"))
            })
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(FLAME_HEIGHT))
            .px(px(SPACE_S))
            .overflow_hidden()
            .rounded(px(ROW_RADIUS))
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text)
            .bg(color)
            .cursor_pointer()
            .tooltip(move |_, cx| tooltip(hint.clone(), None, cx))
            .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.zoom_to(zoom_path.clone(), cx)
            }))
            .child(div().flex_1().min_w_0().truncate().child(label));
        let children: Vec<Div> = node
            .children
            .iter()
            .enumerate()
            .map(|(index, child)| {
                let mut child_path = path.clone();
                child_path.push(index);
                self.flame_node(child, child_path, node.total, theme, cx)
            })
            .collect();
        div()
            .flex()
            .flex_col()
            .flex_none()
            .w(relative(share))
            .min_w_0()
            .child(bar)
            .child(div().flex().w_full().min_w_0().children(children))
    }

    fn call_body(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *theme;
        let rows = self.call_rows();
        let count = rows.len();
        let search = div()
            .id("profiler-call-search")
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .mx(px(SPACE_M))
            .mb(px(SPACE_XS))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.border)
            .track_focus(&self.search_focus)
            .on_click(cx.listener(|this, _, window, cx| window.focus(&this.search_focus, cx)))
            .child(icon("general/search", &theme))
            .child(
                div()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .children(text_field_children(
                        FieldMouse::new(cx, |this| Some(&mut this.search)),
                        &self.search,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        16.,
                        self.search_focus.is_focused(window),
                    )),
            );
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(search)
            .child(
                scrollable(
                    "profiler-calls",
                    &self.scroll_call,
                    div().size_full().child(
                        uniform_list(
                            "profiler-calls",
                            count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                this.call_rows_elements(range, &theme, cx)
                            }),
                        )
                        .track_scroll(&self.scroll_call)
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    fn call_rows_elements(
        &self,
        range: std::ops::Range<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let rows = self.call_rows();
        range
            .filter_map(|index| rows.get(index).cloned().map(|row| (index, row)))
            .map(|(index, row)| self.call_row_element(index, row, theme, cx))
            .collect()
    }

    fn call_row_element(
        &self,
        index: usize,
        row: CallRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let path = row.path.clone();
        let label = row.label.clone();
        let hint = match (row.source.as_ref(), row.line) {
            (Some(path), Some(line)) => format!("{}:{line}", path.display()),
            (Some(path), None) => path.display().to_string(),
            _ => label.clone(),
        };
        let source = row.source.is_some();
        let total_weight = self
            .snapshot
            .as_ref()
            .map_or(1, |snapshot| snapshot.samples.max(1));
        let own = self.weight_text(row.own_samples, total_weight);
        let total = self.weight_text(row.total_samples, total_weight);
        div()
            .id(("profiler-call", index))
            .debug_selector(move || format!("profiler-call-{index}"))
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_L + row.depth as f32 * SPACE_L))
            .pr(px(SPACE_M))
            .rounded(px(ROW_RADIUS))
            .hover(move |style| style.bg(theme.selection_hover))
            .tooltip(move |_, cx| tooltip(hint.clone(), None, cx))
            .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                window.focus(&this.focus, cx);
                if event.click_count() >= 2 {
                    this.open_call(index, window, cx);
                } else {
                    this.toggle_call(&path, cx);
                }
            }))
            .child(
                div()
                    .w(px(SPACE_L))
                    .flex_none()
                    .text_color(theme.text_secondary)
                    .child(if row.has_children {
                        if row.expanded { "▾" } else { "▸" }
                    } else {
                        ""
                    }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if source {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(row.label.clone()),
            )
            .child(
                div()
                    .w(px(COLUMN_WIDTH))
                    .flex_none()
                    .text_color(theme.text_secondary)
                    .child(own),
            )
            .child(div().w(px(COLUMN_WIDTH)).flex_none().child(total))
    }

    fn methods_body(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *theme;
        let rows = self.method_rows();
        let count = rows.len();
        let header = div()
            .flex()
            .items_center()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_M))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(UI_SIZE))
            .text_color(theme.text_secondary)
            .child(div().flex_1().min_w_0().child("Method"))
            .children(
                [ProfileColumn::Own, ProfileColumn::Total]
                    .into_iter()
                    .enumerate()
                    .map(|(index, column)| {
                        let selected = self.column == column;
                        div()
                            .id(("profiler-method-column", index))
                            .debug_selector(move || {
                                format!("profiler-method-column-{}", column.label())
                            })
                            .w(px(COLUMN_WIDTH))
                            .flex_none()
                            .text_color(if selected {
                                theme.text
                            } else {
                                theme.text_secondary
                            })
                            .cursor_pointer()
                            .on_click(
                                cx.listener(move |this, _, _, cx| this.sort_methods(column, cx)),
                            )
                            .child(column.label())
                    }),
            );
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(
                scrollable(
                    "profiler-methods",
                    &self.scroll_methods,
                    div().size_full().child(
                        uniform_list(
                            "profiler-methods",
                            count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                this.method_rows_elements(range, &theme, cx)
                            }),
                        )
                        .track_scroll(&self.scroll_methods)
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    fn method_rows_elements(
        &self,
        range: std::ops::Range<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let rows = self.method_rows();
        range
            .filter_map(|index| rows.get(index).cloned().map(|row| (index, row)))
            .map(|(index, row)| self.method_row_element(index, row, theme, cx))
            .collect()
    }

    fn method_row_element(
        &self,
        index: usize,
        row: MethodRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let hint = match (row.source.as_ref(), row.line) {
            (Some(path), Some(line)) => format!("{}:{line}", path.display()),
            (Some(path), None) => path.display().to_string(),
            _ => row.label.clone(),
        };
        let source = row.source.is_some();
        let total = self
            .snapshot
            .as_ref()
            .map_or(row.total_samples.max(1), |snapshot| snapshot.samples);
        let own = self.weight_text(row.own_samples, total);
        let total = self.weight_text(row.total_samples, total);
        div()
            .id(("profiler-method", index))
            .debug_selector(move || format!("profiler-method-{index}"))
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_M))
            .rounded(px(ROW_RADIUS))
            .hover(move |style| style.bg(theme.selection_hover))
            .tooltip(move |_, cx| tooltip(hint.clone(), None, cx))
            .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                window.focus(&this.focus, cx);
                if event.click_count() >= 2 {
                    this.open_method(index, window, cx);
                }
            }))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if source {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(row.label.clone()),
            )
            .child(
                div()
                    .w(px(COLUMN_WIDTH))
                    .flex_none()
                    .text_color(theme.text_secondary)
                    .child(own),
            )
            .child(div().w(px(COLUMN_WIDTH)).flex_none().child(total))
    }

    fn types_body(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *theme;
        if !self
            .report()
            .is_some_and(lang_api::ProfileReport::has_allocations)
        {
            let message = match &self.error {
                Some(_) => "No profile data",
                None => "The recording has no allocation samples",
            };
            return empty(message, &theme).into_any_element();
        }
        let rows = self.allocation_rows();
        let count = rows.len();
        let filter = self.selected_type.clone().map(|class| {
            let clear = icon_button(
                "profiler-alloc-clear",
                "general/close",
                "Show every allocation",
                None,
                &theme,
            )
            .debug_selector(|| "profiler-alloc-clear".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.choose_type(None, cx)));
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap(px(SPACE_S))
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .text_color(theme.text_secondary)
                .child(format!("Only {class}"))
                .child(clear)
        });
        let header = div()
            .flex()
            .items_center()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_M))
            .border_b_1()
            .border_color(theme.border)
            .text_size(px(UI_SIZE))
            .text_color(theme.text_secondary)
            .child(div().flex_1().min_w_0().child("Class"))
            .child(div().w(px(COLUMN_WIDTH)).flex_none().child("Weight"))
            .child(div().w(px(COLUMN_WIDTH)).flex_none().child("Count"));
        div()
            .flex()
            .flex_col()
            .size_full()
            .children(filter)
            .child(header)
            .child(
                scrollable(
                    "profiler-types",
                    &self.scroll_types,
                    div().size_full().child(
                        uniform_list(
                            "profiler-types",
                            count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                this.type_rows_elements(range, &theme, cx)
                            }),
                        )
                        .track_scroll(&self.scroll_types)
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    fn type_rows_elements(
        &self,
        range: std::ops::Range<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        let rows = self.allocation_rows();
        range
            .filter_map(|index| rows.get(index).cloned().map(|row| (index, row)))
            .map(|(index, row)| {
                let class = row.class.clone();
                let selected = row.selected;
                let hint = match &row.source {
                    Some(source) => source.display().to_string(),
                    None => row.class.clone(),
                };
                div()
                    .id(("allocated-type", index))
                    .debug_selector(move || format!("allocated-type-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .pl(px(SPACE_M))
                    .pr(px(SPACE_M))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .hover(move |style| style.bg(theme.selection_hover))
                    .tooltip(move |_, cx| tooltip(hint.clone(), None, cx))
                    .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        window.focus(&this.focus, cx);
                        match selected {
                            true => this.choose_type(None, cx),
                            false => this.choose_type(Some(&class), cx),
                        }
                    }))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_color(theme.text)
                            .child(row.class.clone()),
                    )
                    .child(
                        div()
                            .w(px(COLUMN_WIDTH))
                            .flex_none()
                            .text_color(theme.text_secondary)
                            .child(row.weight_text.clone()),
                    )
                    .child(
                        div()
                            .w(px(COLUMN_WIDTH))
                            .flex_none()
                            .child(row.count.to_string()),
                    )
            })
            .collect()
    }

    fn timeline_body(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *theme;
        let Some(report) = self.report() else {
            return empty("No profile data", &theme).into_any_element();
        };
        if report.cpu.samples.is_empty() {
            return empty("The recording has no CPU samples", &theme).into_any_element();
        }
        let filtered = self.range.is_some() || self.lane.is_some();
        let reset = filtered.then(|| {
            icon_button(
                "profiler-timeline-reset",
                "general/close",
                "Show every sample",
                None,
                &theme,
            )
            .debug_selector(|| "profiler-timeline-reset".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.reset_timeline(cx)))
        });
        let width = self.timeline_width.get().max(1.).round() as usize;
        let lanes: Vec<Lane> = report
            .cpu
            .lanes
            .iter()
            .enumerate()
            .map(|(index, name)| {
                let buckets = timeline_buckets(&report.cpu, index as u32, width);
                let peak = buckets.iter().map(|bucket| bucket.count).max().unwrap_or(1);
                Lane {
                    name: name.clone(),
                    buckets,
                    peak,
                    width,
                }
            })
            .collect();
        let count = lanes.len();
        let duration = report.cpu.duration().max(1);
        let header = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .h(px(ROW_HEIGHT))
            .px(px(SPACE_M))
            .text_color(theme.text_secondary)
            .child(format!(
                "Drag over a thread to keep its samples of the interval, {count} threads"
            ))
            .children(reset);
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(header)
            .child(
                scrollable(
                    "profiler-timeline",
                    &self.scroll_timeline,
                    div().size_full().child(
                        uniform_list(
                            "profiler-timeline",
                            count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                this.timeline_rows(range, &lanes, duration, &theme, cx)
                            }),
                        )
                        .track_scroll(&self.scroll_timeline)
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .into_any_element()
    }

    fn timeline_rows(
        &self,
        range: std::ops::Range<usize>,
        lanes: &[Lane],
        duration: u64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        range
            .map(|lane| self.timeline_row(lane, lanes, duration, theme, cx))
            .collect()
    }

    fn timeline_row(
        &self,
        lane: usize,
        lanes: &[Lane],
        duration: u64,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let Some(data) = lanes.get(lane) else {
            return div().id(("profiler-lane", lane));
        };
        let selected_lane = self.lane == Some(lane as u32);
        let range = self.range.filter(|_| selected_lane);
        let lane_id = lane as u32;
        let rows: Vec<Div> = data
            .buckets
            .iter()
            .map(|bucket| {
                let intensity = bucket.count as f32 / data.peak.max(1) as f32;
                let color = hsla(0.58, 0.7, 0.55, 0.12 + 0.88 * intensity);
                let column = bucket.column as f32 / data.width as f32;
                div()
                    .absolute()
                    .top_0()
                    .bottom_0()
                    .left(relative(column))
                    .w(relative(1. / data.width as f32))
                    .bg(color)
            })
            .collect();
        let selection = range.map(|(from, to)| {
            let left = from as f32 / duration as f32;
            let span = to.saturating_sub(from) as f32 / duration as f32;
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(relative(left))
                .w(relative(span.max(0.001)))
                .bg(hsla(0.58, 0.8, 0.6, 0.25))
        });
        let bounds: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
        let measure = canvas(
            {
                let bounds = bounds.clone();
                let width = self.timeline_width.clone();
                move |row, _, _| {
                    bounds.set(Some(row));
                    let measured = f32::from(row.size.width);
                    if measured > 0. {
                        width.set(measured);
                    }
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .top_0()
        .left_0()
        .size_full();
        fn moment(event_x: Pixels, bounds: Bounds<Pixels>, duration: u64) -> u64 {
            let width = f32::from(bounds.size.width).max(1.);
            let x = (f32::from(event_x) - f32::from(bounds.origin.x)).clamp(0., width);
            ((x / width) * duration as f32) as u64
        }
        let down = {
            let bounds = bounds.clone();
            move |this: &mut Self,
                  event: &MouseDownEvent,
                  _: &mut Window,
                  cx: &mut Context<Self>| {
                let Some(bounds) = bounds.get() else {
                    return;
                };
                let at = moment(event.position.x, bounds, duration);
                this.drag = Some(LaneDrag {
                    lane: lane_id,
                    from: at,
                    to: at,
                });
                cx.notify();
            }
        };
        let moved = {
            let bounds = bounds.clone();
            move |this: &mut Self,
                  event: &MouseMoveEvent,
                  _: &mut Window,
                  cx: &mut Context<Self>| {
                let Some(mut drag) = this.drag else {
                    return;
                };
                if drag.lane != lane_id {
                    return;
                }
                let Some(bounds) = bounds.get() else {
                    return;
                };
                drag.to = moment(event.position.x, bounds, duration);
                this.drag = Some(drag);
                cx.notify();
            }
        };
        let up =
            move |this: &mut Self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>| {
                let Some(drag) = this.drag.take() else {
                    return;
                };
                if drag.lane != lane_id {
                    return;
                }
                this.select_timeline(drag.lane, drag.from, drag.to, cx);
            };
        div()
            .id(("profiler-lane", lane))
            .debug_selector(move || format!("profiler-lane-{lane}"))
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_M))
            .when(selected_lane, |this| this.bg(theme.selection_hover))
            .child(
                div()
                    .w(px(LANE_WIDTH))
                    .flex_none()
                    .min_w_0()
                    .truncate()
                    .text_color(if selected_lane {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(data.name.clone()),
            )
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_1()
                    .min_w_0()
                    .h(px(FLAME_HEIGHT))
                    .rounded(px(ROW_RADIUS))
                    .bg(theme.island_bg)
                    .cursor_pointer()
                    .on_mouse_down(MouseButton::Left, cx.listener(down))
                    .on_mouse_move(cx.listener(moved))
                    .on_mouse_up(MouseButton::Left, cx.listener(up))
                    .children(rows)
                    .children(selection)
                    .child(measure),
            )
    }
}

/// One thread of the timeline with its columns, folded by the width of the plot.
struct Lane {
    name: String,
    buckets: Vec<TimelineBucket>,
    peak: u64,
    width: usize,
}

fn percent(part: u64, total: u64) -> u32 {
    match total {
        0 => 0,
        _ => (part.saturating_mul(100) / total) as u32,
    }
}

fn share(part: u64, total: u64) -> f32 {
    match total {
        0 => 0.,
        _ => part as f32 / total as f32,
    }
}

fn collect_bars(node: &ProfileNode, path: Vec<usize>, samples: u64, bars: &mut Vec<FlameBar>) {
    bars.push(FlameBar {
        selector: format!(
            "flame-{}",
            path.iter()
                .map(usize::to_string)
                .collect::<Vec<_>>()
                .join("-")
        ),
        label: node.frame.name(),
        share: share(node.total, samples),
        own: node.frame.is_own(),
        samples: node.total,
        percent: percent(node.total, samples),
        path: path.clone(),
    });
    for (index, child) in node.children.iter().enumerate() {
        let mut child_path = path.clone();
        child_path.push(index);
        collect_bars(child, child_path, samples, bars);
    }
}

fn branch_matches(node: &ProfileNode, query: &str) -> bool {
    node.frame.name().to_lowercase().contains(query)
        || node
            .children
            .iter()
            .any(|child| branch_matches(child, query))
}

#[allow(clippy::too_many_arguments)]
fn collect_calls(
    node: &ProfileNode,
    path: Vec<usize>,
    depth: usize,
    samples: u64,
    query: &str,
    expanded: &std::collections::HashSet<Vec<usize>>,
    rows: &mut Vec<CallRow>,
) {
    let open = expanded.contains(&path) || !query.is_empty();
    rows.push(CallRow {
        label: node.frame.name(),
        path: path.clone(),
        depth,
        own: percent(node.own, samples),
        total: percent(node.total, samples),
        own_samples: node.own,
        total_samples: node.total,
        expanded: open,
        has_children: !node.children.is_empty(),
        source: node.frame.source.clone(),
        line: node.frame.line,
    });
    if !open {
        return;
    }
    for (index, child) in node.children.iter().enumerate() {
        if !query.is_empty() && !branch_matches(child, query) {
            continue;
        }
        let mut child_path = path.clone();
        child_path.push(index);
        collect_calls(child, child_path, depth + 1, samples, query, expanded, rows);
    }
}

/// The colour of a bar: a hue of its package, louder for code of the project.
fn frame_color(frame: &ProfileFrame, dark: bool) -> Hsla {
    let hash = frame.package_or_class().bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    let hue = (hash % 360) as f32 / 360.;
    match frame.is_own() {
        true => hsla(hue, 0.62, if dark { 0.46 } else { 0.72 }, 1.),
        false => hsla(hue, 0.16, if dark { 0.34 } else { 0.8 }, 1.),
    }
}

fn path_hash(path: &[usize]) -> u64 {
    path.iter().fold(1469598103934665603u64, |hash, index| {
        (hash ^ *index as u64).wrapping_mul(1099511628211)
    })
}

fn empty(text: &str, theme: &Theme) -> Div {
    div()
        .p(px(SPACE_L))
        .text_color(theme.text_secondary)
        .child(text.to_owned())
}

impl Render for ProfilerPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "profiler");
        let theme = *cx.global::<Theme>();
        let workspace = self.workspace.clone();
        let options =
            crate::tool_menu::options_menu(workspace.clone(), workspace::PROFILER_TOOL, |_| {
                Vec::new()
            });
        let hide = move |_: &ClickEvent, window: &mut Window, cx: &mut App| {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.toggle_tool(PROFILER_TOOL, window, cx)
                })
                .ok();
        };
        let note = self.note_text().map(|(text, error)| {
            div()
                .flex_none()
                .px(px(SPACE_M))
                .py(px(SPACE_XS))
                .text_color(if error {
                    theme.stripe_error
                } else {
                    theme.text_secondary
                })
                .child(text)
        });
        let body = match self.tab {
            ProfilerTab::FlameGraph => scrollable(
                "profiler-flame",
                &self.scroll_flame,
                div().size_full().child(self.flame_body(&theme, cx)),
                &theme,
                window,
                cx,
            )
            .flex_1()
            .min_h_0()
            .into_any_element(),
            ProfilerTab::CallTree => self.call_body(&theme, window, cx),
            ProfilerTab::MethodList => self.methods_body(&theme, window, cx),
            ProfilerTab::AllocatedTypes => self.types_body(&theme, window, cx),
            ProfilerTab::Timeline => self.timeline_body(&theme, window, cx),
        };
        island(&theme)
            .id(PROFILER_TOOL)
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(panel_header(
                "Profiler", GROUP, "profiler", options, hide, &theme,
            ))
            .child(self.modes(&theme, cx))
            .child(self.tabs(&theme, cx))
            .children(self.choices(&theme, cx))
            .children(note)
            .child(body)
    }
}

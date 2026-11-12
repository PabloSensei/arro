//! The Performance Monitor tool window: the latency percentiles of the subsystems and
//! the memory of the IDE, its child processes and the stores it keeps in memory.
//!
//! Sampling runs on the background executor once a second. The window only swaps a
//! finished snapshot into a field, so a repaint never waits for a process or a lock.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use ::base::perf::{self, MemorySnapshot, MetricSnapshot};
use ::platform::ProcessMemory;
use editor_view::EditorView;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{PERFORMANCE_TOOL, Workspace};

/// How often a finished snapshot replaces the one on screen.
const REFRESH: Duration = Duration::from_secs(1);
/// The window the percentiles describe.
const WINDOW: Duration = Duration::from_secs(300);
/// The values of `budgets.toml`, embedded at build time: the panel must show the same
/// numbers CI checks, without reading a file at runtime.
const BUDGETS: &str = include_str!("../../../../budgets.toml");

/// The budget of a metric, where a metric id in `budgets.toml` describes it.
fn budget_of(metric: &str) -> Option<&'static Budget> {
    let id = match metric {
        "editor.keystroke" => "keydown_to_present_p95",
        "search.files" => "find_in_files_15k_files_warm",
        "index.scan" => "source_index_15k_files",
        "git.command" => "git_status_update",
        // The server budgets are pending, so their ids are found by prefix and state.
        "lsp.completion" => return pending_budget("completion_"),
        "lsp.definition" => return pending_budget("ctrl_b_"),
        _ => return None,
    };
    budgets().get(id)
}

/// A budget whose value waits for a spike: only its id and the pending mark are known.
fn pending_budget(prefix: &str) -> Option<&'static Budget> {
    budgets()
        .iter()
        .find(|(id, budget)| budget.pending && id.starts_with(prefix))
        .map(|(_, budget)| budget)
}

#[derive(Default)]
struct Budget {
    limit: Option<f64>,
    unit: Option<String>,
    pending: bool,
}

fn budgets() -> &'static HashMap<&'static str, Budget> {
    static MAP: OnceLock<HashMap<&'static str, Budget>> = OnceLock::new();
    MAP.get_or_init(|| parse_budgets(BUDGETS))
}

fn parse_budgets(text: &str) -> HashMap<&str, Budget> {
    let mut found = HashMap::new();
    let mut current = Budget::default();
    let mut id: Option<&str> = None;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line == "[[metric]]" {
            if let Some(name) = id.take() {
                found.insert(name, std::mem::take(&mut current));
            }
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let value = value.trim();
        match key.trim() {
            "id" => id = Some(unquote(value)),
            "limit" => current.limit = value.parse().ok(),
            "unit" => current.unit = Some(unquote(value).to_owned()),
            "pending" => current.pending = true,
            _ => {}
        }
    }
    if let Some(name) = id {
        found.insert(name, current);
    }
    found
}

fn unquote(value: &str) -> &str {
    value.trim().trim_matches('"')
}

/// Whether the p95 of `metric` is over the limit of its budget.
fn over_budget(metric: &str, p95: Option<Duration>) -> bool {
    let (Some(budget), Some(p95)) = (budget_of(metric), p95) else {
        return false;
    };
    let (Some(limit), Some(unit)) = (budget.limit, budget.unit.as_deref()) else {
        return false;
    };
    let value = match unit {
        "ms" => p95.as_secs_f64() * 1000.,
        _ => return false,
    };
    value > limit
}

fn budget_text(metric: &str) -> String {
    let Some(budget) = budget_of(metric) else {
        return String::new();
    };
    if budget.pending {
        return "pending".to_owned();
    }
    match (budget.limit, budget.unit.as_deref()) {
        (Some(limit), Some(unit)) => format!("{} {unit}", number(limit)),
        _ => String::new(),
    }
}

fn number(value: f64) -> String {
    let text = format!("{value:.3}");
    text.trim_end_matches('0').trim_end_matches('.').to_owned()
}

fn millis(value: Option<Duration>) -> String {
    value.map_or_else(
        || "—".to_owned(),
        |value| format!("{:.3}", value.as_secs_f64() * 1000.),
    )
}

fn megabytes(bytes: u64) -> String {
    format!("{:.1}", bytes as f64 / (1024. * 1024.))
}

#[derive(Default)]
struct Sample {
    metrics: Vec<MetricSnapshot>,
    processes: Vec<ProcessMemory>,
    subsystems: Vec<MemorySnapshot>,
}

/// The panel of the window, for scripts and tests.
#[allow(dead_code)]
pub struct ActivePerformance(pub Entity<PerformancePanel>);

impl Global for ActivePerformance {}

pub struct PerformancePanel {
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    sample: Arc<Sample>,
    scroll: ScrollHandle,
    /// The sampling loop runs only while the tool window shows the result.
    running: bool,
}

impl PerformancePanel {
    pub fn create(workspace: WeakEntity<Workspace>, cx: &mut Context<Workspace>) -> Entity<Self> {
        let observer = workspace.clone();
        cx.new(|cx| {
            let panel = Self {
                focus: cx.focus_handle(),
                workspace,
                sample: Arc::new(Sample::default()),
                scroll: ScrollHandle::new(),
                running: false,
            };
            if let Some(workspace) = observer.upgrade() {
                cx.observe(&workspace, |this: &mut Self, _, cx| {
                    this.visibility_changed(cx)
                })
                .detach();
            }
            panel
        })
    }

    /// A closed tool window keeps the last table and asks nothing: no clock, no process.
    fn visibility_changed(&mut self, cx: &mut Context<Self>) {
        let open = self
            .workspace
            .read_with(cx, |workspace, _| workspace.tool_open(PERFORMANCE_TOOL))
            .unwrap_or(false);
        if open {
            self.start(cx);
        } else {
            self.running = false;
        }
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// Events of one metric in this snapshot, zero when it was never recorded.
    #[allow(dead_code)]
    pub(crate) fn count(&self, metric: &str) -> u64 {
        self.sample
            .metrics
            .iter()
            .find(|row| row.name == metric)
            .map_or(0, |row| row.count)
    }

    /// The table as Copy as Text writes it.
    pub(crate) fn report(&self) -> String {
        let mut text = String::from("Performance Monitor\n");
        text.push_str(&format!("Latency, last {} s\n", WINDOW.as_secs()));
        text.push_str("Metric\tp50 ms\tp95 ms\tp99 ms\tEvents\tBudget\n");
        for row in &self.sample.metrics {
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\t{}\n",
                row.name,
                millis(row.p50),
                millis(row.p95),
                millis(row.p99),
                row.count,
                budget_text(row.name)
            ));
        }
        text.push_str("\nMemory by process\n");
        text.push_str("Process\tPID\tWorking set MB\tPrivate working set MB\tPrivate bytes MB\n");
        for process in &self.sample.processes {
            let name = match process.role {
                Some(role) => format!("{role} ({})", process.name),
                None => process.name.clone(),
            };
            text.push_str(&format!(
                "{}\t{}\t{}\t{}\t{}\n",
                name,
                process.pid,
                megabytes(process.working_set),
                megabytes(process.private_working_set),
                megabytes(process.private_bytes)
            ));
        }
        text.push_str("\nMemory by subsystem\n");
        text.push_str("Subsystem\tMB\n");
        for entry in &self.sample.subsystems {
            text.push_str(&format!("{}\t{}\n", entry.name, megabytes(entry.bytes)));
        }
        text
    }

    fn copy(&mut self, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(self.report()));
        cx.notify();
    }

    fn start(&mut self, cx: &mut Context<Self>) {
        if self.running {
            return;
        }
        self.running = true;
        cx.spawn(async move |this, cx| {
            loop {
                let shown = this.update(cx, |this, _| this.running);
                if !matches!(shown, Ok(true)) {
                    break;
                }
                // Local History counts its versions under its own lock, which a save
                // also takes: that count runs with the rest of the sample, off the window.
                let history = this
                    .update(cx, |_, cx| crate::history::get(cx))
                    .ok()
                    .flatten();
                let collected = cx
                    .background_executor()
                    .spawn(async move {
                        (
                            perf::snapshot(WINDOW),
                            ::platform::process_tree(),
                            perf::memory(),
                            history.map_or(0, |history| history.memory_bytes()),
                        )
                    })
                    .await;
                let alive = this.update(cx, |this, cx| {
                    let buffers = this.buffer_bytes(cx);
                    let history = collected.3;
                    let mut subsystems = collected.2;
                    subsystems.push(MemorySnapshot {
                        name: "Open buffers",
                        bytes: buffers,
                    });
                    subsystems.push(MemorySnapshot {
                        name: "Local history",
                        bytes: history,
                    });
                    subsystems.sort_by_key(|entry| entry.name);
                    this.sample = Arc::new(Sample {
                        metrics: collected.0,
                        processes: collected.1,
                        subsystems,
                    });
                    cx.notify();
                });
                if alive.is_err() {
                    break;
                }
                cx.background_executor().timer(REFRESH).await;
            }
        })
        .detach();
    }

    fn buffer_bytes(&self, cx: &mut Context<Self>) -> u64 {
        self.workspace
            .update(cx, |workspace, cx| {
                workspace
                    .open_editors()
                    .into_iter()
                    .filter_map(|(_, view)| view.downcast::<EditorView>().ok())
                    .map(|view| view.read(cx).buffer_bytes() as u64)
                    .sum()
            })
            .unwrap_or(0)
    }

    fn metric_rows(&self, theme: &Theme) -> Vec<AnyElement> {
        let mut rows = vec![self.header_row(
            vec!["Metric", "p50", "p95", "p99", "Events", "Budget"],
            theme,
        )];
        if self.sample.metrics.is_empty() {
            rows.push(self.empty_row("No latency was recorded yet.", theme));
            return rows;
        }
        for row in &self.sample.metrics {
            let colour = if over_budget(row.name, row.p95) {
                theme.close_hover
            } else {
                theme.text
            };
            rows.push(
                self.cells(
                    [
                        row.name.to_owned(),
                        millis(row.p50),
                        millis(row.p95),
                        millis(row.p99),
                        row.count.to_string(),
                        budget_text(row.name),
                    ],
                    colour,
                )
                .into_any_element(),
            );
        }
        rows
    }

    fn process_rows(&self, theme: &Theme) -> Vec<AnyElement> {
        let mut rows = vec![self.header_row(
            vec![
                "Process",
                "PID",
                "Working set MB",
                "Private working set MB",
                "Private bytes MB",
            ],
            theme,
        )];
        for process in &self.sample.processes {
            let name = match process.role {
                Some(role) => format!("{role} ({})", process.name),
                None => process.name.clone(),
            };
            rows.push(
                self.cells5(
                    [
                        name,
                        process.pid.to_string(),
                        megabytes(process.working_set),
                        megabytes(process.private_working_set),
                        megabytes(process.private_bytes),
                    ],
                    theme.text,
                )
                .into_any_element(),
            );
        }
        rows
    }

    fn subsystem_rows(&self, theme: &Theme) -> Vec<AnyElement> {
        let mut rows = vec![self.header_row(vec!["Subsystem", "MB"], theme)];
        for entry in &self.sample.subsystems {
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .gap(px(SPACE_M))
                    .text_size(px(UI_SIZE))
                    .text_color(theme.text)
                    .child(self.cell(entry.name.to_owned(), px(190.)))
                    .child(self.cell(megabytes(entry.bytes), px(80.)))
                    .into_any_element(),
            );
        }
        rows
    }

    fn header_row(&self, cells: Vec<&'static str>, theme: &Theme) -> AnyElement {
        let widths = widths();
        div()
            .flex()
            .items_center()
            .w_full()
            .h(px(ROW_HEIGHT))
            .gap(px(SPACE_M))
            .text_size(px(UI_SIZE))
            .text_color(theme.text_muted)
            .children(
                cells
                    .into_iter()
                    .zip(widths)
                    .map(|(text, width)| self.cell(text.to_owned(), width)),
            )
            .into_any_element()
    }

    fn cells(&self, cells: [String; 6], colour: Hsla) -> Div {
        let widths = widths();
        div()
            .flex()
            .items_center()
            .w_full()
            .h(px(ROW_HEIGHT))
            .gap(px(SPACE_M))
            .text_size(px(UI_SIZE))
            .text_color(colour)
            .children(
                cells
                    .into_iter()
                    .zip(widths)
                    .map(|(text, width)| self.cell(text, width)),
            )
    }

    fn cells5(&self, cells: [String; 5], colour: Hsla) -> Div {
        let widths = widths();
        div()
            .flex()
            .items_center()
            .w_full()
            .h(px(ROW_HEIGHT))
            .gap(px(SPACE_M))
            .text_size(px(UI_SIZE))
            .text_color(colour)
            .children(
                cells
                    .into_iter()
                    .zip(widths)
                    .map(|(text, width)| self.cell(text, width)),
            )
    }

    fn cell(&self, text: String, width: Pixels) -> Div {
        div().w(width).overflow_hidden().child(text)
    }

    fn empty_row(&self, text: &'static str, theme: &Theme) -> AnyElement {
        div()
            .text_size(px(UI_SIZE))
            .text_color(theme.text_secondary)
            .child(text)
            .into_any_element()
    }

    fn section(&self, title: &'static str, theme: &Theme) -> Div {
        div()
            .mt(px(SPACE_L))
            .font_weight(FontWeight::MEDIUM)
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .child(title)
    }

    fn render_body(&self, theme: &Theme, window: &mut Window, cx: &mut App) -> AnyElement {
        let metric_rows = self.metric_rows(theme);
        let process_rows = self.process_rows(theme);
        let subsystem_rows = self.subsystem_rows(theme);
        scrollable(
            "performance-body",
            &self.scroll,
            div()
                .id("performance-body")
                .flex()
                .flex_col()
                .w_full()
                .px(px(SPACE_L))
                .pb(px(SPACE_M))
                .gap(px(2.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll)
                .child(self.section("Latency", theme))
                .children(metric_rows)
                .child(self.section("Memory by process", theme))
                .children(process_rows)
                .child(self.section("Memory by subsystem", theme))
                .children(subsystem_rows),
            theme,
            window,
            cx,
        )
        .flex_1()
        .min_h_0()
        .w_full()
        .into_any_element()
    }
}

/// The widths of a row of the tables, shared by headers and cells.
fn widths() -> [Pixels; 6] {
    [px(190.), px(80.), px(80.), px(80.), px(70.), px(110.)]
}

impl Render for PerformancePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Performance Monitor"),
            )
            .child(
                div()
                    .id("perf-copy")
                    .debug_selector(|| "perf-copy".to_owned())
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(px(4.))
                    .text_size(px(UI_SIZE))
                    .cursor_pointer()
                    .hover(move |style| style.bg(theme.hover))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _: &MouseDownEvent, _, cx| this.copy(cx)),
                    )
                    .child("Copy as Text"),
            );
        let body = self.render_body(&theme, window, cx);
        island(&theme)
            .id(PERFORMANCE_TOOL)
            .debug_selector(|| "performance".to_owned())
            .key_context("PerformanceMonitor ToolWindow")
            .track_focus(&self.focus)
            .group("bottom-panel")
            .h_full()
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                let key = &event.keystroke;
                if key.modifiers.control && !key.modifiers.alt && key.key == "c" {
                    cx.stop_propagation();
                    this.copy(cx);
                }
            }))
            .child(header)
            .child(div().flex_1().min_h_0().w_full().child(body))
    }
}

#[cfg(test)]
mod tests {
    use super::{budget_text, over_budget, parse_budgets};
    use std::time::Duration;

    #[test]
    fn a_budget_file_is_read() {
        let text = r#"
            [[metric]]
            id = "a"
            unit = "ms"
            limit = 12
            applies_from = "M11"

            [[metric]]
            id = "b"
            unit = "ms"
            pending = "S9"
            applies_from = "M11"
        "#;
        let found = parse_budgets(text);
        assert_eq!(found["a"].limit, Some(12.));
        assert!(found["b"].pending);
    }

    #[test]
    fn only_a_present_limit_marks_red() {
        assert!(!over_budget(
            "nothing.maps.here",
            Some(Duration::from_secs(1))
        ));
        assert!(!over_budget("lsp.completion", Some(Duration::from_secs(1))));
        assert!(!over_budget(
            "editor.keystroke",
            Some(Duration::from_millis(1))
        ));
        assert!(over_budget(
            "editor.keystroke",
            Some(Duration::from_millis(13))
        ));
        assert_eq!(budget_text("lsp.completion"), "pending");
    }
}

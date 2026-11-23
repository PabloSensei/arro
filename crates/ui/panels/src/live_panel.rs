//! The live tab of the Run window: the sections a running application publishes about
//! itself. The language reads the data and names the places; this panel only shows the
//! tables, the filter and the buttons.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use ::http_client::{Cancel, Error, Method, PreparedRequest, Response, execute};
use editor_view::Spot;
use runner::{LiveApplication, LiveNote, LiveRow, LiveSection, LiveTable, LiveTarget, LiveTone};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::endpoints;

const GROUP: &str = "bottom-panel";
const FILTER_CONTEXT: &str = "LivePanel";
/// The wait of one section read.
const TIMEOUT: Duration = Duration::from_secs(10);

/// What a section shows now.
#[derive(Clone, Default)]
enum SectionState {
    #[default]
    Idle,
    Loading,
    Ready(LiveTable),
    Failed(String),
    NotExposed(LiveNote),
}

/// One live tab of one run. The data of each section comes when it is opened, and again
/// when Refresh asks for it.
pub struct LivePanel {
    app: Arc<dyn LiveApplication>,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    section: LiveSection,
    states: [SectionState; 4],
    filters: [TextField; 4],
    selected: [Option<usize>; 4],
    scrolls: [UniformListScrollHandle; 4],
    loading: bool,
    stopped: bool,
    /// Bumped per read, so a late answer of an old section is dropped.
    generation: u64,
    cancel: Option<Cancel>,
    /// A dependency the build file lacks.
    missing: Option<LiveNote>,
}

impl LivePanel {
    pub fn create(
        app: Arc<dyn LiveApplication>,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let missing = app.missing();
            Self {
                app,
                workspace,
                focus: cx.focus_handle(),
                section: LiveSection::Health,
                states: std::array::from_fn(|_| SectionState::default()),
                filters: std::array::from_fn(|_| TextField::default()),
                selected: [None; 4],
                scrolls: std::array::from_fn(|_| UniformListScrollHandle::new()),
                loading: false,
                stopped: false,
                generation: 0,
                cancel: None,
                missing,
            }
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    pub fn title(&self) -> String {
        self.app.title()
    }

    pub fn section(&self) -> LiveSection {
        self.section
    }

    pub fn started(&self) -> bool {
        self.app.started()
    }

    pub fn stopped(&self) -> bool {
        self.stopped
    }

    /// The port the application listens on, when the language knows it.
    pub fn app_port(&self) -> Option<u16> {
        self.app.app_port()
    }

    pub fn missing(&self) -> Option<&LiveNote> {
        self.missing.as_ref()
    }

    pub fn note(&self, section: LiveSection) -> Option<&LiveNote> {
        match &self.states[section.index()] {
            SectionState::NotExposed(note) => Some(note),
            _ => None,
        }
    }

    pub fn error(&self, section: LiveSection) -> Option<&str> {
        match &self.states[section.index()] {
            SectionState::Failed(message) => Some(message),
            _ => None,
        }
    }

    pub fn loading(&self) -> bool {
        self.loading
    }

    /// The table of a section as it came, without the filter.
    pub fn table(&self, section: LiveSection) -> Option<&LiveTable> {
        match &self.states[section.index()] {
            SectionState::Ready(table) => Some(table),
            _ => None,
        }
    }

    /// The rows the section shows with its filter.
    pub fn rows(&self, section: LiveSection) -> Vec<LiveRow> {
        let Some(table) = self.table(section) else {
            return Vec::new();
        };
        let query = self.filters[section.index()].text.trim().to_lowercase();
        if query.is_empty() {
            return table.rows.clone();
        }
        table
            .rows
            .iter()
            .filter(|row| {
                row.cells
                    .iter()
                    .any(|cell| cell.to_lowercase().contains(&query))
            })
            .cloned()
            .collect()
    }

    pub fn set_section(&mut self, section: LiveSection, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        self.section = section;
        self.selected[section.index()] = None;
        self.load_if_needed(cx);
        cx.notify();
    }

    pub fn set_query(
        &mut self,
        section: LiveSection,
        text: impl Into<String>,
        cx: &mut Context<Self>,
    ) {
        self.filters[section.index()] = TextField::from_text(text);
        self.selected[section.index()] = None;
        cx.notify();
    }

    /// Refresh of the open section: it is read again even when it was read before.
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if self.stopped || self.missing.is_some() {
            return;
        }
        self.states[self.section.index()] = SectionState::Idle;
        self.load_if_needed(cx);
        cx.notify();
    }

    /// A new line of the console. The language decides what it means.
    pub fn observe(&mut self, line: &str, cx: &mut Context<Self>) {
        if !self.app.observe(line) {
            return;
        }
        self.load_if_needed(cx);
        cx.notify();
    }

    /// The program is over: the data stays, the header says it stopped.
    pub fn stop(&mut self, cx: &mut Context<Self>) {
        if self.stopped {
            return;
        }
        self.stopped = true;
        if let Some(cancel) = self.cancel.take() {
            cancel.cancel();
        }
        self.generation += 1;
        self.loading = false;
        cx.notify();
    }

    /// A section is read only when it is opened, the application started and the section
    /// has no answer yet. Refresh is the only other way to ask.
    fn load_if_needed(&mut self, cx: &mut Context<Self>) {
        if self.stopped || self.missing.is_some() || !self.app.started() {
            return;
        }
        if !matches!(self.states[self.section.index()], SectionState::Idle) {
            return;
        }
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(endpoint) = self.app.endpoint() else {
            return;
        };
        let Some(path) = self.app.section_path(self.section) else {
            return;
        };
        let url = format!("http://{}:{}{path}", endpoint.host, endpoint.port);
        let app = self.app.clone();
        let section = self.section;
        let request = PreparedRequest {
            name: None,
            method: Method::Get,
            url,
            headers: Vec::new(),
            body: None,
            line: 0,
        };
        let cancel = Cancel::new();
        if let Some(previous) = self.cancel.replace(cancel.clone()) {
            previous.cancel();
        }
        self.generation += 1;
        let generation = self.generation;
        self.states[section.index()] = SectionState::Loading;
        self.loading = true;
        let job = cx
            .background_executor()
            .spawn(async move { execute(&request, TIMEOUT, &cancel) });
        cx.spawn(async move |this, cx| {
            let answer = job.await;
            this.update(cx, |this, cx| {
                this.finish(section, generation, answer, app, cx)
            })
            .ok();
        })
        .detach();
    }

    fn finish(
        &mut self,
        section: LiveSection,
        generation: u64,
        answer: Result<Response, Error>,
        app: Arc<dyn LiveApplication>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation || self.stopped {
            return;
        }
        self.loading = false;
        let state = match answer {
            Ok(response) if response.status == 404 => SectionState::NotExposed(app.not_exposed()),
            Ok(response) if (200..=299).contains(&response.status) => {
                match app.read(section, &response.text()) {
                    Ok(table) => SectionState::Ready(table),
                    Err(message) => SectionState::Failed(message),
                }
            }
            Ok(response) => SectionState::Failed(format!(
                "The server answered {} {}",
                response.status, response.reason
            )),
            Err(error) => SectionState::Failed(error.to_string()),
        };
        self.states[section.index()] = state;
        cx.notify();
    }

    /// A double click or Enter on a row opens the place of the bean or of the handler.
    pub fn open_row(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows(self.section).get(index).cloned() else {
            return;
        };
        let Some(target) = row.target else {
            return;
        };
        let Some((path, offset)) = self.place_of(&target, cx) else {
            let _ = self.workspace.update(cx, |workspace, cx| {
                workspace.notify_user(
                    "Live Data",
                    "The source of this name is not in the project",
                    cx,
                );
            });
            return;
        };
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    crate::navigation::go_to(
                        workspace,
                        path,
                        Some(Spot::Offset(offset)),
                        true,
                        window,
                        cx,
                    )
                })
                .ok();
        });
    }

    /// The place of a target in the project index: a type by its name, a method in the
    /// type that declares it.
    fn place_of(&self, target: &LiveTarget, cx: &mut Context<Self>) -> Option<(PathBuf, usize)> {
        let (class, method) = match target {
            LiveTarget::Class(class) => (class.as_str(), None),
            LiveTarget::Method { class, method } => (class.as_str(), Some(method.as_str())),
        };
        let simple = class.rsplit(['.', '$']).next().unwrap_or(class);
        let index = cx.try_global::<crate::index::ActiveIndex>()?.0.clone();
        let symbols = index
            .symbols
            .read()
            .unwrap_or_else(|poison| poison.into_inner());
        let found = symbols
            .search(
                simple,
                |symbol| symbol.kind.is_type() && symbol.name == simple,
                8,
            )
            .into_iter()
            .next()?;
        let path = found.item.path.clone();
        let offset = match method {
            None => found.item.symbol.name_range.start,
            Some(method) => {
                let symbols = symbols.file(&path)?;
                let symbol = symbols.iter().find(|symbol| {
                    symbol.name == method
                        && symbol
                            .container
                            .as_deref()
                            .is_some_and(|container| container.ends_with(simple))
                })?;
                symbol.name_range.start
            }
        };
        Some((path, offset))
    }

    /// Open in HTTP Client: the handler becomes a request of the live port.
    pub fn open_request(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows(self.section).get(index).cloned() else {
            return;
        };
        let Some(request) = row.request else {
            return;
        };
        let Some(url) = self.app.app_url(&request.path) else {
            return;
        };
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            let _ = workspace.update(cx, |workspace, cx| {
                endpoints::generate_url(&request, &url, workspace, window, cx)
            });
        });
    }

    /// Opens the settings file of the hint, such as the one that turns an endpoint on.
    pub fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(file) = self
            .missing
            .as_ref()
            .and_then(|note| note.file.clone())
            .or_else(|| self.note(self.section).and_then(|note| note.file.clone()))
        else {
            return;
        };
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            if !file.is_file() {
                let _ = workspace.update(cx, |workspace, cx| {
                    workspace.notify_error(
                        "Live Data",
                        format!("{} does not exist", file.display()),
                        cx,
                    );
                });
                return;
            }
            let _ = workspace.update(cx, |workspace, cx| {
                crate::navigation::go_to(workspace, file.clone(), None, true, window, cx)
            });
        });
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" => {
                self.move_selection(1, cx);
                cx.stop_propagation();
            }
            "up" => {
                self.move_selection(-1, cx);
                cx.stop_propagation();
            }
            "enter" => {
                if let Some(row) = self.selected[self.section.index()] {
                    self.open_row(row, window, cx);
                }
                cx.stop_propagation();
            }
            _ => {
                let section = self.section;
                if self.filters[section.index()].key_tool_field(event, false, cx) {
                    self.selected[section.index()] = None;
                    cx.stop_propagation();
                    cx.notify();
                }
            }
        }
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let count = self.rows(self.section).len();
        if count == 0 {
            return;
        }
        let next = match self.selected[self.section.index()] {
            Some(current) => (current as isize + delta).clamp(0, count as isize - 1) as usize,
            None if delta < 0 => count - 1,
            None => 0,
        };
        self.selected[self.section.index()] = Some(next);
        cx.notify();
    }

    fn section_tab(
        &self,
        section: LiveSection,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.section == section;
        div()
            .id(("live-section", section.index()))
            .debug_selector(move || format!("actuator-section-{}", section.index()))
            .flex()
            .items_center()
            .flex_none()
            .h(px(22.))
            .px(px(8.))
            .rounded(px(CONTROL_RADIUS))
            .truncate()
            .text_color(if selected {
                theme.text
            } else {
                theme.text_secondary
            })
            .when(selected, |this| this.bg(theme.selection))
            .when(!selected, |this| {
                this.hover(move |style| style.bg(theme.selection_hover))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.set_section(section, cx)))
            .child(section.title())
    }

    fn row_element(
        &self,
        index: usize,
        row: &LiveRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.selected[self.section.index()] == Some(index) && !row.group;
        let cells = row.cells.clone();
        let depth = row.depth;
        let tone = row.tone;
        let group = row.group;
        let request = row.request.clone();
        let stopped = self.stopped;
        let color = match tone {
            LiveTone::Good => theme.semantic_type,
            LiveTone::Bad => theme.stripe_error,
            LiveTone::Muted => theme.text_muted,
            LiveTone::Normal => theme.text,
        };
        let text_color = if group {
            theme.text_secondary
        } else if stopped {
            theme.text_muted
        } else {
            color
        };
        let map_request = request.is_some();
        let mut element = div()
            .id(("live-row", index))
            .debug_selector(move || format!("actuator-row-{index}"))
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(ROW_HEIGHT))
            .gap(px(SPACE_S))
            .pl(px(SPACE_M + depth as f32 * SPACE_L))
            .pr(px(SPACE_M))
            .rounded(px(ROW_RADIUS))
            .when(selected, |this| this.bg(theme.selection))
            .when(!group, |this| {
                this.hover(move |style| style.bg(theme.selection_hover))
            })
            .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                window.focus(&this.focus, cx);
                if !group {
                    this.selected[this.section.index()] = Some(index);
                    if event.click_count() >= 2 {
                        this.open_row(index, window, cx);
                    }
                    cx.notify();
                }
            }));
        for (column, cell) in cells.iter().enumerate() {
            let cell_color = if column == 0 { text_color } else { theme.text };
            let cell_color = if stopped {
                theme.text_muted
            } else {
                cell_color
            };
            let badge = column == 0 && matches!(tone, LiveTone::Good | LiveTone::Bad);
            element = element.child(
                div()
                    .when(column == 0, |this| this.flex_none().w(px(120.)))
                    .when(column != 0, |this| this.flex_1().min_w_0())
                    .truncate()
                    .text_color(cell_color)
                    .when(badge, |this| this.font_weight(FontWeight::MEDIUM))
                    .child(cell.clone()),
            );
        }
        if map_request {
            element =
                element.child(
                    button_frame(("live-http", index), "Open in HTTP Client", None, theme)
                        .debug_selector(move || format!("actuator-http-{index}"))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open_request(index, window, cx)
                        }))
                        .child(
                            div()
                                .px(px(SPACE_S))
                                .text_size(px(TEXT_SMALL))
                                .child("Open in HTTP Client"),
                        ),
                );
        }
        element
    }
}

impl Render for LivePanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "actuator");
        let theme = *cx.global::<Theme>();
        let section = self.section;
        let rows = self.rows(section);
        let count = rows.len();
        let table_columns = self
            .table(section)
            .map(|table| table.columns.clone())
            .unwrap_or_default();
        let sections: Vec<AnyElement> = LiveSection::ALL
            .into_iter()
            .map(|found| self.section_tab(found, &theme, cx).into_any_element())
            .collect();
        let refresh = button_frame("actuator-refresh", "Refresh", None, &theme)
            .debug_selector(|| "actuator-refresh".to_owned())
            .on_click(cx.listener(|this, _, _, cx| this.refresh(cx)))
            .child(icon("run/restart", &theme));
        let has_filter = section != LiveSection::Health;
        let search = has_filter.then(|| {
            let query = &self.filters[section.index()];
            div()
                .flex()
                .items_center()
                .flex_none()
                .gap(px(SPACE_S))
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .child(icon("general/search", &theme))
                .child(
                    div()
                        .id("actuator-filter")
                        .debug_selector(|| "actuator-filter".to_owned())
                        .flex_1()
                        .min_w_0()
                        .flex()
                        .items_center()
                        .overflow_hidden()
                        .whitespace_nowrap()
                        .on_click(cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
                        .children(text_field_children_named(
                            FieldMouse::with_edit(
                                cx,
                                move |this: &mut Self| this.filters.get_mut(section.index()),
                                move |this: &mut Self, _| this.selected[section.index()] = None,
                            ),
                            "actuator-filter",
                            query,
                            theme.text,
                            theme.text,
                            theme.selection,
                            theme.caret,
                            16.,
                            true,
                        )),
                )
                .when(!query.text.is_empty(), |this| {
                    this.child(
                        icon_button(
                            "actuator-clear",
                            "general/clear",
                            "Clear Filter",
                            None,
                            &theme,
                        )
                        .debug_selector(|| "actuator-clear".to_owned())
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.set_query(section, "", cx)),
                        ),
                    )
                })
        });
        let note = self.missing.clone().or_else(|| self.note(section).cloned());
        let body: AnyElement = if let Some(note) = note {
            note_element(note, &theme, cx)
        } else if !self.started() && !self.stopped {
            div()
                .debug_selector(|| "actuator-waiting".to_owned())
                .child(message_element(
                    "Waiting for the application to start",
                    &theme,
                ))
                .into_any_element()
        } else if let Some(message) = self.error(section).map(str::to_owned) {
            message_element(&message, &theme)
        } else if count == 0 {
            if self.loading() {
                message_element("Loading", &theme)
            } else {
                message_element("No data", &theme)
            }
        } else {
            let header = div()
                .flex()
                .items_center()
                .flex_none()
                .h(px(CONTROL_HEIGHT))
                .gap(px(SPACE_S))
                .px(px(SPACE_M))
                .text_color(theme.text_secondary)
                .text_size(px(TEXT_SMALL))
                .children(table_columns.into_iter().enumerate().map(|(column, name)| {
                    div()
                        .when(column == 0, |this| this.flex_none().w(px(120.)))
                        .when(column != 0, |this| this.flex_1().min_w_0())
                        .truncate()
                        .child(name)
                }));
            let stopped = self.stopped;
            div()
                .flex()
                .flex_col()
                .size_full()
                .when(stopped, |this| this.opacity(0.65))
                .child(header)
                .child(
                    scrollable(
                        "actuator-rows",
                        &self.scrolls[section.index()],
                        div().size_full().child(
                            uniform_list(
                                "actuator-rows",
                                count,
                                cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                    let rows = this.rows(this.section);
                                    range
                                        .filter_map(|index| {
                                            rows.get(index).map(|row| (index, row.clone()))
                                        })
                                        .map(|(index, row)| {
                                            this.row_element(index, &row, &theme, cx)
                                        })
                                        .collect::<Vec<_>>()
                                }),
                            )
                            .track_scroll(&self.scrolls[section.index()])
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
        };
        let stopped = self.stopped.then(|| {
            div()
                .flex_none()
                .debug_selector(|| "actuator-stopped".to_owned())
                .px(px(SPACE_M))
                .py(px(SPACE_XS))
                .text_color(theme.text_muted)
                .text_size(px(TEXT_SMALL))
                .child("The application has stopped")
        });
        div()
            .flex()
            .flex_col()
            .size_full()
            .key_context(FILTER_CONTEXT)
            .track_focus(&self.focus)
            .group(GROUP)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .gap(px(SPACE_S))
                    .px(px(SPACE_M))
                    .pt(px(SPACE_S))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(SPACE_S))
                            .overflow_hidden()
                            .children(sections),
                    )
                    .child(refresh),
            )
            .children(search)
            .children(stopped)
            .child(div().flex_1().min_h_0().child(body))
            .capture_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
    }
}

fn message_element(message: &str, theme: &Theme) -> AnyElement {
    div()
        .flex()
        .items_center()
        .justify_center()
        .size_full()
        .p(px(SPACE_L))
        .text_color(theme.text_secondary)
        .child(message.to_owned())
        .into_any_element()
}

fn note_element(note: LiveNote, theme: &Theme, cx: &mut Context<LivePanel>) -> AnyElement {
    let open = note.file.is_some().then(|| {
        let label = note
            .label
            .clone()
            .unwrap_or_else(|| "Open Settings".to_owned());
        button_frame("actuator-open-settings", label.clone(), None, theme)
            .debug_selector(|| "actuator-open-settings".to_owned())
            .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
            .child(div().px(px(SPACE_S)).text_size(px(TEXT_SMALL)).child(label))
    });
    div()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(SPACE_S))
        .size_full()
        .p(px(SPACE_L))
        .text_color(theme.text_secondary)
        .child(note.text)
        .children(open)
        .into_any_element()
}

/// The title of the live tab with the dot that marks a started application.
pub fn live_tab_label(title: &str, started: bool, stopped: bool, theme: &Theme) -> AnyElement {
    let color = if stopped {
        theme.text_disabled
    } else if started {
        theme.semantic_type
    } else {
        theme.text_muted
    };
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_XS))
        .child(div().size(px(6.)).rounded_full().bg(color))
        .child(title.to_owned())
        .into_any_element()
}

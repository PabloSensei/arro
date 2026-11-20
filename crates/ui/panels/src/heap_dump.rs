//! The tab of a heap dump: the histogram of the classes, the dominator tree, the biggest
//! objects and the strings of the heap, with the instances of a class and the path from an
//! instance to its nearest root below. The window only reads the model of the language.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use editor_view::Spot;
use futures_util::StreamExt;
use lang_api::{HeapProgress, HeapSnapshot, bytes_text};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::navigation;
use crate::prompt::TextField;
use crate::script::ActiveRunPanel;

use ::base::Cancel;

/// The width of the numeric columns of the tables.
const COLUMN_WIDTH: f32 = 92.;
/// The height of the lower part with the instances and the details.
const LOWER_HEIGHT: f32 = 280.;

/// The part of a heap dump the tab shows.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HeapSection {
    #[default]
    Classes,
    Dominators,
    Biggest,
    Strings,
}

impl HeapSection {
    fn label(self) -> &'static str {
        match self {
            Self::Classes => "Classes",
            Self::Dominators => "Dominators",
            Self::Biggest => "Biggest objects",
            Self::Strings => "Strings",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Classes => "classes",
            Self::Dominators => "dominators",
            Self::Biggest => "biggest",
            Self::Strings => "strings",
        }
    }
}

/// The column a click sorts a table by.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HeapColumn {
    #[default]
    Retained,
    Shallow,
    Count,
    Name,
}

impl HeapColumn {
    fn label(self) -> &'static str {
        match self {
            Self::Retained => "Retained",
            Self::Shallow => "Shallow",
            Self::Count => "Count",
            Self::Name => "Name",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Self::Retained => "retained",
            Self::Shallow => "shallow",
            Self::Count => "count",
            Self::Name => "name",
        }
    }
}

/// One row of the class histogram.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeapClassRow {
    /// The index of the class in the snapshot, for a click.
    pub index: usize,
    pub name: String,
    pub package: String,
    pub count: usize,
    pub shallow: u64,
    pub retained: u64,
    pub source: Option<PathBuf>,
    pub selected: bool,
}

/// One object row of a tree or of a list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeapObjectRow {
    pub object: u32,
    pub class: String,
    pub address: u64,
    pub shallow: u64,
    pub retained: u64,
    /// The depth in the dominator tree.
    pub depth: usize,
    pub selected: bool,
}

/// One instance of the selected class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeapInstanceRow {
    pub object: u32,
    pub class: String,
    pub address: u64,
    pub shallow: u64,
    pub retained: u64,
    pub selected: bool,
    pub expanded: bool,
}

/// One field of an instance, or one element of an array.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeapFieldRow {
    pub name: String,
    pub value: String,
    /// The object the value points to, for a click.
    pub object: Option<u32>,
}

/// One step of the path from a root to an object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeapPathRow {
    pub object: u32,
    pub class: String,
    pub address: u64,
    pub root: bool,
    /// The object the path ends at.
    pub target: bool,
}

/// One group of equal strings.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeapStringRow {
    pub index: usize,
    pub text: String,
    pub count: usize,
    pub wasted: u64,
    /// The strings with the same text, for a chosen row.
    pub objects: Vec<u32>,
}

/// The state of a reading the window shows in the tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Loading,
    Loaded,
    Failed(String),
}

pub struct HeapDumpView {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    path: PathBuf,
    snapshot: Option<Arc<HeapSnapshot>>,
    state: State,
    progress: HeapProgress,
    section: HeapSection,
    filter: TextField,
    filter_focus: FocusHandle,
    column: HeapColumn,
    descending: bool,
    /// The class whose instances the lower part shows.
    class: Option<usize>,
    /// The instance whose fields and path the lower part shows.
    object: Option<u32>,
    /// The instances the user opened in the list.
    expanded: HashSet<u32>,
    /// The rows of the dominator tree, built once for a snapshot.
    dominators: Arc<Vec<(u32, usize)>>,
    /// The objects with the largest retained size, the largest first.
    biggest: Arc<Vec<(u32, usize)>>,
    /// The instances of the chosen class, the largest retained size first.
    instances: Arc<Vec<u32>>,
    /// The objects the Strings section lists.
    string_objects: Vec<Vec<u32>>,
    scroll_classes: UniformListScrollHandle,
    scroll_tree: UniformListScrollHandle,
    scroll_objects: UniformListScrollHandle,
    scroll_instances: UniformListScrollHandle,
    scroll_details: ScrollHandle,
    /// Stops the reading when the tab is closed.
    cancel: Cancel,
}

impl HeapDumpView {
    /// Opens an empty tab and reads `path` in the background. The window does not wait for
    /// the reading; the rows appear when the language is done.
    pub fn open(
        workspace: &mut Workspace,
        path: PathBuf,
        provider: Arc<dyn runner::RunConfigProvider>,
        model: project_model::ProjectModel,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let view = Self::open_loading(workspace, path.clone(), window, cx);
        let cancel = view.read(cx).cancel.clone();
        let (sender, mut receiver) = futures_channel::mpsc::unbounded();
        let read_path = path.clone();
        let reading = view.downgrade();
        // The reading and the rows of the tab both take long on a dump of millions of
        // objects, so the whole of it runs off the UI thread.
        let task = cx.background_executor().spawn(async move {
            let mut progress = |progress: HeapProgress| {
                let _ = sender.unbounded_send(progress);
            };
            provider
                .heap_dump(&read_path, &model, &cancel, &mut progress)
                .map(Prepared::new)
        });
        cx.spawn_in(window, async move |_, cx| {
            while let Some(progress) = receiver.next().await {
                reading
                    .update_in(cx, |view, _, cx| view.set_progress(progress, cx))
                    .ok();
            }
            let result = task.await;
            reading
                .update_in(cx, |view, _, cx| match result {
                    Ok(prepared) => view.set_prepared(prepared, cx),
                    Err(error) => view.set_error(error.to_string(), cx),
                })
                .ok();
        })
        .detach();
        view
    }

    /// Opens a tab with a snapshot the caller has read already.
    pub fn open_snapshot(
        workspace: &mut Workspace,
        path: PathBuf,
        snapshot: HeapSnapshot,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let view = Self::open_loading(workspace, path.clone(), window, cx);
        view.update(cx, |view, cx| view.set_snapshot(snapshot, cx));
        view
    }

    fn open_loading(
        workspace: &mut Workspace,
        path: PathBuf,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let weak = cx.weak_entity();
        let title = path
            .file_name()
            .map_or_else(String::new, |name| name.to_string_lossy().into_owned());
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            path: path.clone(),
            snapshot: None,
            state: State::Loading,
            progress: HeapProgress::default(),
            section: HeapSection::Classes,
            filter: TextField::default(),
            filter_focus: cx.focus_handle(),
            column: HeapColumn::Retained,
            descending: true,
            class: None,
            object: None,
            expanded: HashSet::new(),
            dominators: Arc::new(Vec::new()),
            biggest: Arc::new(Vec::new()),
            instances: Arc::new(Vec::new()),
            string_objects: Vec::new(),
            scroll_classes: UniformListScrollHandle::new(),
            scroll_tree: UniformListScrollHandle::new(),
            scroll_objects: UniformListScrollHandle::new(),
            scroll_instances: UniformListScrollHandle::new(),
            scroll_details: ScrollHandle::new(),
            cancel: Cancel::new(),
        });
        let focus = view.read(cx).focus.clone();
        workspace.open_view_tab(
            path,
            title,
            "general/logs",
            view.clone().into(),
            focus,
            window,
            cx,
        );
        view
    }

    pub fn set_progress(&mut self, progress: HeapProgress, cx: &mut Context<Self>) {
        self.progress = progress;
        cx.notify();
    }

    pub fn set_snapshot(&mut self, snapshot: HeapSnapshot, cx: &mut Context<Self>) {
        self.set_prepared(Prepared::new(snapshot), cx);
    }

    fn set_prepared(&mut self, prepared: Prepared, cx: &mut Context<Self>) {
        self.dominators = prepared.dominators;
        self.biggest = prepared.biggest;
        self.string_objects = prepared.string_objects;
        self.snapshot = Some(prepared.snapshot);
        self.state = State::Loaded;
        self.class = None;
        self.object = None;
        self.instances = Arc::new(Vec::new());
        self.expanded.clear();
        cx.notify();
    }

    pub fn set_error(&mut self, error: String, cx: &mut Context<Self>) {
        self.state = State::Failed(error);
        cx.notify();
    }

    pub fn section(&self) -> HeapSection {
        self.section
    }

    pub fn set_section(&mut self, section: HeapSection, cx: &mut Context<Self>) {
        if self.section == section {
            return;
        }
        self.section = section;
        cx.notify();
    }

    pub fn filter(&self) -> &str {
        &self.filter.text
    }

    pub fn set_filter(&mut self, text: &str, cx: &mut Context<Self>) {
        self.filter.text = text.to_owned();
        cx.notify();
    }

    pub fn sort(&self) -> (HeapColumn, bool) {
        (self.column, self.descending)
    }

    pub fn set_sort(&mut self, column: HeapColumn, descending: bool, cx: &mut Context<Self>) {
        self.column = column;
        self.descending = descending;
        cx.notify();
    }

    /// A click on a header: the same column flips, a new column starts with the largest
    /// numbers first.
    fn sort_by(&mut self, column: HeapColumn, cx: &mut Context<Self>) {
        let descending = if self.column == column {
            !self.descending
        } else {
            column != HeapColumn::Name
        };
        self.set_sort(column, descending, cx);
    }

    /// The classes of the histogram, the filter applied, in the order of the sort.
    pub fn class_rows(&self) -> Vec<HeapClassRow> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        let filter = self.filter.text.trim().to_lowercase();
        let mut rows: Vec<HeapClassRow> = snapshot
            .classes
            .iter()
            .enumerate()
            .filter(|(_, class)| {
                filter.is_empty()
                    || class.name.to_lowercase().contains(&filter)
                    || class.package.to_lowercase().contains(&filter)
            })
            .map(|(index, class)| HeapClassRow {
                index,
                name: class.name.clone(),
                package: class.package.clone(),
                count: class.count,
                shallow: class.shallow,
                retained: class.retained,
                source: class.source.clone(),
                selected: self.class == Some(index),
            })
            .collect();
        rows.sort_by(|left, right| {
            let order = match self.column {
                HeapColumn::Name => left.name.cmp(&right.name),
                HeapColumn::Count => left.count.cmp(&right.count),
                HeapColumn::Shallow => left.shallow.cmp(&right.shallow),
                HeapColumn::Retained => left.retained.cmp(&right.retained),
            };
            let order = if self.descending {
                order.reverse()
            } else {
                order
            };
            order.then_with(|| left.name.cmp(&right.name))
        });
        rows
    }

    /// The dominator tree, the largest retained size first at every level.
    pub fn dominator_rows(&self) -> Vec<HeapObjectRow> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        self.dominators
            .iter()
            .map(|(object, depth)| self.object_row(snapshot, *object, *depth))
            .collect()
    }

    /// The objects with the largest retained size.
    pub fn biggest_rows(&self) -> Vec<HeapObjectRow> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        self.biggest
            .iter()
            .map(|(object, depth)| self.object_row(snapshot, *object, *depth))
            .collect()
    }

    fn object_row(&self, snapshot: &HeapSnapshot, object: u32, depth: usize) -> HeapObjectRow {
        let instance = &snapshot.objects[object as usize];
        let class = snapshot
            .class_of(object)
            .map_or_else(String::new, |class| class.name.clone());
        HeapObjectRow {
            object,
            class,
            address: instance.address,
            shallow: instance.shallow,
            retained: instance.retained,
            depth,
            selected: self.object == Some(object),
        }
    }

    /// The groups of equal strings, the most wasted memory first.
    pub fn string_rows(&self) -> Vec<HeapStringRow> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        snapshot
            .strings
            .iter()
            .enumerate()
            .map(|(index, entry)| HeapStringRow {
                index,
                text: entry.text.to_string(),
                count: entry.count,
                wasted: entry.wasted,
                objects: self.string_objects.get(index).cloned().unwrap_or_default(),
            })
            .collect()
    }

    /// The class whose instances the lower part shows.
    pub fn chosen_class(&self) -> Option<usize> {
        self.class
    }

    /// A click on a class: the lower part lists its instances.
    pub fn choose_class(&mut self, index: Option<usize>, cx: &mut Context<Self>) {
        self.class = index;
        self.object = None;
        self.expanded.clear();
        self.instances = Arc::new(self.class_instances());
        cx.notify();
    }

    fn class_instances(&self) -> Vec<u32> {
        let (Some(snapshot), Some(class)) = (&self.snapshot, self.class) else {
            return Vec::new();
        };
        let mut objects: Vec<u32> = snapshot
            .objects
            .iter()
            .enumerate()
            .filter(|(_, object)| object.class as usize == class)
            .map(|(index, _)| index as u32)
            .collect();
        objects
            .sort_by_key(|object| std::cmp::Reverse(snapshot.objects[*object as usize].retained));
        objects
    }

    pub fn chosen_object(&self) -> Option<u32> {
        self.object
    }

    /// A click on an instance: the lower part shows its fields and its path to a root.
    pub fn choose_object(&mut self, object: Option<u32>, cx: &mut Context<Self>) {
        self.object = object;
        if let Some(object) = object {
            if self.expanded.contains(&object) {
                self.expanded.remove(&object);
            } else {
                self.expanded.insert(object);
            }
        }
        cx.notify();
    }

    pub fn expanded(&self, object: u32) -> bool {
        self.expanded.contains(&object)
    }

    /// The instances of the chosen class, the ones with the largest retained size first.
    pub fn instance_rows(&self) -> Vec<HeapInstanceRow> {
        let Some(snapshot) = &self.snapshot else {
            return Vec::new();
        };
        self.instances
            .iter()
            .map(|object| self.instance_row(snapshot, *object))
            .collect()
    }

    fn instance_row(&self, snapshot: &HeapSnapshot, object: u32) -> HeapInstanceRow {
        let instance = &snapshot.objects[object as usize];
        HeapInstanceRow {
            object,
            class: snapshot
                .class_of(object)
                .map_or_else(String::new, |class| class.name.clone()),
            address: instance.address,
            shallow: instance.shallow,
            retained: instance.retained,
            selected: self.object == Some(object),
            expanded: self.expanded.contains(&object),
        }
    }

    /// The fields of the chosen instance, or the elements of an array.
    pub fn field_rows(&self) -> Vec<HeapFieldRow> {
        let (Some(snapshot), Some(object)) = (&self.snapshot, self.object) else {
            return Vec::new();
        };
        let Some(instance) = snapshot.objects.get(object as usize) else {
            return Vec::new();
        };
        instance
            .fields
            .iter()
            .map(|field| HeapFieldRow {
                name: field.name.to_string(),
                value: snapshot.value_text(&field.value),
                object: match field.value {
                    lang_api::HeapValue::Object(target) => Some(target),
                    _ => None,
                },
            })
            .collect()
    }

    /// The shortest path from a root to the chosen instance, the root first.
    pub fn path_rows(&self) -> Vec<HeapPathRow> {
        let (Some(snapshot), Some(object)) = (&self.snapshot, self.object) else {
            return Vec::new();
        };
        snapshot
            .path(object)
            .into_iter()
            .map(|step| {
                let instance = &snapshot.objects[step as usize];
                let class = snapshot
                    .class_of(step)
                    .map_or_else(String::new, |class| class.name.clone());
                HeapPathRow {
                    object: step,
                    class,
                    address: instance.address,
                    root: instance.root,
                    target: step == object,
                }
            })
            .collect()
    }

    /// A double click on a class of the project: its source.
    pub fn open_class(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(snapshot) = &self.snapshot else {
            return;
        };
        let Some(class) = snapshot.classes.get(index) else {
            return;
        };
        let Some(source) = class.source.clone() else {
            return;
        };
        let workspace = self.workspace.clone();
        workspace
            .update(cx, |workspace, cx| {
                navigation::go_to(
                    workspace,
                    source,
                    Some(Spot::Line { line: 0, column: 0 }),
                    true,
                    window,
                    cx,
                )
            })
            .ok();
    }

    fn note(&self) -> Option<(String, bool)> {
        match &self.state {
            State::Loading => {
                let percent = match self.progress.total {
                    0 => String::new(),
                    total => format!(
                        " {}%",
                        (self.progress.bytes.saturating_mul(100) / total).min(100)
                    ),
                };
                Some((format!("Reading {}…{percent}", self.path.display()), false))
            }
            State::Failed(error) => Some((format!("Cannot read the heap dump: {error}"), true)),
            State::Loaded => None,
        }
    }

    fn section_button(
        &self,
        section: HeapSection,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.section == section;
        div()
            .id(("heap-section", section as usize))
            .debug_selector(move || format!("heap-section-{}", section.id()))
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
            .on_click(cx.listener(move |this, _, _, cx| this.set_section(section, cx)))
            .child(section.label())
    }

    fn header_cell(
        &self,
        column: HeapColumn,
        width: Option<f32>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let selected = self.column == column;
        let arrow = if selected {
            if self.descending { " ↓" } else { " ↑" }
        } else {
            ""
        };
        let label = format!("{}{arrow}", column.label());
        let mut cell = div()
            .id(("heap-column", column as usize))
            .debug_selector(move || format!("heap-sort-{}", column.id()))
            .flex_none()
            .flex()
            .items_center()
            .justify_end()
            .text_color(if selected {
                theme.text
            } else {
                theme.text_secondary
            })
            .cursor_pointer();
        if let Some(width) = width {
            cell = cell.w(px(width));
        }
        cell.on_click(cx.listener(move |this, _, _, cx| this.sort_by(column, cx)))
            .child(label)
    }

    fn classes_body(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "heap-classes");
        let rows = self.class_rows();
        let count = rows.len();
        let theme = *theme;
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .pl(px(SPACE_L))
                    .pr(px(SPACE_M))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().min_w_0().child("Class"))
                    .child(self.header_cell(HeapColumn::Count, Some(COLUMN_WIDTH), &theme, cx))
                    .child(self.header_cell(HeapColumn::Shallow, Some(COLUMN_WIDTH), &theme, cx))
                    .child(self.header_cell(HeapColumn::Retained, Some(COLUMN_WIDTH), &theme, cx)),
            )
            .child(
                scrollable(
                    "heap-classes",
                    &self.scroll_classes,
                    div().size_full().child(
                        uniform_list(
                            "heap-classes",
                            count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                this.class_row_elements(&rows, range, &theme, cx)
                            }),
                        )
                        .track_scroll(&self.scroll_classes)
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

    fn class_row_elements(
        &self,
        rows: &[HeapClassRow],
        range: std::ops::Range<usize>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Stateful<Div>> {
        range
            .filter_map(|index| rows.get(index).cloned().map(|row| (index, row)))
            .map(|(index, row)| {
                let own = row.source.is_some();
                let class = row.index;
                let retained = bytes_text(row.retained);
                let shallow = bytes_text(row.shallow);
                div()
                    .id(("heap-class", index))
                    .debug_selector(move || format!("heap-class-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .w_full()
                    .h(px(ROW_HEIGHT))
                    .pl(px(SPACE_L))
                    .pr(px(SPACE_M))
                    .rounded(px(ROW_RADIUS))
                    .when(row.selected, |this| this.bg(theme.selection))
                    .hover(move |style| style.bg(theme.selection_hover))
                    .text_color(if own {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        window.focus(&this.focus, cx);
                        this.choose_class(Some(class), cx);
                        if event.click_count() >= 2 {
                            this.open_class(class, window, cx);
                        }
                    }))
                    .child(div().flex_1().min_w_0().truncate().child(row.name.clone()))
                    .child(
                        div()
                            .w(px(COLUMN_WIDTH))
                            .flex_none()
                            .text_align(TextAlign::Right)
                            .child(row.count.to_string()),
                    )
                    .child(
                        div()
                            .w(px(COLUMN_WIDTH))
                            .flex_none()
                            .text_align(TextAlign::Right)
                            .child(shallow),
                    )
                    .child(
                        div()
                            .w(px(COLUMN_WIDTH))
                            .flex_none()
                            .text_align(TextAlign::Right)
                            .child(retained),
                    )
            })
            .collect()
    }

    fn objects_body(
        &self,
        rows: Arc<Vec<(u32, usize)>>,
        id: &'static str,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let count = rows.len();
        let theme = *theme;
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .pl(px(SPACE_L))
                    .pr(px(SPACE_M))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().min_w_0().child("Object"))
                    .child(div().w(px(COLUMN_WIDTH)).flex_none().child("Shallow"))
                    .child(div().w(px(COLUMN_WIDTH)).flex_none().child("Retained")),
            )
            .child(
                scrollable(
                    id,
                    &self.scroll_tree,
                    div().size_full().child(
                        uniform_list(
                            id,
                            count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                let Some(snapshot) = &this.snapshot else {
                                    return Vec::new();
                                };
                                range
                                    .filter_map(|index| {
                                        rows.get(index).map(|(object, depth)| {
                                            (index, this.object_row(snapshot, *object, *depth))
                                        })
                                    })
                                    .map(|(index, row)| {
                                        this.object_row_element(index, row, &theme, cx)
                                    })
                                    .collect()
                            }),
                        )
                        .track_scroll(&self.scroll_tree)
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

    fn object_row_element(
        &self,
        index: usize,
        row: HeapObjectRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let object = row.object;
        let retained = bytes_text(row.retained);
        let shallow = bytes_text(row.shallow);
        let label = format!("{} @{}", row.class, row.address);
        div()
            .id(("heap-object", index))
            .debug_selector(move || format!("heap-object-{index}"))
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_L + row.depth as f32 * SPACE_L))
            .pr(px(SPACE_M))
            .rounded(px(ROW_RADIUS))
            .when(row.selected, |this| this.bg(theme.selection))
            .hover(move |style| style.bg(theme.selection_hover))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.choose_object(Some(object), cx)))
            .child(div().flex_1().min_w_0().truncate().child(label))
            .child(
                div()
                    .w(px(COLUMN_WIDTH))
                    .flex_none()
                    .text_align(TextAlign::Right)
                    .child(shallow),
            )
            .child(
                div()
                    .w(px(COLUMN_WIDTH))
                    .flex_none()
                    .text_align(TextAlign::Right)
                    .child(retained),
            )
    }

    fn strings_body(
        &self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let rows = self.string_rows();
        let count = rows.len();
        let theme = *theme;
        div()
            .flex()
            .flex_col()
            .size_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .pl(px(SPACE_L))
                    .pr(px(SPACE_M))
                    .border_b_1()
                    .border_color(theme.border)
                    .child(div().flex_1().min_w_0().child("String"))
                    .child(div().w(px(COLUMN_WIDTH)).flex_none().child("Count"))
                    .child(div().w(px(COLUMN_WIDTH)).flex_none().child("Wasted")),
            )
            .child(
                scrollable(
                    "heap-strings",
                    &self.scroll_objects,
                    div().size_full().child(
                        uniform_list(
                            "heap-strings",
                            count,
                            cx.processor(move |_this, range: std::ops::Range<usize>, _, _cx| {
                                range
                                    .filter_map(|index| {
                                        rows.get(index).cloned().map(|row| (index, row))
                                    })
                                    .map(|(index, row)| {
                                        let wasted = bytes_text(row.wasted);
                                        div()
                                            .id(("heap-string", index))
                                            .debug_selector(move || format!("heap-string-{index}"))
                                            .flex()
                                            .items_center()
                                            .flex_none()
                                            .w_full()
                                            .h(px(ROW_HEIGHT))
                                            .pl(px(SPACE_L))
                                            .pr(px(SPACE_M))
                                            .rounded(px(ROW_RADIUS))
                                            .hover(move |style| style.bg(theme.selection_hover))
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .truncate()
                                                    .child(format!("\"{}\"", row.text)),
                                            )
                                            .child(
                                                div()
                                                    .w(px(COLUMN_WIDTH))
                                                    .flex_none()
                                                    .child(row.count.to_string()),
                                            )
                                            .child(
                                                div().w(px(COLUMN_WIDTH)).flex_none().child(wasted),
                                            )
                                    })
                                    .collect()
                            }),
                        )
                        .track_scroll(&self.scroll_objects)
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

    fn lower(&self, theme: &Theme, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *theme;
        let instances = self.instances.clone();
        let count = instances.len();
        let fields = self.field_rows();
        let path = self.path_rows();
        let list = div()
            .flex()
            .flex_col()
            .w(relative(0.55))
            .h_full()
            .border_r_1()
            .border_color(theme.border)
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .px(px(SPACE_M))
                    .text_color(theme.text_secondary)
                    .child(
                        match self.class.and_then(|class| {
                            self.snapshot
                                .as_ref()
                                .and_then(|snapshot| snapshot.classes.get(class))
                                .map(|class| class.name.clone())
                        }) {
                            Some(name) => format!("Instances of {name}"),
                            None => "Instances".to_owned(),
                        },
                    ),
            )
            .child(
                scrollable(
                    "heap-instances",
                    &self.scroll_instances,
                    div().size_full().child(
                        uniform_list(
                            "heap-instances",
                            count,
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                let Some(snapshot) = &this.snapshot else {
                                    return Vec::new();
                                };
                                range
                                    .filter_map(|index| {
                                        instances.get(index).map(|object| {
                                            (index, this.instance_row(snapshot, *object))
                                        })
                                    })
                                    .map(|(index, row)| {
                                        this.instance_row_element(index, row, &theme, cx)
                                    })
                                    .collect()
                            }),
                        )
                        .track_scroll(&self.scroll_instances)
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            );
        let details = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_w_0()
            .h_full()
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(HEADER_HEIGHT))
                    .px(px(SPACE_M))
                    .text_color(theme.text_secondary)
                    .child(match self.object {
                        Some(_) => "Fields and the path to a root",
                        None => "Select an instance",
                    }),
            )
            .child(
                scrollable(
                    "heap-details",
                    &self.scroll_details,
                    div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .px(px(SPACE_M))
                        .children(fields.into_iter().enumerate().map(|(index, field)| {
                            let object = field.object;
                            div()
                                .id(("heap-field", index))
                                .debug_selector(move || format!("heap-field-{index}"))
                                .flex()
                                .items_center()
                                .flex_none()
                                .h(px(ROW_HEIGHT))
                                .gap(px(SPACE_S))
                                .rounded(px(ROW_RADIUS))
                                .when(object.is_some(), |this| {
                                    this.cursor_pointer()
                                        .hover(move |style| style.bg(theme.selection_hover))
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.choose_object(object, cx)
                                        }))
                                })
                                .child(
                                    div()
                                        .w(px(140.))
                                        .flex_none()
                                        .truncate()
                                        .text_color(theme.text_secondary)
                                        .child(field.name),
                                )
                                .child(div().flex_1().min_w_0().truncate().child(field.value))
                        }))
                        .when(!path.is_empty(), |this| {
                            this.child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .flex_none()
                                    .pt(px(SPACE_S))
                                    .gap(px(SPACE_XS))
                                    .child(
                                        div()
                                            .text_color(theme.text_secondary)
                                            .child("Path to a root"),
                                    )
                                    .children(path.into_iter().enumerate().map(|(index, step)| {
                                        let label = format!("{} @{}", step.class, step.address);
                                        div()
                                            .id(("heap-path", index))
                                            .debug_selector(move || format!("heap-path-{index}"))
                                            .flex()
                                            .items_center()
                                            .flex_none()
                                            .h(px(ROW_HEIGHT))
                                            .gap(px(SPACE_S))
                                            .rounded(px(ROW_RADIUS))
                                            .pl(px(index as f32 * SPACE_M))
                                            .when(step.target, |this| this.bg(theme.selection))
                                            .child(if step.root { "◆" } else { "→" })
                                            .child(div().flex_1().min_w_0().truncate().child(label))
                                    })),
                            )
                        }),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            );
        div()
            .flex()
            .flex_row()
            .flex_none()
            .h(px(LOWER_HEIGHT))
            .border_t_1()
            .border_color(theme.border)
            .child(list)
            .child(details)
            .into_any_element()
    }

    fn instance_row_element(
        &self,
        index: usize,
        row: HeapInstanceRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let object = row.object;
        let retained = bytes_text(row.retained);
        div()
            .id(("heap-instance", index))
            .debug_selector(move || format!("heap-instance-{index}"))
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(ROW_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_M))
            .rounded(px(ROW_RADIUS))
            .when(row.selected, |this| this.bg(theme.selection))
            .hover(move |style| style.bg(theme.selection_hover))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| this.choose_object(Some(object), cx)))
            .child(
                div()
                    .w(px(SPACE_L))
                    .flex_none()
                    .child(if row.expanded { "▾" } else { "▸" }),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .child(format!("{} @{}", row.class, row.address)),
            )
            .child(
                div()
                    .w(px(COLUMN_WIDTH))
                    .flex_none()
                    .text_align(TextAlign::Right)
                    .child(retained),
            )
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.filter_focus.is_focused(window) && self.filter.key_with_clipboard(event, false, cx)
        {
            cx.stop_propagation();
            cx.notify();
        }
    }
}

impl Render for HeapDumpView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "heap-dump");
        let theme = *cx.global::<Theme>();
        let note = self.note();
        let filter = div()
            .id("heap-filter")
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .w(px(280.))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.border)
            .track_focus(&self.filter_focus)
            .on_click(cx.listener(|this, _, window, cx| window.focus(&this.filter_focus, cx)))
            .child(icon("general/search", &theme))
            .child(
                div()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .children(text_field_children(
                        FieldMouse::new(cx, |this| Some(&mut this.filter)),
                        &self.filter,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        16.,
                        self.filter_focus.is_focused(window),
                    )),
            );
        let header = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .h(px(HEADER_HEIGHT + SPACE_S))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .text_size(px(UI_SIZE))
            .font_weight(FontWeight::MEDIUM)
            .child(
                div().flex_1().min_w_0().truncate().child(match &note {
                    Some((text, true)) => text.clone(),
                    _ => self
                        .path
                        .file_name()
                        .map_or_else(String::new, |name| name.to_string_lossy().into_owned()),
                }),
            )
            .when(note.as_ref().is_some_and(|(_, error)| !error), |this| {
                this.child(
                    div()
                        .flex_none()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_secondary)
                        .child(note.clone().map_or_else(String::new, |(text, _)| text)),
                )
            })
            .child(filter);
        let sections = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_XS))
            .px(px(SPACE_S))
            .pb(px(SPACE_XS))
            .children(
                [
                    HeapSection::Classes,
                    HeapSection::Dominators,
                    HeapSection::Biggest,
                    HeapSection::Strings,
                ]
                .into_iter()
                .map(|section| self.section_button(section, &theme, cx)),
            );
        let body = match self.section {
            HeapSection::Classes => self.classes_body(&theme, window, cx),
            HeapSection::Dominators => self.objects_body(
                self.dominators.clone(),
                "heap-dominators",
                &theme,
                window,
                cx,
            ),
            HeapSection::Biggest => {
                self.objects_body(self.biggest.clone(), "heap-biggest", &theme, window, cx)
            }
            HeapSection::Strings => self.strings_body(&theme, window, cx),
        };
        div()
            .id("heap-dump")
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.editor_bg)
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::key))
            .child(header)
            .child(sections)
            .child(div().flex_1().min_h_0().child(body))
            .child(self.lower(&theme, window, cx))
    }
}

/// The rows the tab keeps from a snapshot: built once, in the background with the reading
/// of a large dump, because the dominator tree and the sort of the biggest objects cost as
/// much as the reading itself.
struct Prepared {
    snapshot: Arc<HeapSnapshot>,
    dominators: Arc<Vec<(u32, usize)>>,
    biggest: Arc<Vec<(u32, usize)>>,
    string_objects: Vec<Vec<u32>>,
}

impl Prepared {
    fn new(snapshot: HeapSnapshot) -> Self {
        let snapshot = Arc::new(snapshot);
        let dominators = Arc::new(dominator_rows(&snapshot));
        let mut biggest: Vec<(u32, usize)> = (0..snapshot.objects.len() as u32)
            .map(|object| (object, 0))
            .collect();
        biggest.sort_by_key(|(object, _)| {
            std::cmp::Reverse(snapshot.objects[*object as usize].retained)
        });
        let string_objects = string_objects(&snapshot);
        Self {
            snapshot,
            dominators,
            biggest: Arc::new(biggest),
            string_objects,
        }
    }
}

/// The dominator tree as a flat list of objects and their depth, the largest first.
fn dominator_rows(snapshot: &HeapSnapshot) -> Vec<(u32, usize)> {
    let mut children: HashMap<u32, Vec<u32>> = HashMap::new();
    let mut top: Vec<u32> = Vec::new();
    for (index, object) in snapshot.objects.iter().enumerate() {
        match object.dominator {
            Some(parent) => children.entry(parent).or_default().push(index as u32),
            None => top.push(index as u32),
        }
    }
    for list in children.values_mut() {
        list.sort_by_key(|object| std::cmp::Reverse(snapshot.objects[*object as usize].retained));
    }
    top.sort_by_key(|object| std::cmp::Reverse(snapshot.objects[*object as usize].retained));
    let mut rows = Vec::with_capacity(snapshot.objects.len());
    let mut stack: Vec<(u32, usize)> = top
        .into_iter()
        .rev()
        .map(|object| (object, 0usize))
        .collect();
    while let Some((object, depth)) = stack.pop() {
        rows.push((object, depth));
        if let Some(list) = children.get(&object) {
            for child in list.iter().rev() {
                stack.push((*child, depth + 1));
            }
        }
    }
    rows
}

/// The objects of every group of equal strings.
fn string_objects(snapshot: &HeapSnapshot) -> Vec<Vec<u32>> {
    let mut groups: HashMap<String, Vec<u32>> = HashMap::new();
    for (index, object) in snapshot.objects.iter().enumerate() {
        if let Some(text) = &object.text {
            groups
                .entry(text.to_string())
                .or_default()
                .push(index as u32);
        }
    }
    snapshot
        .strings
        .iter()
        .map(|string| groups.remove(string.text.as_ref()).unwrap_or_default())
        .collect()
}

impl Drop for HeapDumpView {
    fn drop(&mut self) {
        self.cancel.cancel();
    }
}

/// Opens a heap dump from File > Open, when a provider knows the file. False leaves the
/// file to the text editor.
pub fn open_file(
    workspace: &mut Workspace,
    path: PathBuf,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) -> bool {
    let Some(runs) = cx
        .try_global::<ActiveRunPanel>()
        .map(|panel| panel.0.clone())
    else {
        return false;
    };
    let Some((provider, model)) = runs.read(cx).heap_reader(&path) else {
        return false;
    };
    HeapDumpView::open(workspace, path, provider, model, window, cx);
    true
}

/// Runs `GC.heap_dump` of the program of the last run into `.arro/snapshots` and opens
/// the tab when the file is ready. Nothing blocks the window.
pub fn capture(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some(runs) = cx
        .try_global::<ActiveRunPanel>()
        .map(|panel| panel.0.clone())
    else {
        return;
    };
    let Some(request) = runs.read(cx).heap_request() else {
        workspace.notify_user(
            "Memory Snapshot",
            "Run a program first: a heap dump needs a running JVM.",
            cx,
        );
        return;
    };
    capture_request(workspace, request, window, cx);
}

/// The same capture for the program of a debug session, whose process id the adapter
/// reported to the Debug window.
pub fn capture_pid(
    workspace: &mut Workspace,
    pid: Option<u32>,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(runs) = cx
        .try_global::<ActiveRunPanel>()
        .map(|panel| panel.0.clone())
    else {
        return;
    };
    let Some(request) = pid.and_then(|pid| runs.read(cx).heap_request_for(pid)) else {
        workspace.notify_user(
            "Memory Snapshot",
            "The debugger did not report the process of the program.",
            cx,
        );
        return;
    };
    capture_request(workspace, request, window, cx);
}

fn capture_request(
    workspace: &mut Workspace,
    mut request: HeapRequest,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if let Some(folder) = cx
        .try_global::<workspace::Settings>()
        .map(|settings| settings.heap_snapshot_folder.trim().to_owned())
        .filter(|folder| !folder.is_empty())
    {
        let folder = PathBuf::from(folder);
        let folder = if folder.is_absolute() {
            folder
        } else {
            workspace.root().join(folder)
        };
        if let Some(name) = request.file.file_name() {
            request.file = folder.join(name);
        }
    }
    let capture_file = request.file.clone();
    let provider = request.provider.clone();
    let model = request.model.clone();
    let cancel = Cancel::new();
    let capture_pid = request.pid;
    let task = cx.background_executor().spawn(async move {
        if let Some(folder) = capture_file.parent()
            && let Err(error) = std::fs::create_dir_all(folder)
        {
            return Err(format!("cannot create {}: {error}", folder.display()).into());
        }
        provider.capture_heap_dump(capture_pid, &capture_file, &model, &cancel)
    });
    cx.spawn_in(window, async move |workspace, cx| {
        let result = task.await;
        workspace
            .update_in(cx, |workspace, window, cx| match result {
                Ok(()) => {
                    let path = request.file.clone();
                    HeapDumpView::open(
                        workspace,
                        path,
                        request.provider.clone(),
                        request.model.clone(),
                        window,
                        cx,
                    );
                }
                Err(error) => workspace.notify_user(
                    "Memory Snapshot",
                    format!("Cannot capture the heap: {error}"),
                    cx,
                ),
            })
            .ok();
    })
    .detach();
}

/// What the Run panel gives the capture: the program, the language and the file.
pub struct HeapRequest {
    pub pid: u32,
    pub provider: Arc<dyn runner::RunConfigProvider>,
    pub model: project_model::ProjectModel,
    pub file: PathBuf,
}

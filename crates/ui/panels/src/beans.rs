//! The Beans tool window and the badges of the gutter that open the beans and their
//! points of an injection. The model comes from the language in the background: the
//! window only shows what the last answer holds, and the badges of every open source
//! follow that answer.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use editor_view::{EditorView, PlaceMark, PlaceTarget, Semantic, Spot};
use futures_util::StreamExt;
use lang_api::{
    Bean, BeanModel, BeanQuery, DiagnosticEntry, LanguageSupport, Severity, TextPoint, TextSpan,
    ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::navigation;

const GROUP: &str = "bottom-panel";
const FILTER_CONTEXT: &str = "BeansPanel";
/// The hint of a badge that opens the points a bean is injected into.
const BEAN_HINT: &str = "Navigate to usages of the bean";
/// The hint of a badge that opens the beans a point receives.
const POINT_HINT: &str = "Navigate to autowired candidates";

/// The hub keeps the last model of the beans and gives it to the window and the badges
/// of every open editor.
pub struct BeanHub {
    model: Arc<BeanModel>,
    listening: bool,
}

impl BeanHub {
    pub fn create(cx: &mut App) -> Entity<Self> {
        cx.new(|_| Self {
            model: Arc::new(BeanModel::default()),
            listening: false,
        })
    }

    pub fn model(&self) -> Arc<BeanModel> {
        self.model.clone()
    }

    /// Starts reading the model of the language once, and keeps it while the window is
    /// open. A language that has no model of beans starts nothing.
    pub fn listen(&mut self, support: Arc<dyn LanguageSupport>, cx: &mut Context<Self>) {
        if self.listening || support.beans().is_none() {
            return;
        }
        self.listening = true;
        cx.spawn(async move |hub, cx| {
            let Some(provider) = support.beans() else {
                return;
            };
            let mut replies = ask(provider, BeanQuery);
            while let Some(answer) = replies.next().await {
                let model = Arc::new(answer.value);
                let applied = hub.update(cx, |hub, cx| {
                    hub.model = model.clone();
                    cx.emit(BeansChanged(model.clone()));
                    apply_to_open_editors(&model, cx);
                });
                if applied.is_err() {
                    return;
                }
            }
        })
        .detach();
    }
}

/// Told when the model of the beans is new, so the window rebuilds its rows.
pub struct BeansChanged(pub Arc<BeanModel>);

impl EventEmitter<BeansChanged> for BeanHub {}

/// The hub of the window, for the gutter and the tests.
pub struct ActiveBeanHub(pub Entity<BeanHub>);

impl Global for ActiveBeanHub {}

/// The panel of the window, for the command and the tests.
pub struct ActiveBeans(pub Entity<BeansPanel>);

impl Global for ActiveBeans {}

/// One row of the tree: a module, a stereotype or a bean.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BeanRow {
    pub kind: BeanRowKind,
    pub label: String,
    pub hint: String,
    pub depth: usize,
    /// The place of the bean in [`BeansPanel::beans`] for a bean row.
    pub bean: Option<usize>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BeanRowKind {
    Module,
    Stereotype,
    Bean,
}

pub struct BeansPanel {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    hub: Entity<BeanHub>,
    support: Option<Arc<dyn LanguageSupport>>,
    model: Arc<BeanModel>,
    query: TextField,
    selected: Option<usize>,
    rows: Vec<BeanRow>,
    beans: Vec<Bean>,
    scroll: UniformListScrollHandle,
    _model: Subscription,
}

impl BeansPanel {
    pub fn create(
        hub: Entity<BeanHub>,
        workspace: WeakEntity<Workspace>,
        cx: &mut App,
    ) -> Entity<Self> {
        cx.new(|cx| {
            let model = hub.read(cx).model();
            let _model = cx.subscribe(&hub, |panel: &mut Self, _hub, event: &BeansChanged, cx| {
                panel.model = event.0.clone();
                panel.rebuild(cx);
                cx.notify();
            });
            Self {
                workspace,
                focus: cx.focus_handle(),
                hub,
                support: None,
                model,
                query: TextField::default(),
                selected: None,
                rows: Vec::new(),
                beans: Vec::new(),
                scroll: UniformListScrollHandle::new(),
                _model,
            }
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The rows of the tree, group rows included.
    pub fn rows(&self) -> &[BeanRow] {
        &self.rows
    }

    /// The beans the current filter keeps.
    pub fn beans(&self) -> &[Bean] {
        &self.beans
    }

    /// The language is up: the model of the beans can be read.
    pub fn language_ready(&mut self, support: Arc<dyn LanguageSupport>, cx: &mut Context<Self>) {
        self.support = Some(support.clone());
        self.hub.update(cx, |hub, cx| hub.listen(support, cx));
        self.rebuild(cx);
    }

    /// A new filter text, as typing changes it.
    pub fn set_query(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        self.query = TextField::from_text(text);
        self.rebuild(cx);
        cx.notify();
    }

    fn matches(&self, bean: &Bean, module: &str) -> bool {
        let needle = self.query.text.trim().to_lowercase();
        if needle.is_empty() {
            return true;
        }
        format!(
            "{} {} {} {module}",
            bean.name, bean.type_name, bean.stereotype
        )
        .to_lowercase()
        .contains(&needle)
    }

    /// Builds the tree from the model and the filter.
    fn rebuild(&mut self, _cx: &mut Context<Self>) {
        let model = &self.model;
        let support = self.support.clone();
        let module_of = |path: &Path| {
            support
                .as_ref()
                .and_then(|support| support.module_of(path))
                .unwrap_or_default()
        };
        let mut modules: BTreeMap<String, BTreeMap<String, Vec<Bean>>> = BTreeMap::new();
        for bean in &model.beans {
            let module = module_of(&bean.file);
            if !self.matches(bean, &module) {
                continue;
            }
            modules
                .entry(module)
                .or_default()
                .entry(bean.stereotype.clone())
                .or_default()
                .push(bean.clone());
        }
        self.beans.clear();
        self.rows.clear();
        for (module, stereotypes) in modules {
            self.rows.push(BeanRow {
                kind: BeanRowKind::Module,
                label: module,
                hint: String::new(),
                depth: 0,
                bean: None,
            });
            for (stereotype, beans) in stereotypes {
                self.rows.push(BeanRow {
                    kind: BeanRowKind::Stereotype,
                    label: stereotype,
                    hint: String::new(),
                    depth: 1,
                    bean: None,
                });
                for bean in beans {
                    self.beans.push(bean);
                    self.rows.push(BeanRow {
                        kind: BeanRowKind::Bean,
                        label: self.beans.last().map_or("", |bean| &bean.name).to_owned(),
                        hint: self
                            .beans
                            .last()
                            .map_or_else(String::new, |bean| bean.type_name.clone()),
                        depth: 2,
                        bean: Some(self.beans.len() - 1),
                    });
                }
            }
        }
        self.selected = self
            .selected
            .filter(|index| self.rows.get(*index).is_some_and(|row| row.bean.is_some()))
            .or_else(|| self.rows.iter().position(|row| row.bean.is_some()));
    }

    fn bean_at(&self, row: usize) -> Option<&Bean> {
        let index = self.rows.get(row)?.bean?;
        self.beans.get(index)
    }

    /// A click or Enter on a bean row opens its declaration.
    pub fn open_row(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(bean) = self.bean_at(row) else {
            return;
        };
        let path = bean.file.clone();
        let offset = bean.offset;
        let workspace = self.workspace.clone();
        window.defer(cx, move |window, cx| {
            workspace
                .update(cx, |workspace, cx| {
                    navigation::go_to(
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

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let selectable: Vec<usize> = self
            .rows
            .iter()
            .enumerate()
            .filter_map(|(index, row)| row.bean.map(|_| index))
            .collect();
        if selectable.is_empty() {
            return;
        }
        let next = match self.selected {
            Some(current) => {
                let at = selectable
                    .iter()
                    .position(|index| *index == current)
                    .unwrap_or(0);
                let at = (at as isize + delta).clamp(0, selectable.len() as isize - 1);
                selectable[at as usize]
            }
            None if delta < 0 => *selectable.last().unwrap_or(&selectable[0]),
            None => selectable[0],
        };
        self.selected = Some(next);
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "up" => {
                self.move_selection(-1, cx);
                cx.stop_propagation();
            }
            "down" => {
                self.move_selection(1, cx);
                cx.stop_propagation();
            }
            "enter" => {
                if let Some(row) = self.selected {
                    self.open_row(row, window, cx);
                }
                cx.stop_propagation();
            }
            _ => {
                if self.query.key_with_clipboard(event, false, cx) {
                    self.rebuild(cx);
                    cx.stop_propagation();
                    cx.notify();
                }
            }
        }
    }

    fn row_element(
        &self,
        index: usize,
        row: &BeanRow,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let label = row.label.clone();
        let hint = row.hint.clone();
        let kind = row.kind;
        let depth = row.depth;
        let selected = self.selected == Some(index) && kind == BeanRowKind::Bean;
        let icon_name = match kind {
            BeanRowKind::Module => "nodes/module",
            BeanRowKind::Stereotype => "toolwindows/services",
            BeanRowKind::Bean => "nodes/class",
        };
        let text_color = match kind {
            BeanRowKind::Bean => theme.text,
            _ => theme.text_secondary,
        };
        div()
            .id(("beans-row", index))
            .debug_selector(move || format!("beans-row-{index}"))
            .flex()
            .items_center()
            .flex_none()
            .w_full()
            .h(px(ROW_HEIGHT))
            .gap(px(SPACE_XS))
            .pl(px(SPACE_M + depth as f32 * SPACE_L))
            .pr(px(SPACE_M))
            .rounded(px(ROW_RADIUS))
            .when(selected, |this| this.bg(theme.selection))
            .hover(move |style| style.bg(theme.selection_hover))
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.focus, cx);
                this.selected = Some(index);
                this.open_row(index, window, cx);
                cx.notify();
            }))
            .child(icon(icon_name, theme))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(text_color)
                    .child(label),
            )
            .when(!hint.is_empty(), |this| {
                this.child(
                    div()
                        .flex_none()
                        .truncate()
                        .text_color(theme.text_secondary)
                        .text_size(px(TEXT_SMALL))
                        .child(hint),
                )
            })
    }
}

impl Render for BeansPanel {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        #[cfg(test)]
        workspace::note_view_render(cx, "beans");
        let theme = *cx.global::<Theme>();
        let count = self.beans().len();
        let header = div()
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .pl(px(SPACE_M))
            .pr(px(SPACE_S))
            .text_size(px(UI_SIZE))
            .font_weight(FontWeight::MEDIUM)
            .child("Beans")
            .child(
                div()
                    .text_color(theme.text_secondary)
                    .text_size(px(TEXT_SMALL))
                    .child(format!("{count}")),
            );
        let search = div()
            .flex()
            .items_center()
            .flex_none()
            .gap(px(SPACE_S))
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .child(icon("general/search", &theme))
            .child(
                div()
                    .id("beans-filter")
                    .debug_selector(|| "beans-filter".to_owned())
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_center()
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .on_click(cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
                    .children(text_field_children_named(
                        "beans-filter",
                        &self.query,
                        theme.text,
                        theme.text,
                        theme.selection,
                        theme.caret,
                        16.,
                        true,
                    )),
            )
            .when(!self.query.text.is_empty(), |this| {
                this.child(
                    icon_button("beans-clear", "general/clear", "Clear Filter", None, &theme)
                        .debug_selector(|| "beans-clear".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| this.set_query("", cx))),
                )
            });
        let empty = self.rows.is_empty();
        let hint = empty.then(|| {
            div()
                .px(px(SPACE_L))
                .py(px(SPACE_S))
                .text_color(theme.text_secondary)
                .child("No beans found")
        });
        island(&theme)
            .id("beans-panel")
            .key_context(FILTER_CONTEXT)
            .track_focus(&self.focus)
            .group(GROUP)
            .h_full()
            .child(header)
            .child(search)
            .children(hint)
            .child(
                scrollable(
                    "beans-rows",
                    &self.scroll,
                    div().size_full().child(
                        uniform_list(
                            "beans-rows",
                            self.rows().len(),
                            cx.processor(move |this, range: std::ops::Range<usize>, _, cx| {
                                range
                                    .filter_map(|index| {
                                        this.rows.get(index).map(|row| (index, row.clone()))
                                    })
                                    .map(|(index, row)| this.row_element(index, &row, &theme, cx))
                                    .collect::<Vec<_>>()
                            }),
                        )
                        .track_scroll(&self.scroll)
                        .size_full(),
                    ),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            )
            .capture_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
    }
}

/// Applies the last model of the beans to an editor: the badges of its lines and the
/// checks of its points.
pub(crate) fn refresh(view: &mut EditorView, cx: &mut Context<EditorView>) {
    let Some(support) = cx
        .try_global::<Semantic>()
        .map(|semantic| semantic.0.clone())
    else {
        return;
    };
    if support.beans().is_none() {
        return;
    }
    let Some(hub) = cx.try_global::<ActiveBeanHub>().map(|hub| hub.0.clone()) else {
        return;
    };
    hub.update(cx, |hub, cx| hub.listen(support, cx));
    let model = hub.read(cx).model();
    apply(view, &model, cx);
}

fn apply_to_open_editors(model: &BeanModel, cx: &mut App) {
    for view in crate::outline::OpenEditors::all(cx) {
        view.update(cx, |view, cx| apply(view, model, cx));
    }
}

fn apply(view: &mut EditorView, model: &BeanModel, cx: &mut Context<EditorView>) {
    let path = view.path().to_path_buf();
    let Some(text) = view.text() else {
        return;
    };
    let theme = *cx.global::<Theme>();
    let color = theme.accent;
    let mut marks = Vec::new();
    for bean in model.beans_of(&path) {
        marks.push(PlaceMark {
            line: bean.line,
            icon: ui_kit::icon_file(theme.icon_set(), "nodes/class", theme.dark, 13.),
            color,
            hint: BEAN_HINT.into(),
            list: true,
            places: bean.usages.iter().map(target).collect(),
        });
    }
    for point in model.injections_of(&path) {
        if point.candidates.is_empty() {
            continue;
        }
        marks.push(PlaceMark {
            line: point.line,
            icon: ui_kit::icon_file(theme.icon_set(), "general/locate", theme.dark, 13.),
            color,
            hint: POINT_HINT.into(),
            list: false,
            places: point.candidates.iter().map(target).collect(),
        });
    }
    marks.sort_by_key(|mark| mark.line);
    let mut merged: Vec<PlaceMark> = Vec::with_capacity(marks.len());
    for mark in marks {
        if let Some(last) = merged.last_mut()
            && last.line == mark.line
        {
            last.list |= mark.list;
            last.places.extend(mark.places);
            continue;
        }
        merged.push(mark);
    }
    view.set_place_marks(merged, cx);
    let warnings: Vec<DiagnosticEntry> = model
        .warnings_of(&path)
        .map(|warning| DiagnosticEntry {
            span: span_in(&text, &warning.range),
            severity: Severity::Warning,
            message: warning.message.clone(),
            unnecessary: false,
            code: Some(warning.id.clone()),
        })
        .collect();
    view.set_native_marks(warnings, cx);
}

fn target(place: &lang_api::InjectionPlace) -> PlaceTarget {
    PlaceTarget {
        label: place.label.clone(),
        file: place.file.clone(),
        offset: place.offset,
    }
}

/// The text span of a byte range of a source.
fn span_in(text: &str, range: &std::ops::Range<usize>) -> TextSpan {
    TextSpan {
        start: point_in(text, range.start),
        end: point_in(text, range.end),
    }
}

fn point_in(text: &str, offset: usize) -> TextPoint {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    let line = text[..line_start]
        .bytes()
        .filter(|byte| *byte == b'\n')
        .count() as u32;
    TextPoint { line, character }
}

/// A click on a badge: the only place opens at once, several open their list.
pub(crate) fn show_place_menu(
    view: &Entity<EditorView>,
    line: usize,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(mark) = view
        .read(cx)
        .place_marks()
        .iter()
        .find(|mark| mark.line as usize == line)
        .cloned()
    else {
        return;
    };
    if mark.places.is_empty() {
        return;
    }
    if mark.places.len() == 1 && !mark.list {
        let place = mark.places[0].clone();
        navigation::go_to(
            workspace,
            place.file,
            Some(Spot::Offset(place.offset)),
            true,
            window,
            cx,
        );
        return;
    }
    let weak = cx.weak_entity();
    let entries: Vec<MenuEntry> = mark
        .places
        .iter()
        .map(|place| {
            let file = place.file.clone();
            let offset = place.offset;
            let weak = weak.clone();
            MenuEntry::new(place.label.clone(), move |window, cx| {
                weak.update(cx, |workspace, cx| {
                    navigation::go_to(
                        workspace,
                        file.clone(),
                        Some(Spot::Offset(offset)),
                        true,
                        window,
                        cx,
                    )
                })
                .ok();
            })
            .icon("general/locate")
        })
        .collect();
    workspace.show_menu(window.mouse_position(), entries, cx);
}

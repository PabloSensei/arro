//! The Sequence Diagram tab: the participants of a method with their lines of life, the
//! calls and the returns of its body in order, and the frames of the `alt`, `opt` and
//! `loop` fragments around them. The build runs in the language's own thread; the tab
//! draws every answer it gets.

use std::cell::Cell;
use std::collections::HashSet;
use std::rc::Rc;
use std::sync::Arc;

use editor_view::{Semantic, uri_path};
use lang_api::{
    Confidence, DocumentSpot, LanguageSupport, SequenceDiagram, SequenceFragmentKind,
    SequenceMessageKind, SequenceQuery, SequenceSettings, ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::diagram_canvas::{self, MAX_ZOOM, MIN_ZOOM, ZOOM_STEP};
use crate::sequence_export;
use crate::sequence_layout::{self, Geometry, PARTICIPANT_HEIGHT, PARTICIPANT_WIDTH, ROW_HEIGHT};

const TAB_PATH_FOLDER: &str = ".sequence";
const TAB_PATH_NAME: &str = "Sequence.sequence";
/// The space between a frame and the label of its condition.
const FRAME_LABEL_GAP: f32 = 4.;
/// The width of a message label.
const LABEL_WIDTH: f32 = 280.;

/// What an export writes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportKind {
    Svg,
    PlantUml,
    Mermaid,
}

impl ExportKind {
    fn name(self) -> &'static str {
        match self {
            ExportKind::Svg => "SVG",
            ExportKind::PlantUml => "PlantUML",
            ExportKind::Mermaid => "Mermaid",
        }
    }

    fn default_name(self) -> &'static str {
        match self {
            ExportKind::Svg => "sequence.svg",
            ExportKind::PlantUml => "sequence.puml",
            ExportKind::Mermaid => "sequence.mmd",
        }
    }
}

/// One message arrow as the canvas paints it, in window units.
struct Arrow {
    points: Vec<Point<f32>>,
    returning: bool,
    color: Hsla,
}

/// The view of one sequence diagram in a tab of the editor.
pub struct SequenceDiagramView {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    support: Option<Arc<dyn LanguageSupport>>,
    spot: Option<DocumentSpot>,
    settings: SequenceSettings,
    diagram: SequenceDiagram,
    geometry: Option<Geometry>,
    /// The participants the user hid with the right click.
    hidden: HashSet<usize>,
    zoom: f32,
    pan: Point<f32>,
    /// The box of the drawing area of the last frame, in window coordinates.
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    pan_from: Option<Point<Pixels>>,
    /// The query that is in flight; a late answer of an older one is dropped.
    serial: u64,
    export_open: bool,
    note: Option<SharedString>,
}

impl SequenceDiagramView {
    fn create(workspace: WeakEntity<Workspace>, cx: &mut App) -> Entity<Self> {
        let support = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.clone());
        cx.new(|cx| Self {
            workspace,
            focus: cx.focus_handle(),
            support,
            spot: None,
            settings: SequenceSettings::default(),
            diagram: SequenceDiagram::default(),
            geometry: None,
            hidden: HashSet::new(),
            zoom: 1.,
            pan: point(0., 0.),
            bounds: Rc::new(Cell::new(None)),
            pan_from: None,
            serial: 0,
            export_open: false,
            note: None,
        })
    }

    pub fn focus(&self) -> FocusHandle {
        self.focus.clone()
    }

    /// The names of the participants of the model, for tests and scripts.
    #[allow(dead_code)]
    pub fn participant_names(&self) -> Vec<String> {
        self.diagram
            .participants
            .iter()
            .map(|participant| participant.name.clone())
            .collect()
    }

    /// The number of messages of the model, for tests and scripts.
    #[allow(dead_code)]
    pub fn message_count(&self) -> usize {
        self.diagram.messages.len()
    }

    /// The labels of the messages of the model, for tests and scripts.
    #[allow(dead_code)]
    pub fn message_labels(&self) -> Vec<String> {
        self.diagram
            .messages
            .iter()
            .map(|message| message.label.clone())
            .collect()
    }

    /// The settings the last build used, for tests and scripts.
    #[allow(dead_code)]
    pub fn settings(&self) -> SequenceSettings {
        self.settings
    }

    /// The names of the hidden participants, for tests and scripts.
    #[allow(dead_code)]
    pub fn hidden_names(&self) -> Vec<String> {
        self.hidden
            .iter()
            .filter_map(|index| self.diagram.participants.get(*index))
            .map(|participant| participant.name.clone())
            .collect()
    }

    /// Shows the sequence of a method, replacing what the tab had.
    pub(crate) fn start(
        &mut self,
        spot: DocumentSpot,
        settings: SequenceSettings,
        cx: &mut Context<Self>,
    ) {
        self.spot = Some(spot);
        self.settings = settings;
        self.diagram = SequenceDiagram::default();
        self.geometry = None;
        self.hidden.clear();
        self.note = None;
        self.export_open = false;
        self.serial += 1;
        cx.notify();
        self.load(cx);
    }

    /// Builds the diagram again with the settings of the toolbar.
    fn rebuild(&mut self, cx: &mut Context<Self>) {
        self.diagram = SequenceDiagram::default();
        self.geometry = None;
        self.hidden.clear();
        self.note = None;
        self.serial += 1;
        cx.notify();
        self.load(cx);
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let (Some(spot), Some(support)) = (self.spot.clone(), self.support.clone()) else {
            return;
        };
        let Some(provider) = support.sequence_diagram() else {
            self.note = Some("No sequence diagram for this language".into());
            cx.notify();
            return;
        };
        let query = SequenceQuery {
            spot,
            settings: self.settings,
        };
        let serial = self.serial;
        let mut replies = ask(provider, query);
        cx.spawn(async move |this, cx| {
            use futures_util::StreamExt;
            while let Some(answer) = replies.next().await {
                let Ok(current) = this.update(cx, |this, cx| {
                    if this.serial != serial {
                        return false;
                    }
                    if answer.confidence == Confidence::Unknown {
                        this.note = Some("No sequence diagram for this place".into());
                        cx.notify();
                        return false;
                    }
                    this.diagram = answer.value;
                    this.geometry = Some(sequence_layout::compute(&this.diagram));
                    this.note = None;
                    cx.notify();
                    true
                }) else {
                    break;
                };
                if !current {
                    break;
                }
            }
        })
        .detach();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.export_open && event.keystroke.key.to_lowercase() == "escape" {
            self.export_open = false;
            cx.notify();
            return;
        }
        let viewport = window.viewport_size();
        let center = point(viewport.width / 2., viewport.height / 2.);
        match event.keystroke.key.to_lowercase().as_str() {
            "=" | "+" => {
                self.zoom_at(ZOOM_STEP, center);
                cx.notify();
            }
            "-" => {
                self.zoom_at(1. / ZOOM_STEP, center);
                cx.notify();
            }
            _ => {}
        }
    }

    fn zoom_at(&mut self, factor: f32, around: Point<Pixels>) {
        diagram_canvas::zoom_at(
            &mut self.zoom,
            &mut self.pan,
            factor,
            around,
            self.bounds.get(),
        );
    }

    fn fit(&mut self) {
        let Some(geometry) = &self.geometry else {
            return;
        };
        diagram_canvas::fit(
            &mut self.zoom,
            &mut self.pan,
            self.bounds.get(),
            (0., 0., geometry.width, geometry.height),
        );
    }

    fn is_hidden(&self, participant: usize) -> bool {
        self.hidden.contains(&participant)
    }

    fn message_hidden(&self, message: &lang_api::SequenceMessage) -> bool {
        self.is_hidden(message.from) || self.is_hidden(message.to)
    }

    fn open_place(
        &self,
        place: Option<lang_api::SourceSpan>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(place) = place else {
            return;
        };
        let Some(path) = uri_path(&place.uri) else {
            return;
        };
        let spot = editor_view::Spot::Line {
            line: place.span.start.line as usize,
            column: place.span.start.character as usize,
        };
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        cx.defer(move |cx| {
            handle
                .update(cx, |_, window, cx| {
                    workspace
                        .update(cx, |workspace, cx| {
                            crate::navigation::go_to(workspace, path, Some(spot), true, window, cx)
                        })
                        .ok();
                })
                .ok();
        });
    }

    fn export_text(&self, kind: ExportKind) -> String {
        match kind {
            ExportKind::PlantUml => sequence_export::plantuml(&self.diagram),
            ExportKind::Mermaid => sequence_export::mermaid(&self.diagram),
            ExportKind::Svg => match &self.geometry {
                Some(geometry) => sequence_export::svg(&self.diagram, geometry),
                None => String::new(),
            },
        }
    }

    fn copy(&mut self, kind: ExportKind, cx: &mut Context<Self>) {
        let text = self.export_text(kind);
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        self.note = Some(format!("Copied {}", kind.name()).into());
        self.export_open = false;
        cx.notify();
    }

    fn save(&mut self, kind: ExportKind, window: &mut Window, cx: &mut Context<Self>) {
        let text = self.export_text(kind);
        let directory = self
            .workspace
            .upgrade()
            .map(|workspace| workspace.read(cx).root().to_path_buf())
            .unwrap_or_default();
        let chosen = cx.prompt_for_new_path(&directory, Some(kind.default_name()));
        self.export_open = false;
        cx.spawn_in(window, async move |this, cx| {
            let Ok(Ok(Some(path))) = chosen.await else {
                return;
            };
            let target = path.clone();
            let write = cx
                .background_executor()
                .spawn(async move { std::fs::write(&target, text) });
            let result = write.await;
            this.update(cx, |this, cx| {
                this.note = Some(
                    match &result {
                        Ok(()) => format!("Saved {}", path.display()),
                        Err(error) => format!("{}: {error}", path.display()),
                    }
                    .into(),
                );
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn arrows(&self, theme: &Theme) -> Rc<Vec<Arrow>> {
        let Some(geometry) = &self.geometry else {
            return Rc::new(Vec::new());
        };
        let color = theme.text_muted;
        let mut arrows = Vec::new();
        for (index, message) in self.diagram.messages.iter().enumerate() {
            if self.message_hidden(message) {
                continue;
            }
            let (Some(row), Some(from), Some(to)) = (
                geometry.rows.get(index),
                geometry.columns.get(message.from),
                geometry.columns.get(message.to),
            ) else {
                continue;
            };
            let y = row * self.zoom + self.pan.y;
            let from_x = from * self.zoom + self.pan.x;
            let to_x = to * self.zoom + self.pan.x;
            let points = if message.from == message.to {
                let side = 34. * self.zoom;
                vec![
                    point(from_x, y - 4. * self.zoom),
                    point(from_x + side, y - 16. * self.zoom),
                    point(from_x + side, y + 16. * self.zoom),
                    point(from_x, y + 4. * self.zoom),
                ]
            } else {
                vec![point(from_x, y), point(to_x, y)]
            };
            arrows.push(Arrow {
                points,
                returning: message.kind == SequenceMessageKind::Return,
                color,
            });
        }
        Rc::new(arrows)
    }
}

impl Render for SequenceDiagramView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let toolbar = self.toolbar(&theme, cx);
        let drawing = self.drawing(&theme, window, cx);
        let export = self.export_popup(&theme, cx);
        let note = self.note.clone();
        div()
            .id("sequence-tab")
            .key_context("SequenceDiagramTab")
            .track_focus(&self.focus)
            .flex()
            .flex_col()
            .size_full()
            .bg(theme.editor_bg)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .child(toolbar)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .child(drawing)
                    .children(export),
            )
            .when_some(note, |this, note| {
                this.child(
                    div()
                        .px(px(SPACE_M))
                        .py(px(SPACE_XS))
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_secondary)
                        .child(note),
                )
            })
    }
}

impl SequenceDiagramView {
    fn toolbar(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let toggle = |id: &'static str,
                      label: &'static str,
                      on: bool,
                      theme: &Theme,
                      cx: &mut Context<Self>,
                      run: fn(&mut Self, &mut Context<Self>)| {
            let theme = *theme;
            div()
                .id(id)
                .debug_selector(move || id.to_owned())
                .flex()
                .items_center()
                .h(px(24.))
                .px(px(SPACE_S))
                .rounded(px(CONTROL_RADIUS))
                .cursor_pointer()
                .when(on, |this| this.bg(theme.selection))
                .when(!on, |this| this.text_color(theme.text_secondary))
                .tooltip(move |_, cx| tooltip(SharedString::from(label), None, cx))
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| run(this, cx)))
        };
        let icon = |id: &'static str,
                    name: &'static str,
                    label: &'static str,
                    theme: &Theme,
                    cx: &mut Context<Self>,
                    run: fn(&mut Self, &mut Context<Self>)| {
            icon_button(id, name, label, None, theme)
                .debug_selector(move || id.to_owned())
                .on_click(cx.listener(move |this, _, _, cx| run(this, cx)))
        };
        let mut bar = div()
            .id("sequence-toolbar")
            .flex()
            .items_center()
            .flex_wrap()
            .gap(px(SPACE_XS))
            .px(px(SPACE_M))
            .py(px(SPACE_XS))
            .border_b_1()
            .border_color(theme.toolbar_border);
        bar = bar
            .child(
                div()
                    .id("sequence-depth-down")
                    .debug_selector(|| "sequence-depth-down".to_owned())
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .cursor_pointer()
                    .tooltip(|_, cx| tooltip("Less Deep", None, cx))
                    .child("-")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.settings.depth = this.settings.depth.saturating_sub(1);
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("sequence-depth")
                    .debug_selector(|| "sequence-depth".to_owned())
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .px(px(SPACE_XS))
                    .text_color(theme.text_secondary)
                    .child(format!("Depth {}", self.settings.depth)),
            )
            .child(icon(
                "sequence-depth-up",
                "general/add",
                "Deeper",
                theme,
                cx,
                |this, cx| {
                    this.settings.depth = (this.settings.depth + 1).min(20);
                    cx.notify();
                },
            ))
            .child(toggle(
                "sequence-skip-accessors",
                "Skip Getters/Setters",
                self.settings.skip_accessors,
                theme,
                cx,
                |this, cx| {
                    this.settings.skip_accessors = !this.settings.skip_accessors;
                    cx.notify();
                },
            ))
            .child(toggle(
                "sequence-skip-private",
                "Skip Private",
                self.settings.skip_private,
                theme,
                cx,
                |this, cx| {
                    this.settings.skip_private = !this.settings.skip_private;
                    cx.notify();
                },
            ))
            .child(toggle(
                "sequence-skip-constructors",
                "Skip Constructors",
                self.settings.skip_constructors,
                theme,
                cx,
                |this, cx| {
                    this.settings.skip_constructors = !this.settings.skip_constructors;
                    cx.notify();
                },
            ))
            .child(toggle(
                "sequence-project-only",
                "Project Only",
                self.settings.project_only,
                theme,
                cx,
                |this, cx| {
                    this.settings.project_only = !this.settings.project_only;
                    cx.notify();
                },
            ))
            .child(icon(
                "sequence-rebuild",
                "general/refresh",
                "Rebuild",
                theme,
                cx,
                |this, cx| this.rebuild(cx),
            ))
            .child(div().flex_1());
        if !self.hidden.is_empty() {
            bar = bar.child(
                div()
                    .id("sequence-show-hidden")
                    .debug_selector(|| "sequence-show-hidden".to_owned())
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .cursor_pointer()
                    .text_color(theme.text_secondary)
                    .tooltip(|_, cx| tooltip("Show Hidden Participants", None, cx))
                    .child("Show Hidden")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hidden.clear();
                        cx.notify();
                    })),
            );
        }
        bar = bar
            .child(icon(
                "sequence-fit",
                "general/expandAll",
                "Fit",
                theme,
                cx,
                |this, cx| {
                    this.fit();
                    cx.notify();
                },
            ))
            .child(icon(
                "sequence-zoom-out",
                "general/minus",
                "Zoom Out",
                theme,
                cx,
                |this, cx| {
                    this.zoom = (this.zoom / ZOOM_STEP).clamp(MIN_ZOOM, MAX_ZOOM);
                    cx.notify();
                },
            ))
            .child(icon(
                "sequence-zoom-in",
                "general/add",
                "Zoom In",
                theme,
                cx,
                |this, cx| {
                    this.zoom = (this.zoom * ZOOM_STEP).clamp(MIN_ZOOM, MAX_ZOOM);
                    cx.notify();
                },
            ))
            .child(
                icon_button("sequence-export", "general/download", "Export", None, theme)
                    .debug_selector(|| "sequence-export".to_owned())
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.export_open = !this.export_open;
                        cx.notify();
                    })),
            );
        bar.into_any_element()
    }

    fn drawing(&self, theme: &Theme, _window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let Some(geometry) = &self.geometry else {
            return div()
                .id("sequence-diagram")
                .debug_selector(|| "sequence-diagram".to_owned())
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme.text_secondary)
                .child("Building the diagram...")
                .into_any_element();
        };
        let arrows = self.arrows(theme);
        let participants = self.participant_elements(geometry, theme, cx);
        let lifelines = self.lifeline_elements(geometry, theme);
        let frames = self.frame_elements(geometry, theme);
        let activations = self.activation_elements(geometry, theme);
        let labels = self.label_elements(geometry, theme);
        let rows = self.message_elements(geometry, theme, cx);
        let title = self.diagram.title.clone();
        let bounds = self.bounds.clone();
        div()
            .id("sequence-diagram")
            .debug_selector(|| "sequence-diagram".to_owned())
            .absolute()
            .inset_0()
            .bg(theme.editor_bg)
            .cursor_pointer()
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.pan_from = Some(event.position);
                    cx.notify();
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                let Some(from) = this.pan_from else {
                    return;
                };
                if event.pressed_button != Some(MouseButton::Left) {
                    return;
                }
                this.pan = point(
                    this.pan.x + f32::from(event.position.x - from.x),
                    this.pan.y + f32::from(event.position.y - from.y),
                );
                this.pan_from = Some(event.position);
                cx.notify();
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.pan_from = None;
                    cx.notify();
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.pan_from = None;
                    cx.notify();
                }),
            )
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _, cx| {
                let delta = match event.delta {
                    ScrollDelta::Pixels(delta) => point(f32::from(delta.x), f32::from(delta.y)),
                    ScrollDelta::Lines(delta) => point(delta.x * ROW_HEIGHT, delta.y * ROW_HEIGHT),
                };
                if event.modifiers.control {
                    let factor = if delta.y > 0. {
                        ZOOM_STEP
                    } else if delta.y < 0. {
                        1. / ZOOM_STEP
                    } else {
                        1.
                    };
                    this.zoom_at(factor, event.position);
                } else {
                    this.pan = point(this.pan.x + delta.x, this.pan.y + delta.y);
                }
                cx.notify();
            }))
            .child(
                div()
                    .absolute()
                    .left(px(SPACE_M))
                    .top(px(SPACE_S))
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme.text)
                    .child(title),
            )
            .children(lifelines)
            .children(frames)
            .children(activations)
            .child(
                canvas(
                    {
                        let bounds = bounds.clone();
                        move |area, _, _| bounds.set(Some(area))
                    },
                    {
                        let bounds = bounds.clone();
                        let arrows = arrows.clone();
                        move |area, _, window, _| {
                            bounds.set(Some(area));
                            paint_arrows(&arrows, area, window);
                        }
                    },
                )
                .absolute()
                .inset_0(),
            )
            .children(labels)
            .children(rows)
            .children(participants)
            .into_any_element()
    }

    fn participant_elements(
        &self,
        geometry: &Geometry,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut found = Vec::new();
        for (index, participant) in self.diagram.participants.iter().enumerate() {
            if self.is_hidden(index) {
                continue;
            }
            let Some(column) = geometry.columns.get(index) else {
                continue;
            };
            let left = (column - PARTICIPANT_WIDTH / 2.) * self.zoom + self.pan.x;
            let top = sequence_layout::TOP * self.zoom + self.pan.y;
            let width = PARTICIPANT_WIDTH * self.zoom;
            let height = PARTICIPANT_HEIGHT * self.zoom;
            if !diagram_canvas::visible(left, top, width, height, self.bounds.get()) {
                continue;
            }
            let background = if participant.kind == lang_api::SequenceParticipantKind::External {
                theme.fold_bg
            } else {
                theme.island_bg
            };
            let name = participant.name.clone();
            let selector = format!("sequence-participant-{}", participant.name);
            found.push(
                div()
                    .id(("sequence-participant", index))
                    .debug_selector(move || selector.clone())
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(width))
                    .h(px(height))
                    .rounded(px(CONTROL_RADIUS))
                    .bg(background)
                    .border_1()
                    .border_color(theme.border)
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .text_size(px((13. * self.zoom).max(4.)))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(
                        if participant.kind == lang_api::SequenceParticipantKind::External {
                            theme.text_muted
                        } else {
                            theme.text
                        },
                    )
                    .child(name)
                    .on_mouse_down(
                        MouseButton::Right,
                        cx.listener(move |this, _, _, cx| {
                            this.hidden.insert(index);
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .into_any_element(),
            );
        }
        found
    }

    fn lifeline_elements(&self, geometry: &Geometry, theme: &Theme) -> Vec<AnyElement> {
        let mut found = Vec::new();
        let top = (sequence_layout::TOP + PARTICIPANT_HEIGHT) * self.zoom + self.pan.y;
        let bottom = (geometry.height - sequence_layout::MARGIN) * self.zoom + self.pan.y;
        for (index, column) in geometry.columns.iter().enumerate() {
            if self.is_hidden(index) {
                continue;
            }
            let x = column * self.zoom + self.pan.x;
            if !diagram_canvas::visible(x, top, 1., bottom - top, self.bounds.get()) {
                continue;
            }
            found.push(
                div()
                    .id(("sequence-lifeline", index))
                    .absolute()
                    .left(px(x))
                    .top(px(top))
                    .w(px(1.))
                    .h(px((bottom - top).max(0.)))
                    .bg(theme.border)
                    .into_any_element(),
            );
        }
        found
    }

    fn frame_elements(&self, geometry: &Geometry, theme: &Theme) -> Vec<AnyElement> {
        let mut found = Vec::new();
        for frame in &geometry.fragments {
            let Some(fragment) = self.diagram.fragments.get(frame.fragment) else {
                continue;
            };
            let left = frame.left * self.zoom + self.pan.x;
            let top = frame.top * self.zoom + self.pan.y;
            let width = frame.width * self.zoom;
            let height = frame.height * self.zoom;
            if !diagram_canvas::visible(left, top, width, height, self.bounds.get()) {
                continue;
            }
            let condition = fragment
                .sections
                .first()
                .map(|section| section.condition.clone())
                .unwrap_or_default();
            let label = sequence_layout::fragment_label(fragment.kind, &condition);
            let border = match fragment.kind {
                SequenceFragmentKind::Alt => theme.text_muted,
                _ => theme.border,
            };
            let mut body = div()
                .id(("sequence-frame", frame.fragment))
                .debug_selector({
                    let index = frame.fragment;
                    move || format!("sequence-fragment-{index}")
                })
                .absolute()
                .left(px(left))
                .top(px(top))
                .w(px(width))
                .h(px(height))
                .border_1()
                .border_color(border)
                .rounded(px(CONTROL_RADIUS));
            if !label.is_empty() {
                body = body.child(
                    div()
                        .absolute()
                        .left(px(FRAME_LABEL_GAP * self.zoom))
                        .top(px((FRAME_LABEL_GAP - 8.) * self.zoom))
                        .px(px(SPACE_XS * self.zoom))
                        .bg(theme.editor_bg)
                        .text_size(px((11. * self.zoom).max(4.)))
                        .text_color(theme.text_secondary)
                        .child(label),
                );
            }
            for separator in &frame.separators {
                let y = (separator - frame.top) * self.zoom;
                body = body.child(
                    div()
                        .absolute()
                        .left(px(0.))
                        .top(px(y))
                        .w_full()
                        .border_t_1()
                        .border_color(theme.border),
                );
            }
            for section in fragment.sections.iter().skip(1) {
                let Some(row) = geometry.rows.get(section.messages.start) else {
                    continue;
                };
                let y = (row - ROW_HEIGHT / 2. - frame.top) * self.zoom;
                body = body.child(
                    div()
                        .absolute()
                        .left(px(FRAME_LABEL_GAP * self.zoom))
                        .top(px(y + (FRAME_LABEL_GAP - 8.) * self.zoom))
                        .px(px(SPACE_XS * self.zoom))
                        .bg(theme.editor_bg)
                        .text_size(px((11. * self.zoom).max(4.)))
                        .text_color(theme.text_muted)
                        .child(format!("[{}]", section.condition)),
                );
            }
            found.push(body.into_any_element());
        }
        found
    }

    fn activation_elements(&self, geometry: &Geometry, theme: &Theme) -> Vec<AnyElement> {
        let mut found = Vec::new();
        for (index, activation) in geometry.activations.iter().enumerate() {
            if self.is_hidden(activation.participant) {
                continue;
            }
            let Some(column) = geometry.columns.get(activation.participant) else {
                continue;
            };
            let left = (column - 5.) * self.zoom + self.pan.x;
            let top = activation.top * self.zoom + self.pan.y;
            let height = (activation.bottom - activation.top) * self.zoom;
            if !diagram_canvas::visible(left, top, 10. * self.zoom, height, self.bounds.get()) {
                continue;
            }
            found.push(
                div()
                    .id(("sequence-activation", index))
                    .debug_selector(move || format!("sequence-activation-{index}"))
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(10. * self.zoom))
                    .h(px(height))
                    .bg(theme.fold_bg)
                    .border_1()
                    .border_color(theme.border)
                    .into_any_element(),
            );
        }
        found
    }

    fn label_elements(&self, geometry: &Geometry, theme: &Theme) -> Vec<AnyElement> {
        let mut found = Vec::new();
        for (index, message) in self.diagram.messages.iter().enumerate() {
            if self.message_hidden(message) {
                continue;
            }
            let (Some(row), Some(from), Some(to)) = (
                geometry.rows.get(index),
                geometry.columns.get(message.from),
                geometry.columns.get(message.to),
            ) else {
                continue;
            };
            let y = row * self.zoom + self.pan.y;
            let from_x = from * self.zoom + self.pan.x;
            let to_x = to * self.zoom + self.pan.x;
            let left = if message.from == message.to {
                from_x + 40. * self.zoom
            } else {
                (from_x + to_x) / 2. - LABEL_WIDTH / 2.
            };
            let top = y - 22. * self.zoom;
            if !diagram_canvas::visible(left, top, LABEL_WIDTH, 18. * self.zoom, self.bounds.get())
            {
                continue;
            }
            let color = if message.kind == SequenceMessageKind::Return {
                theme.text_muted
            } else {
                theme.code
            };
            found.push(
                div()
                    .id(("sequence-message-label", index))
                    .absolute()
                    .left(px(left))
                    .top(px(top))
                    .w(px(LABEL_WIDTH))
                    .text_size(px((12. * self.zoom).max(4.)))
                    .text_color(color)
                    .when(message.kind == SequenceMessageKind::Return, |this| {
                        this.italic()
                    })
                    .child(message.label.clone())
                    .into_any_element(),
            );
        }
        found
    }

    fn message_elements(
        &self,
        geometry: &Geometry,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<AnyElement> {
        let mut found = Vec::new();
        for (index, message) in self.diagram.messages.iter().enumerate() {
            if self.message_hidden(message) {
                continue;
            }
            let Some(row) = geometry.rows.get(index) else {
                continue;
            };
            let top = (row - ROW_HEIGHT / 2.) * self.zoom + self.pan.y;
            let height = ROW_HEIGHT * self.zoom;
            if !diagram_canvas::visible(0., top, f32::MAX, height, self.bounds.get()) {
                continue;
            }
            let place = message.place.clone();
            found.push(
                div()
                    .id(("sequence-message", index))
                    .debug_selector(move || format!("sequence-message-{index}"))
                    .absolute()
                    .left(px(0.))
                    .top(px(top))
                    .w_full()
                    .h(px(height))
                    .hover(move |style| style.bg(theme.hover))
                    .on_click(cx.listener(move |this, event: &ClickEvent, window, cx| {
                        if event.click_count() >= 2 {
                            this.open_place(place.clone(), window, cx);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }))
                    .into_any_element(),
            );
        }
        found
    }

    fn export_popup(&self, theme: &Theme, cx: &mut Context<Self>) -> Option<AnyElement> {
        if !self.export_open {
            return None;
        }
        let rows = [
            ("sequence-copy-plantuml", "Copy PlantUML"),
            ("sequence-copy-mermaid", "Copy Mermaid"),
            ("sequence-copy-svg", "Copy SVG"),
            ("sequence-save-plantuml", "Save PlantUML..."),
            ("sequence-save-mermaid", "Save Mermaid..."),
            ("sequence-save-svg", "Save SVG..."),
        ];
        let mut items = Vec::new();
        for (index, (id, label)) in rows.into_iter().enumerate() {
            let kind = match index {
                0 | 3 => ExportKind::PlantUml,
                1 | 4 => ExportKind::Mermaid,
                _ => ExportKind::Svg,
            };
            let save = index >= 3;
            items.push(
                div()
                    .id(("sequence-export-row", index))
                    .debug_selector(move || id.to_owned())
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .px(px(SPACE_S))
                    .rounded(px(CONTROL_RADIUS))
                    .hover(move |style| style.bg(theme.hover))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if save {
                            this.save(kind, window, cx);
                        } else {
                            this.copy(kind, cx);
                        }
                    }))
                    .into_any_element(),
            );
        }
        Some(
            div()
                .id("sequence-export-popup")
                .debug_selector(|| "sequence-export-popup".to_owned())
                .absolute()
                .top(px(SPACE_S))
                .right(px(SPACE_M))
                .w(px(220.))
                .p(px(SPACE_XS))
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(theme.popup_border)
                .bg(theme.popup_bg)
                .shadow_lg()
                .children(items)
                .into_any_element(),
        )
    }
}

/// Paints the arrows of the visible messages, with a head at the target end.
fn paint_arrows(arrows: &Rc<Vec<Arrow>>, bounds: Bounds<Pixels>, window: &mut Window) {
    let width = f32::from(bounds.size.width);
    let height = f32::from(bounds.size.height);
    for arrow in arrows.iter() {
        let right = arrow.points.iter().map(|at| at.x).fold(f32::MIN, f32::max);
        let left = arrow.points.iter().map(|at| at.x).fold(f32::MAX, f32::min);
        let bottom = arrow.points.iter().map(|at| at.y).fold(f32::MIN, f32::max);
        let top = arrow.points.iter().map(|at| at.y).fold(f32::MAX, f32::min);
        if right < 0. || bottom < 0. || left > width || top > height {
            continue;
        }
        let points: Vec<Point<Pixels>> = arrow
            .points
            .iter()
            .map(|at| point(bounds.left() + px(at.x), bounds.top() + px(at.y)))
            .collect();
        if arrow.returning {
            for pair in points.windows(2) {
                diagram_canvas::paint_dashed(pair[0], pair[1], arrow.color, window);
            }
        } else {
            let mut path = PathBuilder::stroke(px(1.2));
            path.move_to(points[0]);
            for at in &points[1..] {
                path.line_to(*at);
            }
            if let Ok(path) = path.build() {
                window.paint_path(path, arrow.color);
            }
        }
        let tip = *points.last().unwrap_or(&points[0]);
        let before = points
            .iter()
            .rev()
            .find(|at| at.x != tip.x || at.y != tip.y)
            .copied()
            .unwrap_or(tip);
        let dx = f32::from(tip.x - before.x);
        let dy = f32::from(tip.y - before.y);
        let length = (dx * dx + dy * dy).sqrt().max(0.001);
        let (ux, uy) = (dx / length, dy / length);
        let base = point(tip.x - px(ux * 8.), tip.y - px(uy * 8.));
        let (nx, ny) = (-uy * 4., ux * 4.);
        let mut head = PathBuilder::fill();
        head.move_to(tip);
        head.line_to(point(base.x + px(nx), base.y + px(ny)));
        head.line_to(point(base.x - px(nx), base.y - px(ny)));
        head.line_to(tip);
        if let Ok(path) = head.build() {
            window.paint_path(path, arrow.color);
        }
    }
}

/// Opens the sequence diagram of a method, in the tab that shows sequence diagrams.
pub(crate) fn show(
    workspace: &mut Workspace,
    spot: DocumentSpot,
    settings: SequenceSettings,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let path = workspace.root().join(TAB_PATH_FOLDER).join(TAB_PATH_NAME);
    let title = "Sequence Diagram";
    let existing = workspace
        .view_of(&path)
        .and_then(|view| view.downcast::<SequenceDiagramView>().ok());
    if let Some(view) = existing {
        view.update(cx, |view, cx| view.start(spot, settings, cx));
        let focus = view.read(cx).focus();
        workspace.open_view_tab(
            path,
            title,
            "general/refresh",
            view.into(),
            focus,
            window,
            cx,
        );
        return;
    }
    let weak = cx.weak_entity();
    let view = SequenceDiagramView::create(weak, cx);
    let focus = view.read(cx).focus();
    view.update(cx, |view, cx| view.start(spot, settings, cx));
    workspace.open_view_tab(
        path,
        title,
        "general/refresh",
        view.into(),
        focus,
        window,
        cx,
    );
}

/// Generate Sequence Diagram: the method under the caret of the active editor.
pub(crate) fn show_current(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(view) = workspace
        .active_view()
        .and_then(|view| view.downcast::<editor_view::EditorView>().ok())
    else {
        return;
    };
    let Some((spot, _)) = view.read(cx).spot() else {
        return;
    };
    show(workspace, spot, SequenceSettings::default(), window, cx);
}

impl Drop for SequenceDiagramView {
    fn drop(&mut self) {
        self.serial += 1;
    }
}

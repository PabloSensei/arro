//! The frame of a modal dialog: a title bar with a close cross, a drag by the title bar, a
//! resize by the four edges and the four corners, and a backdrop that takes the pointer
//! away from the window under the dialog. A dialog keeps its place and its size for the
//! session, by its key. The geometry is plain functions, so the rules are tested here
//! without a window.

use std::collections::HashMap;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::metrics::*;
use crate::{Theme, enter, icon_button};

/// Width and height in pixels.
pub type Size2 = (f32, f32);

/// The height of the title bar: the padding above it and the height of a control.
const TITLE_HEIGHT: f32 = CONTROL_HEIGHT + DIALOG_PADDING;
/// The grab area of an edge or a corner.
const GRIP: f32 = 8.;
/// How much of a dialog stays on the window when it is dragged off a side.
const KEPT: f32 = 120.;

/// Where a dialog window stands in the window of the application, in pixels from its top
/// left corner. `height` is `None` while the content decides the height.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DialogPlace {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: Option<f32>,
}

/// What a dialog gives its frame.
pub struct DialogSpec {
    /// Names the dialog for the session and for its selectors: `{key}-title`,
    /// `{key}-close`, `{key}-resize-*`.
    pub key: &'static str,
    /// The text of the title bar.
    pub title: SharedString,
    /// What the title bar shows after the title, before the close cross.
    pub extra: Option<AnyElement>,
    /// The place and the size of the dialog on its first opening.
    pub place: DialogPlace,
    /// The smallest size of the dialog. A window smaller than this shows the dialog at its
    /// own size.
    pub min: Size2,
}

/// The sides of a dialog window that a drag can grab.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DialogEdge {
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl DialogEdge {
    /// Every grip. The edges come first, so a corner is painted over the edges it touches.
    const ALL: [DialogEdge; 8] = [
        DialogEdge::Left,
        DialogEdge::Right,
        DialogEdge::Top,
        DialogEdge::Bottom,
        DialogEdge::TopLeft,
        DialogEdge::TopRight,
        DialogEdge::BottomLeft,
        DialogEdge::BottomRight,
    ];

    /// The part of the grip name in its selector. The bottom right corner keeps the name
    /// `corner` that the Settings window has always used.
    fn name(self) -> &'static str {
        match self {
            DialogEdge::Left => "left",
            DialogEdge::Right => "right",
            DialogEdge::Top => "top",
            DialogEdge::Bottom => "bottom",
            DialogEdge::TopLeft => "top-left",
            DialogEdge::TopRight => "top-right",
            DialogEdge::BottomLeft => "bottom-left",
            DialogEdge::BottomRight => "corner",
        }
    }

    fn west(self) -> bool {
        matches!(
            self,
            DialogEdge::Left | DialogEdge::TopLeft | DialogEdge::BottomLeft
        )
    }

    fn east(self) -> bool {
        matches!(
            self,
            DialogEdge::Right | DialogEdge::TopRight | DialogEdge::BottomRight
        )
    }

    fn north(self) -> bool {
        matches!(
            self,
            DialogEdge::Top | DialogEdge::TopLeft | DialogEdge::TopRight
        )
    }

    fn south(self) -> bool {
        matches!(
            self,
            DialogEdge::Bottom | DialogEdge::BottomLeft | DialogEdge::BottomRight
        )
    }
}

/// `value` kept between `low` and `high`. When `high` is below `low`, the result is `high`:
/// a window smaller than a minimum shows the dialog at the size of the window.
fn bounded(value: f32, low: f32, high: f32) -> f32 {
    let high = high.max(0.);
    let low = low.min(high);
    value.max(low).min(high)
}

/// The place of a dialog on a window of `viewport`: no larger than the window, no smaller
/// than `min` unless the window is smaller, and inside the window.
pub fn fitted(place: DialogPlace, min: Size2, viewport: Size2) -> DialogPlace {
    let width = bounded(place.width, min.0, viewport.0);
    let height = place
        .height
        .map(|height| bounded(height, min.1, viewport.1));
    let left = bounded(place.left, 0., viewport.0 - width);
    let top = bounded(place.top, 0., viewport.1 - height.unwrap_or(TITLE_HEIGHT));
    DialogPlace {
        left,
        top,
        width,
        height,
    }
}

/// The place after a drag of the title bar by (`dx`, `dy`) from `start`. The dialog may go
/// off a side of the window, but at least `KEPT` pixels of it stay on the window, and its
/// title bar never goes above the window.
pub fn moved(start: DialogPlace, dx: f32, dy: f32, viewport: Size2) -> DialogPlace {
    DialogPlace {
        left: bounded(start.left + dx, KEPT - start.width, viewport.0 - KEPT),
        top: bounded(start.top + dy, 0., viewport.1 - TITLE_HEIGHT),
        ..start
    }
}

/// The place after a drag of `edge` by (`dx`, `dy`) from `start`. `shown` is the size the
/// dialog is drawn at, which measures a height that the content decides. The side opposite
/// the edge stays where it is. No size goes below `min` unless the window is smaller, and no
/// side leaves the window. A horizontal drag leaves an automatic height automatic.
pub fn resized(
    start: DialogPlace,
    shown: Size2,
    edge: DialogEdge,
    dx: f32,
    dy: f32,
    min: Size2,
    viewport: Size2,
) -> DialogPlace {
    let mut next = DialogPlace {
        width: shown.0,
        height: start.height,
        ..start
    };
    if edge.east() {
        let room = viewport.0 - start.left;
        next.width = bounded(shown.0 + dx, min.0.min(room), room);
    }
    if edge.west() {
        let right = start.left + shown.0;
        next.left = bounded(start.left + dx, 0., right - min.0.min(right));
        next.width = right - next.left;
    }
    if edge.south() {
        let room = viewport.1 - start.top;
        next.height = Some(bounded(shown.1 + dy, min.1.min(room), room));
    }
    if edge.north() {
        let bottom = start.top + shown.1;
        next.top = bounded(start.top + dy, 0., bottom - min.1.min(bottom));
        next.height = Some(bottom - next.top);
    }
    next
}

/// The places of the dialogs of the session, the size each one is drawn at, and the drag or
/// resize under way. A dialog that closes in the middle of a drag leaves its gesture behind,
/// which the next dialog ignores: a gesture is tied to the entity that started it.
#[derive(Default)]
pub struct DialogPlaces {
    places: HashMap<&'static str, DialogPlace>,
    shown: HashMap<&'static str, Size2>,
    gesture: Option<Gesture>,
}

impl Global for DialogPlaces {}

#[derive(Clone, Copy)]
struct Gesture {
    entity: EntityId,
    key: &'static str,
    /// The edge the drag grabs, or `None` for a drag of the title bar.
    edge: Option<DialogEdge>,
    from: Point<Pixels>,
    start: DialogPlace,
    shown: Size2,
    min: Size2,
}

fn size_of(size: Size<Pixels>) -> Size2 {
    (f32::from(size.width), f32::from(size.height))
}

fn begin(
    cx: &mut App,
    entity: EntityId,
    key: &'static str,
    edge: Option<DialogEdge>,
    from: Point<Pixels>,
    min: Size2,
) {
    let places = cx.default_global::<DialogPlaces>();
    let Some(start) = places.places.get(key).copied() else {
        return;
    };
    let shown = places
        .shown
        .get(key)
        .copied()
        .unwrap_or((start.width, start.height.unwrap_or(TITLE_HEIGHT)));
    places.gesture = Some(Gesture {
        entity,
        key,
        edge,
        from,
        start,
        shown,
        min,
    });
    cx.notify(entity);
}

/// Follows the pointer: the dialog moves or resizes from where the gesture began. A move
/// that reports no button held ends the gesture, so a missed release cannot stick.
fn drag(cx: &mut App, entity: EntityId, position: Point<Pixels>, held: bool, viewport: Size2) {
    let places = cx.default_global::<DialogPlaces>();
    let Some(gesture) = places.gesture else {
        return;
    };
    if gesture.entity != entity {
        return;
    }
    if !held {
        places.gesture = None;
        cx.notify(entity);
        return;
    }
    let dx = f32::from(position.x) - f32::from(gesture.from.x);
    let dy = f32::from(position.y) - f32::from(gesture.from.y);
    let next = match gesture.edge {
        None => moved(gesture.start, dx, dy, viewport),
        Some(edge) => resized(
            gesture.start,
            gesture.shown,
            edge,
            dx,
            dy,
            gesture.min,
            viewport,
        ),
    };
    places.places.insert(gesture.key, next);
    cx.notify(entity);
}

fn end(cx: &mut App, entity: EntityId) {
    let places = cx.default_global::<DialogPlaces>();
    if places
        .gesture
        .is_some_and(|gesture| gesture.entity == entity)
    {
        places.gesture = None;
        cx.notify(entity);
    }
}

/// One edge or corner of the window: a strip that starts a resize from it.
fn grip<T: 'static>(
    cx: &mut Context<T>,
    entity: EntityId,
    key: &'static str,
    edge: DialogEdge,
    min: Size2,
) -> Stateful<Div> {
    let name = format!("{key}-resize-{}", edge.name());
    let grip = div()
        .id(name.clone())
        .debug_selector(move || name.clone())
        .absolute()
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                begin(cx, entity, key, Some(edge), event.position, min);
            }),
        )
        .on_mouse_move(cx.listener(move |_, event: &MouseMoveEvent, window, cx| {
            drag(
                cx,
                entity,
                event.position,
                event.dragging(),
                size_of(window.viewport_size()),
            );
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |_, _: &MouseUpEvent, _, cx| end(cx, entity)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(move |_, _: &MouseUpEvent, _, cx| end(cx, entity)),
        );
    match edge {
        DialogEdge::Left => grip
            .top_0()
            .bottom_0()
            .left_0()
            .w(px(GRIP))
            .cursor_col_resize(),
        DialogEdge::Right => grip
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(GRIP))
            .cursor_col_resize(),
        DialogEdge::Top => grip
            .top_0()
            .left_0()
            .right_0()
            .h(px(GRIP))
            .cursor_row_resize(),
        DialogEdge::Bottom => grip
            .bottom_0()
            .left_0()
            .right_0()
            .h(px(GRIP))
            .cursor_row_resize(),
        DialogEdge::TopLeft => grip
            .top_0()
            .left_0()
            .w(px(GRIP))
            .h(px(GRIP))
            .cursor_nwse_resize(),
        DialogEdge::TopRight => grip
            .top_0()
            .right_0()
            .w(px(GRIP))
            .h(px(GRIP))
            .cursor_nesw_resize(),
        DialogEdge::BottomLeft => grip
            .bottom_0()
            .left_0()
            .w(px(GRIP))
            .h(px(GRIP))
            .cursor_nesw_resize(),
        DialogEdge::BottomRight => grip
            .bottom_0()
            .right_0()
            .w(px(GRIP))
            .h(px(GRIP))
            .cursor_nwse_resize(),
    }
}

/// The frame of a dialog, as the window the dialog draws. `shell` is the dialog's own
/// element, with its id, its key context, its focus and its keys; the frame sets its place,
/// its size and its children: the title bar, `contents`, and the grips. `close` runs when
/// the close cross is clicked, and it should do what the Cancel button does.
///
/// The result covers the whole window and takes the pointer from the window under it, so a
/// click outside the dialog does nothing. The caller puts it into the overlay of the window.
pub fn dialog_window<T: 'static>(
    spec: DialogSpec,
    shell: Stateful<Div>,
    window: &mut Window,
    cx: &mut Context<T>,
    close: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    contents: impl IntoElement,
) -> Div {
    let DialogSpec {
        key,
        title,
        extra,
        place: first,
        min,
    } = spec;
    let theme = *cx.global::<Theme>();
    let entity = cx.entity_id();
    let viewport = size_of(window.viewport_size());
    // The place the dialog stood on last, fitted to the window as it is now.
    let place = {
        let places = cx.default_global::<DialogPlaces>();
        let stored = places.places.get(key).copied().unwrap_or(first);
        let place = fitted(stored, min, viewport);
        places.places.insert(key, place);
        place
    };
    let dragging = cx
        .default_global::<DialogPlaces>()
        .gesture
        .is_some_and(|gesture| gesture.entity == entity);

    let cross = icon_button(
        format!("{key}-close"),
        "general/close",
        "Close",
        None,
        &theme,
    )
    .debug_selector(move || format!("{key}-close"))
    // A press on the cross is not the start of a drag of the title bar.
    .on_mouse_down(
        MouseButton::Left,
        |_: &MouseDownEvent, _: &mut Window, cx: &mut App| {
            cx.stop_propagation();
        },
    )
    .on_click(close);
    let header = div()
        .flex_none()
        .px(px(DIALOG_PADDING))
        .pt(px(DIALOG_PADDING))
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |_, event: &MouseDownEvent, _, cx| {
                begin(cx, entity, key, None, event.position, min);
            }),
        )
        .on_mouse_move(cx.listener(move |_, event: &MouseMoveEvent, window, cx| {
            drag(
                cx,
                entity,
                event.position,
                event.dragging(),
                size_of(window.viewport_size()),
            );
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(move |_, _: &MouseUpEvent, _, cx| end(cx, entity)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(move |_, _: &MouseUpEvent, _, cx| end(cx, entity)),
        )
        .child(
            div()
                .debug_selector(move || format!("{key}-title"))
                .flex()
                .items_center()
                .justify_between()
                .h(px(CONTROL_HEIGHT))
                .child(
                    div()
                        .text_size(px(TEXT_TITLE))
                        .font_weight(FontWeight::MEDIUM)
                        .child(title),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SPACE_M))
                        .children(extra)
                        .child(cross),
                ),
        );
    let body = div().flex().flex_col().flex_1().min_h_0().child(contents);
    // Measures the size the window is drawn at: a height that the content decides is
    // only known after the layout, and a resize starts from it.
    let measure = canvas(
        move |bounds, _, cx| {
            cx.default_global::<DialogPlaces>()
                .shown
                .insert(key, size_of(bounds.size));
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full();

    let mut frame = shell
        .absolute()
        .left(px(place.left))
        .top(px(place.top))
        .w(px(place.width))
        .occlude()
        .child(header)
        .child(body)
        .child(measure);
    for edge in DialogEdge::ALL {
        frame = frame.child(grip(cx, entity, key, edge, min));
    }
    let frame = match place.height {
        Some(height) => frame.h(px(height)),
        None => frame.max_h(px(viewport.1)),
    };

    // Only while a drag or a resize is under way: a layer over the window that keeps the
    // pointer events of the gesture, even where the pointer leaves the dialog.
    let catcher = dragging.then(|| {
        div()
            .id(format!("{key}-drag-catcher"))
            .debug_selector(move || format!("{key}-drag-catcher"))
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .occlude()
            .on_mouse_move(cx.listener(move |_, event: &MouseMoveEvent, window, cx| {
                drag(
                    cx,
                    entity,
                    event.position,
                    event.dragging(),
                    size_of(window.viewport_size()),
                );
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |_, _: &MouseUpEvent, _, cx| end(cx, entity)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(move |_, _: &MouseUpEvent, _, cx| end(cx, entity)),
            )
    });

    // The backdrop covers the window: a click outside the dialog reaches no control under it.
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .occlude()
        .child(enter(format!("{key}-enter"), frame))
        .children(catcher)
}

#[cfg(test)]
mod tests {
    use super::{DialogEdge, DialogPlace, fitted, moved, resized};

    const VIEW: (f32, f32) = (1000., 800.);

    fn place(left: f32, top: f32, width: f32, height: Option<f32>) -> DialogPlace {
        DialogPlace {
            left,
            top,
            width,
            height,
        }
    }

    #[test]
    fn a_place_that_fits_the_window_stays_as_it_is() {
        let at = place(100., 50., 400., Some(300.));
        assert_eq!(fitted(at, (300., 200.), VIEW), at);
    }

    #[test]
    fn a_place_outside_a_smaller_window_is_brought_inside() {
        let at = fitted(
            place(700., 600., 500., Some(500.)),
            (300., 200.),
            (800., 700.),
        );
        assert_eq!(at, place(300., 200., 500., Some(500.)));
    }

    #[test]
    fn a_window_smaller_than_the_minimum_gives_the_dialog_its_size() {
        let at = fitted(place(0., 0., 500., Some(400.)), (420., 300.), (300., 200.));
        assert_eq!(at.width, 300., "the minimum is not larger than the window");
        assert_eq!(at.height, Some(200.));
    }

    #[test]
    fn a_drag_moves_the_dialog_by_the_offset() {
        let at = moved(place(100., 100., 400., None), 30., -20., VIEW);
        assert_eq!(at, place(130., 80., 400., None));
    }

    #[test]
    fn the_dialog_can_leave_a_side_but_not_all_of_it() {
        let at = moved(place(100., 100., 400., Some(300.)), -5000., 0., VIEW);
        assert_eq!(at.left, 120. - 400., "120 pixels stay on the right side");
        assert!(at.left + at.width >= 120.);
        let far = moved(place(100., 100., 400., Some(300.)), 5000., 0., VIEW);
        assert_eq!(far.left, VIEW.0 - 120., "120 pixels stay on the left side");
    }

    #[test]
    fn the_title_bar_never_goes_above_the_window() {
        let at = moved(place(100., 100., 400., Some(300.)), 0., -5000., VIEW);
        assert_eq!(at.top, 0.);
        let low = moved(place(100., 100., 400., Some(300.)), 0., 5000., VIEW);
        assert!(low.top < VIEW.1, "the title bar stays on the window");
    }

    #[test]
    fn the_corner_grows_the_dialog_and_the_far_sides_stay() {
        let at = resized(
            place(100., 100., 400., None),
            (400., 250.),
            DialogEdge::BottomRight,
            80.,
            50.,
            (300., 200.),
            VIEW,
        );
        assert_eq!(at, place(100., 100., 480., Some(300.)));
    }

    #[test]
    fn the_left_edge_keeps_the_right_edge_where_it_is() {
        let at = resized(
            place(100., 100., 400., Some(300.)),
            (400., 300.),
            DialogEdge::Left,
            -50.,
            0.,
            (300., 200.),
            VIEW,
        );
        assert_eq!(at.left, 50.);
        assert_eq!(at.left + at.width, 500., "the right edge stays at 500");
    }

    #[test]
    fn the_left_edge_stops_at_the_minimum_and_keeps_the_right_edge() {
        let at = resized(
            place(100., 100., 400., Some(300.)),
            (400., 300.),
            DialogEdge::Left,
            300.,
            0.,
            (300., 200.),
            VIEW,
        );
        assert_eq!(at.width, 300.);
        assert_eq!(at.left + at.width, 500., "the right edge did not move");
    }

    #[test]
    fn the_top_edge_stops_at_the_minimum_and_keeps_the_bottom_edge() {
        let at = resized(
            place(100., 100., 400., Some(300.)),
            (400., 300.),
            DialogEdge::Top,
            0.,
            250.,
            (300., 200.),
            VIEW,
        );
        assert_eq!(at.height, Some(200.));
        assert_eq!(at.top + 200., 400., "the bottom edge stays at 400");
    }

    #[test]
    fn the_top_edge_cannot_leave_the_window() {
        let at = resized(
            place(100., 100., 400., Some(300.)),
            (400., 300.),
            DialogEdge::Top,
            0.,
            -5000.,
            (300., 200.),
            VIEW,
        );
        assert_eq!(at.top, 0.);
        assert_eq!(at.height, Some(400.));
    }

    #[test]
    fn a_resize_stops_at_the_minimum_and_at_the_window_edge() {
        let small = resized(
            place(100., 100., 400., Some(300.)),
            (400., 300.),
            DialogEdge::Right,
            -1000.,
            0.,
            (300., 200.),
            VIEW,
        );
        assert_eq!(small.width, 300.);
        let wide = resized(
            place(100., 100., 400., Some(300.)),
            (400., 300.),
            DialogEdge::Right,
            5000.,
            0.,
            (300., 200.),
            VIEW,
        );
        assert_eq!(wide.width, VIEW.0 - 100., "the window is the largest size");
    }

    #[test]
    fn a_horizontal_resize_keeps_an_automatic_height() {
        let at = resized(
            place(100., 100., 400., None),
            (400., 250.),
            DialogEdge::Right,
            20.,
            0.,
            (300., 200.),
            VIEW,
        );
        assert_eq!(at, place(100., 100., 420., None));
    }

    #[test]
    fn the_edges_come_before_the_corners_and_the_corner_keeps_settings_name() {
        assert_eq!(DialogEdge::BottomRight.name(), "corner");
        assert!(matches!(DialogEdge::ALL[0], DialogEdge::Left));
        assert!(matches!(DialogEdge::ALL[7], DialogEdge::BottomRight));
    }
}

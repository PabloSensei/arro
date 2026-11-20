//! The frame of a modal dialog: a title bar with a close cross, a drag by the title bar, a
//! resize by the four edges and the four corners, and a backdrop that takes the pointer
//! away from the window under the dialog. A dialog keeps its place and its size for the
//! session, by its key. The geometry and the gestures live in `floating`, shared with the
//! search popups, so the rules are tested there without a window.

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::floating::{
    DialogEdge, DialogPlace, DialogPlaces, Size2, begin, drag, drag_catcher, end, fitted, grip,
    size_of,
};
use crate::metrics::*;
use crate::{Theme, enter, icon_button};

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
    let catcher = dragging.then(|| drag_catcher(cx, entity, key));

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

//! Scrollbars over scrolled content: a thin rounded thumb at the edge of the area.
//! The thumb widens and brightens under the pointer, shows itself while the content
//! scrolls and fades out a second after the last movement. It is not drawn when the
//! content fits, and it is drawn over the content, so the layout does not change.
//!
//! `scrollable(id, handle, content, theme, window, cx)` wraps a scrolled `div` or a
//! `uniform_list` and gives it both bars. The handle carries the offset and the sizes;
//! the bars only read and set it, so the wrapper works with any scrolled element.

use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::Theme;
use crate::metrics::*;

/// How long the bars stay after the last scroll or after the pointer leaves them.
const HIDE_DELAY: Duration = Duration::from_millis(1000);
/// The fade of a bar that no longer needs to be shown.
const FADE: Duration = Duration::from_millis(120);

/// The scroll state a bar reads and moves. A `div` tracks a [`ScrollHandle`], a
/// uniform list a [`UniformListScrollHandle`]; both keep the same inner handle.
#[derive(Clone, Debug)]
pub enum ScrollAreaHandle {
    Item(ScrollHandle),
    List(UniformListScrollHandle),
}

impl ScrollAreaHandle {
    /// The handle that carries the offset in both cases.
    pub fn handle(&self) -> ScrollHandle {
        match self {
            Self::Item(handle) => handle.clone(),
            Self::List(handle) => handle.0.borrow().base_handle.clone(),
        }
    }
}

impl From<ScrollHandle> for ScrollAreaHandle {
    fn from(handle: ScrollHandle) -> Self {
        Self::Item(handle)
    }
}

impl From<&ScrollHandle> for ScrollAreaHandle {
    fn from(handle: &ScrollHandle) -> Self {
        Self::Item(handle.clone())
    }
}

impl From<UniformListScrollHandle> for ScrollAreaHandle {
    fn from(handle: UniformListScrollHandle) -> Self {
        Self::List(handle)
    }
}

impl From<&UniformListScrollHandle> for ScrollAreaHandle {
    fn from(handle: &UniformListScrollHandle) -> Self {
        Self::List(handle.clone())
    }
}

/// One thumb in the coordinates of its track.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thumb {
    pub start: f32,
    pub length: f32,
}

/// Where the thumb of a track sits and how long it is. `None` when the content fits
/// and there is nothing to scroll.
pub fn thumb(
    track: f32,
    viewport: f32,
    content: f32,
    offset: f32,
    min_length: f32,
) -> Option<Thumb> {
    if track <= 0. || viewport <= 0. || content <= viewport + 0.5 {
        return None;
    }
    let max_scroll = content - viewport;
    let length = (track * viewport / content).max(min_length).min(track);
    let travel = track - length;
    let start = (offset / max_scroll).clamp(0., 1.) * travel;
    Some(Thumb { start, length })
}

/// The scroll offset of a thumb whose top moved to `start`.
pub fn offset_for_thumb(track: f32, length: f32, start: f32, max_scroll: f32) -> f32 {
    let travel = (track - length).max(0.);
    if travel <= 0. {
        return 0.;
    }
    (start / travel).clamp(0., 1.) * max_scroll.max(0.)
}

/// The offset after a click on the track: one page towards the click.
pub fn page_offset(offset: f32, viewport: f32, max_scroll: f32, below: bool) -> f32 {
    let page = if below { viewport } else { -viewport };
    (offset + page).clamp(0., max_scroll.max(0.))
}

/// Moves the handle of `source` on one axis. Scroll offsets are positive here; the
/// handle keeps them negative, as GPUI does.
pub fn set_scroll(source: &ScrollAreaHandle, vertical: bool, offset: f32) {
    let handle = source.handle();
    let current = handle.offset();
    let offset = px(offset.max(0.));
    handle.set_offset(if vertical {
        point(current.x, -offset)
    } else {
        point(-offset, current.y)
    });
}

/// What a bar needs to move its thumb.
#[derive(Clone, Copy, Debug)]
struct Drag {
    vertical: bool,
    grip: f32,
}

struct ScrollBars {
    source: ScrollAreaHandle,
    theme: Theme,
    name: String,
    hovered: bool,
    dragging: Option<Drag>,
    last_activity: Instant,
    fade_from: Option<Instant>,
    generation: u64,
}

impl ScrollBars {
    fn new(source: ScrollAreaHandle, theme: Theme, name: String) -> Self {
        Self {
            source,
            theme,
            name,
            hovered: false,
            dragging: None,
            last_activity: Instant::now() - HIDE_DELAY - FADE,
            fade_from: None,
            generation: 0,
        }
    }

    fn active(&self) -> bool {
        self.hovered || self.dragging.is_some()
    }

    /// Zero when the bar is gone, one while it is needed, and a step down during the
    /// fade. The bar is drawn while this is above zero.
    fn opacity(&self, now: Instant) -> f32 {
        if self.active() || now.saturating_duration_since(self.last_activity) < HIDE_DELAY {
            return 1.;
        }
        match self.fade_from {
            Some(from) => {
                let gone = now.saturating_duration_since(from).as_secs_f32() / FADE.as_secs_f32();
                (1. - gone).clamp(0., 1.)
            }
            None => 0.,
        }
    }

    /// A scroll or a movement of the pointer: show the bars and restart the wait.
    fn touch(&mut self, cx: &mut Context<Self>) {
        self.last_activity = Instant::now();
        self.fade_from = None;
        self.generation += 1;
        self.schedule_hide(cx);
        cx.notify();
    }

    /// Hides the bars after `HIDE_DELAY` of rest, unless the pointer is on them or a
    /// thumb is being dragged. A newer touch cancels this wait by its generation.
    fn schedule_hide(&mut self, cx: &mut Context<Self>) {
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            cx.background_executor().timer(HIDE_DELAY).await;
            this.update(cx, |this, cx| {
                if this.generation != generation || this.active() {
                    return;
                }
                this.fade_from = Some(Instant::now());
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn hovered(&mut self, hovered: bool, cx: &mut Context<Self>) {
        if self.hovered == hovered {
            return;
        }
        self.hovered = hovered;
        if hovered {
            self.fade_from = None;
            self.last_activity = Instant::now();
            cx.notify();
        } else {
            self.touch(cx);
        }
    }

    /// The viewport and the scroll extent of one axis, and the current offset.
    fn axis(&self, vertical: bool) -> (f32, f32, f32) {
        let handle = self.source.handle();
        let bounds = handle.bounds();
        let max = handle.max_offset();
        let offset = handle.offset();
        if vertical {
            (
                f32::from(bounds.size.height),
                f32::from(max.y),
                f32::from(-offset.y),
            )
        } else {
            (
                f32::from(bounds.size.width),
                f32::from(max.x),
                f32::from(-offset.x),
            )
        }
    }

    /// The track in the coordinates of the strip, and the thumb on it.
    fn layout(&self, vertical: bool) -> Option<(f32, Thumb)> {
        let (viewport, max_scroll, offset) = self.axis(vertical);
        let track = viewport - SCROLLBAR_INSET * 2.;
        let content = viewport + max_scroll;
        thumb(track, viewport, content, offset, SCROLLBAR_MIN_LENGTH).map(|thumb| (track, thumb))
    }

    fn press(&mut self, vertical: bool, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some((_track, thumb)) = self.layout(vertical) else {
            return;
        };
        let (viewport, max_scroll, offset) = self.axis(vertical);
        let at = if vertical {
            f32::from(position.y) - self.track_origin(vertical)
        } else {
            f32::from(position.x) - self.track_origin(vertical)
        };
        if at >= thumb.start && at <= thumb.start + thumb.length {
            self.dragging = Some(Drag {
                vertical,
                grip: at - thumb.start,
            });
        } else {
            let offset = page_offset(offset, viewport, max_scroll, at > thumb.start);
            set_scroll(&self.source, vertical, offset);
        }
        self.touch(cx);
    }

    /// The window coordinate of the start of the track.
    fn track_origin(&self, vertical: bool) -> f32 {
        let bounds = self.source.handle().bounds();
        let origin = if vertical {
            bounds.top() + px(SCROLLBAR_INSET)
        } else {
            bounds.left() + px(SCROLLBAR_INSET)
        };
        f32::from(origin)
    }

    fn drag_to(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.dragging else {
            return;
        };
        let Some((track, thumb)) = self.layout(drag.vertical) else {
            return;
        };
        let (_, max_scroll, _) = self.axis(drag.vertical);
        let at = if drag.vertical {
            f32::from(position.y) - self.track_origin(drag.vertical)
        } else {
            f32::from(position.x) - self.track_origin(drag.vertical)
        };
        let start = (at - drag.grip).clamp(0., (track - thumb.length).max(0.));
        let offset = offset_for_thumb(track, thumb.length, start, max_scroll);
        set_scroll(&self.source, drag.vertical, offset);
        self.touch(cx);
    }

    fn release(&mut self, _: bool, cx: &mut Context<Self>) {
        if self.dragging.take().is_some() {
            self.touch(cx);
        }
    }

    fn vertical(
        &self,
        opacity: f32,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let (_, thumb) = self.layout(true)?;
        let name = self.name.as_str();
        let track_color = theme.scrollbar_track;
        let (thumb_color, hover_color) = (theme.scrollbar_thumb, theme.scrollbar_thumb_hover);
        let hovered = self.hovered || self.dragging.is_some_and(|drag| drag.vertical);
        let thumb_color = if hovered { hover_color } else { thumb_color };
        let strip_id = ElementId::Name(format!("scrollbar-v-{name}").into());
        let track_id = ElementId::Name(format!("scrollbar-v-track-{name}").into());
        let thumb_id = ElementId::Name(format!("scrollbar-v-thumb-{name}").into());
        let strip = div()
            .id(strip_id)
            .debug_selector(move || format!("scrollbar-v-{name}"))
            .absolute()
            .top(px(SCROLLBAR_INSET))
            .bottom(px(SCROLLBAR_INSET))
            .right_0()
            .w(px(SCROLLBAR_HIT))
            .occlude()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| this.hovered(*hovered, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.press(true, event.position, cx)
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.dragging() {
                    this.drag_to(event.position, cx)
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.release(true, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.release(true, cx)),
            )
            .when(opacity > 0., |this| {
                this.child(
                    div()
                        .id(track_id)
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px((SCROLLBAR_HIT - SCROLLBAR_HOVER_THICKNESS) / 2.))
                        .w(px(SCROLLBAR_HOVER_THICKNESS))
                        .rounded(px(SCROLLBAR_RADIUS))
                        .bg(track_color)
                        .opacity(opacity * 0.8),
                )
                .child(
                    div()
                        .id(thumb_id)
                        .debug_selector(move || format!("scrollbar-thumb-v-{name}"))
                        .absolute()
                        .top(px(thumb.start))
                        .right(px((SCROLLBAR_HIT - width) / 2.))
                        .w(px(width))
                        .h(px(thumb.length))
                        .rounded(px(SCROLLBAR_RADIUS))
                        .bg(thumb_color)
                        .opacity(opacity),
                )
            });
        Some(strip)
    }

    fn horizontal(
        &self,
        opacity: f32,
        width: f32,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        let (_, thumb) = self.layout(false)?;
        let name = self.name.as_str();
        let track_color = theme.scrollbar_track;
        let (thumb_color, hover_color) = (theme.scrollbar_thumb, theme.scrollbar_thumb_hover);
        let hovered = self.hovered || self.dragging.is_some_and(|drag| !drag.vertical);
        let thumb_color = if hovered { hover_color } else { thumb_color };
        let strip_id = ElementId::Name(format!("scrollbar-h-{name}").into());
        let track_id = ElementId::Name(format!("scrollbar-h-track-{name}").into());
        let thumb_id = ElementId::Name(format!("scrollbar-h-thumb-{name}").into());
        let strip = div()
            .id(strip_id)
            .debug_selector(move || format!("scrollbar-h-{name}"))
            .absolute()
            .left(px(SCROLLBAR_INSET))
            .right(px(SCROLLBAR_INSET))
            .bottom_0()
            .h(px(SCROLLBAR_HIT))
            .occlude()
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| this.hovered(*hovered, cx)))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, _, cx| {
                    this.press(false, event.position, cx)
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, _, cx| {
                if event.dragging() {
                    this.drag_to(event.position, cx)
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.release(false, cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.release(false, cx)),
            )
            .when(opacity > 0., |this| {
                this.child(
                    div()
                        .id(track_id)
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom(px((SCROLLBAR_HIT - SCROLLBAR_HOVER_THICKNESS) / 2.))
                        .h(px(SCROLLBAR_HOVER_THICKNESS))
                        .rounded(px(SCROLLBAR_RADIUS))
                        .bg(track_color)
                        .opacity(opacity * 0.8),
                )
                .child(
                    div()
                        .id(thumb_id)
                        .debug_selector(move || format!("scrollbar-thumb-h-{name}"))
                        .absolute()
                        .left(px(thumb.start))
                        .bottom(px((SCROLLBAR_HIT - width) / 2.))
                        .h(px(width))
                        .w(px(thumb.length))
                        .rounded(px(SCROLLBAR_RADIUS))
                        .bg(thumb_color)
                        .opacity(opacity),
                )
            });
        Some(strip)
    }
}

impl Render for ScrollBars {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = Instant::now();
        let opacity = self.opacity(now);
        if self.fade_from.is_some() {
            window.request_animation_frame();
        }
        let width = if self.active() {
            SCROLLBAR_HOVER_THICKNESS
        } else {
            SCROLLBAR_THICKNESS
        };
        let theme = self.theme;
        let vertical = self.vertical(opacity, width, &theme, cx);
        let horizontal = self.horizontal(opacity, width, &theme, cx);
        div()
            .absolute()
            .top_0()
            .left_0()
            .size_full()
            .children(vertical)
            .children(horizontal)
            .into_any_element()
    }
}

/// Wraps a scrolled element and draws its bars over it. `id` names the wrapper and
/// must be unique in the parent; `scroll` is the handle that the element tracks.
/// The caller keeps its layout, for example `.flex_1().min_h_0()` on the result.
pub fn scrollable(
    id: impl Into<ElementId>,
    scroll: impl Into<ScrollAreaHandle>,
    content: impl Styled + IntoElement,
    theme: &Theme,
    window: &mut Window,
    cx: &mut App,
) -> Stateful<Div> {
    let id = id.into();
    let source = scroll.into();
    let key = ElementId::NamedChild(Arc::new(id.clone()), "scrollbars".into());
    let name = id.to_string();
    let bars = window.use_keyed_state(key, cx, {
        let source = source.clone();
        let theme = *theme;
        let name = name.clone();
        move |_, _| ScrollBars::new(source, theme, name)
    });
    let scroll = bars.clone();
    div()
        .id(id)
        .debug_selector({
            let name = name.clone();
            move || format!("scroll-area-{name}")
        })
        .relative()
        .on_scroll(move |_, _, cx| scroll.update(cx, |bars, cx| bars.touch(cx)))
        .child(content.size_full().min_h_0().min_w_0())
        .child(bars)
}

#[cfg(test)]
mod tests {
    use gpui_kit::{ScrollHandle, px};

    use super::{ScrollAreaHandle, offset_for_thumb, page_offset, set_scroll, thumb};

    #[test]
    fn the_thumb_is_a_fraction_of_the_track_with_a_minimum() {
        let bar = thumb(100., 50., 200., 0., 20.).unwrap();
        assert_eq!(bar.length, 25., "a quarter of the content is seen");
        assert_eq!(bar.start, 0.);

        let bar = thumb(100., 90., 100., 0., 24.).unwrap();
        assert_eq!(bar.length, 90.);
        assert_eq!(bar.start, 0.);

        let bar = thumb(100., 99., 100., 0., 24.).unwrap();
        assert_eq!(bar.length, 99.);
    }

    #[test]
    fn a_thumb_stops_at_the_end_of_its_track() {
        let bar = thumb(100., 25., 100., 75., 10.).unwrap();
        assert_eq!(bar.length, 25.);
        assert_eq!(bar.start, 75.);
    }

    #[test]
    fn content_that_fits_has_no_thumb() {
        assert!(thumb(100., 100., 100., 0., 24.).is_none());
        assert!(thumb(100., 120., 100., 0., 24.).is_none());
        assert!(thumb(0., 50., 200., 0., 24.).is_none());
    }

    #[test]
    fn a_thumb_maps_back_to_the_scroll_offset() {
        assert_eq!(offset_for_thumb(100., 25., 0., 300.), 0.);
        assert_eq!(offset_for_thumb(100., 25., 75., 300.), 300.);
        assert_eq!(offset_for_thumb(100., 25., 37.5, 300.), 150.);
    }

    #[test]
    fn a_track_click_moves_by_a_page_and_stops_at_the_ends() {
        assert_eq!(page_offset(0., 200., 1000., true), 200.);
        assert_eq!(page_offset(950., 200., 1000., true), 1000.);
        assert_eq!(page_offset(100., 200., 1000., false), 0.);
    }

    #[test]
    fn a_drag_and_a_track_click_move_a_scroll_handle() {
        let handle = ScrollHandle::new();
        let source = ScrollAreaHandle::from(&handle);
        let dragged = offset_for_thumb(100., 25., 37.5, 300.);
        set_scroll(&source, true, dragged);
        assert_eq!(handle.offset().y, px(-150.), "the thumb at half the track");

        let clicked = page_offset(150., 200., 300., true);
        set_scroll(&source, true, clicked);
        assert_eq!(handle.offset().y, px(-300.), "a click below the thumb");

        set_scroll(&source, false, 12.);
        assert_eq!(handle.offset().x, px(-12.));
        set_scroll(&source, true, -5.);
        assert_eq!(handle.offset().y, px(0.), "a positive offset is a clamp");
    }
}

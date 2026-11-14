//! Buttons, icons, islands, tooltips and the frame of tool windows.

use std::sync::Mutex;
use std::time::{Duration, Instant};

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::Theme;
use crate::metrics::*;

/// A themed icon from `assets/icons`, picking the `_dark` variant when it exists.
pub fn icon(name: &str, theme: &Theme) -> Img {
    icon_sized(name, ICON, theme)
}

pub fn icon_sized(name: &str, size: f32, theme: &Theme) -> Img {
    let suffix = if theme.dark && name != "fileTypes/gitignore" {
        "_dark"
    } else {
        ""
    };
    let path: SharedString = format!("icons/{name}{suffix}.svg").into();
    img(path).size(px(size)).flex_none()
}

/// A checkbox: an accent square with a check mark when `on`.
pub fn checkbox(on: bool, theme: &Theme) -> Div {
    let (background, border) = if on {
        (theme.accent, theme.accent)
    } else {
        (theme.popup_bg, theme.border)
    };
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .size(px(14.))
        .rounded(px(3.))
        .border_1()
        .border_color(border)
        .bg(background)
        .children(on.then(|| icon_sized("general/check", 10., theme)))
}

/// Toolbar-style square button with hover and pressed states, its name and its key.
pub fn icon_button(
    id: impl Into<ElementId>,
    name: &str,
    label: impl Into<SharedString>,
    shortcut: Option<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    button_frame(id, label, shortcut, theme).child(icon(name, theme))
}

/// The same button with the larger icons of the main toolbar.
pub fn toolbar_button(
    id: impl Into<ElementId>,
    name: &str,
    label: impl Into<SharedString>,
    shortcut: Option<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    button_frame(id, label, shortcut, theme).child(icon_sized(name, ICON, theme))
}

/// The frame under an icon button. The name is required: a button without a tooltip is
/// not a button the user can know.
pub fn button_frame(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    shortcut: Option<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    let label = label.into();
    let id = id.into();
    #[cfg(any(test, feature = "test-support"))]
    let name = id.to_string();
    let button = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(ICON_BUTTON))
        .rounded(px(CONTROL_RADIUS))
        .occlude()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .tooltip(move |_, cx| tooltip(label.clone(), shortcut.clone(), cx))
        .tooltip_show_delay(TOOLTIP_CREATE_DELAY);
    #[cfg(any(test, feature = "test-support"))]
    let button = button.child(crate::bounds_probe(name, None));
    button
}

/// Text button with an optional leading icon and a trailing chevron.
pub fn toolbar_dropdown(
    id: impl Into<ElementId>,
    leading: Option<AnyElement>,
    label: impl Into<SharedString>,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    let label = label.into();
    let id = id.into();
    #[cfg(any(test, feature = "test-support"))]
    let name = id.to_string();
    let dropdown = div()
        .id(id)
        .flex()
        .items_center()
        .gap(px(SPACE_XS))
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .text_size(px(UI_SIZE))
        .occlude()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .children(leading)
        .child(label.clone())
        .child(icon_sized("general/chevronDown", TEXT_SMALL, theme));
    #[cfg(any(test, feature = "test-support"))]
    let dropdown = dropdown.child(crate::bounds_probe(name, Some(label)));
    dropdown
}

/// Minimize, maximize and close: the areas Windows answers for itself.
pub fn window_button(
    id: &'static str,
    name: &str,
    label: impl Into<SharedString>,
    area: WindowControlArea,
    danger: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let path: SharedString =
        format!("icons/{name}{}.svg", if theme.dark { "_dark" } else { "" }).into();
    let label = label.into();
    let hover = if danger {
        theme.close_hover
    } else {
        theme.hover
    };
    let pressed = if danger {
        theme.close_hover.opacity(0.8)
    } else {
        theme.pressed
    };
    let over = theme.over_accent;
    let button = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .group(id)
        .flex()
        .items_center()
        .justify_center()
        .w(px(WINDOW_BUTTON_WIDTH))
        .h(px(TOOLBAR_HEIGHT))
        .window_control_area(area)
        .occlude()
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .tooltip(move |_, cx| tooltip(label.clone(), None, cx))
        .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
        .child(
            svg()
                .path(path)
                .size(px(ICON))
                .text_color(theme.text)
                .when(danger, |this| {
                    this.group_hover(id, move |style| style.text_color(over))
                }),
        );
    #[cfg(any(test, feature = "test-support"))]
    let button = button.child(crate::bounds_probe(id, None));
    button
}

/// A button of the tool window stripes.
pub fn stripe_button(
    id: impl Into<ElementId>,
    icon_name: &str,
    label: SharedString,
    shortcut: Option<SharedString>,
    selected: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    let id = id.into();
    #[cfg(any(test, feature = "test-support"))]
    let name = id.to_string();
    let button = div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .size(px(ICON_BUTTON))
        .rounded(px(CONTROL_RADIUS))
        .occlude()
        .when(selected, |this| this.bg(pressed))
        .when(!selected, |this| this.hover(move |style| style.bg(hover)))
        .active(move |style| style.bg(pressed))
        .tooltip(move |_, cx| tooltip(label.clone(), shortcut.clone(), cx))
        .tooltip_show_delay(TOOLTIP_CREATE_DELAY)
        .child(icon_sized(icon_name, ICON, theme));
    #[cfg(any(test, feature = "test-support"))]
    let button = button.child(crate::bounds_probe(name, None));
    button
}

/// Rounded panel: the unit of the Islands layout.
pub fn island(theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .overflow_hidden()
        .rounded(px(ISLAND_RADIUS))
        .border_1()
        .border_color(theme.border)
        .bg(theme.island_bg)
}

/// Buttons a tool window shows only under the pointer. `on_hide` hides the panel.
/// Hide has the key of the whole tool window, as the window binds it.
pub fn panel_tools(
    group: &'static str,
    on_hide: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    theme: &Theme,
) -> Div {
    div()
        .flex()
        .items_center()
        .invisible()
        .group_hover(group, |style| style.visible())
        .child(
            icon_button(
                SharedString::from(format!("{group}-hide")),
                "general/hide",
                "Hide",
                Some("Shift+Escape".into()),
                theme,
            )
            .on_click(on_hide),
        )
}

pub fn panel_header(
    title: impl Into<SharedString>,
    group: &'static str,
    on_hide: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    theme: &Theme,
) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .flex_none()
        .h(px(CONTROL_HEIGHT))
        .pl(px(SPACE_M))
        .pr(px(SPACE_S))
        .text_size(px(UI_SIZE))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_XS))
                .font_weight(FontWeight::MEDIUM)
                .child(title.into()),
        )
        .child(panel_tools(group, on_hide, theme))
}

pub fn popup_shadow(theme: &Theme) -> Vec<BoxShadow> {
    let alpha = if theme.dark { 0.45 } else { 0.18 };
    vec![BoxShadow {
        color: hsla(0., 0., 0., alpha),
        offset: point(px(0.), px(8.)),
        blur_radius: px(24.),
        spread_radius: px(0.),
        inset: false,
    }]
}

/// The one frame of a floating surface: the popup background, a hairline border,
/// the popup radius and the themed shadow.
pub fn popup_frame(theme: &Theme) -> Div {
    div()
        .bg(theme.popup_bg)
        .border_1()
        .border_color(theme.border)
        .rounded(px(8.))
        .shadow(popup_shadow(theme))
}

/// How long a tooltip waits before it shows over a button.
pub const TOOLTIP_DELAY: Duration = Duration::from_millis(500);
/// After a tooltip closes, the next one on a neighbouring button shows at once
/// for this long.
pub const TOOLTIP_WARM: Duration = Duration::from_millis(1000);
/// The gap between a tooltip and the control under it.
pub const TOOLTIP_GAP: f32 = SPACE_S;

/// The rule of tooltip delays: half a second from rest, none while the pointer
/// walks the buttons that just showed a tooltip.
#[derive(Clone, Copy, Debug, Default)]
pub struct TooltipTiming {
    shown: Option<Instant>,
    hidden: Option<Instant>,
}

impl TooltipTiming {
    pub const fn new() -> Self {
        Self {
            shown: None,
            hidden: None,
        }
    }

    fn recent(at: Option<Instant>, now: Instant) -> bool {
        at.is_some_and(|at| now.saturating_duration_since(at) <= TOOLTIP_WARM)
    }

    /// The delay of the tooltip that starts hovering at `now`.
    pub fn delay(&self, now: Instant) -> Duration {
        if Self::recent(self.shown, now) || Self::recent(self.hidden, now) {
            Duration::ZERO
        } else {
            TOOLTIP_DELAY
        }
    }

    /// A tooltip became visible at `now`.
    pub fn shown(&mut self, now: Instant) {
        self.shown = Some(now);
    }

    /// A tooltip closed at `now`.
    pub fn hidden(&mut self, now: Instant) {
        self.hidden = Some(now);
        self.shown = None;
    }
}

static TOOLTIP_TIMING: Mutex<TooltipTiming> = Mutex::new(TooltipTiming::new());

/// The delay GPUI waits before it asks for a tooltip. It is zero: the entity of the
/// tooltip is created when the pointer enters the element, and that is where the real
/// wait is decided. A view drawn once keeps its frame, so a delay read at draw time would
/// be the delay of the previous hover.
pub const TOOLTIP_CREATE_DELAY: Duration = Duration::ZERO;

/// The delay of the next tooltip, as GPUI needs it: none. Kept for call sites that still
/// pass a delay explicitly; the wait itself is in [`Tooltip`].
pub const fn tooltip_show_delay() -> Duration {
    TOOLTIP_CREATE_DELAY
}

/// The delay of a tooltip whose element is entered now.
fn tooltip_wait() -> Duration {
    let now = Instant::now();
    TOOLTIP_TIMING
        .lock()
        .map_or(TOOLTIP_DELAY, |timing| timing.delay(now))
}

/// Where a tooltip of `size` stands for a control at `anchor`: under the control
/// with a small gap, above it when the bottom of the window is too close, and
/// always inside the window.
pub fn tooltip_position(
    anchor: Bounds<Pixels>,
    size: Size<Pixels>,
    viewport: Size<Pixels>,
) -> Point<Pixels> {
    let margin = px(SPACE_M);
    let max_x = (viewport.width - size.width - margin).max(margin);
    let x = anchor.left().clamp(margin, max_x);
    let below = anchor.bottom() + px(TOOLTIP_GAP);
    let y = if below + size.height + margin <= viewport.height {
        below
    } else {
        (anchor.top() - px(TOOLTIP_GAP) - size.height).max(margin)
    };
    point(x, y)
}

/// A tooltip with an optional shortcut. It is created when the pointer enters its
/// element and shows itself after the delay of that moment, so a cached frame cannot
/// keep the delay of a past render.
pub struct Tooltip {
    text: SharedString,
    shortcut: Option<SharedString>,
    ready: bool,
    shown_at: Option<Instant>,
}

impl Drop for Tooltip {
    fn drop(&mut self) {
        // A tooltip that never became visible does not open the warm window.
        if self.shown_at.is_none() {
            return;
        }
        if let Ok(mut timing) = TOOLTIP_TIMING.lock() {
            timing.hidden(Instant::now());
        }
    }
}

pub fn tooltip(
    text: impl Into<SharedString>,
    shortcut: Option<SharedString>,
    cx: &mut App,
) -> AnyView {
    let text = text.into();
    let wait = tooltip_wait();
    cx.new(move |cx| {
        if !wait.is_zero() {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                this.update(cx, |tooltip: &mut Tooltip, cx| {
                    tooltip.ready = true;
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        Tooltip {
            text,
            shortcut,
            ready: wait.is_zero(),
            shown_at: None,
        }
    })
    .into()
}

// The tooltips a test build rendered, oldest first.
#[cfg(any(test, feature = "test-support"))]
thread_local! {
    static SHOWN_TOOLTIPS: std::cell::RefCell<Vec<(SharedString, Option<SharedString>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The name and the key of every tooltip rendered since the last clear, for tests.
#[cfg(any(test, feature = "test-support"))]
pub fn shown_tooltips() -> Vec<(String, Option<String>)> {
    SHOWN_TOOLTIPS.with(|list| {
        list.borrow()
            .iter()
            .map(|(text, shortcut)| (text.to_string(), shortcut.as_ref().map(ToString::to_string)))
            .collect()
    })
}

/// Forgets the tooltips a test build rendered.
#[cfg(any(test, feature = "test-support"))]
pub fn clear_shown_tooltips() {
    SHOWN_TOOLTIPS.with(|list| list.borrow_mut().clear());
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.ready && self.shown_at.is_none() {
            let now = Instant::now();
            self.shown_at = Some(now);
            if let Ok(mut timing) = TOOLTIP_TIMING.lock() {
                timing.shown(now);
            }
        }
        #[cfg(any(test, feature = "test-support"))]
        if self.ready {
            SHOWN_TOOLTIPS.with(|list| {
                let mut list = list.borrow_mut();
                let shown = (self.text.clone(), self.shortcut.clone());
                if list.last() != Some(&shown) {
                    list.push(shown);
                }
            });
        }
        let theme = *cx.global::<Theme>();
        // The gap is on both sides, so the content keeps it whether GPUI puts the
        // tooltip under the pointer or above it near the bottom edge. While the wait
        // runs the frame stays empty, which keeps the tooltip where the pointer is.
        div()
            .when(self.ready, |this| {
                this.pt(px(TOOLTIP_GAP))
                    .pb(px(TOOLTIP_GAP))
                    .pl(px(SPACE_S))
                    .font_family(UI_FONT)
            })
            .children(self.ready.then(|| {
                popup_frame(&theme)
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .px(px(SPACE_M))
                    .py(px(SPACE_S))
                    .text_size(px(UI_SIZE))
                    .text_color(theme.text)
                    .child(self.text.clone())
                    .children(
                        self.shortcut
                            .clone()
                            .map(|shortcut| div().text_color(theme.text_muted).child(shortcut)),
                    )
            }))
    }
}

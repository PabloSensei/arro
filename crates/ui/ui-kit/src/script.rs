//! Where the elements that a `--script` step names were drawn.
//!
//! GPUI records `debug_selector` keys only in a debug build with the test platform, and
//! a release build of the IDE has neither. A scripted run keeps its own table instead:
//! an element that a script names puts a spot in its middle, and the table holds the
//! bounds of the last frame the spot was drawn in. `--script` presses the middle of a
//! spot the way the mouse does and checks that a spot was drawn on the next frame.
//!
//! The table exists only while a script runs: `ScriptSpots` is a global that `--script`
//! installs before its first step, and a window no script drives passes `false` to the
//! spot elements, which then cost nothing at all.

use std::collections::HashMap;

use gpui_kit::prelude::*;
use gpui_kit::*;

/// The spots named by `--script`, as the window drew them.
///
/// A spot of a view the window caches, such as a stripe, is kept between frames: the
/// view draws again only when its contents change, while the element stays on screen.
/// Such a spot belongs to its view, and the view drops all of its spots when it draws:
/// an element it no longer has leaves the table even when no other spot replaced it.
/// Every other spot lives for one frame, so a popup that closed is gone from the table
/// on the next frame.
#[derive(Default)]
pub struct ScriptSpots {
    frame: u64,
    shown: HashMap<String, Bounds<Pixels>>,
    kept: HashMap<String, KeptSpot>,
}

struct KeptSpot {
    view: &'static str,
    bounds: Bounds<Pixels>,
}

impl Global for ScriptSpots {}

impl ScriptSpots {
    /// Starts the table of the frame the window is drawing now.
    pub fn begin_frame(&mut self) {
        self.frame = self.frame.wrapping_add(1);
        self.shown.clear();
    }

    /// Starts the draw of a view the window caches: its spots are taken away, and the
    /// ones it still shows are recorded again while it draws.
    pub fn begin_view(&mut self, view: &'static str) {
        self.kept.retain(|_, spot| spot.view != view);
    }

    /// The number of the frame drawn last, which a step waits to pass.
    pub fn frame(&self) -> u64 {
        self.frame
    }

    /// Records a spot of the frame being drawn.
    pub fn show(&mut self, name: String, bounds: Bounds<Pixels>) {
        self.shown.insert(name, bounds);
    }

    /// Records a spot of a view the window caches, which no later frame clears.
    pub fn keep(&mut self, view: &'static str, name: String, bounds: Bounds<Pixels>) {
        self.kept.insert(name, KeptSpot { view, bounds });
    }

    /// Where the spot was drawn on the last frame, if it was on screen.
    pub fn find(&self, name: &str) -> Option<Bounds<Pixels>> {
        self.shown
            .get(name)
            .or_else(|| self.kept.get(name).map(|spot| &spot.bounds))
            .copied()
    }

    /// Whether the spot was on screen on the last frame.
    pub fn holds(&self, name: &str) -> bool {
        self.shown.contains_key(name) || self.kept.contains_key(name)
    }
}

/// A spot in the middle of the element it is a child of, named for `--script`. Without
/// a script the element is empty and records nothing.
pub fn script_spot(enabled: bool, name: impl Into<String>) -> AnyElement {
    if !enabled {
        return Empty.into_any_element();
    }
    let name = name.into();
    canvas(
        move |bounds, _, cx| {
            cx.default_global::<ScriptSpots>().show(name, bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

/// A spot for an element of the view `view`, which the window caches: it stays in the
/// table after the view stops drawing, until the view draws again and either moves it
/// or no longer has it.
pub fn kept_spot(enabled: bool, view: &'static str, name: impl Into<String>) -> AnyElement {
    if !enabled {
        return Empty.into_any_element();
    }
    let name = name.into();
    canvas(
        move |bounds, _, cx| {
            cx.default_global::<ScriptSpots>().keep(view, name, bounds);
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::ScriptSpots;
    use crate::gpui::{Bounds, point, px};

    fn bounds(left: f32) -> Bounds<crate::gpui::Pixels> {
        Bounds::new(point(px(left), px(0.)), crate::gpui::size(px(10.), px(10.)))
    }

    #[test]
    fn a_spot_of_the_frame_goes_with_the_next_frame() {
        let mut spots = ScriptSpots::default();
        spots.show("places-empty".into(), bounds(0.));
        assert!(spots.holds("places-empty"));
        spots.begin_frame();
        assert!(!spots.holds("places-empty"));
    }

    #[test]
    fn a_kept_spot_stays_until_its_view_draws_again() {
        let mut spots = ScriptSpots::default();
        spots.keep("right-stripe", "stripe-s-ai".into(), bounds(0.));
        spots.keep("left-stripe", "stripe-s-log".into(), bounds(0.));
        spots.begin_frame();
        assert!(spots.holds("stripe-s-ai"));

        // A view that draws without the element takes its spot away, while the spots of
        // the views that did not draw stay.
        spots.begin_view("right-stripe");
        assert!(!spots.holds("stripe-s-ai"));
        assert!(spots.holds("stripe-s-log"));
        assert_eq!(spots.find("stripe-s-log"), Some(bounds(0.)));
    }
}

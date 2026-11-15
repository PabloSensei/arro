//! The viewport of a diagram: the zoom, the pan and the visible part of the drawing,
//! shared by the diagram of types and the diagram of a sequence. The drawing itself is
//! laid out in diagram units; this module maps them onto the window.

use ui_kit::*;

/// The smallest and the largest zoom of a diagram.
pub const MIN_ZOOM: f32 = 0.25;
pub const MAX_ZOOM: f32 = 2.5;
/// One press of a zoom button or key.
pub const ZOOM_STEP: f32 = 1.2;
/// The space Fit leaves around the diagram.
pub const FIT_MARGIN: f32 = 24.;

/// Where the pointer is in diagram units.
pub fn to_diagram(
    pan: Point<f32>,
    bounds: Option<Bounds<Pixels>>,
    position: Point<Pixels>,
) -> Point<f32> {
    let origin = bounds.map(|bounds| bounds.origin).unwrap_or_default();
    point(
        f32::from(position.x - origin.x) - pan.x,
        f32::from(position.y - origin.y) - pan.y,
    )
}

/// Zooms around a point of the window, keeping the diagram under the pointer in place.
pub fn zoom_at(
    zoom: &mut f32,
    pan: &mut Point<f32>,
    factor: f32,
    around: Point<Pixels>,
    bounds: Option<Bounds<Pixels>>,
) {
    let origin = bounds.map(|bounds| bounds.origin).unwrap_or_default();
    let local = point(
        f32::from(around.x - origin.x),
        f32::from(around.y - origin.y),
    );
    let before = point((local.x - pan.x) / *zoom, (local.y - pan.y) / *zoom);
    *zoom = (*zoom * factor).clamp(MIN_ZOOM, MAX_ZOOM);
    let after = point(before.x * *zoom, before.y * *zoom);
    *pan = point(local.x - after.x, local.y - after.y);
}

/// Fits a box of diagram units into the window, centring it. False when the window is
/// unknown or the box is empty, and the zoom and the pan stay as they were.
pub fn fit(
    zoom: &mut f32,
    pan: &mut Point<f32>,
    bounds: Option<Bounds<Pixels>>,
    content: (f32, f32, f32, f32),
) -> bool {
    let Some(bounds) = bounds else {
        return false;
    };
    let (left, top, right, bottom) = content;
    if right <= left || bottom <= top {
        return false;
    }
    let width = f32::from(bounds.size.width).max(1.);
    let height = f32::from(bounds.size.height).max(1.);
    let fit_x = (width - FIT_MARGIN * 2.) / (right - left);
    let fit_y = (height - FIT_MARGIN * 2.) / (bottom - top);
    *zoom = fit_x.min(fit_y).clamp(MIN_ZOOM, MAX_ZOOM);
    *pan = point(
        (width - (right - left) * *zoom) / 2. - left * *zoom,
        (height - (bottom - top) * *zoom) / 2. - top * *zoom,
    );
    true
}

/// Where a point of the diagram lands in the window.
pub fn screen(at: Point<f32>, zoom: f32, pan: Point<f32>) -> Point<f32> {
    point(at.x * zoom + pan.x, at.y * zoom + pan.y)
}

/// Whether a box of the diagram, already in window units, touches the visible part.
pub fn visible(
    left: f32,
    top: f32,
    width: f32,
    height: f32,
    bounds: Option<Bounds<Pixels>>,
) -> bool {
    let Some(bounds) = bounds else {
        return true;
    };
    let right = left + width;
    let bottom = top + height;
    right > 0.
        && bottom > 0.
        && left < f32::from(bounds.size.width)
        && top < f32::from(bounds.size.height)
}

/// Paints a dashed line of one segment of a drawing.
pub fn paint_dashed(from: Point<Pixels>, to: Point<Pixels>, color: Hsla, window: &mut Window) {
    const DASH: f32 = 6.;
    const GAP: f32 = 4.;
    let dx = f32::from(to.x - from.x);
    let dy = f32::from(to.y - from.y);
    let length = (dx * dx + dy * dy).sqrt();
    if length < 0.5 {
        return;
    }
    let mut at = 0.;
    while at < length {
        let finish = (at + DASH).min(length);
        let along = |distance: f32| {
            let fraction = distance / length;
            point(from.x + px(dx * fraction), from.y + px(dy * fraction))
        };
        let mut path = PathBuilder::stroke(px(1.2));
        path.move_to(along(at));
        path.line_to(along(finish));
        if let Ok(path) = path.build() {
            window.paint_path(path, color);
        }
        at += DASH + GAP;
    }
}

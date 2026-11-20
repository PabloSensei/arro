//! The mouse and the menu of a text field. A click puts the caret at the nearest character,
//! a drag selects, a double click selects a word, a triple click selects all, and a right
//! click opens a menu with Cut, Copy, Paste and Select All.
//!
//! The owner of a field connects it once, with [`FieldMouse::new`], and passes the handle to
//! the renderers of [`crate::TextField`]. The renderers place the elements that measure each
//! line of the text and the element that listens to the pointer; the listeners change the
//! field of the owner through [`FieldMouse::apply`], which redraws the owner on a change.

use std::cell::RefCell;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::prelude::*;
use gpui_kit::*;

use crate::Theme;
use crate::metrics::*;
use crate::popups::surface;
use crate::text_field::{FieldMenu, TextField};

/// The change of a field. It returns whether the field changed, and then the owner redraws.
pub type FieldChange<'a> = &'a mut dyn FnMut(&mut TextField, &mut App) -> bool;

/// How a text field reaches its owner: the owner, the way to its field, and the theme of
/// the menu. `off()` leaves a field without a mouse and without a menu.
#[derive(Clone, Default)]
pub struct FieldMouse {
    link: Option<Link>,
    /// The owner opens its own menu on a right click, as a console does.
    no_menu: bool,
}

/// Runs a change of the field. The flag is set for a change of the text, and then the
/// owner's own after-edit work runs too, as its key handler does.
type FieldEdit = dyn Fn(&mut App, bool, FieldChange<'_>);

/// Runs the owner's start of editing, before a left press of the field is heard.
type FieldStart = dyn Fn(&mut App);

#[derive(Clone)]
struct Link {
    theme: Theme,
    edit: Rc<FieldEdit>,
    start: Option<Rc<FieldStart>>,
    /// A read-only field is selected and copied, and its edits do nothing.
    read_only: bool,
}

impl FieldMouse {
    /// The mouse of the field that `field` finds in the owner that `cx` belongs to. The
    /// function returns `None` while the owner has no such field, and then the mouse does
    /// nothing. The function is called at each event, so it reaches the stored model.
    pub fn new<V: 'static>(
        cx: &Context<V>,
        field: impl for<'a> Fn(&'a mut V) -> Option<&'a mut TextField> + 'static,
    ) -> Self {
        Self::with_edit(cx, field, |_, _| {})
    }

    /// [`FieldMouse::new`] for an owner that does more after a change of the text than
    /// redraw: `after` runs after a Cut or a Paste from the menu, as the owner's key handler
    /// runs its own work after a key.
    pub fn with_edit<V: 'static>(
        cx: &Context<V>,
        field: impl for<'a> Fn(&'a mut V) -> Option<&'a mut TextField> + 'static,
        after: impl Fn(&mut V, &mut Context<V>) + 'static,
    ) -> Self {
        let owner = cx.entity();
        let edit = Rc::new(
            move |cx: &mut App, text_edit: bool, change: FieldChange<'_>| {
                owner.update(cx, |this, cx| {
                    let changed = match field(this) {
                        Some(text) => change(text, cx),
                        None => false,
                    };
                    if changed {
                        if text_edit {
                            after(this, cx);
                        }
                        cx.notify();
                    }
                });
            },
        );
        Self {
            no_menu: false,
            link: Some(Link {
                theme: *cx.global::<Theme>(),
                edit,
                start: None,
                read_only: false,
            }),
        }
    }

    /// The field can be selected and copied, but not changed: its mouse moves the caret and
    /// the selection, its menu has Copy and Select All only, and its edits do nothing.
    pub fn read_only(mut self) -> Self {
        if let Some(link) = &mut self.link {
            link.read_only = true;
        }
        self
    }

    /// Whether the field is read-only.
    pub fn is_read_only(&self) -> bool {
        self.link.as_ref().is_some_and(|link| link.read_only)
    }

    /// Runs `start` on the owner before a left press of the field is heard, so that an owner
    /// which starts editing on a press gives the caret of that same press the place of the
    /// pointer. `start` returns whether the owner changed, and then the owner is redrawn.
    pub fn starting<V: 'static>(
        mut self,
        cx: &Context<V>,
        start: impl Fn(&mut V, &mut Context<V>) -> bool + 'static,
    ) -> Self {
        if let Some(link) = &mut self.link {
            let owner = cx.entity();
            link.start = Some(Rc::new(move |cx: &mut App| {
                owner.update(cx, |this, cx| {
                    if start(this, cx) {
                        cx.notify();
                    }
                });
            }));
        }
        self
    }

    /// The same mouse without the field menu: a right click goes on to the owner, whose
    /// own menu (a console's Send EOF, Clear…) opens instead.
    pub fn without_menu(mut self) -> Self {
        self.no_menu = true;
        self
    }

    /// A field whose owner is not connected yet: its mouse and its menu do nothing.
    pub fn off() -> Self {
        Self {
            link: None,
            no_menu: false,
        }
    }

    /// Whether the field listens to the pointer.
    pub fn is_on(&self) -> bool {
        self.link.is_some()
    }

    /// Runs `change` on the field of the owner. The owner is redrawn when `change` returns
    /// true. The change moves the caret or the selection and edits no text.
    pub fn apply(&self, cx: &mut App, change: FieldChange<'_>) {
        if let Some(link) = &self.link {
            (link.edit)(cx, false, change);
        }
    }

    /// [`FieldMouse::apply`] for a change that edits the text, so the owner's after-edit
    /// work runs too. A read-only field takes no edit.
    pub fn apply_edit(&self, cx: &mut App, change: FieldChange<'_>) {
        if let Some(link) = self.link.as_ref().filter(|link| !link.read_only) {
            (link.edit)(cx, true, change);
        }
    }

    fn theme(&self) -> Option<Theme> {
        self.link.as_ref().map(|link| link.theme)
    }

    /// Runs the owner's start of editing, if the field has one.
    fn begin(&self, cx: &mut App) {
        if let Some(start) = self.link.as_ref().and_then(|link| link.start.as_ref()) {
            start(cx);
        }
    }
}

/// One line of a field as the last frame placed it: its byte range, the place of the line
/// on the screen, and its shaped text.
#[derive(Clone)]
pub(crate) struct Line {
    range: Range<usize>,
    bounds: Bounds<Pixels>,
    shaped: ShapedLine,
}

/// The lines of a field, filled by the measuring elements while the frame is prepainted.
pub(crate) type Lines = Rc<RefCell<Vec<Option<Line>>>>;

/// A store for `count` lines, not measured yet.
pub(crate) fn lines(count: usize) -> Lines {
    Rc::new(RefCell::new(vec![None; count]))
}

/// The element that measures line `index` of a field. It records the place of the line and
/// shapes its text with the text style of the line, so that the pointer maps to characters.
pub(crate) fn measure_line(
    lines: &Lines,
    index: usize,
    range: Range<usize>,
    text: &str,
) -> AnyElement {
    let lines = lines.clone();
    // A line holds no line break; a break in a single-line field is shown as a space.
    let text = text.replace('\n', " ");
    canvas(
        move |bounds, window, _| {
            let style = window.text_style();
            let size = style.font_size.to_pixels(window.rem_size());
            let shaped = if text.is_empty() {
                ShapedLine::default()
            } else {
                let run = style.to_run(text.len());
                window
                    .text_system()
                    .shape_line(text.into(), size, std::slice::from_ref(&run), None)
            };
            if let Some(slot) = lines.borrow_mut().get_mut(index) {
                *slot = Some(Line {
                    range,
                    bounds,
                    shaped,
                });
            }
        },
        |_, (), _, _| {},
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// The element that covers the whole field. It takes the pointer for the field, and it
/// sets the I-beam cursor. The listeners are registered when it is painted, after every
/// line has been measured.
pub(crate) fn hit_area(mouse: &FieldMouse, lines: Lines) -> AnyElement {
    let mouse = mouse.clone();
    canvas(
        |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
        move |_, hitbox, window, _| {
            window.set_cursor_style(CursorStyle::IBeam, &hitbox);
            let lines: Rc<Vec<Line>> = Rc::new(lines.borrow().iter().flatten().cloned().collect());
            listen(&mouse, lines, hitbox, window);
        },
    )
    .absolute()
    .inset_0()
    .into_any_element()
}

/// Registers the listeners of a field. A press of the left button is heard in the capture
/// phase, so the caret moves before the owner focuses the field, and the owner's handler
/// still runs. A right click stops the bubble, so an element around the field opens no menu
/// of its own. Moves and releases are heard everywhere, so a drag that leaves the field
/// still selects up to its edge.
fn listen(mouse: &FieldMouse, lines: Rc<Vec<Line>>, hitbox: Hitbox, window: &mut Window) {
    let (press, press_lines, press_hit) = (mouse.clone(), lines.clone(), hitbox.clone());
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Capture
            || event.button != MouseButton::Left
            || !press_hit.is_hovered(window)
        {
            return;
        }
        press.begin(cx);
        let at = index_at(&press_lines, event.position);
        let (shift, clicks) = (event.modifiers.shift, event.click_count);
        press.apply(cx, &mut |field, _| {
            field.menu = None;
            field.press(at, shift, clicks);
            true
        });
    });

    let (drag, drag_lines) = (mouse.clone(), lines.clone());
    window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
        if phase != DispatchPhase::Capture {
            return;
        }
        let at = index_at(&drag_lines, event.position);
        let held = event.pressed_button == Some(MouseButton::Left);
        drag.apply(cx, &mut |field, _| {
            if held {
                field.drag_to(at)
            } else {
                field.release()
            }
        });
    });

    let release = mouse.clone();
    window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
            return;
        }
        release.apply(cx, &mut |field, _| field.release());
    });

    let (menu, menu_lines, menu_hit) = (mouse.clone(), lines, hitbox);
    window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
        if phase != DispatchPhase::Bubble
            || event.button != MouseButton::Right
            || menu.no_menu
            || !menu_hit.is_hovered(window)
        {
            return;
        }
        let at = index_at(&menu_lines, event.position);
        let position = event.position;
        menu.apply(cx, &mut |field, app| {
            // A click inside the selection keeps it, as a click elsewhere moves the caret.
            let selection = field.selection();
            if at < selection.start || at > selection.end {
                field.press(at, false, 1);
                field.release();
            }
            let can_paste = app
                .read_from_clipboard()
                .and_then(|item| item.text())
                .is_some();
            field.menu = Some(FieldMenu {
                at: position,
                can_paste,
            });
            true
        });
        open_menu(&menu, cx);
        cx.stop_propagation();
    });
}

/// The field whose menu is open, and the keystroke interceptor that closes it. The interceptor
/// runs before the keys reach the elements, so a key closes the menu first, and Escape stops
/// there and does not reach the owner.
struct OpenMenu {
    field: Option<FieldMouse>,
    _keys: Subscription,
}

impl Global for OpenMenu {}

/// Makes `mouse` the field whose menu is open. The first menu installs the interceptor.
fn open_menu(mouse: &FieldMouse, cx: &mut App) {
    if !cx.has_global::<OpenMenu>() {
        let keys = cx.intercept_keystrokes(|event, _, cx| {
            let Some(mouse) = cx.global_mut::<OpenMenu>().field.take() else {
                return;
            };
            let mut closed = false;
            mouse.apply(cx, &mut |field, _| {
                closed = field.menu.take().is_some();
                closed
            });
            if closed && event.keystroke.key == "escape" {
                cx.stop_propagation();
            }
        });
        cx.set_global(OpenMenu {
            field: None,
            _keys: keys,
        });
    }
    cx.global_mut::<OpenMenu>().field = Some(mouse.clone());
}

/// The byte offset of the character nearest to `point`, in the line nearest to it.
fn index_at(lines: &[Line], point: Point<Pixels>) -> usize {
    let y = f32::from(point.y);
    let mut nearest: Option<(&Line, f32)> = None;
    for line in lines {
        let top = f32::from(line.bounds.top());
        let bottom = f32::from(line.bounds.bottom());
        let distance = if y < top {
            top - y
        } else if y > bottom {
            y - bottom
        } else {
            0.
        };
        if nearest.is_none_or(|(_, best)| distance < best) {
            nearest = Some((line, distance));
        }
    }
    let Some((line, _)) = nearest else {
        return 0;
    };
    let x = (point.x - line.bounds.left()).max(px(0.));
    let offset = index_for_x(&boundaries(&line.shaped), x);
    (line.range.start + offset).min(line.range.end)
}

/// The places where a caret can stand in a shaped line: the start of each glyph, with its
/// byte offset, and the end of the line.
fn boundaries(line: &ShapedLine) -> Vec<(Pixels, usize)> {
    let mut boundaries: Vec<(Pixels, usize)> = line
        .runs
        .iter()
        .flat_map(|run| {
            run.glyphs
                .iter()
                .map(|glyph| (glyph.position.x, glyph.index))
        })
        .collect();
    boundaries.push((line.width, line.len()));
    boundaries
}

/// The byte offset of the boundary nearest to `x`. A tie goes to the earlier boundary. The
/// glyph starts alone are not enough: a click on the left half of the last letter must
/// stand before it.
pub(crate) fn index_for_x(boundaries: &[(Pixels, usize)], x: Pixels) -> usize {
    let mut best: Option<(f32, usize)> = None;
    for &(position, index) in boundaries {
        let distance = (f32::from(position) - f32::from(x)).abs();
        if best.is_none_or(|(known, _)| distance < known) {
            best = Some((distance, index));
        }
    }
    best.map_or(0, |(_, index)| index)
}

/// The menu that a right click opened on a field, or nothing when there is none or the
/// field has no mouse.
pub(crate) fn menu(mouse: &FieldMouse, field: &TextField, multiline: bool) -> Option<AnyElement> {
    let open = field.menu.clone()?;
    let theme = mouse.theme()?;
    let has_selection = field.has_selection();
    let empty = field.text.is_empty();
    // A read-only field has no Cut and no Paste: its menu only copies.
    let read_only = mouse.is_read_only();

    let close = mouse.clone();
    let cut = mouse.clone();
    let copy = mouse.clone();
    let paste = mouse.clone();
    let select_all = mouse.clone();
    let panel = surface(&theme)
        .occlude()
        .min_w(px(MENU_WIDTH))
        .py(px(POPUP_PADDING))
        .on_mouse_down_out(move |_: &MouseDownEvent, _: &mut Window, cx: &mut App| {
            close.apply(cx, &mut |field, _| field.menu.take().is_some());
        })
        .when(!read_only, |panel| {
            panel.child(menu_row(
                &theme,
                "Cut",
                "Ctrl+X",
                has_selection,
                move |cx| {
                    cut.apply_edit(cx, &mut |field, app| {
                        field.menu = None;
                        if !field.has_selection() {
                            return true;
                        }
                        let text = field.selected_text().to_owned();
                        app.write_to_clipboard(ClipboardItem::new_string(text));
                        field.backspace(false);
                        true
                    });
                },
            ))
        })
        .child(menu_row(
            &theme,
            "Copy",
            "Ctrl+C",
            has_selection,
            move |cx| {
                copy.apply(cx, &mut |field, app| {
                    field.menu = None;
                    if !field.has_selection() {
                        return true;
                    }
                    let text = field.selected_text().to_owned();
                    app.write_to_clipboard(ClipboardItem::new_string(text));
                    true
                });
            },
        ))
        .when(!read_only, |panel| {
            panel.child(menu_row(
                &theme,
                "Paste",
                "Ctrl+V",
                open.can_paste,
                move |cx| {
                    paste.apply_edit(cx, &mut |field, app| {
                        field.menu = None;
                        let Some(text) = app.read_from_clipboard().and_then(|item| item.text())
                        else {
                            return true;
                        };
                        field.paste(&text, multiline);
                        true
                    });
                },
            ))
        })
        .child(menu_row(
            &theme,
            "Select All",
            "Ctrl+A",
            !empty,
            move |cx| {
                select_all.apply(cx, &mut |field, _| {
                    field.menu = None;
                    field.select_all();
                    true
                });
            },
        ));

    Some(
        deferred(anchored().position(open.at).snap_to_window().child(panel))
            .with_priority(1)
            .into_any_element(),
    )
}

/// The width of a field menu.
const MENU_WIDTH: f32 = 200.;

/// One row of a field menu: its name, its keys, and what it does when it is picked. A row
/// whose action does not apply is grey and does nothing.
fn menu_row(
    theme: &Theme,
    label: &'static str,
    keys: &'static str,
    enabled: bool,
    pick: impl Fn(&mut App) + 'static,
) -> Div {
    let row = div()
        .flex()
        .items_center()
        .justify_between()
        .h(px(ROW_HEIGHT))
        .px(px(SPACE_M))
        .gap(px(SPACE_L))
        .debug_selector(move || format!("field-menu-{}", label.to_lowercase().replace(' ', "-")))
        .text_color(if enabled {
            theme.text
        } else {
            theme.text_muted
        })
        .child(label)
        .child(div().text_color(theme.text_muted).child(keys));
    if !enabled {
        return row;
    }
    row.cursor_pointer()
        .hover(|style| style.bg(theme.selection))
        .on_mouse_down(
            MouseButton::Left,
            move |_: &MouseDownEvent, _: &mut Window, cx: &mut App| pick(cx),
        )
}

#[cfg(test)]
mod tests {
    use gpui_kit::{Pixels, px};

    use super::index_for_x;

    /// The boundaries of "abc" when each letter is 10 pixels wide.
    fn abc() -> Vec<(Pixels, usize)> {
        vec![(px(0.), 0), (px(10.), 1), (px(20.), 2), (px(30.), 3)]
    }

    #[test]
    fn the_caret_goes_to_the_boundary_nearest_to_the_pointer() {
        let boundaries = abc();
        assert_eq!(index_for_x(&boundaries, px(-4.)), 0, "left of the text");
        assert_eq!(index_for_x(&boundaries, px(0.)), 0, "the start");
        assert_eq!(index_for_x(&boundaries, px(4.)), 0, "left half of a");
        assert_eq!(index_for_x(&boundaries, px(6.)), 1, "right half of a");
        assert_eq!(index_for_x(&boundaries, px(14.)), 1, "middle of b");
        assert_eq!(index_for_x(&boundaries, px(22.)), 2, "left half of c");
        assert_eq!(index_for_x(&boundaries, px(28.)), 3, "right half of c");
        assert_eq!(index_for_x(&boundaries, px(30.)), 3, "the end");
        assert_eq!(index_for_x(&boundaries, px(90.)), 3, "after the end");
    }

    #[test]
    fn an_empty_line_has_one_boundary_at_its_start() {
        assert_eq!(index_for_x(&[(px(0.), 0)], px(12.)), 0);
        assert_eq!(index_for_x(&[], px(12.)), 0);
    }
}

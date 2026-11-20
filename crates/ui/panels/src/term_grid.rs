//! A terminal screen painted as a grid of cells, the selection and the runs a search
//! found, and the pointer of the window turned into a place in the text. The session
//! keeps the screen; this only reads its latest snapshot.

use std::cell::Cell as SharedCell;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use terminal::{
    Cell, CellColor, CellFlags, Key, KeyEvent, Mods, Screen, SearchMatch, TerminalSession,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::term_links::LinkText;

/// How often a view looks for a new snapshot.
const REFRESH: Duration = Duration::from_millis(16);
const FONT_SIZE: f32 = 13.;
const LINE_HEIGHT: f32 = 18.;
const INSET: f32 = 6.;
const ANSI: [u32; 16] = [
    0x000000, 0xcd3131, 0x0dbc79, 0xe5e510, 0x2472c8, 0xbc3fbc, 0x11a8cd, 0xe5e5e5, 0x666666,
    0xf14c4c, 0x23d18b, 0xf5f543, 0x3b8eea, 0xd670d6, 0x29b8db, 0xffffff,
];
const CUBE: [u8; 6] = [0, 95, 135, 175, 215, 255];

/// Calls `tick` on every refresh while the entity lives.
pub fn every_frame<T: 'static>(
    cx: &mut Context<T>,
    mut tick: impl FnMut(&mut T, &mut Context<T>) + 'static,
) {
    cx.spawn(async move |this, cx| {
        loop {
            cx.background_executor().timer(REFRESH).await;
            if this.update(cx, |this, cx| tick(this, cx)).is_err() {
                break;
            }
        }
    })
    .detach();
}

fn rgb(r: u8, g: u8, b: u8) -> Hsla {
    let channel = |value: u8| value as f32 / 255.;
    Rgba {
        r: channel(r),
        g: channel(g),
        b: channel(b),
        a: 1.,
    }
    .into()
}

fn indexed(index: u8) -> Hsla {
    match index {
        0..16 => {
            let value = ANSI[index as usize];
            rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
        }
        16..232 => {
            let cube = index - 16;
            let level = |step: u8| CUBE[step as usize];
            rgb(level(cube / 36), level(cube / 6 % 6), level(cube % 6))
        }
        _ => {
            let gray = 8 + (index - 232) * 10;
            rgb(gray, gray, gray)
        }
    }
}

fn color(color: CellColor, default: Hsla) -> Hsla {
    match color {
        CellColor::Default => default,
        CellColor::Indexed(index) => indexed(index),
        CellColor::Rgb(r, g, b) => rgb(r, g, b),
    }
}

/// Foreground and background of a cell, `None` for the background of the panel.
fn colors(cell: &Cell, theme: &Theme) -> (Hsla, Option<Hsla>) {
    let fg = color(cell.fg, theme.code);
    let bg = match cell.bg {
        CellColor::Default => None,
        other => Some(color(other, theme.island_bg)),
    };
    let (fg, bg) = if cell.flags.contains(CellFlags::INVERSE) {
        (bg.unwrap_or(theme.island_bg), Some(fg))
    } else {
        (fg, bg)
    };
    let fg = if cell.flags.contains(CellFlags::DIM) {
        fg.opacity(0.6)
    } else {
        fg
    };
    (fg, bg)
}

/// Where a place of the text is: a line counted like the rows of [`Screen`] with its
/// display offset, so that a line of the history keeps its text when the screen scrolls.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct Position {
    pub line: i32,
    pub column: u16,
}

impl Position {
    pub fn new(line: i32, column: u16) -> Self {
        Self { line, column }
    }
}

/// What the user selected, by its ends, both included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    pub anchor: Position,
    pub head: Position,
}

impl Selection {
    pub fn new(anchor: Position, head: Position) -> Self {
        Self { anchor, head }
    }

    /// The ends in the order of the text.
    pub fn ordered(&self) -> (Position, Position) {
        if self.anchor <= self.head {
            (self.anchor, self.head)
        } else {
            (self.head, self.anchor)
        }
    }
}

/// The columns of one row a selection covers, with the last one left out.
pub fn selection_span(selection: Selection, line: i32, cols: u16) -> Option<(u16, u16)> {
    let (start, end) = selection.ordered();
    if line < start.line || line > end.line {
        return None;
    }
    let from = if line == start.line { start.column } else { 0 };
    let to = if line == end.line {
        end.column.saturating_add(1)
    } else {
        cols
    };
    let from = from.min(cols);
    let to = to.min(cols);
    (from < to).then_some((from, to))
}

/// The text of one row as the screen shows it, one character per column.
pub fn row_text(screen: &Screen, row: u16) -> String {
    screen
        .row(row)
        .iter()
        .map(|cell| {
            if cell.ch == '\0' || cell.flags.contains(CellFlags::WIDE_SPACER) {
                ' '
            } else {
                cell.ch
            }
        })
        .collect()
}

/// The text of the cells a selection covers. A line the screen does not show is left
/// out, as a selection cannot name text nobody sees.
pub fn selection_text(screen: &Screen, selection: Selection) -> String {
    let (start, end) = selection.ordered();
    let offset = screen.display_offset as i32;
    let mut lines = Vec::new();
    for line in start.line..=end.line {
        let row = line + offset;
        if row < 0 || row >= screen.rows as i32 {
            continue;
        }
        let text: Vec<char> = row_text(screen, row as u16).chars().collect();
        let from = if line == start.line {
            start.column as usize
        } else {
            0
        };
        let to = if line == end.line {
            end.column as usize + 1
        } else {
            screen.cols as usize
        };
        let from = from.min(text.len());
        let to = to.min(text.len()).max(from);
        let mut piece: String = text[from..to].iter().collect();
        while piece.ends_with(' ') {
            piece.pop();
        }
        lines.push(piece);
    }
    lines.join("\n")
}

/// The word around `column`: the run of characters that are not blanks.
pub fn word_at(screen: &Screen, row: u16, line: i32, column: u16) -> Option<Selection> {
    let text: Vec<char> = row_text(screen, row).chars().collect();
    let at = column as usize;
    if at >= text.len() || text[at].is_whitespace() {
        return None;
    }
    let start = (0..at)
        .rev()
        .take_while(|&index| !text[index].is_whitespace())
        .last()
        .unwrap_or(at);
    let end = (at..text.len())
        .take_while(|&index| !text[index].is_whitespace())
        .last()
        .unwrap_or(at);
    Some(Selection::new(
        Position::new(line, start as u16),
        Position::new(line, end as u16),
    ))
}

/// The whole row a triple click picks.
pub fn line_selection(screen: &Screen, row: u16, line: i32) -> Selection {
    let end = screen
        .row(row)
        .iter()
        .rposition(|cell| !cell.ch.is_whitespace() && !cell.flags.contains(CellFlags::WIDE_SPACER))
        .unwrap_or(0) as u16;
    Selection::new(Position::new(line, 0), Position::new(line, end))
}

/// Where the cells of the grid are on the window and how the text maps onto them.
#[derive(Clone, Copy, Debug)]
pub struct CellMetrics {
    pub bounds: Bounds<Pixels>,
    pub origin: Point<Pixels>,
    pub cell_width: Pixels,
    pub line_height: Pixels,
    pub cols: u16,
    pub rows: u16,
    pub offset: usize,
}

impl CellMetrics {
    /// The place of the text under the pointer, `None` outside the grid.
    pub fn position(&self, at: Point<Pixels>) -> Option<Position> {
        if !self.bounds.contains(&at) {
            return None;
        }
        let column = (f32::from(at.x - self.origin.x) / f32::from(self.cell_width)).floor();
        let row = (f32::from(at.y - self.origin.y) / f32::from(self.line_height)).floor();
        if column < 0. || row < 0. {
            return None;
        }
        let column = column as u16;
        let row = row as u16;
        if column >= self.cols || row >= self.rows {
            return None;
        }
        Some(Position::new(
            row as i32 - self.offset as i32,
            column.min(self.cols.saturating_sub(1)),
        ))
    }

    /// The place of a line of the text on the screen, `None` when it is scrolled out.
    #[cfg(test)]
    pub fn row_of(&self, line: i32) -> Option<u16> {
        let row = line + self.offset as i32;
        (row >= 0 && row < self.rows as i32).then_some(row as u16)
    }
}

fn text_run(len: usize, cell: &Cell, fg: Hsla, base: &Font) -> TextRun {
    let mut font = base.clone();
    if cell.flags.contains(CellFlags::BOLD) {
        font.weight = FontWeight::BOLD;
    }
    if cell.flags.contains(CellFlags::ITALIC) {
        font.style = FontStyle::Italic;
    }
    let underline = cell
        .flags
        .contains(CellFlags::UNDERLINE)
        .then(|| UnderlineStyle {
            thickness: px(1.),
            color: Some(fg),
            wavy: false,
        });
    let strikethrough = cell
        .flags
        .contains(CellFlags::STRIKEOUT)
        .then(|| StrikethroughStyle {
            thickness: px(1.),
            color: Some(fg),
        });
    TextRun {
        len,
        font,
        color: fg,
        background_color: None,
        underline,
        strikethrough,
    }
}

fn same_look(a: &Cell, b: &Cell) -> bool {
    a.fg == b.fg && a.bg == b.bg && a.flags == b.flags
}

/// The panel that owns a grid and hears its pointer. The terminal window and the AI sessions
/// implement it, so one grid draws and listens the same way for both.
pub trait GridPanel: Sized + 'static {
    /// The focus that a press in `pane` gives to the window.
    fn pane_focus(&self, pane: usize) -> FocusHandle;
    /// A press of the left button on the grid; `clicks` tells a click from a double or a triple one.
    fn grid_pressed(
        &mut self,
        pane: usize,
        position: Position,
        clicks: usize,
        cx: &mut Context<Self>,
    );
    /// The pointer moved; `position` is `None` outside the grid. Returns true over a link.
    fn grid_moved(
        &mut self,
        pane: usize,
        position: Option<Position>,
        control: bool,
        cx: &mut Context<Self>,
    ) -> bool;
    /// The left button came up.
    fn grid_released(&mut self, pane: usize, cx: &mut Context<Self>);
    /// The right button: the menu of the grid, at the pointer.
    fn grid_context_menu(&mut self, pane: usize, position: Point<Pixels>, cx: &mut Context<Self>);
    /// The link under a press with the control key, for a panel that opens links.
    fn link_at(&self, _pane: usize, _position: Position) -> Option<LinkText> {
        None
    }
    /// Opens the link that `link_at` found.
    fn open_link(
        &mut self,
        _pane: usize,
        _position: Position,
        _link: LinkText,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) {
    }
}

/// What a press of the left button does to the selection of `screen`: one click clears the
/// selection and starts a drag from its place, two clicks take the word under it and three
/// clicks the row. Returns the selection and the place a drag extends from.
pub fn pressed(
    screen: &Screen,
    position: Position,
    clicks: usize,
) -> (Option<Selection>, Option<Position>) {
    let row = position.line + screen.display_offset as i32;
    let row = match u16::try_from(row) {
        Ok(row) if row < screen.rows => row,
        _ => return (None, None),
    };
    match clicks {
        3.. => (Some(line_selection(screen, row, position.line)), None),
        2 => (word_at(screen, row, position.line, position.column), None),
        _ => (None, Some(position)),
    }
}

/// Copies the text that `selection` covers on `session` to the clipboard. False when there is
/// no selection or it covers no text.
pub fn copy_selection(
    session: &TerminalSession,
    selection: Option<Selection>,
    cx: &mut App,
) -> bool {
    let Some(selection) = selection else {
        return false;
    };
    let text = selection_text(&session.screen(), selection);
    if text.is_empty() {
        return false;
    }
    cx.write_to_clipboard(ClipboardItem::new_string(text));
    true
}

/// The pointer of a grid: who hears it and where its cells are.
struct Pointer<H: GridPanel> {
    host: Entity<H>,
    pane: usize,
    metrics: CellMetrics,
    hitbox: Hitbox,
}

impl<H: GridPanel> Pointer<H> {
    /// The clicks select or open a link, a drag extends the selection, and the control
    /// key shows the link under the pointer.
    fn listen(self, window: &mut Window) {
        let Pointer {
            host,
            pane,
            metrics,
            hitbox,
        } = self;

        let target = host.clone();
        let pressed = hitbox.clone();
        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || !pressed.bounds.contains(&event.position) {
                return;
            }
            let Some(position) = metrics.position(event.position) else {
                return;
            };
            let focus = target.read(cx).pane_focus(pane);
            window.focus(&focus, cx);
            if event.button == MouseButton::Right {
                target.update(cx, |panel, cx| {
                    panel.grid_context_menu(pane, event.position, cx)
                });
                return;
            }
            if event.button != MouseButton::Left {
                return;
            }
            let link = event
                .modifiers
                .control
                .then(|| target.read(cx).link_at(pane, position))
                .flatten();
            match link {
                Some(link) => target.update(cx, |panel, cx| {
                    panel.open_link(pane, position, link, window, cx)
                }),
                None => target.update(cx, |panel, cx| {
                    panel.grid_pressed(pane, position, event.click_count, cx)
                }),
            }
        });

        let moved = host.clone();
        let hovered = hitbox;
        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble {
                return;
            }
            let inside = hovered.bounds.contains(&event.position);
            let position = inside.then(|| metrics.position(event.position)).flatten();
            let over_link = moved.update(cx, |panel, cx| {
                panel.grid_moved(pane, position, event.modifiers.control, cx)
            });
            if over_link {
                window.set_cursor_style(CursorStyle::PointingHand, &hovered);
            }
        });

        let released = host;
        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            released.update(cx, |panel, cx| panel.grid_released(pane, cx));
        });
    }
}

pub struct Frame<H: GridPanel> {
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<(Bounds<Pixels>, Hsla)>,
    line_height: Pixels,
    pointer: Option<Pointer<H>>,
}

impl<H: GridPanel> Frame<H> {
    fn paint(mut self, window: &mut Window, cx: &mut App) {
        if let Some(pointer) = self.pointer.take() {
            pointer.listen(window);
        }
        for (bounds, color) in self.backgrounds {
            window.paint_quad(fill(bounds, color));
        }
        if let Some((bounds, color)) = self.cursor {
            window.paint_quad(fill(bounds, color));
        }
        for (origin, line) in self.lines {
            let _ = line.paint(origin, self.line_height, TextAlign::Left, None, window, cx);
        }
    }
}

/// What a view of the grid shows and who hears its pointer.
pub struct GridView<H: GridPanel> {
    pub session: Arc<TerminalSession>,
    pub focused: bool,
    pub theme: Theme,
    pub selection: Option<Selection>,
    /// Every run the search found, over the whole scrollback.
    pub matches: Vec<SearchMatch>,
    /// The run the search moved to, when it is one of `matches`.
    pub current: Option<SearchMatch>,
    /// The link under the pointer with the control key held, if any.
    pub hover: Option<SearchMatch>,
    pub host: Option<Entity<H>>,
    pub pane: usize,
    /// Where the last frame put the grid, for the drag of the split divider.
    pub metrics: Rc<SharedCell<Option<CellMetrics>>>,
}

/// Shapes the visible rows and asks the session for the size that fits the bounds.
fn layout<H: GridPanel>(
    view: &GridView<H>,
    bounds: Bounds<Pixels>,
    window: &mut Window,
) -> Frame<H> {
    let theme = &view.theme;
    let mut mono = font(MONO_FONT);
    mono.features = FontFeatures::disable_ligatures();
    let font_size = px(snapped_code_size(FONT_SIZE, window.scale_factor()));
    let system = window.text_system().clone();
    let probe = [text_run(1, &Cell::default(), theme.code, &mono)];
    let cell_width = system.shape_line("0".into(), font_size, &probe, None).width;
    let line_height = px(LINE_HEIGHT);

    let width = f32::from(bounds.size.width) - 2. * INSET;
    let height = f32::from(bounds.size.height) - 2. * INSET;
    let cols = (width / f32::from(cell_width)).floor().max(2.) as u16;
    let rows = (height / LINE_HEIGHT).floor().max(1.) as u16;
    if view.session.size() != (cols, rows) {
        view.session.resize(cols, rows);
    }

    let screen: Arc<Screen> = view.session.screen();
    let origin = point(bounds.left() + px(INSET), bounds.top() + px(INSET));
    let metrics = CellMetrics {
        bounds,
        origin,
        cell_width,
        line_height,
        cols,
        rows,
        offset: screen.display_offset,
    };
    let mut frame = Frame {
        backgrounds: Vec::new(),
        lines: Vec::new(),
        cursor: None,
        line_height,
        pointer: None,
    };
    for row in 0..screen.rows.min(rows) {
        let y = origin.y + line_height * row as f32;
        let cells = screen.row(row);
        let (mut text, mut runs) = (String::new(), Vec::new());
        let mut start = 0;
        while start < cells.len() {
            let first = &cells[start];
            let end = (start..cells.len())
                .find(|&index| !same_look(first, &cells[index]))
                .unwrap_or(cells.len());
            let (fg, bg) = colors(first, theme);
            if let Some(bg) = bg {
                let left = origin.x + cell_width * start as f32;
                let span = size(cell_width * (end - start) as f32, line_height);
                frame
                    .backgrounds
                    .push((Bounds::new(point(left, y), span), bg));
            }
            let before = text.len();
            for cell in &cells[start..end] {
                // The wide glyph on the left covers it; a space keeps the columns in place.
                let spacer = cell.flags.contains(CellFlags::WIDE_SPACER);
                text.push(if spacer || cell.ch == '\0' {
                    ' '
                } else {
                    cell.ch
                });
            }
            runs.push(text_run(text.len() - before, first, fg, &mono));
            start = end;
        }
        let line = system.shape_line(text.into(), font_size, &runs, Some(cell_width));
        frame.lines.push((point(origin.x, y), line));
    }
    // The selection, the runs of the search and the link under the pointer sit over the
    // background of the cells and under their text.
    let line = metrics.offset as i32;
    for row in 0..metrics.rows {
        let absolute = row as i32 - line;
        let y = origin.y + line_height * row as f32;
        if let Some(selection) = view.selection
            && let Some((start, end)) = selection_span(selection, absolute, metrics.cols)
        {
            let left = origin.x + cell_width * start as f32;
            let span = size(cell_width * (end.saturating_sub(start)) as f32, line_height);
            frame
                .backgrounds
                .push((Bounds::new(point(left, y), span), theme.selection));
        }
        for found in &view.matches {
            if found.line != absolute {
                continue;
            }
            let current = view.current == Some(*found);
            let color = if current {
                theme.accent
            } else {
                theme.match_bg
            };
            let span = size(
                cell_width * (found.end_col.saturating_sub(found.start_col)) as f32,
                line_height,
            );
            let at = point(origin.x + cell_width * found.start_col as f32, y);
            frame.backgrounds.push((Bounds::new(at, span), color));
        }
        if let Some(hover) = view.hover
            && hover.line == absolute
        {
            let left = origin.x + cell_width * hover.start_col as f32;
            let span = size(
                cell_width * (hover.end_col.saturating_sub(hover.start_col)) as f32,
                px(1.),
            );
            let at = point(left, y + line_height - px(1.));
            frame.backgrounds.push((Bounds::new(at, span), theme.link));
        }
    }
    if let Some((row, col)) = screen.cursor
        && row < rows
    {
        let at = point(
            origin.x + cell_width * col as f32,
            origin.y + line_height * row as f32,
        );
        let alpha = if view.focused { 0.6 } else { 0.25 };
        let block = Bounds::new(at, size(cell_width, line_height));
        frame.cursor = Some((block, theme.caret.opacity(alpha)));
    }
    view.metrics.set(Some(metrics));
    let hitbox = window.insert_hitbox(metrics.bounds, HitboxBehavior::Normal);
    frame.pointer = view.host.clone().map(|host| Pointer {
        host,
        pane: view.pane,
        metrics,
        hitbox,
    });
    frame
}

/// The screen of the view, sized to fill its parent.
pub fn grid<H: GridPanel>(view: GridView<H>) -> impl IntoElement {
    div()
        .debug_selector(|| "terminal-grid".to_owned())
        .size_full()
        .child(
            canvas(
                move |bounds, window, _| layout(&view, bounds, window),
                |_, frame: Frame<H>, window, cx| frame.paint(window, cx),
            )
            .size_full(),
        )
}

fn named(key: &str) -> Option<Key> {
    Some(match key {
        "enter" => Key::Enter,
        "tab" => Key::Tab,
        "backspace" => Key::Backspace,
        "escape" => Key::Escape,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" => Key::PageUp,
        "pagedown" => Key::PageDown,
        "insert" => Key::Insert,
        "delete" => Key::Delete,
        "space" => Key::Char(' '),
        _ => {
            let number: u8 = key.strip_prefix('f')?.parse().ok()?;
            if !(1..=12).contains(&number) {
                return None;
            }
            Key::F(number)
        }
    })
}

/// What the program should receive for a key press, nothing when the window keeps it.
pub fn key_events(stroke: &Keystroke) -> Vec<KeyEvent> {
    let modifiers = stroke.modifiers;
    if modifiers.platform || modifiers.function {
        return Vec::new();
    }
    let mods = Mods {
        shift: modifiers.shift,
        ctrl: modifiers.control,
        alt: modifiers.alt,
    };
    if let Some(key) = named(&stroke.key) {
        return vec![KeyEvent::new(key, mods)];
    }
    if !mods.ctrl
        && !mods.alt
        && let Some(text) = &stroke.key_char
    {
        let keys = text.chars().map(|ch| KeyEvent::plain(Key::Char(ch)));
        return keys.collect();
    }
    let mut chars = stroke.key.chars();
    match (chars.next(), chars.next()) {
        (Some(ch), None) => vec![KeyEvent::new(Key::Char(ch), mods)],
        _ => Vec::new(),
    }
}

/// Ctrl+V and Shift+Insert paste, as in the terminal of the IDE we follow.
pub fn is_paste(stroke: &Keystroke) -> bool {
    let modifiers = stroke.modifiers;
    let ctrl_v = modifiers.control && !modifiers.alt && stroke.key == "v";
    let shift_insert = modifiers.shift && !modifiers.control && stroke.key == "insert";
    ctrl_v || shift_insert
}

/// Sends a key press to `session`. Returns false when the press is not for the terminal.
/// The keys of the terminal window itself are handled before this: on macOS Control
/// reaches the shell, so Ctrl+V is a quoted insert there and not a paste.
pub fn send_key(
    session: &TerminalSession,
    stroke: &Keystroke,
    platform: keymap::Platform,
    cx: &mut App,
) -> bool {
    if is_paste(stroke) && !platform.uses_command() {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            let _ = session.paste(&text);
        }
        return true;
    }
    let events = key_events(stroke);
    for event in &events {
        if session.key(*event).is_err() {
            break;
        }
    }
    if !events.is_empty() {
        session.scroll_to_bottom();
    }
    !events.is_empty()
}

/// Wheel lines into history lines, three per notch.
pub fn scroll_lines(event: &ScrollEvent) -> i32 {
    let delta = event.delta.pixel_delta(px(LINE_HEIGHT));
    (f32::from(delta.y) / LINE_HEIGHT).round() as i32
}

#[cfg(test)]
mod tests {
    use super::{
        Key, KeyEvent, Keystroke, Mods, OperatingSystem, Position, Selection, indexed, is_paste,
        key_events, line_selection, pressed, rgb, row_text, selection_span, selection_text,
        word_at,
    };
    use terminal::{Cell, Screen};
    use ui_kit::{gpui, px};

    fn stroke(text: &str) -> Keystroke {
        Keystroke::parse_with_os(text, OperatingSystem::Windows).unwrap()
    }

    #[test]
    fn typed_text_and_named_keys_become_terminal_keys() {
        let typed = Keystroke {
            key: "{".to_string(),
            key_char: Some("{".to_string()),
            ..Keystroke::default()
        };
        assert_eq!(key_events(&typed), [KeyEvent::plain(Key::Char('{'))]);
        assert_eq!(key_events(&stroke("enter")), [KeyEvent::plain(Key::Enter)]);
        assert_eq!(
            key_events(&stroke("ctrl-c")),
            [KeyEvent::new(Key::Char('c'), Mods::CTRL)]
        );
        assert_eq!(key_events(&stroke("f5")), [KeyEvent::plain(Key::F(5))]);
    }

    #[test]
    fn ctrl_v_pastes_instead_of_typing() {
        assert!(is_paste(&stroke("ctrl-v")));
        assert!(is_paste(&stroke("shift-insert")));
        assert!(!is_paste(&stroke("v")));
    }

    #[test]
    fn the_palette_covers_all_256_colors() {
        assert_eq!(indexed(1), rgb(0xcd, 0x31, 0x31));
        assert_eq!(indexed(16), rgb(0, 0, 0));
        assert_eq!(indexed(231), rgb(255, 255, 255));
        assert_eq!(indexed(255), rgb(238, 238, 238));
    }

    fn screen(lines: &[&str], offset: usize) -> Screen {
        let cols = lines
            .iter()
            .map(|line| line.chars().count())
            .max()
            .unwrap_or(1) as u16;
        let mut cells = Vec::new();
        for line in lines {
            let mut row: Vec<Cell> = line
                .chars()
                .map(|ch| Cell {
                    ch,
                    ..Cell::default()
                })
                .collect();
            row.resize(cols as usize, Cell::default());
            cells.extend(row);
        }
        Screen {
            cols,
            rows: lines.len() as u16,
            cells,
            display_offset: offset,
            ..Screen::default()
        }
    }

    #[test]
    fn a_selection_covers_its_lines_and_stops_at_their_ends() {
        let selection = Selection::new(Position::new(2, 3), Position::new(4, 1));

        assert_eq!(selection_span(selection, 1, 20), None);
        assert_eq!(selection_span(selection, 2, 20), Some((3, 20)));
        assert_eq!(selection_span(selection, 3, 20), Some((0, 20)));
        assert_eq!(selection_span(selection, 4, 20), Some((0, 2)));
        assert_eq!(selection_span(selection, 5, 20), None);

        let backwards = Selection::new(Position::new(4, 1), Position::new(2, 3));
        assert_eq!(selection_span(backwards, 4, 20), Some((0, 2)));
    }

    #[test]
    fn the_text_of_a_selection_follows_the_screen_and_its_scroll() {
        let screen = screen(&["alpha beta", "second   ", "third"], 2);

        let selection = Selection::new(Position::new(-2, 6), Position::new(-2, 9));
        assert_eq!(selection_text(&screen, selection), "beta");

        let selection = Selection::new(Position::new(-2, 0), Position::new(-1, 5));
        assert_eq!(selection_text(&screen, selection), "alpha beta\nsecond");
        assert_eq!(row_text(&screen, 1), "second    ");
    }

    #[test]
    fn a_click_turns_into_the_cell_under_the_pointer() {
        let metrics = super::CellMetrics {
            bounds: gpui::Bounds::new(gpui::point(px(0.), px(0.)), gpui::size(px(80.), px(36.))),
            origin: gpui::point(px(6.), px(6.)),
            cell_width: px(8.),
            line_height: px(18.),
            cols: 10,
            rows: 2,
            offset: 3,
        };

        assert_eq!(
            metrics.position(gpui::point(px(6. + 8. * 4. + 1.), px(6. + 18. + 1.))),
            Some(Position::new(-2, 4))
        );
        assert_eq!(metrics.position(gpui::point(px(1.), px(1.))), None);
        assert_eq!(metrics.position(gpui::point(px(200.), px(200.))), None);
        assert_eq!(metrics.row_of(-3), Some(0));
        assert_eq!(metrics.row_of(-2), Some(1));
        assert_eq!(metrics.row_of(0), None, "a line below the two rows");
    }

    #[test]
    fn a_double_click_takes_a_word_and_a_triple_click_the_row() {
        let screen = screen(&["  run the build  "], 0);
        assert_eq!(
            word_at(&screen, 0, 0, 8),
            Some(Selection::new(Position::new(0, 6), Position::new(0, 8)))
        );
        assert_eq!(word_at(&screen, 0, 0, 1), None, "a blank has no word");
        assert_eq!(
            line_selection(&screen, 0, 0),
            Selection::new(Position::new(0, 0), Position::new(0, 14))
        );
    }

    #[test]
    fn a_press_starts_a_drag_or_takes_the_word_or_the_row() {
        let screen = screen(&["  run the build  "], 0);
        let at = Position::new(0, 8);
        assert_eq!(pressed(&screen, at, 1), (None, Some(at)));
        assert_eq!(
            pressed(&screen, at, 2),
            (
                Some(Selection::new(Position::new(0, 6), Position::new(0, 8))),
                None
            )
        );
        assert_eq!(
            pressed(&screen, at, 3),
            (Some(line_selection(&screen, 0, 0)), None)
        );
        // A place above the screen is not on the grid, so nothing is pressed.
        let above = Position::new(-1, 0);
        assert_eq!(pressed(&screen, above, 1), (None, None));
    }
}

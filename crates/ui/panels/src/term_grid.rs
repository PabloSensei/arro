//! A terminal screen painted as a grid of cells, and key presses of the window turned into
//! terminal keys. The session keeps the screen; this only reads its latest snapshot.

use std::sync::Arc;
use std::time::Duration;

use terminal::{Cell, CellColor, CellFlags, Key, KeyEvent, Mods, Screen, TerminalSession};
use ui_kit::metrics::*;
use ui_kit::*;

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

pub struct Frame {
    backgrounds: Vec<(Bounds<Pixels>, Hsla)>,
    lines: Vec<(Point<Pixels>, ShapedLine)>,
    cursor: Option<(Bounds<Pixels>, Hsla)>,
    line_height: Pixels,
}

impl Frame {
    fn paint(self, window: &mut Window, cx: &mut App) {
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

/// Shapes the visible rows and asks the session for the size that fits the bounds.
fn layout(
    session: &TerminalSession,
    bounds: Bounds<Pixels>,
    focused: bool,
    theme: &Theme,
    window: &mut Window,
) -> Frame {
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
    if session.size() != (cols, rows) {
        session.resize(cols, rows);
    }

    let screen: Arc<Screen> = session.screen();
    let origin = point(bounds.left() + px(INSET), bounds.top() + px(INSET));
    let mut frame = Frame {
        backgrounds: Vec::new(),
        lines: Vec::new(),
        cursor: None,
        line_height,
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
    if let Some((row, col)) = screen.cursor
        && row < rows
    {
        let at = point(
            origin.x + cell_width * col as f32,
            origin.y + line_height * row as f32,
        );
        let alpha = if focused { 0.6 } else { 0.25 };
        let block = Bounds::new(at, size(cell_width, line_height));
        frame.cursor = Some((block, theme.caret.opacity(alpha)));
    }
    frame
}

/// The screen of `session`, sized to fill its parent.
pub fn grid(session: Arc<TerminalSession>, focused: bool, theme: Theme) -> impl IntoElement {
    canvas(
        move |bounds, window, _| layout(&session, bounds, focused, &theme, window),
        |_, frame: Frame, window, cx| frame.paint(window, cx),
    )
    .size_full()
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
pub fn send_key(session: &TerminalSession, stroke: &Keystroke, cx: &mut App) -> bool {
    if is_paste(stroke) {
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
        Key, KeyEvent, Keystroke, Mods, OperatingSystem, indexed, is_paste, key_events, rgb,
    };

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
}

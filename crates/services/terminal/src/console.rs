//! A session drawn into the console of this process, for trying the core before the terminal panel
//! exists. The renderer is crude on purpose: it repaints the whole screen.

use std::fmt::Write as _;
use std::io::{self, Write as _};
use std::mem::zeroed;
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Console::{
    CONSOLE_SCREEN_BUFFER_INFO, ENABLE_VIRTUAL_TERMINAL_PROCESSING, ENABLE_WINDOW_INPUT,
    GetConsoleMode, GetConsoleScreenBufferInfo, GetStdHandle, INPUT_RECORD, KEY_EVENT,
    KEY_EVENT_RECORD, ReadConsoleInputW, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, SetConsoleMode,
    SetConsoleOutputCP, WINDOW_BUFFER_SIZE_EVENT,
};

use crate::keys::{Key, KeyEvent, Mods};
use crate::screen::{Cell, CellColor, CellFlags, Screen};
use crate::session::TerminalSession;

const UTF8: u32 = 65001;
const PAINT_INTERVAL: Duration = Duration::from_millis(33);
const VK_RETURN: u16 = 0x0D;
const SHIFT_PRESSED: u32 = 0x10;

struct Handles {
    input: HANDLE,
    output: HANDLE,
}

fn handles() -> Handles {
    unsafe {
        Handles {
            input: GetStdHandle(STD_INPUT_HANDLE),
            output: GetStdHandle(STD_OUTPUT_HANDLE),
        }
    }
}

/// The size of the console window, for spawning a session that fills it.
pub fn console_size() -> Option<(u16, u16)> {
    let mut info: CONSOLE_SCREEN_BUFFER_INFO = unsafe { zeroed() };
    let found = unsafe { GetConsoleScreenBufferInfo(handles().output, &mut info) };
    let window = info.srWindow;
    (found != 0).then_some((
        (window.Right - window.Left + 1) as u16,
        (window.Bottom - window.Top + 1) as u16,
    ))
}

fn color(out: &mut String, color: CellColor, base: u8) {
    let _ = match color {
        CellColor::Default => write!(out, "\x1b[{}m", base + 9),
        CellColor::Indexed(index) => write!(out, "\x1b[{};5;{index}m", base + 8),
        CellColor::Rgb(r, g, b) => write!(out, "\x1b[{};2;{r};{g};{b}m", base + 8),
    };
}

fn style(out: &mut String, cell: &Cell) {
    out.push_str("\x1b[0m");
    let pairs = [
        (CellFlags::BOLD, "\x1b[1m"),
        (CellFlags::DIM, "\x1b[2m"),
        (CellFlags::ITALIC, "\x1b[3m"),
        (CellFlags::UNDERLINE, "\x1b[4m"),
        (CellFlags::INVERSE, "\x1b[7m"),
        (CellFlags::STRIKEOUT, "\x1b[9m"),
    ];
    for (flag, code) in pairs {
        if cell.flags.contains(flag) {
            out.push_str(code);
        }
    }
    color(out, cell.fg, 30);
    color(out, cell.bg, 40);
}

fn paint(screen: &Screen) -> String {
    let mut out = String::from("\x1b[?25l");
    for row in 0..screen.rows {
        let _ = write!(out, "\x1b[{};1H", row + 1);
        let mut last = None;
        for cell in screen.row(row) {
            if cell.flags.contains(CellFlags::WIDE_SPACER) {
                continue;
            }
            let look = (cell.fg, cell.bg, cell.flags);
            if last != Some(look) {
                style(&mut out, cell);
                last = Some(look);
            }
            out.push(cell.ch);
        }
    }
    out.push_str("\x1b[0m");
    if let Some((row, col)) = screen.cursor {
        let _ = write!(out, "\x1b[{};{}H\x1b[?25h", row + 1, col + 1);
    }
    out
}

fn forward_key(session: &TerminalSession, key: &KEY_EVENT_RECORD) {
    let vk = key.wVirtualKeyCode;
    let control = key.dwControlKeyState;
    let down = key.bKeyDown != 0;
    let plain_shift = control & 0x1F == SHIFT_PRESSED;
    if vk == VK_RETURN && plain_shift {
        // The encoding depends on the program, and the session knows which one runs.
        if down {
            let _ = session.key(KeyEvent::new(Key::Enter, Mods::SHIFT));
        }
        return;
    }
    let scan = key.wVirtualScanCode;
    let unicode = unsafe { key.uChar.UnicodeChar };
    let repeat = key.wRepeatCount;
    let down = down as u8;
    let record = format!("\x1b[{vk};{scan};{unicode};{down};{control};{repeat}_");
    let _ = session.send(record.into_bytes());
}

fn forward_input(session: Arc<TerminalSession>, input: HANDLE) {
    let mut records: [INPUT_RECORD; 32] = unsafe { zeroed() };
    loop {
        let mut count = 0;
        let read = unsafe { ReadConsoleInputW(input, records.as_mut_ptr(), 32, &mut count) };
        if read == 0 {
            return;
        }
        for record in &records[..count as usize] {
            match record.EventType as u32 {
                KEY_EVENT => forward_key(&session, unsafe { &record.Event.KeyEvent }),
                WINDOW_BUFFER_SIZE_EVENT => {
                    if let Some((cols, rows)) = console_size() {
                        session.resize(cols, rows);
                    }
                }
                _ => {}
            }
        }
    }
}

/// Blocks until the session ends. Keys go in as they are, which is what mode 9001 is for.
pub fn attach_console(session: Arc<TerminalSession>) -> io::Result<()> {
    let Handles { input, output } = handles();
    let (mut input_mode, mut output_mode) = (0, 0);
    unsafe {
        SetConsoleOutputCP(UTF8);
        GetConsoleMode(input, &mut input_mode);
        GetConsoleMode(output, &mut output_mode);
        SetConsoleMode(output, output_mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        SetConsoleMode(input, ENABLE_WINDOW_INPUT);
    }
    let reader = session.clone();
    let input_address = input as usize;
    // Stays blocked in the console read after the session ends, and goes with the process.
    thread::Builder::new()
        .name("console-input".to_string())
        .spawn(move || forward_input(reader, input_address as HANDLE))?;

    let mut stdout = io::stdout();
    write!(stdout, "\x1b[?1049h\x1b[2J")?;
    let mut painted = 0;
    while !session.is_closed() {
        let screen = session.screen();
        if screen.version != painted {
            painted = screen.version;
            stdout.write_all(paint(&screen).as_bytes())?;
            stdout.flush()?;
        }
        thread::sleep(PAINT_INTERVAL);
    }
    write!(stdout, "\x1b[0m\x1b[?1049l\x1b[?25h")?;
    stdout.flush()?;
    unsafe {
        SetConsoleMode(input, input_mode);
        SetConsoleMode(output, output_mode);
    }
    Ok(())
}

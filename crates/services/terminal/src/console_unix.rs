//! A session drawn into the console of this process, for trying the core before the terminal
//! panel exists. The renderer is crude on purpose: it repaints the whole screen.

use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::screen::{Cell, CellColor, CellFlags, Screen};
use crate::session::TerminalSession;

const PAINT_INTERVAL: Duration = Duration::from_millis(33);

/// The size of the console window, for spawning a session that fills it.
pub fn console_size() -> Option<(u16, u16)> {
    let mut size: libc::winsize = unsafe { std::mem::zeroed() };
    // SAFETY: the descriptor is the standard output and the structure is writable.
    let done = unsafe { libc::ioctl(libc::STDOUT_FILENO, libc::TIOCGWINSZ, &mut size) };
    (done == 0 && size.ws_col > 0 && size.ws_row > 0).then_some((size.ws_col, size.ws_row))
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

/// The original terminal settings, put back when the session ends.
struct RawTerminal(libc::termios);

impl RawTerminal {
    fn enter() -> io::Result<Self> {
        // SAFETY: a plain query of the terminal of this process.
        let mut original: libc::termios = unsafe { std::mem::zeroed() };
        if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut original) } != 0 {
            return Err(io::Error::last_os_error());
        }
        let mut raw = original;
        // SAFETY: the structure was filled by the call above.
        unsafe { libc::cfmakeraw(&mut raw) };
        if unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) } != 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self(original))
    }
}

impl Drop for RawTerminal {
    fn drop(&mut self) {
        // SAFETY: the structure is the one the terminal had before.
        unsafe {
            libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.0);
        }
    }
}

/// Blocks until the session ends. Bytes of the keyboard go in as they are.
pub fn attach_console(session: Arc<TerminalSession>) -> io::Result<()> {
    let terminal = RawTerminal::enter()?;
    let reader = session.clone();
    thread::Builder::new()
        .name("console-input".to_string())
        .spawn(move || {
            let mut stdin = io::stdin();
            let mut buffer = [0u8; 4096];
            while let Ok(count @ 1..) = stdin.read(&mut buffer) {
                if reader.send(buffer[..count].to_vec()).is_err() {
                    return;
                }
            }
        })?;

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
    drop(terminal);
    Ok(())
}

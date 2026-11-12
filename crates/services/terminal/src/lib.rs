//! Terminal sessions in a pseudoterminal: the screen as immutable snapshots and input that
//! never blocks the caller. Nothing here knows which program runs inside.

#[cfg(windows)]
mod conpty;
#[cfg(unix)]
#[path = "pty_unix.rs"]
mod conpty;
#[cfg(windows)]
mod console;
#[cfg(unix)]
#[path = "console_unix.rs"]
mod console;
mod engine;
mod env;
mod keys;
mod screen;
mod session;
#[cfg(windows)]
mod shell;
#[cfg(unix)]
#[path = "shell_unix.rs"]
mod shell;

pub use conpty::{Backend, command_line};
pub use console::{attach_console, console_size};
pub use env::{TERM_VARS, allowlisted, inherited};
pub use keys::{Key, KeyEvent, Mods, ShiftEnter, encode};
pub use screen::{Cell, CellColor, CellFlags, Link, Modes, Screen};
pub use session::{InputError, SpawnSpec, TerminalEvent, TerminalSession};
pub use shell::default_shell;

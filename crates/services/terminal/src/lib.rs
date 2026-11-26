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
mod dialect;
mod engine;
mod env;
mod keys;
mod screen;
mod search;
mod session;
#[cfg(windows)]
mod shell;
#[cfg(unix)]
#[path = "shell_unix.rs"]
mod shell;
mod shells;

pub use conpty::{Backend, command_line};
pub use console::{attach_console, console_size};
pub use dialect::{Dialect, QuoteError, shell_line};
pub use env::{TERM_VARS, allowlisted, inherited};
pub use keys::{Key, KeyEvent, Mods, ShiftEnter, encode};
pub use screen::{Cell, CellColor, CellFlags, Link, Modes, Screen, SearchMatch, SearchResults};
pub use session::{InputError, SearchQuery, SpawnSpec, TerminalEvent, TerminalSession};
pub use shell::{available_shells, default_shell};
pub use shells::Shell;

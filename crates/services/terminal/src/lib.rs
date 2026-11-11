//! Terminal sessions on ConPTY: a program in a pseudoconsole, the screen as immutable snapshots
//! and input that never blocks the caller. Nothing here knows which program runs inside.

mod conpty;
mod console;
mod engine;
mod env;
mod keys;
mod screen;
mod session;

pub use conpty::{Backend, command_line};
pub use console::{attach_console, console_size};
pub use env::{TERM_VARS, allowlisted, inherited};
pub use keys::{Key, KeyEvent, Mods, ShiftEnter, encode};
pub use screen::{Cell, CellColor, CellFlags, Link, Modes, Screen};
pub use session::{InputError, SpawnSpec, TerminalEvent, TerminalSession};

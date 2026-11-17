//! A native TUI harness as an agent session: the terminal is its surface, and the IDE types into
//! it only when the input box is on screen.

mod gate;
mod launch;
mod runtime;
mod session;

pub use gate::input_box_visible;
pub use launch::{Launch, MCP_SERVER_NAME, TOKEN_VARIABLE, launch_arguments};
pub use runtime::PtyRuntime;

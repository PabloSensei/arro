//! Starts processes so that no process outlives its owner.
//!
//! ```no_run
//! # fn main() -> std::io::Result<()> {
//! let mut server = proc_sup::ProcessSpec::new("server").arg("--stdio").start()?;
//! let stdout = server.take_stdout();
//! server.stop()?;
//! # Ok(())
//! # }
//! ```

mod capture;
mod input;
mod process;

pub use capture::{CaptureError, Captured, capture};
pub use input::{Input, InputWriter};
pub use process::{Process, ProcessClass, ProcessSpec};

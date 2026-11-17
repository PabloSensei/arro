/// An error that crosses crates and threads. A crate whose callers tell its errors apart
/// defines its own type instead.
pub type Error = Box<dyn std::error::Error + Send + Sync>;

pub type Result<T = (), E = Error> = std::result::Result<T, E>;

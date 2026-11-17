//! For tests only: a local stand-in for the model APIs and throwaway homes, so that a real harness
//! runs real turns with no login, no network and no spend. Never a dependency of a product crate.

mod fake_tools;
mod mock_api;
mod sandbox;

pub use fake_tools::FakeTools;
pub use mock_api::{FAKE_TOKEN, MOCK_MODEL, MockApi};
pub use sandbox::{Sandbox, claude_env, codex_env};

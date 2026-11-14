//! The completion sources of the open consoles, by the address of their file.
//!
//! The window registers the source when it starts a helper and removes it when the helper
//! ends. The editor only knows the address of the text it completes.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use crate::protocol::CompletionItem;
use crate::session::CompletionSource;

/// The sources of the consoles that are open now.
#[derive(Default)]
pub struct Hub {
    sources: Mutex<HashMap<String, Arc<dyn CompletionSource>>>,
}

impl Hub {
    /// Makes `source` the one that answers for the text at `uri`.
    pub fn register(&self, uri: impl Into<String>, source: Arc<dyn CompletionSource>) {
        if let Ok(mut sources) = self.sources.lock() {
            sources.insert(uri.into(), source);
        }
    }

    /// Forgets the source of a console that has ended.
    pub fn unregister(&self, uri: &str) {
        if let Ok(mut sources) = self.sources.lock() {
            sources.remove(uri);
        }
    }

    /// Asks the console of `uri` for its continuations. `None` when no console is open.
    pub fn complete(
        &self,
        uri: &str,
        text: &str,
        cursor: usize,
        timeout: Duration,
    ) -> Option<Result<Vec<CompletionItem>, String>> {
        let source = self.sources.lock().ok()?.get(uri).cloned()?;
        Some(source.complete(text, cursor, timeout))
    }
}

/// The hub of the application. The window and the editor share it; a test starts with an
/// empty one when no other test holds it.
pub fn hub() -> &'static Hub {
    static HUB: OnceLock<Hub> = OnceLock::new();
    HUB.get_or_init(Hub::default)
}

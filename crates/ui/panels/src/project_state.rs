//! A short stable name for the state of one project, shared by the windows that keep
//! something per project in the state folder or in the store.

use std::path::Path;

pub fn project_key(root: &Path) -> String {
    let text = root.to_string_lossy().to_lowercase();
    let hash = text.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ byte as u64).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}")
}

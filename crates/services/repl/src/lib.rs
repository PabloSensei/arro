//! The console helpers of the window, their protocol and the completion they answer.
//!
//! A console is one helper process: it runs the fragments of a text and prints what they
//! write. The window owns the session; the editor asks the same session for completion
//! through [`CompletionSource`]. This crate has no UI and knows no language: the language
//! layer says which modules the window offers and how to start their helper.

mod hub;
mod protocol;
mod session;

pub use hub::{Hub, hub};
pub use protocol::{
    Command, CompletionItem, Diagnostic, Event, Exception, FragmentKind, MAX_VALUE_CHARS, PROTOCOL,
};
pub use session::{COMPLETION_TIMEOUT, CompletionSource, ConsoleSession, Launch, Session};

use std::path::Path;

/// What the language layer adds to the console window: the modules whose consoles the
/// window offers and the way to start the helper of one.
pub trait Support: Send + Sync {
    /// The modules of the project, in the order the selector shows them.
    fn modules(&self) -> Vec<String>;

    /// The program of one module, with the files it needs. It carries no user data.
    fn launch(&self, module: &str) -> Result<Launch, String>;
}

/// A folder that keeps the identity of a module in a path and cannot leave it: letters,
/// digits and `-` stay, every other byte becomes `_` and two hex digits, so the module id
/// can be read back from the folder.
pub fn folder_name(module: &str) -> String {
    let mut name = String::new();
    for byte in module.bytes() {
        if byte.is_ascii_alphanumeric() || byte == b'-' {
            name.push(byte as char);
        } else {
            name.push('_');
            name.push_str(&format!("{byte:02x}"));
        }
    }
    if name.is_empty() {
        "default".to_owned()
    } else {
        name
    }
}

/// The module id a folder made by [`folder_name`] keeps, when it is one.
pub fn module_from_folder(folder: &str) -> Option<String> {
    if folder.is_empty() {
        return None;
    }
    if folder == "default" {
        return Some("default".to_owned());
    }
    let bytes = folder.as_bytes();
    let mut module = Vec::with_capacity(bytes.len());
    let mut at = 0;
    while at < bytes.len() {
        if bytes[at] == b'_' {
            let hex = folder.get(at + 1..at + 3)?;
            module.push(u8::from_str_radix(hex, 16).ok()?);
            at += 3;
        } else {
            module.push(bytes[at]);
            at += 1;
        }
    }
    String::from_utf8(module).ok()
}

/// The module a file of the console folder belongs to, when it is named as `file`.
pub fn module_of_folder(root: &Path, path: &Path, file: &str) -> Option<String> {
    let name = path.file_name()?;
    if !name.eq_ignore_ascii_case(file) {
        return None;
    }
    let (root, path) = match (root.canonicalize(), path.canonicalize()) {
        (Ok(root), Ok(path)) => (root, path),
        _ => (root.to_path_buf(), path.to_path_buf()),
    };
    let relative = path.strip_prefix(&root).ok()?;
    let mut parts = relative.components();
    let std::path::Component::Normal(folder) = parts.next()? else {
        return None;
    };
    match parts.next() {
        Some(std::path::Component::Normal(leaf))
            if leaf
                .to_str()
                .is_some_and(|leaf| leaf.eq_ignore_ascii_case(file)) =>
        {
            module_from_folder(folder.to_str()?)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_module_name_cannot_leave_its_folder() {
        assert_eq!(folder_name("../old module"), "_2e_2e_2fold_20module");
        assert_eq!(folder_name(""), "default");
        assert_eq!(folder_name("com.example").as_str(), "com_2eexample");
        assert_eq!(
            module_from_folder(&folder_name("com.example")).as_deref(),
            Some("com.example")
        );
        assert_eq!(module_from_folder("bad_zz"), None);
    }
}

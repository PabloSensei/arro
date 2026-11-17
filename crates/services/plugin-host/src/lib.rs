//! User plugins: a folder with `plugin.toml` in `%APPDATA%/Arro/plugins` and, when it
//! has commands, a process that speaks JSON-RPC over stdio. A plugin may be written in
//! any language, and a crash of one does not take the IDE along. The protocol and the
//! manifest are described in docs/PLUGINS.md.
//!
//! The window calls [`install`] for the commands and keys, applies the themes,
//! runs [`PluginHost::execute`] off its thread and answers each [`HostRequest`].

mod host;
mod install;
mod manifest;
mod protocol;
pub mod rpc;
mod theme;

pub use host::{
    ContributedCommand, ContributedTheme, Plugin, PluginError, PluginHost, PluginSettings,
    PluginState, Timeouts, default_plugins_dir,
};
pub use install::install;
pub use manifest::{
    API_VERSION, CommandContribution, Contributes, KeyBinding, MANIFEST_FILE, Manifest,
    ManifestError, ThemeContribution,
};
pub use protocol::{ActiveDocument, HostRequest, IdeCall, MessageKind, Selection};
pub use theme::PluginTheme;

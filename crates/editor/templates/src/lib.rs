//! Live templates: an abbreviation or a postfix key turns into text with fields, Tab and
//! Shift+Tab walk the fields, a field typed in once shows up in its copies. The templates
//! and the syntax contexts they fit come from the language as data.

mod context;
mod expand;
mod session;
mod snippet;
mod template;
mod text;

pub use context::context_at;
pub use expand::{Expansion, Field, expand, surround};
pub use session::{Session, Step};
pub use snippet::snippet_expansion;
pub use template::{ANY, Kind, Template, TemplateLanguage};
pub use text::{postfix_at, receiver_start, word_before};

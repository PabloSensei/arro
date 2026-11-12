//! Live templates: an abbreviation or a postfix key turns into text with fields, Tab and
//! Shift+Tab walk the fields, a field typed in once shows up in its copies. The templates
//! and the syntax contexts they fit come from the language as data.

mod context;
mod expand;
mod file;
mod overrides;
mod session;
mod snippet;
mod template;
mod text;
mod variables;

pub use context::context_at;
pub use expand::{Expansion, Field, expand, surround};
pub use expand::{ExpansionSite, expand_fields, surround_fields};
pub use file::{
    FILE_TEMPLATE_VARIABLES, FileTemplateDefinition, render_file_template, today, user_name,
};
pub use overrides::ResolvedTemplate;
pub use overrides::{
    CustomTemplate, ExpandWith, LiveTemplateEdit, LoadedTemplates, TEMPLATES_FILE,
    TEMPLATES_VERSION, TemplateField, TemplateOverrides, TemplateView, group_of, id_of,
};
pub use session::{Session, Step};
pub use snippet::snippet_expansion;
pub use template::{ANY, Kind, Template, TemplateLanguage};
pub use text::{postfix_at, receiver_start, word_before};
pub use variables::{
    Declarations, DeclarationsProvider, EXPRESSIONS, Variables, declarations, expression_warning,
};

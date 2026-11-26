//! Run configurations another IDE stored in a project. The window offers them and
//! imports the ones the user takes; the language reads the files, so the window knows
//! nothing of their format.

use crate::RunConfig;

/// What one source of configurations of another IDE holds.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ForeignConfigs {
    /// The name of the other IDE, for the offer and the summary of an import.
    pub source: &'static str,
    /// The configurations that can be stored here, in the order the source lists them.
    pub configs: Vec<RunConfig>,
    /// The kinds that stay out, with how many of each.
    pub unsupported: Vec<(String, usize)>,
    /// Something was read but not kept, such as a macro of a path. The import window
    /// shows them.
    pub warnings: Vec<String>,
}

impl ForeignConfigs {
    /// Whether the source holds nothing at all: no configuration, no kind that stays
    /// out and no warning.
    pub fn is_empty(&self) -> bool {
        self.configs.is_empty() && self.unsupported.is_empty() && self.warnings.is_empty()
    }
}

/// The name of a configuration in the terms an import compares: the kind and the name.
pub fn identity(config: &RunConfig) -> String {
    format!("{}\u{1}{}", config.provider, config.name)
}

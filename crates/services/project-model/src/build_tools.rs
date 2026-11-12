//! How the build tools of a project start: where each lives, which settings file it
//! reads, and what it does on the network. The page that shows these settings names the
//! tools; this module only holds the values.

use serde::{Deserialize, Serialize};

/// The settings of one build tool.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildToolSettings {
    /// Where the tool is installed. Empty means the wrapper of the project, or the
    /// program found on `PATH`.
    pub home: String,
    /// The settings file the tool reads. Empty means the place the tool chooses.
    pub settings_file: String,
    /// The repository the settings file names. Read-only: it comes from that file.
    pub repository: String,
    /// The tool resolves dependencies only from its cache.
    pub offline: bool,
    /// Options for the JVM that starts the tool.
    pub vm_options: String,
    /// The tool refreshes moving versions of its dependencies.
    pub update_snapshots: bool,
    /// The folder where the tool keeps its own data. Empty means its default.
    pub user_home: String,
    /// The runtime that starts the tool. Empty means the runtime of the project.
    pub runtime_home: String,
}

impl BuildToolSettings {
    /// A configured path, or `None` when the setting is empty or only spaces.
    pub fn path(value: &str) -> Option<&str> {
        let value = value.trim();
        (!value.is_empty()).then_some(value)
    }

    /// The home of the tool, when the user set one.
    pub fn home_path(&self) -> Option<&str> {
        Self::path(&self.home)
    }

    /// The settings file of the tool, when the user set one.
    pub fn settings_path(&self) -> Option<&str> {
        Self::path(&self.settings_file)
    }

    /// The user home of the tool, when the user set one.
    pub fn user_home_path(&self) -> Option<&str> {
        Self::path(&self.user_home)
    }

    /// The runtime of the tool, when the user set one.
    pub fn runtime_home_path(&self) -> Option<&str> {
        Self::path(&self.runtime_home)
    }
}

/// The settings of both build tools, in the order their Settings pages are shown.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildTools {
    pub first: BuildToolSettings,
    pub second: BuildToolSettings,
}

impl BuildTools {
    /// The settings of a tool by the index of its page.
    pub fn tool(&self, index: usize) -> &BuildToolSettings {
        if index == 1 {
            &self.second
        } else {
            &self.first
        }
    }

    /// The settings of a tool by the index of its page, to change.
    pub fn tool_mut(&mut self, index: usize) -> &mut BuildToolSettings {
        if index == 1 {
            &mut self.second
        } else {
            &mut self.first
        }
    }

    /// No network for dependency resolution, whichever tool imports the project.
    pub fn offline(&self) -> bool {
        self.first.offline || self.second.offline
    }
}

#[cfg(test)]
mod tests {
    use super::{BuildToolSettings, BuildTools};

    #[test]
    fn the_tools_are_found_by_the_index_of_their_page() {
        let mut tools = BuildTools::default();
        tools.tool_mut(0).home = "one".into();
        tools.tool_mut(1).home = "two".into();

        assert_eq!(tools.tool(0).home, "one");
        assert_eq!(tools.tool(1).home, "two");
        assert!(!tools.offline());
        tools.tool_mut(1).offline = true;
        assert!(tools.offline(), "either tool can turn the network off");
    }

    #[test]
    fn a_path_of_spaces_is_no_path() {
        let mut tool = BuildToolSettings::default();
        assert!(tool.home_path().is_none());
        tool.home = "  ".into();
        assert!(tool.home_path().is_none());
        tool.home = " C:/tool ".into();
        assert_eq!(tool.home_path(), Some("C:/tool"));
    }
}

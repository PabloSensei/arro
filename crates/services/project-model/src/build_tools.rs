//! How the build tools of a project start: where each lives, which settings file it
//! reads, and what it does on the network. The page that shows these settings names the
//! tools; this module only holds the values.

use serde::{Deserialize, Serialize};

/// Where the home of a build tool is taken from. `Auto` is the wrapper of the project,
/// the home named below, the copy the IDE downloaded and then the program on `PATH`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToolHome {
    /// The wrapper of the project, the configured home, the downloaded copy, `PATH`.
    #[default]
    Auto,
    /// The copy the IDE downloaded into its data folder.
    Bundled,
    /// The program on `PATH`.
    Path,
    /// The wrapper script of the project.
    Wrapper,
    /// The folder in [`BuildToolSettings::home`].
    Custom,
}

/// The settings of one build tool.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct BuildToolSettings {
    /// Where the tool is installed. Empty means the wrapper of the project, or the
    /// program found on `PATH`.
    pub home: String,
    /// Where the home of the tool comes from: the automatic order, the downloaded copy,
    /// `PATH`, the wrapper or the folder above.
    pub home_kind: ToolHome,
    /// Adds the folder of the downloaded tool to `PATH` of the terminals of the window.
    pub terminal_path: bool,
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
    /// The IDE downloads the distribution a project wrapper names before the tool runs.
    /// A file written before this switch existed reads as on.
    #[serde(default = "default_true")]
    pub download_wrapper: bool,
}

/// The value of a switch that a file written before the switch existed means.
fn default_true() -> bool {
    true
}

impl Default for BuildToolSettings {
    fn default() -> Self {
        Self {
            home: String::new(),
            home_kind: ToolHome::Auto,
            terminal_path: false,
            settings_file: String::new(),
            repository: String::new(),
            offline: false,
            vm_options: String::new(),
            update_snapshots: false,
            user_home: String::new(),
            runtime_home: String::new(),
            download_wrapper: true,
        }
    }
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

    /// The way the tool is found, as the settings mean it. A file written before the
    /// distribution choice existed has the automatic order, and a folder there means a
    /// local installation rather than the fallback: the folder wins over the wrapper.
    pub fn chosen_home_kind(&self) -> ToolHome {
        match self.home_kind {
            ToolHome::Auto if self.home_path().is_some() => ToolHome::Custom,
            kind => kind,
        }
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
    use super::{BuildToolSettings, BuildTools, ToolHome};

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

    #[test]
    fn a_file_before_the_distribution_choice_keeps_its_meaning() {
        let mut tool = BuildToolSettings::default();
        assert_eq!(
            tool.chosen_home_kind(),
            ToolHome::Auto,
            "an empty old value leaves the automatic order, which starts with the wrapper"
        );
        assert!(tool.download_wrapper, "the download is on by default");

        tool.home = "C:/tool-dist".into();
        assert_eq!(
            tool.chosen_home_kind(),
            ToolHome::Custom,
            "an old folder is a local installation"
        );

        tool.home_kind = ToolHome::Bundled;
        tool.home = String::new();
        assert_eq!(
            tool.chosen_home_kind(),
            ToolHome::Bundled,
            "a named kind does not depend on the old folder"
        );
    }

    #[test]
    fn an_old_file_reads_the_download_switch_as_on() {
        let tool: BuildToolSettings = serde_json::from_str(r#"{"home":"C:/tool-dist"}"#).unwrap();

        assert!(tool.download_wrapper);
        assert_eq!(tool.chosen_home_kind(), ToolHome::Custom);
        let off: BuildToolSettings = serde_json::from_str(r#"{"download_wrapper":false}"#).unwrap();
        assert!(!off.download_wrapper);
    }
}

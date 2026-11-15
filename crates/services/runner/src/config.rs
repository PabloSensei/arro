use std::collections::BTreeMap;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base::Result;
use proc_sup::ProcessSpec;
use project_model::ProjectModel;
use serde::{Deserialize, Serialize};
use toml::Table;

/// The folder of the project that keeps the IDE's own files.
const METADATA_FOLDER: &str = ".arro";
/// The stored run configurations, in the project and under git.
const CONFIG_FILE: &str = "run.toml";

/// The provider of a configuration that starts several other configurations at once.
pub const COMPOUND_PROVIDER: &str = "compound";

/// One entry of the run list. `settings` is what the provider asked to keep.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RunConfig {
    pub name: String,
    pub provider: String,
    /// Build the project first. Only the tool knows about code generators and processors.
    /// A configuration with an explicit [`RunConfig::before_launch`] list keeps the build
    /// in that list instead.
    #[serde(default = "assemble_by_default")]
    pub assemble_first: bool,
    /// The steps before the program, in order. `None` is a configuration written before
    /// the list existed: it builds when `assemble_first` is set. An empty list runs no
    /// step at all, so a removed build stays removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub before_launch: Option<Vec<BeforeLaunchStep>>,
    /// A second Run of this configuration opens another tab instead of asking to stop
    /// the one that runs.
    #[serde(default)]
    pub allow_multiple_instances: bool,
    /// Variables read from a file before the program starts. Values set by the
    /// configuration itself win over the file.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub environment_file: Option<PathBuf>,
    /// The other configurations this one starts, for [`COMPOUND_PROVIDER`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub compound: Vec<String>,
    #[serde(default)]
    pub settings: Table,
}

fn assemble_by_default() -> bool {
    true
}

impl RunConfig {
    /// The before launch steps, with the old `assemble_first` flag as a build step.
    pub fn before_launch_steps(&self) -> Vec<BeforeLaunchStep> {
        match &self.before_launch {
            Some(steps) => steps.clone(),
            None if self.assemble_first => vec![BeforeLaunchStep::Build],
            None => Vec::new(),
        }
    }

    /// Whether this configuration starts other configurations instead of a program.
    pub fn is_compound(&self) -> bool {
        self.provider == COMPOUND_PROVIDER
    }
}

/// A step that runs before the program of a configuration.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BeforeLaunchStep {
    /// Build the project with its build tool.
    Build,
    /// A goal or task line of the build tool.
    Task { target: String },
    /// Another stored configuration, run to its end first.
    Configuration { name: String },
}

impl BeforeLaunchStep {
    /// A short name for the console and the list of the dialog.
    pub fn label(&self) -> String {
        match self {
            Self::Build => "Build".to_owned(),
            Self::Task { target } => format!("Task {target}"),
            Self::Configuration { name } => format!("Run {name}"),
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct ConfigFile {
    #[serde(default, rename = "run")]
    configs: Vec<RunConfig>,
}

/// A missing file is an empty list, a broken one is an error: overwriting it would lose it.
pub fn load_configs(file: &Path) -> Result<Vec<RunConfig>> {
    match fs::read_to_string(file) {
        Ok(text) => Ok(toml::from_str::<ConfigFile>(&text)?.configs),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(error.into()),
    }
}

pub fn save_configs(file: &Path, configs: &[RunConfig]) -> Result {
    let configs = configs.to_vec();
    if let Some(directory) = file.parent() {
        fs::create_dir_all(directory)?;
    }
    fs::write(file, toml::to_string_pretty(&ConfigFile { configs })?)?;
    Ok(())
}

/// Where the stored configurations of a project live.
pub fn configs_file(root: &Path) -> PathBuf {
    root.join(METADATA_FOLDER).join(CONFIG_FILE)
}

/// The stored configurations first, then the discovered ones whose names are not stored.
/// A stored configuration wins over a discovered one of the same name.
pub fn merge_configs(stored: &[RunConfig], discovered: &[RunConfig]) -> Vec<RunConfig> {
    let stored_names: HashSet<&str> = stored.iter().map(|config| config.name.as_str()).collect();
    let mut merged = stored.to_vec();
    merged.extend(
        discovered
            .iter()
            .filter(|config| !stored_names.contains(config.name.as_str()))
            .cloned(),
    );
    merged
}

/// Splits a line of arguments the way IDEA does: spaces separate arguments unless they
/// are inside double quotes, `\"` inside or outside quotes is a quote, `\\` is a
/// backslash. Quotes of their own mark an empty argument.
pub fn split_arguments(text: &str) -> Vec<String> {
    let mut arguments = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut started = false;
    let mut characters = text.chars();
    while let Some(character) = characters.next() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            '\\' => {
                let next = characters.next();
                match next {
                    Some(escaped @ ('"' | '\\')) => {
                        current.push(escaped);
                        started = true;
                    }
                    Some(other) => {
                        current.push('\\');
                        current.push(other);
                        started = true;
                    }
                    None => {
                        current.push('\\');
                        started = true;
                    }
                }
            }
            character if character.is_whitespace() && !quoted => {
                if started {
                    arguments.push(std::mem::take(&mut current));
                    started = false;
                }
            }
            character => {
                current.push(character);
                started = true;
            }
        }
    }
    if started {
        arguments.push(current);
    }
    arguments
}

/// Turns arguments back into a line [`split_arguments`] gives them back from. An
/// argument with a space or a quote is kept in quotes.
pub fn join_arguments(arguments: &[String]) -> String {
    let mut line = String::new();
    for (index, argument) in arguments.iter().enumerate() {
        if index > 0 {
            line.push(' ');
        }
        let needs_quotes = argument.is_empty()
            || argument
                .chars()
                .any(|character| character.is_whitespace() || matches!(character, '"' | '\\'));
        if !needs_quotes {
            line.push_str(argument);
            continue;
        }
        line.push('"');
        for character in argument.chars() {
            if matches!(character, '"' | '\\') {
                line.push('\\');
            }
            line.push(character);
        }
        line.push('"');
    }
    line
}

/// Reads `A=1;B=2`: `;` separates the variables, the first `=` of an entry splits the
/// name from the value, and the value may hold `=` too. Empty entries are skipped.
pub fn parse_environment(text: &str) -> BTreeMap<String, String> {
    let mut variables = BTreeMap::new();
    for entry in text.split(';') {
        let Some((name, value)) = entry.split_once('=') else {
            continue;
        };
        let name = name.trim();
        if name.is_empty() {
            continue;
        }
        variables.insert(name.to_owned(), value.to_owned());
    }
    variables
}

/// The text form of the variables, as [`parse_environment`] reads them back.
pub fn format_environment(variables: &BTreeMap<String, String>) -> String {
    variables
        .iter()
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join(";")
}

/// What a field of a configuration looks like in the dialog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConfigFieldKind {
    Text,
    /// A line of arguments, split with [`split_arguments`].
    Arguments,
    /// `A=1;B=2`, read with [`parse_environment`].
    Env,
    Path,
    Bool,
    Choice(Vec<String>),
    /// Text the dialog shows with a button that copies it; the user cannot change it.
    ReadOnly,
}

/// One editable field of a configuration. The provider reads and writes the value in
/// `settings`, so the dialog knows nothing about the language.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigField {
    /// The key the provider uses in `settings`.
    pub key: &'static str,
    pub label: &'static str,
    pub kind: ConfigFieldKind,
    pub hint: Option<&'static str>,
}

impl ConfigField {
    pub fn text(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            kind: ConfigFieldKind::Text,
            hint: None,
        }
    }

    pub fn arguments(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            kind: ConfigFieldKind::Arguments,
            hint: None,
        }
    }

    pub fn env(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            kind: ConfigFieldKind::Env,
            hint: None,
        }
    }

    pub fn path(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            kind: ConfigFieldKind::Path,
            hint: None,
        }
    }

    /// A line the user copies rather than edits, such as the command that starts a
    /// program for the debugger elsewhere.
    pub fn read_only(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            kind: ConfigFieldKind::ReadOnly,
            hint: None,
        }
    }

    pub fn flag(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            kind: ConfigFieldKind::Bool,
            hint: None,
        }
    }

    pub fn choice(key: &'static str, label: &'static str, choices: Vec<String>) -> Self {
        Self {
            key,
            label,
            kind: ConfigFieldKind::Choice(choices),
            hint: None,
        }
    }

    pub fn hint(mut self, hint: &'static str) -> Self {
        self.hint = Some(hint);
        self
    }
}

/// One row of the Add menu of the dialog, a kind of configuration a provider creates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConfigTemplate {
    pub id: &'static str,
    pub name: &'static str,
}

/// A place in a source file that starts a program: the line and the configuration that
/// runs it. The provider reads the text of the file and says which places can be started.
#[derive(Clone, Debug, PartialEq)]
pub struct LaunchPoint {
    /// One-based.
    pub line: u32,
    pub config: RunConfig,
}

/// What it takes to run one configuration.
pub struct RunPlan {
    /// Runs first, the program starts only if this succeeds.
    pub before: Option<ProcessSpec>,
    pub program: ProcessSpec,
    /// How long a step gets to finish after Ctrl+C.
    pub grace: Duration,
}

/// A debugger that attaches to a program somebody else started, such as a JVM waiting
/// for the debugger on a remote machine. A configuration of this kind has no command.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AttachPlan {
    pub host: String,
    pub port: u16,
    /// Names the program in the Debug window.
    pub name: String,
    /// The project name the adapter knows, when it needs one.
    pub project: Option<String>,
}

/// Turns a stored configuration into a command, and finds what can be run at all.
pub trait RunConfigProvider: Send + Sync {
    fn id(&self) -> &'static str;
    fn discover(&self, model: &ProjectModel) -> Result<Vec<RunConfig>>;
    fn plan(&self, config: &RunConfig, model: &ProjectModel) -> Result<RunPlan>;

    /// The places in `text`, one source file, that start a program, for the gutter marks
    /// and the keys that run at the caret. A language that starts no file leaves it empty.
    fn launch_points(&self, _text: &str, _file: &Path, _model: &ProjectModel) -> Vec<LaunchPoint> {
        Vec::new()
    }

    /// A configuration that runs a named task of a module, when the provider makes such
    /// configurations. The build tool window asks for one.
    fn task_config(&self, _model: &ProjectModel, _module: &str, _task: &str) -> Option<RunConfig> {
        None
    }

    /// The group of the Services window this configuration is listed under. `None` keeps
    /// the configuration out of the window: not every program is a service.
    fn service_group(&self, _config: &RunConfig) -> Option<crate::ServiceGroup> {
        None
    }

    /// Whether [`RunConfigProvider::coverage_plan`] makes a plan. The menus offer Run
    /// with Coverage only for a provider that has it.
    fn supports_coverage(&self) -> bool {
        false
    }

    /// The same program under a debugger. Providers that cannot debug keep the
    /// plain plan and say so by their name.
    fn debug_plan(&self, config: &RunConfig, model: &ProjectModel) -> Result<RunPlan> {
        self.plan(config, model)
    }

    /// The address a debugger attaches to, for a configuration that starts nothing and
    /// waits for a program somebody else runs. `None` for a configuration that starts.
    fn attach_plan(
        &self,
        _config: &RunConfig,
        _model: &ProjectModel,
    ) -> Result<Option<AttachPlan>> {
        Ok(None)
    }

    /// The same program with a coverage agent, and the file the agent writes. A
    /// provider without coverage keeps the error of this default.
    fn coverage_plan(
        &self,
        _config: &RunConfig,
        _model: &ProjectModel,
    ) -> Result<crate::CoveragePlan> {
        Err(format!("{} cannot collect coverage", self.id()).into())
    }

    /// Reads the data of the last run of [`RunConfigProvider::coverage_plan`].
    fn coverage_report(
        &self,
        _exec: &Path,
        _model: &ProjectModel,
    ) -> Result<lang_api::CoverageReport> {
        Err(format!("{} cannot read coverage", self.id()).into())
    }

    /// Whether [`RunConfigProvider::profile_plan`] makes a plan. The menus offer Profile
    /// only for a provider that has it.
    fn supports_profiler(&self) -> bool {
        false
    }

    /// The same program with its samples recorded, and the file the runtime writes. A
    /// provider that cannot record keeps the error of this default.
    fn profile_plan(
        &self,
        _config: &RunConfig,
        _model: &ProjectModel,
    ) -> Result<crate::ProfilePlan> {
        Err(format!("{} cannot record a profile", self.id()).into())
    }

    /// Reads the recording of the last run of [`RunConfigProvider::profile_plan`], or a
    /// file the user opened.
    fn profile_snapshot(
        &self,
        _recording: &Path,
        _model: &ProjectModel,
    ) -> Result<lang_api::ProfileSnapshot> {
        Err(format!("{} cannot read a profile", self.id()).into())
    }

    /// Reads a recording with everything it holds: the CPU samples, the allocations and
    /// the classes they made. A provider that gives only the CPU tree keeps this default.
    fn profile_report(
        &self,
        recording: &Path,
        model: &ProjectModel,
    ) -> Result<lang_api::ProfileReport> {
        self.profile_snapshot(recording, model)
            .map(lang_api::ProfileReport::of)
    }

    /// The recordings of this provider still on disk, the newest first.
    fn profile_recordings(&self, _model: &ProjectModel) -> Result<Vec<lang_api::ProfileRecording>> {
        Ok(Vec::new())
    }

    /// Every file one profile run left: a runtime may write one recording per JVM.
    fn profile_run_recordings(&self, recording: &Path) -> Vec<PathBuf> {
        vec![recording.to_owned()]
    }

    /// The fields the dialog edits, in the order it shows them. A field that does not
    /// apply to this configuration, such as one of a framework the program does not use,
    /// is left out.
    fn fields(&self, _config: &RunConfig) -> Vec<ConfigField> {
        Vec::new()
    }

    /// The live sections of the application this configuration starts, when it is one
    /// that publishes them. The language reads them; the window only shows them.
    fn live_application(
        &self,
        _config: &RunConfig,
        _model: &ProjectModel,
    ) -> Option<Arc<dyn crate::LiveApplication>> {
        None
    }

    /// The text of a field, as the dialog shows it.
    fn read_field(&self, _config: &RunConfig, _key: &str) -> String {
        String::new()
    }

    /// Stores what the user typed in `settings`.
    fn write_field(&self, _config: &mut RunConfig, _key: &str, _value: &str) {}

    /// The kinds of configuration the Add button offers.
    fn templates(&self) -> Vec<ConfigTemplate> {
        Vec::new()
    }

    /// A fresh configuration of a template, named the way the provider suggests it.
    fn create_config(&self, template: &str, _model: &ProjectModel) -> Result<RunConfig> {
        Err(format!("{} cannot create {template}", self.id()).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_file(name: &str) -> std::path::PathBuf {
        test_support::scratch("runner-config").join(name)
    }

    #[test]
    fn configurations_survive_a_round_trip() {
        let file = temp_file("round-trip.toml");
        let mut settings = Table::new();
        settings.insert("entry".into(), "demo.Main".into());
        let configs = vec![RunConfig {
            name: "Демо".into(),
            provider: "any".into(),
            assemble_first: false,
            settings,
            ..RunConfig::default()
        }];

        save_configs(&file, &configs).unwrap();

        assert_eq!(load_configs(&file).unwrap(), configs);
    }

    #[test]
    fn a_missing_file_is_an_empty_list() {
        assert!(load_configs(&temp_file("missing.toml")).unwrap().is_empty());
    }

    #[test]
    fn a_broken_file_is_an_error() {
        let file = temp_file("broken.toml");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "not [ toml").unwrap();

        assert!(load_configs(&file).is_err());
    }

    #[test]
    fn a_hand_written_entry_needs_only_the_basics() {
        let text = "[[run]]\nname = \"App\"\nprovider = \"any\"\n";

        let configs = toml::from_str::<ConfigFile>(text).unwrap().configs;

        assert!(configs[0].assemble_first);
        assert!(configs[0].settings.is_empty());
    }

    #[test]
    fn fields_of_a_configuration_survive_a_round_trip() {
        let file = temp_file("fields.toml");
        let mut settings = Table::new();
        settings.insert("entry".into(), "demo.Main".into());
        settings.insert(
            "args".into(),
            toml::Value::Array(vec!["a b".into(), "".into(), "c".into()]),
        );
        let configs = vec![RunConfig {
            name: "App".into(),
            provider: "any".into(),
            assemble_first: false,
            settings,
            ..RunConfig::default()
        }];

        save_configs(&file, &configs).unwrap();
        let loaded = load_configs(&file).unwrap();

        assert_eq!(loaded, configs);
        let stored = loaded[0].settings["args"].as_array().unwrap();
        assert_eq!(stored[0].as_str(), Some("a b"));
        assert_eq!(stored[1].as_str(), Some(""));
        let _ = fs::remove_file(&file);
    }

    #[test]
    fn configurations_with_before_launch_and_compound_survive_a_round_trip() {
        let file = temp_file("before-launch.toml");
        let mut settings = Table::new();
        settings.insert("entry".into(), "demo.Main".into());
        let configs = vec![
            RunConfig {
                name: "App".into(),
                provider: "any".into(),
                assemble_first: false,
                before_launch: Some(vec![
                    BeforeLaunchStep::Build,
                    BeforeLaunchStep::Task {
                        target: "package".into(),
                    },
                    BeforeLaunchStep::Configuration {
                        name: "Other".into(),
                    },
                ]),
                allow_multiple_instances: true,
                environment_file: Some(".env.local".into()),
                compound: Vec::new(),
                settings,
            },
            RunConfig {
                name: "All".into(),
                provider: COMPOUND_PROVIDER.into(),
                assemble_first: false,
                before_launch: Some(Vec::new()),
                allow_multiple_instances: false,
                environment_file: None,
                compound: vec!["App".into(), "Other".into()],
                settings: Table::new(),
            },
        ];

        save_configs(&file, &configs).unwrap();
        let loaded = load_configs(&file).unwrap();

        assert_eq!(loaded, configs);
        assert_eq!(loaded[0].before_launch_steps().len(), 3);
        assert!(loaded[1].is_compound());
        assert!(loaded[1].before_launch_steps().is_empty());
    }

    #[test]
    fn an_old_configuration_builds_through_its_flag() {
        let text = "[[run]]\nname = \"App\"\nprovider = \"any\"\n";
        let config = toml::from_str::<ConfigFile>(text).unwrap().configs[0].clone();

        assert_eq!(config.before_launch_steps(), [BeforeLaunchStep::Build]);
        assert!(!config.allow_multiple_instances);
        assert!(config.environment_file.is_none());
    }

    #[test]
    fn an_empty_list_removes_the_build() {
        let config = RunConfig {
            assemble_first: true,
            before_launch: Some(Vec::new()),
            ..RunConfig::default()
        };

        assert!(config.before_launch_steps().is_empty());
    }

    #[test]
    fn steps_are_named_for_the_console() {
        assert_eq!(BeforeLaunchStep::Build.label(), "Build");
        assert_eq!(
            BeforeLaunchStep::Task {
                target: "package".into()
            }
            .label(),
            "Task package"
        );
        assert_eq!(
            BeforeLaunchStep::Configuration {
                name: "Other".into()
            }
            .label(),
            "Run Other"
        );
    }

    #[test]
    fn stored_configurations_come_first_and_discovered_ones_fill_the_rest() {
        let stored = vec![RunConfig {
            name: "App".into(),
            provider: "any".into(),
            assemble_first: false,
            settings: Table::new(),
            ..RunConfig::default()
        }];
        let discovered = vec![
            RunConfig {
                name: "App".into(),
                provider: "any".into(),
                assemble_first: true,
                settings: Table::new(),
                ..RunConfig::default()
            },
            RunConfig {
                name: "Other".into(),
                provider: "any".into(),
                assemble_first: true,
                settings: Table::new(),
                ..RunConfig::default()
            },
        ];

        let merged = merge_configs(&stored, &discovered);

        let names: Vec<&str> = merged.iter().map(|config| config.name.as_str()).collect();
        assert_eq!(names, vec!["App", "Other"]);
        assert!(!merged[0].assemble_first, "the stored one wins by name");
    }

    #[test]
    fn the_file_of_the_configurations_lives_under_the_project() {
        let file = configs_file(Path::new("project"));

        assert!(file.ends_with("run.toml"), "{file:?}");
        assert!(file.starts_with("project"), "{file:?}");
    }

    #[test]
    fn arguments_are_split_the_way_idea_does() {
        assert_eq!(split_arguments(""), Vec::<String>::new());
        assert_eq!(split_arguments("   "), Vec::<String>::new());
        assert_eq!(split_arguments("one two"), vec!["one", "two"]);
        assert_eq!(split_arguments("  one \t two  "), vec!["one", "two"]);
        assert_eq!(split_arguments("\"a b\" c"), vec!["a b", "c"]);
        assert_eq!(split_arguments("a\"b c\"d"), vec!["ab cd"]);
        assert_eq!(split_arguments("\"\""), vec![""]);
        assert_eq!(split_arguments("one \"\" two"), vec!["one", "", "two"]);
        assert_eq!(split_arguments("\"say \\\"hi\\\"\""), vec!["say \"hi\""]);
        assert_eq!(split_arguments("\"c:\\\\dir\""), vec!["c:\\dir"]);
        assert_eq!(split_arguments("back\\slash"), vec!["back\\slash"]);
    }

    #[test]
    fn joined_arguments_split_back_the_same() {
        let arguments = vec![
            "plain".to_owned(),
            "with space".to_owned(),
            "quote\"inside".to_owned(),
            String::new(),
            "back\\slash".to_owned(),
        ];

        let line = join_arguments(&arguments);

        assert_eq!(split_arguments(&line), arguments);
    }

    #[test]
    fn environment_variables_are_read_up_to_the_first_equals() {
        let variables = parse_environment("A=1;B=two=three; C = four ;;=empty;BAD");

        assert_eq!(variables.get("A").map(String::as_str), Some("1"));
        assert_eq!(variables.get("B").map(String::as_str), Some("two=three"));
        assert_eq!(variables.get("C").map(String::as_str), Some(" four "));
        assert_eq!(variables.len(), 3);
    }

    #[test]
    fn environment_variables_are_written_back_the_same() {
        let variables = parse_environment("A=1;B=2=3");

        let line = format_environment(&variables);

        assert_eq!(line, "A=1;B=2=3");
        assert_eq!(parse_environment(&line), variables);
    }
}

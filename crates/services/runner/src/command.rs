//! A configuration that runs a command line the user typed: the terminal sends the line
//! of its prompt here, and the window starts exactly the program it names.

use std::time::Duration;

use base::Result;
use proc_sup::ProcessSpec;
use project_model::ProjectModel;

use crate::{ConfigField, RunConfig, RunConfigProvider, RunPlan, split_arguments};

/// The provider of a configuration that keeps its command line.
pub const PROVIDER_ID: &str = "command-line";
/// The key of `settings` that holds the line.
pub const COMMAND: &str = "command";

/// Runs a command line as the user typed it, without a build step of the project.
pub struct CommandProvider;

impl CommandProvider {
    /// The temporary configuration of `line`, ready for [`crate::RunPlan`] of this
    /// provider. The line itself names the configuration in the list.
    pub fn config_of(line: &str) -> RunConfig {
        let mut settings = toml::Table::new();
        settings.insert(COMMAND.into(), line.to_owned().into());
        RunConfig {
            name: line.to_owned(),
            provider: PROVIDER_ID.to_owned(),
            // The line may be a build or test command itself: nothing runs before it.
            assemble_first: false,
            before_launch: Some(Vec::new()),
            settings,
            ..RunConfig::default()
        }
    }
}

/// The spec of the program and its arguments, starting in the root of the project.
#[cfg(not(windows))]
fn program_spec(program: &str, arguments: &[String], root: &std::path::Path) -> ProcessSpec {
    let mut spec = ProcessSpec::new(program).cwd(root);
    for argument in arguments {
        spec = spec.arg(argument);
    }
    spec
}

/// On Windows a command of a batch file such as a build wrapper has no extension in the
/// line of the prompt. The interpreter finds it through `PATHEXT`, and this does the
/// same, so the configuration starts the file the shell would.
#[cfg(windows)]
fn program_spec(program: &str, arguments: &[String], root: &std::path::Path) -> ProcessSpec {
    let program = script_of(program, root).unwrap_or_else(|| std::path::PathBuf::from(program));
    let mut spec = ProcessSpec::new(program).cwd(root);
    for argument in arguments {
        spec = spec.arg(argument);
    }
    spec
}

/// The file a command names, trying the extensions of the programs and of the scripts.
#[cfg(windows)]
fn script_of(program: &str, root: &std::path::Path) -> Option<std::path::PathBuf> {
    let path = std::path::Path::new(program);
    if path.extension().is_some() {
        return path.is_file().then(|| path.to_path_buf());
    }
    let mut bases = Vec::new();
    if path.is_absolute() || program.contains(['\\', '/']) {
        bases.push(if path.is_absolute() {
            path.to_path_buf()
        } else {
            root.join(path)
        });
    } else {
        bases.push(root.join(path));
        if let Some(paths) = std::env::var_os("PATH") {
            bases.extend(std::env::split_paths(&paths).map(|folder| folder.join(path)));
        }
    }
    for base in bases {
        for extension in ["exe", "com", "cmd", "bat"] {
            let candidate = base.with_extension(extension);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
        if base.is_file() {
            return Some(base);
        }
    }
    None
}

impl RunConfigProvider for CommandProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn group_name(&self, _config: &RunConfig) -> &'static str {
        "Shell Script"
    }

    fn discover(&self, _model: &ProjectModel) -> Result<Vec<RunConfig>> {
        Ok(Vec::new())
    }

    fn plan(&self, config: &RunConfig, model: &ProjectModel) -> Result<RunPlan> {
        let line = config
            .settings
            .get(COMMAND)
            .and_then(|value| value.as_str())
            .ok_or("The configuration names no command line")?;
        let arguments = split_arguments(line);
        let Some((program, arguments)) = arguments.split_first() else {
            return Err("The command line names no program".into());
        };
        Ok(RunPlan {
            before: None,
            program: program_spec(program, arguments, &model.root),
            grace: Duration::from_secs(5),
        })
    }

    fn fields(&self, _config: &RunConfig) -> Vec<ConfigField> {
        vec![ConfigField::text(COMMAND, "Command line")]
    }

    fn read_field(&self, config: &RunConfig, key: &str) -> String {
        match key {
            COMMAND => config
                .settings
                .get(COMMAND)
                .and_then(|value| value.as_str())
                .unwrap_or_default()
                .to_owned(),
            _ => String::new(),
        }
    }

    fn write_field(&self, config: &mut RunConfig, key: &str, value: &str) {
        if key == COMMAND {
            config
                .settings
                .insert(COMMAND.into(), value.to_owned().into());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{COMMAND, CommandProvider, PROVIDER_ID};
    use crate::{RunConfig, RunConfigProvider};
    use project_model::ProjectModel;

    fn model(root: &std::path::Path) -> ProjectModel {
        ProjectModel {
            root: root.to_path_buf(),
            resolved: true,
            ..ProjectModel::default()
        }
    }

    #[test]
    fn a_typed_line_becomes_the_program_and_its_arguments() {
        let root = test_support::scratch("runner-command");
        let config = CommandProvider::config_of("tool test --name \"a b\"");
        assert_eq!(config.provider, PROVIDER_ID);
        assert_eq!(config.name, "tool test --name \"a b\"");
        assert_eq!(
            config
                .settings
                .get(COMMAND)
                .and_then(|value| value.as_str()),
            Some("tool test --name \"a b\"")
        );
        assert!(
            config.before_launch_steps().is_empty(),
            "nothing runs first"
        );

        let plan = CommandProvider
            .plan(&config, &model(&root))
            .expect("the line plans");
        assert_eq!(plan.program.arguments(), ["test", "--name", "a b"]);
        assert_eq!(plan.program.working_dir(), Some(root.as_path()));
    }

    #[test]
    fn a_configuration_without_a_line_plans_nothing() {
        let root = test_support::scratch("runner-command-empty");
        let config = RunConfig {
            provider: PROVIDER_ID.to_owned(),
            ..RunConfig::default()
        };
        assert!(CommandProvider.plan(&config, &model(&root)).is_err());
    }

    /// A command of a batch file is named without its extension in the prompt, as the
    /// shell of Windows allows.
    #[cfg(windows)]
    #[test]
    fn a_bare_command_name_finds_its_script_in_the_project() {
        let root = test_support::scratch("runner-command-script");
        std::fs::write(root.join("tool.cmd"), "@echo off\r\n").unwrap();
        assert_eq!(super::script_of("tool", &root), Some(root.join("tool.cmd")));
    }
}

//! Run configurations of build tool tasks. The configuration keeps the module and the
//! task; the build tool makes the command when the configuration is planned.

use std::sync::Arc;

use base::Result;
use project_model::{BuildSystem, ProjectModel, TaskOptions};
use serde::{Deserialize, Serialize};
use toml::Table;

use crate::{ConfigField, RunConfig, RunConfigProvider, RunPlan};

pub const PROVIDER_ID: &str = "build-task";
const FIELD_MODULE: &str = "module";
const FIELD_TASK: &str = "task";
const FIELD_SKIP_TESTS: &str = "skip_tests";

/// The part of a task configuration that lives in its `settings`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct TaskTarget {
    module: String,
    task: String,
    skip_tests: bool,
}

/// Runs a task of the build tool, as the build tool window does.
pub struct BuildTaskProvider {
    tool: Arc<dyn BuildSystem>,
}

impl BuildTaskProvider {
    pub fn new(tool: Arc<dyn BuildSystem>) -> Self {
        Self { tool }
    }

    fn target(config: &RunConfig) -> TaskTarget {
        config.settings.clone().try_into().unwrap_or_default()
    }

    fn store(target: &TaskTarget) -> Result<Table> {
        Ok(Table::try_from(target.clone())?)
    }

    fn options(target: &TaskTarget) -> TaskOptions {
        TaskOptions {
            skip_tests: target.skip_tests,
        }
    }
}

impl RunConfigProvider for BuildTaskProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn discover(&self, _model: &ProjectModel) -> Result<Vec<RunConfig>> {
        Ok(Vec::new())
    }

    fn plan(&self, config: &RunConfig, model: &ProjectModel) -> Result<RunPlan> {
        let target = Self::target(config);
        let module = match target.module.is_empty() {
            true => None,
            false => Some(
                model
                    .module(&target.module)
                    .ok_or_else(|| format!("no module {}", target.module))?,
            ),
        };
        let program =
            self.tool
                .task_in(&model.root, module, &target.task, Self::options(&target))?;
        Ok(RunPlan {
            before: None,
            program,
            grace: std::time::Duration::from_secs(5),
        })
    }

    fn debug_plan(&self, config: &RunConfig, model: &ProjectModel) -> Result<RunPlan> {
        let target = Self::target(config);
        let module = match target.module.is_empty() {
            true => None,
            false => Some(
                model
                    .module(&target.module)
                    .ok_or_else(|| format!("no module {}", target.module))?,
            ),
        };
        let program =
            self.tool
                .debug_task_in(&model.root, module, &target.task, Self::options(&target))?;
        Ok(RunPlan {
            before: None,
            program,
            grace: std::time::Duration::from_secs(5),
        })
    }

    fn fields(&self, _config: &RunConfig) -> Vec<ConfigField> {
        vec![
            ConfigField::text(FIELD_MODULE, "Module").hint("The module of the project"),
            ConfigField::text(FIELD_TASK, "Task").hint("The name of the build tool task"),
            ConfigField::flag(FIELD_SKIP_TESTS, "Skip tests"),
        ]
    }

    fn read_field(&self, config: &RunConfig, key: &str) -> String {
        let target = Self::target(config);
        match key {
            FIELD_MODULE => target.module,
            FIELD_TASK => target.task,
            FIELD_SKIP_TESTS => target.skip_tests.to_string(),
            _ => String::new(),
        }
    }

    fn write_field(&self, config: &mut RunConfig, key: &str, value: &str) {
        let mut target = Self::target(config);
        match key {
            FIELD_MODULE => target.module = value.trim().to_owned(),
            FIELD_TASK => target.task = value.trim().to_owned(),
            FIELD_SKIP_TESTS => target.skip_tests = value.trim() == "true",
            _ => return,
        }
        if let Ok(settings) = Self::store(&target) {
            config.settings = settings;
        }
    }

    fn task_config(&self, model: &ProjectModel, module: &str, task: &str) -> Option<RunConfig> {
        let name = model
            .module(module)
            .map_or_else(|| module.to_owned(), |module| module.name.clone());
        let target = TaskTarget {
            module: module.to_owned(),
            task: task.to_owned(),
            skip_tests: false,
        };
        Some(RunConfig {
            name: format!("{name} [{task}]"),
            provider: PROVIDER_ID.into(),
            assemble_first: false,
            settings: Self::store(&target).ok()?,
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use base::Cancel;
    use proc_sup::ProcessSpec;
    use project_model::{Module, TaskOptions};

    use super::*;

    struct RecordTool;

    impl BuildSystem for RecordTool {
        fn detect(&self, _: &Path) -> bool {
            true
        }

        fn scan(&self, _: &Path) -> Result<ProjectModel> {
            Err("not used".into())
        }

        fn sync(&self, _: &Path, _: &Cancel) -> Result<ProjectModel> {
            Err("not used".into())
        }

        fn assemble(&self, _: &Path) -> Result<ProcessSpec> {
            Ok(ProcessSpec::new("assemble"))
        }

        fn task_in(
            &self,
            _: &Path,
            module: Option<&Module>,
            task: &str,
            options: TaskOptions,
        ) -> Result<ProcessSpec> {
            let mut spec = ProcessSpec::new("task").arg(task);
            if let Some(module) = module {
                spec = spec.arg(&module.id);
            }
            if options.skip_tests {
                spec = spec.arg("skip-tests");
            }
            Ok(spec)
        }
    }

    fn model() -> ProjectModel {
        ProjectModel {
            root: "project".into(),
            modules: vec![Module {
                id: "app".into(),
                name: "demo".into(),
                ..Module::default()
            }],
            resolved: true,
            ..ProjectModel::default()
        }
    }

    #[test]
    fn a_task_configuration_keeps_the_module_and_the_task() {
        let provider = BuildTaskProvider::new(Arc::new(RecordTool));
        let model = model();

        let config = provider.task_config(&model, "app", "test").unwrap();

        assert_eq!(config.name, "demo [test]");
        assert_eq!(provider.read_field(&config, FIELD_MODULE), "app");
        assert_eq!(provider.read_field(&config, FIELD_TASK), "test");
        assert!(!config.assemble_first);
        let plan = provider.plan(&config, &model).unwrap();
        assert_eq!(
            plan.program
                .arguments()
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned())
                .collect::<Vec<_>>(),
            ["test", "app"]
        );
    }

    #[test]
    fn the_dialog_fields_write_the_settings_back() {
        let provider = BuildTaskProvider::new(Arc::new(RecordTool));
        let mut config = provider.task_config(&model(), "app", "test").unwrap();

        provider.write_field(&mut config, FIELD_MODULE, "app");
        provider.write_field(&mut config, FIELD_TASK, "package");
        provider.write_field(&mut config, FIELD_SKIP_TESTS, "true");

        assert_eq!(provider.read_field(&config, FIELD_TASK), "package");
        assert_eq!(provider.read_field(&config, FIELD_SKIP_TESTS), "true");
    }
}

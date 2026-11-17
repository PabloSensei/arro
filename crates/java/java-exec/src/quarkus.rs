//! The Dev Mode of a Quarkus module as a run configuration. The build tool starts it, so
//! every change of a source reaches the running application without a restart.

use std::sync::Arc;

use base::Result;
use java_project::Framework;
use java_project::frameworks_of;
use project_model::{BuildSystem, Module, ProjectModel, TaskOptions};
use runner::{ConfigField, RunConfig, RunConfigProvider, RunPlan, ServiceGroup};
use serde::{Deserialize, Serialize};
use toml::Table;

pub const PROVIDER_ID: &str = "quarkus-dev";
const FIELD_MODULE: &str = "module";
const FIELD_TASK: &str = "task";
/// The group of the Services window a Quarkus application is listed under.
const SERVICE_GROUP: &str = "quarkus";
/// The dev task of the two build tools.
const MAVEN_DEV: &str = "quarkus:dev";
const GRADLE_DEV: &str = "quarkusDev";

/// The part of a Quarkus configuration that lives in its `settings`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct QuarkusTarget {
    module: String,
    task: String,
}

/// Starts the Dev Mode of a Quarkus module through its build tool.
pub struct QuarkusDevProvider {
    tool: Arc<dyn BuildSystem>,
}

impl QuarkusDevProvider {
    pub fn new(tool: Arc<dyn BuildSystem>) -> Self {
        Self { tool }
    }

    fn target(config: &RunConfig) -> QuarkusTarget {
        config.settings.clone().try_into().unwrap_or_default()
    }

    fn store(target: &QuarkusTarget) -> Result<Table> {
        Ok(Table::try_from(target.clone())?)
    }

    /// The task of the build tool of the project that starts the Dev Mode.
    fn task(model: &ProjectModel) -> &'static str {
        if model.tool.eq_ignore_ascii_case("gradle") {
            GRADLE_DEV
        } else {
            MAVEN_DEV
        }
    }

    fn module_of<'a>(model: &'a ProjectModel, target: &QuarkusTarget) -> Option<&'a Module> {
        model.module(&target.module)
    }
}

impl RunConfigProvider for QuarkusDevProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    /// One configuration per module whose dependencies name Quarkus.
    fn discover(&self, model: &ProjectModel) -> Result<Vec<RunConfig>> {
        if !model.resolved {
            return Ok(Vec::new());
        }
        let task = Self::task(model);
        let mut configs = Vec::new();
        for module in &model.modules {
            let uses_quarkus = model
                .dependencies_of(&module.id)
                .is_some_and(|dependencies| {
                    frameworks_of(dependencies).contains(Framework::Quarkus)
                });
            if !uses_quarkus {
                continue;
            }
            let name = if module.name.is_empty() {
                module.id.clone()
            } else {
                module.name.clone()
            };
            let target = QuarkusTarget {
                module: module.id.clone(),
                task: task.to_owned(),
            };
            configs.push(RunConfig {
                name: format!("{name} Quarkus Dev Mode"),
                provider: PROVIDER_ID.into(),
                // The dev task compiles the module itself.
                assemble_first: false,
                settings: Self::store(&target)?,
                ..RunConfig::default()
            });
        }
        Ok(configs)
    }

    fn plan(&self, config: &RunConfig, model: &ProjectModel) -> Result<RunPlan> {
        let target = Self::target(config);
        let module = Self::module_of(model, &target);
        let task = if target.task.is_empty() {
            Self::task(model)
        } else {
            &target.task
        };
        let program = self
            .tool
            .task_in(&model.root, module, task, TaskOptions::default())?;
        Ok(RunPlan {
            before: None,
            program,
            grace: std::time::Duration::from_secs(5),
        })
    }

    fn service_group(&self, _config: &RunConfig) -> Option<ServiceGroup> {
        Some(ServiceGroup {
            id: SERVICE_GROUP,
            title: "Quarkus",
            optional: false,
        })
    }

    fn fields(&self, _config: &RunConfig) -> Vec<ConfigField> {
        vec![
            ConfigField::text(FIELD_MODULE, "Module").hint("The module of the project"),
            ConfigField::text(FIELD_TASK, "Task").hint("The task that starts the Dev Mode"),
        ]
    }

    fn read_field(&self, config: &RunConfig, key: &str) -> String {
        let target = Self::target(config);
        match key {
            FIELD_MODULE => target.module,
            FIELD_TASK => target.task,
            _ => String::new(),
        }
    }

    fn write_field(&self, config: &mut RunConfig, key: &str, value: &str) {
        let mut target = Self::target(config);
        match key {
            FIELD_MODULE => target.module = value.trim().to_owned(),
            FIELD_TASK => target.task = value.trim().to_owned(),
            _ => return,
        }
        if let Ok(settings) = Self::store(&target) {
            config.settings = settings;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use base::Cancel;
    use proc_sup::ProcessSpec;
    use project_model::{Dependency, DependencyScope, ModuleDependencies, Unit};

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
            _: TaskOptions,
        ) -> Result<ProcessSpec> {
            let mut spec = ProcessSpec::new("task").arg(task);
            if let Some(module) = module {
                spec = spec.arg(&module.id);
            }
            Ok(spec)
        }
    }

    fn model(tool: &str) -> ProjectModel {
        ProjectModel {
            root: "project".into(),
            tool: tool.to_owned(),
            resolved: true,
            modules: vec![Module {
                id: "app".into(),
                name: "shop".into(),
                units: vec![Unit {
                    name: "main".into(),
                    ..Unit::default()
                }],
                ..Module::default()
            }],
            dependencies: vec![ModuleDependencies {
                module: "app".into(),
                scopes: vec![DependencyScope {
                    name: "compile".into(),
                    roots: vec![Dependency {
                        coordinates: "io.quarkus:quarkus-resteasy-reactive:3.9.0".into(),
                        ..Dependency::default()
                    }],
                }],
                ..ModuleDependencies::default()
            }],
            ..ProjectModel::default()
        }
    }

    fn provider() -> QuarkusDevProvider {
        QuarkusDevProvider::new(Arc::new(RecordTool))
    }

    fn arguments(plan: &RunPlan) -> Vec<String> {
        plan.program
            .arguments()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn a_quarkus_module_gets_a_dev_mode_configuration() {
        let provider = provider();
        let model = model("maven");

        let configs = provider.discover(&model).unwrap();

        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "shop Quarkus Dev Mode");
        assert_eq!(configs[0].provider, PROVIDER_ID);
        assert!(!configs[0].assemble_first);
        assert_eq!(provider.read_field(&configs[0], FIELD_TASK), "quarkus:dev");
        let plan = provider.plan(&configs[0], &model).unwrap();
        assert_eq!(arguments(&plan), ["quarkus:dev", "app"]);
        assert!(provider.service_group(&configs[0]).is_some());
    }

    #[test]
    fn the_gradle_module_starts_its_own_dev_task() {
        let provider = provider();
        let model = model("gradle");

        let config = provider.discover(&model).unwrap().remove(0);

        assert_eq!(provider.read_field(&config, FIELD_TASK), "quarkusDev");
        let plan = provider.plan(&config, &model).unwrap();
        assert_eq!(arguments(&plan), ["quarkusDev", "app"]);
    }

    #[test]
    fn a_project_without_quarkus_gets_no_configuration() {
        let provider = provider();
        let mut model = model("maven");
        model.dependencies.clear();

        assert!(provider.discover(&model).unwrap().is_empty());
    }
}

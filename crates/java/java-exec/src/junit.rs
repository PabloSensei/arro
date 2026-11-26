//! JUnit run configurations: a class, a method or a package of tests. The Tests window
//! runs them through the test provider of the project, the same path the gutter marks
//! take, so the result lands in the tree of the last test run.

use std::collections::BTreeMap;
use std::path::PathBuf;

use base::Result;
use project_model::ProjectModel;
use runner::{
    ConfigField, ConfigTemplate, RunConfig, RunConfigProvider, RunPlan, TestRequest,
    format_environment, join_arguments, parse_environment, split_arguments,
};
use serde::{Deserialize, Serialize};
use toml::Table;

pub const PROVIDER_ID: &str = "junit";
const TEMPLATE_JUNIT: &str = "junit";
/// What the new configuration is called before the user renames it.
const NEW_CONFIG_NAME: &str = "JUnit";
/// The group of the dialog and of the Services window.
const GROUP_NAME: &str = "JUnit";
const ICON: &str = "run/junit";

const FIELD_OBJECT: &str = "test_object";
const FIELD_CLASS: &str = "class";
const FIELD_METHOD: &str = "method";
const FIELD_PACKAGE: &str = "package";
const FIELD_PATTERN: &str = "pattern";
const FIELD_MODULE: &str = "module";
const FIELD_VM_OPTIONS: &str = "jvm_args";
const FIELD_ENV: &str = "env";
const FIELD_WORKING_DIR: &str = "working_dir";

/// The JUnit part of a run configuration, kept in its `settings`.
///
/// `test_object` is the `TEST_OBJECT` of the other IDE: `class`, `method`, `package`,
/// `directory` or `pattern`. A directory runs its package and a pattern the package it
/// belongs to; the dialog shows the fields the object uses.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JUnitTarget {
    pub test_object: String,
    pub class: String,
    pub method: String,
    pub package: String,
    pub pattern: String,
    pub module: String,
    pub jvm_args: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub working_dir: Option<PathBuf>,
}

impl JUnitTarget {
    /// The request the Tests window runs: a method needs its class, a class runs alone,
    /// and a package or a directory runs every test of it.
    pub fn request(&self) -> TestRequest {
        let module = (!self.module.trim().is_empty()).then(|| self.module.trim().to_owned());
        let class = self.class.trim();
        let method = self.method.trim();
        let package = self.package.trim();
        let suite = (!class.is_empty()).then(|| class.to_owned());
        let case = (suite.is_some() && !method.is_empty()).then(|| method.to_owned());
        let package = (suite.is_none() && !package.is_empty()).then(|| package.to_owned());
        TestRequest {
            module,
            package,
            suite,
            case,
            cases: Vec::new(),
        }
    }
}

/// Makes configurations that run tests, and names the group of the dialog.
pub struct JUnitProvider;

impl JUnitProvider {
    fn target(config: &RunConfig) -> JUnitTarget {
        config.settings.clone().try_into().unwrap_or_default()
    }

    fn store(target: &JUnitTarget) -> Result<Table> {
        Ok(Table::try_from(target.clone())?)
    }
}

impl RunConfigProvider for JUnitProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn icon(&self, _config: &RunConfig) -> &'static str {
        ICON
    }

    fn group_name(&self, _config: &RunConfig) -> &'static str {
        GROUP_NAME
    }

    fn group_icon(&self, _config: &RunConfig) -> &'static str {
        ICON
    }

    fn template_icon(&self, _template: &ConfigTemplate) -> &'static str {
        ICON
    }

    /// A test configuration comes from the user or from an import, not from the sources.
    fn discover(&self, _model: &ProjectModel) -> Result<Vec<RunConfig>> {
        Ok(Vec::new())
    }

    /// The tests of this configuration run through the test provider; the provider is
    /// asked for the request before a program is planned.
    fn plan(&self, _config: &RunConfig, _model: &ProjectModel) -> Result<RunPlan> {
        Err("a JUnit configuration runs its tests through the test runner".into())
    }

    fn test_request(&self, config: &RunConfig) -> Option<TestRequest> {
        Some(Self::target(config).request())
    }

    fn fields(&self, _config: &RunConfig) -> Vec<ConfigField> {
        vec![
            ConfigField::text(FIELD_CLASS, "Test class")
                .hint("The fully qualified name of the class"),
            ConfigField::text(FIELD_METHOD, "Test method")
                .hint("One method of the class, empty for every test of it"),
            ConfigField::text(FIELD_PACKAGE, "Package")
                .hint("Every test of the package and its subpackages"),
            ConfigField::text(FIELD_MODULE, "Module").hint("The module of the project"),
            ConfigField::arguments(FIELD_VM_OPTIONS, "VM options")
                .hint("Options of the JVM, such as -Xmx512m"),
            ConfigField::env(FIELD_ENV, "Environment variables").hint("A=1;B=2"),
            ConfigField::path(FIELD_WORKING_DIR, "Working directory")
                .hint("The module directory if empty"),
        ]
    }

    fn read_field(&self, config: &RunConfig, key: &str) -> String {
        let target = Self::target(config);
        match key {
            FIELD_OBJECT => target.test_object,
            FIELD_CLASS => target.class,
            FIELD_METHOD => target.method,
            FIELD_PACKAGE => target.package,
            FIELD_PATTERN => target.pattern,
            FIELD_MODULE => target.module,
            FIELD_VM_OPTIONS => join_arguments(&target.jvm_args),
            FIELD_ENV => format_environment(&target.env),
            FIELD_WORKING_DIR => target
                .working_dir
                .map(|dir| dir.display().to_string())
                .unwrap_or_default(),
            _ => String::new(),
        }
    }

    fn write_field(&self, config: &mut RunConfig, key: &str, value: &str) {
        let mut target = Self::target(config);
        match key {
            FIELD_OBJECT => target.test_object = value.trim().to_owned(),
            FIELD_CLASS => target.class = value.trim().to_owned(),
            FIELD_METHOD => target.method = value.trim().to_owned(),
            FIELD_PACKAGE => target.package = value.trim().to_owned(),
            FIELD_PATTERN => target.pattern = value.trim().to_owned(),
            FIELD_MODULE => target.module = value.trim().to_owned(),
            FIELD_VM_OPTIONS => target.jvm_args = split_arguments(value),
            FIELD_ENV => target.env = parse_environment(value),
            FIELD_WORKING_DIR => {
                let dir = value.trim();
                target.working_dir = (!dir.is_empty()).then(|| dir.into());
            }
            _ => return,
        }
        if let Ok(settings) = Self::store(&target) {
            config.settings = settings;
        }
    }

    fn templates(&self) -> Vec<ConfigTemplate> {
        vec![ConfigTemplate {
            id: TEMPLATE_JUNIT,
            name: GROUP_NAME,
        }]
    }

    fn create_config(&self, template: &str, model: &ProjectModel) -> Result<RunConfig> {
        if template != TEMPLATE_JUNIT {
            return Err(format!("{PROVIDER_ID} cannot create {template}").into());
        }
        let target = JUnitTarget {
            test_object: "class".into(),
            module: model
                .modules
                .first()
                .map(|module| module.id.clone())
                .unwrap_or_default(),
            ..JUnitTarget::default()
        };
        Ok(RunConfig {
            name: NEW_CONFIG_NAME.into(),
            provider: PROVIDER_ID.into(),
            assemble_first: false,
            before_launch: Some(Vec::new()),
            settings: Self::store(&target)?,
            ..RunConfig::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(settings: JUnitTarget) -> RunConfig {
        RunConfig {
            name: "Tests".into(),
            provider: PROVIDER_ID.into(),
            assemble_first: false,
            before_launch: Some(Vec::new()),
            settings: JUnitProvider::store(&settings).unwrap(),
            ..RunConfig::default()
        }
    }

    #[test]
    fn a_class_runs_every_test_of_it() {
        let target = JUnitTarget {
            test_object: "class".into(),
            class: "demo.AppTest".into(),
            module: "demo".into(),
            ..JUnitTarget::default()
        };

        let request = config(target);
        let request = JUnitProvider::target(&request).request();

        assert_eq!(request.suite.as_deref(), Some("demo.AppTest"));
        assert_eq!(request.case, None);
        assert_eq!(request.module.as_deref(), Some("demo"));
    }

    #[test]
    fn a_method_runs_one_test_of_its_class() {
        let target = JUnitTarget {
            test_object: "method".into(),
            class: "demo.AppTest".into(),
            method: "passes".into(),
            ..JUnitTarget::default()
        };

        let request = target.request();

        assert_eq!(request.suite.as_deref(), Some("demo.AppTest"));
        assert_eq!(request.case.as_deref(), Some("passes"));
    }

    #[test]
    fn a_package_runs_without_a_class() {
        let target = JUnitTarget {
            test_object: "package".into(),
            package: "demo.tests".into(),
            ..JUnitTarget::default()
        };

        let request = target.request();

        assert_eq!(request.package.as_deref(), Some("demo.tests"));
        assert_eq!(request.suite, None);
    }

    #[test]
    fn a_method_without_a_class_runs_the_package_of_the_configuration() {
        let target = JUnitTarget {
            test_object: "method".into(),
            method: "passes".into(),
            package: "demo.tests".into(),
            ..JUnitTarget::default()
        };

        let request = target.request();

        assert_eq!(request.suite, None);
        assert_eq!(request.case, None);
        assert_eq!(request.package.as_deref(), Some("demo.tests"));
    }

    #[test]
    fn the_fields_round_trip_through_the_settings() {
        let provider = JUnitProvider;
        let mut config = provider
            .create_config(TEMPLATE_JUNIT, &ProjectModel::default())
            .unwrap();
        provider.write_field(&mut config, FIELD_CLASS, " demo.AppTest ");
        provider.write_field(&mut config, FIELD_METHOD, "passes");
        provider.write_field(&mut config, FIELD_VM_OPTIONS, "-Xmx256m \"-Dd=a b\"");
        provider.write_field(&mut config, FIELD_ENV, "A=1;B=2");
        provider.write_field(&mut config, FIELD_WORKING_DIR, "build");

        assert_eq!(provider.read_field(&config, FIELD_CLASS), "demo.AppTest");
        assert_eq!(provider.read_field(&config, FIELD_METHOD), "passes");
        assert_eq!(
            provider.read_field(&config, FIELD_VM_OPTIONS),
            "-Xmx256m \"-Dd=a b\""
        );
        assert_eq!(provider.read_field(&config, FIELD_ENV), "A=1;B=2");
        assert_eq!(provider.read_field(&config, FIELD_WORKING_DIR), "build");
        let target = JUnitProvider::target(&config);
        assert_eq!(target.jvm_args, ["-Xmx256m", "-Dd=a b"]);
        assert_eq!(target.env.get("B").map(String::as_str), Some("2"));
    }

    #[test]
    fn a_new_configuration_is_a_class_of_the_first_module() {
        let model = ProjectModel {
            modules: vec![project_model::Module {
                id: "demo".into(),
                name: "demo".into(),
                ..project_model::Module::default()
            }],
            ..ProjectModel::default()
        };

        let config = JUnitProvider.create_config(TEMPLATE_JUNIT, &model).unwrap();

        assert_eq!(config.provider, PROVIDER_ID);
        assert!(config.before_launch_steps().is_empty());
        assert_eq!(JUnitProvider::target(&config).module, "demo");
        assert_eq!(config.name, NEW_CONFIG_NAME);
    }

    #[test]
    fn only_the_test_template_is_created() {
        assert!(
            JUnitProvider
                .create_config("application", &ProjectModel::default())
                .is_err()
        );
    }
}

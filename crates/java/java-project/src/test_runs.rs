//! Test runs through the build tool, with the IDE listener on the test classpath. The tool
//! compiles what changed and keeps the project's own test setup: its `argLine`, agents
//! like JaCoCo, forks and system properties.

use std::fs;
use std::path::{Path, PathBuf};

use base::Result;
use proc_sup::ProcessSpec;
use project_model::cache_file;

use crate::env::fresh_directory;
use crate::{BuildEnv, Gradle, Maven};

pub const TEST_LISTENER: &str = "ide-test-listener.jar";
const INIT_SCRIPT: &str = include_str!("../../../../helpers/gradle-init/ide-test.init.gradle");

/// What to run, in the terms of the model and of Java.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestSelection {
    /// A module id of the model.
    pub module: Option<String>,
    /// A binary class name, `demo.Outer$Inner`.
    pub class: Option<String>,
    pub method: Option<String>,
    /// `(class, method)` pairs, run instead of `class` and `method` when not empty.
    pub cases: Vec<(String, String)>,
}

/// Where the listener reports to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestHook {
    pub port: u16,
    pub token: String,
}

impl BuildEnv {
    pub fn test_listener(&self) -> Result<PathBuf> {
        let listener = self.helpers_dir()?.join(TEST_LISTENER);
        if !listener.is_file() {
            return Err(format!("no helper at {}", listener.display()).into());
        }
        Ok(listener)
    }
}

impl Maven {
    /// `test` with Surefire's additional classpath. The port goes both in the environment
    /// and as a user property, which Surefire hands to its forks as a system property.
    pub fn test_command(
        &self,
        root: &Path,
        selection: &TestSelection,
        hook: &TestHook,
    ) -> Result<ProcessSpec> {
        self.test_spec(root, selection, hook, None)
    }

    /// The same run with Surefire's debug option: the agent is appended to the `argLine`
    /// the project already set, so JaCoCo and the like keep working.
    pub fn debug_test_command(
        &self,
        root: &Path,
        selection: &TestSelection,
        hook: &TestHook,
        agent: &str,
    ) -> Result<ProcessSpec> {
        self.test_spec(root, selection, hook, Some(agent))
    }

    fn test_spec(
        &self,
        root: &Path,
        selection: &TestSelection,
        hook: &TestHook,
        debug_agent: Option<&str>,
    ) -> Result<ProcessSpec> {
        let listener = self.env.test_listener()?;
        let mut spec = self
            .command(root)?
            .env("IDE_TEST_PORT", hook.port.to_string())
            .env("IDE_TEST_TOKEN", &hook.token)
            .arg(format!(
                "-Dmaven.test.additionalClasspath={}",
                listener.display()
            ))
            .arg(format!("-Dide.test.port={}", hook.port))
            .arg(format!("-Dide.test.token={}", hook.token));
        if let Some(agent) = debug_agent {
            spec = spec.arg(format!("-Dmaven.surefire.debug={agent}"));
        }
        if let Some(module) = &selection.module {
            // `-am` builds what the module needs, the filter keeps their tests out.
            spec = spec.arg("-pl").arg(maven_selector(module)).arg("-am");
        }
        if let Some(pattern) = surefire_pattern(selection) {
            spec = spec
                .arg(format!("-Dtest={pattern}"))
                .arg("-Dsurefire.failIfNoSpecifiedTests=false");
        }
        Ok(spec.arg("test"))
    }
}

/// `group:artifact:version` of the model to the `group:artifact` that `-pl` takes.
fn maven_selector(module: &str) -> &str {
    match module.rsplit_once(':') {
        Some((selector, _)) if selector.contains(':') => selector,
        _ => module,
    }
}

fn surefire_pattern(selection: &TestSelection) -> Option<String> {
    if !selection.cases.is_empty() {
        let cases = selection.cases.iter();
        return Some(
            cases
                .map(|(class, method)| format!("{class}#{method}"))
                .collect::<Vec<_>>()
                .join(","),
        );
    }
    let class = selection.class.as_ref()?;
    Some(match &selection.method {
        Some(method) => format!("{class}#{method}"),
        None => class.clone(),
    })
}

impl Gradle {
    /// The `test` task of the module with the init script that adds the listener. The init
    /// script also makes the task run when Gradle thinks it is up to date.
    pub fn test_command(
        &self,
        root: &Path,
        selection: &TestSelection,
        hook: &TestHook,
    ) -> Result<ProcessSpec> {
        self.test_spec(root, selection, hook, None)
    }

    /// The same run with a debug agent in the test JVM. The init script adds it to the
    /// `jvmArgs` of every `Test` task.
    pub fn debug_test_command(
        &self,
        root: &Path,
        selection: &TestSelection,
        hook: &TestHook,
        agent: &str,
    ) -> Result<ProcessSpec> {
        self.test_spec(root, selection, hook, Some(agent))
    }

    fn test_spec(
        &self,
        root: &Path,
        selection: &TestSelection,
        hook: &TestHook,
        debug_agent: Option<&str>,
    ) -> Result<ProcessSpec> {
        let listener = self.env.test_listener()?;
        let directory = cache_file(&self.env.cache_root(), root).with_file_name("tests");
        let directory = fresh_directory(&directory)?;
        let script = directory.join("ide-test.init.gradle");
        fs::write(&script, INIT_SCRIPT)?;
        let task = match selection.module.as_deref() {
            Some(":") => ":test".to_owned(),
            Some(path) => format!("{path}:test"),
            None => "test".to_owned(),
        };
        let mut spec = self
            .command(root)?
            .arg("--init-script")
            .arg(&script)
            .arg(format!("-Pide.test.listener={}", listener.display()))
            .arg(format!("-Pide.test.port={}", hook.port))
            .arg(format!("-Pide.test.token={}", hook.token));
        if let Some(agent) = debug_agent {
            spec = spec.arg(format!("-Pide.debug.agent={agent}"));
        }
        let mut spec = spec.arg(task);
        for (class, method) in &selection.cases {
            spec = spec.arg("--tests").arg(format!("{class}.{method}"));
        }
        if let (Some(class), true) = (&selection.class, selection.cases.is_empty()) {
            let pattern = match &selection.method {
                Some(method) => format!("{class}.{method}"),
                None => class.clone(),
            };
            spec = spec.arg("--tests").arg(pattern);
        }
        Ok(spec)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_maven_module_is_selected_without_its_version() {
        assert_eq!(
            maven_selector("org.demo:core:1.0-SNAPSHOT"),
            "org.demo:core"
        );
        assert_eq!(maven_selector("org.demo:core"), "org.demo:core");
        assert_eq!(maven_selector("core"), "core");
    }

    #[test]
    fn a_method_joins_its_class() {
        let selection = TestSelection {
            class: Some("demo.VetTests".into()),
            method: Some("serialization".into()),
            ..TestSelection::default()
        };

        assert_eq!(
            surefire_pattern(&selection).as_deref(),
            Some("demo.VetTests#serialization")
        );
        assert_eq!(surefire_pattern(&TestSelection::default()), None);
    }

    #[test]
    fn failed_tests_of_several_classes_join_in_one_pattern() {
        let selection = TestSelection {
            cases: vec![
                ("demo.A".into(), "x".into()),
                ("demo.B$Inner".into(), "y".into()),
            ],
            ..TestSelection::default()
        };

        assert_eq!(
            surefire_pattern(&selection).as_deref(),
            Some("demo.A#x,demo.B$Inner#y")
        );
    }
}

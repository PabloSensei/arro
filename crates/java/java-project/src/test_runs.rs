//! Test runs through the build tool, with the IDE listener on the test classpath. The tool
//! compiles what changed and keeps the project's own test setup: its `argLine`, agents
//! like JaCoCo, forks and system properties.

use std::fs;
use std::path::{Path, PathBuf};

use base::Result;
use proc_sup::ProcessSpec;
use project_model::cache_file;

use crate::env::fresh_directory;
use crate::{BuildEnv, Gradle, LocalRepositories, Maven};

pub const TEST_LISTENER: &str = "ide-test-listener.jar";
/// The Maven plugin that appends the coverage agent to the `argLine` the project set.
const JACOCO_PLUGIN: &str = "jacoco-maven-plugin";
/// What the console says when the project's `argLine` is replaced by the agent.
pub const ARGLINE_REPLACED: &str =
    "the argLine of the project is replaced by the coverage agent for this run";
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
        self.test_spec(root, selection, hook, None, &[])
            .map(|(spec, _)| spec)
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
        self.test_spec(root, selection, hook, Some(agent), &[])
            .map(|(spec, _)| spec)
    }

    /// The same run with the coverage agent in every test JVM. The JaCoCo plugin appends
    /// the agent to the `argLine` of the project, so the flags there stay; a project
    /// that cannot load the plugin gets the agent as `argLine` itself, and its own flags
    /// are replaced for this run.
    pub fn coverage_test_command(
        &self,
        root: &Path,
        selection: &TestSelection,
        hook: &TestHook,
        agent: &str,
        agent_version: Option<&str>,
        exec: &Path,
    ) -> Result<(ProcessSpec, Option<String>)> {
        let version = agent_version.filter(|_| self.coverage_plugin_available(agent_version));
        let (extra, note) = maven_coverage_arguments(version, agent, exec);
        self.test_spec(root, selection, hook, None, &extra)
            .map(|(spec, _)| (spec, note))
    }

    /// Whether Maven can run the coverage plugin without downloading it: online it can
    /// download it, offline only when the local repository already has it.
    fn coverage_plugin_available(&self, version: Option<&str>) -> bool {
        let Some(version) = version else {
            return false;
        };
        if !self.env.build_tools.first.offline {
            return true;
        }
        let jar = format!("{JACOCO_PLUGIN}-{version}.jar");
        LocalRepositories::detect(None, None)
            .maven
            .iter()
            .any(|repository| {
                repository
                    .join("org")
                    .join("jacoco")
                    .join(JACOCO_PLUGIN)
                    .join(version)
                    .join(&jar)
                    .is_file()
            })
    }

    fn test_spec(
        &self,
        root: &Path,
        selection: &TestSelection,
        hook: &TestHook,
        debug_agent: Option<&str>,
        extra: &[String],
    ) -> Result<(ProcessSpec, Option<String>)> {
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
        for argument in extra {
            spec = spec.arg(argument);
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
        Ok((spec.arg("test"), None))
    }
}

/// The arguments that put the coverage agent into the test JVMs of Maven. With the
/// plugin the agent follows the `argLine` of the project; without it the agent is the
/// `argLine`, and the note tells the user what that replaced.
fn maven_coverage_arguments(
    version: Option<&str>,
    agent: &str,
    exec: &Path,
) -> (Vec<String>, Option<String>) {
    match version {
        Some(version) => (
            vec![
                format!("org.jacoco:{JACOCO_PLUGIN}:{version}:prepare-agent"),
                format!("-Djacoco.destFile={}", exec.display()),
                "-Djacoco.append=false".to_owned(),
            ],
            None,
        ),
        None => (
            vec![format!("-DargLine={agent}")],
            Some(ARGLINE_REPLACED.to_owned()),
        ),
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
        self.test_spec(root, selection, hook, None, None)
            .map(|(spec, _)| spec)
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
        self.test_spec(root, selection, hook, Some(agent), None)
            .map(|(spec, _)| spec)
    }

    /// The same run with the coverage agent in every test JVM, which the init script
    /// adds to the `jvmArgs` of the `Test` tasks too.
    pub fn coverage_test_command(
        &self,
        root: &Path,
        selection: &TestSelection,
        hook: &TestHook,
        agent: &str,
    ) -> Result<(ProcessSpec, Option<String>)> {
        self.test_spec(root, selection, hook, None, Some(agent))
    }

    fn test_spec(
        &self,
        root: &Path,
        selection: &TestSelection,
        hook: &TestHook,
        debug_agent: Option<&str>,
        coverage_agent: Option<&str>,
    ) -> Result<(ProcessSpec, Option<String>)> {
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
        if let Some(agent) = coverage_agent {
            spec = spec.arg(format!("-Pide.coverage.agent={agent}"));
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
        Ok((spec, None))
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
    fn the_coverage_plugin_keeps_the_argline_of_the_project() {
        let agent = "-javaagent:C:/tools/agent.jar=destfile=C:/cache/run.exec,append=false";

        let (extra, note) =
            maven_coverage_arguments(Some("0.8.15"), agent, Path::new("C:/cache/run.exec"));

        assert_eq!(
            extra[0],
            "org.jacoco:jacoco-maven-plugin:0.8.15:prepare-agent"
        );
        assert_eq!(extra[1], "-Djacoco.destFile=C:/cache/run.exec");
        assert_eq!(extra[2], "-Djacoco.append=false");
        assert!(note.is_none());
    }

    #[test]
    fn without_the_plugin_the_agent_replaces_the_argline_and_says_so() {
        let agent = "-javaagent:C:/tools/agent.jar=destfile=C:/cache/run.exec,append=false";

        let (extra, note) = maven_coverage_arguments(None, agent, Path::new("C:/cache/run.exec"));

        assert_eq!(extra, vec![format!("-DargLine={agent}")]);
        assert_eq!(note.as_deref(), Some(ARGLINE_REPLACED));
    }

    #[test]
    fn the_gradle_init_script_takes_the_coverage_agent_of_the_run() {
        assert!(INIT_SCRIPT.contains("ide.coverage.agent"));
        assert!(INIT_SCRIPT.contains("test.jvmArgs coverageAgent"));
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

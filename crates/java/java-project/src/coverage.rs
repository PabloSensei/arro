//! The JaCoCo tools of a run with coverage: the agent the JVM starts with and the helper
//! jars that turn its data file into lines.

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use base::Result;

use crate::BuildEnv;

/// Where the agent and the report helper of a coverage run are.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoverageTools {
    /// `org.jacoco.agent-<version>-runtime.jar`.
    pub agent: PathBuf,
    /// The folder with `coverage-report.jar` and the jars it loads.
    pub report: PathBuf,
}

impl CoverageTools {
    /// The version of the agent, `0.8.15`, which the build tool asks the plugin of.
    pub fn version(&self) -> Option<String> {
        let name = self.agent.file_name()?.to_string_lossy();
        let rest = name.strip_prefix("org.jacoco.agent-")?;
        let version = rest.strip_suffix("-runtime.jar")?;
        (!version.is_empty()).then(|| version.to_owned())
    }

    /// The option the JVM of the run starts with.
    pub fn java_agent(&self, exec: &Path) -> String {
        format!(
            "-javaagent:{}=destfile={},append=false",
            self.agent.display(),
            exec.display()
        )
    }
}

impl BuildEnv {
    /// The tools of a coverage run. The folder can be set for a test, then the one of the
    /// environment is used, then the folder of the distribution next to the program.
    pub fn coverage_tools(&self) -> Result<CoverageTools> {
        if let Some(tools) = &self.coverage {
            return Ok(tools.clone());
        }
        let mut directories = Vec::new();
        if let Some(directory) = env::var_os("IDE_COVERAGE_DIR") {
            directories.push(PathBuf::from(directory));
        }
        if let Ok(exe) = env::current_exe()
            && let Some(dir) = exe.parent()
        {
            let beside = dir.join("tools").join("coverage");
            directories.push(beside.clone());
            let resources = platform::resources_dir(&exe).join("tools").join("coverage");
            if resources != beside {
                directories.push(resources);
            }
        }
        if let Some(tools) = env::var_os("IDE_TOOLS").map(PathBuf::from) {
            directories.push(tools.join("coverage"));
            directories.push(tools.join("tools").join("coverage"));
        }
        let agent = directories
            .iter()
            .find_map(|directory| newest_agent(directory))
            .ok_or_else(|| {
                let looked: Vec<String> = directories
                    .iter()
                    .map(|directory| directory.display().to_string())
                    .collect();
                format!(
                    "no coverage agent in {}; put org.jacoco.agent-<version>-runtime.jar there",
                    looked.join(", ")
                )
            })?;
        Ok(CoverageTools {
            agent,
            report: self.helpers_dir()?.join("coverage"),
        })
    }
}

/// The newest `org.jacoco.agent-*-runtime.jar` of a folder, by the numbers of its version.
fn newest_agent(directory: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(directory).ok()?;
    let mut agents: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            path.is_file()
                && name.starts_with("org.jacoco.agent-")
                && name.ends_with("-runtime.jar")
        })
        .collect();
    agents.sort_by_cached_key(|path| {
        version_order(&path.file_name().unwrap_or_default().to_string_lossy())
    });
    agents.pop()
}

/// The numbers at the end of the file name: `org.jacoco.agent-0.8.15-runtime.jar` is
/// `[0, 8, 15]`.
fn version_order(name: &str) -> Vec<u64> {
    let tail = name
        .strip_prefix("org.jacoco.agent-")
        .unwrap_or(name)
        .strip_suffix("-runtime.jar")
        .unwrap_or(name);
    tail.split(|character: char| !character.is_ascii_digit())
        .filter(|part| !part.is_empty())
        .map(|part| part.parse().unwrap_or(u64::MAX))
        .collect()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn the_version_comes_from_the_name_of_the_agent() {
        let tools = CoverageTools {
            agent: PathBuf::from("tools/coverage/org.jacoco.agent-0.8.15-runtime.jar"),
            report: PathBuf::from("helpers/coverage"),
        };

        assert_eq!(tools.version().as_deref(), Some("0.8.15"));
    }

    #[test]
    fn the_option_names_the_file_of_the_run() {
        let tools = CoverageTools {
            agent: PathBuf::from("C:/tools/agent.jar"),
            report: PathBuf::from("helpers/coverage"),
        };

        assert_eq!(
            tools.java_agent(Path::new("C:/cache/coverage/App-1.exec")),
            "-javaagent:C:/tools/agent.jar=destfile=C:/cache/coverage/App-1.exec,append=false"
        );
    }

    #[test]
    fn the_newest_agent_of_a_folder_wins() {
        let root = test_support::scratch("coverage-agent");
        fs::write(root.join("org.jacoco.agent-0.8.9-runtime.jar"), b"").unwrap();
        fs::write(root.join("org.jacoco.agent-0.8.15-runtime.jar"), b"").unwrap();
        fs::write(root.join("org.jacoco.agent-0.8.15-sources.jar"), b"").unwrap();

        let agent = newest_agent(&root).unwrap();

        assert_eq!(
            agent.file_name().unwrap().to_string_lossy(),
            "org.jacoco.agent-0.8.15-runtime.jar"
        );
    }

    #[test]
    fn a_folder_without_an_agent_has_none() {
        let root = test_support::scratch("coverage-agent-empty");
        assert!(newest_agent(&root).is_none());
    }
}

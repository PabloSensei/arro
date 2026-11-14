use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base::{Cancel, Result};
use proc_sup::{ProcessSpec, capture};
use project_model::{BuildSystem, BuildTools, default_cache_root};

use crate::{Gradle, Jdk, Maven};

/// How tools get started on this machine.
#[derive(Clone, Debug, Default)]
pub struct BuildEnv {
    /// Set by the user, wins over whatever the project asks for.
    pub jdk: Option<Jdk>,
    /// Added to the environment of every tool.
    pub vars: Vec<(OsString, OsString)>,
    /// Added to every Maven command line.
    pub maven_args: Vec<String>,
    /// What the Settings page says about each build tool.
    pub build_tools: BuildTools,
    /// Where tools write their reports and the model cache lives.
    pub cache_root: Option<PathBuf>,
    /// Where `maven-ext.jar` lies, `helpers` next to the executable if not set.
    pub helpers: Option<PathBuf>,
    /// Where `ide-ctrl` lies, when it is not next to the executable.
    pub ctrl_helper: Option<PathBuf>,
    /// The coverage tools of a test, which wins over the folders of the environment.
    pub coverage: Option<crate::CoverageTools>,
}

impl BuildEnv {
    pub fn cache_root(&self) -> PathBuf {
        self.cache_root.clone().unwrap_or_else(default_cache_root)
    }

    pub(crate) fn helpers_dir(&self) -> Result<PathBuf> {
        if let Some(helpers) = &self.helpers {
            return Ok(helpers.clone());
        }
        if let Some(helpers) = std::env::var_os("IDE_HELPERS_DIR") {
            return Ok(helpers.into());
        }
        let exe = std::env::current_exe()?;
        Ok(helpers_for(&exe))
    }

    /// The JDK that builds and runs this project. The choice of the user in Project
    /// Structure wins over the build files. The terminal has to get the same one: a
    /// different `JAVA_HOME` means a second Gradle server.
    pub fn project_jdk(&self, root: &Path) -> Result<Jdk> {
        if let Some(jdk) = &self.jdk {
            return Ok(jdk.clone());
        }
        crate::project_jdk(root)
    }

    /// What every process of the project gets, whichever tool starts it.
    pub fn apply(&self, mut spec: ProcessSpec, jdk: &Jdk) -> ProcessSpec {
        spec = spec.env("JAVA_HOME", &jdk.home);
        if let Some(helper) = &self.ctrl_helper {
            spec = spec.ctrl_helper(helper);
        }
        for (name, value) in &self.vars {
            spec = spec.env(name, value);
        }
        spec
    }
}

/// The `helpers` folder of the program at `exe`: next to the program when it is there,
/// then the `helpers` of the resources of a macOS bundle, then next to the program, where
/// the caller reports it missing.
fn helpers_for(exe: &Path) -> PathBuf {
    let beside = exe.with_file_name("helpers");
    if beside.is_dir() {
        return beside;
    }
    let resources = platform::resources_dir(exe).join("helpers");
    if resources.is_dir() {
        return resources;
    }
    beside
}

/// Picks the tool of the project in `root`, Maven first when both are present: a project
/// with both files is usually a Maven project that also offers a Gradle build.
pub fn detect_build_system(root: &Path, env: &BuildEnv) -> Option<Arc<dyn BuildSystem>> {
    let tools: [Arc<dyn BuildSystem>; 2] = [
        Arc::new(Maven::new(env.clone())),
        Arc::new(Gradle::new(env.clone())),
    ];
    tools.into_iter().find(|tool| tool.detect(root))
}

/// Runs a tool to the end. Its output is the only explanation a failure comes with.
pub(crate) fn run_tool(spec: &ProcessSpec, cancel: &Cancel) -> Result {
    let captured = capture(spec, cancel)?;
    if captured.status.success() {
        return Ok(());
    }
    let mut text = captured.stdout;
    text.push_str(&captured.stderr);
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(40)..].join("\n");
    Err(format!("exit {:?}\n{tail}", captured.status.code()).into())
}

/// Where a tool writes its report, cleaned of the previous run.
pub(crate) fn fresh_directory(directory: &Path) -> Result<PathBuf> {
    match fs::remove_dir_all(directory) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    fs::create_dir_all(directory)?;
    Ok(directory.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_helpers_of_a_bundle_are_in_its_resources() {
        let directory = test_support::TempDir::new("helpers-bundle");
        let macos = directory
            .path()
            .join("Arro.app")
            .join("Contents")
            .join("MacOS");
        fs::create_dir_all(&macos).unwrap();
        let exe = macos.join("arro");
        fs::write(&exe, b"").unwrap();
        let helpers = directory
            .path()
            .join("Arro.app")
            .join("Contents")
            .join("Resources")
            .join("helpers");
        fs::create_dir_all(&helpers).unwrap();

        assert_eq!(helpers_for(&exe), helpers);
    }

    #[test]
    fn the_helpers_next_to_the_program_win_over_the_resources() {
        let directory = test_support::TempDir::new("helpers-beside");
        let macos = directory
            .path()
            .join("Arro.app")
            .join("Contents")
            .join("MacOS");
        fs::create_dir_all(macos.join("helpers")).unwrap();
        let exe = macos.join("arro");
        fs::write(&exe, b"").unwrap();
        let resources = directory
            .path()
            .join("Arro.app")
            .join("Contents")
            .join("Resources")
            .join("helpers");
        fs::create_dir_all(&resources).unwrap();

        assert_eq!(helpers_for(&exe), macos.join("helpers"));
    }

    #[test]
    fn missing_helpers_are_looked_for_next_to_the_program() {
        let directory = test_support::TempDir::new("helpers-missing");
        let exe = directory
            .path()
            .join("Arro.app")
            .join("Contents")
            .join("MacOS")
            .join("arro");

        assert_eq!(helpers_for(&exe), exe.with_file_name("helpers"));
    }
}

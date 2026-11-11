use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base::{Cancel, Result};
use proc_sup::{ProcessSpec, capture};
use project_model::{BuildSystem, default_cache_root};

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
    /// Where tools write their reports and the model cache lives.
    pub cache_root: Option<PathBuf>,
    /// Where `maven-ext.jar` lies, `helpers` next to the executable if not set.
    pub helpers: Option<PathBuf>,
    /// Where `ide-ctrl` lies, when it is not next to the executable.
    pub ctrl_helper: Option<PathBuf>,
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
        Ok(exe.with_file_name("helpers"))
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

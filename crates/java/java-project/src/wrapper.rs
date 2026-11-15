//! The wrapper of a build tool: the scripts and the properties a new project needs to
//! build without the tool installed. Only the tool on this machine writes them, as IDEA
//! does, because the wrapper jar and the distribution cannot be made up locally.

use std::path::{Path, PathBuf};

use base::Cancel;
use proc_sup::{ProcessSpec, capture};

use crate::{Jdk, find_maven};

/// Which tool writes the wrapper.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wrapper {
    Maven,
    Gradle,
}

impl Wrapper {
    /// The tool that writes it, `None` when the machine has none.
    pub fn tool(self) -> Option<PathBuf> {
        match self {
            Self::Maven => find_maven(Path::new("")),
            Self::Gradle => crate::gradle::find_gradle(),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Maven => "Maven",
            Self::Gradle => "Gradle",
        }
    }
}

/// Runs the tool in the folder of the new project, where it writes the wrapper of its
/// own version. Offline for Maven, so a missing plugin is a note rather than a download.
pub fn generate(
    kind: Wrapper,
    folder: &Path,
    jdk: Option<&Jdk>,
    cancel: &Cancel,
) -> Result<(), String> {
    let program = kind.tool().ok_or_else(|| {
        format!(
            "no {} on this machine, so the project has no wrapper",
            kind.label()
        )
    })?;
    let mut spec = ProcessSpec::new(program)
        .cwd(folder)
        .label("Build tool wrapper");
    spec = match kind {
        Wrapper::Maven => spec.args(["-N", "-o", "-q", "wrapper:wrapper"]),
        Wrapper::Gradle => spec.args([
            "wrapper",
            "--no-daemon",
            "--console=plain",
            "-q",
            "--offline",
        ]),
    };
    if let Some(jdk) = jdk {
        spec = spec.env("JAVA_HOME", &jdk.home);
    }
    let finished = capture(&spec, cancel)
        .map_err(|error| format!("the {} wrapper could not be written: {error}", kind.label()))?;
    if finished.status.success() {
        return Ok(());
    }
    let mut text = finished.stdout;
    text.push_str(&finished.stderr);
    let lines: Vec<&str> = text.lines().collect();
    let tail = lines[lines.len().saturating_sub(20)..].join("\n");
    Err(format!(
        "the {} wrapper could not be written: exit {:?}\n{tail}",
        kind.label(),
        finished.status.code()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tool_that_is_not_installed_is_named() {
        let missing = Wrapper::Maven.tool();
        if missing.is_none() {
            assert!(generate(Wrapper::Maven, Path::new("."), None, &Cancel::new()).is_err());
        }
        assert_eq!(Wrapper::Gradle.label(), "Gradle");
    }
}

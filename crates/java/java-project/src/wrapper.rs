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

    /// The script the tool writes on this system, the one the user runs by hand.
    pub fn script(self, folder: &Path) -> PathBuf {
        let name = match (self, cfg!(windows)) {
            (Self::Maven, true) => "mvnw.cmd",
            (Self::Maven, false) => "mvnw",
            (Self::Gradle, true) => "gradlew.bat",
            (Self::Gradle, false) => "gradlew",
        };
        folder.join(name)
    }
}

/// Makes the wrapper of a project runnable on a Unix system, where a script from an
/// archive made on Windows or from a copy that dropped the bit fails with a raw
/// `PermissionDenied`. `Some` is the line the build reports when it starts; `Ok(None)`
/// means the bit was already there or the system has no permission bits. An error names
/// the file and the command that fixes it when the bits cannot be changed, such as a file
/// of another owner.
pub fn prepare(program: &Path) -> std::result::Result<Option<String>, String> {
    #[cfg(not(unix))]
    {
        let _ = program;
        Ok(None)
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let metadata = std::fs::metadata(program)
            .map_err(|error| format!("cannot read {}: {error}", program.display()))?;
        if metadata.permissions().mode() & 0o111 != 0 {
            return Ok(None);
        }
        platform::make_executable(program).map_err(|error| {
            format!(
                "{} is not executable and its permissions cannot be changed: {error}. \
                 Run `chmod +x {}` and start the build again",
                program.display(),
                program.display()
            )
        })?;
        Ok(Some(format!(
            "{} was not executable; Arro added the execute bit to start it",
            program.display()
        )))
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
        // The tool writes the script for every system; a folder that does not carry the
        // bits of a Unix file, such as a mounted share, would leave it unrunnable. A file
        // whose bits cannot be changed is reported by the build that starts it, with the
        // path and the command that fixes it.
        let script = kind.script(folder);
        if script.is_file() {
            let _ = prepare(&script);
        }
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

    #[test]
    fn the_script_of_a_wrapper_is_named_as_the_system_names_it() {
        let folder = Path::new("project");
        let maven = Wrapper::Maven.script(folder);
        let gradle = Wrapper::Gradle.script(folder);

        assert_eq!(maven.parent(), Some(folder));
        assert_eq!(gradle.parent(), Some(folder));
        assert!(
            maven
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("mvnw"))
        );
        assert!(
            gradle
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with("gradlew"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_wrapper_without_the_execute_bit_gets_it_and_the_build_says_so() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let folder = test_support::TempDir::new("wrapper-bit");
        let script = Wrapper::Maven.script(folder.path());
        fs::write(&script, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o644)).unwrap();

        let note = prepare(&script).unwrap().expect("the bit was missing");

        assert!(note.contains("mvnw"), "{note}");
        let mode = fs::metadata(&script).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);
        assert_eq!(prepare(&script).unwrap(), None);
    }
}

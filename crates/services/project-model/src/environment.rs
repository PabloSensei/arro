//! What a project needs to build and run: the runtimes it may use, the one it uses now
//! and the language level of its build. The language fills it; nothing here knows which
//! language a project is written in.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use lang_api::WorkspaceChange;

use crate::ProjectModel;

/// One runtime that the project may build and run with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RuntimeInfo {
    /// What the interface shows, usually the name of the folder.
    pub name: String,
    /// The full version, as the runtime reports it.
    pub version: String,
    /// The number that matters for the compiler.
    pub feature: u32,
    pub home: PathBuf,
    /// The user added the folder; a found one cannot be removed.
    pub added: bool,
}

/// Why the current runtime is the one in use.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RuntimeReason {
    /// The user picked it in Project Structure.
    User,
    /// The build files ask for it.
    Project,
    /// Nothing asks for anything, so the default or the newest one is used.
    #[default]
    Default,
}

/// The language level of the build and the values the language offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LanguageLevelInfo {
    pub values: Vec<String>,
    pub selected: Option<String>,
    /// The build file that decides the level, when one does.
    pub file: Option<PathBuf>,
    /// The line of the level in `file`, counted from zero.
    pub line: Option<u32>,
}

/// One read-only line of the project page that the language fills, such as a dependency
/// it found in the project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentDetail {
    pub label: String,
    pub value: String,
}

/// The change of a build file that sets a language level, before it is written.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LanguageLevelEdit {
    /// The build file the change goes to.
    pub file: PathBuf,
    /// The line the value takes in the changed file, counted from zero.
    pub line: Option<u32>,
    /// What the form says before the change is written, such as the line it adds.
    pub summary: String,
    /// The file has no place for the level yet, so the change brings one into being.
    pub added: bool,
    /// The edits, in the coordinates of the text the language read.
    pub change: WorkspaceChange,
}

/// Everything Project Structure shows about the environment of one project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ProjectEnvironmentInfo {
    pub runtimes: Vec<RuntimeInfo>,
    pub selected: Option<PathBuf>,
    pub reason: RuntimeReason,
    /// What the user has to know, such as a saved folder that is gone.
    pub notice: Option<String>,
    pub language: Option<LanguageLevelInfo>,
    /// The variables every tool of the project gets, so that the terminal and the
    /// harnesses use the same runtime as the build.
    pub variables: Vec<(String, String)>,
}

impl ProjectEnvironmentInfo {
    /// The chosen runtime, when the project has one.
    pub fn selected_runtime(&self) -> Option<&RuntimeInfo> {
        let selected = self.selected.as_deref()?;
        self.runtimes
            .iter()
            .find(|runtime| runtime.home == selected)
    }
}

/// Reads and changes the environment of one project. The application implements it for
/// its language; the windows only show what it returns.
pub trait EnvironmentProvider: Send + Sync {
    /// Reads the environment from the project files. Blocks on the disk: not for the UI
    /// thread.
    fn describe(&self) -> ProjectEnvironmentInfo;

    /// Read-only lines the project page shows under the level, such as a dependency
    /// the language found. Blocks on the disk: not for the UI thread.
    fn details(&self) -> Vec<EnvironmentDetail> {
        Vec::new()
    }

    /// The model of the project changed. A language may have settings that it reads
    /// only when its server starts, such as a dependency. Called on the UI thread, so
    /// the reaction must not block.
    fn project_model(&self, _model: &ProjectModel) {}

    /// The last model the language read, for the pages that show its modules. Cheap:
    /// it hands out a copy of a value the language already holds.
    fn model(&self) -> Option<Arc<ProjectModel>> {
        None
    }

    /// The level of one module and the build file that decides it. Blocks on the disk:
    /// not for the UI thread.
    fn module_language(&self, _module: &str) -> Option<LanguageLevelInfo> {
        None
    }

    /// The change that sets the level of the project, or of the module `module`, in the
    /// coordinates of `text` when that is the text of the build file the user sees.
    /// Blocks on the disk: not for the UI thread.
    fn language_change(
        &self,
        _level: &str,
        _module: Option<&str>,
        _text: Option<&str>,
    ) -> Result<LanguageLevelEdit, String> {
        Err("The language of this project cannot change its level".to_owned())
    }

    /// A short line for the user that has not been shown yet, taken once.
    fn take_notice(&self) -> Option<String> {
        None
    }

    /// A runtime at `home`, read from the folder itself. An error is the text that
    /// Project Structure shows under the field.
    fn runtime_at(&self, home: &Path) -> Result<RuntimeInfo, String>;

    /// Stores the choice of the user and the folders added by hand, then makes the next
    /// build, run and server use the result. Blocks on the disk: not for the UI thread.
    fn apply(
        &self,
        selected: Option<&Path>,
        added: &[PathBuf],
    ) -> Result<ProjectEnvironmentInfo, String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_chosen_runtime_is_found_by_its_folder() {
        let info = ProjectEnvironmentInfo {
            runtimes: vec![
                RuntimeInfo {
                    name: "one".into(),
                    version: "17".into(),
                    feature: 17,
                    home: "one".into(),
                    added: false,
                },
                RuntimeInfo {
                    name: "two".into(),
                    version: "21".into(),
                    feature: 21,
                    home: "two".into(),
                    added: true,
                },
            ],
            selected: Some("two".into()),
            ..ProjectEnvironmentInfo::default()
        };

        assert_eq!(
            info.selected_runtime().map(|runtime| runtime.name.as_str()),
            Some("two")
        );
    }
}

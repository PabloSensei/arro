//! The SDKs and the language level of one project, as Project Structure shows and
//! changes them. The choice of the user lives in the project; the server starts again
//! when it changes, so no restart of the window is needed.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use java_project::{
    Jdk, LocalRepositories, ProjectSettings, language_level, resolve_environment, runtime_info,
    save_settings,
};
use project_model::{
    EnvironmentDetail, EnvironmentProvider, LanguageLevelInfo, ProjectEnvironmentInfo,
    ProjectModel, RuntimeInfo,
};

use crate::JavaProject;

/// The environment of one project: files on the disk and, when the server runs, the
/// process that reads them at startup.
pub struct JavaEnvironment {
    root: PathBuf,
    project: Option<Arc<JavaProject>>,
    /// The last model of the project, for a page opened after the sync.
    model: Mutex<Option<Arc<ProjectModel>>>,
}

impl JavaEnvironment {
    pub fn new(root: PathBuf, project: Option<Arc<JavaProject>>) -> Self {
        Self {
            root,
            project,
            model: Mutex::new(None),
        }
    }

    /// The Lombok version of the project: from the server when it already learned it,
    /// from the model, and from the build files before the first sync.
    fn lombok_version(&self) -> Option<String> {
        if let Some(project) = &self.project {
            if let Some(version) = project.lombok_version() {
                return Some(version);
            }
            let model = self
                .model
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .clone();
            if let Some(model) = model {
                return project.lombok_of(&model).map(|lombok| lombok.version);
            }
        }
        let repositories = self
            .project
            .as_ref()
            .map(|project| project.lombok_repositories().clone())
            .unwrap_or_else(|| LocalRepositories::detect(None, None));
        java_project::find_lombok_in_build_files(&self.root, &repositories)
            .map(|lombok| lombok.version)
    }

    fn info(&self) -> ProjectEnvironmentInfo {
        let resolved = resolve_environment(&self.root);
        let runtimes: Vec<RuntimeInfo> = resolved
            .runtimes
            .iter()
            .map(|(jdk, added)| runtime_info(jdk, *added))
            .collect();
        let variables = resolved
            .selected
            .as_ref()
            .map(|jdk| {
                vec![(
                    "JAVA_HOME".to_owned(),
                    jdk.home.to_string_lossy().into_owned(),
                )]
            })
            .unwrap_or_default();
        let language: Option<LanguageLevelInfo> = language_level(&self.root);
        ProjectEnvironmentInfo {
            runtimes,
            selected: resolved.selected.as_ref().map(|jdk| jdk.home.clone()),
            reason: resolved.reason,
            notice: resolved.notice,
            language,
            variables,
        }
    }
}

/// The folder is not an SDK unless it holds the launcher and the version file.
fn check_folder(home: &Path) -> Result<Jdk, String> {
    if !home.join("bin/java.exe").is_file() {
        return Err(format!(
            "The folder is not a JDK: {} has no bin\\java.exe",
            home.display()
        ));
    }
    Jdk::at(home).ok_or_else(|| {
        format!(
            "The folder is not a JDK: {} has no release file with a version",
            home.display()
        )
    })
}

impl EnvironmentProvider for JavaEnvironment {
    fn describe(&self) -> ProjectEnvironmentInfo {
        self.info()
    }

    fn details(&self) -> Vec<EnvironmentDetail> {
        let value = match self.lombok_version() {
            Some(version) => format!("{version} (from project dependencies)"),
            None => "not used".to_owned(),
        };
        vec![EnvironmentDetail {
            label: "Lombok".to_owned(),
            value,
        }]
    }

    fn project_model(&self, model: &ProjectModel) {
        let model = Arc::new(model.clone());
        *self
            .model
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some(model.clone());
        if let Some(project) = &self.project {
            project.set_project_model(&model);
        }
    }

    fn take_notice(&self) -> Option<String> {
        self.project
            .as_ref()
            .and_then(|project| project.take_notice())
    }

    fn runtime_at(&self, home: &Path) -> Result<RuntimeInfo, String> {
        let jdk = check_folder(home)?;
        Ok(runtime_info(&jdk, true))
    }

    fn apply(
        &self,
        selected: Option<&Path>,
        added: &[PathBuf],
    ) -> Result<ProjectEnvironmentInfo, String> {
        if let Some(home) = selected {
            check_folder(home)?;
        }
        save_settings(
            &self.root,
            &ProjectSettings {
                selected: selected.map(Path::to_path_buf),
                added: added.to_vec(),
            },
        )
        .map_err(|error| error.to_string())?;
        let resolved = resolve_environment(&self.root);
        if let Some(project) = &self.project {
            project.set_project_jdk(&resolved);
        }
        Ok(self.info())
    }
}

/// The environment of the project, to give the window at startup.
pub fn project_environment(
    root: PathBuf,
    project: Option<Arc<JavaProject>>,
) -> Arc<dyn EnvironmentProvider> {
    Arc::new(JavaEnvironment::new(root, project))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch(name: &str) -> PathBuf {
        let path =
            std::env::temp_dir().join(format!("java-lang-env-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).unwrap();
        path
    }

    fn jdk(root: &Path, name: &str, version: &str) -> PathBuf {
        let home = root.join(name);
        fs::create_dir_all(home.join("bin")).unwrap();
        fs::write(home.join("bin/java.exe"), "").unwrap();
        fs::write(home.join("bin/javac.exe"), "").unwrap();
        fs::write(
            home.join("release"),
            format!("JAVA_VERSION=\"{version}\"\n"),
        )
        .unwrap();
        home
    }

    #[test]
    fn a_folder_without_the_launcher_is_refused() {
        let root = scratch("bad");
        let environment = JavaEnvironment::new(root.clone(), None);

        let error = environment.runtime_at(&root).err().unwrap_or_default();

        assert!(error.contains("bin\\java.exe"), "{error}");
    }

    #[test]
    fn a_runtime_has_the_name_and_version_of_its_folder() {
        let root = scratch("good");
        let home = jdk(&root, "sdk-21", "21.0.2");
        let environment = JavaEnvironment::new(root, None);

        let runtime = environment.runtime_at(&home).unwrap();

        assert_eq!(runtime.name, "sdk-21");
        assert_eq!(runtime.version, "21.0.2");
        assert_eq!(runtime.feature, 21);
        assert!(runtime.added);
    }
}

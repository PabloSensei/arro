use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The unit a neighbour module exposes to the modules that depend on it.
pub const MAIN_UNIT: &str = "main";
pub const TEST_UNIT: &str = "test";

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProjectModel {
    pub root: PathBuf,
    pub tool: String,
    pub tool_version: Option<String>,
    pub modules: Vec<Module>,
    /// False for a model read from project files alone, without the tool: it knows the
    /// modules and their roots, not their dependencies.
    pub resolved: bool,
    pub problems: Vec<String>,
}

impl ProjectModel {
    pub fn module(&self, id: &str) -> Option<&Module> {
        self.modules.iter().find(|module| module.id == id)
    }

    /// Everything a program of this unit needs at run time, outputs first. A neighbour
    /// module counts through its outputs: its archive may not exist before it is packaged.
    pub fn runtime_paths(&self, module: &str, unit: &str) -> Vec<PathBuf> {
        let Some(found) = self.module(module).and_then(|module| module.unit(unit)) else {
            return Vec::new();
        };
        let mut paths = found.output_dirs.clone();
        for entry in &found.runtime_paths {
            match &entry.kind {
                PathKind::Module { id, .. } => paths.extend(self.outputs_of(id, &entry.path)),
                _ => paths.push(entry.path.clone()),
            }
        }
        paths
    }

    fn outputs_of(&self, module: &str, fallback: &Path) -> Vec<PathBuf> {
        let outputs = self
            .module(module)
            .and_then(|module| module.unit(MAIN_UNIT))
            .map(|unit| unit.output_dirs.clone())
            .filter(|outputs| !outputs.is_empty());
        outputs.unwrap_or_else(|| vec![fallback.to_owned()])
    }
}

/// One task the build tool window offers.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildTask {
    /// What the tool runs: a phase, a goal or a task path.
    pub name: String,
    /// Whether running it starts a program that a debugger can attach to.
    pub program: bool,
}

/// Tasks of one module that the tool shows under one heading.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildGroup {
    pub name: String,
    pub tasks: Vec<BuildTask>,
}

/// The task tree of one module, as the build tool window shows it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildModule {
    pub id: String,
    pub name: String,
    pub groups: Vec<BuildGroup>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Module {
    pub id: String,
    pub name: String,
    pub dir: PathBuf,
    pub units: Vec<Unit>,
}

impl Module {
    pub fn unit(&self, name: &str) -> Option<&Unit> {
        self.units.iter().find(|unit| unit.name == name)
    }
}

/// Sources that compile together, like `main` or `test`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Unit {
    pub name: String,
    pub source_roots: Vec<PathBuf>,
    pub resource_roots: Vec<PathBuf>,
    pub output_dirs: Vec<PathBuf>,
    pub compile_paths: Vec<PathEntry>,
    pub runtime_paths: Vec<PathEntry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathEntry {
    pub path: PathBuf,
    pub kind: PathKind,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum PathKind {
    Library { coordinates: String },
    Module { id: String, built: bool },
    File,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(path: &str, kind: PathKind) -> PathEntry {
        let path = PathBuf::from(path);
        PathEntry { path, kind }
    }

    #[test]
    fn a_neighbour_counts_through_its_outputs() {
        let library = PathKind::Library {
            coordinates: "group:lib:1".into(),
        };
        let neighbour = PathKind::Module {
            id: "core".into(),
            built: false,
        };
        let model = ProjectModel {
            modules: vec![
                Module {
                    id: "core".into(),
                    units: vec![Unit {
                        name: "main".into(),
                        output_dirs: vec!["core/out".into()],
                        ..Unit::default()
                    }],
                    ..Module::default()
                },
                Module {
                    id: "app".into(),
                    units: vec![Unit {
                        name: "main".into(),
                        output_dirs: vec!["app/out".into()],
                        runtime_paths: vec![
                            entry("core/libs/core.pack", neighbour),
                            entry("lib.pack", library),
                        ],
                        ..Unit::default()
                    }],
                    ..Module::default()
                },
            ],
            ..ProjectModel::default()
        };

        let paths = model.runtime_paths("app", "main");

        let expected = ["app/out", "core/out", "lib.pack"].map(PathBuf::from);
        assert_eq!(paths, expected);
    }

    #[test]
    fn an_unknown_neighbour_keeps_the_path_the_tool_gave() {
        let neighbour = PathKind::Module {
            id: "gone".into(),
            built: true,
        };
        let model = ProjectModel {
            modules: vec![Module {
                id: "app".into(),
                units: vec![Unit {
                    name: "main".into(),
                    runtime_paths: vec![entry("gone.pack", neighbour)],
                    ..Unit::default()
                }],
                ..Module::default()
            }],
            ..ProjectModel::default()
        };

        assert_eq!(
            model.runtime_paths("app", "main"),
            [PathBuf::from("gone.pack")]
        );
    }

    #[test]
    fn an_unknown_module_has_no_paths() {
        assert!(
            ProjectModel::default()
                .runtime_paths("app", "main")
                .is_empty()
        );
    }
}

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
    /// What each module depends on, when the tool resolved it.
    #[serde(default)]
    pub dependencies: Vec<ModuleDependencies>,
    /// False for a model read from project files alone, without the tool: it knows the
    /// modules and their roots, not their dependencies.
    pub resolved: bool,
    pub problems: Vec<String>,
}

impl ProjectModel {
    pub fn module(&self, id: &str) -> Option<&Module> {
        self.modules.iter().find(|module| module.id == id)
    }

    /// The dependencies of one module, when the tool reported them.
    pub fn dependencies_of(&self, module: &str) -> Option<&ModuleDependencies> {
        self.dependencies
            .iter()
            .find(|found| found.module == module)
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

/// What one module depends on: the file it is declared in and a tree per scope.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleDependencies {
    pub module: String,
    /// The file that declares the dependencies of the module, when the tool named one.
    #[serde(default)]
    pub build_file: Option<PathBuf>,
    /// The dependencies of the module, one tree per scope.
    #[serde(default)]
    pub scopes: Vec<DependencyScope>,
}

/// The dependencies of a module under one scope, as the tool resolved them.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DependencyScope {
    /// `compile`, `runtime`, `test`, or the name the tool gives its own scope.
    pub name: String,
    pub roots: Vec<Dependency>,
}

/// One edge of the dependency tree: a dependency and everything it pulls in.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Dependency {
    /// `group:artifact:version` of the version resolution chose; the id of a module
    /// for a neighbour of the project.
    pub coordinates: String,
    /// The scope the tool gave this edge.
    #[serde(default)]
    pub scope: String,
    /// `group:artifact:version` the build asks for at this edge, when it differs from
    /// [`Dependency::coordinates`].
    #[serde(default)]
    pub requested: Option<String>,
    /// `group:artifact:version` resolution chose instead of this one, when this edge
    /// lost a version conflict.
    #[serde(default)]
    pub winner: Option<String>,
    /// The file the dependency comes as, when the tool resolved one.
    #[serde(default)]
    pub path: Option<PathBuf>,
    /// The project module this dependency is, rather than a library.
    #[serde(default)]
    pub module: Option<String>,
    #[serde(default)]
    pub children: Vec<Dependency>,
}

/// `group:artifact:version` of a dependency, split for the window.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Coordinates {
    pub group: String,
    pub artifact: String,
    pub version: String,
}

impl Coordinates {
    /// Reads `group:artifact:version`. `None` for a name that does not have all three,
    /// such as the id of a project module.
    pub fn parse(text: &str) -> Option<Self> {
        let (group, rest) = text.split_once(':')?;
        let (artifact, version) = rest.split_once(':')?;
        if group.is_empty() || artifact.is_empty() || version.is_empty() {
            return None;
        }
        Some(Self {
            group: group.to_owned(),
            artifact: artifact.to_owned(),
            version: version.to_owned(),
        })
    }

    /// `group:artifact`, the name an artifact keeps across its versions.
    pub fn artifact_name(&self) -> String {
        format!("{}:{}", self.group, self.artifact)
    }
}

impl ModuleDependencies {
    /// The tree of one scope, when the tool reported it.
    pub fn scope(&self, name: &str) -> Option<&DependencyScope> {
        self.scopes.iter().find(|scope| scope.name == name)
    }
}

impl Dependency {
    /// The version this edge asks for in the build file, when the tool reported one.
    pub fn requested_version(&self) -> Option<String> {
        Coordinates::parse(self.requested.as_deref()?).map(|found| found.version)
    }
}

impl Module {
    pub fn unit(&self, name: &str) -> Option<&Unit> {
        self.units.iter().find(|unit| unit.name == name)
    }
}

/// A dependency left out of another one: the group and the artifact.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Exclusion {
    pub group: String,
    pub artifact: String,
    /// `group:artifact:version` of the dependency declared by the module whose
    /// declaration gets the exclusion.
    pub declared: String,
}

/// A change to the text of one file, in offsets of the text the caller holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEdit {
    pub path: PathBuf,
    pub start: usize,
    pub end: usize,
    pub text: String,
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

    #[test]
    fn coordinates_split_into_group_artifact_and_version() {
        let found = Coordinates::parse("com.fasterxml.jackson.core:jackson-databind:2.17.0");
        let found = found.unwrap();

        assert_eq!(found.group, "com.fasterxml.jackson.core");
        assert_eq!(found.artifact, "jackson-databind");
        assert_eq!(found.version, "2.17.0");
        assert_eq!(
            found.artifact_name(),
            "com.fasterxml.jackson.core:jackson-databind"
        );
        assert_eq!(
            Coordinates::parse(":app"),
            None,
            "a module id is no artifact"
        );
        assert_eq!(Coordinates::parse("group:artifact"), None);
    }

    #[test]
    fn a_node_keeps_the_version_it_asks_for_and_the_one_it_lost_to() {
        let node = Dependency {
            coordinates: "g:a:1".into(),
            requested: Some("g:a:2".into()),
            winner: Some("g:a:3".into()),
            ..Dependency::default()
        };

        assert_eq!(node.requested_version(), Some("2".to_owned()));
        assert_eq!(node.children.len(), 0);
    }

    /// The cache of an older model has no dependency fields, and must still load.
    #[test]
    fn a_model_without_dependencies_still_loads() {
        let text = r#"{"root":"p","tool":"m","modules":[{"id":"a","name":"a","dir":"p",
            "units":[]}],"resolved":true,"problems":[]}"#;

        let model: ProjectModel = serde_json::from_str(text).unwrap();

        assert!(model.dependencies.is_empty());
        assert_eq!(model.modules[0].name, "a");
    }
}

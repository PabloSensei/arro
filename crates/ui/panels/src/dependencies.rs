//! The Dependency Analyzer of the build window: every artifact of the project with the
//! paths that bring it in, the versions that lost a conflict, and the change of a build
//! file that leaves an artifact out of the dependency it is declared in.

use std::collections::BTreeMap;

use project_model::{Coordinates, Dependency, ProjectModel};

/// The name of the command that opens the analyzer from Find Action.
pub(crate) const ANALYZE_COMMAND: &str = "dependencies.analyze";

/// One artifact of the project, across the modules that bring it in.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct Artifact {
    /// `group:artifact`, the name the artifact keeps across its versions.
    pub name: String,
    /// The version most paths resolve to.
    pub selected: String,
    /// The version of every path, selected first.
    pub versions: Vec<String>,
    pub usages: Vec<Usage>,
}

impl Artifact {
    /// The coordinates the analyzer copies and searches the declaration by.
    pub fn coordinates(&self) -> String {
        format!("{}:{}", self.name, self.selected)
    }

    /// A path brought a version resolution dropped, or asked for another one.
    pub fn conflict(&self) -> bool {
        self.usages.iter().any(Usage::conflict)
    }

    /// The versions the resolution did not choose.
    pub fn losing(&self) -> impl Iterator<Item = &String> {
        self.versions
            .iter()
            .filter(|version| **version != self.selected)
    }
}

/// One path that brings an artifact in, from the first-level dependency down to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Usage {
    pub module: String,
    pub module_name: String,
    pub scope: String,
    /// `group:artifact:version` of every edge of the path, the first-level one first.
    pub chain: Vec<String>,
    pub winner: Option<String>,
    pub requested: Option<String>,
}

impl Usage {
    /// Whether this path names a version other than the one it resolved to.
    pub fn conflict(&self) -> bool {
        if self.winner.is_some() {
            return true;
        }
        match (&self.requested, self.chain.last()) {
            (Some(asked), Some(selected)) => asked != selected,
            _ => false,
        }
    }

    /// `spring-boot-starter-web → spring-web → jackson-databind:2.17.0`.
    pub fn path(&self) -> String {
        let last = self.chain.len().saturating_sub(1);
        self.chain
            .iter()
            .enumerate()
            .map(|(index, coordinates)| {
                if index == last {
                    short_name(coordinates)
                } else {
                    artifact_name(coordinates)
                }
            })
            .collect::<Vec<_>>()
            .join(" → ")
    }

    /// What the window says about the version of this path, when it is not the one that
    /// asks for nothing else.
    pub fn mark(&self) -> Option<String> {
        if let Some(winner) = &self.winner {
            return Some(format!("omitted for conflict with {}", version_of(winner)));
        }
        let asked = self.requested.as_deref()?;
        let selected = self.chain.last()?;
        (asked != selected).then(|| format!("{} → {}", version_of(asked), version_of(selected)))
    }

    /// The first-level dependency whose declaration gets an exclusion.
    pub fn declared(&self) -> Option<&str> {
        self.chain.first().map(String::as_str)
    }
}

/// `artifact:version` of a dependency, or the name itself when it is not an artifact.
pub(crate) fn short_name(coordinates: &str) -> String {
    Coordinates::parse(coordinates).map_or_else(
        || coordinates.to_owned(),
        |found| format!("{}:{}", found.artifact, found.version),
    )
}

fn artifact_name(coordinates: &str) -> String {
    Coordinates::parse(coordinates).map_or_else(|| coordinates.to_owned(), |found| found.artifact)
}

fn version_of(coordinates: &str) -> String {
    Coordinates::parse(coordinates).map_or_else(|| coordinates.to_owned(), |found| found.version)
}

/// What the build window shows next to a dependency of the tree.
pub(crate) fn conflict_mark(node: &Dependency) -> Option<String> {
    if let Some(winner) = &node.winner {
        return Some(format!("omitted for conflict with {}", version_of(winner)));
    }
    let asked = node.requested_version()?;
    let selected = Coordinates::parse(&node.coordinates)?.version;
    (asked != selected).then(|| format!("{asked} → {selected}"))
}

/// Every artifact of the project with the paths that bring it in.
pub(crate) fn analyze(model: &ProjectModel) -> Vec<Artifact> {
    let mut found: BTreeMap<String, Artifact> = BTreeMap::new();
    for module in &model.dependencies {
        let name = model
            .module(&module.module)
            .map_or_else(|| module.module.clone(), |found| found.name.clone());
        for scope in &module.scopes {
            for root in &scope.roots {
                let mut chain = Vec::new();
                walk(
                    &module.module,
                    &name,
                    &scope.name,
                    &mut chain,
                    root,
                    &mut found,
                );
            }
        }
    }
    let mut artifacts: Vec<Artifact> = found.into_values().collect();
    for artifact in &mut artifacts {
        let preferred = artifact
            .usages
            .iter()
            .find(|usage| !usage.conflict())
            .or_else(|| artifact.usages.first());
        artifact.selected = preferred
            .and_then(Usage::selected_version)
            .unwrap_or_default();
        let mut versions: Vec<String> = artifact
            .usages
            .iter()
            .filter_map(Usage::selected_version)
            .collect();
        // The selected version first, the rest as the paths met them.
        versions.sort_by_key(|version| *version != artifact.selected);
        versions.dedup();
        artifact.versions = versions;
    }
    artifacts
}

impl Usage {
    fn selected_version(&self) -> Option<String> {
        Coordinates::parse(self.chain.last()?).map(|found| found.version)
    }
}

fn walk(
    module: &str,
    module_name: &str,
    scope: &str,
    chain: &mut Vec<String>,
    node: &Dependency,
    found: &mut BTreeMap<String, Artifact>,
) {
    chain.push(node.coordinates.clone());
    if let Some(coordinates) = Coordinates::parse(&node.coordinates) {
        let name = coordinates.artifact_name();
        let usage = Usage {
            module: module.to_owned(),
            module_name: module_name.to_owned(),
            scope: scope.to_owned(),
            chain: chain.clone(),
            winner: node.winner.clone(),
            requested: node.requested.clone(),
        };
        found
            .entry(name.clone())
            .or_insert_with(|| Artifact {
                name,
                ..Artifact::default()
            })
            .usages
            .push(usage);
    }
    for child in &node.children {
        walk(module, module_name, scope, chain, child, found);
    }
    chain.pop();
}

/// Where the text of a build file declares `coordinates`: the quoted coordinates of a
/// build script, or the `artifactId` element of a POM.
pub(crate) fn declaration_offset(text: &str, coordinates: &str) -> Option<usize> {
    let found = Coordinates::parse(coordinates)?;
    let quoted = format!("{}:{}:", found.group, found.artifact);
    if let Some(at) = text.find(&quoted) {
        return Some(at);
    }
    let element = format!("<artifactId>{}", found.artifact);
    text.find(&element)
}

#[cfg(test)]
mod tests {
    use super::*;
    use project_model::{DependencyScope, Module, ModuleDependencies};

    fn node(coordinates: &str, children: Vec<Dependency>) -> Dependency {
        Dependency {
            coordinates: coordinates.into(),
            children,
            ..Dependency::default()
        }
    }

    fn model(roots: Vec<Dependency>) -> ProjectModel {
        ProjectModel {
            modules: vec![Module {
                id: "app".into(),
                name: "app".into(),
                ..Module::default()
            }],
            dependencies: vec![ModuleDependencies {
                module: "app".into(),
                scopes: vec![DependencyScope {
                    name: "compile".into(),
                    roots,
                }],
                ..ModuleDependencies::default()
            }],
            ..ProjectModel::default()
        }
    }

    #[test]
    fn every_path_of_an_artifact_is_a_usage() {
        let mut lost = node("g:core:1", Vec::new());
        lost.winner = Some("g:core:2".into());
        let tree = node(
            "g:starter:1",
            vec![node("g:web:1", vec![lost]), node("g:core:2", Vec::new())],
        );

        let artifacts = analyze(&model(vec![tree]));

        let core = artifacts
            .iter()
            .find(|artifact| artifact.name == "g:core")
            .unwrap();
        assert_eq!(core.usages.len(), 2);
        assert!(core.conflict());
        assert_eq!(core.selected, "2");
        assert_eq!(core.losing().collect::<Vec<_>>(), ["1"]);
        assert_eq!(
            core.usages[0].path(),
            "starter → web → core:1",
            "the path names the artifacts and the version of the end"
        );
        assert_eq!(
            core.usages[0].mark().as_deref(),
            Some("omitted for conflict with 2")
        );
        assert_eq!(core.usages[0].declared(), Some("g:starter:1"));
    }

    #[test]
    fn a_version_resolution_upgraded_says_both() {
        let mut upgraded = node("g:core:2", Vec::new());
        upgraded.requested = Some("g:core:1".into());

        let artifacts = analyze(&model(vec![node("g:starter:1", vec![upgraded])]));

        let core = &artifacts.iter().find(|a| a.name == "g:core").unwrap();
        assert!(core.conflict());
        assert_eq!(
            core.usages[0].mark().as_deref(),
            Some("1 → 2"),
            "the asked version comes before the chosen one"
        );
    }

    #[test]
    fn a_conflict_free_artifact_is_not_marked() {
        let artifacts = analyze(&model(vec![node(
            "g:starter:1",
            vec![node("g:web:1", Vec::new())],
        )]));

        let web = artifacts.iter().find(|a| a.name == "g:web").unwrap();
        assert!(!web.conflict());
        assert_eq!(web.usages[0].mark(), None);
        assert_eq!(web.selected, "1");
    }

    #[test]
    fn the_declaration_is_found_in_both_kinds_of_file() {
        let pom = "<dependency>\n  <artifactId>jackson-databind</artifactId>\n</dependency>";
        let script = "implementation(\"com.fasterxml.jackson.core:jackson-databind:2.17.0\")";

        let at = declaration_offset(pom, "com.fasterxml.jackson.core:jackson-databind:2.17.0");
        assert_eq!(at, Some(pom.find("<artifactId>").unwrap()));
        let at = declaration_offset(script, "com.fasterxml.jackson.core:jackson-databind:2.17.0");
        assert_eq!(at, Some(script.find("\"com.").unwrap() + 1));
        assert_eq!(declaration_offset("nothing here", "g:a:1"), None);
    }
}

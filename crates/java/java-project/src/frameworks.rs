//! The frameworks a module of a project uses, read from the dependencies of its model.
//! A feature that belongs to one framework is offered only inside a module that names it,
//! so a plain project sees no CDI badges and no keys of a framework it does not have.

use std::path::{Path, PathBuf};

use project_model::{Dependency, ModuleDependencies, ProjectModel};

/// A framework whose annotations and configuration a module may use.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framework {
    Spring,
    /// Jakarta or `javax` CDI, which Quarkus implements with ArC.
    Jakarta,
    Quarkus,
    Micronaut,
    /// The schema migrations of a project, by its tool.
    Flyway,
    Liquibase,
}

/// The frameworks of one module, as a small set.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Frameworks(u8);

impl Frameworks {
    pub const NONE: Self = Self(0);

    pub fn of(list: &[Framework]) -> Self {
        let mut found = Self::NONE;
        for framework in list {
            found.insert(*framework);
        }
        found
    }

    pub fn insert(&mut self, framework: Framework) {
        self.0 |= 1 << framework as u8;
    }

    pub fn contains(self, framework: Framework) -> bool {
        self.0 & (1 << framework as u8) != 0
    }

    pub fn any(self) -> bool {
        self.0 != 0
    }

    /// CDI beans, of Jakarta or of the ArC implementation of Quarkus.
    pub fn cdi(self) -> bool {
        self.contains(Framework::Jakarta) || self.contains(Framework::Quarkus)
    }
}

/// The frameworks the dependencies of one module name, with the transitive ones.
pub fn frameworks_of(dependencies: &ModuleDependencies) -> Frameworks {
    let mut found = Frameworks::NONE;
    for scope in &dependencies.scopes {
        for dependency in &scope.roots {
            collect(dependency, &mut found);
        }
    }
    found
}

fn collect(dependency: &Dependency, found: &mut Frameworks) {
    if let Some(framework) = framework_of(&dependency.coordinates) {
        found.insert(framework);
    }
    for child in &dependency.children {
        collect(child, found);
    }
}

/// The framework a `group:artifact:version` belongs to. A module id and any other
/// coordinate name no framework.
fn framework_of(coordinates: &str) -> Option<Framework> {
    let mut parts = coordinates.split(':');
    let group = parts.next()?;
    if !coordinates.contains(':') || group.is_empty() {
        return None;
    }
    let under = |prefix: &str| group == prefix || group.starts_with(&format!("{prefix}."));
    if under("io.quarkus") {
        return Some(Framework::Quarkus);
    }
    if under("io.micronaut") {
        return Some(Framework::Micronaut);
    }
    if under("org.flywaydb") {
        return Some(Framework::Flyway);
    }
    if under("org.liquibase") {
        return Some(Framework::Liquibase);
    }
    if under("jakarta.enterprise")
        || under("jakarta.platform")
        || under("jakarta.inject")
        || under("javax.enterprise")
        || under("javax.inject")
    {
        return Some(Framework::Jakarta);
    }
    if under("org.springframework") {
        return Some(Framework::Spring);
    }
    None
}

/// The frameworks of every source root of a project. A file takes the frameworks of the
/// longest root that holds it, so a nested module wins over its parent.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FrameworkLookup {
    roots: Vec<(PathBuf, Frameworks)>,
}

impl FrameworkLookup {
    /// The roots of every module of the model with the frameworks of its dependencies.
    pub fn of(model: &ProjectModel) -> Self {
        let mut roots = Vec::new();
        for module in &model.modules {
            let frameworks = model
                .dependencies_of(&module.id)
                .map_or(Frameworks::NONE, frameworks_of);
            if !frameworks.any() {
                continue;
            }
            for unit in &module.units {
                for root in &unit.source_roots {
                    roots.push((root.clone(), frameworks));
                }
            }
        }
        Self::of_roots(roots)
    }

    /// The lookup of roots the caller names, for a test that has no model.
    pub fn of_roots(mut roots: Vec<(PathBuf, Frameworks)>) -> Self {
        roots.sort_by(|left, right| {
            right
                .0
                .components()
                .count()
                .cmp(&left.0.components().count())
                .then_with(|| left.0.cmp(&right.0))
        });
        roots.dedup_by(|left, right| left.0 == right.0);
        Self { roots }
    }

    /// The frameworks of the module a file belongs to, or none outside every root.
    pub fn of_path(&self, path: &Path) -> Frameworks {
        self.roots
            .iter()
            .find(|(root, _)| path.starts_with(root))
            .map_or(Frameworks::NONE, |(_, frameworks)| *frameworks)
    }

    /// Every framework of the project, for a reader that answers for all of it.
    pub fn all(&self) -> Frameworks {
        let mut found = Frameworks::NONE;
        for (_, frameworks) in &self.roots {
            found.0 |= frameworks.0;
        }
        found
    }

    pub fn is_empty(&self) -> bool {
        self.roots.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use project_model::{DependencyScope, Module, Unit};

    fn dependency(coordinates: &str, children: Vec<Dependency>) -> Dependency {
        Dependency {
            coordinates: coordinates.to_owned(),
            children,
            ..Dependency::default()
        }
    }

    fn dependencies(list: Vec<Dependency>) -> ModuleDependencies {
        module_dependencies("app", list)
    }

    fn module_dependencies(module: &str, list: Vec<Dependency>) -> ModuleDependencies {
        ModuleDependencies {
            module: module.to_owned(),
            scopes: vec![DependencyScope {
                name: "compile".to_owned(),
                roots: list,
            }],
            ..ModuleDependencies::default()
        }
    }

    #[test]
    fn a_quarkus_dependency_of_any_artifact_marks_the_module() {
        let found = frameworks_of(&dependencies(vec![dependency(
            "io.quarkus:quarkus-resteasy-reactive:3.9.0",
            Vec::new(),
        )]));
        assert!(found.contains(Framework::Quarkus));
        assert!(!found.contains(Framework::Micronaut));
    }

    #[test]
    fn a_micronaut_dependency_below_another_one_counts() {
        let found = frameworks_of(&dependencies(vec![dependency(
            "com.example:app:1",
            vec![dependency(
                "io.micronaut:micronaut-http-server:4.2.0",
                Vec::new(),
            )],
        )]));
        assert!(found.contains(Framework::Micronaut));
    }

    #[test]
    fn the_cdi_packages_of_jakarta_and_javax_are_recognized() {
        let jakarta = frameworks_of(&dependencies(vec![dependency(
            "jakarta.enterprise:jakarta.enterprise.cdi-api:4.0.1",
            Vec::new(),
        )]));
        assert!(jakarta.cdi());
        let platform = frameworks_of(&dependencies(vec![dependency(
            "jakarta.platform:jakarta.jakartaee-api:10.0.0",
            Vec::new(),
        )]));
        assert!(platform.contains(Framework::Jakarta));
        let legacy = frameworks_of(&dependencies(vec![dependency(
            "javax.enterprise:cdi-api:2.0",
            Vec::new(),
        )]));
        assert!(legacy.contains(Framework::Jakarta));
    }

    #[test]
    fn a_project_without_a_framework_has_none() {
        let found = frameworks_of(&dependencies(vec![
            dependency("com.example:shop:1", Vec::new()),
            dependency(":neighbour", Vec::new()),
        ]));
        assert_eq!(found, Frameworks::NONE);
        assert!(!found.any());
    }

    #[test]
    fn the_migration_tools_are_recognized_by_their_groups() {
        let flyway = frameworks_of(&dependencies(vec![dependency(
            "org.flywaydb:flyway-core:10.20.0",
            Vec::new(),
        )]));
        assert!(flyway.contains(Framework::Flyway));
        assert!(!flyway.contains(Framework::Liquibase));

        let liquibase = frameworks_of(&dependencies(vec![dependency(
            "com.example:shop:1",
            vec![dependency(
                "org.liquibase:liquibase-core:4.29.0",
                Vec::new(),
            )],
        )]));
        assert!(liquibase.contains(Framework::Liquibase));
        assert!(!liquibase.contains(Framework::Flyway));
    }

    #[test]
    fn a_file_takes_the_frameworks_of_the_longest_root() {
        let model = ProjectModel {
            modules: vec![
                Module {
                    id: "root".to_owned(),
                    units: vec![Unit {
                        name: "main".to_owned(),
                        source_roots: vec![PathBuf::from("C:/shop/src")],
                        ..Unit::default()
                    }],
                    ..Module::default()
                },
                Module {
                    id: "web".to_owned(),
                    units: vec![Unit {
                        name: "main".to_owned(),
                        source_roots: vec![PathBuf::from("C:/shop/web/src")],
                        ..Unit::default()
                    }],
                    ..Module::default()
                },
            ],
            dependencies: vec![
                module_dependencies(
                    "root",
                    vec![dependency(
                        "org.springframework:spring-web:6.1.0",
                        Vec::new(),
                    )],
                ),
                module_dependencies(
                    "web",
                    vec![dependency("io.quarkus:quarkus-arc:3.9.0", Vec::new())],
                ),
            ],
            ..ProjectModel::default()
        };

        let lookup = FrameworkLookup::of(&model);

        assert!(
            lookup
                .of_path(Path::new("C:/shop/src/demo/Shop.java"))
                .contains(Framework::Spring)
        );
        assert_eq!(
            lookup.of_path(Path::new("C:/shop/web/src/demo/Shop.java")),
            Frameworks::of(&[Framework::Quarkus])
        );
        assert_eq!(
            lookup.of_path(Path::new("C:/other/Shop.java")),
            Frameworks::NONE
        );
        assert!(lookup.all().contains(Framework::Spring));
    }
}

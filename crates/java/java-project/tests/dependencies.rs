//! The dependency graph of a real project: Maven resolves PetClinic from the prepared
//! repository, Gradle resolves a small build from the prepared cache. Both run only with
//! `--ignored`, because neither starts without the corpus and the local repositories:
//!
//! ```text
//! IDE_CORPUS=<folder that holds src> MAVEN_USER_HOME=<folder with repository and wrapper>
//! IDE_HELPERS_DIR=<folder with maven-ext.jar>
//! cargo test -p java-project --test dependencies -- --ignored --test-threads=1
//! ```

use std::collections::HashSet;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use base::Cancel;
use java_project::{BuildEnv, Gradle, Jdk, Maven};
use project_model::{
    BuildSystem, Coordinates, Dependency, MAIN_UNIT, Module, PathEntry, PathKind, ProjectModel,
};

fn corpus() -> PathBuf {
    env::var_os("IDE_CORPUS")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../corpus")))
}

/// `target/debug/helpers`, or what the run of the suite passed as `IDE_HELPERS_DIR`.
fn helpers_dir() -> PathBuf {
    env::var_os("IDE_HELPERS_DIR").map_or_else(
        || {
            env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .with_file_name("helpers")
        },
        PathBuf::from,
    )
}

/// The local repository the environment prepared, and the cache beside it.
fn maven_home() -> PathBuf {
    PathBuf::from(env::var("MAVEN_USER_HOME").expect("set MAVEN_USER_HOME"))
}

fn jdk() -> Jdk {
    let home = corpus().join("jdk").join("17");
    Jdk::at(&home).unwrap_or_else(|| panic!("no JDK 17 at {}", home.display()))
}

fn maven_cmd() -> PathBuf {
    let mut pending = vec![maven_home().join("wrapper").join("dists")];
    while let Some(folder) = pending.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "mvn.cmd") {
                return path;
            }
        }
    }
    panic!("no mvn.cmd in the wrapper dists of MAVEN_USER_HOME");
}

/// A project that runs its own copy of the wrapper of the environment.
fn wrapper(root: &Path) {
    let command = maven_cmd();
    fs::write(
        root.join("mvnw.cmd"),
        format!("@echo off\r\ncall \"{}\" %*\r\n", command.display()),
    )
    .unwrap();
}

fn environment(cache: PathBuf) -> BuildEnv {
    let mut env = BuildEnv {
        jdk: Some(jdk()),
        cache_root: Some(cache),
        helpers: Some(helpers_dir()),
        ..BuildEnv::default()
    };
    env.build_tools.first.offline = true;
    let repository = maven_home().join("repository");
    env.maven_args = vec![format!("-Dmaven.repo.local={}", repository.display())];
    env
}

/// The project files of a corpus project, without its outputs and history.
fn copy_project(name: &str, to: &Path, skipped: &[&str]) {
    let from = corpus().join("src").join(name);
    copy_tree(&from, to, skipped);
    assert!(
        to.join("pom.xml").is_file() || to.join("build.gradle").is_file(),
        "{} has no project file",
        from.display()
    );
}

fn copy_tree(from: &Path, to: &Path, skipped: &[&str]) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().into_owned();
        if skipped.contains(&name.as_str()) {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &to.join(&name), skipped);
        } else {
            fs::copy(entry.path(), to.join(&name)).unwrap();
        }
    }
}

/// Walks a tree and returns every edge, the root first.
fn all_nodes<'a>(roots: &'a [Dependency], out: &mut Vec<&'a Dependency>) {
    for root in roots {
        out.push(root);
        all_nodes(&root.children, out);
    }
}

fn nodes<'a>(model: &'a ProjectModel, module: &str) -> Vec<&'a Dependency> {
    let mut out = Vec::new();
    let Some(found) = model.dependencies_of(module) else {
        return out;
    };
    for scope in &found.scopes {
        all_nodes(&scope.roots, &mut out);
    }
    out
}

fn conflicts<'a>(model: &'a ProjectModel, module: &str) -> Vec<&'a Dependency> {
    nodes(model, module)
        .into_iter()
        .filter(|node| node.winner.is_some() || node.requested.is_some())
        .collect()
}

/// The class path entries of a library whose coordinates contain `artifact`.
fn libraries<'a>(entries: &'a [PathEntry], artifact: &str) -> Vec<&'a str> {
    entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            PathKind::Library { coordinates } if coordinates.contains(artifact) => {
                Some(coordinates.as_str())
            }
            _ => None,
        })
        .collect()
}

/// `group:artifact` of every library on a class path, one name per entry.
fn artifact_names(entries: &[PathEntry]) -> Vec<String> {
    entries
        .iter()
        .filter_map(|entry| match &entry.kind {
            PathKind::Library { coordinates } => {
                let (group, rest) = coordinates.split_once(':')?;
                let artifact = rest.split_once(':')?.0;
                Some(format!("{group}:{artifact}"))
            }
            _ => None,
        })
        .collect()
}

/// The first module whose dependencies the tool reported.
fn main_module(model: &ProjectModel) -> &Module {
    let found = model
        .dependencies
        .iter()
        .find(|found| !found.scopes.is_empty())
        .expect("a module with dependencies");
    model
        .module(&found.module)
        .expect("the module of the graph")
}

#[test]
#[ignore = "needs the corpus, MAVEN_USER_HOME and the helpers"]
fn maven_reports_the_tree_of_petclinic_with_its_children() {
    let root = test_support::TempDir::new("dependencies-petclinic");
    copy_project("petclinic", root.path(), &[".git", "target", "build"]);
    wrapper(root.path());
    let tool = Maven::new(environment(root.path().join("cache")));

    let model = tool
        .sync(root.path(), &Cancel::new())
        .expect("Maven reads the project");

    assert!(model.resolved);
    let module = main_module(&model);
    let found = model.dependencies_of(&module.id).unwrap();
    assert!(
        found.build_file.is_some(),
        "the report names the file the dependencies are declared in"
    );
    let compile = found
        .scope("compile")
        .expect("the compilation scope is there");
    assert!(!compile.roots.is_empty());
    let starter = nodes(&model, &module.id)
        .into_iter()
        .find(|node| node.coordinates.contains("spring-boot-starter-web"))
        .expect("spring-boot-starter-web is in the graph");
    assert!(
        !starter.children.is_empty(),
        "the starter keeps what it pulls in"
    );
    assert!(
        compile.roots.iter().any(|root| root.scope != "test"),
        "a compilation scope has no test-only root"
    );
    // The graph keeps the versions that lost a conflict, the class path holds the
    // versions that won only: no artifact gets two jars.
    let main = module.unit(MAIN_UNIT).expect("the main unit");
    for (name, entries) in [
        ("compile", &main.compile_paths),
        ("runtime", &main.runtime_paths),
    ] {
        let names = artifact_names(entries);
        let unique: HashSet<&String> = names.iter().collect();
        assert_eq!(
            unique.len(),
            names.len(),
            "the {name} class path has one version of every artifact"
        );
    }
}

#[test]
#[ignore = "needs the corpus, MAVEN_USER_HOME and the helpers"]
fn maven_names_the_version_it_chose_instead_of_the_one_that_lost() {
    let root = test_support::TempDir::new("dependencies-conflict");
    fs::create_dir_all(root.path()).unwrap();
    wrapper(root.path());
    // Two paths to jackson-databind: the direct one is nearer, the one of the starter
    // asks for the version of the Spring Boot BOM and loses.
    fs::write(
        root.path().join("pom.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <groupId>demo</groupId>
    <artifactId>conflict</artifactId>
    <version>1.0</version>
    <dependencies>
        <dependency>
            <groupId>org.springframework.boot</groupId>
            <artifactId>spring-boot-starter-web</artifactId>
            <version>4.1.1</version>
        </dependency>
        <dependency>
            <groupId>com.fasterxml.jackson.core</groupId>
            <artifactId>jackson-databind</artifactId>
            <version>2.17.2</version>
        </dependency>
    </dependencies>
</project>
"#,
    )
    .unwrap();
    let tool = Maven::new(environment(root.path().join("cache")));

    let model = tool
        .sync(root.path(), &Cancel::new())
        .expect("Maven reads the project");

    let module = main_module(&model);
    let lost = conflicts(&model, &module.id)
        .into_iter()
        .find(|node| {
            node.coordinates
                .starts_with("com.fasterxml.jackson.core:jackson-annotations:")
        })
        .expect("a version of jackson-annotations lost the conflict");
    let winner = lost.winner.as_deref().expect("the loser names the winner");
    assert!(winner.starts_with("com.fasterxml.jackson.core:jackson-annotations:"));
    assert_ne!(winner, lost.coordinates);
    // The graph keeps the losing node, the class path must not: one version, the winner.
    let chosen = Coordinates::parse(winner)
        .expect("the winner has coordinates")
        .version;
    let main = module.unit(MAIN_UNIT).expect("the main unit");
    for (name, entries) in [
        ("compile", &main.compile_paths),
        ("runtime", &main.runtime_paths),
    ] {
        let found = libraries(entries, "jackson-annotations");
        assert_eq!(
            found.len(),
            1,
            "the {name} class path holds one jackson-annotations, not {found:?}"
        );
        assert!(
            found[0].ends_with(&format!(":{chosen}")),
            "the {name} class path holds the version that won, not {found:?}"
        );
    }
    assert!(
        !model
            .problems
            .iter()
            .any(|problem| problem.contains("not resolved")),
        "a version that lost is no resolution problem: {:?}",
        model.problems
    );
}

#[test]
#[ignore = "needs the corpus, MAVEN_USER_HOME, the Gradle cache and the helpers"]
fn gradle_reports_the_tree_with_the_version_it_chose() {
    let root = test_support::TempDir::new("dependencies-gradle");
    let project = corpus().join("src").join("dgs-framework");
    for file in [
        "gradlew.bat",
        "gradle/wrapper/gradle-wrapper.jar",
        "gradle/wrapper/gradle-wrapper.properties",
    ] {
        let to = root.path().join(file);
        fs::create_dir_all(to.parent().unwrap()).unwrap();
        fs::copy(project.join(file), &to).unwrap();
    }
    fs::write(
        root.path().join("settings.gradle"),
        "rootProject.name = 'sample'\n",
    )
    .unwrap();
    // Two paths to jackson-databind of different versions: resolution keeps the higher.
    fs::write(
        root.path().join("build.gradle"),
        r#"plugins { id 'java' }
repositories { mavenCentral() }
dependencies {
    implementation 'com.fasterxml.jackson.core:jackson-databind:2.20.1'
    implementation 'com.fasterxml.jackson.core:jackson-databind:2.21.5'
}
"#,
    )
    .unwrap();
    let mut env = environment(root.path().join("cache"));
    env.build_tools.second.offline = true;
    env.build_tools.second.runtime_home = jdk().home.display().to_string();
    if let Some(gradle) = maven_home().parent().map(|parent| parent.join("gradle"))
        && gradle.is_dir()
    {
        env.build_tools.second.user_home = gradle.display().to_string();
    }
    let tool = Gradle::new(env);

    let model = tool
        .sync(root.path(), &Cancel::new())
        .expect("Gradle reads the project");

    assert!(model.resolved);
    let module = main_module(&model);
    let found = model.dependencies_of(&module.id).unwrap();
    assert!(
        found.build_file.is_some(),
        "the report names the build file"
    );
    let compile = found
        .scope("compile")
        .expect("the compilation scope is there");
    let databind: Vec<&Dependency> = nodes(&model, &module.id)
        .into_iter()
        .filter(|node| {
            node.coordinates
                .starts_with("com.fasterxml.jackson.core:jackson-databind:")
        })
        .collect();
    assert!(
        databind.len() >= 2,
        "both edges to jackson-databind are kept"
    );
    assert!(
        databind.iter().any(
            |node| node.requested.is_some() && node.requested != Some(node.coordinates.clone())
        ),
        "the edge that asked for another version says so"
    );
    assert!(
        !compile.roots.is_empty(),
        "the compilation scope of the build is there"
    );
}

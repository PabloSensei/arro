//! Finding the Lombok jar of a project: the version from the model and the file from
//! the local repositories.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use java_project::{LocalRepositories, find_lombok, find_lombok_in_build_files};
use project_model::{MAIN_UNIT, Module, PathEntry, PathKind, ProjectModel, Unit};

fn scratch(name: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    let path = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join("lombok")
        .join(format!("{name}-{}-{serial}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn jar(path: &Path) -> PathBuf {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"jar").unwrap();
    path.to_path_buf()
}

fn model(coordinates: &str, path: Option<PathBuf>) -> ProjectModel {
    ProjectModel {
        root: PathBuf::from("project"),
        tool: "maven".into(),
        resolved: true,
        modules: vec![Module {
            id: "demo:app:1.0".into(),
            name: "app".into(),
            dir: PathBuf::from("project"),
            units: vec![Unit {
                name: MAIN_UNIT.into(),
                compile_paths: vec![PathEntry {
                    path: path.unwrap_or_else(|| PathBuf::from("missing.jar")),
                    kind: PathKind::Library {
                        coordinates: coordinates.into(),
                    },
                }],
                ..Unit::default()
            }],
        }],
        ..ProjectModel::default()
    }
}

fn maven_jar(repository: &Path, version: &str) -> PathBuf {
    jar(&repository
        .join("org/projectlombok/lombok")
        .join(version)
        .join(format!("lombok-{version}.jar")))
}

fn gradle_jar(cache: &Path, version: &str) -> PathBuf {
    jar(&cache
        .join("org.projectlombok/lombok")
        .join(version)
        .join("a1b2c3")
        .join(format!("lombok-{version}.jar")))
}

#[test]
fn a_maven_model_gets_the_jar_of_its_version() {
    let root = scratch("maven");
    let repository = root.join("repository");
    maven_jar(&repository, "1.18.30");
    maven_jar(&repository, "1.18.38");
    let repositories = LocalRepositories {
        maven: vec![repository],
        gradle: Vec::new(),
    };

    let found = find_lombok(
        &model("org.projectlombok:lombok:jar:1.18.38", None),
        &repositories,
    )
    .expect("the model names Lombok");

    assert_eq!(found.version, "1.18.38");
    assert!(found.jar.ends_with("lombok-1.18.38.jar"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_gradle_model_gets_the_jar_from_its_cache() {
    let root = scratch("gradle");
    let cache = root.join("files-2.1");
    gradle_jar(&cache, "1.18.30");
    gradle_jar(&cache, "1.18.38");
    let repositories = LocalRepositories {
        maven: Vec::new(),
        gradle: vec![cache],
    };

    let found = find_lombok(
        &model("org.projectlombok:lombok:1.18.38", None),
        &repositories,
    )
    .expect("the model names Lombok");

    assert_eq!(found.version, "1.18.38");
    assert!(found.jar.ends_with("lombok-1.18.38.jar"));
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_model_without_a_version_takes_the_newest_jar() {
    let root = scratch("newest");
    let repository = root.join("repository");
    maven_jar(&repository, "1.18.30");
    maven_jar(&repository, "1.18.38");
    maven_jar(&repository, "1.18.40");
    let repositories = LocalRepositories {
        maven: vec![repository],
        gradle: Vec::new(),
    };

    let found = find_lombok(&model("org.projectlombok:lombok", None), &repositories)
        .expect("the model names Lombok");

    assert_eq!(found.version, "1.18.40");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_project_without_lombok_gets_none() {
    let root = scratch("absent");
    let repository = root.join("repository");
    maven_jar(&repository, "1.18.38");
    let repositories = LocalRepositories {
        maven: vec![repository],
        gradle: Vec::new(),
    };

    assert!(find_lombok(&model("com.google.guava:guava:33.0.0", None), &repositories).is_none());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_lombok_without_a_jar_gets_none() {
    let repositories = LocalRepositories::default();

    assert!(
        find_lombok(
            &model("org.projectlombok:lombok:jar:1.18.38", None),
            &repositories
        )
        .is_none()
    );
}

#[test]
fn the_jar_of_the_model_wins_over_the_repository() {
    let root = scratch("model-path");
    let repository = root.join("repository");
    maven_jar(&repository, "1.18.38");
    let own = root.join("libs/lombok-1.18.36.jar");
    let own = jar(&own);
    let repositories = LocalRepositories {
        maven: vec![repository],
        gradle: Vec::new(),
    };

    let found = find_lombok(
        &model("org.projectlombok:lombok:jar:1.18.36", Some(own.clone())),
        &repositories,
    )
    .expect("the dependency has a file");

    assert_eq!(found.version, "1.18.36");
    assert_eq!(found.jar, own);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_build_file_names_a_dependency_that_the_model_does_not_have_yet() {
    let root = scratch("build-file");
    fs::write(
        root.join("pom.xml"),
        "<project><dependencies><dependency>\
         <groupId>org.projectlombok</groupId><artifactId>lombok</artifactId>\
         <version>1.18.38</version></dependency></dependencies></project>",
    )
    .unwrap();
    let repository = root.join("repository");
    maven_jar(&repository, "1.18.38");
    let repositories = LocalRepositories {
        maven: vec![repository],
        gradle: Vec::new(),
    };

    let found = find_lombok_in_build_files(&root, &repositories).expect("the POM names Lombok");

    assert_eq!(found.version, "1.18.38");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_gradle_script_names_a_dependency_that_the_model_does_not_have_yet() {
    let root = scratch("gradle-script");
    fs::write(
        root.join("build.gradle"),
        "dependencies {\n    compileOnly 'org.projectlombok:lombok:1.18.38'\n}\n",
    )
    .unwrap();
    let cache = root.join("files-2.1");
    gradle_jar(&cache, "1.18.38");
    let repositories = LocalRepositories {
        maven: Vec::new(),
        gradle: vec![cache],
    };

    let found = find_lombok_in_build_files(&root, &repositories).expect("the script names Lombok");

    assert_eq!(found.version, "1.18.38");
    let _ = fs::remove_dir_all(root);
}

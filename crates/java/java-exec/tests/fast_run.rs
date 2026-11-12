//! The fast build before a run: a project that has not changed since the last successful
//! build starts without Maven. Needs a JDK and Maven, so it does not run in CI:
//!
//!   cargo test -p java-exec --test fast_run -- --ignored --test-threads=1

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use base::Cancel;
use java_project::{BuildEnv, Gradle, Jdk, Maven};
use proc_sup::capture;
use project_model::BuildSystem;

fn corpus() -> PathBuf {
    env::var_os("IDE_CORPUS").map_or_else(
        || PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/../../../corpus")),
        PathBuf::from,
    )
}

fn jdk() -> Jdk {
    let candidate = corpus().join("jdk").join("17");
    if let Some(jdk) = Jdk::at(&candidate) {
        return jdk;
    }
    let home = env::var_os("JAVA_HOME").expect("IE_CORPUS has no JDK 17 and JAVA_HOME is not set");
    Jdk::at(Path::new(&home)).expect("JAVA_HOME is not a JDK")
}

fn maven_cmd() -> PathBuf {
    if let Some(home) = env::var_os("MAVEN_HOME") {
        let command = PathBuf::from(home).join("bin").join("mvn.cmd");
        if command.is_file() {
            return command;
        }
    }
    let user = env::var_os("MAVEN_USER_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(|home| PathBuf::from(home).join(".m2")));
    if let Some(user) = user
        && let Some(found) = find_mvn(&user.join("wrapper").join("dists"), 0)
    {
        return found;
    }
    panic!("no mvn.cmd: set MAVEN_HOME or fill the wrapper dists of MAVEN_USER_HOME");
}

fn find_mvn(folder: &Path, depth: usize) -> Option<PathBuf> {
    if depth > 6 {
        return None;
    }
    let entries = fs::read_dir(folder).ok()?;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Some(found) = find_mvn(&path, depth + 1) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|name| name == "mvn.cmd") {
            return Some(path);
        }
    }
    None
}

fn temp_dir(name: &str) -> PathBuf {
    test_support::scratch(&format!("fast-run-{name}"))
}

fn project(root: &Path) {
    let source = root.join("src/main/java/demo/App.java");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(
        &source,
        "package demo;\npublic class App {\n    public static void main(String[] args) {}\n}\n",
    )
    .unwrap();
    fs::write(
        root.join("pom.xml"),
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project xmlns="http://maven.apache.org/POM/4.0.0">
    <modelVersion>4.0.0</modelVersion>
    <groupId>demo</groupId>
    <artifactId>app</artifactId>
    <version>1.0-SNAPSHOT</version>
    <properties>
        <maven.compiler.release>17</maven.compiler.release>
        <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>
    </properties>
    <build>
        <plugins>
            <plugin>
                <groupId>org.apache.maven.plugins</groupId>
                <artifactId>maven-resources-plugin</artifactId>
                <version>3.5.0</version>
            </plugin>
            <plugin>
                <groupId>org.apache.maven.plugins</groupId>
                <artifactId>maven-compiler-plugin</artifactId>
                <version>3.10.1</version>
            </plugin>
        </plugins>
    </build>
</project>
"#,
    )
    .unwrap();
    // The wrapper is what the tool looks for first; it hands over to the Maven of the
    // environment.
    let maven = maven_cmd();
    fs::write(
        root.join("mvnw.cmd"),
        format!("@echo off\r\ncall \"{}\" %*\r\n", maven.display()),
    )
    .unwrap();
}

/// A project that only needs the Gradle wrapper of the corpus and a JDK.
fn gradle_project(root: &Path) {
    let source = root.join("src/main/java/demo/App.java");
    fs::create_dir_all(source.parent().unwrap()).unwrap();
    fs::write(
        &source,
        "package demo;\npublic class App {\n    public static void main(String[] args) {}\n}\n",
    )
    .unwrap();
    fs::write(root.join("settings.gradle"), "rootProject.name = 'app'\n").unwrap();
    fs::write(
        root.join("build.gradle"),
        "plugins { id 'java' }\njava { toolchain { languageVersion = JavaLanguageVersion.of(17) } }\n",
    )
    .unwrap();
    // The wrapper is what the tool starts; its distribution waits in the Gradle home.
    let wrapper = corpus().join("src/petclinic/gradle/wrapper");
    let target = root.join("gradle/wrapper");
    fs::create_dir_all(&target).unwrap();
    fs::copy(
        wrapper.join("gradle-wrapper.jar"),
        target.join("gradle-wrapper.jar"),
    )
    .unwrap();
    fs::copy(
        wrapper.join("gradle-wrapper.properties"),
        target.join("gradle-wrapper.properties"),
    )
    .unwrap();
}

#[test]
#[ignore = "needs a JDK and Gradle with its distribution"]
fn a_second_run_of_an_unchanged_gradle_project_needs_no_gradle() {
    let root = temp_dir("gradle");
    gradle_project(&root);
    let env = BuildEnv {
        jdk: Some(jdk()),
        cache_root: Some(root.join("cache")),
        ..BuildEnv::default()
    };
    let tool = Gradle::new(env);

    let first = tool
        .assemble_if_needed(&root)
        .unwrap()
        .expect("the first run builds");
    let built = capture(&first, &Cancel::new()).expect("Gradle starts");
    assert!(
        built.status.success(),
        "Gradle failed: {}{}",
        built.stdout,
        built.stderr
    );
    tool.built(&root);

    assert!(
        tool.assemble_if_needed(&root).unwrap().is_none(),
        "the second run of an unchanged project calls no Gradle"
    );

    fs::write(
        root.join("src/main/java/demo/App.java"),
        "package demo;\npublic class App {\n    public static void main(String[] args) { }\n}\n",
    )
    .unwrap();
    assert!(
        tool.assemble_if_needed(&root).unwrap().is_some(),
        "a source change builds again"
    );
}

#[test]
#[ignore = "needs a JDK, Maven and its local repository"]
fn a_second_run_of_an_unchanged_maven_project_needs_no_maven() {
    let root = temp_dir("maven");
    project(&root);
    let env = BuildEnv {
        jdk: Some(jdk()),
        cache_root: Some(root.join("cache")),
        ..BuildEnv::default()
    };
    let tool = Maven::new(env);

    let first = tool
        .assemble_if_needed(&root)
        .unwrap()
        .expect("the first run builds");
    let built = capture(&first, &Cancel::new()).expect("Maven starts");
    assert!(
        built.status.success(),
        "Maven failed: {}{}",
        built.stdout,
        built.stderr
    );
    tool.built(&root);

    assert!(
        tool.assemble_if_needed(&root).unwrap().is_none(),
        "the second run of an unchanged project calls no Maven"
    );

    fs::write(
        root.join("src/main/java/demo/App.java"),
        "package demo;\npublic class App {\n    public static void main(String[] args) { }\n}\n",
    )
    .unwrap();
    assert!(
        tool.assemble_if_needed(&root).unwrap().is_some(),
        "a source change builds again"
    );
}

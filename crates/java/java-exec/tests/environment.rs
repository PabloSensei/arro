//! The run and the build take the SDK the user chose in Project Structure, not the one
//! the build file asks for. Files only: no build tool and no SDK starts.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use java_exec::{JavaProvider, JavaTarget};
use java_project::{BuildEnv, Maven};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, MAIN_UNIT, Module, ProjectModel, Unit};
use runner::{RunConfig, RunConfigProvider};

fn scratch(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("java-exec-env-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

fn sdk(root: &Path, name: &str, version: &str) -> PathBuf {
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

fn project(root: &Path, selected: &Path) {
    fs::write(
        root.join("pom.xml"),
        "<project><properties><maven.compiler.release>17</maven.compiler.release>\
         </properties></project>",
    )
    .unwrap();
    // The wrapper is found before the Maven of the machine; it is never started here.
    fs::write(root.join("mvnw.cmd"), "@echo off\r\n").unwrap();
    let text = format!(
        "selected = \"{}\"\n",
        selected.display().to_string().replace('\\', "/")
    );
    fs::create_dir_all(root.join(".arro")).unwrap();
    fs::write(root.join(".arro").join("project.toml"), text).unwrap();
}

fn java_home_of(spec: &ProcessSpec) -> Option<PathBuf> {
    spec.environment()
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("JAVA_HOME"))
        .and_then(|(_, value)| value.clone())
        .map(PathBuf::from)
}

#[test]
fn the_build_and_the_run_use_the_chosen_sdk() {
    let root = scratch("plan");
    let seventeen = sdk(&root, "sdk-17", "17.0.2");
    let twenty_one = sdk(&root, "sdk-21", "21.0.2");
    project(&root, &twenty_one);
    let env = BuildEnv {
        cache_root: Some(root.join("cache")),
        ..BuildEnv::default()
    };
    let maven = Arc::new(Maven::new(env.clone()));
    let provider = JavaProvider::new(env, maven.clone());

    let build = maven.assemble(&root).unwrap();
    assert_eq!(java_home_of(&build), Some(twenty_one.clone()));
    assert_ne!(java_home_of(&build), Some(seventeen));

    let output = root.join("target/classes");
    fs::create_dir_all(&output).unwrap();
    let model = ProjectModel {
        root: root.clone(),
        tool: "maven".into(),
        resolved: true,
        modules: vec![Module {
            id: "app".into(),
            name: "app".into(),
            dir: root.clone(),
            units: vec![Unit {
                name: MAIN_UNIT.into(),
                output_dirs: vec![output],
                ..Unit::default()
            }],
        }],
        ..ProjectModel::default()
    };
    let config = RunConfig {
        name: "App".into(),
        provider: java_exec::PROVIDER_ID.into(),
        assemble_first: false,
        settings: toml::Table::try_from(JavaTarget {
            module: "app".into(),
            main_class: "demo.App".into(),
            ..JavaTarget::default()
        })
        .unwrap(),
    };

    let plan = provider.plan(&config, &model).unwrap();

    assert_eq!(java_home_of(&plan.program), Some(twenty_one));
}

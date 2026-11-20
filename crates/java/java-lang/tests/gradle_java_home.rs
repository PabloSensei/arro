//! The JVM of Gradle in the Settings reaches the initialization options of jdtls.
//!
//! No server starts here: the chain from the settings page to the settings the server
//! reads at its start is checked on its own.

use std::fs;
use std::path::{Path, PathBuf};

use java_lang::{JdtlsConfig, Toolchain, config_of_with};
use project_model::{BuildToolSettings, BuildTools};

fn toolchain() -> Toolchain {
    Toolchain {
        java_home: PathBuf::from(r"C:\jdk\25"),
        jdtls_home: PathBuf::from(r"C:\tools\jdtls"),
        debug_plugin: PathBuf::from(r"C:\tools\java-debug.jar"),
    }
}

fn project(name: &str) -> PathBuf {
    let root = test_support::scratch(&format!("gradle-java-home-{name}"));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("build.gradle"), "plugins { id 'java' }\n").unwrap();
    root
}

fn config(root: &Path, runtime_home: &str) -> JdtlsConfig {
    let build_tools = BuildTools {
        second: BuildToolSettings {
            runtime_home: runtime_home.to_owned(),
            ..BuildToolSettings::default()
        },
        ..BuildTools::default()
    };
    config_of_with(&toolchain(), root, &root.join("state"), &build_tools)
}

#[test]
fn the_gradle_jvm_of_the_settings_reaches_the_server() {
    let root = project("set");
    let config = config(&root, r"C:\jdk\21");

    assert_eq!(
        config.gradle_java_home.as_deref(),
        Some(std::path::Path::new(r"C:\jdk\21"))
    );
    let settings = jdtls_adapter::settings_of(&config);
    assert_eq!(
        settings["java"]["import"]["gradle"]["java"]["home"],
        serde_json::json!("C:/jdk/21")
    );
    assert_eq!(
        settings["java"]["import"]["gradle"]["wrapper"]["enabled"],
        serde_json::json!(true)
    );
}

#[test]
fn without_a_gradle_jvm_the_server_chooses_one() {
    let root = project("empty");
    let config = config(&root, "  ");

    assert!(config.gradle_java_home.is_none());
    let settings = jdtls_adapter::settings_of(&config);
    assert!(
        settings["java"]["import"]["gradle"].get("java").is_none(),
        "{settings}"
    );
}

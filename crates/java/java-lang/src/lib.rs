//! The facade the application uses to reach everything about the language: syntax for the
//! editor, and `lang-api` traits over one open project backed by jdtls.

mod debug;
mod environment;
mod language;
mod live_templates;
mod lombok;
mod problems;
mod project;
mod session;
mod setup;

pub use debug::{DebugExtension, DebugSession, JavaDebugger, ResolvedMain};
pub use environment::{JavaEnvironment, project_environment};

use std::path::{Path, PathBuf};
use std::sync::Arc;

use java_exec::{JavaProvider, JavaTestProvider, parse_tests};
use java_project::{BuildEnv, detect_build_system};
use lang_api::SymbolProvider;
use project_model::BuildSystem;
use runner::{RunConfigProvider, TestProvider, TestSite};
use syntax::LanguageConfig;

pub use jdtls_adapter::{ImportFailure, JdtlsConfig, Runtime};
pub use language::{JavaSyntax, LANGUAGE_ID};
pub use live_templates::templates_for;
pub use project::{JavaProject, OpenError};
pub use session::{
    DEBUG_PLUGIN, JAVA_HOME, JDTLS_HOME, JavaSession, MAVEN_SETTINGS, OFFLINE, config_from_env,
    config_of, open_error_text, project_state_dir, status_text,
};
pub use setup::{Missing, Search, Toolchain, discover};
pub use sidecar::{State, Status, Trust};

fn is_source(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("java"))
}

pub fn language_for(path: &Path) -> Option<LanguageConfig> {
    is_source(path).then(java_syntax::config)
}

pub fn symbols_for(path: &Path) -> Option<&'static dyn SymbolProvider> {
    is_source(path).then(|| java_syntax::symbols() as &dyn SymbolProvider)
}

/// How a project builds and runs.
pub struct ProjectBuild {
    pub tool: Option<Arc<dyn BuildSystem>>,
    pub providers: Vec<Arc<dyn RunConfigProvider>>,
    /// Runs tests through the build tool.
    pub tests: Option<Arc<dyn TestProvider>>,
    pub cache_root: PathBuf,
}

/// The test classes and test methods of one Java source, for the run marks in the gutter.
pub fn test_sites(source: &str, path: &Path) -> Vec<TestSite> {
    let mut sites = Vec::new();
    for class in parse_tests(source, path) {
        let suite = class.class.clone();
        sites.push(TestSite {
            suite: suite.clone(),
            case: None,
            line: class.line,
        });
        for method in class.methods {
            sites.push(TestSite {
                suite: suite.clone(),
                case: Some(method.name),
                line: method.line,
            });
        }
    }
    sites.sort_by_key(|site| site.line);
    sites
}

/// The build tool of the project in `root` and the provider of its run configurations.
/// `cache_root` moves the model cache, the default is the user cache.
pub fn project_build(root: &Path, cache_root: Option<PathBuf>) -> ProjectBuild {
    let env = BuildEnv {
        cache_root,
        ..BuildEnv::default()
    };
    let tool = detect_build_system(root, &env);
    let providers = tool.iter().map(|tool| {
        let provider = JavaProvider::new(env.clone(), tool.clone());
        Arc::new(provider) as Arc<dyn RunConfigProvider>
    });
    let tests = tool
        .is_some()
        .then(|| Arc::new(JavaTestProvider::new(env.clone())) as Arc<dyn TestProvider>);
    ProjectBuild {
        providers: providers.collect(),
        tests,
        cache_root: env.cache_root(),
        tool,
    }
}

/// The package a folder of the project stands for: the folders under `java`, joined by
/// dots. A folder outside any source root is the default package, which is empty.
pub fn package_of(root: &Path, folder: &Path) -> String {
    let relative = folder.strip_prefix(root).unwrap_or(folder);
    let parts: Vec<String> = relative
        .components()
        .map(|part| part.as_os_str().to_string_lossy().into_owned())
        .collect();
    let source_root = parts.iter().rposition(|part| part == "java");
    let inside = source_root.map_or(&[][..], |at| &parts[at + 1..]);
    inside.join(".")
}

/// The text of a new source file: `kind` is `class`, `interface`, `enum` or `record`.
pub fn new_source(kind: &str, root: &Path, folder: &Path, name: &str) -> String {
    let package = package_of(root, folder);
    let header = if package.is_empty() {
        String::new()
    } else {
        format!("package {package};\n\n")
    };
    let parens = if kind == "record" { "()" } else { "" };
    format!("{header}public {kind} {name}{parens} {{\n}}\n")
}

/// The package folder name of a project: lower case letters and digits only.
fn package_name(name: &str) -> String {
    let letters: String = name
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect();
    if letters.is_empty() || letters.starts_with(|c: char| c.is_ascii_digit()) {
        format!("app{letters}")
    } else {
        letters
    }
}

/// The artifact name: lower case words joined by dashes.
fn artifact_name(name: &str) -> String {
    let dashed: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    dashed.trim_matches('-').to_string()
}

/// The files of a new project, with paths inside its folder. `kind` is `maven` or
/// `gradle`. The files are written as they are; no wrapper script is created.
pub fn new_project_files(kind: &str, name: &str) -> Vec<(String, String)> {
    let package = package_name(name);
    let artifact = artifact_name(name);
    let main = format!(
        "package com.example.{package};\n\npublic class App {{\n    public static void main(String[] args) {{\n        System.out.println(\"Hello from {name}\");\n    }}\n}}\n"
    );
    let source = (
        format!("src/main/java/com/example/{package}/App.java"),
        main,
    );
    let build = if kind == "gradle" {
        let settings = format!("rootProject.name = '{artifact}'\n");
        let script = format!(
            "plugins {{\n    id 'java'\n    id 'application'\n}}\n\nrepositories {{\n    mavenCentral()\n}}\n\njava {{\n    toolchain {{\n        languageVersion = JavaLanguageVersion.of(17)\n    }}\n}}\n\napplication {{\n    mainClass = 'com.example.{package}.App'\n}}\n"
        );
        vec![
            ("settings.gradle".to_string(), settings),
            ("build.gradle".to_string(), script),
            (".gitignore".to_string(), "build/\n.gradle/\n".to_string()),
        ]
    } else {
        let pom = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<project xmlns=\"http://maven.apache.org/POM/4.0.0\">\n    <modelVersion>4.0.0</modelVersion>\n    <groupId>com.example</groupId>\n    <artifactId>{artifact}</artifactId>\n    <version>1.0-SNAPSHOT</version>\n    <properties>\n        <maven.compiler.release>17</maven.compiler.release>\n        <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>\n    </properties>\n</project>\n"
        );
        vec![
            ("pom.xml".to_string(), pom),
            (".gitignore".to_string(), "target/\n".to_string()),
        ]
    };
    let mut files = build;
    files.push(source);
    files
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn an_untrusted_project_starts_nothing_and_touches_no_disk() {
        let missing = PathBuf::from(r"Z:\does\not\exist");
        let config = JdtlsConfig {
            jdtls_home: missing.clone(),
            debug_plugin: missing.clone(),
            java_home: missing.clone(),
            project_root: missing.clone(),
            state_dir: missing,
            runtimes: Vec::new(),
            maven_settings: None,
            gradle_user_home: None,
            gradle_java_home: None,
            offline: true,
            environment: HashMap::new(),
        };
        let outcome = JavaProject::open(config, Trust::Untrusted);
        assert!(matches!(outcome, Err(OpenError::NotTrusted)));
    }

    #[test]
    fn a_folder_under_the_source_root_is_a_package() {
        let root = Path::new("C:/shop");
        let folder = root.join("src/main/java/com/shop/orders");
        assert_eq!(package_of(root, &folder), "com.shop.orders");
        assert_eq!(package_of(root, &root.join("src/main/java")), "");
        assert_eq!(package_of(root, &root.join("docs")), "");
    }

    #[test]
    fn a_new_source_has_its_package_and_a_body() {
        let root = Path::new("C:/shop");
        let folder = root.join("src/main/java/com/shop");
        let class = new_source("class", root, &folder, "Order");
        assert_eq!(class, "package com.shop;\n\npublic class Order {\n}\n");
        let record = new_source("record", root, &root.join("src/main/java"), "Point");
        assert_eq!(record, "public record Point() {\n}\n");
    }

    #[test]
    fn a_new_maven_project_has_a_pom_and_a_main_class_in_its_package() {
        let files = new_project_files("maven", "Shop Front");
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert!(paths.contains(&"pom.xml"));
        assert!(paths.contains(&"src/main/java/com/example/shopfront/App.java"));
        let pom = &files.iter().find(|(path, _)| path == "pom.xml").unwrap().1;
        assert!(pom.contains("<artifactId>shop-front</artifactId>"));
        let app = &files.last().unwrap().1;
        assert!(app.starts_with("package com.example.shopfront;"));
    }

    #[test]
    fn a_new_gradle_project_has_a_build_script_and_digits_do_not_start_a_package() {
        let files = new_project_files("gradle", "2fast");
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert!(paths.contains(&"build.gradle") && paths.contains(&"settings.gradle"));
        assert!(paths.contains(&"src/main/java/com/example/app2fast/App.java"));
        assert!(!paths.contains(&"pom.xml"));
    }
}

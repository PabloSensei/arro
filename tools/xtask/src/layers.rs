//! Layer rules from docs/ARCHITECTURE.md section 4.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::{Result, cargo};

const FORBIDDEN_WORDS: &[&str] = &[
    "java",
    "maven",
    "gradle",
    "jdtls",
    "jdwp",
    "classpath",
    "surefire",
    "junit",
];
const DIRECTORY_LITERALS: &[&str] = &["target", "build"];

/// Explicit `(package, word)` pairs allowed in neutral crates.
const WORD_EXCEPTIONS: &[(&str, &str)] = &[
    // `QuickJavaDoc` is the IntelliJ IDEA id of Quick Documentation, the key of its binding.
    ("keymap", "java"),
];

/// The only crates outside `crates/java` that may know a language. The application
/// ships; the test support tool only exists for the window tests of neutral crates.
const LANGUAGE_AWARE_APP: &str = "crates/app/ide";
const LANGUAGE_AWARE_TEST_SUPPORT: &str = "tools/window-language";
const LANGUAGE_FACADE: &str = "java-lang";

const UI_KIT: &str = "ui-kit";
const UI_FREE_CRATES: &[&str] = &[
    "crates/editor/text-core",
    "crates/editor/display-map",
    "crates/editor/keymap",
];

/// Published under the GPUI name, but free of the UI framework.
const GPUI_LIBRARIES: &[&str] = &[
    "gpui-pre-sum-tree",
    "gpui-pre-ztracing",
    "gpui-pre-ztracing-macro",
];

const WIN32_DEPENDENCY_PREFIXES: &[&str] = &["windows", "winapi", "winreg"];
const WIN32_CRATES: &[&str] = &[
    "crates/foundation/platform",
    "crates/foundation/proc-sup",
    "crates/services/terminal",
];
/// `extern "C"` reaches the same functions on x86-64, so the link attribute matters too.
const WIN32_SOURCE_MARKERS: &[&str] = &["extern \"system\"", "#[link(", "raw-dylib"];

const AI_ALLOWED_GROUPS: &[&str] = &["ai", "foundation", "services"];

pub fn run(root: &Path) -> Result {
    let graph = Graph::load(root)?;
    let mut violations = graph.dependency_violations();
    violations.extend(graph.source_violations()?);
    if violations.is_empty() {
        return Ok(());
    }
    for violation in &violations {
        eprintln!("error: {violation}");
    }
    Err(format!("{} layer violation(s)", violations.len()).into())
}

struct Package {
    name: String,
    /// Workspace-relative directory with `/` separators, `None` outside the workspace.
    path: Option<String>,
    directory: PathBuf,
}

impl Package {
    fn group(&self) -> Option<&str> {
        self.path
            .as_deref()?
            .strip_prefix("crates/")?
            .split('/')
            .next()
    }

    fn is_gpui(&self) -> bool {
        self.name.starts_with("gpui") && !GPUI_LIBRARIES.contains(&self.name.as_str())
    }

    fn is_ui(&self) -> bool {
        self.group() == Some("ui") || self.is_gpui()
    }

    fn is_win32(&self) -> bool {
        let mut prefixes = WIN32_DEPENDENCY_PREFIXES.iter();
        prefixes.any(|prefix| self.name.starts_with(prefix))
    }
}

struct Dependency {
    package: String,
    /// False for edges that exist only for tests of the dependent crate.
    linked: bool,
}

struct Graph {
    packages: HashMap<String, Package>,
    dependencies: HashMap<String, Vec<Dependency>>,
    members: Vec<String>,
}

impl Graph {
    fn load(root: &Path) -> Result<Self> {
        let output = Command::new(cargo())
            .args(["metadata", "--format-version", "1", "--all-features"])
            .arg("--manifest-path")
            .arg(root.join("Cargo.toml"))
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(format!("cargo metadata failed: {stderr}").into());
        }
        Self::parse(&serde_json::from_slice(&output.stdout)?)
    }

    fn parse(metadata: &Value) -> Result<Self> {
        let workspace_root = metadata["workspace_root"]
            .as_str()
            .ok_or("metadata has no workspace_root")?;

        let mut packages = HashMap::new();
        for package in array(&metadata["packages"]) {
            let manifest = Path::new(package["manifest_path"].as_str().unwrap_or_default());
            let directory = manifest.parent().ok_or("manifest has no parent")?;
            let path = directory
                .strip_prefix(workspace_root)
                .ok()
                .filter(|_| package["source"].is_null())
                .map(|path| path.to_string_lossy().replace('\\', "/"));
            packages.insert(
                string(&package["id"]),
                Package {
                    name: string(&package["name"]),
                    path,
                    directory: directory.to_path_buf(),
                },
            );
        }

        let mut dependencies = HashMap::new();
        for node in array(&metadata["resolve"]["nodes"]) {
            let edges = array(&node["deps"]).iter().map(|dependency| Dependency {
                package: string(&dependency["pkg"]),
                linked: array(&dependency["dep_kinds"])
                    .iter()
                    .any(|kind| kind["kind"].as_str() != Some("dev")),
            });
            dependencies.insert(string(&node["id"]), edges.collect());
        }

        let mut members: Vec<String> = array(&metadata["workspace_members"])
            .iter()
            .map(string)
            .collect();
        members.sort_by_key(|id| &packages[id].name);
        Ok(Self {
            packages,
            dependencies,
            members,
        })
    }

    fn direct(&self, id: &str) -> impl Iterator<Item = &Package> {
        let edges = self
            .dependencies
            .get(id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        edges.iter().map(|edge| &self.packages[&edge.package])
    }

    /// Everything reachable from `id`. Test-only edges count only on `id` itself:
    /// those of its dependencies never reach it.
    fn reachable(&self, id: &str) -> Vec<&Package> {
        let mut seen = HashSet::new();
        let edges = self
            .dependencies
            .get(id)
            .map(Vec::as_slice)
            .unwrap_or_default();
        let mut pending: Vec<&str> = edges.iter().map(|edge| edge.package.as_str()).collect();
        while let Some(next) = pending.pop() {
            if seen.insert(next) {
                let edges = self
                    .dependencies
                    .get(next)
                    .map(Vec::as_slice)
                    .unwrap_or_default();
                let linked = edges.iter().filter(|edge| edge.linked);
                pending.extend(linked.map(|edge| edge.package.as_str()));
            }
        }
        seen.into_iter().map(|id| &self.packages[id]).collect()
    }

    fn dependency_violations(&self) -> Vec<String> {
        let mut violations = Vec::new();
        for id in &self.members {
            let package = &self.packages[id];
            let Some(group) = package.group() else {
                continue;
            };
            for dependency in self.direct(id) {
                if let Some(rule) = direct_rule(package, group, dependency) {
                    let (from, to) = (&package.name, &dependency.name);
                    violations.push(format!("{from} must not depend on {to}: {rule}"));
                }
            }
            if must_stay_ui_free(package, group) {
                for dependency in self.reachable(id).iter().filter(|it| it.is_ui()) {
                    let (from, to) = (&package.name, &dependency.name);
                    violations.push(format!("{from} must not reach {to}: this crate has no UI"));
                }
            }
        }
        violations
    }

    fn source_violations(&self) -> Result<Vec<String>> {
        let mut violations = Vec::new();
        for id in &self.members {
            let package = &self.packages[id];
            let Some(path) = package
                .path
                .as_deref()
                .filter(|_| package.group().is_some())
            else {
                continue;
            };
            let neutral = package.group() != Some("java") && path != LANGUAGE_AWARE_APP;
            let win32 = WIN32_CRATES.contains(&path);
            for file in source_files(&package.directory)? {
                let text = String::from_utf8_lossy(&fs::read(&file)?).into_owned();
                let rust = file.extension().is_some_and(|extension| extension == "rs");
                let scan = FileScan {
                    package: &package.name,
                    file: &file,
                    text: &text,
                };
                if neutral {
                    violations.extend(scan.forbidden_words(rust));
                }
                if rust && !win32 {
                    violations.extend(scan.foreign_abi());
                }
            }
        }
        Ok(violations)
    }
}

fn array(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or_default()
}

fn string(value: &Value) -> String {
    value.as_str().unwrap_or_default().to_owned()
}

fn must_stay_ui_free(package: &Package, group: &str) -> bool {
    let path = package.path.as_deref().unwrap_or_default();
    group == "services" || UI_FREE_CRATES.contains(&path)
}

fn direct_rule(package: &Package, group: &str, dependency: &Package) -> Option<&'static str> {
    let target_group = dependency.group();
    let path = package.path.as_deref().unwrap_or_default();
    if target_group == Some("java") && group != "java" {
        let facade = (path == LANGUAGE_AWARE_APP || path == LANGUAGE_AWARE_TEST_SUPPORT)
            && dependency.name == LANGUAGE_FACADE;
        if !facade {
            return Some("crates outside crates/java do not depend on it");
        }
    }
    if dependency.is_gpui() && package.name != UI_KIT {
        return Some("only ui-kit depends on GPUI");
    }
    if dependency.is_win32() && !WIN32_CRATES.contains(&path) {
        return Some("Win32 lives in platform, proc-sup and terminal");
    }
    if group == "ai" && target_group.is_some_and(|it| !AI_ALLOWED_GROUPS.contains(&it)) {
        return Some("crates/ai may use foundation, services and ai only");
    }
    None
}

fn source_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut files = Vec::new();
    let mut pending = vec![directory.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(next)? {
            let path = entry?.path();
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if path.is_dir() {
                if name != "target" && !name.starts_with('.') {
                    pending.push(path);
                }
            } else if name == "Cargo.toml" || name.ends_with(".rs") {
                files.push(path);
            }
        }
    }
    Ok(files)
}

struct FileScan<'a> {
    package: &'a str,
    file: &'a Path,
    text: &'a str,
}

impl FileScan<'_> {
    fn forbidden_words(&self, rust: bool) -> Vec<String> {
        let mut violations = Vec::new();
        for (index, line) in self.text.lines().enumerate() {
            let found = forbidden_words(line).into_iter();
            let found = found.chain(rust.then(|| directory_literals(line)).into_iter().flatten());
            for word in found {
                if !WORD_EXCEPTIONS.contains(&(self.package, word)) {
                    let location = format!("{}:{}", self.file.display(), index + 1);
                    violations.push(format!(
                        "{location}: forbidden word `{word}` in neutral crate"
                    ));
                }
            }
        }
        violations
    }

    fn foreign_abi(&self) -> Vec<String> {
        let lines = self.text.lines().enumerate();
        let calls = lines.filter(|(_, line)| {
            let mut markers = WIN32_SOURCE_MARKERS.iter();
            markers.any(|marker| line.contains(marker))
        });
        calls
            .map(|(index, _)| {
                let location = format!("{}:{}", self.file.display(), index + 1);
                format!("{location}: Win32 lives in platform, proc-sup and terminal")
            })
            .collect()
    }
}

/// Splits on non-alphanumerics and camelCase humps, so `JAVA_HOME`, `javaHome` and
/// `ClassPath` all match while `javadoc` does not.
fn forbidden_words(line: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    for token in line.split(|c: char| !c.is_ascii_alphanumeric()) {
        let lower = token.to_ascii_lowercase();
        let mut hump_start = 0;
        let mut humps = Vec::new();
        for (index, _) in token.match_indices(|c: char| c.is_ascii_uppercase()) {
            let previous = token.as_bytes().get(index.wrapping_sub(1));
            if previous.is_some_and(u8::is_ascii_lowercase) {
                humps.push(&lower[hump_start..index]);
                hump_start = index;
            }
        }
        humps.push(&lower[hump_start..]);
        humps.push(&lower);
        found.extend(FORBIDDEN_WORDS.iter().filter(|word| humps.contains(word)));
    }
    found
}

/// Finds `target` and `build` as whole path segments of string literals.
fn directory_literals(line: &str) -> Vec<&'static str> {
    let mut found = Vec::new();
    for literal in line.split('"').skip(1).step_by(2) {
        for segment in literal.split(['/', '\\']) {
            found.extend(DIRECTORY_LITERALS.iter().filter(|word| **word == segment));
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::{directory_literals, forbidden_words};

    #[test]
    fn finds_words_in_every_spelling() {
        for line in [
            "// java",
            "JAVA_HOME",
            "java_home",
            "javaHome",
            "JavaParser",
            "let x = \"java\";",
        ] {
            assert_eq!(forbidden_words(line), ["java"], "{line}");
        }
        assert_eq!(
            forbidden_words("ClassPath classpath_entries"),
            ["classpath", "classpath"]
        );
        assert_eq!(forbidden_words("JUnit"), ["junit"]);
    }

    #[test]
    fn keeps_longer_words_that_only_contain_a_forbidden_one() {
        for line in ["javadoc", "javascript", "gradlew", "mavenish"] {
            assert_eq!(forbidden_words(line), [] as [&str; 0], "{line}");
        }
    }

    #[test]
    fn finds_directory_names_in_literals_only() {
        assert_eq!(directory_literals(r#"join("target")"#), ["target"]);
        assert_eq!(directory_literals(r#"join("build/libs")"#), ["build"]);
        assert_eq!(
            directory_literals(r#"join("out\\target\\classes")"#),
            ["target"]
        );
        assert_eq!(
            directory_literals("fn build(target: &str) {}"),
            [] as [&str; 0]
        );
        assert_eq!(
            directory_literals(r#"join("rebuild", "build.rs")"#),
            [] as [&str; 0]
        );
    }
}

use std::collections::HashMap;
use std::fs;
use std::path::Path;

use base::Result;
use project_model::{BuildProfile, MAIN_UNIT, Module, ProjectModel, TEST_UNIT, Unit};
use roxmltree::{Document, Node};

const RELEASE_PROPERTIES: &[&str] = &[
    "maven.compiler.release",
    "maven.compiler.target",
    "maven.compiler.source",
    "java.version",
];

/// What one POM says by itself, with no parent POM read and no dependency resolved.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pom {
    pub group: Option<String>,
    pub artifact: Option<String>,
    pub version: Option<String>,
    pub modules: Vec<String>,
    pub source_dir: Option<String>,
    pub test_source_dir: Option<String>,
    pub java_release: Option<String>,
    pub profiles: Vec<PomProfile>,
}

/// One profile a POM declares. The window shows it; nothing here decides whether it is
/// on, because that depends on the machine and the command line.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PomProfile {
    pub id: String,
    /// The profile turns on by itself when the user says nothing.
    pub active_by_default: bool,
    /// The condition the tool checks before turning the profile on, as a line for the
    /// window. `None` when only the command line turns the profile on.
    pub condition: Option<String>,
}

pub fn read_pom(file: &Path) -> Result<Pom> {
    let text = fs::read_to_string(file)?;
    let document =
        Document::parse(&text).map_err(|error| format!("{}: {error}", file.display()))?;
    let project = document.root_element();
    let parent = child(project, "parent");
    let inherited = |name| parent.and_then(|parent| text_of(parent, name));
    let build = child(project, "build");
    let properties = properties_of(project);
    let expand = |value: String| expand(&value, &properties);
    Ok(Pom {
        group: text_of(project, "groupId")
            .or_else(|| inherited("groupId"))
            .map(expand),
        artifact: text_of(project, "artifactId").map(expand),
        version: text_of(project, "version")
            .or_else(|| inherited("version"))
            .map(expand),
        modules: child(project, "modules").map_or_else(Vec::new, |modules| {
            let list = modules
                .children()
                .filter(|node| node.has_tag_name_local("module"));
            list.filter_map(|node| node.text())
                .map(str::trim)
                .map(str::to_owned)
                .collect()
        }),
        source_dir: build
            .and_then(|build| text_of(build, "sourceDirectory"))
            .map(expand),
        test_source_dir: build
            .and_then(|build| text_of(build, "testSourceDirectory"))
            .map(expand),
        java_release: RELEASE_PROPERTIES
            .iter()
            .find_map(|name| properties.get(*name))
            .map(|value| expand(value.clone())),
        profiles: profiles_of(project),
    })
}

trait LocalName {
    fn has_tag_name_local(&self, name: &str) -> bool;
}

impl LocalName for Node<'_, '_> {
    fn has_tag_name_local(&self, name: &str) -> bool {
        self.is_element() && self.tag_name().name() == name
    }
}

fn child<'a, 'input>(node: Node<'a, 'input>, name: &str) -> Option<Node<'a, 'input>> {
    node.children().find(|child| child.has_tag_name_local(name))
}

fn text_of(node: Node, name: &str) -> Option<String> {
    let text = child(node, name)?.text()?.trim();
    (!text.is_empty()).then(|| text.to_owned())
}

fn properties_of(project: Node) -> HashMap<String, String> {
    let Some(properties) = child(project, "properties") else {
        return HashMap::new();
    };
    let elements = properties.children().filter(Node::is_element);
    let pairs = elements.filter_map(|node| {
        let value = node.text()?.trim().to_owned();
        Some((node.tag_name().name().to_owned(), value))
    });
    pairs.collect()
}

/// `${name}` from the same file. Anything else stays as written: the sync will know better.
fn expand(value: &str, properties: &HashMap<String, String>) -> String {
    let inner = value
        .strip_prefix("${")
        .and_then(|rest| rest.strip_suffix('}'));
    let found = inner.and_then(|name| properties.get(name));
    found.map_or_else(|| value.to_owned(), Clone::clone)
}

/// The profiles a POM declares, in the order of the file.
fn profiles_of(project: Node) -> Vec<PomProfile> {
    let Some(profiles) = child(project, "profiles") else {
        return Vec::new();
    };
    profiles
        .children()
        .filter(|node| node.has_tag_name_local("profile"))
        .filter_map(|profile| {
            let id = text_of(profile, "id")?;
            let activation = child(profile, "activation");
            let active_by_default = activation
                .and_then(|node| text_of(node, "activeByDefault"))
                .is_some_and(|text| text.eq_ignore_ascii_case("true"));
            Some(PomProfile {
                id,
                active_by_default,
                condition: activation.and_then(activation_line),
            })
        })
        .collect()
}

/// The conditions of one `<activation>` as a line for the window. Maven turns the profile
/// on only when everything named here matches, so the parts are joined by `and`.
fn activation_line(activation: Node) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(jdk) = text_of(activation, "jdk") {
        parts.push(format!("JDK {jdk}"));
    }
    if let Some(os) = child(activation, "os") {
        let fields: Vec<String> = ["family", "name", "arch", "version"]
            .into_iter()
            .filter_map(|field| text_of(os, field).map(|value| format!("{field}={value}")))
            .collect();
        if !fields.is_empty() {
            parts.push(format!("OS {}", fields.join(", ")));
        }
    }
    if let Some(property) = child(activation, "property") {
        match (text_of(property, "name"), text_of(property, "value")) {
            (Some(name), Some(value)) => parts.push(format!("property {name} = {value}")),
            (Some(name), None) => parts.push(format!("property {name}")),
            (None, Some(value)) => parts.push(format!("property = {value}")),
            (None, None) => {}
        }
    }
    if let Some(file) = child(activation, "file") {
        if let Some(path) = text_of(file, "exists") {
            parts.push(format!("file exists {path}"));
        }
        if let Some(path) = text_of(file, "missing") {
            parts.push(format!("file missing {path}"));
        }
    }
    (!parts.is_empty()).then(|| parts.join(" and "))
}

/// The profiles of the POM files of the modules, one per name: an id that several modules
/// declare is listed once, by the first file that names it.
pub fn module_profiles(modules: &[Module]) -> Vec<BuildProfile> {
    let mut profiles: Vec<BuildProfile> = Vec::new();
    for module in modules {
        let file = module
            .build_file
            .clone()
            .unwrap_or_else(|| module.dir.join("pom.xml"));
        let Ok(pom) = read_pom(&file) else {
            continue;
        };
        for profile in pom.profiles {
            if profiles.iter().any(|known| known.name == profile.id) {
                continue;
            }
            profiles.push(BuildProfile {
                name: profile.id,
                active_by_default: profile.active_by_default,
                source: Some(file.clone()),
                condition: profile.condition,
            });
        }
    }
    profiles
}

/// Modules and their roots from the POM files alone, without starting anything. A module
/// that cannot be read becomes a problem, the rest of the project is still worth showing.
pub fn scan_poms(root: &Path) -> Result<ProjectModel> {
    read_pom(&root.join("pom.xml"))?;
    let mut model = ProjectModel {
        root: root.to_owned(),
        tool: "maven".into(),
        ..ProjectModel::default()
    };
    scan_module(root, &mut model);
    model.profiles = module_profiles(&model.modules);
    Ok(model)
}

fn scan_module(directory: &Path, model: &mut ProjectModel) {
    let file = directory.join("pom.xml");
    let pom = match read_pom(&file) {
        Ok(pom) => pom,
        Err(error) => return model.problems.push(error.to_string()),
    };
    if model.modules.iter().any(|module| module.dir == directory) {
        return;
    }
    let name = pom.artifact.clone().unwrap_or_default();
    let id = [&pom.group, &pom.artifact, &pom.version].map(|part| part.as_deref().unwrap_or(""));
    let units = vec![
        unit(directory, MAIN_UNIT, pom.source_dir.as_deref(), "classes"),
        unit(
            directory,
            TEST_UNIT,
            pom.test_source_dir.as_deref(),
            "test-classes",
        ),
    ];
    model.modules.push(Module {
        id: id.join(":"),
        name,
        dir: directory.to_owned(),
        build_file: Some(file.clone()),
        generated_roots: Vec::new(),
        excluded_roots: vec![directory.join("target")],
        units,
    });
    for module in &pom.modules {
        let nested = directory.join(module);
        // A module entry may name the POM itself instead of its directory.
        let nested = match nested.extension() {
            Some(_) if nested.is_file() => nested.parent().map_or(nested.clone(), Path::to_owned),
            _ => nested,
        };
        scan_module(&nested, model);
    }
}

fn unit(directory: &Path, name: &str, source_dir: Option<&str>, output: &str) -> Unit {
    let standard = directory.join("src").join(name);
    Unit {
        name: name.into(),
        source_roots: vec![
            source_dir.map_or_else(|| standard.join("java"), |dir| directory.join(dir)),
        ],
        resource_roots: vec![standard.join("resources")],
        output_dirs: vec![directory.join("target").join(output)],
        ..Unit::default()
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn temp_dir(name: &str) -> PathBuf {
        test_support::scratch(&format!("project-pom-{name}"))
    }

    fn write_pom(directory: &Path, text: &str) {
        fs::create_dir_all(directory).unwrap();
        fs::write(directory.join("pom.xml"), text).unwrap();
    }

    const PARENT: &str = r#"<?xml version="1.0"?>
        <project xmlns="http://maven.apache.org/POM/4.0.0">
          <groupId>demo</groupId><artifactId>parent</artifactId><version>1.0</version>
          <properties><java.version>17</java.version></properties>
          <modules><module>core</module><module>app/pom.xml</module></modules>
        </project>"#;

    #[test]
    fn a_module_inherits_coordinates_from_its_parent_section() {
        let root = temp_dir("inherit");
        let text = r#"<project>
              <parent><groupId>demo</groupId><artifactId>parent</artifactId>
                <version>1.0</version></parent>
              <artifactId>core</artifactId>
            </project>"#;
        write_pom(&root, text);

        let pom = read_pom(&root.join("pom.xml")).unwrap();

        assert_eq!(pom.group.as_deref(), Some("demo"));
        assert_eq!(pom.artifact.as_deref(), Some("core"));
        assert_eq!(pom.version.as_deref(), Some("1.0"));
    }

    #[test]
    fn the_release_comes_through_a_property() {
        let root = temp_dir("release");
        let text = "<project><properties><java.version>21</java.version>\
             <maven.compiler.release>${java.version}</maven.compiler.release>\
             </properties></project>";
        write_pom(&root, text);

        let pom = read_pom(&root.join("pom.xml")).unwrap();

        assert_eq!(pom.java_release.as_deref(), Some("21"));
    }

    #[test]
    fn a_scan_walks_the_modules() {
        let root = temp_dir("scan");
        write_pom(&root, PARENT);
        let core = "<project><parent><groupId>demo</groupId><version>1.0</version></parent>\
                    <artifactId>core</artifactId>\
                    <build><sourceDirectory>src/gen</sourceDirectory></build></project>";
        write_pom(&root.join("core"), core);
        let app = core.replace("core", "app");
        write_pom(&root.join("app"), &app);

        let model = scan_poms(&root).unwrap();

        let ids: Vec<_> = model
            .modules
            .iter()
            .map(|module| module.id.as_str())
            .collect();
        assert_eq!(ids, ["demo:parent:1.0", "demo:core:1.0", "demo:app:1.0"]);
        assert!(!model.resolved);
        assert!(model.problems.is_empty(), "{:?}", model.problems);
        let main = model.modules[1].unit(MAIN_UNIT).unwrap();
        assert_eq!(main.source_roots, [root.join("core").join("src/gen")]);
        assert_eq!(
            main.output_dirs,
            [root.join("core").join("target").join("classes")]
        );
    }

    #[test]
    fn a_broken_module_is_a_problem_and_not_the_end() {
        let root = temp_dir("broken");
        write_pom(&root, PARENT);
        write_pom(&root.join("core"), "not xml");

        let model = scan_poms(&root).unwrap();

        assert_eq!(model.modules.len(), 1);
        assert_eq!(model.problems.len(), 2);
    }

    #[test]
    fn modules_that_point_in_a_circle_end() {
        let root = temp_dir("circle");
        let text =
            "<project><artifactId>a</artifactId><modules><module>.</module></modules></project>";
        write_pom(&root, text);

        let model = scan_poms(&root).unwrap();

        assert!(model.modules.len() <= 2);
    }

    #[test]
    fn a_missing_root_is_an_error() {
        let root = temp_dir("missing");

        assert!(scan_poms(&root).is_err());
    }

    const PROFILES: &str = r#"<project>
          <artifactId>app</artifactId>
          <profiles>
            <profile><id>dev</id>
              <activation><activeByDefault>true</activeByDefault></activation>
            </profile>
            <profile><id>only-named</id></profile>
            <profile><id>jdk17</id><activation><jdk>17</jdk></activation></profile>
            <profile><id>windows</id><activation>
              <os><family>windows</family><arch>amd64</arch></os>
            </activation></profile>
            <profile><id>debug</id><activation>
              <property><name>debug</name><value>true</value></property>
            </activation></profile>
            <profile><id>generated</id><activation>
              <file><exists>${basedir}/gen</exists></file>
            </activation></profile>
          </profiles>
        </project>"#;

    #[test]
    fn the_profiles_of_a_pom_keep_their_default_and_their_conditions() {
        let root = temp_dir("profiles");
        write_pom(&root, PROFILES);

        let pom = read_pom(&root.join("pom.xml")).unwrap();

        let found: Vec<(&str, bool)> = pom
            .profiles
            .iter()
            .map(|profile| (profile.id.as_str(), profile.active_by_default))
            .collect();
        assert_eq!(
            found,
            [
                ("dev", true),
                ("only-named", false),
                ("jdk17", false),
                ("windows", false),
                ("debug", false),
                ("generated", false),
            ]
        );
        assert_eq!(pom.profiles[0].condition, None, "no condition is a default");
        assert_eq!(
            pom.profiles[1].condition, None,
            "only the command line turns it on"
        );
        assert_eq!(pom.profiles[2].condition.as_deref(), Some("JDK 17"));
        assert_eq!(
            pom.profiles[3].condition.as_deref(),
            Some("OS family=windows, arch=amd64")
        );
        assert_eq!(
            pom.profiles[4].condition.as_deref(),
            Some("property debug = true")
        );
        assert_eq!(
            pom.profiles[5].condition.as_deref(),
            Some("file exists ${basedir}/gen")
        );
    }

    #[test]
    fn a_scan_collects_the_profiles_of_every_module_once() {
        let root = temp_dir("scan-profiles");
        let parent = r#"<project><groupId>demo</groupId><artifactId>parent</artifactId>
            <version>1.0</version><modules><module>core</module><module>app</module></modules>
            <profiles><profile><id>dev</id>
              <activation><activeByDefault>true</activeByDefault></activation>
            </profile></profiles>
          </project>"#;
        write_pom(&root, parent);
        let module = r#"<project><parent><groupId>demo</groupId><version>1.0</version></parent>
            <artifactId>core</artifactId>
            <profiles><profile><id>dev</id></profile>
              <profile><id>core-only</id><activation><jdk>21</jdk></activation></profile>
            </profiles>
          </project>"#;
        write_pom(&root.join("core"), module);
        let app = r#"<project><parent><groupId>demo</groupId><version>1.0</version></parent>
            <artifactId>app</artifactId>
            <profiles><profile><id>dev</id></profile></profiles>
          </project>"#;
        write_pom(&root.join("app"), app);

        let model = scan_poms(&root).unwrap();

        let names: Vec<&str> = model
            .profiles
            .iter()
            .map(|profile| profile.name.as_str())
            .collect();
        assert_eq!(names, ["dev", "core-only"]);
        assert!(
            model.profiles[0].active_by_default,
            "the parent turns it on"
        );
        assert_eq!(
            model.profiles[0].source.as_deref(),
            Some(root.join("pom.xml").as_path())
        );
        assert_eq!(model.profiles[1].condition.as_deref(), Some("JDK 21"));
        assert_eq!(
            model.profiles[1].source.as_deref(),
            Some(root.join("core").join("pom.xml").as_path())
        );
    }
}

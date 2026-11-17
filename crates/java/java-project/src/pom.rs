use std::collections::HashMap;
use std::fs;
use std::path::Path;

use base::Result;
use project_model::{MAIN_UNIT, Module, ProjectModel, TEST_UNIT, Unit};
use roxmltree::{Document, Node};

const RELEASE_PROPERTIES: &[&str] = &[
    "maven.compiler.release",
    "maven.compiler.target",
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
}

//! Create Test for a Java class: the test libraries of the module, the defaults of the
//! window, the text of the new test class and the edit that adds a missing library.
//!
//! The window knows none of this. It sends the place of the class and gets back a
//! [`TestForm`], then sends the values of the form back and gets the file.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use lang_api::{
    DocumentEdit, TestChoice, TestFile, TestFolder, TestForm, TestFramework, TextPoint, TextSpan,
    WorkspaceChange,
};
use project_model::{ProjectModel, TEST_UNIT};

/// The version each library gets when Fix adds it. There is no network here, so the
/// values are the last ones this IDE shipped with.
const JUNIT5_VERSION: &str = "5.11.4";
const JUNIT4_VERSION: &str = "4.13.2";
const TESTNG_VERSION: &str = "7.10.2";

const NO_CLASS: &str = "The file declares no class to test";
const BAD_NAME: &str = "A test class needs a name that Java accepts";

/// A library a test class can be written for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TestLibrary {
    JUnit5,
    JUnit4,
    TestNg,
}

impl TestLibrary {
    pub fn all() -> [Self; 3] {
        [Self::JUnit5, Self::JUnit4, Self::TestNg]
    }

    /// The value the window sends back with the form.
    pub fn handle(self) -> &'static str {
        match self {
            Self::JUnit5 => "junit5",
            Self::JUnit4 => "junit4",
            Self::TestNg => "testng",
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::JUnit5 => "JUnit 5",
            Self::JUnit4 => "JUnit 4",
            Self::TestNg => "TestNG",
        }
    }

    pub fn from_handle(handle: &str) -> Option<Self> {
        Self::all()
            .into_iter()
            .find(|library| library.handle() == handle)
    }

    /// `group:artifact:version` of the library, for the build file.
    fn coordinates(self) -> String {
        match self {
            Self::JUnit5 => format!("org.junit.jupiter:junit-jupiter:{JUNIT5_VERSION}"),
            Self::JUnit4 => format!("junit:junit:{JUNIT4_VERSION}"),
            Self::TestNg => format!("org.testng:testng:{TESTNG_VERSION}"),
        }
    }

    /// Whether the class of the test is public. JUnit 5 finds a package-private one.
    fn public_class(self) -> bool {
        self != Self::JUnit5
    }

    fn setup_annotation(self) -> &'static str {
        match self {
            Self::JUnit5 => "@BeforeEach",
            Self::JUnit4 => "@Before",
            Self::TestNg => "@BeforeMethod",
        }
    }

    fn teardown_annotation(self) -> &'static str {
        match self {
            Self::JUnit5 => "@AfterEach",
            Self::JUnit4 => "@After",
            Self::TestNg => "@AfterMethod",
        }
    }

    fn imports(self, setup: bool, teardown: bool) -> Vec<&'static str> {
        let mut imports = Vec::new();
        match self {
            Self::JUnit5 => {
                if setup {
                    imports.push("import org.junit.jupiter.api.BeforeEach;");
                }
                if teardown {
                    imports.push("import org.junit.jupiter.api.AfterEach;");
                }
                imports.push("import org.junit.jupiter.api.Test;");
            }
            Self::JUnit4 => {
                if setup {
                    imports.push("import org.junit.Before;");
                }
                if teardown {
                    imports.push("import org.junit.After;");
                }
                imports.push("import org.junit.Test;");
            }
            Self::TestNg => {
                if setup {
                    imports.push("import org.testng.annotations.BeforeMethod;");
                }
                if teardown {
                    imports.push("import org.testng.annotations.AfterMethod;");
                }
                imports.push("import org.testng.annotations.Test;");
            }
        }
        imports
    }
}

/// The libraries the dependencies of a module contain, in the order of [`TestLibrary`].
pub fn libraries(dependencies: &[String]) -> Vec<TestLibrary> {
    let mut found: Vec<TestLibrary> = TestLibrary::all()
        .into_iter()
        .filter(|library| {
            dependencies
                .iter()
                .any(|dependency| dependency_is(dependency, *library))
        })
        .collect();
    found.dedup();
    found
}

/// The libraries a build file names, for a project whose dependencies were not resolved
/// by its tool yet.
pub fn libraries_in_script(script: &str) -> Vec<TestLibrary> {
    let script = script.to_lowercase();
    let mut found = Vec::new();
    if script.contains("junit-jupiter") || script.contains("org.junit.jupiter") {
        found.push(TestLibrary::JUnit5);
    }
    if script.contains("junit:junit") || script.contains("<artifactId>junit</artifactId>") {
        found.push(TestLibrary::JUnit4);
    }
    if script.contains("org.testng") {
        found.push(TestLibrary::TestNg);
    }
    found
}

fn dependency_is(dependency: &str, library: TestLibrary) -> bool {
    let lower = dependency.to_lowercase();
    let mut parts = lower.split(':');
    let (Some(group), Some(artifact)) = (parts.next(), parts.next()) else {
        return false;
    };
    match library {
        TestLibrary::JUnit5 => {
            group.starts_with("org.junit.jupiter") || artifact.starts_with("junit-jupiter")
        }
        TestLibrary::JUnit4 => (group == "org.junit" || group == "junit") && artifact == "junit",
        TestLibrary::TestNg => group == "org.testng" && artifact == "testng",
    }
}

/// The file of a test class of `package` in `test_root`.
pub fn test_path(test_root: &Path, package: &str, class_name: &str) -> PathBuf {
    let mut path = test_root.to_path_buf();
    for part in package.split('.').filter(|part| !part.is_empty()) {
        path.push(part);
    }
    path.push(format!("{class_name}.java"));
    path
}

/// The names of the generated methods: the name of each selected method, and a serial
/// after the first one with that name, as IDEA names the tests of overloads.
pub fn unique_names(selected: &[String]) -> Vec<String> {
    let mut used: HashSet<String> = HashSet::new();
    let mut names = Vec::with_capacity(selected.len());
    for signature in selected {
        let name = signature
            .split('(')
            .next()
            .unwrap_or(signature)
            .trim()
            .to_owned();
        if name.is_empty() {
            continue;
        }
        let mut candidate = name.clone();
        let mut serial = 1;
        while !used.insert(candidate.clone()) {
            candidate = format!("{name}{serial}");
            serial += 1;
        }
        names.push(candidate);
    }
    names
}

/// A test class and the place of its first test method.
pub struct GeneratedTest {
    pub text: String,
    pub caret: Option<TextPoint>,
}

/// The text of a test class for `library`.
#[allow(clippy::too_many_arguments)]
pub fn test_source(
    library: TestLibrary,
    package: &str,
    class_name: &str,
    superclass: &str,
    setup: bool,
    teardown: bool,
    selected: &[String],
) -> GeneratedTest {
    let mut text = String::new();
    if !package.is_empty() {
        text.push_str(&format!("package {package};\n\n"));
    }
    for import in library.imports(setup, teardown) {
        text.push_str(import);
        text.push('\n');
    }
    text.push('\n');
    let modifier = if library.public_class() {
        "public "
    } else {
        ""
    };
    let extends = match superclass.trim() {
        "" => String::new(),
        superclass => format!(" extends {}", superclass.trim()),
    };
    text.push_str(&format!("{modifier}class {class_name}{extends} {{\n"));
    let mut caret = None;
    if setup {
        text.push_str(&format!(
            "    {}\n    {modifier}void setUp() {{\n    }}\n\n",
            library.setup_annotation(),
        ));
    }
    if teardown {
        text.push_str(&format!(
            "    {}\n    {modifier}void tearDown() {{\n    }}\n\n",
            library.teardown_annotation(),
        ));
    }
    for name in unique_names(selected) {
        let line = text.bytes().filter(|byte| *byte == b'\n').count() as u32 + 1;
        caret.get_or_insert(TextPoint {
            line,
            character: 4 + modifier.len() as u32,
        });
        text.push_str(&format!(
            "    @Test\n    {modifier}void {name}() {{\n    }}\n\n",
        ));
    }
    text.push_str("}\n");
    GeneratedTest { text, caret }
}

/// One module of the project, as Create Test needs it.
pub(crate) struct TestModule {
    pub dir: PathBuf,
    pub source_roots: Vec<PathBuf>,
    pub test_roots: Vec<PathBuf>,
    pub coordinates: Vec<String>,
    pub build_file: Option<PathBuf>,
}

/// The module that holds `path`, from the model when it has one and from the folders
/// around the file otherwise.
pub(crate) fn module_for(
    project_root: &Path,
    model: Option<&ProjectModel>,
    path: &Path,
) -> TestModule {
    if let Some(model) = model {
        let module = model
            .modules
            .iter()
            .filter(|module| path.starts_with(&module.dir))
            .max_by_key(|module| module.dir.components().count());
        if let Some(module) = module {
            let main = module.unit(project_model::MAIN_UNIT);
            let test = module.unit(TEST_UNIT);
            let dependencies = model.dependencies_of(&module.id);
            let coordinates = dependencies
                .map(|dependencies| {
                    dependencies
                        .scopes
                        .iter()
                        .flat_map(|scope| scope.roots.iter())
                        .map(|dependency| dependency.coordinates.clone())
                        .collect()
                })
                .unwrap_or_default();
            let build_file = dependencies.and_then(|dependencies| dependencies.build_file.clone());
            let source_roots = main
                .map(|unit| unit.source_roots.clone())
                .unwrap_or_default();
            let mut test_roots = test
                .map(|unit| unit.source_roots.clone())
                .unwrap_or_default();
            if test_roots.is_empty() {
                test_roots.push(default_test_root(&module.dir));
            }
            return TestModule {
                dir: module.dir.clone(),
                source_roots,
                test_roots,
                coordinates,
                build_file,
            };
        }
    }
    let dir = module_dir(project_root, path);
    TestModule {
        source_roots: vec![dir.join("src").join("main").join("java")],
        test_roots: vec![default_test_root(&dir)],
        coordinates: Vec::new(),
        build_file: build_file_of(&dir),
        dir,
    }
}

/// `src/test/java` of a module, which a project without a test root gets.
pub(crate) fn default_test_root(module: &Path) -> PathBuf {
    module.join("src").join("test").join("java")
}

/// The nearest folder of `path` that holds a build file, the project root at worst.
fn module_dir(project_root: &Path, path: &Path) -> PathBuf {
    let mut folder = path.parent();
    while let Some(current) = folder {
        if build_file_of(current).is_some() {
            return current.to_path_buf();
        }
        if current == project_root {
            break;
        }
        folder = current.parent();
    }
    project_root.to_path_buf()
}

/// The build file of a folder, the Maven one first.
pub(crate) fn build_file_of(folder: &Path) -> Option<PathBuf> {
    ["pom.xml", "build.gradle", "build.gradle.kts"]
        .into_iter()
        .map(|name| folder.join(name))
        .find(|file| file.is_file())
}

/// The form of Create Test for the class of `text` at `path`.
pub fn form_for(
    project_root: &Path,
    model: Option<&ProjectModel>,
    path: &Path,
    text: &str,
) -> TestForm {
    let Some(class) = java_syntax::first_type_name(text).or_else(|| {
        path.file_stem()
            .map(|stem| stem.to_string_lossy().into_owned())
    }) else {
        return TestForm {
            error: Some(NO_CLASS.to_owned()),
            ..TestForm::default()
        };
    };
    let module = module_for(project_root, model, path);
    let mut found = libraries(&module.coordinates);
    if let Some(script) = module
        .build_file
        .as_ref()
        .and_then(|file| std::fs::read_to_string(file).ok())
    {
        found.extend(libraries_in_script(&script));
    }
    found.sort_by_key(|library| {
        TestLibrary::all()
            .into_iter()
            .position(|candidate| candidate == *library)
            .unwrap_or(0)
    });
    found.dedup();
    let can_fix = module.build_file.is_some();
    let frameworks: Vec<TestFramework> = TestLibrary::all()
        .into_iter()
        .map(|library| {
            let available = found.contains(&library);
            TestFramework {
                handle: library.handle().to_owned(),
                name: library.name().to_owned(),
                available,
                note: (!available).then(|| {
                    format!(
                        "{} library not found in the module{}",
                        library.name(),
                        if can_fix {
                            ""
                        } else {
                            ", and nothing can add it"
                        }
                    )
                }),
            }
        })
        .collect();
    let framework = TestLibrary::all()
        .into_iter()
        .position(|library| found.contains(&library))
        .unwrap_or(0);
    let package = crate::package_of(project_root, path.parent().unwrap_or(project_root));
    let folders: Vec<TestFolder> = module
        .test_roots
        .iter()
        .map(|root| TestFolder {
            path: root.clone(),
            name: shown_path(project_root, root),
        })
        .collect();
    let mut methods: Vec<TestChoice> = java_syntax::testable_method_signatures(text)
        .into_iter()
        .map(|method| TestChoice {
            name: method.name,
            detail: Some(method.signature.clone()),
            checked: true,
            inherited: false,
            handle: method.signature,
        })
        .collect();
    methods.extend(inherited_methods(&module, text));
    TestForm {
        source_class: class.clone(),
        frameworks,
        framework,
        class_name: format!("{class}Test"),
        superclass: String::new(),
        package,
        folders,
        folder: 0,
        setup: true,
        teardown: false,
        methods,
        can_fix,
        error: None,
    }
}

fn shown_path(project_root: &Path, path: &Path) -> String {
    let relative = path.strip_prefix(project_root).unwrap_or(path);
    let text = relative.to_string_lossy().replace('\\', "/");
    if text.is_empty() {
        ".".to_owned()
    } else {
        text
    }
}

/// The methods of the superclasses of the class of `text`, found among the sources of
/// the project. A library superclass is not there, so it adds nothing.
fn inherited_methods(module: &TestModule, text: &str) -> Vec<TestChoice> {
    let mut found = Vec::new();
    let mut visited = HashSet::new();
    let mut current = text.to_owned();
    for _ in 0..4 {
        let Some(name) = java_syntax::superclass_name(&current) else {
            break;
        };
        if !visited.insert(name.clone()) {
            break;
        }
        let Some((_, source)) = find_type_source(module, &name) else {
            break;
        };
        found.extend(
            java_syntax::testable_method_signatures(&source)
                .into_iter()
                .map(|method| TestChoice {
                    name: method.name,
                    detail: Some(method.signature.clone()),
                    checked: false,
                    inherited: true,
                    handle: method.signature,
                }),
        );
        current = source;
    }
    found
}

fn find_type_source(module: &TestModule, name: &str) -> Option<(PathBuf, String)> {
    let file = format!("{name}.java");
    let roots: Vec<&Path> = module
        .source_roots
        .iter()
        .chain(module.test_roots.iter())
        .map(PathBuf::as_path)
        .chain(std::iter::once(module.dir.as_path()))
        .collect();
    for root in roots {
        if let Some(found) = find_file(root, &file, 0) {
            return std::fs::read_to_string(&found)
                .ok()
                .map(|source| (found, source));
        }
    }
    None
}

fn find_file(folder: &Path, name: &str, depth: usize) -> Option<PathBuf> {
    if depth > 8 {
        return None;
    }
    let entries = std::fs::read_dir(folder).ok()?;
    let mut folders = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let lower = entry.file_name().to_string_lossy().to_lowercase();
            if !matches!(
                lower.as_str(),
                "target" | "build" | "bin" | ".gradle" | ".git" | "node_modules"
            ) {
                folders.push(path);
            }
        } else if path.file_name().is_some_and(|file| file == name) {
            return Some(path);
        }
    }
    folders
        .into_iter()
        .find_map(|folder| find_file(&folder, name, depth + 1))
}

/// The test file of a filled form, or the reason there is none. A file that is already
/// there comes back without text, so the window asks whether to open it.
pub(crate) fn create(request: &lang_api::TestCreateRequest) -> lang_api::TestCreateOutcome {
    let Some(library) = TestLibrary::from_handle(&request.framework) else {
        return lang_api::TestCreateOutcome {
            error: Some("The chosen test library is not known".to_owned()),
            ..lang_api::TestCreateOutcome::default()
        };
    };
    let class_name = request.class_name.trim();
    if !is_class_name(class_name) {
        return lang_api::TestCreateOutcome {
            error: Some(BAD_NAME.to_owned()),
            ..lang_api::TestCreateOutcome::default()
        };
    }
    let package = request.package.trim();
    if !package.is_empty() && !package.split('.').all(is_identifier) {
        return lang_api::TestCreateOutcome {
            error: Some("The package name is not a Java package".to_owned()),
            ..lang_api::TestCreateOutcome::default()
        };
    }
    let path = test_path(&request.folder, package, class_name);
    if path.exists() {
        return lang_api::TestCreateOutcome {
            file: Some(TestFile {
                path,
                text: None,
                caret: None,
            }),
            error: None,
        };
    }
    let generated = test_source(
        library,
        package,
        class_name,
        &request.superclass,
        request.setup,
        request.teardown,
        &request.methods,
    );
    lang_api::TestCreateOutcome {
        file: Some(TestFile {
            path,
            text: Some(generated.text),
            caret: generated.caret,
        }),
        error: None,
    }
}

fn is_class_name(name: &str) -> bool {
    is_identifier(name)
}

fn is_identifier(name: &str) -> bool {
    let mut characters = name.chars();
    let Some(first) = characters.next() else {
        return false;
    };
    (first.is_alphabetic() || first == '_' || first == '$')
        && characters
            .all(|character| character.is_alphanumeric() || character == '_' || character == '$')
}

/// The edits that add the library of `framework` to the build file of the module of
/// `path`, or `None` when the file is not one this IDE writes.
pub(crate) fn fix(
    project_root: &Path,
    model: Option<&ProjectModel>,
    path: &Path,
    framework: &str,
) -> lang_api::TestFixOutcome {
    let Some(library) = TestLibrary::from_handle(framework) else {
        return lang_api::TestFixOutcome {
            error: Some("The chosen test library is not known".to_owned()),
            ..lang_api::TestFixOutcome::default()
        };
    };
    let module = module_for(project_root, model, path);
    let Some(build_file) = module.build_file.filter(|file| file.is_file()) else {
        return lang_api::TestFixOutcome {
            error: Some("The module has no build file to change".to_owned()),
            ..lang_api::TestFixOutcome::default()
        };
    };
    let Ok(text) = std::fs::read_to_string(&build_file) else {
        return lang_api::TestFixOutcome {
            error: Some(format!("{} cannot be read", build_file.display())),
            ..lang_api::TestFixOutcome::default()
        };
    };
    let Some(edit) = build_file_edit(&build_file, &text, library) else {
        return lang_api::TestFixOutcome {
            error: Some(format!(
                "{} has no place for a dependency",
                build_file.display()
            )),
            ..lang_api::TestFixOutcome::default()
        };
    };
    let Some(uri) = platform::CanonPath::new(&build_file)
        .ok()
        .and_then(|path| lsp_client::uri::file_uri(&path))
    else {
        return lang_api::TestFixOutcome {
            error: Some(format!("{} has no URI", build_file.display())),
            ..lang_api::TestFixOutcome::default()
        };
    };
    lang_api::TestFixOutcome {
        change: WorkspaceChange {
            edits: vec![lang_api::SourceEdit {
                uri: uri.as_str().to_owned(),
                edits: vec![edit],
            }],
            renames: Vec::new(),
        },
        error: None,
    }
}

/// The insertion that makes the library a test dependency of the build file.
pub fn build_file_edit(
    build_file: &Path,
    text: &str,
    library: TestLibrary,
) -> Option<DocumentEdit> {
    let name = build_file.file_name()?.to_string_lossy().to_lowercase();
    let (offset, inserted) = if name == "pom.xml" {
        let at = maven_dependency_offset(text)?;
        (at, maven_dependency(library))
    } else {
        gradle_insertion(text, &name, library)?
    };
    Some(DocumentEdit {
        span: TextSpan {
            start: point_in_text(text, offset),
            end: point_in_text(text, offset),
        },
        text: inserted,
    })
}

/// The start of the line of the `</dependencies>` that closes the dependencies of the
/// project, not the ones of `dependencyManagement`.
fn maven_dependency_offset(text: &str) -> Option<usize> {
    let mut from = 0;
    while let Some(found) = text[from..].find("</dependencies>") {
        let at = from + found;
        let before = &text[..at];
        let opened = before.matches("<dependencyManagement").count();
        let closed = before.matches("</dependencyManagement>").count();
        if opened == closed {
            return Some(text[..at].rfind('\n').map_or(0, |line| line + 1));
        }
        from = at + "</dependencies>".len();
    }
    None
}

fn maven_dependency(library: TestLibrary) -> String {
    let coordinates = library.coordinates();
    let mut parts = coordinates.split(':');
    let group = parts.next().unwrap_or_default();
    let artifact = parts.next().unwrap_or_default();
    let version = parts.next().unwrap_or_default();
    format!(
        "    <dependency>\n        <groupId>{group}</groupId>\n        <artifactId>{artifact}</artifactId>\n        <version>{version}</version>\n        <scope>test</scope>\n    </dependency>\n"
    )
}

fn gradle_insertion(text: &str, name: &str, library: TestLibrary) -> Option<(usize, String)> {
    let at = text
        .find("dependencies {")
        .or_else(|| text.find("dependencies{"))?;
    let line_end = text[at..].find('\n').map_or(text.len(), |end| at + end + 1);
    let notation = library.coordinates();
    let line = if name.ends_with(".kts") {
        format!("    testImplementation(\"{notation}\")\n")
    } else {
        format!("    testImplementation '{notation}'\n")
    };
    Some((line_end, line))
}

fn point_in_text(text: &str, offset: usize) -> TextPoint {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let line_start = before.rfind('\n').map_or(0, |index| index + 1);
    let character = text[line_start..offset].encode_utf16().count() as u32;
    TextPoint { line, character }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn methods(selected: &[&str]) -> Vec<String> {
        selected.iter().map(|name| (*name).to_owned()).collect()
    }

    #[test]
    fn the_three_libraries_are_recognized_by_their_dependencies() {
        let dependencies = methods(&[
            "org.junit.jupiter:junit-jupiter:5.11.4",
            "org.testng:testng:7.10.2",
        ]);
        assert_eq!(
            libraries(&dependencies),
            [TestLibrary::JUnit5, TestLibrary::TestNg]
        );
        assert_eq!(
            libraries(&methods(&["junit:junit:4.13.2"])),
            [TestLibrary::JUnit4]
        );
        assert!(libraries(&methods(&["org.springframework:spring-core:6.0.0"])).is_empty());
    }

    #[test]
    fn a_build_script_names_its_library_when_the_dependencies_are_not_read_yet() {
        assert_eq!(
            libraries_in_script("<artifactId>junit-jupiter</artifactId>"),
            [TestLibrary::JUnit5]
        );
        assert_eq!(
            libraries_in_script("testImplementation 'junit:junit:4.13.2'"),
            [TestLibrary::JUnit4]
        );
        assert_eq!(
            libraries_in_script("<groupId>org.testng</groupId>"),
            [TestLibrary::TestNg]
        );
    }

    #[test]
    fn each_library_writes_its_imports_annotations_and_modifiers() {
        let junit5 = test_source(
            TestLibrary::JUnit5,
            "com.shop",
            "OwnerTest",
            "",
            true,
            true,
            &methods(&["find(int id)"]),
        );
        assert_eq!(
            junit5.text,
            "package com.shop;\n\
             \n\
             import org.junit.jupiter.api.BeforeEach;\n\
             import org.junit.jupiter.api.AfterEach;\n\
             import org.junit.jupiter.api.Test;\n\
             \n\
             class OwnerTest {\n\
             \x20   @BeforeEach\n\
             \x20   void setUp() {\n\
             \x20   }\n\
             \n\
             \x20   @AfterEach\n\
             \x20   void tearDown() {\n\
             \x20   }\n\
             \n\
             \x20   @Test\n\
             \x20   void find() {\n\
             \x20   }\n\
             \n\
             }\n"
        );
        assert_eq!(
            junit5.caret,
            Some(TextPoint {
                line: 16,
                character: 4
            })
        );

        let junit4 = test_source(
            TestLibrary::JUnit4,
            "com.shop",
            "OwnerTest",
            "BaseTest",
            true,
            false,
            &methods(&["find()"]),
        );
        assert!(junit4.text.contains("import org.junit.Before;"));
        assert!(junit4.text.contains("import org.junit.Test;"));
        assert!(!junit4.text.contains("After"));
        assert!(
            junit4
                .text
                .contains("public class OwnerTest extends BaseTest {")
        );
        assert!(
            junit4
                .text
                .contains("    @Before\n    public void setUp() {")
        );
        assert!(junit4.text.contains("    @Test\n    public void find() {"));

        let testng = test_source(
            TestLibrary::TestNg,
            "",
            "OwnerTest",
            "",
            false,
            true,
            &methods(&["find()"]),
        );
        assert!(
            testng
                .text
                .starts_with("import org.testng.annotations.AfterMethod;")
        );
        assert!(testng.text.contains("import org.testng.annotations.Test;"));
        assert!(testng.text.contains("public class OwnerTest {"));
        assert!(
            testng
                .text
                .contains("    @AfterMethod\n    public void tearDown() {")
        );
        assert!(testng.text.contains("    @Test\n    public void find() {"));
    }

    #[test]
    fn a_test_file_follows_the_package_below_the_test_root() {
        assert_eq!(
            test_path(
                Path::new("app/src/test/java"),
                "com.shop.owner",
                "OwnerTest"
            ),
            PathBuf::from("app/src/test/java/com/shop/owner/OwnerTest.java")
        );
        assert_eq!(
            test_path(Path::new("app/src/test/java"), "", "OwnerTest"),
            PathBuf::from("app/src/test/java/OwnerTest.java")
        );
    }

    #[test]
    fn overloads_get_a_serial_after_the_name() {
        assert_eq!(
            unique_names(&methods(&["find(int id)", "find(String name)", "save()"])),
            ["find", "find1", "save"]
        );
        assert_eq!(
            unique_names(&methods(&["find(String name)", "find(int id)"])),
            ["find", "find1"],
            "the source order decides which one keeps the name"
        );
    }

    #[test]
    fn a_package_that_is_not_one_does_not_reach_out_of_the_test_folder() {
        let folder = test_support::scratch("create-test-package").join("test");
        let mut request = lang_api::TestCreateRequest {
            spot: lang_api::DocumentSpot {
                uri: "file:///Owner.java".to_owned(),
                version: 1,
                point: TextPoint::default(),
            },
            framework: TestLibrary::JUnit5.handle().to_owned(),
            class_name: "OwnerTest".to_owned(),
            superclass: String::new(),
            package: "../../src".to_owned(),
            folder: folder.clone(),
            setup: false,
            teardown: false,
            methods: Vec::new(),
        };

        assert!(create(&request).error.is_some());
        request.package = "com.shop".to_owned();
        assert!(create(&request).file.is_some());
    }

    #[test]
    fn a_filled_form_makes_the_file_names_and_the_methods() {
        let folder = test_support::scratch("create-test-filled").join("test");
        let request = lang_api::TestCreateRequest {
            spot: lang_api::DocumentSpot {
                uri: "file:///Owner.java".to_owned(),
                version: 1,
                point: TextPoint::default(),
            },
            framework: TestLibrary::JUnit5.handle().to_owned(),
            class_name: "OwnerTest".to_owned(),
            superclass: "BaseTest".to_owned(),
            package: "com.shop".to_owned(),
            folder: folder.clone(),
            setup: false,
            teardown: false,
            methods: methods(&["find(int id)", "find(String name)"]),
        };

        let outcome = create(&request);

        let file = outcome.file.expect("a new file");
        assert_eq!(
            file.path,
            folder.join("com/shop/OwnerTest.java"),
            "the package becomes folders below the test root"
        );
        let text = file.text.expect("the text of the new test");
        assert!(text.contains("class OwnerTest extends BaseTest {"));
        assert!(text.contains("    void find() {"));
        assert!(text.contains("    void find1() {"));
        assert_eq!(
            file.caret,
            Some(TextPoint {
                line: 6,
                character: 4
            })
        );
    }

    #[test]
    fn a_maven_dependency_goes_before_the_end_of_the_list() {
        let text = "<project>\n    <dependencies>\n    </dependencies>\n</project>\n";
        let edit = build_file_edit(Path::new("app/pom.xml"), text, TestLibrary::JUnit5).unwrap();
        assert_eq!(
            edit.span.start,
            point_in_text(text, text.find("    </dependencies>").unwrap())
        );
        assert!(edit.text.contains("<artifactId>junit-jupiter</artifactId>"));
        assert!(edit.text.contains("<scope>test</scope>"));
    }

    #[test]
    fn a_managed_dependency_does_not_take_the_new_one() {
        let text = "<project>\n    <dependencyManagement>\n        <dependencies>\n        </dependencies>\n    </dependencyManagement>\n    <dependencies>\n    </dependencies>\n</project>\n";
        let edit = build_file_edit(Path::new("app/pom.xml"), text, TestLibrary::JUnit4).unwrap();
        assert_eq!(
            edit.span.start.line, 6,
            "the block of the project itself comes last"
        );
    }

    #[test]
    fn a_gradle_dependency_goes_into_the_block() {
        let text = "dependencies {\n    implementation 'x:y:1'\n}\n";
        let edit =
            build_file_edit(Path::new("app/build.gradle"), text, TestLibrary::JUnit4).unwrap();
        assert_eq!(edit.span.start.line, 1);
        assert_eq!(edit.text, "    testImplementation 'junit:junit:4.13.2'\n");
        let text = "dependencies {\n}\n";
        let edit =
            build_file_edit(Path::new("app/build.gradle.kts"), text, TestLibrary::TestNg).unwrap();
        assert_eq!(
            edit.text,
            "    testImplementation(\"org.testng:testng:7.10.2\")\n"
        );
    }

    #[test]
    fn the_form_reads_the_package_the_test_root_the_library_and_the_inherited_methods() {
        use project_model::{
            Dependency, DependencyScope, MAIN_UNIT, Module, ModuleDependencies, Unit,
        };

        let root = test_support::scratch("create-test-form");
        let main = root.join("src/main/java/com/shop");
        fs::create_dir_all(&main).unwrap();
        fs::create_dir_all(root.join("src/test/java")).unwrap();
        let path = main.join("Owner.java");
        let text = "package com.shop;\n\nclass Owner extends BaseOwner {\n    public void find(int id) {\n    }\n    private void secret() {\n    }\n}\n";
        fs::write(&path, text).unwrap();
        fs::write(
            main.join("BaseOwner.java"),
            "package com.shop;\n\nclass BaseOwner {\n    protected void base() {\n    }\n}\n",
        )
        .unwrap();
        let build_file = root.join("pom.xml");
        fs::write(&build_file, "<project/>\n").unwrap();
        let model = ProjectModel {
            root: root.clone(),
            tool: "maven".to_owned(),
            modules: vec![Module {
                id: "app".to_owned(),
                name: "app".to_owned(),
                dir: root.clone(),
                units: vec![
                    Unit {
                        name: MAIN_UNIT.to_owned(),
                        source_roots: vec![root.join("src/main/java")],
                        ..Unit::default()
                    },
                    Unit {
                        name: TEST_UNIT.to_owned(),
                        source_roots: vec![root.join("src/test/java")],
                        ..Unit::default()
                    },
                ],
            }],
            dependencies: vec![ModuleDependencies {
                module: "app".to_owned(),
                build_file: Some(build_file),
                scopes: vec![DependencyScope {
                    name: "test".to_owned(),
                    roots: vec![Dependency {
                        coordinates: "org.junit.jupiter:junit-jupiter:5.11.0".to_owned(),
                        ..Dependency::default()
                    }],
                }],
            }],
            resolved: true,
            ..ProjectModel::default()
        };

        let form = form_for(&root, Some(&model), &path, text);

        assert_eq!(form.source_class, "Owner");
        assert_eq!(form.class_name, "OwnerTest");
        assert_eq!(form.package, "com.shop");
        assert_eq!(form.folders[0].path, root.join("src/test/java"));
        assert_eq!(form.folders[0].name, "src/test/java");
        assert!(
            form.frameworks[0].available,
            "the module depends on JUnit 5"
        );
        assert!(!form.frameworks[1].available);
        assert_eq!(form.framework, 0);
        assert!(form.setup);
        assert!(!form.teardown);

        let own: Vec<&TestChoice> = form
            .methods
            .iter()
            .filter(|method| !method.inherited)
            .collect();
        assert_eq!(own.len(), 1, "private methods are left out");
        assert_eq!(own[0].name, "find");
        assert_eq!(own[0].detail.as_deref(), Some("find(int id)"));
        assert!(own[0].checked);
        let inherited: Vec<&TestChoice> = form
            .methods
            .iter()
            .filter(|method| method.inherited)
            .collect();
        assert_eq!(inherited.len(), 1, "Show inherited methods has a row");
        assert_eq!(inherited[0].name, "base");
        assert!(!inherited[0].checked);
    }
}

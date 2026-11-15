//! New projects as the New Project form offers them: a plain project of Maven or Gradle
//! and a project a Spring Initializr server writes. Every generator answers the fields of
//! the form and, away from its thread, the files or the archive of the project.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use base::Cancel;
use java_project::initializr::{
    self, BOOT_VERSION, DEFAULT_SERVER, JAVA_VERSION, LANGUAGE, PACKAGING, PROJECT_TYPE,
    StarterRequest,
};
use java_project::{Jdk, Wrapper, find_jdks, generate_wrapper};
use project_model::scaffold::{Choice, Created, Field, Generator, Request, Values, artifact_name};

/// The keys of the fields the form shows.
pub const BUILD: &str = "build";
pub const DSL: &str = "dsl";
pub const JDK: &str = "jdk";
pub const SAMPLE: &str = "sample";
pub const GROUP: &str = "group";
pub const ARTIFACT: &str = "artifact";
pub const VERSION: &str = "version";
pub const SERVER: &str = "server";
pub const PACKAGE: &str = "package";

/// The build system of a plain project, as the value of [`BUILD`].
pub const MAVEN: &str = "maven";
pub const GRADLE: &str = "gradle";

/// The script dialect of a Gradle project, as the value of [`DSL`].
pub const KOTLIN_DSL: &str = "kotlin";
pub const GROOVY_DSL: &str = "groovy";

/// The version of JUnit the generated projects test with.
const JUNIT: &str = "5.11.4";
/// The language level of a project whose machine has no JDK.
const FALLBACK_JDK: u32 = 21;
/// The values the form offers before a server answers.
const FALLBACK_BOOT: &str = "4.0.0";

/// What the application says about the generators it supplies.
#[derive(Clone, Debug)]
pub struct NewProjectOptions {
    /// Whether a new project gets the wrapper of its build tool when the tool is here.
    /// The wrapper jar and its distribution cannot be made up, so a machine without the
    /// tool gets a project without a wrapper and a note.
    pub wrapper: bool,
    /// The server the Spring Boot generator asks for its metadata.
    pub initializr: String,
}

impl Default for NewProjectOptions {
    fn default() -> Self {
        Self {
            wrapper: true,
            initializr: DEFAULT_SERVER.to_owned(),
        }
    }
}

/// The generators of the form, in the order it lists them.
pub fn project_generators(
    state_dir: &Path,
    options: &NewProjectOptions,
) -> Vec<Arc<dyn Generator>> {
    vec![
        Arc::new(PlainProject {
            jdks: find_jdks(),
            wrapper: options.wrapper,
        }),
        Arc::new(SpringProject {
            cache: state_dir.join("initializr"),
            server: options.initializr.clone(),
        }),
        Arc::new(EmptyProject),
    ]
}

/// A project with the files of a build tool and, if asked, a class with `main`.
struct PlainProject {
    jdks: Vec<Jdk>,
    wrapper: bool,
}

impl Generator for PlainProject {
    fn label(&self) -> &str {
        "New Project"
    }

    fn fields(&self) -> Vec<Field> {
        let mut fields = vec![
            Field::choice(
                BUILD,
                "Build system",
                vec![Choice::new(MAVEN, "Maven"), Choice::new(GRADLE, "Gradle")],
            )
            .with_hint("The wrapper is written when the build tool is installed on this machine"),
            Field::choice(
                DSL,
                "Gradle DSL",
                vec![
                    Choice::new(GROOVY_DSL, "Groovy"),
                    Choice::new(KOTLIN_DSL, "Kotlin"),
                ],
            )
            .shown_when(BUILD, GRADLE),
        ];
        let choices: Vec<Choice> = self
            .jdks
            .iter()
            .map(|jdk| {
                Choice::new(
                    jdk.home.display().to_string(),
                    format!("JDK {}", jdk.version),
                )
                .with_hint(jdk.home.display().to_string())
            })
            .collect();
        let mut jdk = Field::choice(JDK, "JDK", choices);
        if let Some(chosen) = self.default_jdk() {
            jdk.default = chosen.home.display().to_string();
        }
        jdk.hint = Some(if self.jdks.is_empty() {
            format!("No JDK found on this machine; the project asks for Java {FALLBACK_JDK}")
        } else {
            "The language level of the project comes from this JDK".to_owned()
        });
        fields.push(jdk);
        fields.push(Field::toggle(SAMPLE, "Add sample code", true));
        fields.push(Field::text(GROUP, "GroupId", "org.example"));
        fields.push(
            Field::text(ARTIFACT, "ArtifactId", "")
                .with_hint("The project name in lower case, when left empty")
                .from_name(),
        );
        fields.push(Field::text(VERSION, "Version", "1.0-SNAPSHOT"));
        fields
    }

    fn create(&self, request: &Request, _cancel: &Cancel) -> Result<Created, String> {
        let options = PlainOptions::of(request);
        Ok(Created::files(options.files(request.git)))
    }

    fn finish(
        &self,
        request: &Request,
        folder: &Path,
        cancel: &Cancel,
    ) -> Result<Option<String>, String> {
        if !self.wrapper {
            return Ok(None);
        }
        let kind = if PlainOptions::of(request).build == GRADLE {
            Wrapper::Gradle
        } else {
            Wrapper::Maven
        };
        if kind.tool().is_none() {
            return Ok(Some(format!(
                "The project is created without a {} wrapper: no {} is on this machine.",
                kind.label(),
                kind.label()
            )));
        }
        let jdk = PlainOptions::of(request)
            .jdk
            .and_then(|home| Jdk::at(Path::new(&home)));
        match generate_wrapper(kind, folder, jdk.as_ref(), cancel) {
            Ok(()) => Ok(None),
            Err(error) => Ok(Some(format!("{error}. The project is created without it."))),
        }
    }
}

impl PlainProject {
    /// The JDK the form offers first: the one `JAVA_HOME` names, else the newest.
    fn default_jdk(&self) -> Option<&Jdk> {
        let home = std::env::var_os("JAVA_HOME").map(PathBuf::from);
        home.and_then(|home| {
            self.jdks.iter().find(|jdk| jdk.home == home).or_else(|| {
                Jdk::at(&home)
                    .as_ref()
                    .and_then(|found| self.jdks.iter().find(|jdk| jdk.home == found.home))
            })
        })
        .or_else(|| self.jdks.iter().max_by_key(|jdk| jdk.feature))
    }
}

/// The values of a plain project, once the form is filled in.
struct PlainOptions {
    build: String,
    kotlin: bool,
    jdk: Option<String>,
    feature: u32,
    sample: bool,
    group: String,
    artifact: String,
    version: String,
}

impl PlainOptions {
    fn of(request: &Request) -> Self {
        let values = &request.values;
        let build = match values.text(BUILD) {
            GRADLE => GRADLE,
            _ => MAVEN,
        };
        let home = values.text(JDK).to_owned();
        let feature = Jdk::at(Path::new(&home))
            .map(|jdk| jdk.feature)
            .unwrap_or(FALLBACK_JDK);
        Self {
            build: build.to_owned(),
            kotlin: build == GRADLE && values.text(DSL) == KOTLIN_DSL,
            jdk: (!home.is_empty()).then_some(home),
            feature,
            sample: values.flag(SAMPLE),
            group: non_empty(values.text(GROUP), "org.example"),
            artifact: non_empty(values.text(ARTIFACT), &dashed(&request.name)),
            version: non_empty(values.text(VERSION), "1.0-SNAPSHOT"),
        }
    }

    /// The package of the sample code: the group, so the class sits where the user
    /// named its root package.
    fn package(&self) -> String {
        sanitized_package(&self.group)
    }

    fn files(&self, git: bool) -> Vec<(String, String)> {
        let package = self.package();
        let mut files = if self.build == GRADLE {
            if self.kotlin {
                self.kotlin_files(&package)
            } else {
                self.groovy_files(&package)
            }
        } else {
            self.maven_files()
        };
        if self.sample {
            files.push((source_path(&package, "Main"), self.main_class(&package)));
        }
        if git {
            files.push((".gitignore".to_owned(), self.gitignore().to_owned()));
        }
        files
    }

    fn maven_files(&self) -> Vec<(String, String)> {
        let pom = format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
             <project xmlns=\"http://maven.apache.org/POM/4.0.0\"\n\
             \x20        xmlns:xsi=\"http://www.w3.org/2001/XMLSchema-instance\"\n\
             \x20        xsi:schemaLocation=\"http://maven.apache.org/POM/4.0.0 \
             https://maven.apache.org/xsd/maven-4.0.0.xsd\">\n\
             \x20   <modelVersion>4.0.0</modelVersion>\n\
             \x20   <groupId>{group}</groupId>\n\
             \x20   <artifactId>{artifact}</artifactId>\n\
             \x20   <version>{version}</version>\n\
             \x20   <properties>\n\
             \x20       <maven.compiler.release>{release}</maven.compiler.release>\n\
             \x20       <project.build.sourceEncoding>UTF-8</project.build.sourceEncoding>\n\
             \x20   </properties>\n\
             \x20   <dependencies>\n\
             \x20       <dependency>\n\
             \x20           <groupId>org.junit.jupiter</groupId>\n\
             \x20           <artifactId>junit-jupiter</artifactId>\n\
             \x20           <version>{junit}</version>\n\
             \x20           <scope>test</scope>\n\
             \x20       </dependency>\n\
             \x20   </dependencies>\n\
             \x20   <build>\n\
             \x20       <plugins>\n\
             \x20           <plugin>\n\
             \x20               <groupId>org.apache.maven.plugins</groupId>\n\
             \x20               <artifactId>maven-surefire-plugin</artifactId>\n\
             \x20               <version>3.5.2</version>\n\
             \x20           </plugin>\n\
             \x20       </plugins>\n\
             \x20   </build>\n\
             </project>\n",
            group = self.group,
            artifact = self.artifact,
            version = self.version,
            release = self.feature,
            junit = JUNIT,
        );
        vec![("pom.xml".to_owned(), pom)]
    }

    fn groovy_files(&self, package: &str) -> Vec<(String, String)> {
        let mut script = "plugins {\n    id 'java'\n".to_owned();
        if self.sample {
            script.push_str("    id 'application'\n");
        }
        script.push_str(&format!(
            "}}\n\ngroup = '{group}'\nversion = '{version}'\n\n\
             repositories {{\n    mavenCentral()\n}}\n\n\
             java {{\n    toolchain {{\n        languageVersion = JavaLanguageVersion.of({release})\n    }}\n}}\n\n\
             dependencies {{\n    testImplementation platform('org.junit:junit-bom:{junit}')\n\
             \x20   testImplementation 'org.junit.jupiter:junit-jupiter'\n\
             \x20   testRuntimeOnly 'org.junit.platform:junit-platform-launcher'\n}}\n\n\
             test {{\n    useJUnitPlatform()\n}}\n",
            group = self.group,
            version = self.version,
            release = self.feature,
            junit = JUNIT,
        ));
        if self.sample {
            script.push_str(&format!(
                "\napplication {{\n    mainClass = '{package}.Main'\n}}\n"
            ));
        }
        vec![
            (
                "settings.gradle".to_owned(),
                format!("rootProject.name = '{}'\n", self.artifact),
            ),
            ("build.gradle".to_owned(), script),
        ]
    }

    fn kotlin_files(&self, package: &str) -> Vec<(String, String)> {
        let mut script = "plugins {\n    id(\"java\")\n".to_owned();
        if self.sample {
            script.push_str("    id(\"application\")\n");
        }
        script.push_str(&format!(
            "}}\n\ngroup = \"{group}\"\nversion = \"{version}\"\n\n\
             repositories {{\n    mavenCentral()\n}}\n\n\
             java {{\n    toolchain {{\n        languageVersion = JavaLanguageVersion.of({release})\n    }}\n}}\n\n\
             dependencies {{\n    testImplementation(platform(\"org.junit:junit-bom:{junit}\"))\n\
             \x20   testImplementation(\"org.junit.jupiter:junit-jupiter\")\n\
             \x20   testRuntimeOnly(\"org.junit.platform:junit-platform-launcher\")\n}}\n\n\
             tasks.test {{\n    useJUnitPlatform()\n}}\n",
            group = self.group,
            version = self.version,
            release = self.feature,
            junit = JUNIT,
        ));
        if self.sample {
            script.push_str(&format!(
                "\napplication {{\n    mainClass = \"{package}.Main\"\n}}\n"
            ));
        }
        vec![
            (
                "settings.gradle.kts".to_owned(),
                format!("rootProject.name = \"{}\"\n", self.artifact),
            ),
            ("build.gradle.kts".to_owned(), script),
        ]
    }

    /// The class with `main`. The JDK reads the text as UTF-8 by default from 18 on, and
    /// the file of a project of an older release names no characters outside ASCII.
    fn main_class(&self, package: &str) -> String {
        format!(
            "package {package};\n\npublic class Main {{\n    public static void main(String[] args) {{\n        System.out.println(\"Hello from {name}!\");\n    }}\n}}\n",
            name = self.artifact,
        )
    }

    fn gitignore(&self) -> &'static str {
        if self.build == GRADLE {
            "build/\n.gradle/\n.idea/\n*.iml\n"
        } else {
            "target/\n.idea/\n*.iml\n"
        }
    }
}

/// A Spring Boot project of a Spring Initializr server: the metadata of the server fills
/// the choices, and the archive of the project comes straight from it.
struct SpringProject {
    cache: PathBuf,
    server: String,
}

impl Generator for SpringProject {
    fn label(&self) -> &str {
        "Spring Boot"
    }

    fn fields(&self) -> Vec<Field> {
        spring_fields(&self.server, None)
    }

    fn loads(&self) -> bool {
        true
    }

    fn load(&self, values: &Values, cancel: &Cancel) -> Result<Vec<Field>, String> {
        cancel.check().map_err(|error| error.to_string())?;
        let server = non_empty(values.text(SERVER), &self.server);
        let metadata = initializr::fetch(&server, &self.cache)?;
        cancel.check().map_err(|error| error.to_string())?;
        Ok(spring_fields(&server, Some(&metadata)))
    }

    fn create(&self, request: &Request, cancel: &Cancel) -> Result<Created, String> {
        let values = &request.values;
        let server = non_empty(values.text(SERVER), &self.server);
        let package = non_empty(values.text(PACKAGE), "org.example.demo");
        let java = non_empty(values.text(JAVA_VERSION), "21");
        let starter = StarterRequest {
            server,
            kind: non_empty(values.text(PROJECT_TYPE), "maven-project"),
            language: non_empty(values.text(LANGUAGE), "java"),
            boot: non_empty(values.text(BOOT_VERSION), FALLBACK_BOOT),
            group: non_empty(values.text(GROUP), "org.example"),
            artifact: non_empty(values.text(ARTIFACT), &dashed(&request.name)),
            name: request.name.trim().to_owned(),
            description: "Demo project for Spring Boot".to_owned(),
            package,
            packaging: non_empty(values.text(PACKAGING), "jar"),
            java,
            dependencies: values.many(DEPENDENCIES).to_vec(),
        };
        cancel.check().map_err(|error| error.to_string())?;
        let bytes = starter.download()?;
        Ok(Created::archive(bytes))
    }
}

/// The field of the dependencies of a Spring Boot project.
pub const DEPENDENCIES: &str = "dependencies";

/// The fields of the Spring Boot generator, with the choices of `metadata` when a server
/// has answered and the ones the form can offer before that.
fn spring_fields(server: &str, metadata: Option<&initializr::Metadata>) -> Vec<Field> {
    let choices = |key: &str, fallback: Vec<Choice>| -> Vec<Choice> {
        match metadata {
            Some(metadata) if !metadata.choices(key).is_empty() => metadata.choices(key).to_vec(),
            _ => fallback,
        }
    };
    let mut types = Field::choice(
        PROJECT_TYPE,
        "Type",
        choices(
            PROJECT_TYPE,
            vec![
                Choice::new("maven-project", "Maven"),
                Choice::new("gradle-project", "Gradle - Groovy"),
                Choice::new("gradle-project-kotlin", "Gradle - Kotlin"),
            ],
        ),
    );
    let mut language = Field::choice(
        LANGUAGE,
        "Language",
        choices(
            LANGUAGE,
            vec![
                Choice::new("java", "Java"),
                Choice::new("kotlin", "Kotlin"),
                Choice::new("groovy", "Groovy"),
            ],
        ),
    );
    let mut boot = Field::choice(
        BOOT_VERSION,
        "Spring Boot version",
        choices(
            BOOT_VERSION,
            vec![Choice::new(FALLBACK_BOOT, FALLBACK_BOOT)],
        ),
    );
    let mut java = Field::choice(
        JAVA_VERSION,
        "Java version",
        choices(
            JAVA_VERSION,
            vec![
                Choice::new("17", "17"),
                Choice::new("21", "21"),
                Choice::new("25", "25"),
            ],
        ),
    );
    let mut packaging = Field::choice(
        PACKAGING,
        "Packaging",
        choices(
            PACKAGING,
            vec![Choice::new("jar", "Jar"), Choice::new("war", "War")],
        ),
    );
    if let Some(metadata) = metadata {
        for (field, key) in [
            (&mut types, PROJECT_TYPE),
            (&mut language, LANGUAGE),
            (&mut boot, BOOT_VERSION),
            (&mut java, JAVA_VERSION),
            (&mut packaging, PACKAGING),
        ] {
            if let Some(default) = metadata.default_of(key) {
                field.set_default(default);
            }
        }
    }
    let dependencies = metadata
        .map(|metadata| metadata.dependencies.clone())
        .unwrap_or_default();
    vec![
        Field::text(SERVER, "Server URL", server),
        types,
        language,
        Field::text(GROUP, "Group", "org.example"),
        Field::text(ARTIFACT, "Artifact", "demo"),
        Field::text(PACKAGE, "Package name", "org.example.demo"),
        java,
        packaging,
        boot,
        Field::many(DEPENDENCIES, "Dependencies", dependencies).with_hint(if metadata.is_some() {
            "Search the dependencies of the server; the chosen ones become chips"
        } else {
            "Reload the metadata of the server to choose dependencies"
        }),
    ]
}

/// A folder and, if asked, a repository: nothing else.
struct EmptyProject;

impl Generator for EmptyProject {
    fn label(&self) -> &str {
        "Empty Project"
    }

    fn fields(&self) -> Vec<Field> {
        Vec::new()
    }

    fn create(&self, _request: &Request, _cancel: &Cancel) -> Result<Created, String> {
        Ok(Created::default())
    }
}

/// The value when it has something in it, the fallback otherwise.
fn non_empty(value: &str, fallback: &str) -> String {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        fallback.to_owned()
    } else {
        trimmed.to_owned()
    }
}

/// The name as the artifact of a build tool: lower case words joined by dashes.
fn dashed(name: &str) -> String {
    artifact_name(name)
}

/// A group as the package of a class: every part keeps letters, digits and underscores.
fn sanitized_package(group: &str) -> String {
    let parts: Vec<String> = group
        .split('.')
        .filter(|part| !part.is_empty())
        .map(|part| {
            part.chars()
                .filter(|character| character.is_ascii_alphanumeric() || *character == '_')
                .collect::<String>()
        })
        .filter(|part| {
            !part.is_empty() && !part.starts_with(|character: char| character.is_ascii_digit())
        })
        .collect();
    if parts.is_empty() {
        "org.example".to_owned()
    } else {
        parts.join(".")
    }
}

/// The path of a source file of a package inside the folder of the project.
fn source_path(package: &str, class: &str) -> String {
    format!("src/main/java/{}/{class}.java", package.replace('.', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(name: &str, values: Values, git: bool) -> Request {
        Request {
            name: name.to_owned(),
            folder: PathBuf::from(name),
            values,
            git,
        }
    }

    /// A folder that looks like a JDK to the locator, so a test names its release.
    fn fake_jdk(feature: u32) -> PathBuf {
        let home = test_support::scratch(&format!("newproject-jdk-{feature}"));
        std::fs::create_dir_all(home.join("bin")).unwrap();
        std::fs::write(home.join("bin/javac"), "").unwrap();
        std::fs::write(home.join("bin/javac.exe"), "").unwrap();
        std::fs::write(
            home.join("release"),
            format!(
                "JAVA_VERSION=\"{feature}.0.2\"
"
            ),
        )
        .unwrap();
        home
    }

    fn plain_values(build: &str, dsl: &str, feature: u32, sample: bool) -> Values {
        let mut values = Values::default();
        values.set(BUILD, build);
        values.set(DSL, dsl);
        values.set(JDK, fake_jdk(feature).display().to_string());
        values.set_flag(SAMPLE, sample);
        values.set(GROUP, "org.example");
        values.set(ARTIFACT, "shop-front");
        values.set(VERSION, "1.0-SNAPSHOT");
        values
    }

    fn generator() -> PlainProject {
        PlainProject {
            jdks: Vec::new(),
            wrapper: false,
        }
    }

    fn file<'a>(files: &'a [(String, String)], name: &str) -> Option<&'a str> {
        files
            .iter()
            .find(|(path, _)| path == name)
            .map(|(_, text)| text.as_str())
    }

    #[test]
    fn a_maven_project_has_a_pom_with_the_release_of_the_jdk_and_junit() {
        let mut values = plain_values(MAVEN, GROOVY_DSL, 17, true);
        values.set(GROUP, "org.example");
        let files = generator()
            .create(&request("Shop Front", values, true), &Cancel::new())
            .unwrap()
            .files;

        let pom = file(&files, "pom.xml").expect("a pom");
        assert!(pom.contains("<groupId>org.example</groupId>"), "{pom}");
        assert!(pom.contains("<artifactId>shop-front</artifactId>"), "{pom}");
        assert!(pom.contains("<version>1.0-SNAPSHOT</version>"), "{pom}");
        assert!(pom.contains("<maven.compiler.release>17</maven.compiler.release>"));
        assert!(pom.contains("org.junit.jupiter"));
        let main = file(&files, "src/main/java/org/example/Main.java").expect("a main class");
        assert!(main.starts_with("package org.example;"), "{main}");
        assert!(
            main.contains("public static void main(String[] args)"),
            "{main}"
        );
        assert_eq!(file(&files, ".gitignore"), Some("target/\n.idea/\n*.iml\n"));
    }

    #[test]
    fn a_maven_project_without_sample_code_has_no_class_and_no_gitignore_without_git() {
        let files = generator()
            .create(
                &request("shop", plain_values(MAVEN, GROOVY_DSL, 21, false), false),
                &Cancel::new(),
            )
            .unwrap()
            .files;

        assert!(file(&files, "pom.xml").is_some());
        assert!(!files.iter().any(|(path, _)| path.ends_with(".java")));
        assert!(file(&files, ".gitignore").is_none());
    }

    #[test]
    fn a_gradle_project_in_groovy_has_a_script_and_a_settings_file() {
        let files = generator()
            .create(
                &request("shop", plain_values(GRADLE, GROOVY_DSL, 21, true), false),
                &Cancel::new(),
            )
            .unwrap()
            .files;

        assert_eq!(
            file(&files, "settings.gradle"),
            Some("rootProject.name = 'shop-front'\n")
        );
        let script = file(&files, "build.gradle").expect("a build script");
        assert!(script.contains("id 'java'"), "{script}");
        assert!(script.contains("JavaLanguageVersion.of(21)"), "{script}");
        assert!(script.contains("junit-jupiter"), "{script}");
        assert!(
            script.contains("mainClass = 'org.example.Main'"),
            "{script}"
        );
        assert!(file(&files, "build.gradle.kts").is_none());
        assert!(files.iter().any(|(path, _)| path.ends_with("Main.java")));
    }

    #[test]
    fn a_gradle_project_in_kotlin_has_kotlin_scripts() {
        let files = generator()
            .create(
                &request("shop", plain_values(GRADLE, KOTLIN_DSL, 25, true), true),
                &Cancel::new(),
            )
            .unwrap()
            .files;

        assert_eq!(
            file(&files, "settings.gradle.kts"),
            Some("rootProject.name = \"shop-front\"\n")
        );
        let script = file(&files, "build.gradle.kts").expect("a build script");
        assert!(script.contains("id(\"java\")"), "{script}");
        assert!(script.contains("JavaLanguageVersion.of(25)"), "{script}");
        assert!(
            script.contains("mainClass = \"org.example.Main\""),
            "{script}"
        );
        assert_eq!(
            file(&files, ".gitignore"),
            Some("build/\n.gradle/\n.idea/\n*.iml\n")
        );
    }

    #[test]
    fn the_package_of_the_sample_is_the_group_of_the_project() {
        let mut values = plain_values(MAVEN, GROOVY_DSL, 17, true);
        values.set(GROUP, "com.shop.orders");
        let files = generator()
            .create(&request("shop", values, false), &Cancel::new())
            .unwrap()
            .files;

        assert!(file(&files, "src/main/java/com/shop/orders/Main.java").is_some());
    }

    #[test]
    fn a_name_that_a_tool_cannot_take_becomes_an_artifact() {
        assert_eq!(dashed("Shop Front"), "shop-front");
        assert_eq!(dashed("2fast!"), "2fast");
        assert_eq!(dashed("  "), "demo");
        assert_eq!(sanitized_package("com.example"), "com.example");
        assert_eq!(sanitized_package("2bad"), "org.example");
    }

    #[test]
    fn the_spring_fields_name_the_choices_the_form_offers_without_a_server() {
        let fields = SpringProject {
            cache: PathBuf::from("cache"),
            server: DEFAULT_SERVER.to_owned(),
        }
        .fields();
        let by_key = |key: &str| fields.iter().find(|field| field.key == key).unwrap();

        assert_eq!(by_key(SERVER).default, DEFAULT_SERVER);
        assert_eq!(by_key(PROJECT_TYPE).default, "maven-project");
        assert_eq!(by_key(BOOT_VERSION).default, FALLBACK_BOOT);
        assert_eq!(by_key(JAVA_VERSION).default, "17");
        assert!(by_key(DEPENDENCIES).choices.is_empty());
        assert_eq!(
            by_key(DEPENDENCIES).kind,
            project_model::scaffold::Kind::Many
        );
    }

    #[test]
    fn an_empty_project_has_no_files() {
        let created = EmptyProject
            .create(&request("blank", Values::default(), false), &Cancel::new())
            .unwrap();

        assert!(created.files.is_empty());
        assert!(created.archive.is_none());
        assert_eq!(EmptyProject.label(), "Empty Project");
    }
}

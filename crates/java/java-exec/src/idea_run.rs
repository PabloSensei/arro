//! Run configurations of another IDE kept in the project: the files under `.idea`, the
//! `*.run.xml` files of the project folder, and the stored ones of its workspace. The
//! destination is an Arro configuration of the matching kind; the kinds Arro has no
//! program for stay out with their number for the summary of the import.
//!
//! `roxmltree` resolves no external entities, so the files are parsed as data only.

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use project_model::ProjectModel;
use runner::{
    BeforeLaunchStep, COMPOUND_PROVIDER, ForeignConfigs, RunConfig, RunConfigProvider, RunPlan,
    split_arguments,
};
use toml::Table;

use crate::junit::{JUnitTarget, PROVIDER_ID as TEST_PROVIDER};
use crate::provider::{JavaTarget, PROVIDER_ID as PROGRAM_PROVIDER};

/// The name of the other IDE in the offer and in the summary of an import.
pub const SOURCE: &str = "IntelliJ IDEA";
/// The provider the window reads the project through.
pub const PROVIDER_ID: &str = "foreign-run-configs";
/// The key a folder of the tree keeps in the settings, until the window has folders.
const FIELD_FOLDER: &str = "folder";

/// The folders whose files never hold a run configuration.
const SKIPPED_FOLDERS: &[&str] = &[
    "target",
    "build",
    "out",
    "node_modules",
    ".git",
    ".idea",
    ".gradle",
    ".arro",
];

/// A provider that only reads the files: the configurations it finds are offered for an
/// import, never discovered into the list.
pub struct IdeaRunProvider;

impl RunConfigProvider for IdeaRunProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn discover(&self, _model: &ProjectModel) -> base::Result<Vec<RunConfig>> {
        Ok(Vec::new())
    }

    fn plan(&self, _config: &RunConfig, _model: &ProjectModel) -> base::Result<RunPlan> {
        Err("a run configuration of another IDE is imported, not started".into())
    }

    fn foreign_configs(&self, model: &ProjectModel) -> Option<ForeignConfigs> {
        Some(read(model))
    }
}

/// Reads every configuration of the project, in the order of the files.
pub fn read(model: &ProjectModel) -> ForeignConfigs {
    let context = Context::new(model);
    let mut found = ForeignConfigs {
        source: SOURCE,
        ..ForeignConfigs::default()
    };
    let mut unsupported: BTreeMap<String, usize> = BTreeMap::new();
    let mut seen: HashSet<String> = HashSet::new();
    for file in config_files(model) {
        let text = match fs::read_to_string(&file) {
            Ok(text) => text,
            Err(error) => {
                found.warnings.push(format!("{}: {error}", file.display()));
                continue;
            }
        };
        let document = match roxmltree::Document::parse(&text) {
            Ok(document) => document,
            Err(error) => {
                found.warnings.push(format!("{}: {error}", file.display()));
                continue;
            }
        };
        let workspace = file.file_name().is_some_and(|name| name == "workspace.xml");
        let nodes: Vec<roxmltree::Node> = if workspace {
            document
                .descendants()
                .filter(|node| {
                    node.is_element()
                        && node.tag_name().name() == "component"
                        && node.attribute("name") == Some("RunManager")
                })
                .flat_map(|component| {
                    component.children().filter(|node| {
                        node.is_element() && node.tag_name().name() == "configuration"
                    })
                })
                .collect()
        } else {
            document
                .descendants()
                .filter(|node| node.is_element() && node.tag_name().name() == "configuration")
                .collect()
        };
        for node in nodes {
            let mut warnings = Vec::new();
            match configuration(node, &context, &mut warnings) {
                Parsed::Config(config) => {
                    if seen.insert(runner::foreign_identity(&config)) {
                        found.warnings.extend(warnings);
                        found.configs.push(config);
                    }
                }
                Parsed::Unsupported(kind) => *unsupported.entry(kind).or_default() += 1,
                Parsed::Skipped => {}
            }
        }
    }
    found.unsupported = unsupported.into_iter().collect();
    found
}

/// What one entry of a file became.
enum Parsed {
    Config(RunConfig),
    /// A kind Arro has no program for; the number goes into the summary.
    Unsupported(String),
    /// A temporary configuration or a template of the other IDE.
    Skipped,
}

/// What expansion of the macros of a path needs.
struct Context {
    root: PathBuf,
    modules: Vec<(String, PathBuf)>,
    home: Option<PathBuf>,
}

impl Context {
    fn new(model: &ProjectModel) -> Self {
        Self {
            root: model.root.clone(),
            modules: model
                .modules
                .iter()
                .map(|module| (module.id.clone(), module.dir.clone()))
                .collect(),
            home: base::program::home_dir(),
        }
    }

    /// The module of this project that a module name of the other IDE means. The other
    /// IDE names a Maven or Gradle module by its artifact or folder, `shop-api`, where the
    /// model names it `com.acme:shop-api`; a name no module matches stays as it is.
    fn module_id(&self, name: &str) -> String {
        if name.is_empty() || self.modules.iter().any(|(id, _)| id == name) {
            return name.to_owned();
        }
        let artifact = self.modules.iter().find(|(id, _)| {
            id.rsplit(':').next() == Some(name) || id.split(':').nth(1) == Some(name)
        });
        let folder = || {
            self.modules.iter().find(|(_, dir)| {
                dir.file_name()
                    .is_some_and(|folder| folder.to_string_lossy() == name)
            })
        };
        artifact
            .or_else(folder)
            .map_or_else(|| name.to_owned(), |(id, _)| id.clone())
    }

    fn module_dir(&self, module: &str) -> &Path {
        self.modules
            .iter()
            .find(|(id, _)| id == module)
            .map_or(self.root.as_path(), |(_, dir)| dir.as_path())
    }

    /// The module whose folder holds `dir`, so a build configuration of another IDE keeps
    /// the module it builds.
    fn module_of(&self, dir: &Path) -> String {
        self.modules
            .iter()
            .filter(|(_, module_dir)| dir.starts_with(module_dir))
            .max_by_key(|(_, module_dir)| module_dir.components().count())
            .map_or_else(String::new, |(id, _)| id.clone())
    }
}

/// Expands the macros of the other IDE. An unknown one stays as it is and is named, so
/// the summary of the import says what the user has to fix.
fn expand(text: &str, module: &str, context: &Context) -> (String, Vec<String>) {
    let mut expanded = String::with_capacity(text.len());
    let mut unknown = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find('$') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('$') else {
            expanded.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let name = &after[..end];
        let value = match name {
            "PROJECT_DIR" => Some(context.root.display().to_string()),
            "MODULE_DIR" | "MODULE_WORKING_DIR" => {
                Some(context.module_dir(module).display().to_string())
            }
            "USER_HOME" => context.home.as_ref().map(|home| home.display().to_string()),
            _ => None,
        };
        match value {
            Some(value) => expanded.push_str(&value),
            None => {
                expanded.push('$');
                expanded.push_str(name);
                expanded.push('$');
                if !unknown.iter().any(|known| known == name) {
                    unknown.push(name.to_owned());
                }
            }
        }
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    let warnings = unknown
        .into_iter()
        .map(|name| format!("${name}$ is not a path macro and stays as it is"))
        .collect();
    (expanded, warnings)
}

/// One configuration of a file, expanded with the module the file names.
fn configuration(node: roxmltree::Node, context: &Context, warnings: &mut Vec<String>) -> Parsed {
    if node.attribute("default") == Some("true") || node.attribute("temporary") == Some("true") {
        return Parsed::Skipped;
    }
    let Some(name) = node.attribute("name").filter(|name| !name.is_empty()) else {
        return Parsed::Skipped;
    };
    let name = name.to_owned();
    let display_name = name.clone();
    let module = context.module_id(&module_name(node));
    let steps = before_launch(node);
    let field = |option: &str| option_expanded(node, option, &module, context);
    let variables = envs(node, &module, context);
    let working_dir = field("WORKING_DIRECTORY")
        .or_else(|| field("workingDirPath"))
        .filter(|dir| !dir.trim().is_empty())
        .map(PathBuf::from);
    let kind = node.attribute("type").unwrap_or_default();
    let parsed = match kind {
        "Application" => {
            warn_unsupported_jre(node, &name, warnings);
            warn_unused_options(node, &name, warnings);
            let target = JavaTarget {
                main_class: field("MAIN_CLASS_NAME").unwrap_or_default(),
                program_args: field("PROGRAM_PARAMETERS")
                    .map(|args| split_arguments(&args))
                    .unwrap_or_default(),
                jvm_args: field("VM_PARAMETERS")
                    .map(|args| split_arguments(&args))
                    .unwrap_or_default(),
                module: module.clone(),
                env: variables,
                working_dir,
                ..JavaTarget::default()
            };
            program_config(name, target, steps)
        }
        "SpringBootApplicationConfigurationType" => {
            warn_unsupported_jre(node, &name, warnings);
            warn_unused_options(node, &name, warnings);
            let main_class = field("SPRING_BOOT_MAIN_CLASS")
                .or_else(|| field("MAIN_CLASS_NAME"))
                .unwrap_or_default();
            let profiles = field("ACTIVE_PROFILES")
                .map(|profiles| {
                    profiles
                        .split(',')
                        .map(str::trim)
                        .filter(|profile| !profile.is_empty())
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let target = JavaTarget {
                main_class,
                spring_boot: true,
                spring_profiles: profiles,
                program_args: field("PROGRAM_PARAMETERS")
                    .map(|args| split_arguments(&args))
                    .unwrap_or_default(),
                jvm_args: field("VM_PARAMETERS")
                    .map(|args| split_arguments(&args))
                    .unwrap_or_default(),
                module: module.clone(),
                env: variables,
                working_dir,
                ..JavaTarget::default()
            };
            program_config(name, target, steps)
        }
        "Remote" => {
            let host = field("HOST").unwrap_or_default();
            let port = field("PORT")
                .and_then(|port| port.trim().parse().ok())
                .unwrap_or(0);
            let target = JavaTarget {
                remote: true,
                host,
                port,
                module: module.clone(),
                ..JavaTarget::default()
            };
            program_config(name, target, steps)
        }
        "JUnit" => {
            warn_unsupported_jre(node, &name, warnings);
            let target = JUnitTarget {
                test_object: field("TEST_OBJECT")
                    .unwrap_or_else(|| "class".to_owned())
                    .trim()
                    .to_owned(),
                class: field("MAIN_CLASS_NAME").unwrap_or_default(),
                method: field("METHOD_NAME").unwrap_or_default(),
                package: field("PACKAGE_NAME").unwrap_or_default(),
                pattern: field("PATTERN").unwrap_or_default(),
                module: module.clone(),
                jvm_args: field("VM_PARAMETERS")
                    .map(|args| split_arguments(&args))
                    .unwrap_or_default(),
                env: variables,
                working_dir,
            };
            Parsed::Config(store_config(
                RunConfig {
                    name,
                    provider: TEST_PROVIDER.into(),
                    settings: table(&target),
                    ..RunConfig::default()
                },
                steps,
            ))
        }
        "MavenRunConfiguration" => {
            let goals = option_list(node, "goals");
            let mut task = goals.join(" ");
            for profile in profiles(node) {
                if !task.is_empty() {
                    task.push(' ');
                }
                task.push_str("-P");
                task.push_str(&profile);
            }
            let module = working_dir
                .as_deref()
                .map_or_else(|| module.clone(), |dir| context.module_of(dir));
            build_task(name, module, task, steps)
        }
        "GradleRunConfiguration" | "GradleExternalTaskConfigurationType" => {
            let mut parts = option_list(node, "taskNames");
            if let Some(parameters) =
                field("scriptParameters").filter(|text| !text.trim().is_empty())
            {
                parts.push(parameters);
            }
            let project = field("externalProjectPath").filter(|dir| !dir.trim().is_empty());
            let module = project
                .as_deref()
                .map(|dir| context.module_of(Path::new(dir)))
                .unwrap_or_else(|| module.clone());
            build_task(name, module, parts.join(" "), steps)
        }
        "CompoundRunConfigurationType" => {
            let mut children: Vec<String> = node
                .descendants()
                .filter(|node| node.is_element() && node.tag_name().name() == "toRun")
                .filter_map(|node| node.attribute("name"))
                .map(str::to_owned)
                .collect();
            children.dedup();
            Parsed::Config(store_config(
                RunConfig {
                    name,
                    provider: COMPOUND_PROVIDER.into(),
                    compound: children,
                    ..RunConfig::default()
                },
                steps,
            ))
        }
        kind => Parsed::Unsupported(type_title(kind)),
    };
    warnings.extend(
        unknown_macros(node, &module, context)
            .into_iter()
            .map(|warning| format!("{display_name}: {warning}")),
    );
    match parsed {
        Parsed::Config(mut config) => {
            if let Some(folder) = node
                .attribute("folderName")
                .filter(|folder| !folder.is_empty())
            {
                config
                    .settings
                    .insert(FIELD_FOLDER.into(), folder.to_owned().into());
            }
            Parsed::Config(config)
        }
        other => other,
    }
}

/// The value of an option with the macros of its path expanded.
fn option_expanded(
    node: roxmltree::Node,
    name: &str,
    module: &str,
    context: &Context,
) -> Option<String> {
    option_value(node, name).map(|value| expand(&value, module, context).0)
}

/// The environment of a configuration.
fn envs(node: roxmltree::Node, module: &str, context: &Context) -> BTreeMap<String, String> {
    let mut variables = BTreeMap::new();
    for env in node
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "env")
    {
        let Some(variable) = env.attribute("name") else {
            continue;
        };
        let value = env.attribute("value").unwrap_or_default();
        variables.insert(variable.to_owned(), expand(value, module, context).0);
    }
    variables
}

/// The macros of the values of a configuration that stay as they are, each named once.
fn unknown_macros(node: roxmltree::Node, module: &str, context: &Context) -> Vec<String> {
    let mut warnings: Vec<String> = Vec::new();
    for value in node
        .descendants()
        .filter_map(|child| child.attribute("value"))
    {
        for warning in expand(value, module, context).1 {
            if !warnings.contains(&warning) {
                warnings.push(warning);
            }
        }
    }
    warnings
}

/// Fills the parts the window keeps outside the settings of a provider.
fn store_config(mut config: RunConfig, steps: Vec<BeforeLaunchStep>) -> RunConfig {
    config.assemble_first = false;
    config.before_launch = Some(steps);
    config
}

fn program_config(name: String, target: JavaTarget, steps: Vec<BeforeLaunchStep>) -> Parsed {
    Parsed::Config(store_config(
        RunConfig {
            name,
            provider: PROGRAM_PROVIDER.into(),
            settings: table(&target),
            ..RunConfig::default()
        },
        steps,
    ))
}

fn build_task(name: String, module: String, task: String, steps: Vec<BeforeLaunchStep>) -> Parsed {
    let mut settings = Table::new();
    settings.insert("module".into(), module.into());
    settings.insert("task".into(), task.into());
    settings.insert("skip_tests".into(), false.into());
    Parsed::Config(store_config(
        RunConfig {
            name,
            provider: runner::BUILD_TASK_PROVIDER.into(),
            settings,
            ..RunConfig::default()
        },
        steps,
    ))
}

fn table<T: serde::Serialize>(value: &T) -> Table {
    Table::try_from(value).unwrap_or_default()
}

/// The module of the configuration, empty when it names none.
fn module_name(node: roxmltree::Node) -> String {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == "module")
        .or_else(|| {
            node.descendants()
                .find(|child| child.is_element() && child.tag_name().name() == "module")
        })
        .and_then(|module| module.attribute("name"))
        .unwrap_or_default()
        .to_owned()
}

/// The value of the `option` named `name`, or the first one below `node`.
fn option_value(node: roxmltree::Node, name: &str) -> Option<String> {
    node.descendants()
        .find(|child| {
            child.is_element()
                && child.tag_name().name() == "option"
                && child.attribute("name") == Some(name)
        })
        .and_then(|option| option.attribute("value"))
        .map(str::to_owned)
}

/// The values of a list option, as Maven writes its goals and Gradle its tasks: the
/// entries are `option` elements with a `value` and no `name`.
fn option_list(node: roxmltree::Node, name: &str) -> Vec<String> {
    let Some(option) = node.descendants().find(|child| {
        child.is_element()
            && child.tag_name().name() == "option"
            && child.attribute("name") == Some(name)
    }) else {
        return Vec::new();
    };
    if let Some(value) = option.attribute("value") {
        return split_arguments(value);
    }
    option
        .descendants()
        .filter(|child| child.is_element() && child.tag_name().name() == "option")
        .filter(|child| child.attribute("name").is_none())
        .filter_map(|child| child.attribute("value"))
        .map(str::to_owned)
        .collect()
}

/// The profiles Maven turns on for the configuration.
fn profiles(node: roxmltree::Node) -> Vec<String> {
    let Some(map) = node.descendants().find(|child| {
        child.is_element()
            && child.tag_name().name() == "option"
            && child.attribute("name") == Some("profilesMap")
    }) else {
        return Vec::new();
    };
    map.descendants()
        .filter(|child| child.is_element() && child.tag_name().name() == "entry")
        .filter(|entry| entry.attribute("value") != Some("false"))
        .filter_map(|entry| entry.attribute("key"))
        .map(str::to_owned)
        .collect()
}

/// The steps before the program: a build when the other IDE makes one, then the other
/// configurations it starts first.
fn before_launch(node: roxmltree::Node) -> Vec<BeforeLaunchStep> {
    let mut steps = Vec::new();
    for method in node
        .children()
        .filter(|child| child.is_element() && child.tag_name().name() == "method")
    {
        for option in method
            .children()
            .filter(|child| child.is_element() && child.tag_name().name() == "option")
        {
            if option.attribute("enabled") != Some("true") {
                continue;
            }
            match option.attribute("name") {
                Some("Make") => {
                    if !steps.contains(&BeforeLaunchStep::Build) {
                        steps.push(BeforeLaunchStep::Build);
                    }
                }
                Some("RunConfigurationTask") => {
                    if let Some(name) = option.attribute("run_configuration_name") {
                        push_configuration(&mut steps, name);
                    }
                }
                _ => {}
            }
        }
    }
    for task in node
        .descendants()
        .filter(|child| child.is_element() && child.tag_name().name() == "RunConfigurationTask")
    {
        if let Some(name) = task.attribute("run_configuration_name") {
            push_configuration(&mut steps, name);
        }
    }
    steps
}

fn push_configuration(steps: &mut Vec<BeforeLaunchStep>, name: &str) {
    let step = BeforeLaunchStep::Configuration {
        name: name.to_owned(),
    };
    if !steps.contains(&step) {
        steps.push(step);
    }
}

/// The JDK of the configuration is not part of an Arro configuration yet.
fn warn_unsupported_jre(node: roxmltree::Node, name: &str, warnings: &mut Vec<String>) {
    let enabled = option_value(node, "ALTERNATIVE_JRE_PATH_ENABLED") == Some("true".to_owned());
    let path = option_value(node, "ALTERNATIVE_JRE_PATH").unwrap_or_default();
    if enabled && !path.is_empty() {
        warnings.push(format!(
            "{name}: the JDK {path} of the configuration is not kept, the project SDK runs it"
        ));
    }
}

/// Options the other IDE has that change how the command is built and have no Arro
/// counterpart yet. They stay in the warning of the import instead of being dropped
/// silently.
fn warn_unused_options(node: roxmltree::Node, name: &str, warnings: &mut Vec<String>) {
    if option_value(node, "INCLUDE_PROVIDED_SCOPE") == Some("true".to_owned()) {
        warnings.push(format!(
            "{name}: INCLUDE_PROVIDED_SCOPE is not kept, the classpath is the same as for a run"
        ));
    }
    if let Some(shorten) = option_value(node, "SHORTEN_COMMAND_LINE")
        && !shorten.is_empty()
        && shorten != "NONE"
    {
        warnings.push(format!(
            "{name}: SHORTEN_COMMAND_LINE={shorten} is not kept, the program starts with a classpath file"
        ));
    }
}

/// The name of a kind Arro cannot import, as the other IDE shows it.
fn type_title(kind: &str) -> String {
    match kind {
        "JavascriptDebugType" => "JavaScript Debug".to_owned(),
        "NodeJSConfigurationType" => "Node.js".to_owned(),
        "js.build_tools.npm" => "npm".to_owned(),
        "DockerRunConfigurationType" | "docker-deploy" => "Docker".to_owned(),
        "TerraformConfigurationType" => "Terraform".to_owned(),
        "PythonConfigurationType" => "Python".to_owned(),
        "PyTestConfigurationType" => "Python tests".to_owned(),
        "" | "unknown" => "Unknown".to_owned(),
        kind => kind.to_owned(),
    }
}

/// The files that hold a configuration: the project files of the other IDE and the
/// `*.run.xml` files anywhere in the project outside the output folders.
fn config_files(model: &ProjectModel) -> Vec<PathBuf> {
    let root = &model.root;
    let mut files = Vec::new();
    collect_xml(&root.join(".idea").join("runConfigurations"), &mut files);
    let workspace = root.join(".idea").join("workspace.xml");
    if workspace.is_file() {
        files.push(workspace);
    }
    let mut excluded: Vec<PathBuf> = model
        .modules
        .iter()
        .flat_map(|module| module.excluded_roots.clone())
        .collect();
    excluded.retain(|path| path != root);
    walk(root, &excluded, &mut files);
    files.sort();
    files.dedup();
    files
}

fn collect_xml(folder: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_xml(&path, files);
        } else if path.extension().is_some_and(|extension| extension == "xml") {
            files.push(path);
        }
    }
}

fn walk(folder: &Path, excluded: &[PathBuf], files: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(folder) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if SKIPPED_FOLDERS.contains(&name.as_ref()) {
                continue;
            }
            if excluded.iter().any(|excluded| path.starts_with(excluded)) {
                continue;
            }
            walk(&path, excluded, files);
        } else if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().ends_with(".run.xml"))
        {
            files.push(path);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use project_model::Module;

    use super::*;

    fn project(name: &str) -> (test_support::TempDir, ProjectModel) {
        let folder = test_support::TempDir::new(name);
        let root = folder.path().to_path_buf();
        fs::create_dir_all(root.join(".idea/runConfigurations")).unwrap();
        fs::create_dir_all(root.join(".run")).unwrap();
        fs::create_dir_all(root.join("module")).unwrap();
        let model = ProjectModel {
            root: root.clone(),
            resolved: true,
            modules: vec![Module {
                id: "demo".into(),
                name: "demo".into(),
                dir: root.clone(),
                ..Module::default()
            }],
            ..ProjectModel::default()
        };
        (folder, model)
    }

    fn normalized(text: &str) -> String {
        text.replace('\\', "/")
    }

    fn workspace_xml() -> &'static str {
        r#"<?xml version="1.0" encoding="UTF-8"?>
<project version="4">
  <component name="RunManager" selected="Application.App">
    <configuration name="App" type="Application" factoryName="Application">
      <option name="MAIN_CLASS_NAME" value="demo.Main" />
      <option name="PROGRAM_PARAMETERS" value="--one &quot;two words&quot;" />
      <option name="VM_PARAMETERS" value="-Xmx256m -Dhome=$PROJECT_DIR$/conf" />
      <option name="WORKING_DIRECTORY" value="$MODULE_DIR$" />
      <option name="ALTERNATIVE_JRE_PATH" value="C:/jdk/21" />
      <option name="ALTERNATIVE_JRE_PATH_ENABLED" value="true" />
      <option name="INCLUDE_PROVIDED_SCOPE" value="true" />
      <option name="SHORTEN_COMMAND_LINE" value="JAR_MANIFEST" />
      <module name="demo" />
      <envs>
        <env name="APP_ENV" value="dev" />
        <env name="APP_HOME" value="$USER_HOME$/app" />
      </envs>
      <method v="2">
        <option name="Make" enabled="true" />
        <option name="RunConfigurationTask" enabled="true" run_configuration_name="Prep" />
      </method>
    </configuration>
    <configuration name="Web" type="SpringBootApplicationConfigurationType" factoryName="Spring Boot">
      <option name="SPRING_BOOT_MAIN_CLASS" value="demo.Web" />
      <option name="ACTIVE_PROFILES" value="dev, local" />
      <option name="VM_PARAMETERS" value="-Dspring.devtools.restart.enabled=false" />
      <option name="WORKING_DIRECTORY" value="$PROJECT_DIR$" />
      <module name="demo" />
      <method v="2" />
    </configuration>
    <configuration name="AppTest" type="JUnit" factoryName="JUnit">
      <option name="TEST_OBJECT" value="class" />
      <option name="MAIN_CLASS_NAME" value="demo.AppTest" />
      <option name="VM_PARAMETERS" value="-ea" />
      <module name="demo" />
      <method v="2" />
    </configuration>
    <configuration name="AppTest.passes" type="JUnit" factoryName="JUnit">
      <option name="TEST_OBJECT" value="method" />
      <option name="MAIN_CLASS_NAME" value="demo.AppTest" />
      <option name="METHOD_NAME" value="passes" />
      <option name="PACKAGE_NAME" value="demo" />
      <module name="demo" />
      <method v="2" />
    </configuration>
    <configuration name="AppTest.package" type="JUnit" factoryName="JUnit">
      <option name="TEST_OBJECT" value="package" />
      <option name="PACKAGE_NAME" value="demo.tests" />
      <module name="demo" />
      <method v="2" />
    </configuration>
    <configuration name="Remote" type="Remote" factoryName="Remote">
      <option name="HOST" value="192.168.1.5" />
      <option name="PORT" value="5006" />
      <method v="2" />
    </configuration>
    <configuration name="clean" type="MavenRunConfiguration" factoryName="Maven">
      <MavenSettings>
        <option name="myRunnerParameters">
          <MavenRunnerParameters>
            <option name="goals">
              <list>
                <option value="clean" />
                <option value="package" />
              </list>
            </option>
            <option name="profilesMap">
              <map>
                <entry key="dev" value="true" />
                <entry key="slow" value="false" />
              </map>
            </option>
            <option name="workingDirPath" value="$PROJECT_DIR$" />
          </MavenRunnerParameters>
        </option>
      </MavenSettings>
      <method v="2" />
    </configuration>
    <configuration name="build" type="GradleRunConfiguration" factoryName="Gradle">
      <ExternalSystemSettings>
        <option name="externalProjectPath" value="$PROJECT_DIR$/module" />
        <option name="scriptParameters" value="--info" />
        <option name="taskNames">
          <list>
            <option value="clean" />
            <option value="build" />
          </list>
        </option>
      </ExternalSystemSettings>
      <method v="2" />
    </configuration>
    <configuration name="All" type="CompoundRunConfigurationType" factoryName="Compound">
      <toRun name="App" type="Application" />
      <toRun name="Web" type="SpringBootApplicationConfigurationType" />
      <method v="2" />
    </configuration>
    <configuration name="Temporary" type="Application" temporary="true">
      <option name="MAIN_CLASS_NAME" value="demo.Temp" />
    </configuration>
    <configuration default="true" type="Application" factoryName="Application">
      <option name="MAIN_CLASS_NAME" value="demo.Template" />
    </configuration>
    <configuration name="Debug JS" type="JavascriptDebugType" factoryName="JavaScript Debug">
      <method v="2" />
    </configuration>
  </component>
</project>
"#
    }

    #[test]
    fn every_kind_becomes_a_configuration_of_the_matching_provider() {
        let (folder, model) = project("idea-run-kinds");
        fs::write(folder.path().join(".idea/workspace.xml"), workspace_xml()).unwrap();

        let found = read(&model);
        let names: Vec<&str> = found
            .configs
            .iter()
            .map(|config| config.name.as_str())
            .collect();

        assert_eq!(
            names,
            [
                "App",
                "Web",
                "AppTest",
                "AppTest.passes",
                "AppTest.package",
                "Remote",
                "clean",
                "build",
                "All"
            ],
            "{:?}",
            found.warnings
        );
        let app = found.configs.iter().find(|c| c.name == "App").unwrap();
        assert_eq!(app.provider, PROGRAM_PROVIDER);
        let target: JavaTarget = app.settings.clone().try_into().unwrap();
        assert_eq!(target.main_class, "demo.Main");
        assert_eq!(target.module, "demo");
        assert_eq!(target.program_args, ["--one", "two words"]);
        assert_eq!(
            normalized(&target.jvm_args.join(" ")),
            format!(
                "-Xmx256m -Dhome={}",
                normalized(&model.root.join("conf").display().to_string())
            )
        );
        assert_eq!(
            target.working_dir.as_deref(),
            Some(model.root.as_path()),
            "MODULE_DIR of the root module"
        );
        assert_eq!(target.env.get("APP_ENV").map(String::as_str), Some("dev"));
        if let Some(home) = base::program::home_dir() {
            assert_eq!(
                normalized(target.env.get("APP_HOME").unwrap()),
                normalized(&format!("{}/app", home.display()))
            );
        }
        assert_eq!(
            app.before_launch_steps(),
            [
                BeforeLaunchStep::Build,
                BeforeLaunchStep::Configuration {
                    name: "Prep".into()
                }
            ]
        );
        assert!(
            found
                .warnings
                .iter()
                .any(|warning| warning.contains("SHORTEN_COMMAND_LINE")),
            "{:?}",
            found.warnings
        );
        assert!(
            found
                .warnings
                .iter()
                .any(|warning| warning.contains("INCLUDE_PROVIDED_SCOPE")),
            "{:?}",
            found.warnings
        );
        assert!(
            found
                .warnings
                .iter()
                .any(|warning| warning.contains("C:/jdk/21")),
            "{:?}",
            found.warnings
        );

        let web = found.configs.iter().find(|c| c.name == "Web").unwrap();
        let target: JavaTarget = web.settings.clone().try_into().unwrap();
        assert!(target.spring_boot);
        assert_eq!(target.spring_profiles, ["dev", "local"]);
        assert_eq!(target.main_class, "demo.Web");
        assert!(web.before_launch_steps().is_empty());

        let class = found.configs.iter().find(|c| c.name == "AppTest").unwrap();
        assert_eq!(class.provider, TEST_PROVIDER);
        let target: JUnitTarget = class.settings.clone().try_into().unwrap();
        assert_eq!(target.test_object, "class");
        assert_eq!(target.class, "demo.AppTest");
        assert_eq!(target.jvm_args, ["-ea"]);

        let method = found
            .configs
            .iter()
            .find(|c| c.name == "AppTest.passes")
            .unwrap();
        let request = {
            let target: JUnitTarget = method.settings.clone().try_into().unwrap();
            target.request()
        };
        assert_eq!(request.suite.as_deref(), Some("demo.AppTest"));
        assert_eq!(request.case.as_deref(), Some("passes"));
        assert_eq!(request.package, None);

        let package = found
            .configs
            .iter()
            .find(|c| c.name == "AppTest.package")
            .unwrap();
        let request = {
            let target: JUnitTarget = package.settings.clone().try_into().unwrap();
            target.request()
        };
        assert_eq!(request.package.as_deref(), Some("demo.tests"));
        assert_eq!(request.suite, None);

        let remote = found.configs.iter().find(|c| c.name == "Remote").unwrap();
        let target: JavaTarget = remote.settings.clone().try_into().unwrap();
        assert!(target.remote);
        assert_eq!(target.host, "192.168.1.5");
        assert_eq!(target.port, 5006);

        let maven = found.configs.iter().find(|c| c.name == "clean").unwrap();
        assert_eq!(maven.provider, runner::BUILD_TASK_PROVIDER);
        assert_eq!(maven.settings["task"].as_str(), Some("clean package -Pdev"));
        assert_eq!(maven.settings["module"].as_str(), Some("demo"));

        let gradle = found.configs.iter().find(|c| c.name == "build").unwrap();
        assert_eq!(gradle.provider, runner::BUILD_TASK_PROVIDER);
        assert_eq!(gradle.settings["task"].as_str(), Some("clean build --info"));
        assert_eq!(gradle.settings["module"].as_str(), Some("demo"));

        let compound = found.configs.iter().find(|c| c.name == "All").unwrap();
        assert!(compound.is_compound());
        assert_eq!(compound.compound, ["App", "Web"]);

        assert_eq!(
            found.unsupported,
            [("JavaScript Debug".to_owned(), 1)],
            "a template and a temporary configuration are not counted"
        );
    }

    #[test]
    fn project_files_of_the_other_ide_are_read_the_same_way() {
        let (folder, model) = project("idea-run-files");
        let xml = r#"<component name="ProjectRunConfigurationManager">
  <configuration name="FromFile" type="Application" factoryName="Application">
    <option name="MAIN_CLASS_NAME" value="demo.FromFile" />
    <module name="demo" />
    <method v="2" />
  </configuration>
</component>"#;
        fs::write(
            folder.path().join(".idea/runConfigurations/FromFile.xml"),
            xml,
        )
        .unwrap();
        let run = r#"<component name="ProjectRunConfigurationManager">
  <configuration name="FromRun" type="Application" factoryName="Application" folderName="Team">
    <option name="MAIN_CLASS_NAME" value="demo.FromRun" />
    <method v="2" />
  </configuration>
</component>"#;
        fs::write(folder.path().join(".run/FromRun.run.xml"), run).unwrap();
        fs::write(
            folder.path().join("module/FromModule.run.xml"),
            run.replace("FromRun", "FromModule"),
        )
        .unwrap();
        fs::create_dir_all(folder.path().join("target")).unwrap();
        fs::write(folder.path().join("target/Ignored.run.xml"), run).unwrap();

        let found = read(&model);
        let names: Vec<&str> = found
            .configs
            .iter()
            .map(|config| config.name.as_str())
            .collect();

        assert_eq!(names, ["FromFile", "FromRun", "FromModule"]);
        let from_run = found.configs.iter().find(|c| c.name == "FromRun").unwrap();
        assert_eq!(
            from_run
                .settings
                .get("folder")
                .and_then(|folder| folder.as_str()),
            Some("Team")
        );
    }

    #[test]
    fn an_unknown_macro_stays_and_is_named() {
        let (folder, model) = project("idea-run-macro");
        let xml = r#"<component name="ProjectRunConfigurationManager">
  <configuration name="Macro" type="Application" factoryName="Application">
    <option name="MAIN_CLASS_NAME" value="demo.Macro" />
    <option name="PROGRAM_PARAMETERS" value="--home=$UNKNOWN_HOME$/data" />
    <method v="2" />
  </configuration>
</component>"#;
        fs::write(folder.path().join(".run/Macro.run.xml"), xml).unwrap();

        let found = read(&model);

        let target: JavaTarget = found.configs[0].settings.clone().try_into().unwrap();
        assert_eq!(target.program_args, ["--home=$UNKNOWN_HOME$/data"]);
        assert!(
            found
                .warnings
                .iter()
                .any(|warning| warning.contains("UNKNOWN_HOME")),
            "{:?}",
            found.warnings
        );
    }

    #[test]
    fn a_broken_file_is_a_warning_and_the_rest_is_read() {
        let (folder, model) = project("idea-run-broken");
        fs::write(folder.path().join(".idea/workspace.xml"), "<project").unwrap();
        let xml = r#"<component name="ProjectRunConfigurationManager">
  <configuration name="Fine" type="Application" factoryName="Application">
    <option name="MAIN_CLASS_NAME" value="demo.Fine" />
    <method v="2" />
  </configuration>
</component>"#;
        fs::write(folder.path().join(".run/Fine.run.xml"), xml).unwrap();

        let found = read(&model);

        assert_eq!(found.configs.len(), 1);
        assert!(
            found
                .warnings
                .iter()
                .any(|warning| warning.contains("workspace.xml")),
            "{:?}",
            found.warnings
        );
    }

    #[test]
    fn an_excluded_folder_is_not_walked() {
        let (folder, model) = project("idea-run-excluded");
        let mut model = model;
        let excluded = folder.path().join("module");
        model.modules[0].excluded_roots.push(excluded.clone());
        let xml = r#"<component name="ProjectRunConfigurationManager">
  <configuration name="Hidden" type="Application" factoryName="Application">
    <option name="MAIN_CLASS_NAME" value="demo.Hidden" />
  </configuration>
</component>"#;
        fs::write(excluded.join("Hidden.run.xml"), xml).unwrap();

        let found = read(&model);

        assert!(found.configs.is_empty());
    }

    #[test]
    fn a_file_without_configurations_reads_as_nothing() {
        let (_, model) = project("idea-run-empty");

        let found = read(&model);

        assert!(found.is_empty());
        assert_eq!(found.source, SOURCE);
    }

    #[test]
    fn the_provider_offers_the_files_and_starts_nothing() {
        let (folder, model) = project("idea-run-provider");
        let xml = r#"<component name="ProjectRunConfigurationManager">
  <configuration name="One" type="Application" factoryName="Application">
    <option name="MAIN_CLASS_NAME" value="demo.One" />
  </configuration>
</component>"#;
        fs::write(folder.path().join(".run/One.run.xml"), xml).unwrap();

        let provider = IdeaRunProvider;
        assert!(provider.discover(&model).unwrap().is_empty());
        assert_eq!(provider.foreign_configs(&model).unwrap().configs.len(), 1);
        assert!(provider.plan(&RunConfig::default(), &model).is_err());
    }
}

#[cfg(test)]
mod module_tests {
    use super::Context;
    use std::path::PathBuf;

    #[test]
    fn a_module_name_of_the_other_ide_finds_the_module_of_the_model() {
        let context = Context {
            root: PathBuf::from("/shop"),
            modules: vec![
                ("com.acme:shop-api".to_owned(), PathBuf::from("/shop/api")),
                ("web".to_owned(), PathBuf::from("/shop/frontend")),
            ],
            home: None,
        };
        assert_eq!(context.module_id("shop-api"), "com.acme:shop-api");
        assert_eq!(context.module_id("com.acme:shop-api"), "com.acme:shop-api");
        assert_eq!(context.module_id("frontend"), "web");
        assert_eq!(context.module_id("gone"), "gone");
        assert_eq!(context.module_id(""), "");
    }
}

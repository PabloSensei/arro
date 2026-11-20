use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use base::Cancel;
use base::Result;
use java_project::{BuildEnv, CoverageTools, Jdk};
use proc_sup::ProcessSpec;
use project_model::{BuildSystem, MAIN_UNIT, Module, ProjectModel};
use runner::{
    ConfigField, ConfigTemplate, CoveragePlan, LaunchPoint, ProfilePlan, RunConfig,
    RunConfigProvider, RunPlan, format_environment, join_arguments, parse_environment,
    split_arguments,
};
use serde::{Deserialize, Serialize};
use toml::Table;

use crate::{JvmFlags, classpath_jar, find_entry_points, parse_entry_points};

pub const PROVIDER_ID: &str = "java";
/// The keys the Java fields use in `settings`, the names of [`JavaTarget`] too.
const FIELD_MAIN_CLASS: &str = "main_class";
const FIELD_MODULE: &str = "module";
const FIELD_PROGRAM_ARGS: &str = "program_args";
const FIELD_VM_OPTIONS: &str = "jvm_args";
const FIELD_ENV: &str = "env";
const FIELD_WORKING_DIR: &str = "working_dir";
const FIELD_SPRING_PROFILES: &str = "spring_profiles";
const FIELD_HOST: &str = "host";
const FIELD_PORT: &str = "port";
const FIELD_AGENT: &str = "agent";
/// The only kind of configuration the Add button offers.
const TEMPLATE_APPLICATION: &str = "application";
/// The template that attaches to a JVM another process started.
const TEMPLATE_REMOTE: &str = "remote";
/// The groups of the Services window: an application that starts a web server goes to
/// the first one, a plain program is listed only when the user asks for every
/// configuration.
const SPRING_BOOT_GROUP: &str = "spring-boot";
const APPLICATION_GROUP: &str = "application";
/// What the new configuration is called before the user renames it.
const NEW_CONFIG_NAME: &str = "Unnamed";
/// The host and the port of a remote JVM before the user changes them.
pub const REMOTE_HOST: &str = "localhost";
pub const REMOTE_PORT: u16 = 5005;
/// How long a program gets for its shutdown hooks after Ctrl+C.
const GRACE: Duration = Duration::from_secs(5);
/// Makes Spring Boot color its log, the console renders ANSI.
const SPRING_ANSI: &str = "-Dspring.output.ansi.enabled=always";
/// The process stops before `main` and waits for the debugger on a free loopback port.
pub const DEBUG_AGENT: &str =
    "-agentlib:jdwp=transport=dt_socket,server=y,suspend=y,address=127.0.0.1:0";

/// The Java part of a run configuration, kept in its `settings`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JavaTarget {
    pub module: String,
    pub unit: Option<String>,
    pub main_class: String,
    pub spring_boot: bool,
    pub spring_profiles: Vec<String>,
    pub jvm_args: Vec<String>,
    pub program_args: Vec<String>,
    pub env: BTreeMap<String, String>,
    /// The module directory if not set.
    pub working_dir: Option<PathBuf>,
    /// The configuration attaches to a JVM somebody else started rather than runs one.
    pub remote: bool,
    pub host: String,
    pub port: u16,
}

/// A configuration written before the before launch list existed builds through its flag;
/// one with a list has the build as a step of that list instead.
fn needs_build(config: &RunConfig) -> bool {
    config.assemble_first && config.before_launch.is_none()
}

/// Starts `java` directly with an explicit classpath. Only a direct start gets Ctrl+C
/// through to shutdown hooks, a start through the build tool ends without them.
pub struct JavaProvider {
    env: BuildEnv,
    tool: Arc<dyn BuildSystem>,
}

impl JavaProvider {
    pub fn new(env: BuildEnv, tool: Arc<dyn BuildSystem>) -> Self {
        Self { env, tool }
    }

    fn jdk(&self, model: &ProjectModel) -> Result<Jdk> {
        self.env.project_jdk(&model.root)
    }

    /// The Java part of a stored configuration. A table that cannot be read as one is an
    /// empty target, so the dialog can still show and fix it.
    fn target(config: &RunConfig) -> JavaTarget {
        config.settings.clone().try_into().unwrap_or_default()
    }

    fn store(target: &JavaTarget) -> Result<Table> {
        Ok(Table::try_from(target.clone())?)
    }

    /// The port of the remote JVM, the usual one when the configuration names none.
    fn remote_port(target: &JavaTarget) -> u16 {
        if target.port == 0 {
            REMOTE_PORT
        } else {
            target.port
        }
    }

    /// The host of the remote JVM, the local machine when the configuration has none.
    fn remote_host(target: &JavaTarget) -> &str {
        let host = target.host.trim();
        if host.is_empty() { REMOTE_HOST } else { host }
    }

    /// The option the remote JVM starts with, which opens the port for the debugger.
    fn agent_option(port: u16) -> String {
        format!("-agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=*:{port}")
    }

    /// The configuration `discover` makes for a class with `main`. The launch points of a
    /// file use the same shape, so a name found in the gutter is the name in the list.
    fn configuration(module: &Module, class: &str, spring_boot: bool) -> Result<RunConfig> {
        let simple = class.rsplit(['.', '$']).next().unwrap_or(class);
        let target = JavaTarget {
            module: module.id.clone(),
            main_class: class.to_owned(),
            spring_boot,
            ..JavaTarget::default()
        };
        Ok(RunConfig {
            name: simple.to_owned(),
            provider: PROVIDER_ID.into(),
            assemble_first: true,
            settings: Self::store(&target)?,
            ..RunConfig::default()
        })
    }

    /// The module that holds `file`: the innermost one whose folder contains it.
    fn module_of<'a>(model: &'a ProjectModel, file: &Path) -> Option<&'a Module> {
        model
            .modules
            .iter()
            .filter(|module| file.starts_with(&module.dir))
            .max_by_key(|module| module.dir.components().count())
    }

    fn program(&self, target: &JavaTarget, model: &ProjectModel, jdk: &Jdk) -> Result<ProcessSpec> {
        self.program_with(target, model, jdk, false, None, None)
    }

    #[allow(clippy::too_many_arguments)]
    fn program_with(
        &self,
        target: &JavaTarget,
        model: &ProjectModel,
        jdk: &Jdk,
        debug: bool,
        coverage: Option<(&CoverageTools, &Path)>,
        recording: Option<&Path>,
    ) -> Result<ProcessSpec> {
        let unit = target.unit.as_deref().unwrap_or(MAIN_UNIT);
        let module = model
            .module(&target.module)
            .ok_or_else(|| format!("no module {}", target.module))?;
        let paths = model.runtime_paths(&target.module, unit);
        if paths.is_empty() {
            return Err(format!("{}: nothing to run in {unit}", target.module).into());
        }
        let jar = classpath_jar(&self.env.cache_root(), &model.root, &paths)?;
        let directory = match &target.working_dir {
            Some(dir) if dir.is_absolute() => dir.clone(),
            Some(dir) => model.root.join(dir),
            None => module.dir.clone(),
        };
        let flags = JvmFlags::for_jdk(jdk.feature);
        let mut spec = ProcessSpec::new(jdk.java())
            .cwd(directory)
            .args(flags.output_encoding())
            .label("Application")
            // A program may read its input, as `Scanner(System.in)` does.
            .input();
        if debug {
            spec = spec.arg(flags.debug_agent("127.0.0.1", 0, true));
        }
        if let Some((tools, exec)) = coverage {
            spec = spec.arg(tools.java_agent(exec));
        }
        if let Some(recording) = recording {
            spec = spec.arg(crate::profile::recording_flag(recording));
        }
        if target.spring_boot {
            spec = spec.arg(SPRING_ANSI);
            if !target.spring_profiles.is_empty() {
                let profiles = target.spring_profiles.join(",");
                spec = spec.arg(format!("-Dspring.profiles.active={profiles}"));
            }
        }
        spec = spec
            .args(&target.jvm_args)
            .arg("-cp")
            .arg(jar)
            .arg(&target.main_class)
            .args(&target.program_args);
        spec = self.env.apply(spec, jdk);
        for (name, value) in &target.env {
            spec = spec.env(name, value);
        }
        Ok(spec)
    }
}

impl RunConfigProvider for JavaProvider {
    fn id(&self) -> &'static str {
        PROVIDER_ID
    }

    fn icon(&self, config: &RunConfig) -> &'static str {
        if Self::target(config).spring_boot {
            "run/springBoot"
        } else {
            "run/application"
        }
    }

    fn discover(&self, model: &ProjectModel) -> Result<Vec<RunConfig>> {
        let jdk = self.jdk(model)?;
        let mut configs = Vec::new();
        for module in &model.modules {
            let Some(unit) = module.unit(MAIN_UNIT) else {
                continue;
            };
            for entry in find_entry_points(&unit.source_roots, jdk.feature) {
                configs.push(Self::configuration(
                    module,
                    &entry.class,
                    entry.spring_boot,
                )?);
            }
        }
        Ok(configs)
    }

    fn plan(&self, config: &RunConfig, model: &ProjectModel) -> Result<RunPlan> {
        if !model.resolved {
            return Err("the project model has no dependencies yet, sync first".into());
        }
        let target: JavaTarget = config.settings.clone().try_into()?;
        if target.remote {
            return Err(
                "the remote configuration attaches to a running JVM, it starts nothing".into(),
            );
        }
        let jdk = self.jdk(model)?;
        let before = needs_build(config)
            .then(|| self.tool.assemble_if_needed(&model.root))
            .transpose()?
            .flatten();
        Ok(RunPlan {
            before,
            program: self.program(&target, model, &jdk)?,
            grace: GRACE,
        })
    }

    fn debug_plan(&self, config: &RunConfig, model: &ProjectModel) -> Result<RunPlan> {
        if !model.resolved {
            return Err("the project model has no dependencies yet, sync first".into());
        }
        let target: JavaTarget = config.settings.clone().try_into()?;
        if target.remote {
            return Err(
                "the remote configuration attaches to a running JVM, it starts nothing".into(),
            );
        }
        let jdk = self.jdk(model)?;
        let before = needs_build(config)
            .then(|| self.tool.assemble_if_needed(&model.root))
            .transpose()?
            .flatten();
        Ok(RunPlan {
            before,
            program: self.program_with(&target, model, &jdk, true, None, None)?,
            grace: GRACE,
        })
    }

    fn supports_coverage(&self) -> bool {
        true
    }

    /// The remote configuration attaches to a JVM that another process started.
    fn attach_plan(
        &self,
        config: &RunConfig,
        _model: &ProjectModel,
    ) -> Result<Option<runner::AttachPlan>> {
        let target: JavaTarget = config.settings.clone().try_into()?;
        if !target.remote {
            return Ok(None);
        }
        Ok(Some(runner::AttachPlan {
            host: Self::remote_host(&target).to_owned(),
            port: Self::remote_port(&target),
            name: config.name.clone(),
            project: None,
        }))
    }

    /// The same program with the coverage agent, whose data goes to a file of the cache
    /// of the project.
    fn coverage_plan(&self, config: &RunConfig, model: &ProjectModel) -> Result<CoveragePlan> {
        if !model.resolved {
            return Err("the project model has no dependencies yet, sync first".into());
        }
        let target: JavaTarget = config.settings.clone().try_into()?;
        let jdk = self.jdk(model)?;
        let tools = self.env.coverage_tools()?;
        if !tools.agent.is_file() {
            return Err(format!("no coverage agent at {}", tools.agent.display()).into());
        }
        let exec = crate::coverage::exec_file(&self.env.cache_root(), &config.name)?;
        let before = needs_build(config)
            .then(|| self.tool.assemble_if_needed(&model.root))
            .transpose()?
            .flatten();
        let program =
            self.program_with(&target, model, &jdk, false, Some((&tools, &exec)), None)?;
        Ok(CoveragePlan {
            plan: RunPlan {
                before,
                program,
                grace: GRACE,
            },
            exec,
            note: None,
        })
    }

    fn coverage_report(
        &self,
        exec: &Path,
        model: &ProjectModel,
    ) -> Result<lang_api::CoverageReport> {
        crate::coverage::report(&self.env, exec, model)
    }

    fn supports_profiler(&self) -> bool {
        true
    }

    /// The same program with its samples recorded to a file of the cache of the project.
    fn profile_plan(&self, config: &RunConfig, model: &ProjectModel) -> Result<ProfilePlan> {
        if !model.resolved {
            return Err("the project model has no dependencies yet, sync first".into());
        }
        let target: JavaTarget = config.settings.clone().try_into()?;
        let jdk = self.jdk(model)?;
        if !crate::profile::supported(&jdk) {
            return Err(format!(
                "the profiler needs Flight Recorder, which JDK {} does not have; use 8u262 or newer",
                jdk.version
            )
            .into());
        }
        let recording = crate::profile::recording_file(&self.env.cache_root(), &config.name)?;
        let before = needs_build(config)
            .then(|| self.tool.assemble_if_needed(&model.root))
            .transpose()?
            .flatten();
        let program = self.program_with(&target, model, &jdk, false, None, Some(&recording))?;
        Ok(ProfilePlan {
            plan: RunPlan {
                before,
                program,
                grace: GRACE,
            },
            recording,
            note: None,
        })
    }

    fn profile_snapshot(
        &self,
        recording: &Path,
        model: &ProjectModel,
    ) -> Result<lang_api::ProfileSnapshot> {
        let jdk = self.jdk(model)?;
        crate::profile::snapshot(&self.env, &jdk, recording, model)
    }

    fn profile_report(
        &self,
        recording: &Path,
        model: &ProjectModel,
    ) -> Result<lang_api::ProfileReport> {
        crate::profile::report_of(&self.env, recording, model)
    }

    fn profile_recordings(&self, _model: &ProjectModel) -> Result<Vec<lang_api::ProfileRecording>> {
        Ok(crate::profile::recordings(&self.env.cache_root()))
    }

    fn profile_run_recordings(&self, recording: &Path) -> Vec<std::path::PathBuf> {
        crate::profile::run_recordings(recording)
    }

    fn is_heap_dump(&self, path: &Path) -> bool {
        path.extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("hprof"))
    }

    fn supports_heap_dump(&self) -> bool {
        true
    }

    fn capture_heap_dump(
        &self,
        pid: u32,
        file: &Path,
        model: &ProjectModel,
        cancel: &Cancel,
    ) -> Result<()> {
        crate::profile::capture_of(&self.env, pid, file, model, cancel)
    }

    fn heap_dump(
        &self,
        path: &Path,
        model: &ProjectModel,
        cancel: &Cancel,
        progress: &mut dyn FnMut(lang_api::HeapProgress),
    ) -> Result<lang_api::HeapSnapshot> {
        crate::profile::read_heap_dump(path, model, cancel, progress)
    }

    fn launch_points(&self, text: &str, file: &Path, model: &ProjectModel) -> Vec<LaunchPoint> {
        let Ok(jdk) = self.jdk(model) else {
            return Vec::new();
        };
        let Some(module) = Self::module_of(model, file) else {
            return Vec::new();
        };
        let mut points = Vec::new();
        for entry in parse_entry_points(text, file, jdk.feature) {
            let Ok(config) = Self::configuration(module, &entry.class, entry.spring_boot) else {
                continue;
            };
            if let Some(line) = entry.type_line {
                points.push(LaunchPoint {
                    line,
                    config: config.clone(),
                });
            }
            if points.last().is_none_or(|point| point.line != entry.line) {
                points.push(LaunchPoint {
                    line: entry.line,
                    config,
                });
            }
        }
        points.sort_by_key(|point| point.line);
        points
    }

    /// An application with a web server is the one the Services window opens for; a plain
    /// program is there only when the user asks the group to show every configuration.
    fn service_group(&self, config: &RunConfig) -> Option<runner::ServiceGroup> {
        let target = Self::target(config);
        if target.remote {
            return None;
        }
        if target.spring_boot {
            return Some(runner::ServiceGroup {
                id: SPRING_BOOT_GROUP,
                title: "Spring Boot",
                optional: false,
            });
        }
        (!target.main_class.trim().is_empty()).then_some(runner::ServiceGroup {
            id: APPLICATION_GROUP,
            title: "Application",
            optional: true,
        })
    }

    fn fields(&self, config: &RunConfig) -> Vec<ConfigField> {
        let target = Self::target(config);
        if target.remote {
            return vec![
                ConfigField::text(FIELD_HOST, "Host")
                    .hint("The machine of the JVM that runs the debug agent"),
                ConfigField::text(FIELD_PORT, "Port")
                    .hint("The port of the debug agent, 5005 by default"),
                ConfigField::read_only(FIELD_AGENT, "Debugger command")
                    .hint("Start the remote JVM with this option"),
            ];
        }
        let mut fields = vec![
            ConfigField::text(FIELD_MAIN_CLASS, "Main class")
                .hint("The fully qualified name of the class with the main method"),
            ConfigField::text(FIELD_MODULE, "Module").hint("The module of the project"),
            ConfigField::arguments(FIELD_PROGRAM_ARGS, "Program arguments")
                .hint("Quotes keep a space inside one argument"),
            ConfigField::arguments(FIELD_VM_OPTIONS, "VM options")
                .hint("Options of the JVM, such as -Xmx512m"),
            ConfigField::env(FIELD_ENV, "Environment variables").hint("A=1;B=2"),
            ConfigField::path(FIELD_WORKING_DIR, "Working directory")
                .hint("The module directory if empty"),
        ];
        if target.spring_boot {
            fields.push(
                ConfigField::text(FIELD_SPRING_PROFILES, "Spring profiles")
                    .hint("Comma-separated profiles of the application"),
            );
        }
        fields
    }

    /// The live sections of a Spring Boot application this configuration starts.
    fn live_application(
        &self,
        config: &RunConfig,
        model: &ProjectModel,
    ) -> Option<Arc<dyn runner::LiveApplication>> {
        let target = Self::target(config);
        crate::live::SpringLive::of(&target, model)
            .map(|live| Arc::new(live) as Arc<dyn runner::LiveApplication>)
    }

    fn read_field(&self, config: &RunConfig, key: &str) -> String {
        let target = Self::target(config);
        match key {
            FIELD_MAIN_CLASS => target.main_class,
            FIELD_MODULE => target.module,
            FIELD_PROGRAM_ARGS => join_arguments(&target.program_args),
            FIELD_VM_OPTIONS => join_arguments(&target.jvm_args),
            FIELD_ENV => format_environment(&target.env),
            FIELD_WORKING_DIR => target
                .working_dir
                .map(|dir| dir.display().to_string())
                .unwrap_or_default(),
            FIELD_SPRING_PROFILES => target.spring_profiles.join(","),
            FIELD_HOST => Self::remote_host(&target).to_owned(),
            FIELD_PORT => Self::remote_port(&target).to_string(),
            FIELD_AGENT => Self::agent_option(Self::remote_port(&target)),
            _ => String::new(),
        }
    }

    fn write_field(&self, config: &mut RunConfig, key: &str, value: &str) {
        let mut target = Self::target(config);
        match key {
            FIELD_MAIN_CLASS => target.main_class = value.trim().to_owned(),
            FIELD_MODULE => target.module = value.trim().to_owned(),
            FIELD_PROGRAM_ARGS => target.program_args = split_arguments(value),
            FIELD_VM_OPTIONS => target.jvm_args = split_arguments(value),
            FIELD_ENV => target.env = parse_environment(value),
            FIELD_WORKING_DIR => {
                let dir = value.trim();
                target.working_dir = (!dir.is_empty()).then(|| dir.into());
            }
            FIELD_SPRING_PROFILES => {
                target.spring_profiles = value
                    .split(',')
                    .map(str::trim)
                    .filter(|profile| !profile.is_empty())
                    .map(str::to_owned)
                    .collect();
            }
            FIELD_HOST => target.host = value.trim().to_owned(),
            FIELD_PORT => {
                if let Ok(port) = value.trim().parse() {
                    target.port = port;
                }
            }
            _ => return,
        }
        if let Ok(settings) = Self::store(&target) {
            config.settings = settings;
        }
    }

    fn templates(&self) -> Vec<ConfigTemplate> {
        vec![
            ConfigTemplate {
                id: TEMPLATE_APPLICATION,
                name: "Application",
            },
            ConfigTemplate {
                id: TEMPLATE_REMOTE,
                name: "Remote JVM Debug",
            },
        ]
    }

    fn create_config(&self, template: &str, model: &ProjectModel) -> Result<RunConfig> {
        if template == TEMPLATE_REMOTE {
            let target = JavaTarget {
                remote: true,
                host: REMOTE_HOST.to_owned(),
                port: REMOTE_PORT,
                ..JavaTarget::default()
            };
            return Ok(RunConfig {
                name: "Remote JVM Debug".into(),
                provider: PROVIDER_ID.into(),
                assemble_first: false,
                settings: Self::store(&target)?,
                ..RunConfig::default()
            });
        }
        if template != TEMPLATE_APPLICATION {
            return Err(format!("{PROVIDER_ID} cannot create {template}").into());
        }
        let target = JavaTarget {
            module: model
                .modules
                .first()
                .map(|module| module.id.clone())
                .unwrap_or_default(),
            ..JavaTarget::default()
        };
        Ok(RunConfig {
            name: NEW_CONFIG_NAME.into(),
            provider: PROVIDER_ID.into(),
            assemble_first: true,
            settings: Self::store(&target)?,
            ..RunConfig::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use base::Cancel;
    use project_model::{Module, Unit};

    use super::*;

    struct NoTool;

    impl BuildSystem for NoTool {
        fn detect(&self, _root: &Path) -> bool {
            true
        }

        fn scan(&self, _root: &Path) -> Result<ProjectModel> {
            Err("not used".into())
        }

        fn sync(&self, _root: &Path, _cancel: &Cancel) -> Result<ProjectModel> {
            Err("not used".into())
        }

        fn assemble(&self, _root: &Path) -> Result<ProcessSpec> {
            Ok(ProcessSpec::new("assemble"))
        }
    }

    fn temp_dir(name: &str) -> PathBuf {
        test_support::scratch(&format!("exec-run-{name}"))
    }

    fn provider(cache: &Path) -> JavaProvider {
        let env = BuildEnv {
            jdk: Some(Jdk {
                home: "C:/jdk".into(),
                version: "17.0.1".into(),
                feature: 17,
            }),
            cache_root: Some(cache.to_owned()),
            ..BuildEnv::default()
        };
        JavaProvider::new(env, Arc::new(NoTool))
    }

    fn model(root: &Path) -> ProjectModel {
        std::fs::create_dir_all(root.join("src/demo")).unwrap();
        let main = "package demo;
            @SpringBootApplication
            public class App {
                public static void main(String[] a) {}
            }";
        std::fs::write(root.join("src/demo/App.java"), main).unwrap();
        ProjectModel {
            root: root.to_owned(),
            resolved: true,
            modules: vec![Module {
                id: "app".into(),
                dir: root.to_owned(),
                units: vec![Unit {
                    name: MAIN_UNIT.into(),
                    source_roots: vec![root.join("src")],
                    output_dirs: vec![root.join("out")],
                    ..Unit::default()
                }],
                ..Module::default()
            }],
            ..ProjectModel::default()
        }
    }

    #[test]
    fn a_main_class_becomes_a_configuration_that_builds_first() {
        let root = temp_dir("discover");
        let provider = provider(&root.join("cache"));

        let configs = provider.discover(&model(&root)).unwrap();

        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].name, "App");
        assert_eq!(configs[0].provider, PROVIDER_ID);
        assert!(configs[0].assemble_first);
        let target: JavaTarget = configs[0].settings.clone().try_into().unwrap();
        assert_eq!(target.main_class, "demo.App");
        assert!(target.spring_boot);
    }

    #[test]
    fn launch_points_are_found_by_the_same_search_as_discover() {
        let root = temp_dir("points");
        let provider = provider(&root.join("cache"));
        let model = model(&root);
        let file = root.join("src/demo/App.java");
        let text = std::fs::read_to_string(&file).unwrap();

        let discovered = provider.discover(&model).unwrap();
        let points = provider.launch_points(&text, &file, &model);

        assert_eq!(points.len(), 2, "the type and the main method");
        assert_eq!(points[0].line, 3, "the line of the type");
        assert_eq!(points[1].line, 4, "the line of main");
        assert_eq!(points[0].config, discovered[0]);
        assert_eq!(points[1].config, discovered[0]);
    }

    #[test]
    fn a_plan_builds_first_only_when_asked() {
        let root = temp_dir("plan");
        let provider = provider(&root.join("cache"));
        let model = model(&root);
        let mut config = provider.discover(&model).unwrap().remove(0);

        assert!(provider.plan(&config, &model).unwrap().before.is_some());
        config.assemble_first = false;
        assert!(provider.plan(&config, &model).unwrap().before.is_none());
    }

    #[test]
    fn a_model_without_dependencies_cannot_be_run_yet() {
        let root = temp_dir("preliminary");
        let provider = provider(&root.join("cache"));
        let mut model = model(&root);
        let config = provider.discover(&model).unwrap().remove(0);
        model.resolved = false;

        assert!(provider.plan(&config, &model).is_err());
    }

    #[test]
    fn a_profile_plan_records_to_the_cache_of_the_project() {
        let root = temp_dir("profile-plan");
        let provider = provider(&root.join("cache"));
        let model = model(&root);
        let mut config = provider.discover(&model).unwrap().remove(0);
        config.assemble_first = false;

        let profile = provider.profile_plan(&config, &model).unwrap();
        let args = arguments(&profile.plan.program);

        assert!(
            args.iter()
                .any(|argument| argument.starts_with("-XX:StartFlightRecording=filename=\"")),
            "{args:?}"
        );
        assert!(
            args.iter().any(|argument| {
                argument.contains("settings=profile") && argument.contains("dumponexit=true")
            }),
            "{args:?}"
        );
        assert!(profile.recording.starts_with(root.join("cache")));
        assert_eq!(profile.recording.extension().unwrap(), "jfr");
        assert!(profile.plan.before.is_none());
    }

    #[test]
    fn a_jdk_8_before_262_has_no_profiler_and_says_so() {
        let root = temp_dir("profile-jdk8");
        let env = BuildEnv {
            jdk: Some(Jdk {
                home: "C:/jdk8".into(),
                version: "1.8.0_252".into(),
                feature: 8,
            }),
            cache_root: Some(root.join("cache")),
            ..BuildEnv::default()
        };
        let provider = JavaProvider::new(env, Arc::new(NoTool));
        let model = model(&root);
        let config = provider.discover(&model).unwrap().remove(0);

        let error = provider.profile_plan(&config, &model).err().unwrap();

        assert!(
            error.to_string().contains("Flight Recorder"),
            "the message names what the JDK lacks: {error}"
        );
    }

    #[test]
    fn a_configuration_of_a_module_that_is_gone_says_so() {
        let root = temp_dir("gone");
        let provider = provider(&root.join("cache"));
        let mut model = model(&root);
        let config = provider.discover(&model).unwrap().remove(0);
        model.modules.clear();

        let error = provider.plan(&config, &model).err().unwrap();

        assert!(error.to_string().contains("no module app"), "{error}");
    }

    fn arguments(spec: &ProcessSpec) -> Vec<String> {
        spec.arguments()
            .iter()
            .map(|argument| argument.to_string_lossy().into_owned())
            .collect()
    }

    fn environment(spec: &ProcessSpec, name: &str) -> Option<String> {
        spec.environment()
            .iter()
            .find(|(key, _)| key.to_string_lossy() == name)
            .and_then(|(_, value)| value.clone())
            .map(|value| value.to_string_lossy().into_owned())
    }

    #[test]
    fn the_services_window_lists_an_application_apart_from_a_plain_program() {
        let root = temp_dir("groups");
        let provider = provider(&root.join("cache"));
        let model = model(&root);
        let boot = provider.discover(&model).unwrap().remove(0);

        let group = provider
            .service_group(&boot)
            .expect("a group of the window");
        assert_eq!(group.id, SPRING_BOOT_GROUP);
        assert_eq!(group.title, "Spring Boot");
        assert!(!group.optional, "an application is what the window is for");

        let plain = RunConfig {
            name: "Plain".into(),
            provider: PROVIDER_ID.into(),
            assemble_first: false,
            settings: Table::try_from(JavaTarget {
                module: "app".into(),
                main_class: "demo.Plain".into(),
                ..JavaTarget::default()
            })
            .unwrap(),
            ..RunConfig::default()
        };
        let group = provider
            .service_group(&plain)
            .expect("a group of the window");
        assert_eq!(group.id, APPLICATION_GROUP);
        assert_eq!(group.title, "Application");
        assert!(group.optional, "a plain program waits for Show all");
    }

    #[test]
    fn a_configuration_without_a_program_is_not_a_service() {
        let root = temp_dir("groups-none");
        let provider = provider(&root.join("cache"));
        let remote = RunConfig {
            name: "Remote".into(),
            provider: PROVIDER_ID.into(),
            assemble_first: false,
            settings: Table::try_from(JavaTarget {
                remote: true,
                ..JavaTarget::default()
            })
            .unwrap(),
            ..RunConfig::default()
        };
        let empty = RunConfig {
            name: "Empty".into(),
            provider: PROVIDER_ID.into(),
            assemble_first: false,
            settings: Table::try_from(JavaTarget::default()).unwrap(),
            ..RunConfig::default()
        };

        assert!(provider.service_group(&remote).is_none());
        assert!(provider.service_group(&empty).is_none());
    }

    #[test]
    fn the_dialog_fields_read_and_write_the_settings() {
        let root = temp_dir("fields");
        let provider = provider(&root.join("cache"));
        let model = model(&root);
        let mut config = provider.discover(&model).unwrap().remove(0);

        let fields = provider.fields(&config);
        let keys: Vec<&str> = fields.iter().map(|field| field.key).collect();
        assert_eq!(
            keys,
            vec![
                FIELD_MAIN_CLASS,
                FIELD_MODULE,
                FIELD_PROGRAM_ARGS,
                FIELD_VM_OPTIONS,
                FIELD_ENV,
                FIELD_WORKING_DIR,
                FIELD_SPRING_PROFILES,
            ],
            "a Spring Boot program shows the profiles too"
        );
        assert_eq!(provider.read_field(&config, FIELD_MAIN_CLASS), "demo.App");
        assert_eq!(provider.read_field(&config, FIELD_MODULE), "app");

        provider.write_field(&mut config, FIELD_PROGRAM_ARGS, "one \"two three\"");
        provider.write_field(&mut config, FIELD_VM_OPTIONS, "-Xmx64m");
        provider.write_field(&mut config, FIELD_ENV, "DEMO=1;B=a=b");
        provider.write_field(&mut config, FIELD_WORKING_DIR, "run");
        provider.write_field(&mut config, FIELD_SPRING_PROFILES, "dev, test");

        assert_eq!(
            provider.read_field(&config, FIELD_PROGRAM_ARGS),
            "one \"two three\""
        );
        assert_eq!(provider.read_field(&config, FIELD_ENV), "B=a=b;DEMO=1");
        assert_eq!(provider.read_field(&config, FIELD_WORKING_DIR), "run");
        assert_eq!(
            provider.read_field(&config, FIELD_SPRING_PROFILES),
            "dev,test"
        );
        let target: JavaTarget = config.settings.clone().try_into().unwrap();
        assert_eq!(target.program_args, vec!["one", "two three"]);
        assert_eq!(target.jvm_args, vec!["-Xmx64m"]);
        assert_eq!(target.env.get("B").map(String::as_str), Some("a=b"));
        assert_eq!(target.spring_profiles, vec!["dev", "test"]);

        provider.write_field(&mut config, FIELD_WORKING_DIR, " ");
        assert_eq!(provider.read_field(&config, FIELD_WORKING_DIR), "");
    }

    #[test]
    fn a_plain_program_has_no_profile_field() {
        let config = RunConfig {
            name: "Plain".into(),
            provider: PROVIDER_ID.into(),
            assemble_first: false,
            settings: Table::try_from(JavaTarget::default()).unwrap(),
            ..RunConfig::default()
        };
        let provider = provider(&temp_dir("plain-program"));

        let fields = provider.fields(&config);

        assert!(
            fields
                .iter()
                .all(|field| field.key != FIELD_SPRING_PROFILES)
        );
    }

    #[test]
    fn the_settings_reach_the_command_of_the_plan_both_ways() {
        let root = temp_dir("plan-fields");
        let provider = provider(&root.join("cache"));
        let model = model(&root);
        let mut config = provider.discover(&model).unwrap().remove(0);
        provider.write_field(&mut config, FIELD_VM_OPTIONS, "-Xmx64m \"-Dname=my app\"");
        provider.write_field(&mut config, FIELD_PROGRAM_ARGS, "one \"two three\"");
        provider.write_field(&mut config, FIELD_ENV, "DEMO=1;SPACED=a b");
        provider.write_field(&mut config, FIELD_WORKING_DIR, "run");
        provider.write_field(&mut config, FIELD_SPRING_PROFILES, "dev, test");

        let cwd = root.join("run");
        for plan in [
            provider.plan(&config, &model).unwrap(),
            provider.debug_plan(&config, &model).unwrap(),
        ] {
            let args = arguments(&plan.program);
            assert!(
                args.windows(2)
                    .any(|pair| pair[0] == "-Xmx64m" && pair[1] == "-Dname=my app"),
                "{args:?}"
            );
            assert!(
                args.iter()
                    .any(|arg| arg == "-Dspring.profiles.active=dev,test"),
                "{args:?}"
            );
            let main = args
                .iter()
                .position(|arg| arg == "demo.App")
                .expect("the main class is in the command");
            assert_eq!(
                args[main + 1..].to_vec(),
                vec!["one".to_owned(), "two three".to_owned()]
            );
            assert_eq!(plan.program.working_dir(), Some(cwd.as_path()));
            assert_eq!(environment(&plan.program, "DEMO").as_deref(), Some("1"));
            assert_eq!(environment(&plan.program, "SPACED").as_deref(), Some("a b"));
        }

        let debug = provider.debug_plan(&config, &model).unwrap();
        assert!(
            arguments(&debug.program)
                .iter()
                .any(|arg| arg.starts_with("-agentlib:jdwp")),
            "the debug plan starts with the agent"
        );
    }

    #[test]
    fn the_add_button_offers_an_application() {
        let root = temp_dir("template");
        let provider = provider(&root.join("cache"));
        let model = model(&root);

        let templates = provider.templates();
        assert_eq!(templates.len(), 2);
        assert_eq!(templates[0].name, "Application");
        assert_eq!(templates[1].name, "Remote JVM Debug");
        let config = provider.create_config(templates[0].id, &model).unwrap();

        assert_eq!(config.name, NEW_CONFIG_NAME);
        assert_eq!(config.provider, PROVIDER_ID);
        assert!(config.assemble_first);
        let target: JavaTarget = config.settings.clone().try_into().unwrap();
        assert_eq!(target.module, "app");
        assert_eq!(target.main_class, "");
        assert!(provider.create_config("unknown", &model).is_err());
    }
}

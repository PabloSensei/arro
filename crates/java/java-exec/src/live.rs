//! The live sections of a Spring Boot application: the port the console names, the
//! readiness line, and the JSON of the management endpoints. The window knows nothing
//! about the framework; this module reads all of it.

use std::path::PathBuf;
use std::sync::Mutex;

use project_model::{Dependency, Module, ProjectModel};
use runner::{
    LiveApplication, LiveEndpoint, LiveNote, LiveRequest, LiveRow, LiveSection, LiveTable,
    LiveTarget, LiveTone,
};
use serde_json::Value;

use crate::provider::JavaTarget;

/// The default of the management base path.
const BASE_PATH: &str = "/actuator";
/// The default port of a development server.
const DEFAULT_PORT: u16 = 8080;
/// The path of every section after the base path.
const SECTION_NAMES: [&str; 4] = ["health", "beans", "mappings", "env"];
/// The artifact that publishes the management endpoints.
const ACTUATOR: &str = "spring-boot-starter-actuator";

/// The settings of one application that shape its live endpoint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpringSettings {
    pub server_port: Option<u16>,
    pub management_port: Option<u16>,
    pub base_path: String,
}

impl SpringSettings {
    /// The base path with a leading slash and without a trailing one.
    fn normalized_base(&self) -> String {
        let path = self.base_path.trim();
        let path = if path.is_empty() { BASE_PATH } else { path };
        let with_slash = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        with_slash.trim_end_matches('/').to_owned()
    }

    /// Reads `server.port`, `management.server.port` and the base path from the pairs of
    /// a properties file or of the command line.
    fn apply(&mut self, key: &str, value: &str) {
        match key {
            "server.port" => self.server_port = value.trim().parse().ok(),
            "management.server.port" => self.management_port = value.trim().parse().ok(),
            "management.endpoints.web.base-path" if !value.trim().is_empty() => {
                self.base_path = value.trim().to_owned();
            }
            _ => {}
        }
    }
}

/// What the console said about the running process.
#[derive(Debug, Default)]
struct Observed {
    app_port: Option<u16>,
    started: bool,
}

/// The live sections of one Spring Boot configuration.
pub struct SpringLive {
    settings: SpringSettings,
    /// `Some` when the build file of the module does not bring the endpoints.
    missing: Option<LiveNote>,
    settings_file: Option<PathBuf>,
    observed: Mutex<Observed>,
}

impl SpringLive {
    /// The application of `target`, or `None` when the configuration starts no Spring Boot
    /// program.
    pub fn of(target: &JavaTarget, model: &ProjectModel) -> Option<Self> {
        if !target.spring_boot {
            return None;
        }
        let module = model.module(&target.module)?;
        let (settings, settings_file) = read_settings(module, target);
        let missing = model
            .resolved
            .then(|| !has_actuator(model, module))
            .filter(|missing| *missing)
            .map(|_| LiveNote {
                text: format!(
                    "The project does not depend on {ACTUATOR}. Add {ACTUATOR} to its build file to read the live sections."
                ),
                file: None,
                label: None,
            });
        Some(Self {
            settings,
            missing,
            settings_file,
            observed: Mutex::new(Observed::default()),
        })
    }

    fn observed(&self) -> std::sync::MutexGuard<'_, Observed> {
        self.observed
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
    }

    /// The port the management endpoints listen on: their own port when the settings name
    /// one, the port of the application otherwise.
    fn management_port(&self) -> u16 {
        self.settings
            .management_port
            .or(self.observed().app_port)
            .or(self.settings.server_port)
            .unwrap_or(DEFAULT_PORT)
    }

    fn application_port(&self) -> u16 {
        self.observed()
            .app_port
            .or(self.settings.server_port)
            .unwrap_or(DEFAULT_PORT)
    }

    fn note(&self, text: String) -> LiveNote {
        LiveNote {
            text,
            file: self.settings_file.clone(),
            label: self
                .settings_file
                .as_ref()
                .map(|_| "Open application.properties".to_owned()),
        }
    }
}

impl LiveApplication for SpringLive {
    fn title(&self) -> String {
        "Actuator".to_owned()
    }

    fn observe(&self, line: &str) -> bool {
        let mut observed = self.observed();
        let before = (observed.app_port, observed.started);
        if let Some(port) = server_port(line) {
            observed.app_port = Some(port);
        }
        if started_line(line) {
            observed.started = true;
        }
        before != (observed.app_port, observed.started)
    }

    fn started(&self) -> bool {
        self.observed().started
    }

    fn endpoint(&self) -> Option<LiveEndpoint> {
        Some(LiveEndpoint {
            host: "127.0.0.1".to_owned(),
            port: self.management_port(),
        })
    }

    fn section_path(&self, section: LiveSection) -> Option<String> {
        let name = match section {
            LiveSection::Health => SECTION_NAMES[0],
            LiveSection::Beans => SECTION_NAMES[1],
            LiveSection::Mappings => SECTION_NAMES[2],
            LiveSection::Environment => SECTION_NAMES[3],
        };
        Some(format!("{}/{name}", self.settings.normalized_base()))
    }

    fn app_url(&self, path: &str) -> Option<String> {
        let path = if path.starts_with('/') {
            path.to_owned()
        } else {
            format!("/{path}")
        };
        Some(format!(
            "http://127.0.0.1:{}{path}",
            self.application_port()
        ))
    }

    fn missing(&self) -> Option<LiveNote> {
        self.missing.clone()
    }

    fn not_exposed(&self) -> LiveNote {
        self.note(
            "The endpoint is not exposed. Add it to management.endpoints.web.exposure.include in application.properties."
                .to_owned(),
        )
    }

    fn read(&self, section: LiveSection, body: &str) -> Result<LiveTable, String> {
        match section {
            LiveSection::Health => parse_health(body),
            LiveSection::Beans => parse_beans(body),
            LiveSection::Mappings => parse_mappings(body),
            LiveSection::Environment => parse_environment(body),
        }
    }
}

/// Whether the module brings the management endpoints, directly or through a starter.
fn has_actuator(model: &ProjectModel, module: &Module) -> bool {
    let Some(dependencies) = model.dependencies_of(&module.id) else {
        return false;
    };
    dependencies
        .scopes
        .iter()
        .flat_map(|scope| scope.roots.iter())
        .any(has_actuator_dependency)
}

fn has_actuator_dependency(dependency: &Dependency) -> bool {
    (dependency.coordinates.contains(ACTUATOR)
        || dependency.coordinates.contains("spring-boot-actuator"))
        || dependency.children.iter().any(has_actuator_dependency)
}

/// The settings of the module and the file that holds them, the file the hint opens.
fn read_settings(module: &Module, target: &JavaTarget) -> (SpringSettings, Option<PathBuf>) {
    let resources = module.dir.join("src").join("main").join("resources");
    let candidates = [
        resources.join("application.properties"),
        resources.join("application.yml"),
        resources.join("application.yaml"),
        module.dir.join("application.properties"),
        module.dir.join("application.yml"),
    ];
    let mut settings = SpringSettings {
        base_path: BASE_PATH.to_owned(),
        ..SpringSettings::default()
    };
    for path in &candidates {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        if path
            .extension()
            .is_some_and(|extension| extension == "yml" || extension == "yaml")
        {
            parse_yaml_settings(&text, &mut settings);
        } else {
            parse_properties_settings(&text, &mut settings);
        }
    }
    for argument in target.jvm_args.iter().chain(&target.program_args) {
        if let Some(option) = argument.strip_prefix("-D")
            && let Some((key, value)) = option.split_once('=')
        {
            settings.apply(key, value);
        }
        if let Some(option) = argument.strip_prefix("--")
            && let Some((key, value)) = option.split_once('=')
        {
            settings.apply(key, value);
        }
    }
    let file = candidates
        .iter()
        .find(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == "properties")
        })
        .or_else(|| candidates.iter().find(|path| path.is_file()))
        .cloned()
        .or_else(|| Some(candidates[0].clone()));
    (settings, file)
}

fn parse_properties_settings(text: &str, settings: &mut SpringSettings) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let split = line.find(['=', ':']).map(|at| (at, 1));
        let Some((at, width)) = split else {
            continue;
        };
        let key = line[..at].trim();
        let value = line[at + width..].trim();
        settings.apply(key, value);
    }
}

/// A flat reader of the YAML keys this module needs: the nesting of plain `key:` lines is
/// kept, list items are skipped. Profiles and anchors are left to the application.
fn parse_yaml_settings(text: &str, settings: &mut SpringSettings) {
    let mut stack: Vec<(usize, String)> = Vec::new();
    for line in text.lines() {
        let without_comment = line.split('#').next().unwrap_or(line);
        let trimmed = without_comment.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('-') {
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();
        while stack.last().is_some_and(|(level, _)| *level >= indent) {
            stack.pop();
        }
        let Some((key, value)) = trimmed.trim_start().split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        let full = stack
            .iter()
            .map(|(_, key)| key.as_str())
            .chain(std::iter::once(key))
            .collect::<Vec<_>>()
            .join(".");
        if value.is_empty() {
            stack.push((indent, key.to_owned()));
        } else {
            settings.apply(&full, value.trim_matches(['"', '\'']));
        }
    }
}

/// The port a line of the console names, when it names one. The three servers of Spring
/// Boot print it in their own words, and versions before 2.3 put a colon and brackets
/// around it.
pub fn server_port(line: &str) -> Option<u16> {
    for server in ["Tomcat", "Netty", "Undertow", "Jetty"] {
        let Some(rest) = line.split_once(server).map(|(_, rest)| rest) else {
            continue;
        };
        let Some(at) = rest.find("started on port") else {
            continue;
        };
        let rest = &rest[at + "started on port".len()..];
        let rest = rest.trim_start_matches('(').trim_start_matches('s');
        let rest = rest.trim_start_matches(')').trim_start_matches(':');
        let digits: String = rest
            .trim_start()
            .chars()
            .take_while(char::is_ascii_digit)
            .collect();
        if let Ok(port) = digits.parse() {
            return Some(port);
        }
    }
    None
}

/// Whether the application said it started, as `Started App in 1.2 seconds` does.
pub fn started_line(line: &str) -> bool {
    let line = line.trim();
    let Some(rest) = line.strip_prefix("Started ") else {
        return false;
    };
    let Some((_, tail)) = rest.split_once(" in ") else {
        return false;
    };
    let Some((_, after)) = tail.split_once(' ') else {
        return false;
    };
    after.starts_with("second")
}

fn parse_json(body: &str) -> Result<Value, String> {
    serde_json::from_str(body).map_err(|error| format!("The answer is not JSON: {error}"))
}

fn tone_of(status: &str) -> LiveTone {
    match status {
        "UP" => LiveTone::Good,
        "DOWN" | "OUT_OF_SERVICE" => LiveTone::Bad,
        "" => LiveTone::Muted,
        _ => LiveTone::Normal,
    }
}

/// The health of the application and its components, as a tree.
pub fn parse_health(body: &str) -> Result<LiveTable, String> {
    let value = parse_json(body)?;
    let mut table = LiveTable::new(&["Status", "Component"]);
    let status = value["status"].as_str().unwrap_or_default();
    table.push(LiveRow {
        cells: vec![status.to_owned(), String::new()],
        tone: tone_of(status),
        ..LiveRow::default()
    });
    if let Some(components) = value.get("components").and_then(Value::as_object) {
        push_health_components(components, 1, &mut table);
    }
    Ok(table)
}

fn push_health_components(
    components: &serde_json::Map<String, Value>,
    depth: usize,
    table: &mut LiveTable,
) {
    let mut names: Vec<&String> = components.keys().collect();
    names.sort();
    for name in names {
        let component = &components[name];
        let status = component["status"].as_str().unwrap_or_default();
        table.push(LiveRow {
            cells: vec![status.to_owned(), name.clone()],
            depth,
            tone: tone_of(status),
            ..LiveRow::default()
        });
        if let Some(nested) = component.get("components").and_then(Value::as_object) {
            push_health_components(nested, depth + 1, table);
        }
    }
}

/// The beans of every context, Boot 2 and Boot 3 share the shape of the answer.
pub fn parse_beans(body: &str) -> Result<LiveTable, String> {
    let value = parse_json(body)?;
    let mut beans = Vec::new();
    if let Some(contexts) = value.get("contexts").and_then(Value::as_object) {
        for context in contexts.values() {
            collect_beans(context, &mut beans);
        }
    } else {
        collect_beans(&value, &mut beans);
    }
    beans.sort_by(|left: &LiveRow, right| left.cells[0].cmp(&right.cells[0]));
    let mut table = LiveTable::new(&["Bean", "Type", "Scope", "Dependencies"]);
    table.rows = beans;
    Ok(table)
}

fn collect_beans(context: &Value, beans: &mut Vec<LiveRow>) {
    let Some(found) = context.get("beans").and_then(Value::as_object) else {
        return;
    };
    for (name, bean) in found {
        let type_name = bean["type"].as_str().unwrap_or_default();
        let scope = bean["scope"].as_str().unwrap_or_default();
        let dependencies = bean["dependencies"]
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        beans.push(LiveRow {
            cells: vec![
                name.clone(),
                type_name.to_owned(),
                scope.to_owned(),
                dependencies,
            ],
            target: (!type_name.is_empty()).then(|| LiveTarget::Class(type_name.to_owned())),
            ..LiveRow::default()
        });
    }
}

/// The handlers of the web contexts. Boot 2 names their map `dispatcherServlets`, Boot 3
/// `dispatcherHandlers`; both hold a list of predicates and handlers.
pub fn parse_mappings(body: &str) -> Result<LiveTable, String> {
    let value = parse_json(body)?;
    let mut rows = Vec::new();
    collect_mappings(&value, &mut rows);
    let mut table = LiveTable::new(&["Method", "Path", "Handler"]);
    table.rows = rows;
    Ok(table)
}

fn collect_mappings(value: &Value, rows: &mut Vec<LiveRow>) {
    let Value::Object(fields) = value else {
        return;
    };
    for (key, child) in fields {
        if key == "dispatcherServlets" || key == "dispatcherHandlers" || key == "webHandler" {
            collect_dispatchers(child, rows);
            continue;
        }
        if key == "beans" || key == "predicate" || key == "handler" {
            continue;
        }
        collect_mappings(child, rows);
    }
}

fn collect_dispatchers(value: &Value, rows: &mut Vec<LiveRow>) {
    match value {
        Value::Object(dispatchers) => {
            for child in dispatchers.values() {
                collect_dispatchers(child, rows);
            }
        }
        Value::Array(items) => {
            for item in items {
                push_mapping(item, rows);
            }
        }
        _ => {}
    }
}

fn push_mapping(item: &Value, rows: &mut Vec<LiveRow>) {
    let predicate = item["predicate"].as_str().unwrap_or_default();
    let mut methods = predicate_methods(predicate);
    let mut paths = predicate_paths(predicate);
    if let Some(conditions) = item.pointer("/details/requestMappingConditions") {
        if let Some(found) = conditions["methods"].as_array() {
            let listed: Vec<String> = found
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if !listed.is_empty() {
                methods = listed;
            }
        }
        if let Some(found) = conditions["patterns"].as_array() {
            let listed: Vec<String> = found
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect();
            if !listed.is_empty() {
                paths = listed;
            }
        }
    }
    let handler = item["handler"]
        .as_str()
        .map(str::to_owned)
        .or_else(|| item["handler"]["className"].as_str().map(str::to_owned))
        .unwrap_or_default();
    let (class, method) = split_handler(&handler);
    if methods.is_empty() {
        methods.push("ANY".to_owned());
    }
    if paths.is_empty() {
        paths.push(String::new());
    }
    for http_method in &methods {
        for path in &paths {
            let target = if class.is_empty() {
                None
            } else if let Some(method) = &method {
                Some(LiveTarget::Method {
                    class: class.clone(),
                    method: method.clone(),
                })
            } else {
                Some(LiveTarget::Class(class.clone()))
            };
            rows.push(LiveRow {
                cells: vec![http_method.clone(), path.clone(), handler.clone()],
                target,
                request: Some(LiveRequest {
                    method: if http_method == "ANY" {
                        "GET".to_owned()
                    } else {
                        http_method.clone()
                    },
                    path: path.clone(),
                }),
                ..LiveRow::default()
            });
        }
    }
}

/// The methods of a predicate such as `{GET [/owners], produces [application/json]}`.
fn predicate_methods(predicate: &str) -> Vec<String> {
    let Some(open) = predicate.find('{') else {
        return Vec::new();
    };
    let rest = &predicate[open + 1..];
    let before_bracket = rest.split('[').next().unwrap_or(rest);
    before_bracket
        .split(['|', ','])
        .map(str::trim)
        .filter(|word| !word.is_empty())
        .filter(|word| {
            matches!(
                *word,
                "GET" | "POST" | "PUT" | "DELETE" | "PATCH" | "HEAD" | "OPTIONS" | "TRACE"
            )
        })
        .map(str::to_owned)
        .collect()
}

/// The paths of a predicate, without the surrounding brackets and quotes.
fn predicate_paths(predicate: &str) -> Vec<String> {
    let Some(open) = predicate.find('[') else {
        return Vec::new();
    };
    let Some(close) = predicate[open + 1..].find(']') else {
        return Vec::new();
    };
    predicate[open + 1..open + 1 + close]
        .split(['|', ','])
        .map(|path| path.trim().trim_matches(['"', '\'']).trim())
        .filter(|path| path.starts_with('/'))
        .map(str::to_owned)
        .collect()
}

/// The class and the method of `com.example.Controller#handler(java.lang.String)`.
fn split_handler(handler: &str) -> (String, Option<String>) {
    let Some((class, rest)) = handler.split_once('#') else {
        return (handler.trim().to_owned(), None);
    };
    let method = rest
        .split(['(', ' '])
        .next()
        .unwrap_or_default()
        .trim()
        .to_owned();
    (
        class.trim().to_owned(),
        (!method.is_empty()).then_some(method),
    )
}

/// The property sources with their properties. A value the application masks stays as it
/// came, `******`.
pub fn parse_environment(body: &str) -> Result<LiveTable, String> {
    let value = parse_json(body)?;
    let mut table = LiveTable::new(&["Source", "Property", "Value"]);
    if let Some(sources) = value["propertySources"].as_array() {
        for source in sources {
            let name = source["name"].as_str().unwrap_or_default();
            table.push(LiveRow {
                cells: vec![name.to_owned(), String::new(), String::new()],
                group: true,
                ..LiveRow::default()
            });
            let Some(properties) = source["properties"].as_object() else {
                continue;
            };
            let mut keys: Vec<&String> = properties.keys().collect();
            keys.sort();
            for key in keys {
                let property = &properties[key];
                let value = match &property["value"] {
                    Value::String(text) => text.clone(),
                    Value::Null => String::new(),
                    other => other.to_string(),
                };
                table.push(LiveRow {
                    cells: vec![String::new(), key.clone(), value],
                    depth: 1,
                    ..LiveRow::default()
                });
            }
        }
    }
    Ok(table)
}

#[cfg(test)]
mod tests {
    use super::*;
    use project_model::{Dependency, DependencyScope, Module, ModuleDependencies, ProjectModel};
    use std::path::Path;

    fn target(spring_boot: bool) -> JavaTarget {
        JavaTarget {
            module: "app".to_owned(),
            main_class: "demo.Application".to_owned(),
            spring_boot,
            ..JavaTarget::default()
        }
    }

    fn model(root: &Path, resolved: bool) -> ProjectModel {
        ProjectModel {
            root: root.to_path_buf(),
            tool: "maven".to_owned(),
            modules: vec![Module {
                id: "app".to_owned(),
                name: "app".to_owned(),
                dir: root.to_path_buf(),
                ..Module::default()
            }],
            resolved,
            ..ProjectModel::default()
        }
    }

    #[test]
    fn the_three_servers_name_their_port() {
        assert_eq!(
            server_port(
                "2026-01-02T03:04:05.000  INFO 1 --- [main] o.s.b.w.embedded.tomcat.TomcatWebServer  : Tomcat started on port 8080 (http) with context path ''"
            ),
            Some(8080)
        );
        assert_eq!(server_port("Netty started on port 8081 (http)"), Some(8081));
        assert_eq!(
            server_port("Undertow started on port 9090 (http)"),
            Some(9090)
        );
        assert_eq!(
            server_port("Tomcat started on port(s): 8070 (http) with context path ''"),
            Some(8070)
        );
        assert_eq!(
            server_port("Tomcat initialized with port(s): 8080 (http)"),
            None
        );
        assert_eq!(server_port("Started App in 1.2 seconds"), None);
    }

    #[test]
    fn the_readiness_line_is_started_and_a_duration() {
        assert!(started_line(
            "Started PetClinicApplication in 1.234 seconds (process running for 1.5)"
        ));
        assert!(started_line("  Started App in 1.2 seconds"));
        assert!(!started_line("Started App"));
        assert!(!started_line("Not Started App in 1.2 seconds"));
        assert!(!started_line("Started App in seconds"));
    }

    #[test]
    fn health_is_a_tree_of_components_with_their_colour() {
        let table = parse_health(include_str!("../tests/data/actuator/health.json")).unwrap();
        assert_eq!(table.columns, vec!["Status", "Component"]);
        assert_eq!(table.rows[0].cells, vec!["UP", ""]);
        assert_eq!(table.rows[0].tone, LiveTone::Good);
        let names: Vec<&str> = table.rows.iter().map(|row| row.cells[1].as_str()).collect();
        assert_eq!(names, vec!["", "db", "diskSpace", "ping"]);
        assert_eq!(table.rows[1].depth, 1);
    }

    #[test]
    fn beans_of_boot_two_and_three_are_the_same_list() {
        for body in [
            include_str!("../tests/data/actuator/beans-boot2.json"),
            include_str!("../tests/data/actuator/beans-boot3.json"),
        ] {
            let table = parse_beans(body).unwrap();
            assert_eq!(table.columns, vec!["Bean", "Type", "Scope", "Dependencies"]);
            assert_eq!(table.rows.len(), 2);
            assert_eq!(table.rows[0].cells[0], "ownerController");
            assert_eq!(table.rows[0].cells[2], "singleton");
            assert_eq!(
                table.rows[0].target,
                Some(LiveTarget::Class(
                    "org.springframework.samples.petclinic.owner.OwnerController".to_owned()
                ))
            );
        }
        let boot2 = parse_beans(include_str!("../tests/data/actuator/beans-boot2.json")).unwrap();
        assert_eq!(boot2.rows[0].cells[3], "ownerRepository, petRepository");
    }

    #[test]
    fn mappings_of_boot_two_and_three_both_name_the_handler() {
        for body in [
            include_str!("../tests/data/actuator/mappings-boot2.json"),
            include_str!("../tests/data/actuator/mappings-boot3.json"),
        ] {
            let table = parse_mappings(body).unwrap();
            assert_eq!(table.columns, vec!["Method", "Path", "Handler"]);
            assert!(table.rows.iter().any(|row| row.cells[0] == "GET"));
            assert!(table.rows.iter().all(|row| row.cells[1].starts_with('/')));
            assert!(table.rows.iter().all(|row| row.request.is_some()));
        }
        let boot2 =
            parse_mappings(include_str!("../tests/data/actuator/mappings-boot2.json")).unwrap();
        assert_eq!(boot2.rows[0].cells[1], "/owners");
        assert_eq!(
            boot2.rows[0].target,
            Some(LiveTarget::Method {
                class: "org.springframework.samples.petclinic.owner.OwnerController".to_owned(),
                method: "findAll".to_owned(),
            })
        );
        let boot3 =
            parse_mappings(include_str!("../tests/data/actuator/mappings-boot3.json")).unwrap();
        assert_eq!(boot3.rows[0].cells[1], "/owners/{ownerId}");
        assert_eq!(
            boot3.rows[0].target,
            Some(LiveTarget::Method {
                class: "org.springframework.samples.petclinic.owner.OwnerController".to_owned(),
                method: "showOwner".to_owned(),
            })
        );
        let web_handler = parse_mappings(
            r#"{"contexts":{"application":{"mappings":{"webHandler":[{"predicate":"{GET [/actuator/health]}","handler":"org.springframework.boot.actuate.endpoint.web.WebMvcEndpointHandlerMapping$OperationHandler#handle(javax.servlet.http.HttpServletRequest)"}]}}}}"#,
        )
        .unwrap();
        assert_eq!(web_handler.rows[0].cells[1], "/actuator/health");
    }

    #[test]
    fn environment_keeps_the_sources_and_the_masked_values() {
        let table = parse_environment(include_str!("../tests/data/actuator/env.json")).unwrap();
        assert_eq!(table.columns, vec!["Source", "Property", "Value"]);
        assert!(table.rows[0].group);
        assert_eq!(table.rows[0].cells[0], "server.ports");
        let masked = table
            .rows
            .iter()
            .find(|row| row.cells[1] == "spring.datasource.password")
            .unwrap();
        assert_eq!(masked.cells[2], "******");
        assert_eq!(masked.depth, 1);
    }

    #[test]
    fn a_module_without_the_starter_keeps_the_tab_and_explains_the_dependency() {
        let root = test_support::scratch("java-exec-live-dependency");
        let mut model = model(&root, true);
        model.dependencies = vec![ModuleDependencies {
            module: "app".to_owned(),
            build_file: None,
            scopes: vec![DependencyScope {
                name: "runtime".to_owned(),
                roots: Vec::new(),
            }],
        }];
        let live = SpringLive::of(&target(true), &model).unwrap();
        let note = live.missing().expect("the dependency hint");
        assert!(
            note.text.contains("spring-boot-starter-actuator"),
            "{}",
            note.text
        );

        model.dependencies[0].scopes[0].roots.push(Dependency {
            coordinates: "org.springframework.boot:spring-boot-starter-actuator:3.5.0".to_owned(),
            ..Dependency::default()
        });
        let live = SpringLive::of(&target(true), &model).unwrap();
        assert!(live.missing().is_none());
    }

    #[test]
    fn a_configuration_that_is_not_spring_boot_has_no_live_tab() {
        let root = test_support::scratch("java-exec-live");
        assert!(SpringLive::of(&target(false), &model(&root, false)).is_none());
        assert!(SpringLive::of(&target(true), &model(&root, false)).is_some());
    }

    #[test]
    fn the_settings_file_names_the_ports_and_the_base_path() {
        let root = test_support::scratch("java-exec-live-settings");
        let resources = root.join("src/main/resources");
        std::fs::create_dir_all(&resources).unwrap();
        std::fs::write(
            resources.join("application.properties"),
            "server.port=9090\nmanagement.server.port=9091\nmanagement.endpoints.web.base-path=/manage\n",
        )
        .unwrap();
        let live = SpringLive::of(&target(true), &model(&root, false)).unwrap();
        assert_eq!(live.settings.server_port, Some(9090));
        assert_eq!(live.settings.management_port, Some(9091));
        assert_eq!(
            live.section_path(LiveSection::Environment).as_deref(),
            Some("/manage/env")
        );
        assert_eq!(
            live.app_url("/owners").as_deref(),
            Some("http://127.0.0.1:9090/owners")
        );
    }

    #[test]
    fn the_console_port_wins_over_the_settings() {
        let root = test_support::scratch("java-exec-live-console");
        let live = SpringLive::of(&target(true), &model(&root, false)).unwrap();
        assert!(!live.started());
        assert_eq!(
            live.endpoint(),
            Some(LiveEndpoint {
                host: "127.0.0.1".to_owned(),
                port: 8080
            })
        );
        assert!(live.observe("Tomcat started on port 8085 (http)"));
        assert_eq!(live.endpoint().map(|endpoint| endpoint.port), Some(8085));
        assert!(!live.started());
        assert!(live.observe("Started App in 1.2 seconds"));
        assert!(live.started());
    }

    #[test]
    fn yaml_settings_are_read_through_their_nesting() {
        let mut settings = SpringSettings::default();
        parse_yaml_settings(
            "server:\n  port: 9095\nmanagement:\n  server:\n    port: 9096\n  endpoints:\n    web:\n      base-path: /admin\n",
            &mut settings,
        );
        assert_eq!(settings.server_port, Some(9095));
        assert_eq!(settings.management_port, Some(9096));
        assert_eq!(settings.normalized_base(), "/admin");
    }
}

//! What the Database window needs from a Java project: the address templates of the
//! drivers, the drivers of the dependency classpath, the data sources of the project files
//! and the way to start the helper in the JVM of the project.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use database::{Driver, DriverKind, Launch, Suggestion, Support, VendorTemplate};
use project_model::{BuildSystem, Module, ProjectModel, open_project};

use crate::BuildEnv;
use crate::spring;

/// How often the support looks at the model the build tool is reading.
const MODEL_POLL: Duration = Duration::from_millis(500);

/// The keys of a Spring data source, as a configuration file spells them.
const URL: &str = "spring.datasource.url";
const USER: &str = "spring.datasource.username";
const DRIVER_CLASS: &str = "spring.datasource.driver-class-name";

/// The Java side of the Database window. It owns the model session, so the drivers and
/// the suggested sources follow the project without waiting for the window to ask.
pub struct DatabaseSupport {
    root: PathBuf,
    cache_root: PathBuf,
    model: Arc<Mutex<Option<Arc<ProjectModel>>>>,
}

impl DatabaseSupport {
    pub fn new(root: PathBuf, tool: Option<Arc<dyn BuildSystem>>, cache_root: PathBuf) -> Self {
        let model = Arc::new(Mutex::new(None));
        if let Some(tool) = tool {
            let slot = model.clone();
            let project = root.clone();
            let (session, mut events) = open_project(tool, project, &cache_root);
            std::thread::Builder::new()
                .name("database-model".into())
                .spawn(move || {
                    loop {
                        if let Some(found) = session.model()
                            && let Ok(mut slot) = slot.lock()
                        {
                            *slot = Some(found);
                        }
                        // The subscription wakes nothing by itself; the session keeps the
                        // model alive and the poll is cheap, a snapshot read.
                        let _ = events.try_next();
                        std::thread::sleep(MODEL_POLL);
                    }
                })
                .ok();
        }
        Self {
            root,
            cache_root,
            model,
        }
    }

    fn model(&self) -> Option<Arc<ProjectModel>> {
        self.model
            .lock()
            .ok()
            .and_then(|model| model.as_ref().cloned())
    }
}

/// The product of a JDBC address, from its prefix.
fn kind_of_url(url: &str) -> Option<DriverKind> {
    let lower = url.trim().to_ascii_lowercase();
    if !lower.starts_with("jdbc:") {
        return None;
    }
    let rest = &lower["jdbc:".len()..];
    let (product, _) = rest.split_once(':')?;
    match product {
        "h2" => Some(DriverKind::H2),
        "postgresql" => Some(DriverKind::PostgreSql),
        "mysql" => Some(DriverKind::MySql),
        "mariadb" => Some(DriverKind::MariaDb),
        "sqlite" => Some(DriverKind::Sqlite),
        "oracle" => Some(DriverKind::Oracle),
        "sqlserver" | "jtds" => Some(DriverKind::SqlServer),
        _ => None,
    }
}

/// The product a driver class names, for an address whose prefix says nothing.
fn kind_of_driver_class(class: &str) -> Option<DriverKind> {
    let lower = class.trim().to_ascii_lowercase();
    if lower.starts_with("org.h2.") {
        Some(DriverKind::H2)
    } else if lower.starts_with("org.postgresql.") {
        Some(DriverKind::PostgreSql)
    } else if lower.starts_with("com.mysql.") {
        Some(DriverKind::MySql)
    } else if lower.starts_with("org.mariadb.") {
        Some(DriverKind::MariaDb)
    } else if lower.starts_with("org.sqlite.") {
        Some(DriverKind::Sqlite)
    } else if lower.starts_with("oracle.jdbc.") {
        Some(DriverKind::Oracle)
    } else if lower.starts_with("com.microsoft.sqlserver.") {
        Some(DriverKind::SqlServer)
    } else {
        None
    }
}

/// The files of a project that may name a data source, in a stable order.
fn config_files(root: &Path, model: &ProjectModel) -> Vec<PathBuf> {
    let mut folders = BTreeSet::new();
    folders.insert(root.to_path_buf());
    for module in &model.modules {
        folders.insert(module.dir.clone());
        folders.insert(module.dir.join("src/main/resources"));
        folders.insert(module.dir.join("src/test/resources"));
        for unit in &module.units {
            for resource in &unit.resource_roots {
                folders.insert(resource.clone());
            }
        }
    }
    let mut files = Vec::new();
    for folder in folders {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if !lower.starts_with("application") {
                continue;
            }
            if lower.ends_with(".properties") || lower.ends_with(".yml") || lower.ends_with(".yaml")
            {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// The profile an `application-<profile>.properties` file belongs to.
fn profile_of(path: &Path) -> Option<String> {
    let name = path.file_name()?.to_str()?;
    let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
    let profile = stem.strip_prefix("application-")?;
    (!profile.is_empty()).then(|| profile.to_owned())
}

/// The last value a file gives a key: a later line overrides an earlier one, as Spring
/// reads them.
fn setting<'a>(settings: &'a [(String, String)], key: &str) -> Option<&'a str> {
    settings
        .iter()
        .rev()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// The data sources the configuration files of the project suggest, one entry per file:
/// the profiles of a project are separate connections and keep their own label. A file
/// with the address but no user still suggests its connection; the dialog fills the rest.
fn suggestions(root: &Path, model: &ProjectModel) -> Vec<Suggestion> {
    let mut suggestions = Vec::new();
    for file in config_files(root, model) {
        let Some(kind) = spring::config_kind(&file) else {
            continue;
        };
        let Ok(text) = fs::read_to_string(&file) else {
            continue;
        };
        let settings = spring::settings(&text, kind);
        let Some(url) = setting(&settings, URL) else {
            continue;
        };
        let driver_class = setting(&settings, DRIVER_CLASS);
        suggestions.push(Suggestion {
            module: module_of(&file, root, model)
                .map(|module| module.id.clone())
                .unwrap_or_default(),
            profile: profile_of(&file),
            url: url.to_owned(),
            user: setting(&settings, USER).unwrap_or_default().to_owned(),
            kind: kind_of_url(url).or_else(|| driver_class.and_then(kind_of_driver_class)),
            driver_jar: None,
        });
    }
    suggestions
}

/// The module whose folder holds `file`, when one does.
fn module_of<'a>(file: &Path, root: &Path, model: &'a ProjectModel) -> Option<&'a Module> {
    let file = file.strip_prefix(root).unwrap_or(file);
    model
        .modules
        .iter()
        .filter(|module| {
            let folder = module.dir.strip_prefix(root).unwrap_or(&module.dir);
            file.starts_with(folder)
        })
        .max_by_key(|module| module.dir.as_os_str().len())
}

impl Support for DatabaseSupport {
    fn vendors(&self) -> Vec<VendorTemplate> {
        vec![
            VendorTemplate {
                kind: DriverKind::H2,
                title: "H2".into(),
                url_template: "jdbc:h2:{file}".into(),
                host: String::new(),
                port: None,
            },
            VendorTemplate {
                kind: DriverKind::PostgreSql,
                title: "PostgreSQL".into(),
                url_template: "jdbc:postgresql://{host}:{port}/{database}".into(),
                host: "localhost".into(),
                port: Some(5432),
            },
            VendorTemplate {
                kind: DriverKind::MySql,
                title: "MySQL".into(),
                url_template: "jdbc:mysql://{host}:{port}/{database}".into(),
                host: "localhost".into(),
                port: Some(3306),
            },
            VendorTemplate {
                kind: DriverKind::MariaDb,
                title: "MariaDB".into(),
                url_template: "jdbc:mariadb://{host}:{port}/{database}".into(),
                host: "localhost".into(),
                port: Some(3306),
            },
            VendorTemplate {
                kind: DriverKind::Sqlite,
                title: "SQLite".into(),
                url_template: "jdbc:sqlite:{file}".into(),
                host: String::new(),
                port: None,
            },
            VendorTemplate {
                kind: DriverKind::Oracle,
                title: "Oracle".into(),
                url_template: "jdbc:oracle:thin:@//{host}:{port}/{database}".into(),
                host: "localhost".into(),
                port: Some(1521),
            },
            VendorTemplate {
                kind: DriverKind::SqlServer,
                title: "SQL Server".into(),
                url_template: "jdbc:sqlserver://{host}:{port};databaseName={database}".into(),
                host: "localhost".into(),
                port: Some(1433),
            },
        ]
    }

    fn drivers(&self) -> Vec<Driver> {
        self.model()
            .map(|model| java_project::drivers(&model))
            .unwrap_or_default()
    }

    fn suggestions(&self) -> Vec<Suggestion> {
        self.model()
            .map(|model| suggestions(&self.root, &model))
            .unwrap_or_default()
    }

    fn helper(&self, jars: &[PathBuf]) -> Result<Launch, String> {
        let helpers = BuildEnv::default()
            .helpers_dir()
            .map_err(|error| format!("no helpers folder: {error}"))?;
        let helper = helpers.join("ide-db-helper.jar");
        if !helper.is_file() {
            return Err(format!(
                "{} is missing; build the helpers of the IDE first",
                helper.display()
            ));
        }
        let jdk = self
            .jdk()
            .ok_or_else(|| "the project has no SDK to start the helper".to_owned())?;
        let mut paths = vec![helper];
        paths.extend(jars.iter().cloned());
        let classpath = java_exec::classpath_jar(&self.cache_root, &self.root, &paths)
            .map_err(|error| format!("no classpath file: {error}"))?;
        Ok(Launch::new(jdk)
            .arg("-cp")
            .arg(classpath)
            .arg("ide.db.Helper"))
    }
}

impl DatabaseSupport {
    fn jdk(&self) -> Option<PathBuf> {
        java_project::resolve_environment(&self.root)
            .selected
            .map(|jdk| jdk.java())
            .or_else(|| {
                std::env::var_os("JAVA_HOME").map(|home| {
                    let home = PathBuf::from(home);
                    home.join("bin")
                        .join(if cfg!(windows) { "java.exe" } else { "java" })
                })
            })
            .filter(|java| java.is_file())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_vendor_has_an_address_template() {
        let support = DatabaseSupport {
            root: PathBuf::new(),
            cache_root: PathBuf::new(),
            model: Arc::new(Mutex::new(None)),
        };

        let vendors = support.vendors();
        assert_eq!(vendors.len(), 7);
        for vendor in vendors {
            assert!(vendor.url_template.starts_with("jdbc:"));
            assert!(vendor.url_template.contains(":"));
        }
    }

    fn project(root: &Path) -> ProjectModel {
        ProjectModel {
            root: root.to_path_buf(),
            modules: vec![Module {
                id: "app".into(),
                name: "app".into(),
                dir: root.to_path_buf(),
                units: Vec::new(),
            }],
            resolved: true,
            ..ProjectModel::default()
        }
    }

    #[test]
    fn each_application_file_suggests_its_own_source_with_its_profile() {
        let folder = test_support::TempDir::new("database-profiles");
        let root = folder.path();
        fs::write(
            root.join("application.properties"),
            "spring.datasource.url=jdbc:h2:mem:pet\nspring.datasource.username=sa\n",
        )
        .unwrap();
        fs::write(
            root.join("application-postgres.properties"),
            "spring.datasource.url=jdbc:postgresql://localhost:5432/pet\n\
             spring.datasource.username=pet\n\
             spring.datasource.driver-class-name=org.postgresql.Driver\n",
        )
        .unwrap();
        // A profile without an address suggests no connection.
        fs::write(
            root.join("application-test.properties"),
            "spring.datasource.username=test\n",
        )
        .unwrap();

        let found = suggestions(root, &project(root));

        assert_eq!(found.len(), 2, "{found:?}");
        let shared = found
            .iter()
            .find(|suggestion| suggestion.profile.is_none())
            .expect("the shared file suggests its source");
        assert_eq!(shared.module, "app");
        assert_eq!(shared.url, "jdbc:h2:mem:pet");
        assert_eq!(shared.user, "sa");
        assert_eq!(shared.kind, Some(DriverKind::H2));
        let postgres = found
            .iter()
            .find(|suggestion| suggestion.profile.as_deref() == Some("postgres"))
            .expect("the profile file suggests its own source");
        assert_eq!(postgres.url, "jdbc:postgresql://localhost:5432/pet");
        assert_eq!(postgres.user, "pet");
        assert_eq!(postgres.kind, Some(DriverKind::PostgreSql));
    }

    #[test]
    fn a_driver_class_names_the_product_when_the_address_does_not() {
        let folder = test_support::TempDir::new("database-driver-class");
        let root = folder.path();
        fs::write(
            root.join("application.yml"),
            "spring:\n  datasource:\n    url: jdbc:something://localhost/db\n\
             \x20   driver-class-name: com.microsoft.sqlserver.jdbc.SQLServerDriver\n",
        )
        .unwrap();

        let found = suggestions(root, &project(root));

        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!(found[0].kind, Some(DriverKind::SqlServer));
        assert_eq!(found[0].profile, None);
    }
}

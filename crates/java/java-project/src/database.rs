//! The drivers the dependencies of a project carry.
//!
//! A driver is a jar of the runtime classpath of a module. The window knows nothing of
//! the build; it asks the language layer for a [`Driver`] and gets plain values. The
//! configuration files of the project are read by the Spring index of the language
//! layer, so the settings of a data source have one reader.

use std::path::PathBuf;

use database::{Driver, DriverKind};
use project_model::ProjectModel;

/// The drivers the dependencies of the project already carry, one entry per module and
/// product. Paths are the jars of the runtime classpath.
pub fn drivers(model: &ProjectModel) -> Vec<Driver> {
    let mut found: Vec<Driver> = Vec::new();
    for module in &model.modules {
        let mut jars: Vec<(DriverKind, PathBuf)> = Vec::new();
        for unit in &module.units {
            if unit.name != "main" {
                continue;
            }
            for path in model.runtime_paths(&module.id, &unit.name) {
                let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                    continue;
                };
                let Some(kind) = kind_of_jar(name) else {
                    continue;
                };
                if jars
                    .iter()
                    .any(|(known, jar)| *known == kind && *jar == path)
                {
                    continue;
                }
                jars.push((kind, path));
            }
        }
        for (kind, path) in jars {
            let label = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_else(|| format!("{kind:?}"));
            found.push(Driver {
                kind,
                label,
                jars: vec![path],
                module: Some(module.id.clone()),
            });
        }
    }
    found
}

/// The product a driver jar belongs to, from the name Maven gives it.
pub fn kind_of_jar(name: &str) -> Option<DriverKind> {
    let lower = name.to_ascii_lowercase();
    if !lower.ends_with(".jar")
        || lower.ends_with("-sources.jar")
        || lower.ends_with("-javadoc.jar")
    {
        return None;
    }
    if lower.starts_with("h2-") {
        Some(DriverKind::H2)
    } else if lower.starts_with("postgresql-") {
        Some(DriverKind::PostgreSql)
    } else if lower.starts_with("mysql-connector-j-") || lower.starts_with("mysql-connector-java-")
    {
        Some(DriverKind::MySql)
    } else if lower.starts_with("mariadb-java-client-") {
        Some(DriverKind::MariaDb)
    } else if lower.starts_with("sqlite-jdbc-") {
        Some(DriverKind::Sqlite)
    } else if lower.starts_with("ojdbc") {
        Some(DriverKind::Oracle)
    } else if lower.starts_with("mssql-jdbc-") {
        Some(DriverKind::SqlServer)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drivers_are_recognized_by_the_names_the_build_gives_them() {
        assert_eq!(kind_of_jar("h2-2.4.240.jar"), Some(DriverKind::H2));
        assert_eq!(
            kind_of_jar("postgresql-42.7.4.jar"),
            Some(DriverKind::PostgreSql)
        );
        assert_eq!(
            kind_of_jar("mysql-connector-j-9.1.0.jar"),
            Some(DriverKind::MySql)
        );
        assert_eq!(kind_of_jar("h2-2.4.240-sources.jar"), None);
        assert_eq!(kind_of_jar("junit-jupiter-5.jar"), None);
    }
}

//! The environments of `http-client.env.json` and `http-client.private.env.json` next to
//! a request file. The private file takes precedence over the public one.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// The names of the files the environments live in.
pub const ENV_FILE: &str = "http-client.env.json";
pub const PRIVATE_ENV_FILE: &str = "http-client.private.env.json";

/// The values of every environment of one folder.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Environments {
    names: Vec<String>,
    values: BTreeMap<String, BTreeMap<String, String>>,
}

impl Environments {
    /// The environment names in the order the files give them, sorted.
    pub fn names(&self) -> &[String] {
        &self.names
    }

    /// The values of `name`; empty for an environment that is not there.
    pub fn values(&self, name: &str) -> BTreeMap<String, String> {
        self.values.get(name).cloned().unwrap_or_default()
    }

    /// The first environment, or `None` when the files name none.
    pub fn default_name(&self) -> Option<&str> {
        self.names.first().map(String::as_str)
    }

    /// The name when it is one of the environments, else the first one.
    pub fn resolved_name(&self, name: Option<&str>) -> Option<String> {
        match name {
            Some(name) if self.values.contains_key(name) => Some(name.to_owned()),
            _ => self.default_name().map(str::to_owned),
        }
    }

    /// Reads the values of one file over the ones read so far.
    fn merge_file(&mut self, path: &Path) {
        let Ok(text) = std::fs::read_to_string(path) else {
            return;
        };
        let Ok(Value::Object(environments)) = serde_json::from_str::<Value>(&text) else {
            return;
        };
        for (name, values) in environments {
            let Value::Object(values) = values else {
                continue;
            };
            let entry = self.values.entry(name.clone()).or_default();
            for (key, value) in values {
                entry.insert(key, text_of(&value));
            }
            if !self.names.contains(&name) {
                self.names.push(name);
            }
        }
    }
}

/// A value of the file as the client substitutes it: strings as they are, numbers and
/// booleans as their text, everything else empty.
fn text_of(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null => String::new(),
        _ => String::new(),
    }
}

/// The two files of the folder of `request_file`; `None` when they do not exist.
pub fn environment_files(request_file: &Path) -> Vec<PathBuf> {
    let Some(folder) = request_file.parent() else {
        return Vec::new();
    };
    [ENV_FILE, PRIVATE_ENV_FILE]
        .into_iter()
        .map(|name| folder.join(name))
        .filter(|path| path.is_file())
        .collect()
}

/// Reads the environments of the folder of `request_file`. The private file is read
/// after the public one, so its values win.
pub fn load_environments(request_file: &Path) -> Environments {
    let mut environments = Environments::default();
    let Some(folder) = request_file.parent() else {
        return environments;
    };
    environments.merge_file(&folder.join(ENV_FILE));
    environments.merge_file(&folder.join(PRIVATE_ENV_FILE));
    environments.names.sort();
    environments
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{Environments, load_environments};

    fn folder(name: &str) -> std::path::PathBuf {
        let root = test_support::scratch(&format!("http-env-{name}"));
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn the_private_file_wins_over_the_public_one() {
        let root = folder("private");
        fs::write(
            root.join("http-client.env.json"),
            r#"{"dev":{"host":"http://dev","token":"public"},"prod":{"host":"http://prod"}}"#,
        )
        .unwrap();
        fs::write(
            root.join("http-client.private.env.json"),
            r#"{"dev":{"token":"private"},"local":{"host":"http://local"}}"#,
        )
        .unwrap();
        let environments = load_environments(&root.join("api.http"));
        assert_eq!(environments.names(), ["dev", "local", "prod"]);
        let dev = environments.values("dev");
        assert_eq!(dev["host"], "http://dev");
        assert_eq!(dev["token"], "private");
        assert_eq!(environments.values("prod")["host"], "http://prod");
        assert_eq!(environments.values("local")["host"], "http://local");
    }

    #[test]
    fn a_folder_without_the_files_has_no_environment() {
        let root = folder("empty");
        let environments = load_environments(&root.join("api.http"));
        assert_eq!(environments, Environments::default());
        assert_eq!(environments.default_name(), None);
    }

    #[test]
    fn a_missing_or_damaged_file_leaves_the_other_one() {
        let root = folder("damaged");
        fs::write(root.join("http-client.env.json"), "{ broken").unwrap();
        fs::write(
            root.join("http-client.private.env.json"),
            r#"{"dev":{"host":"http://dev"}}"#,
        )
        .unwrap();
        let environments = load_environments(&root.join("api.http"));
        assert_eq!(environments.names(), ["dev"]);
    }

    #[test]
    fn a_known_name_stays_and_an_unknown_one_falls_back() {
        let root = folder("names");
        fs::write(
            root.join("http-client.env.json"),
            r#"{"dev":{"host":"http://dev"},"prod":{"host":"http://prod"}}"#,
        )
        .unwrap();
        let environments = load_environments(&root.join("api.http"));
        assert_eq!(
            environments.resolved_name(Some("prod")).as_deref(),
            Some("prod")
        );
        assert_eq!(
            environments.resolved_name(Some("gone")).as_deref(),
            Some("dev")
        );
        assert_eq!(environments.resolved_name(None).as_deref(), Some("dev"));
    }
}

//! The HTTP handlers of one Java file with the port their server listens on. The
//! handlers come from the grammar of the language; the port comes from the
//! configuration of the module the file belongs to.

use std::path::{Path, PathBuf};

use lang_api::EndpointScan;

/// What the Endpoints window needs for one source.
pub fn scan(path: &Path, text: &str) -> EndpointScan {
    EndpointScan {
        port: port_of(path),
        endpoints: java_syntax::endpoints(path, text),
    }
}

/// The `server.port` of the module whose sources hold `path`, when its configuration
/// names one. The lookup walks from the source up to the folder that holds `src` and
/// reads the usual names of a configuration file there, profiles included.
pub fn port_of(path: &Path) -> Option<u16> {
    let mut folders: Vec<PathBuf> = Vec::new();
    for ancestor in path.ancestors() {
        folders.push(ancestor.to_path_buf());
        if ancestor.join("src").is_dir() {
            break;
        }
        if folders.len() >= 16 {
            break;
        }
    }
    for folder in folders {
        for (candidate, kind) in config_files(&folder) {
            let Ok(text) = std::fs::read_to_string(&candidate) else {
                continue;
            };
            let port = match kind {
                ConfigKind::Properties => port_in_properties(&text),
                ConfigKind::Yaml => port_in_yaml(&text),
            };
            if let Some(port) = port {
                return Some(port);
            }
        }
    }
    None
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ConfigKind {
    Properties,
    Yaml,
}

/// The configuration files of a module, the plain names before the profile ones.
fn config_files(folder: &Path) -> Vec<(PathBuf, ConfigKind)> {
    let mut files = Vec::new();
    for directory in [
        folder.join("src").join("main").join("resources"),
        folder.join("resources"),
        folder.to_path_buf(),
    ] {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        let mut found: Vec<(String, PathBuf, ConfigKind)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with("application") {
                continue;
            }
            let kind = if name.ends_with(".properties") {
                ConfigKind::Properties
            } else if name.ends_with(".yml") || name.ends_with(".yaml") {
                ConfigKind::Yaml
            } else {
                continue;
            };
            found.push((name, entry.path(), kind));
        }
        found.sort_by(|left, right| {
            rank(&left.0)
                .cmp(&rank(&right.0))
                .then_with(|| left.0.cmp(&right.0))
        });
        files.extend(found.into_iter().map(|(_, path, kind)| (path, kind)));
        if !files.is_empty() {
            break;
        }
    }
    files
}

/// The place of a file name in the lookup: the plain names first, then the profiles.
fn rank(name: &str) -> (u8, String) {
    let order = match name {
        "application.properties" => 0,
        "application.yml" => 1,
        "application.yaml" => 2,
        _ => 3,
    };
    (order, name.to_owned())
}

/// The keys that name the port of the usual servers.
const PORT_KEYS: &[&str] = &["server.port", "quarkus.http.port", "micronaut.server.port"];

/// A key without the profile prefix of Quarkus, `%dev.quarkus.http.port` giving
/// `quarkus.http.port`.
fn without_profile(key: &str) -> &str {
    let Some(rest) = key.strip_prefix('%') else {
        return key;
    };
    rest.split_once('.').map_or(key, |(_, key)| key)
}

/// The `server.port` of a properties file.
fn port_in_properties(text: &str) -> Option<u16> {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((key, value)) = line.split_once(['=', ':']) else {
            continue;
        };
        if PORT_KEYS.contains(&without_profile(key.trim())) {
            return number(value);
        }
    }
    None
}

/// The port under a dotted key of a YAML file: `quarkus: http: port:`, `micronaut: server:
/// port:` or `server: port:`. The small parse reads the one key the window needs, not the
/// whole language.
fn port_in_yaml(text: &str) -> Option<u16> {
    let mut stack: Vec<(usize, String)> = Vec::new();
    for line in text.lines() {
        let without_comment = line.split('#').next().unwrap_or("");
        if without_comment.trim().is_empty() {
            continue;
        }
        let indent = without_comment.len() - without_comment.trim_start().len();
        while stack.last().is_some_and(|(level, _)| *level >= indent) {
            stack.pop();
        }
        let content = without_comment.trim();
        let Some((key, value)) = content.split_once(':') else {
            continue;
        };
        let key = key.trim().trim_matches('"').trim_matches('\'');
        let value = value.trim();
        if value.is_empty() {
            stack.push((indent, key.to_owned()));
            continue;
        }
        let mut parts: Vec<&str> = stack.iter().map(|(_, key)| key.as_str()).collect();
        parts.push(key);
        let joined = parts.join(".");
        let full = without_profile(&joined);
        if PORT_KEYS.contains(&full) {
            return number(value);
        }
    }
    None
}

fn number(value: &str) -> Option<u16> {
    let value = value.trim().trim_matches('"').trim_matches('\'');
    value.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_properties_file_gives_its_server_port() {
        let text = "# the app\nspring.application.name=demo\nserver.port = 8081\n";
        assert_eq!(port_in_properties(text), Some(8081));
        assert_eq!(port_in_properties("server.port=not-a-number\n"), None);
        assert_eq!(port_in_properties("server.port\n"), None);
    }

    #[test]
    fn a_yaml_file_gives_the_port_under_server() {
        let text =
            "spring:\n  application:\n    name: demo\nserver:\n  port: 9090\n  address: 0.0.0.0\n";
        assert_eq!(port_in_yaml(text), Some(9090));
        assert_eq!(port_in_yaml("server:\n  address: 127.0.0.1\n"), None);
        assert_eq!(port_in_yaml("other:\n  port: 1\n"), None);
    }

    #[test]
    fn a_port_of_a_nested_key_is_not_taken() {
        assert_eq!(port_in_yaml("server:\n  servlet:\n    port: 1\n"), None);
        assert_eq!(
            port_in_yaml("server:\n  servlet:\n    port: 1\n  port: 8082\n"),
            Some(8082)
        );
    }

    #[test]
    fn the_port_is_read_from_the_config_of_the_module() {
        let root = test_support::scratch("endpoints-port");
        let source = root.join("src/main/java/demo/Controller.java");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "class Controller {}\n").unwrap();
        let config = root.join("src/main/resources/application.properties");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "server.port=8084\n").unwrap();
        assert_eq!(port_of(&source), Some(8084));
    }

    #[test]
    fn the_ports_of_quarkus_and_micronaut_are_read() {
        assert_eq!(port_in_properties("quarkus.http.port=8085\n"), Some(8085));
        assert_eq!(
            port_in_properties("micronaut.server.port = 8086\n"),
            Some(8086)
        );
        assert_eq!(
            port_in_yaml("quarkus:\n  http:\n    port: 8087\n    host: 0.0.0.0\n"),
            Some(8087)
        );
        assert_eq!(
            port_in_yaml("micronaut:\n  server:\n    port: 8088\n"),
            Some(8088)
        );
    }

    #[test]
    fn a_profile_key_names_the_same_port() {
        assert_eq!(
            port_in_properties("%dev.quarkus.http.port=8089\n"),
            Some(8089)
        );
        assert_eq!(
            port_in_yaml("\"%dev\":\n  micronaut:\n    server:\n      port: 8090\n"),
            Some(8090)
        );
    }

    #[test]
    fn an_application_profile_file_names_the_port() {
        let root = test_support::scratch("endpoints-profile");
        let source = root.join("src/main/java/demo/Controller.java");
        std::fs::create_dir_all(source.parent().unwrap()).unwrap();
        std::fs::write(&source, "class Controller {}\n").unwrap();
        let config = root.join("src/main/resources/application-dev.yml");
        std::fs::create_dir_all(config.parent().unwrap()).unwrap();
        std::fs::write(&config, "micronaut:\n  server:\n    port: 8091\n").unwrap();
        assert_eq!(port_of(&source), Some(8091));
    }
}

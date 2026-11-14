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
/// reads the usual names of a configuration file there.
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
        for name in [
            "application.properties",
            "application.yml",
            "application.yaml",
        ] {
            let candidates = [
                folder.join("src").join("main").join("resources").join(name),
                folder.join("resources").join(name),
                folder.join(name),
            ];
            for candidate in candidates {
                let Ok(text) = std::fs::read_to_string(&candidate) else {
                    continue;
                };
                let port = if name.ends_with("properties") {
                    port_in_properties(&text)
                } else {
                    port_in_yaml(&text)
                };
                if let Some(port) = port {
                    return Some(port);
                }
            }
        }
    }
    None
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
        if key.trim() == "server.port" {
            return number(value);
        }
    }
    None
}

/// The `port` under a top-level `server:` key of a YAML file. The small parse reads the
/// one key the window needs, not the whole language.
fn port_in_yaml(text: &str) -> Option<u16> {
    let mut in_server = false;
    let mut server_indent = 0;
    // The indent of the keys directly under `server:`, so a deeper key is not taken.
    let mut child_indent: Option<usize> = None;
    for line in text.lines() {
        let without_comment = line.split('#').next().unwrap_or("");
        if without_comment.trim().is_empty() {
            continue;
        }
        let indent = without_comment.len() - without_comment.trim_start().len();
        let content = without_comment.trim();
        if !in_server {
            if content == "server:" {
                in_server = true;
                server_indent = indent;
                child_indent = None;
            }
            continue;
        }
        if indent <= server_indent {
            in_server = false;
            child_indent = None;
            if content == "server:" {
                in_server = true;
                server_indent = indent;
            }
            continue;
        }
        let Some((key, value)) = content.split_once(':') else {
            continue;
        };
        let child = *child_indent.get_or_insert(indent);
        if indent != child {
            continue;
        }
        if key.trim() == "port" {
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
}

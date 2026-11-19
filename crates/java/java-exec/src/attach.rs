//! The JVMs of the machine that already listen for a debugger, for Attach to Process.
//! `jps -lv` of a found JDK names the processes and their agent arguments.

use std::path::{Path, PathBuf};
use std::process::Command;

use base::program::executable;

/// The part of `-agentlib:jdwp=...` and `-Xrunjdwp:...` that opens the options.
const JDWP: [&str; 2] = ["jdwp=", "jdwp:"];

/// One process that listens for a debugger.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DebuggableProcess {
    pub pid: u32,
    /// The main class or the jar, as `jps` names it.
    pub name: String,
    pub host: String,
    pub port: u16,
}

/// Runs `jps -lv` of the first JDK that has one. A machine without a JDK or without a
/// process that listens answers with an empty list, not with an error.
pub fn debug_attach_processes() -> Vec<DebuggableProcess> {
    let jdks = java_project::find_jdks();
    let homes: Vec<PathBuf> = jdks.into_iter().map(|jdk| jdk.home).collect();
    debug_attach_processes_in(&homes)
}

/// The same for the JDK folders the caller already found, in their order.
pub fn debug_attach_processes_in(homes: &[PathBuf]) -> Vec<DebuggableProcess> {
    for home in homes {
        let jps = jps_of(home);
        if !jps.is_file() {
            continue;
        }
        let Ok(output) = Command::new(&jps).arg("-lv").output() else {
            continue;
        };
        if !output.status.success() {
            continue;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        return parse_jps(&text);
    }
    Vec::new()
}

fn jps_of(home: &Path) -> PathBuf {
    home.join("bin").join(executable("jps"))
}

/// One process per line: the pid, the main class or the jar, then the VM arguments.
pub fn parse_jps(text: &str) -> Vec<DebuggableProcess> {
    let mut found: Vec<DebuggableProcess> = text.lines().filter_map(parse_line).collect();
    found.sort_by_key(|process| process.pid);
    found
}

fn parse_line(line: &str) -> Option<DebuggableProcess> {
    let line = line.trim();
    let mut parts = line.splitn(3, ' ');
    let pid: u32 = parts.next()?.trim().parse().ok()?;
    let name = parts.next().unwrap_or_default().trim().to_owned();
    let arguments = parts.next().unwrap_or_default();
    let (host, port) = jdwp_address(arguments)?;
    Some(DebuggableProcess {
        pid,
        name,
        host,
        port,
    })
}

/// The address a JDWP agent of the arguments listens on. A client agent, one that
/// attaches outward, names an address too but listens on nothing, so it is left out.
pub fn jdwp_address(arguments: &str) -> Option<(String, u16)> {
    let (start, marker) = JDWP
        .iter()
        .filter_map(|marker| arguments.find(marker).map(|at| (at, *marker)))
        .min_by_key(|(at, _)| *at)?;
    let start = start + marker.len();
    let rest = &arguments[start..];
    let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
    let mut server = false;
    let mut address = None;
    for option in rest[..end].split(',') {
        let Some((key, value)) = option.split_once('=') else {
            continue;
        };
        match key.trim() {
            "server" => server = value.trim().eq_ignore_ascii_case("y"),
            "address" => address = Some(value.trim()),
            _ => {}
        }
    }
    if !server {
        return None;
    }
    endpoint(address?)
}

/// `5005`, `localhost:5005`, `127.0.0.1:5005` or `*:5005` as a host and a port. A
/// wildcard host listens on every interface of this machine, so the debugger goes to
/// the loopback.
fn endpoint(value: &str) -> Option<(String, u16)> {
    let value = value.trim();
    let (host, port) = match value.rsplit_once(':') {
        Some((host, port)) => (host.trim(), port.trim()),
        None => ("localhost", value),
    };
    let host = host.trim_matches(['[', ']']);
    let host = match host {
        "" | "*" | "0.0.0.0" | "::" => "localhost",
        host => host,
    };
    let port: u16 = port.parse().ok()?;
    if port == 0 {
        return None;
    }
    Some((host.to_owned(), port))
}

#[cfg(test)]
mod tests {
    use super::{DebuggableProcess, jdwp_address, parse_jps};

    #[test]
    fn a_listening_agent_gives_the_process_and_its_address() {
        let text = "1234 demo.Main -agentlib:jdwp=transport=dt_socket,server=y,suspend=n,address=127.0.0.1:5005 -Xmx512m\n4321 demo.Other -Xmx64m\n";
        assert_eq!(
            parse_jps(text),
            vec![DebuggableProcess {
                pid: 1234,
                name: "demo.Main".into(),
                host: "127.0.0.1".into(),
                port: 5005,
            }]
        );
    }

    #[test]
    fn a_process_without_an_agent_is_left_out() {
        assert!(parse_jps("4321 demo.Main -Xmx64m\n").is_empty());
        assert!(parse_jps("not a list of processes\n").is_empty());
        assert!(parse_jps("").is_empty());
    }

    #[test]
    fn the_older_agent_form_is_read_too() {
        assert_eq!(
            jdwp_address("-Xrunjdwp:transport=dt_socket,server=y,suspend=n,address=localhost:5006"),
            Some(("localhost".into(), 5006))
        );
    }

    #[test]
    fn an_agent_that_attaches_outward_does_not_listen() {
        assert_eq!(
            jdwp_address("-agentlib:jdwp=transport=dt_socket,server=n,address=5005"),
            None
        );
        assert_eq!(
            jdwp_address("-agentlib:jdwp=transport=dt_socket,address=5005"),
            None
        );
    }

    #[test]
    fn a_wildcard_host_means_the_loopback() {
        assert_eq!(
            jdwp_address("-agentlib:jdwp=transport=dt_socket,server=y,address=*:5005"),
            Some(("localhost".into(), 5005))
        );
        assert_eq!(
            jdwp_address("-agentlib:jdwp=transport=dt_socket,server=y,address=0.0.0.0:5005"),
            Some(("localhost".into(), 5005))
        );
        assert_eq!(
            jdwp_address("-agentlib:jdwp=transport=dt_socket,server=y,address=[::]:5005"),
            Some(("localhost".into(), 5005))
        );
        assert_eq!(
            jdwp_address("-agentlib:jdwp=transport=dt_socket,server=y,address=[::1]:5005"),
            Some(("::1".into(), 5005))
        );
    }

    #[test]
    fn a_port_alone_is_on_this_machine_and_a_zero_port_is_not_attachable() {
        assert_eq!(
            jdwp_address("-agentlib:jdwp=transport=dt_socket,server=y,address=5005"),
            Some(("localhost".into(), 5005))
        );
        assert_eq!(
            jdwp_address("-agentlib:jdwp=transport=dt_socket,server=y,address=*:0"),
            None
        );
        assert_eq!(
            jdwp_address("-agentlib:jdwp=transport=dt_socket,server=y,address=localhost:x"),
            None
        );
    }
}

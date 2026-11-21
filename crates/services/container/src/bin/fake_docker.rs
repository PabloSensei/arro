//! A stand-in for the `docker` CLI in the tests. Its daemon is a JSON file named by
//! `FAKE_DOCKER_STATE`: `{"up": true, "containers": [...], "images": [...], "logs": {}}`.
//! It prints what the real CLI 29 prints for the calls the crate makes. `"hang": true`
//! makes it never answer and write its pid into the state, for the timeout test.

use std::io::Write;
use std::time::Duration;

use serde_json::{Value, json};

/// How long a stand-in that hangs or streams may live. Its owner normally ends it, and
/// this is the net for a run whose owner is gone: a test that returns early must not leave
/// this executable behind, where the next build cannot remove it.
const MAX_LIFETIME: Duration = Duration::from_secs(30);

fn main() {
    let path = std::env::var("FAKE_DOCKER_STATE").expect("FAKE_DOCKER_STATE");
    let mut state: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("--context") {
        args.drain(..2);
    }
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    if state["hang"].as_bool().unwrap_or(false) {
        // A CLI that never answers, for the caller's timeout. The pid is in the state so
        // that a test can check the process is gone after that timeout.
        state["pid"] = json!(std::process::id());
        save(&path, &state);
        watch_owner();
    }
    let up = state["up"].as_bool().unwrap_or(false);
    if words.first() == Some(&"context") {
        println!(
            r#"{{"Current":false,"Description":"Current DOCKER_HOST based configuration","DockerEndpoint":"npipe:////./pipe/docker_engine","Error":"","Name":"default"}}"#
        );
        println!(
            r#"{{"Current":true,"Description":"Docker Desktop","DockerEndpoint":"npipe:////./pipe/dockerDesktopLinuxEngine","Error":"","Name":"desktop-linux"}}"#
        );
        return;
    }
    if !up {
        if words.first() == Some(&"version") {
            println!(r#"{{"Client":{{"Version":"29.9.2"}},"Server":null}}"#);
        }
        eprintln!(
            "failed to connect to the docker API at npipe:////./pipe/dockerDesktopLinuxEngine; check if the path is correct and if the daemon is running"
        );
        std::process::exit(1);
    }
    match words.as_slice() {
        ["version", ..] => {
            println!(
                r#"{{"Client":{{"Version":"29.9.2"}},"Server":{{"Version":"29.0.0-fake","Os":"linux"}}}}"#
            )
        }
        ["ps", ..] => {
            for container in state["containers"].as_array().unwrap() {
                let line = json!({
                    "ID": container["id"], "Names": container["name"], "Image": container["image"],
                    "State": container["state"],
                    "Status": if container["state"] == "running" { "Up 1 second" } else { "Exited (0) 1 second ago" },
                    "Ports": "", "Labels": container.get("labels").cloned().unwrap_or(json!("")),
                });
                println!("{line}");
            }
        }
        ["images", ..] => {
            for image in state["images"].as_array().unwrap() {
                let line = json!({"ID": image["id"], "Repository": image["repository"],
                    "Tag": image["tag"], "Size": "13.3kB"});
                println!("{line}");
            }
        }
        [action @ ("start" | "stop" | "restart"), id] => {
            let next = if *action == "stop" {
                "exited"
            } else {
                "running"
            };
            set_state(&mut state, id, Some(next));
            save(&path, &state);
            println!("{id}");
        }
        ["rm", "--force", id] => {
            set_state(&mut state, id, None);
            save(&path, &state);
            println!("{id}");
        }
        ["run", "--detach", .., image] => {
            let containers = state["containers"].as_array_mut().unwrap();
            let id = format!("fake{:04}", containers.len() + 1);
            containers.push(
                json!({"id": id, "name": format!("fake_{}", containers.len() + 1),
                "image": image, "state": "running"}),
            );
            save(&path, &state);
            println!("{id}");
        }
        ["logs", "--follow", "--tail", _, id] => {
            // The console ends this call, and the owner's death or the lifetime ends it
            // when the console went away without stopping it.
            std::thread::spawn(watch_owner);
            let mut stdout = std::io::stdout();
            for line in state["logs"][*id].as_array().into_iter().flatten() {
                writeln!(stdout, "{}", line.as_str().unwrap()).unwrap();
            }
            stdout.flush().unwrap();
            // Live lines until the console stops it, as long as the container runs.
            for tick in 1.. {
                std::thread::sleep(Duration::from_millis(100));
                let state: Value =
                    serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
                let running = state["containers"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|container| container["id"] == *id && container["state"] == "running");
                if !running {
                    return;
                }
                writeln!(stdout, "tick {tick}").unwrap();
                stdout.flush().unwrap();
            }
        }
        _ => {
            eprintln!("fake docker: unknown call {words:?}");
            std::process::exit(2);
        }
    }
}

/// Ends this process when the one that started it is gone, or after [`MAX_LIFETIME`].
/// Never returns.
fn watch_owner() {
    if let Some(parent) = platform::parent_process_id() {
        match platform::ProcessWaiter::open(parent) {
            // True when the owner ended, false when the lifetime is over: either way this
            // stand-in has nothing left to answer.
            Ok(Some(waiter)) => {
                let _ = waiter.wait(MAX_LIFETIME);
                std::process::exit(3);
            }
            Ok(None) => std::process::exit(3),
            Err(_) => {}
        }
    }
    std::thread::sleep(MAX_LIFETIME);
    std::process::exit(3);
}

fn set_state(state: &mut Value, id: &str, next: Option<&str>) {
    let containers = state["containers"].as_array_mut().unwrap();
    match next {
        Some(next) => {
            for container in containers
                .iter_mut()
                .filter(|container| container["id"] == id)
            {
                container["state"] = json!(next);
            }
        }
        None => containers.retain(|container| container["id"] != id),
    }
}

fn save(path: &str, state: &Value) {
    std::fs::write(path, serde_json::to_string_pretty(state).unwrap()).unwrap();
}

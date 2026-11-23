//! A stand-in for the `kubectl` CLI in the tests. Its cluster is a JSON file named by
//! `FAKE_KUBE_STATE`: contexts, namespaces, resources, logs and secret values. It prints
//! what the real CLI 1.31 prints for the calls the crate makes and records every call in
//! a file beside the state.

use std::io::{Read, Write};
use std::time::Duration;

use serde_json::{Value, json};

fn main() {
    let path = std::env::var("FAKE_KUBE_STATE").expect("FAKE_KUBE_STATE");
    let mut state: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let args: Vec<String> = std::env::args().skip(1).collect();
    record(&path, &args);
    let words = command_words(&args);
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    let up = state["up"].as_bool().unwrap_or(false);
    if words.first() == Some(&"version") {
        if up {
            println!(
                r#"{{"clientVersion":{{"gitVersion":"v1.31.0"}},"serverVersion":{{"gitVersion":"v1.30.2"}}}}"#
            );
        } else {
            println!(r#"{{"clientVersion":{{"gitVersion":"v1.31.0"}}}}"#);
            eprintln!(
                "Unable to connect to the server: dial tcp 127.0.0.1:6443: connectex: No connection could be made"
            );
            std::process::exit(1);
        }
        return;
    }
    if !up && !matches!(words.first(), Some(&"config")) {
        eprintln!("Unable to connect to the server");
        std::process::exit(1);
    }
    match words.as_slice() {
        ["config", "view", ..] => config_view(&state),
        ["config", "get-contexts", ..] => {
            for context in state["contexts"].as_array().into_iter().flatten() {
                println!("{}", context["name"].as_str().unwrap_or_default());
            }
        }
        ["config", "current-context", ..] => {
            println!("{}", state["current_context"].as_str().unwrap_or_default());
        }
        ["config", "use-context", name, ..] => {
            state["current_context"] = Value::String((*name).to_owned());
            save(&path, &state);
            println!("Switched to context \"{name}\".");
        }
        ["get", "namespaces", ..] => {
            let items: Vec<Value> = state["namespaces"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|name| json!({"metadata": {"name": name}}))
                .collect();
            println!(
                "{}",
                json!({"apiVersion": "v1", "kind": "List", "items": items})
            );
        }
        ["get", kinds, ..] if kinds.contains(',') => {
            let namespace = flag(&args, "--namespace");
            let items: Vec<Value> = state["resources"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|resource| match &namespace {
                    Some(namespace) => resource["metadata"]["namespace"] == *namespace,
                    None => true,
                })
                .cloned()
                .collect();
            println!(
                "{}",
                json!({"apiVersion": "v1", "kind": "List", "items": items})
            );
        }
        ["get", target, ..] if target.starts_with("pod/") => {
            let item = pod_of(&state, target.trim_start_matches("pod/"));
            println!("{item}");
        }
        ["get", "pod", name, ..] => {
            println!("{}", pod_of(&state, name));
        }
        ["get", target, "-o", "json", ..] if target.starts_with("secret/") => {
            let name = target.trim_start_matches("secret/");
            let mut data = serde_json::Map::new();
            if let Some(values) = state["secret_values"].as_object() {
                for (key, value) in values {
                    if let Some(key) = key.strip_prefix(&format!("{name}/"))
                        && let Some(value) = value.as_str()
                    {
                        // The API returns the value base64 encoded; the tests write the
                        // plain value and this encodes it the way the API does.
                        data.insert(key.to_owned(), Value::String(encode_base64(value)));
                    }
                }
            }
            println!("{}", json!({"kind": "Secret", "data": data}));
        }
        ["get", target, "-o", "yaml", ..] => {
            let name = target.rsplit('/').next().unwrap_or(target);
            let kind = target.split('/').next().unwrap_or("Resource");
            println!(
                "apiVersion: v1\nkind: {kind}\nmetadata:\n  name: {name}\n  namespace: {}\n",
                flag(&args, "--namespace").unwrap_or_else(|| "default".into())
            );
        }
        ["describe", _, name, ..] => {
            println!("Name:         {name}");
            println!(
                "Namespace:    {}",
                flag(&args, "--namespace").unwrap_or_else(|| "default".into())
            );
            println!("Events:       <none>");
        }
        ["delete", kind, name, ..] => {
            let kind = singular(kind);
            let namespace = flag(&args, "--namespace");
            let resources = state["resources"].as_array_mut().unwrap();
            resources.retain(|resource| {
                !(resource["kind"] == kind
                    && resource["metadata"]["name"] == *name
                    && namespace
                        .as_ref()
                        .is_none_or(|namespace| resource["metadata"]["namespace"] == *namespace))
            });
            save(&path, &state);
            println!("{kind}/{name} deleted");
        }
        ["rollout", "restart", target, ..] => {
            println!("{target} restarted");
        }
        ["scale", target, ..] => {
            let replicas = flag(&args, "--replicas")
                .and_then(|text| text.parse::<u64>().ok())
                .unwrap_or(1);
            let kind = singular(target.split('/').next().unwrap_or_default());
            let name = target.split('/').nth(1).unwrap_or_default();
            for resource in state["resources"].as_array_mut().unwrap() {
                if resource["kind"] == kind.as_str() && resource["metadata"]["name"] == name {
                    resource["spec"]["replicas"] = json!(replicas);
                }
            }
            save(&path, &state);
            println!("{target} scaled");
        }
        ["apply", "-f", file] => {
            let manifest = std::fs::read_to_string(file).unwrap_or_default();
            let kind = manifest
                .lines()
                .find_map(|line| line.strip_prefix("kind:"))
                .map(str::trim)
                .unwrap_or("resource");
            println!("{} configured", kind.to_lowercase());
        }
        ["diff", "-f", _] => {
            let diff = state["diff"].as_str().unwrap_or_default();
            if diff.is_empty() {
                return;
            }
            println!("{diff}");
            std::process::exit(1);
        }
        ["logs", pod, ..] => {
            let mut stdout = std::io::stdout();
            let previous = words.contains(&"--previous");
            for line in state["logs"][pod].as_array().into_iter().flatten() {
                writeln!(stdout, "{}", line.as_str().unwrap()).unwrap();
            }
            if previous {
                writeln!(stdout, "the previous container of {pod}").unwrap();
            }
            stdout.flush().unwrap();
            let mut tick = 0u32;
            loop {
                std::thread::sleep(Duration::from_millis(100));
                tick += 1;
                writeln!(stdout, "tick {tick}").unwrap();
                stdout.flush().unwrap();
            }
        }
        ["port-forward", _, ports, ..] => port_forward(ports),
        ["exec", ..] => println!("exec"),
        _ => {
            eprintln!("fake kubectl: unknown call {words:?}");
            std::process::exit(2);
        }
    }
}

/// The words after the global options the crate puts before the subcommand.
fn command_words(args: &[String]) -> Vec<String> {
    let mut words = Vec::new();
    let mut rest = args.iter();
    while let Some(word) = rest.next() {
        if matches!(word.as_str(), "--kubeconfig" | "--context") {
            rest.next();
            continue;
        }
        words.push(word.clone());
    }
    words
}

/// The value of a flag of the call.
fn flag(args: &[String], name: &str) -> Option<String> {
    let index = args.iter().position(|word| word == name)?;
    args.get(index + 1).cloned()
}

fn encode_base64(text: &str) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let bytes = text.as_bytes();
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let mut value = 0u32;
        for (index, byte) in chunk.iter().enumerate() {
            value |= u32::from(*byte) << (16 - index * 8);
        }
        for index in 0..4 {
            if index <= chunk.len() {
                let shift = 18 - index * 6;
                out.push(ALPHABET[((value >> shift) & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

fn singular(plural: &str) -> String {
    let plural = plural.trim_end_matches('s');
    match plural {
        "pod" => "Pod",
        "deployment" => "Deployment",
        "statefulset" => "StatefulSet",
        "service" => "Service",
        "configmap" => "ConfigMap",
        "secret" => "Secret",
        "ingress" => "Ingress",
        "job" => "Job",
        "cronjob" => "CronJob",
        other => other,
    }
    .to_owned()
}

fn pod_of(state: &Value, name: &str) -> Value {
    state["resources"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|resource| resource["kind"] == "Pod" && resource["metadata"]["name"] == name)
        .cloned()
        .unwrap_or_else(|| json!({"kind": "Pod", "metadata": {"name": name}}))
}

fn config_view(state: &Value) {
    let contexts: Vec<Value> = state["contexts"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|context| {
            json!({
                "name": context["name"],
                "context": {
                    "cluster": context.get("cluster").cloned().unwrap_or(json!("kind")),
                    "namespace": context.get("namespace").cloned().unwrap_or(json!("")),
                },
            })
        })
        .collect();
    println!(
        "{}",
        json!({
            "current-context": state["current_context"],
            "contexts": contexts,
        })
    );
}

/// Binds the local port of the forwarding and serves it until the process is killed.
fn port_forward(ports: &str) {
    let local = ports
        .split(':')
        .next()
        .and_then(|port| port.parse::<u16>().ok())
        .unwrap_or(0);
    let listener = std::net::TcpListener::bind(("127.0.0.1", local)).unwrap();
    let port = listener.local_addr().unwrap().port();
    println!("Forwarding from 127.0.0.1:{port} -> 80");
    std::io::stdout().flush().unwrap();
    for stream in listener.incoming() {
        let Ok(mut stream) = stream else {
            return;
        };
        let mut buffer = [0; 64];
        let _ = stream.read(&mut buffer);
    }
}

fn record(path: &str, args: &[String]) {
    let calls = format!("{path}.calls");
    let line = format!("{}\n", args.join(" "));
    for _ in 0..200 {
        let opened = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&calls);
        let Ok(mut file) = opened else {
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        if file.write_all(line.as_bytes()).is_ok() {
            return;
        }
    }
}

fn save(path: &str, state: &Value) {
    let _ = std::fs::write(path, serde_json::to_string_pretty(state).unwrap());
}

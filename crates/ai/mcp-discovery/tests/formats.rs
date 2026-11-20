//! The shapes the auto-configuration writes for the clients it knows: the key of the
//! servers, the kind of a transport and the place of the Codex address. Every file lives
//! under a folder of the test.

use std::fs;

use mcp_discovery::clients::{self, Client, Scope, Server, Transport, has_entry, snippet};
use test_support::TempDir;

fn server() -> Server {
    Server {
        command: "ide-mcp.exe".to_string(),
        url: None,
        token: None,
    }
}

fn http_server() -> Server {
    Server {
        command: "ide-mcp.exe".to_string(),
        url: Some("http://127.0.0.1:4321/mcp".to_string()),
        token: Some("secret".to_string()),
    }
}

fn home(name: &str) -> TempDir {
    TempDir::new(name)
}

fn root_of(text: &str) -> serde_json::Value {
    serde_json::from_str(text).unwrap_or_else(|error| panic!("{error}: {text}"))
}

#[test]
fn vscode_writes_the_server_under_servers() {
    let home = home("formats-vscode");
    let project = home.path().join("shop");
    fs::create_dir_all(&project).unwrap();
    let file = project.join(".vscode").join("mcp.json");

    clients::configure(
        Client::VsCode,
        Scope::Project,
        Transport::Stdio,
        &server(),
        home.path(),
        &project,
    )
    .expect("write");

    let text = fs::read_to_string(&file).unwrap();
    let root = root_of(&text);
    assert!(root.get("mcpServers").is_none(), "{text}");
    assert_eq!(root["servers"]["arro"]["type"], "stdio");
    assert_eq!(root["servers"]["arro"]["command"], "ide-mcp.exe");
    assert!(root["servers"]["arro"]["args"].is_array(), "{text}");
    assert!(has_entry(Client::VsCode, &file).unwrap());
}

#[test]
fn vscode_keeps_the_http_kind_of_a_streamed_server() {
    let home = home("formats-vscode-http");
    let project = home.path().join("shop");
    fs::create_dir_all(&project).unwrap();
    let file = project.join(".vscode").join("mcp.json");

    clients::configure(
        Client::VsCode,
        Scope::Project,
        Transport::HttpStream,
        &http_server(),
        home.path(),
        &project,
    )
    .expect("write");

    let root = root_of(&fs::read_to_string(&file).unwrap());
    assert_eq!(root["servers"]["arro"]["type"], "http");
    assert_eq!(root["servers"]["arro"]["url"], "http://127.0.0.1:4321/mcp");
    assert_eq!(
        root["servers"]["arro"]["headers"]["Authorization"],
        "Bearer secret"
    );
}

#[test]
fn junie_writes_the_server_under_mcp_servers() {
    let home = home("formats-junie");
    let project = home.path().join("shop");
    fs::create_dir_all(&project).unwrap();
    let file = project.join(".junie").join("mcp.json");

    clients::configure(
        Client::Junie,
        Scope::Project,
        Transport::Stdio,
        &server(),
        home.path(),
        &project,
    )
    .expect("write");

    let text = fs::read_to_string(&file).unwrap();
    let root = root_of(&text);
    assert!(root.get("servers").is_none(), "{text}");
    assert!(root["mcpServers"]["arro"]["command"].is_string(), "{text}");
    assert!(
        snippet(Client::Junie, Transport::Stdio, &server())
            .expect("snippet")
            .contains("\"mcpServers\""),
        "the manual snippet names the servers key"
    );
}

#[test]
fn copilot_writes_the_server_under_mcp_servers() {
    let home = home("formats-copilot");
    let project = home.path().join("shop");
    fs::create_dir_all(&project).unwrap();
    let file = project.join(".github").join("copilot").join("mcp.json");

    clients::configure(
        Client::CopilotCli,
        Scope::Project,
        Transport::Stdio,
        &server(),
        home.path(),
        &project,
    )
    .expect("write");

    let root = root_of(&fs::read_to_string(&file).unwrap());
    assert!(root.get("servers").is_none());
    assert_eq!(root["mcpServers"]["arro"]["command"], "ide-mcp.exe");
}

#[test]
fn codex_http_writes_a_url_into_the_server_table() {
    let home = home("formats-codex-http");
    let project = home.path().join("shop");
    fs::create_dir_all(&project).unwrap();
    let folder = home.path().join(".codex");
    fs::create_dir_all(&folder).unwrap();
    let file = folder.join("config.toml");
    fs::write(&file, "model = \"gpt\"\n").unwrap();

    for _ in 0..2 {
        clients::configure(
            Client::Codex,
            Scope::User,
            Transport::HttpStream,
            &http_server(),
            home.path(),
            &project,
        )
        .expect("write");
    }

    let text = fs::read_to_string(&file).unwrap();
    assert_eq!(text.matches("[mcp_servers.arro]").count(), 1, "{text}");
    assert!(text.starts_with("model = \"gpt\"\n"), "{text}");
    assert!(text.contains("[mcp_servers.arro]\nurl = "), "{text}");
    assert!(
        text.contains("http://127.0.0.1:4321/mcp"),
        "the address reaches the table: {text}"
    );
    assert!(
        !text.contains("command = "),
        "an HTTP server has no command: {text}"
    );
    assert!(has_entry(Client::Codex, &file).unwrap());
    assert!(
        clients::snippet(Client::Codex, Transport::HttpStream, &http_server())
            .expect("snippet")
            .contains("url = "),
        "the manual snippet names the address too"
    );

    clients::remove(Client::Codex, Scope::User, home.path(), &project).expect("remove");
    let text = fs::read_to_string(&file).unwrap();
    assert!(!text.contains("[mcp_servers.arro]"), "{text}");
    assert!(text.starts_with("model = \"gpt\"\n"), "{text}");
}

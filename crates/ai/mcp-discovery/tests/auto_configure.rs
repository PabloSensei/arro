//! The auto-configuration of a client writes one entry into the settings file of that
//! client and leaves the rest of it alone. Every file lives under a folder of the test;
//! the profile of the user is not read or written.

use std::fs;
use std::path::Path;

use mcp_discovery::clients::{
    self, Client, Scope, Server, Transport, has_entry, home_folder, state,
};
use test_support::TempDir;

fn server() -> Server {
    Server {
        command: r"C:\Tools\ide-mcp.exe".to_string(),
        url: None,
        token: None,
    }
}

fn home(name: &str) -> TempDir {
    TempDir::new(name)
}

#[test]
fn claude_code_gets_the_entry_and_keeps_its_other_keys() {
    let home = home("mcp-claude");
    let project = home.path().join("project");
    let file = home.path().join(".claude.json");
    fs::write(
        &file,
        r#"{"theme":"dark","mcpServers":{"other":{"command":"other-program"}}}"#,
    )
    .unwrap();

    let written = clients::configure(
        Client::ClaudeCode,
        Scope::User,
        Transport::Stdio,
        &server(),
        home.path(),
        &project,
    )
    .expect("write");
    assert_eq!(written, file);

    let text = fs::read_to_string(&file).unwrap();
    let root: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(root["theme"], "dark", "{text}");
    assert_eq!(root["mcpServers"]["other"]["command"], "other-program");
    assert_eq!(
        root["mcpServers"]["arro"]["command"],
        r"C:\Tools\ide-mcp.exe"
    );
    assert!(has_entry(Client::ClaudeCode, &file).unwrap());
    assert!(
        Path::new(&format!("{}.arro-backup", file.display())).is_file(),
        "a copy of the old file stays next to it"
    );
}

#[test]
fn the_second_auto_configure_does_not_duplicate_the_entry() {
    let home = home("mcp-claude-twice");
    let project = home.path().join("project");
    let file = home.path().join(".claude.json");
    fs::write(&file, r#"{"theme":"dark"}"#).unwrap();

    for _ in 0..2 {
        clients::configure(
            Client::ClaudeCode,
            Scope::User,
            Transport::Stdio,
            &server(),
            home.path(),
            &project,
        )
        .expect("write");
    }

    let text = fs::read_to_string(&file).unwrap();
    assert_eq!(text.matches("\"arro\"").count(), 1, "{text}");
    let root: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        root["mcpServers"]["arro"]["command"],
        r"C:\Tools\ide-mcp.exe"
    );
}

#[test]
fn remove_takes_the_entry_and_leaves_the_others() {
    let home = home("mcp-claude-remove");
    let project = home.path().join("project");
    let file = home.path().join(".claude.json");
    fs::write(
        &file,
        r#"{"theme":"dark","mcpServers":{"other":{"command":"other-program"}}}"#,
    )
    .unwrap();
    clients::configure(
        Client::ClaudeCode,
        Scope::User,
        Transport::Stdio,
        &server(),
        home.path(),
        &project,
    )
    .expect("write");

    assert!(
        clients::remove(Client::ClaudeCode, Scope::User, home.path(), &project).expect("remove")
    );

    let text = fs::read_to_string(&file).unwrap();
    let root: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(root["theme"], "dark");
    assert!(root["mcpServers"].get("arro").is_none(), "{text}");
    assert_eq!(root["mcpServers"]["other"]["command"], "other-program");
    assert!(!has_entry(Client::ClaudeCode, &file).unwrap());
    assert!(
        !clients::remove(Client::ClaudeCode, Scope::User, home.path(), &project).expect("remove"),
        "the second remove finds nothing"
    );
}

#[test]
fn codex_gets_a_table_and_keeps_the_rest_of_the_toml() {
    let home = home("mcp-codex");
    let project = home.path().join("project");
    let file = home.path().join(".codex").join("config.toml");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "model = \"gpt\"\n\n[other]\nx = 1\n").unwrap();

    for _ in 0..2 {
        clients::configure(
            Client::Codex,
            Scope::User,
            Transport::Stdio,
            &server(),
            home.path(),
            &project,
        )
        .expect("write");
    }

    let text = fs::read_to_string(&file).unwrap();
    assert_eq!(
        text.matches("[mcp_servers.arro]").count(),
        1,
        "the second write replaces the table: {text}"
    );
    assert!(text.starts_with("model = \"gpt\"\n"), "{text}");
    assert!(text.contains("[other]\nx = 1"), "{text}");
    assert!(
        text.contains("command = \"C:\\\\Tools\\\\ide-mcp.exe\""),
        "{text}"
    );
    assert!(has_entry(Client::Codex, &file).unwrap());

    assert!(clients::remove(Client::Codex, Scope::User, home.path(), &project).expect("remove"));
    let text = fs::read_to_string(&file).unwrap();
    assert!(!text.contains("[mcp_servers.arro]"), "{text}");
    assert!(text.contains("[other]\nx = 1"), "{text}");
    assert!(!has_entry(Client::Codex, &file).unwrap());
}

#[test]
fn a_project_file_takes_the_entry_of_the_project() {
    let home = home("mcp-project");
    let project = home.path().join("shop");
    fs::create_dir_all(&project).unwrap();
    let file = project.join(".mcp.json");

    clients::configure(
        Client::ClaudeCode,
        Scope::Project,
        Transport::Stdio,
        &server(),
        home.path(),
        &project,
    )
    .expect("write");

    let text = fs::read_to_string(&file).unwrap();
    let root: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        root["mcpServers"]["arro"]["command"],
        r"C:\Tools\ide-mcp.exe"
    );
}

#[test]
fn a_client_is_found_by_its_folder_or_its_file() {
    let home = home("mcp-found");
    let project = home.path().join("project");
    assert_eq!(
        state(Client::Codex, Scope::User, home.path(), &project).status,
        clients::Status::Missing,
        "an empty home has no Codex"
    );

    fs::create_dir_all(home.path().join(".codex")).unwrap();
    let found = state(Client::Codex, Scope::User, home.path(), &project);
    assert_eq!(found.status, clients::Status::Found);
    assert!(!found.configured);

    fs::write(
        home.path().join(".codex").join("config.toml"),
        "model = 1\n",
    )
    .unwrap();
    assert_eq!(
        state(Client::CopilotCli, Scope::User, home.path(), &project).status,
        clients::Status::Missing
    );
}

#[test]
fn the_home_folder_is_the_one_of_the_user() {
    let folder = home_folder();
    assert!(!folder.as_os_str().is_empty(), "USERPROFILE or HOME is set");
}

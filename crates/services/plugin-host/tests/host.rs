//! A toy plugin process under the host: start, a command that edits a document through
//! the window, a hang, a crash, a broken manifest. No window: a thread plays it.

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use plugin_host::{
    ActiveDocument, IdeCall, PluginError, PluginHost, PluginSettings, PluginState, Timeouts,
};

const TOY: &str = env!("CARGO_BIN_EXE_plugin-host-toy");

fn plugins_dir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!("plugin-host-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    let write = |path: &str, text: &str| {
        let path = dir.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
    };
    let toy = TOY.replace('\\', "/");
    write(
        "toy/plugin.toml",
        &format!(
            r#"
id = "toy"
name = "Toy"
version = "1.0.0"
api = 1
command = ["{toy}"]

[[contributes.commands]]
id = "upper"
title = "Upper Case Selection"
key = "ctrl-alt-u"

[[contributes.commands]]
id = "hang"
title = "Hang"

[[contributes.commands]]
id = "crash"
title = "Crash"

[[contributes.commands]]
id = "fail"
title = "Fail"

[[contributes.themes]]
name = "Toy Dark"
file = "theme.toml"

[[contributes.keybindings]]
key = "ctrl-alt-h"
command = "toy.hang"
"#
        ),
    );
    write(
        "toy/theme.toml",
        "dark = true\n[colors]\neditor_bg = \"#101010\"\n",
    );
    write("broken/plugin.toml", "id = \"broken\"\nname = \"Broken\"\n");
    write(
        "off/plugin.toml",
        "id = \"off\"\nname = \"Off\"\nversion = \"1\"\napi = 1\n",
    );
    dir
}

/// One document with an undo stack, the way the window would edit it.
#[derive(Default)]
struct Window {
    text: String,
    selection: std::ops::Range<usize>,
    undo: Vec<String>,
}

fn state(host: &PluginHost, id: &str) -> PluginState {
    let plugin = host
        .plugins()
        .iter()
        .find(|plugin| plugin.id() == id)
        .unwrap();
    plugin.state()
}

#[test]
fn a_toy_plugin_runs_edits_hangs_and_crashes_while_the_host_goes_on() {
    // The plugin must not see what the IDE keeps for itself.
    unsafe { std::env::set_var("MCP_TOKEN", "secret") };
    let dir = plugins_dir();
    let settings = PluginSettings {
        disabled: ["off".to_owned()].into(),
    };
    let timeouts = Timeouts {
        command: Duration::from_secs(2),
        ..Timeouts::default()
    };
    let (host, requests) = PluginHost::load(&dir, &settings, timeouts);

    // The list: one broken, one off, one waiting for its first command.
    let ids: Vec<_> = host.plugins().iter().map(|plugin| plugin.id()).collect();
    assert_eq!(ids, ["broken", "off", "toy"]);
    let PluginState::Broken(error) = state(&host, "broken") else {
        panic!("broken is not broken");
    };
    assert!(error.contains("missing field"), "{error}");
    assert_eq!(state(&host, "off"), PluginState::Disabled);
    assert_eq!(state(&host, "toy"), PluginState::NotStarted);

    let commands = host.commands();
    assert_eq!(commands[0].id, "toy.upper");
    assert_eq!(commands[0].key.as_deref(), Some("ctrl-alt-u"));
    assert_eq!(host.keybindings()[0].command, "toy.hang");
    let themes = host.themes();
    assert_eq!(
        themes[0].theme.as_ref().unwrap().colors["editor_bg"],
        0x101010ff
    );

    let window = Arc::new(Mutex::new(Window {
        text: "привет, world".into(),
        selection: 0.."привет".len(),
        undo: Vec::new(),
    }));
    let playing = window.clone();
    thread::spawn(move || {
        for request in requests {
            let mut window = playing.lock().unwrap();
            match request.call.clone() {
                IdeCall::ActiveDocument => {
                    let document =
                        ActiveDocument::new(None, window.text.clone(), window.selection.clone());
                    request.answer(document);
                }
                IdeCall::ReplaceSelection { text } => {
                    let previous = window.text.clone();
                    window.undo.push(previous);
                    let range = window.selection.clone();
                    window.text.replace_range(range.clone(), &text);
                    window.selection = range.start..range.start + text.len();
                    request.done();
                }
                other => request.fail(format!("not in this test: {other:?}")),
            }
        }
    });

    // The first command starts the process, the edit goes through the window.
    host.execute("toy.upper").unwrap();
    assert_eq!(window.lock().unwrap().text, "ПРИВЕТ, world");
    assert_eq!(state(&host, "toy"), PluginState::Running);
    let undo = window.lock().unwrap().undo.pop().unwrap();
    assert_eq!(undo, "привет, world");
    let toy = host
        .plugins()
        .iter()
        .find(|plugin| plugin.id() == "toy")
        .unwrap();
    assert!(
        toy.log().contains(&"started, MCP_TOKEN=\"\"".to_owned()),
        "{:?}",
        toy.log()
    );

    // A command that fails leaves the plugin running.
    let error = host.execute("toy.fail").unwrap_err();
    assert_eq!(
        error,
        PluginError::Command("toy: this command fails".into())
    );
    assert_eq!(state(&host, "toy"), PluginState::Running);

    // A hang is cut off by the timeout and the plugin is marked failed.
    let error = host.execute("toy.hang").unwrap_err();
    assert!(matches!(error, PluginError::Failed(_)), "{error}");
    assert!(matches!(state(&host, "toy"), PluginState::Failed(_)));

    // The next command starts it again, and a crash is told with its code and stderr.
    let error = host.execute("toy.crash").unwrap_err();
    let PluginState::Failed(reason) = state(&host, "toy") else {
        panic!("no failure after a crash: {error}");
    };
    assert!(reason.contains("code 3"), "{reason}");
    assert!(reason.contains("asked to crash"), "{reason}");

    // The host lives on: the plugin starts once more.
    host.execute("toy.upper").unwrap();
    assert_eq!(state(&host, "toy"), PluginState::Running);
    assert!(matches!(
        host.execute("off.anything"),
        Err(PluginError::NoSuchCommand(_))
    ));

    host.shutdown();
    assert_eq!(state(&host, "toy"), PluginState::NotStarted);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn settings_survive_a_save_and_a_missing_file_means_all_on() {
    let path = std::env::temp_dir()
        .join(format!("plugin-settings-{}", std::process::id()))
        .join("plugins.toml");
    assert_eq!(
        PluginSettings::load(&path).unwrap(),
        PluginSettings::default()
    );

    let settings = PluginSettings {
        disabled: ["a".to_owned(), "b".to_owned()].into(),
    };
    settings.save(&path).unwrap();

    assert_eq!(PluginSettings::load(&path).unwrap(), settings);
    let _ = fs::remove_dir_all(path.parent().unwrap());
}

/// The example from `examples/plugins/hello`, as a user would install it. Needs Node.
#[test]
fn the_hello_example_upper_cases_the_selection() {
    let node = std::process::Command::new("node").arg("--version").output();
    if !node.is_ok_and(|output| output.status.success()) {
        eprintln!("no node in PATH, the hello example is not run");
        return;
    }
    let example = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../../examples/plugins/hello");
    let dir = std::env::temp_dir().join(format!("plugin-hello-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    for file in ["plugin.toml", "main.js", "themes/dusk.toml"] {
        let target = dir.join("hello").join(file);
        fs::create_dir_all(target.parent().unwrap()).unwrap();
        fs::copy(example.join(file), target).unwrap();
    }
    let (host, requests) = PluginHost::load(&dir, &PluginSettings::default(), Timeouts::default());
    assert_eq!(host.commands()[0].id, "hello.upper-case");
    assert!(host.themes()[0].theme.as_ref().unwrap().dark);

    let text = Arc::new(Mutex::new("let ёлка = 1;".to_owned()));
    let edited = text.clone();
    thread::spawn(move || {
        for request in requests {
            let mut text = edited.lock().unwrap();
            let selection = 4..4 + "ёлка".len();
            match request.call.clone() {
                IdeCall::ActiveDocument => {
                    request.answer(ActiveDocument::new(None, text.clone(), selection))
                }
                IdeCall::ReplaceSelection { text: new } => {
                    text.replace_range(selection, &new);
                    request.done();
                }
                other => request.fail(format!("not in this test: {other:?}")),
            }
        }
    });

    host.execute("hello.upper-case").unwrap();

    assert_eq!(*text.lock().unwrap(), "let ЁЛКА = 1;");
    host.shutdown();
    let _ = fs::remove_dir_all(&dir);
}

//! Plugins live in the folder of the IDE only. An opened project may carry `plugin.toml`
//! or a `plugins` folder with a plugin, and none of it is ever loaded.

use std::fs;
use std::path::Path;

use plugin_host::{PluginHost, PluginSettings, Timeouts, default_plugins_dir};

fn write_manifest(dir: &Path, id: &str) {
    fs::create_dir_all(dir).unwrap();
    let text = format!("id = \"{id}\"\nname = \"{id}\"\nversion = \"1\"\napi = 1\n");
    fs::write(dir.join("plugin.toml"), text).unwrap();
}

fn ids(host: &PluginHost) -> Vec<String> {
    host.plugins().iter().map(|plugin| plugin.id()).collect()
}

#[test]
fn plugins_come_from_the_ide_folder_and_never_from_the_project() {
    let cwd = std::env::current_dir().unwrap();
    let root = std::env::temp_dir().join(format!("plugin-isolation-{}", std::process::id()));
    let _ = fs::remove_dir_all(&root);
    let appdata = root.join("appdata");
    let project = root.join("project");
    let ide_plugins = appdata.join("Arro").join("plugins");

    // A project that would be a plugin if the IDE looked into it.
    write_manifest(&project, "root-plugin");
    write_manifest(&project.join("plugins").join("nested"), "nested");
    write_manifest(&ide_plugins.join("installed"), "installed");

    // The folder of the IDE does not move with the working folder of the project.
    unsafe {
        std::env::set_var("APPDATA", &appdata);
        std::env::remove_var("IDE_PLUGINS_DIR");
        std::env::set_current_dir(&project).unwrap();
    }
    assert_eq!(default_plugins_dir(), Some(ide_plugins.clone()));
    let dir = default_plugins_dir().unwrap();
    let (host, _requests) = PluginHost::load(&dir, &PluginSettings::default(), Timeouts::default());
    assert_eq!(ids(&host), ["installed"]);

    // IDE_PLUGINS_DIR replaces the folder, and the project is still not read.
    let test_plugins = root.join("test-plugins");
    write_manifest(&test_plugins.join("test"), "test");
    unsafe { std::env::set_var("IDE_PLUGINS_DIR", &test_plugins) };
    assert_eq!(default_plugins_dir(), Some(test_plugins.clone()));
    let dir = default_plugins_dir().unwrap();
    let (host, _requests) = PluginHost::load(&dir, &PluginSettings::default(), Timeouts::default());
    assert_eq!(ids(&host), ["test"]);

    std::env::set_current_dir(cwd).unwrap();
    let _ = fs::remove_dir_all(&root);
}

//! A project that came from another IDE: its code style files name the indentation of
//! the editor, the Settings window shows where the style comes from and can turn it off,
//! and the settings of the window travel in an archive of their own.

use std::fs;
use std::path::PathBuf;

use ui_kit::gpui;
use ui_kit::gpui::{TestAppContext, WindowHandle};
use workspace::{Settings, Workspace};

use crate::settings::SettingsView;
use crate::settings_archive::{parts_for, without_secrets};
use crate::window_tests::{
    buffer_of, click, draw, editor_of, open_path, open_window, press, scratch,
};

fn project(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let root = scratch(name);
    for (path, text) in files {
        let path = root.join(path);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }
    root
}

const CONFIG: &str = r#"<component name="ProjectCodeStyleConfiguration">
  <state>
    <option name="USE_PER_PROJECT_SETTINGS" value="true" />
  </state>
</component>"#;

const PROJECT: &str = r#"<code_scheme name="Project" version="173">
  <option name="RIGHT_MARGIN" value="100" />
  <codeStyleSettings language="TEXT">
    <option name="INDENT_SIZE" value="2" />
    <option name="TAB_SIZE" value="2" />
    <option name="USE_TAB_CHARACTER" value="true" />
  </codeStyleSettings>
</code_scheme>"#;

fn project_with_a_style(name: &str) -> PathBuf {
    project(
        name,
        &[
            (".idea/codeStyles/codeStyleConfig.xml", CONFIG),
            (".idea/codeStyles/Project.xml", PROJECT),
            ("src/main.txt", "alpha {\n"),
        ],
    )
}

fn settings_view(
    cx: &TestAppContext,
    window: WindowHandle<Workspace>,
) -> Option<gpui::Entity<SettingsView>> {
    window
        .read_with(cx, |workspace, _| workspace.overlay_view())
        .ok()
        .flatten()
        .and_then(|view| view.downcast::<SettingsView>().ok())
}

#[gpui::test]
fn the_file_menu_offers_the_settings_archive(cx: &mut TestAppContext) {
    let root = scratch("idea-menu");
    let window = open_window(cx, root);
    let entries = window
        .update(cx, |workspace, _, cx| workspace.main_menu(cx))
        .unwrap();
    let file = entries
        .iter()
        .find(|entry| entry.label.as_ref() == "File")
        .expect("the main menu has File");
    let manage = file
        .submenu
        .iter()
        .find(|entry| entry.label.as_ref() == "Manage IDE Settings")
        .expect("File has Manage IDE Settings");
    let rows: Vec<&str> = manage
        .submenu
        .iter()
        .map(|entry| entry.label.as_ref())
        .collect();
    assert_eq!(rows, ["Export Settings...", "Import Settings..."]);
}

#[gpui::test]
fn the_project_style_gives_the_indent_of_a_new_line(cx: &mut TestAppContext) {
    let root = project_with_a_style("idea-indent");
    let window = open_window(cx, root.clone());
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let view = editor_of(cx, window, &path);
    assert_eq!(
        view.read_with(cx, |view, _| view.tab_width()),
        2,
        "the tab of the file is two columns wide"
    );

    press(cx, window, "end");
    press(cx, window, "enter");
    assert_eq!(
        buffer_of(cx, &view),
        "alpha {\n\t\n",
        "Enter adds the tab of the project style"
    );
}

#[gpui::test]
fn the_settings_window_names_the_project_style_and_can_turn_it_off(cx: &mut TestAppContext) {
    let root = project_with_a_style("idea-settings");
    let window = open_window(cx, root.clone());
    let path = root.join("src").join("main.txt");
    open_path(cx, window, &path);
    let editor = editor_of(cx, window, &path);

    press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    click(cx, window, "settings-page-code-style");
    draw(cx, window);
    let view = settings_view(cx, window).expect("the Settings window is open");
    assert_eq!(
        view.read_with(cx, |view, _| view.project_style_note()),
        Some("Using code style from .idea/codeStyles/Project.xml".to_owned())
    );

    click(cx, window, "settings-use-arro-style");
    draw(cx, window);
    crate::window_tests::wait_for_signal(cx, "the switch of the project style", |_| {
        root.join(".arro").join("no-project-code-style").is_file()
    });
    draw(cx, window);
    let view = settings_view(cx, window).expect("the Settings window stays open");
    assert_eq!(
        view.read_with(cx, |view, _| view.project_style_note()),
        Some("Using Arro settings".to_owned())
    );
    assert!(
        editor.read_with(cx, |view, _| view.indent_source_name().is_none()),
        "the open file gives up the project style"
    );
    assert_eq!(
        editor.read_with(cx, |view, _| view.tab_width()),
        Settings::default().tab_size as usize,
        "the window tab size applies again"
    );
}

#[gpui::test]
fn the_settings_archive_restores_a_changed_setting_and_keeps_secrets_out(_cx: &mut TestAppContext) {
    let root = scratch("idea-export");
    let settings_dir = root.join("settings");
    fs::create_dir_all(&settings_dir).unwrap();
    let exported = Settings {
        font_size: 15.,
        ..Settings::default()
    };
    exported
        .save(&settings_dir.join(workspace::SETTINGS_FILE))
        .unwrap();
    fs::write(settings_dir.join("keymap.json"), "{\"bindings\": []}").unwrap();
    fs::write(settings_dir.join("templates.json"), "{\"templates\": []}").unwrap();
    fs::write(settings_dir.join("plugins.toml"), "disabled = []\n").unwrap();
    // The state of the project holds the secrets and never belongs in this folder.
    fs::write(
        settings_dir.join("sources.json"),
        "{\"sources\": [{\"password\": \"hunter2\"}]}",
    )
    .unwrap();
    fs::write(
        settings_dir.join("http-client.env.json"),
        "{\"token\": \"top-secret\"}",
    )
    .unwrap();

    let archive = root.join("arro-settings.zip");
    let parts = parts_for(&exported, &settings_dir);
    let manifest = store::settings_archive::export(&archive, &parts).unwrap();
    let names: Vec<&str> = manifest
        .parts
        .iter()
        .map(|part| part.file.as_str())
        .collect();
    assert_eq!(
        names,
        [
            "settings.json",
            "keymap.json",
            "templates.json",
            "plugins.toml"
        ]
    );
    assert!(
        !names.contains(&"sources.json") && !names.contains(&"http-client.env.json"),
        "the secrets of a data source and of an HTTP client stay out: {names:?}"
    );
    let settings_part = manifest
        .parts
        .iter()
        .find(|part| part.id == "settings")
        .unwrap();
    let text = store::settings_archive::read_part(&archive, settings_part).unwrap();
    assert!(!text.contains("password") && !text.contains("hunter2"));

    exported
        .clone()
        .save(&settings_dir.join(workspace::SETTINGS_FILE))
        .unwrap();
    let mut changed = exported.clone();
    changed.font_size = 20.;
    changed
        .save(&settings_dir.join(workspace::SETTINGS_FILE))
        .unwrap();
    let another = root.join("another");
    let written =
        store::settings_archive::import(&archive, &another, &["settings".to_owned()]).unwrap();
    assert_eq!(written, ["settings"]);
    let restored = Settings::load(&another.join(workspace::SETTINGS_FILE));
    assert_eq!(
        restored.font_size, 15.,
        "the imported file wins over the change"
    );
}

#[test]
fn a_secret_field_is_removed_at_any_depth() {
    let mut value = serde_json::json!({
        "font_size": 14,
        "password": "one",
        "http_token": "two",
        "nested": {
            "apiKey": "three",
            "volume": 1,
        }
    });
    without_secrets(&mut value);
    assert_eq!(
        value,
        serde_json::json!({"font_size": 14, "nested": {"volume": 1}})
    );
}

// The test below needs `IDE_CORPUS` and a filled package cache:
//
// `cargo test -p panels window_tests_idea_import -- --ignored --test-threads=1`

#[gpui::test]
#[ignore = "needs IDE_CORPUS and a filled package cache"]
fn reformat_uses_the_project_style_of_the_idea_files(cx: &mut TestAppContext) {
    use std::time::Duration;

    let source = window_language::corpus_dir().join("src/petclinic");
    let root = scratch("idea-real").join("petclinic");
    window_language::prepare_project(&source, &root);
    window_language::write_project_code_style(&root, 2, false);
    let sample = window_language::write_vcs_format_sample(&root);
    let language = window_language::RealLanguage::open(root.clone(), scratch("idea-real-state"))
        .expect("the language server starts");
    let window = crate::window_tests_real::build_window(
        cx,
        root.clone(),
        &language,
        crate::window_tests::project_setup(scratch("idea-real-cache")),
    );
    cx.run_until_parked();
    draw(cx, window);
    crate::window_tests_real::wait_ready(cx, window);
    open_path(cx, window, &sample);
    let view = editor_of(cx, window, &sample);
    crate::window_tests_real::wait_for(cx, "the index of the file", Duration::from_secs(120), {
        let view = view.clone();
        move |cx| view.read_with(cx, |view, _| view.has_syntax_tree())
    });
    let before = buffer_of(cx, &view);
    assert!(
        before.contains("\n    public int first()"),
        "the sample starts with four spaces: {before}"
    );

    press(cx, window, "ctrl-alt-l");
    crate::window_tests_real::wait_for(cx, "the formatting", Duration::from_secs(120), |cx| {
        buffer_of(cx, &view) != before
    });
    let after = buffer_of(cx, &view);
    assert!(
        after.contains("\n  public int first()"),
        "Reformat Code takes the two spaces of the project style: {after}"
    );
    assert!(
        !after.contains("\n    public int first()"),
        "the four spaces of the sample are gone: {after}"
    );

    crate::window_tests_real::close(cx, window, language, false);
}

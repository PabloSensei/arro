//! Settings from another IDE and the ignored masks of the File Types page: the import
//! changes the window and names what it left out, and a new mask hides a file in the
//! project tree.

use std::fs;
use std::path::{Path, PathBuf};

use keymap::Shortcut;
use ui_kit::gpui;
use ui_kit::gpui::TestAppContext;
use workspace::Settings;

use crate::idea_import::{self, Choices};
use crate::project::ActiveProjectPanel;
use crate::window_tests::{
    click, draw, open_window, sample_project, scratch, type_text, wait_for_project_tree,
    wait_for_signal,
};

/// A configuration folder of the other IDE with a font, a keymap and a secret that must
/// stay unread.
fn idea_folder(name: &str) -> PathBuf {
    let root = scratch(name);
    for folder in ["options", "keymaps"] {
        fs::create_dir_all(root.join(folder)).unwrap();
    }
    fs::write(
        root.join("options/editor-font.xml"),
        r#"<application>
  <component name="EditorColorsSchemeImpl">
    <option name="EDITOR_FONT_NAME" value="JetBrains Mono" />
    <option name="EDITOR_FONT_SIZE" value="15" />
    <option name="EDITOR_LINE_SPACING" value="1.2" />
  </component>
</application>"#,
    )
    .unwrap();
    fs::write(
        root.join("keymaps/Mine.xml"),
        r#"<keymap version="1" name="Mine" parent="$default">
  <action id="EditorDuplicate">
    <keyboard-shortcut first-keystroke="ctrl alt K" />
  </action>
  <action id="AnActionArroDoesNotHave">
    <keyboard-shortcut first-keystroke="ctrl alt U" />
  </action>
</keymap>"#,
    )
    .unwrap();
    // The secret files of the other IDE hold bytes that no text reader takes; a reader
    // that opened one would stop the import.
    fs::write(
        root.join("options/security.xml"),
        b"\xff\xfe<password>hunter2</password>",
    )
    .unwrap();
    root
}

#[gpui::test]
fn the_import_changes_the_font_and_a_key_and_names_the_skipped_action(cx: &mut TestAppContext) {
    let project = sample_project("idea-import-project");
    let window = open_window(cx, project);
    let config = idea_folder("idea-import-config");

    let summary = window
        .update(cx, |workspace, _, cx| {
            let prepared = idea_import::read(&config, Choices::default(), workspace.settings_dir())
                .expect("the files of the other IDE are read");
            idea_import::apply(workspace, prepared, cx)
        })
        .unwrap();

    assert!(
        summary
            .iter()
            .any(|line| line.contains("AnActionArroDoesNotHave")),
        "the result names the action Arro does not have: {summary:?}"
    );
    assert!(
        summary.iter().any(|line| line.contains("Editor font")),
        "the result names the font: {summary:?}"
    );
    assert!(
        summary.iter().all(|line| !line.contains("security")),
        "the secret of the other IDE is named nowhere: {summary:?}"
    );
    let font = cx.read(|cx| cx.global::<Settings>().font_size);
    assert_eq!(font, 15., "the imported size is in force");

    let shortcut = Shortcut::parse("ctrl-alt-k").unwrap();
    let before = window
        .read_with(cx, |workspace, _| workspace.keymap_generation())
        .unwrap();
    wait_for_signal(cx, "the imported keymap", |cx| {
        window
            .read_with(cx, |workspace, _| {
                workspace.keymap_generation() > before
                    && workspace
                        .keymap()
                        .actions_on(keymap::editor::CONTEXT, &shortcut)
                        .contains(&keymap::editor::DUPLICATE)
            })
            .unwrap_or(false)
    });
}

fn tree_has_row(cx: &TestAppContext, path: &Path) -> bool {
    cx.read(|cx| {
        cx.global::<ActiveProjectPanel>()
            .0
            .read(cx)
            .tree()
            .index_of(path)
            .is_some()
    })
}

#[gpui::test]
fn a_new_ignored_mask_hides_a_file_in_the_project_tree(cx: &mut TestAppContext) {
    let root = sample_project("idea-file-types");
    let secret = root.join("secret.ignoreme");
    fs::write(&secret, "hidden\n").unwrap();
    let window = open_window(cx, root.clone());
    wait_for_project_tree(cx);
    draw(cx, window);
    assert!(tree_has_row(cx, &secret), "the tree shows the file first");

    crate::window_tests::press(cx, window, "ctrl-alt-s");
    draw(cx, window);
    // The window opens with the caret in the page search: the last page of the list
    // comes to the top without scrolling.
    type_text(cx, window, "file types");
    draw(cx, window);
    click(cx, window, "settings-page-file-types");
    draw(cx, window);
    click(cx, window, "settings-ignore-pattern");
    draw(cx, window);
    type_text(cx, window, "*.ignoreme");
    draw(cx, window);
    click(cx, window, "settings-ignore-add");
    draw(cx, window);
    click(cx, window, "settings-apply");
    draw(cx, window);

    wait_for_signal(cx, "the ignored file to leave the tree", |cx| {
        !tree_has_row(cx, &secret)
    });
    let settings = cx.read(|cx| cx.global::<Settings>().ignored_files.clone());
    assert!(
        settings.iter().any(|pattern| pattern == "*.ignoreme"),
        "the mask is in the settings: {settings:?}"
    );
    let keep = root.join("README.txt");
    assert!(tree_has_row(cx, &keep), "the other files stay");
}

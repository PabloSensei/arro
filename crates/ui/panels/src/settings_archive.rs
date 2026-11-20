//! File > Manage IDE Settings: the export of the window settings into a zip and the
//! import of such an archive with a list of the parts it holds.
//!
//! The archive is written by `store::settings_archive`, which names every part with a
//! plain file name and writes only the parts of the manifest. The settings of the window
//! carry no password, token or key of a data source or of an HTTP client; those live in
//! the state of the project and never reach this folder.

use std::path::{Path, PathBuf};

use serde_json::Value;
use store::settings_archive::{self, ExportPart};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{Settings, Workspace};

/// The name the save dialog offers.
const ARCHIVE_NAME: &str = "arro-settings.zip";
const WIDTH: f32 = 480.;

/// The commands of the File menu.
pub(crate) fn bind_commands(workspace: &mut Workspace, _cx: &mut Context<Workspace>) {
    workspace.register_command("settings.export", "Export Settings...", export);
    workspace.register_command("settings.import", "Import Settings...", import);
    workspace.register_command(
        "settings.restart",
        "Start Arro Again",
        |workspace, _, cx| workspace.restart_project(cx),
    );
}

/// File > Manage IDE Settings > Export Settings...
fn export(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let directory = workspace.settings_dir().to_path_buf();
    let chosen = cx.prompt_for_new_path(&directory, Some(ARCHIVE_NAME));
    let settings = cx.try_global::<Settings>().cloned().unwrap_or_default();
    cx.spawn(async move |workspace, cx| {
        let chosen = chosen.await.ok().and_then(Result::ok).flatten();
        let Some(destination) = chosen else {
            return;
        };
        let settings_dir = directory.clone();
        let target = destination.clone();
        let written = cx
            .background_executor()
            .spawn(async move {
                let parts = parts_for(&settings, &settings_dir);
                settings_archive::export(&target, &parts)
            })
            .await;
        workspace
            .update(cx, |workspace, cx| match written {
                Ok(manifest) => {
                    let text = format!(
                        "{} settings are saved in {}.",
                        manifest.parts.len(),
                        destination.display()
                    );
                    workspace.notify_user("Settings exported", text, cx);
                }
                Err(error) => workspace.notify_user("Cannot export the settings", error, cx),
            })
            .ok();
    })
    .detach();
    let _ = window;
}

/// File > Manage IDE Settings > Import Settings...
fn import(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let weak = cx.weak_entity();
    let settings_dir = workspace.settings_dir().to_path_buf();
    let dialog = cx.new(|cx| ImportSettingsDialog {
        workspace: weak,
        focus: cx.focus_handle(),
        archive: PathBuf::new(),
        settings_dir,
        manifest: settings_archive::Manifest {
            format: String::new(),
            version: 0,
            parts: Vec::new(),
        },
        selected: Vec::new(),
    });
    let focus = dialog.read(cx).focus.clone();
    let paths = cx.prompt_for_paths(PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some("Import Settings".into()),
    });
    cx.spawn_in(window, async move |workspace, cx| {
        let chosen = paths.await.ok().and_then(Result::ok).flatten();
        let Some(archive) = chosen.and_then(|paths| paths.into_iter().next()) else {
            return;
        };
        let read_archive = archive.clone();
        let read = cx
            .background_executor()
            .spawn(async move { settings_archive::read_manifest(&read_archive) });
        let manifest = read.await;
        match manifest {
            Ok(manifest) if manifest.parts.is_empty() => {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.notify_user(
                            "Nothing to import",
                            "The archive holds no part this version knows.",
                            cx,
                        );
                    })
                    .ok();
            }
            Ok(manifest) => {
                let selected = (0..manifest.parts.len()).collect();
                dialog.update(cx, |dialog, _| {
                    dialog.archive = archive;
                    dialog.manifest = manifest;
                    dialog.selected = selected;
                });
                workspace
                    .update_in(cx, |workspace, window, cx| {
                        workspace.show_overlay(dialog.into(), focus, window, cx)
                    })
                    .ok();
            }
            Err(error) => {
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.notify_user("Cannot read the settings archive", error, cx)
                    })
                    .ok();
            }
        }
    })
    .detach();
}

/// The parts of the window settings folder that an export writes. A file that is not
/// there is left out, and a secret field never reaches the settings part.
pub(crate) fn parts_for(settings: &Settings, settings_dir: &Path) -> Vec<ExportPart<'static>> {
    let mut parts = vec![ExportPart {
        id: "settings",
        title: "Settings",
        file: workspace::SETTINGS_FILE,
        text: settings_text(settings),
    }];
    let files = [
        ("keymap", "Keymap", "keymap.json"),
        ("templates", "Live and file templates", "templates.json"),
        ("plugins", "Plugin switches", "plugins.toml"),
    ];
    for (id, title, name) in files {
        if let Ok(text) = std::fs::read_to_string(settings_dir.join(name)) {
            parts.push(ExportPart {
                id,
                title,
                file: name,
                text,
            });
        }
    }
    parts
}

/// The settings as the archive keeps them: the fields of the settings file, without a
/// field whose name says it holds a password, a token or a key.
pub(crate) fn settings_text(settings: &Settings) -> String {
    let mut value = serde_json::to_value(settings).unwrap_or(Value::Null);
    without_secrets(&mut value);
    serde_json::to_string_pretty(&value).unwrap_or_default()
}

/// Removes the fields that hold a secret at any depth of the object.
pub(crate) fn without_secrets(value: &mut Value) {
    match value {
        Value::Object(fields) => {
            fields.retain(|name, _| !is_secret_name(name));
            for field in fields.values_mut() {
                without_secrets(field);
            }
        }
        Value::Array(items) => {
            for item in items {
                without_secrets(item);
            }
        }
        _ => {}
    }
}

fn is_secret_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase().replace(['_', '-'], "");
    [
        "password",
        "passwd",
        "token",
        "secret",
        "apikey",
        "credential",
        "passphrase",
    ]
    .iter()
    .any(|word| name.contains(word))
}

struct ImportSettingsDialog {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    archive: PathBuf,
    settings_dir: PathBuf,
    manifest: settings_archive::Manifest,
    /// The parts the user chose, as indices into the manifest.
    selected: Vec<usize>,
}

impl ImportSettingsDialog {
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        if let Some(at) = self.selected.iter().position(|chosen| *chosen == index) {
            self.selected.remove(at);
        } else {
            self.selected.push(index);
        }
        self.selected.sort_unstable();
        cx.notify();
    }

    fn accept(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selected: Vec<String> = self
            .selected
            .iter()
            .filter_map(|index| self.manifest.parts.get(*index))
            .map(|part| part.id.clone())
            .collect();
        if selected.is_empty() {
            self.close(window, cx);
            return;
        }
        let settings_dir = self.settings_dir.clone();
        let workspace = self.workspace.clone();
        // The manifest is read again with the archive, so the dialog needs no second
        // copy of the path.
        let archive = self.archive.clone();
        let write = cx
            .background_executor()
            .spawn(async move { settings_archive::import(&archive, &settings_dir, &selected) });
        cx.spawn_in(window, async move |this, cx| {
            let written = write.await;
            this.update_in(cx, |this, window, cx| this.close(window, cx))
                .ok();
            workspace
                .update(cx, |workspace, cx| match written {
                    Ok(parts) => {
                        let text = format!(
                            "{} parts are restored. Restart Arro to use them?",
                            parts.len()
                        );
                        workspace.notify_user_with_command(
                            "Settings imported",
                            text,
                            "Restart Arro",
                            "settings.restart",
                            cx,
                        );
                    }
                    Err(error) => workspace.notify_user("Cannot import the settings", error, cx),
                })
                .ok();
        })
        .detach();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => {
                self.accept(window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.close(window, cx);
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for ImportSettingsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let left = (window.viewport_size().width - px(WIDTH)) / 2.;
        let top = window.viewport_size().height * 0.2;
        let rows = self
            .manifest
            .parts
            .iter()
            .enumerate()
            .map(|(index, part)| {
                let chosen = self.selected.contains(&index);
                let id = SharedString::from(format!("settings-import-{}", part.id));
                div()
                    .id(id.clone())
                    .debug_selector(move || format!("settings-import-{}", part.id))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .hover(|style| style.bg(theme.selection_hover))
                    .child(checkbox(chosen, &theme))
                    .child(part.title.clone())
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle(index, cx)))
            })
            .collect::<Vec<_>>();
        let shell = surface(&theme)
            .id("settings-import")
            .debug_selector(|| "settings-import".to_owned())
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .child(
                div().flex().flex_col().gap(px(SPACE_XS)).child(
                    div()
                        .text_color(theme.text_secondary)
                        .child("Choose the parts to restore."),
                ),
            )
            .child(div().flex().flex_col().children(rows))
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .id("settings-import-cancel")
                            .debug_selector(|| "settings-import-cancel".to_owned())
                            .px(px(SPACE_L))
                            .h(px(CONTROL_HEIGHT))
                            .flex()
                            .items_center()
                            .rounded(px(CONTROL_RADIUS))
                            .hover(|style| style.bg(theme.selection_hover))
                            .child("Cancel")
                            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .child(
                        div()
                            .id("settings-import-run")
                            .debug_selector(|| "settings-import-run".to_owned())
                            .px(px(SPACE_L))
                            .h(px(CONTROL_HEIGHT))
                            .flex()
                            .items_center()
                            .rounded(px(CONTROL_RADIUS))
                            .bg(theme.accent)
                            .text_color(theme.over_accent)
                            .child("Import")
                            .on_click(cx.listener(|this, _, window, cx| this.accept(window, cx))),
                    ),
            );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "settings-import",
                title: "Import Settings".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(px(WIDTH)),
                    height: None,
                },
                min: (360., 240.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

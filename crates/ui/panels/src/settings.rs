//! The Settings window, Ctrl+Alt+S: what changes the editor and the window, the plugins
//! with a switch each, and the keys to look at. Every change applies at once and is written
//! to the settings file in the background.

use std::path::{Path, PathBuf};

use plugin_host::{Manifest, PluginSettings, default_plugins_dir};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{Settings, Workspace};

use crate::project_structure::{ProjectEnvironmentGlobal, ProjectStructureDialog};
use crate::prompt::TextField;
use crate::settings_catalog::{self, Setting, SettingField, SettingKind, SettingsPage};
use crate::settings_keymap::KeymapPage;

const WIDTH: f32 = 800.;

/// A plugin as the list shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginRow {
    /// The folder name, which is what the switch file lists.
    pub id: String,
    pub title: String,
    pub enabled: bool,
}

/// Reads the plugin folders: the manifest names the plugin, a broken one says so.
/// Blocks: not for the UI thread.
pub fn read_plugins(dir: &Path, settings: &PluginSettings) -> Vec<PluginRow> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut folders: Vec<PathBuf> = entries
        .flatten()
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    folders.sort();
    folders
        .into_iter()
        .map(|folder| {
            let id = folder.file_name().unwrap_or_default().to_string_lossy();
            let id = id.into_owned();
            let title = match Manifest::read(&folder) {
                Ok(manifest) => format!("{} {}", manifest.name, manifest.version),
                Err(error) => format!("{id} (broken: {})", error.0),
            };
            let enabled = !settings.disabled.contains(&id);
            PluginRow { id, title, enabled }
        })
        .collect()
}

/// Every action with a name and its shortcuts, for the list of keys.
#[allow(dead_code)]
pub fn shortcut_list() -> Vec<(String, String)> {
    let (_, keymap) = keymap::default_keymap();
    let tables = [keymap::editor::IDEA_IDS, keymap::workspace::IDEA_IDS];
    let mut rows = Vec::new();
    for table in tables {
        for (name, action) in table {
            let keys: Vec<String> = keymap
                .shortcuts_for(action)
                .iter()
                .map(ToString::to_string)
                .collect();
            rows.push((name.to_string(), keys.join(", ")));
        }
    }
    rows.sort();
    rows.dedup();
    rows
}

/// Rows whose name or keys hold every word of `query`, case ignored.
#[allow(dead_code)]
pub fn filter_shortcuts<'a>(
    rows: &'a [(String, String)],
    query: &str,
) -> Vec<&'a (String, String)> {
    let words: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    rows.iter()
        .filter(|(name, keys)| {
            let text = format!("{name} {keys}").to_lowercase();
            words.iter().all(|word| text.contains(word))
        })
        .collect()
}

/// Ctrl+Alt+S shows the window.
pub fn bind_actions(workspace: WeakEntity<Workspace>, window: AnyWindowHandle, cx: &mut App) {
    cx.on_action(move |_: &workspace::ShowSettings, cx| {
        let workspace = workspace.clone();
        // The window is being updated while its action runs, so the rest waits.
        cx.defer(move |cx| {
            let show = move |_: AnyView, window: &mut Window, cx: &mut App| {
                let open = |this: &mut Workspace, cx: &mut Context<Workspace>| {
                    SettingsView::show(this, window, cx)
                };
                workspace.update(cx, open).ok();
            };
            window.update(cx, show).ok();
        });
    });
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Editing {
    FontFamily,
}

pub struct SettingsView {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    editing: Option<Editing>,
    font_family: TextField,
    plugins: Vec<PluginRow>,
    plugin_file: PathBuf,
    pub(crate) keymap_page: KeymapPage,
    page: SettingsPage,
    /// The row the window came for, drawn as chosen while the window is open.
    highlight: Option<&'static str>,
    /// The name of the SDK of the project, empty until the environment is read.
    sdk: String,
    scroll_pages: ScrollHandle,
    scroll_content: ScrollHandle,
}

impl SettingsView {
    pub fn show(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        Self::show_at(workspace, window, cx, None);
    }

    /// Shows the window on the page of `target`, with that row marked.
    pub fn show_at(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
        target: Option<&'static str>,
    ) {
        let weak = cx.weak_entity();
        let settings_dir = workspace.settings_dir().to_path_buf();
        let settings = cx.global::<Settings>().clone();
        let font_family = settings.editor_font_family.clone();
        let page = target
            .and_then(settings_catalog::find)
            .map_or(SettingsPage::Appearance, |setting| setting.page);
        // The same folder and switch file the plugin host loads from.
        let folder = default_plugins_dir().unwrap_or_else(|| settings_dir.join("plugins"));
        let plugin_file = folder
            .parent()
            .unwrap_or(&folder)
            .join(crate::plugins::SETTINGS_FILE);
        let environment = cx
            .try_global::<ProjectEnvironmentGlobal>()
            .and_then(|global| global.0.clone());
        let keymap_page = KeymapPage::new(weak.clone());
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            editing: None,
            font_family: TextField::from_text(font_family),
            plugins: Vec::new(),
            plugin_file: plugin_file.clone(),
            keymap_page,
            page,
            highlight: target,
            sdk: "not set".to_owned(),
            scroll_pages: ScrollHandle::new(),
            scroll_content: ScrollHandle::new(),
        });
        if let Some(environment) = environment {
            let read = cx
                .background_executor()
                .spawn(async move { environment.describe() });
            let list = view.downgrade();
            cx.spawn(async move |_, cx| {
                let info = read.await;
                list.update(cx, |view, cx| {
                    view.sdk = info
                        .selected_runtime()
                        .map(|runtime| runtime.name.clone())
                        .unwrap_or_else(|| "not set".to_owned());
                    cx.notify();
                })
                .ok();
            })
            .detach();
        }
        let file = plugin_file;
        let read = cx.background_executor().spawn(async move {
            let switches = PluginSettings::load(&file).unwrap_or_default();
            read_plugins(&folder, &switches)
        });
        let list = view.downgrade();
        cx.spawn(async move |_, cx| {
            let plugins = read.await;
            list.update(cx, |view, cx| {
                view.plugins = plugins;
                cx.notify();
            })
            .ok();
        })
        .detach();
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn change(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        let apply = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
            workspace.update_settings(change, cx)
        };
        self.workspace.update(cx, apply).ok();
        cx.notify();
    }

    fn toggle_plugin(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.plugins.get_mut(index) else {
            return;
        };
        row.enabled = !row.enabled;
        let mut switches = PluginSettings::default();
        let off = self.plugins.iter().filter(|row| !row.enabled);
        switches.disabled = off.map(|row| row.id.clone()).collect();
        let file = self.plugin_file.clone();
        let write = cx
            .background_executor()
            .spawn(async move { switches.save(&file) });
        cx.spawn(async move |_, _| {
            if let Err(error) = write.await {
                eprintln!("plugin switches are not saved: {error}");
            }
        })
        .detach();
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        match self.editing {
            Some(Editing::FontFamily) => {
                if self.font_family.key_with_clipboard(event, false, cx) {
                    let text = self.font_family.text.clone();
                    self.change(
                        |settings| {
                            settings_catalog::set_text(settings, "editor_font_family", &text);
                        },
                        cx,
                    );
                    cx.stop_propagation();
                }
            }
            None => {
                if self.page == SettingsPage::Keymap && self.keymap_page.key(event, cx) {
                    cx.stop_propagation();
                }
            }
        }
    }
}

fn pill(id: impl Into<ElementId>, text: &str, on: bool, theme: &Theme) -> Stateful<Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .flex()
        .items_center()
        .justify_center()
        .min_w(px(54.))
        .h(px(24.))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .when(on, |this| {
            this.bg(theme.accent).text_color(theme.over_accent)
        })
        .when(!on, |this| {
            this.bg(theme.pressed).hover(move |style| style.bg(hover))
        })
        .child(text.to_string())
}

fn field(id: &'static str, text: &str, hint: &str, active: bool, theme: &Theme) -> Stateful<Div> {
    let shown = if text.is_empty() { hint } else { text };
    let color = if text.is_empty() {
        theme.text_secondary
    } else {
        theme.text
    };
    div()
        .id(id)
        .flex()
        .items_center()
        .flex_1()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_2()
        .border_color(if active { theme.accent } else { theme.border })
        .text_color(color)
        .child(shown.to_string())
}

fn row(label: &'static str, control: impl IntoElement, theme: &Theme) -> Div {
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap(px(DIALOG_GAP))
        .min_h(px(30.))
        .child(div().text_color(theme.text).child(label))
        .child(control)
}

fn heading(text: &'static str, theme: &Theme) -> Div {
    div()
        .mt(px(DIALOG_GAP))
        .pb(px(SPACE_S))
        .border_b_1()
        .border_color(theme.border)
        .font_weight(FontWeight::MEDIUM)
        .child(text)
}

fn setting_row(
    entry: &Setting,
    control: impl IntoElement,
    highlighted: bool,
    theme: &Theme,
) -> Div {
    row(entry.title, control, theme)
        .px(px(ROW_PADDING_X))
        .rounded(px(CONTROL_RADIUS))
        .when(highlighted, |this| this.bg(theme.selection))
}

fn toggle_button(
    id: &'static str,
    on: bool,
    theme: &Theme,
    cx: &mut Context<SettingsView>,
) -> Stateful<Div> {
    let text = if on { "On" } else { "Off" };
    pill(id, text, on, theme)
        .debug_selector(move || format!("settings-{}", id.replace('_', "-")))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.change(
                |settings| {
                    settings_catalog::toggle(settings, id);
                },
                cx,
            )
        }))
}

/// The id of a step button: the size keeps its old `settings-font-up`, the others name
/// the setting they change.
fn number_selector(id: &'static str, up: bool) -> String {
    if id == "font_size" {
        format!("settings-font-{}", if up { "up" } else { "down" })
    } else {
        format!(
            "settings-{}-{}",
            id.replace('_', "-"),
            if up { "up" } else { "down" }
        )
    }
}

fn number_button(
    setting: &'static Setting,
    up: bool,
    delta: f32,
    theme: &Theme,
    cx: &mut Context<SettingsView>,
) -> Stateful<Div> {
    let id = setting.id;
    let icon = if up { "general/add" } else { "general/minus" };
    let label = if up { "Increase" } else { "Decrease" };
    let selector = number_selector(id, up);
    icon_button(
        SharedString::from(selector.clone()),
        icon,
        label,
        None,
        theme,
    )
    .debug_selector(move || selector.clone())
    .on_click(cx.listener(move |this, _, _, cx| {
        this.change(
            |settings| {
                settings_catalog::adjust_number(settings, id, delta);
            },
            cx,
        )
    }))
}

fn project_sdk_row(sdk: &str, theme: &Theme, cx: &mut Context<SettingsView>) -> Stateful<Div> {
    div()
        .id("settings-project-sdk")
        .debug_selector(|| "settings-project-sdk".to_owned())
        .flex()
        .items_center()
        .justify_between()
        .gap(px(DIALOG_GAP))
        .min_h(px(30.))
        .px(px(ROW_PADDING_X))
        .rounded(px(CONTROL_RADIUS))
        .cursor_pointer()
        .hover({
            let hover = theme.hover;
            move |style| style.bg(hover)
        })
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_XS))
                .child(
                    div()
                        .text_color(theme.text)
                        .child(format!("Project SDK: {sdk}")),
                )
                .child(
                    div()
                        .text_color(theme.text_secondary)
                        .child("— Project Structure..."),
                ),
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.workspace
                .update(cx, |workspace, cx| {
                    ProjectStructureDialog::show(workspace, window, cx)
                })
                .ok();
        }))
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let settings = cx.global::<Settings>().clone();
        let width = px(WIDTH).min(window.viewport_size().width - px(2. * DIALOG_PADDING));
        let left = (window.viewport_size().width - width) / 2.;
        let top = window.viewport_size().height * 0.08;
        let height = window.viewport_size().height * 0.8;

        let on_off = |on: bool| if on { "On" } else { "Off" };
        let pages: Vec<_> = SettingsPage::ALL
            .iter()
            .enumerate()
            .map(|(index, page)| {
                let page = *page;
                let chosen = page == self.page;
                div()
                    .id(("settings-page", index))
                    .debug_selector(move || {
                        format!(
                            "settings-page-{}",
                            page.title().to_lowercase().replace(' ', "-")
                        )
                    })
                    .flex()
                    .items_center()
                    .h(px(26.))
                    .pl(px(ROW_PADDING_X + page.depth() as f32 * SPACE_L))
                    .pr(px(ROW_PADDING_X))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(if chosen {
                        theme.tab_selected_border
                    } else {
                        theme.popup_bg
                    })
                    .when(chosen, |this| this.bg(theme.tab_selected))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.page = page;
                        this.highlight = None;
                        this.editing = None;
                        cx.notify();
                    }))
                    .child(page.title())
            })
            .collect();

        let mut content: Vec<Div> = Vec::new();
        match self.page {
            SettingsPage::Plugins => {
                content.extend(self.plugins.iter().enumerate().map(|(index, plugin)| {
                    let switch = pill(
                        ("plugin", index),
                        on_off(plugin.enabled),
                        plugin.enabled,
                        &theme,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_plugin(index, cx)));
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .min_h(px(28.))
                        .child(plugin.title.clone())
                        .child(switch)
                }));
                if content.is_empty() {
                    content.push(
                        div()
                            .py(px(SPACE_S))
                            .text_color(theme.text_secondary)
                            .child("No plugins in the plugins folder."),
                    );
                }
            }
            SettingsPage::Keymap => {
                content.push(self.keymap_page.render(window, cx));
            }
            _ => {
                if self.page == SettingsPage::Project {
                    content.push(div().child(project_sdk_row(&self.sdk, &theme, cx)));
                }
                for entry in settings_catalog::all()
                    .iter()
                    .filter(|entry| entry.page == self.page)
                {
                    let control = match entry.kind {
                        SettingKind::Choice => {
                            if entry.field == SettingField::Theme {
                                let dark = theme.dark;
                                let label = if dark { "Dark" } else { "Light" };
                                pill("theme", label, true, &theme)
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.workspace
                                            .update(cx, |workspace, cx| workspace.toggle_theme(cx))
                                            .ok();
                                    }))
                                    .into_any_element()
                            } else {
                                let id = entry.id;
                                let label = settings_catalog::choice_label(&settings, id)
                                    .unwrap_or_default();
                                pill(id, label, true, &theme)
                                    .debug_selector(move || {
                                        format!("settings-{}", id.replace('_', "-"))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.change(
                                            |settings| {
                                                settings_catalog::next_choice(settings, id);
                                            },
                                            cx,
                                        )
                                    }))
                                    .into_any_element()
                            }
                        }
                        SettingKind::Number => {
                            let step = match entry.field {
                                SettingField::UiScale | SettingField::LineSpacing => 0.1,
                                SettingField::CompletionDelay => {
                                    workspace::COMPLETION_DELAY_STEP_MS as f32
                                }
                                _ => 1.,
                            };
                            let text = settings_catalog::number_label(&settings, entry.id)
                                .unwrap_or_default();
                            div()
                                .flex()
                                .items_center()
                                .gap(px(SPACE_S))
                                .child(number_button(entry, false, -step, &theme, cx))
                                .child(div().min_w(px(48.)).flex().justify_center().child(text))
                                .child(number_button(entry, true, step, &theme, cx))
                                .into_any_element()
                        }
                        SettingKind::Text => field(
                            entry.id,
                            &self.font_family.text,
                            "JetBrains Mono",
                            self.editing == Some(Editing::FontFamily),
                            &theme,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.editing = Some(Editing::FontFamily);
                            cx.notify();
                        }))
                        .into_any_element(),
                        SettingKind::Toggle => {
                            let on = settings_catalog::value(&settings, entry.id).unwrap_or(false);
                            toggle_button(entry.id, on, &theme, cx).into_any_element()
                        }
                    };
                    let highlighted = self.highlight == Some(entry.id);
                    content.push(setting_row(entry, control, highlighted, &theme));
                }
            }
        }

        let pages = scrollable(
            "settings-pages",
            &self.scroll_pages,
            div()
                .id("settings-pages")
                .flex()
                .flex_col()
                .gap(px(2.))
                .overflow_y_scroll()
                .track_scroll(&self.scroll_pages)
                .children(pages),
            &theme,
            window,
            cx,
        )
        .w(px(160.))
        .flex_none()
        .h_full();
        let content = scrollable(
            "settings-content",
            &self.scroll_content,
            div()
                .id("settings-content")
                .flex()
                .flex_col()
                .gap(px(SPACE_XS))
                .overflow_y_scroll()
                .track_scroll(&self.scroll_content)
                .child(heading(self.page.title(), &theme))
                .children(content),
            &theme,
            window,
            cx,
        )
        .flex_1()
        .min_w_0()
        .h_full();
        let section = div()
            .flex()
            .items_start()
            .flex_1()
            .min_h_0()
            .gap(px(DIALOG_GAP))
            .child(pages)
            .child(content);

        let body = surface(&theme)
            .id("settings")
            .debug_selector(|| "settings".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .h(height)
            .p(px(DIALOG_PADDING))
            .gap(px(SPACE_XS))
            .on_key_down(cx.listener(|this, event, _, cx| this.key(event, cx)))
            .child(
                div()
                    .debug_selector(|| "settings-title".to_owned())
                    .text_size(px(TEXT_TITLE))
                    .font_weight(FontWeight::MEDIUM)
                    .child("Settings"),
            )
            .child(section);
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("settings-enter", body))
    }
}

#[cfg(test)]
mod tests {
    use plugin_host::PluginSettings;

    use super::{PluginRow, filter_shortcuts, read_plugins};

    #[test]
    fn plugins_are_listed_by_folder_with_their_switch() {
        let dir = std::env::temp_dir().join(format!("settings-plugins-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("good")).unwrap();
        std::fs::create_dir_all(dir.join("broken")).unwrap();
        std::fs::write(
            dir.join("good/plugin.toml"),
            "id = \"good\"\nname = \"Good\"\nversion = \"1.0\"\napi = 1\n",
        )
        .unwrap();
        let mut switches = PluginSettings::default();
        switches.disabled.insert("good".into());

        let rows = read_plugins(&dir, &switches);
        std::fs::remove_dir_all(&dir).unwrap();

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].id, "broken");
        assert!(rows[0].title.contains("broken"));
        assert_eq!(
            rows[1],
            PluginRow {
                id: "good".into(),
                title: "Good 1.0".into(),
                enabled: false
            }
        );
        assert!(
            read_plugins(&dir, &switches).is_empty(),
            "a missing folder is no plugins"
        );
    }

    #[test]
    fn the_key_search_wants_every_word_in_the_name_or_the_keys() {
        let rows = vec![
            ("Copy".to_string(), "ctrl-c".to_string()),
            ("Paste".to_string(), "ctrl-v".to_string()),
            ("Save All".to_string(), "ctrl-s".to_string()),
        ];
        assert_eq!(filter_shortcuts(&rows, "").len(), 3);
        assert_eq!(filter_shortcuts(&rows, "ctrl V")[0].0, "Paste");
        assert_eq!(filter_shortcuts(&rows, "save ctrl").len(), 1);
        assert!(filter_shortcuts(&rows, "zzz").is_empty());
    }

    #[test]
    fn the_default_keymap_has_a_shortcut_list() {
        let rows = super::shortcut_list();
        assert!(rows.len() > 20);
        assert!(rows.iter().any(|(_, keys)| !keys.is_empty()));
    }
}

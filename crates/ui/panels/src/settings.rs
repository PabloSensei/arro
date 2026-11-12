//! The Settings window, Ctrl+Alt+S: what changes the editor and the window, the plugins
//! with a switch each, and the keys to look at. Every change applies at once and is written
//! to the settings file in the background.

use std::path::{Path, PathBuf};

use plugin_host::{Manifest, PluginSettings, default_plugins_dir};
use project_model::RuntimeInfo;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{Settings, Workspace};

use crate::project_structure::{ProjectEnvironmentGlobal, ProjectStructureDialog};
use crate::prompt::TextField;
use crate::settings_catalog::{self, Setting, SettingField, SettingKind, SettingsPage};
use crate::settings_keymap::KeymapPage;
use crate::settings_templates::TemplateSection;

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

/// The settings helpers of the window that read and write files outside the UI thread.
fn home_folder() -> PathBuf {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .unwrap_or_default()
}

/// The settings file the first build tool reads when the user names none.
pub(crate) fn default_settings_file() -> PathBuf {
    home_folder().join(".m2").join("settings.xml")
}

/// The repository a settings file names, or the default one of the first build tool.
/// Blocks on the disk: not for the UI thread.
fn local_repository(settings_file: &Path) -> String {
    let chosen = std::fs::read_to_string(settings_file)
        .ok()
        .and_then(|text| {
            let start = text.find("<localRepository>")? + "<localRepository>".len();
            let end = text[start..].find("</localRepository>")?;
            Some(text[start..start + end].trim().to_owned())
        })
        .filter(|path| !path.is_empty());
    chosen.unwrap_or_else(|| {
        home_folder()
            .join(".m2")
            .join("repository")
            .display()
            .to_string()
    })
}

pub struct SettingsView {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    editing: Option<&'static str>,
    text_field: TextField,
    plugins: Vec<PluginRow>,
    plugin_file: PathBuf,
    pub(crate) keymap_page: KeymapPage,
    pub(crate) templates_page: crate::settings_templates::TemplatesPage,
    page: SettingsPage,
    /// The row the window came for, drawn as chosen while the window is open.
    highlight: Option<&'static str>,
    /// The name of the SDK of the project, empty until the environment is read.
    sdk: String,
    /// The SDKs the pages offer where a choice among them is needed.
    runtimes: Vec<RuntimeInfo>,
    /// The local repository the settings file names, read in the background.
    repository: String,
    /// What the Test button of the Git page found.
    git_version: Option<String>,
    testing_git: bool,
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
        let repository = settings
            .build_tools
            .first
            .settings_path()
            .map_or_else(default_settings_file, PathBuf::from);
        let templates_page = crate::settings_templates::page(weak.clone());
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            editing: None,
            text_field: TextField::from_text(font_family),
            plugins: Vec::new(),
            plugin_file: plugin_file.clone(),
            keymap_page,
            templates_page,
            page,
            highlight: target,
            sdk: "not set".to_owned(),
            runtimes: Vec::new(),
            repository: String::new(),
            git_version: None,
            testing_git: false,
            scroll_pages: ScrollHandle::new(),
            scroll_content: ScrollHandle::new(),
        });
        let read_repository = view.clone();
        let repository_file = repository.clone();
        let repository = cx
            .background_executor()
            .spawn(async move { local_repository(&repository_file) });
        cx.spawn(async move |_, cx| {
            let repository = repository.await;
            read_repository.update(cx, |view, cx| {
                view.repository = repository;
                cx.notify();
            });
        })
        .detach();
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
                    view.runtimes = info.runtimes;
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
        if let Some(id) = self.editing {
            if self.text_field.key_with_clipboard(event, false, cx) {
                let text = self.text_field.text.clone();
                self.change(
                    |settings| {
                        settings_catalog::set_text(settings, id, &text);
                    },
                    cx,
                );
                if id == "build_tool_first_settings_file" {
                    self.refresh_repository(cx);
                }
                cx.stop_propagation();
            }
            return;
        }
        let keymap = self.page == SettingsPage::Keymap && self.keymap_page.key(event, cx);
        let templates =
            TemplateSection::section(self.page).is_some() && self.templates_page.key(event, cx);
        if keymap || templates {
            cx.stop_propagation();
        }
    }

    /// Reads the local repository of the settings file again, off the UI thread.
    fn refresh_repository(&self, cx: &mut Context<Self>) {
        let file = cx
            .try_global::<Settings>()
            .and_then(|settings| {
                settings
                    .build_tools
                    .first
                    .settings_path()
                    .map(PathBuf::from)
            })
            .unwrap_or_else(default_settings_file);
        let read = cx
            .background_executor()
            .spawn(async move { local_repository(&file) });
        let view = cx.weak_entity();
        cx.spawn(async move |_, cx| {
            let repository = read.await;
            view.update(cx, |view, cx| {
                view.repository = repository;
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Runs `--version` of the configured Git in the background, so a path that is not
    /// there shows its error instead of blocking the window.
    fn test_git(&mut self, cx: &mut Context<Self>) {
        if self.testing_git {
            return;
        }
        let configured = cx
            .try_global::<Settings>()
            .map(|settings| settings.git_path.trim().to_owned())
            .unwrap_or_default();
        self.testing_git = true;
        self.git_version = None;
        cx.notify();
        let run = cx.background_executor().spawn(async move {
            let git = if configured.is_empty() {
                vcs::GitRunner::from_env()
            } else {
                Some(vcs::GitRunner::new(PathBuf::from(configured)))
            };
            let Some(git) = git else {
                return "Git is not on PATH".to_owned();
            };
            match git.run(Path::new("."), ["--version"]) {
                Ok(output) => String::from_utf8_lossy(&output).trim().to_owned(),
                Err(error) => error.to_string(),
            }
        });
        cx.spawn(async move |this, cx| {
            let version = run.await;
            this.update(cx, |this, cx| {
                this.testing_git = false;
                this.git_version = Some(version);
                cx.notify();
            })
            .ok();
        })
        .detach();
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

/// The grey text of an empty text field: the value applied when the field stays empty.
fn text_hint(field: SettingField) -> &'static str {
    match field {
        SettingField::EditorFontFamily => "JetBrains Mono",
        SettingField::BuildToolFirstHome => "Wrapper or bundled",
        SettingField::BuildToolFirstSettingsFile => "~/.m2/settings.xml",
        SettingField::BuildToolFirstVmOptions => "-Xmx2g",
        SettingField::BuildToolSecondHome => "Wrapper",
        SettingField::BuildToolSecondUserHome => "Default",
        SettingField::GitPath => "Found on PATH",
        SettingField::TerminalShell => "Detected automatically",
        SettingField::TerminalStartDirectory => "Project root",
        SettingField::TerminalEnvironment => "NAME=value per line",
        _ => "",
    }
}

/// The JVM of the second build tool as one choice among the SDKs of the project.
fn runtime_pill(
    entry: &'static Setting,
    settings: &Settings,
    runtimes: &[RuntimeInfo],
    theme: &Theme,
    cx: &mut Context<SettingsView>,
) -> Stateful<Div> {
    let chosen = &settings.build_tools.second.runtime_home;
    let label = if chosen.trim().is_empty() {
        "Project SDK".to_owned()
    } else {
        runtimes
            .iter()
            .find(|runtime| runtime.home.display().to_string() == *chosen)
            .map(|runtime| runtime.name.clone())
            .unwrap_or_else(|| chosen.clone())
    };
    let id = entry.id;
    pill(id, &label, true, theme)
        .debug_selector(move || format!("settings-{}", id.replace('_', "-")))
        .on_click(cx.listener(move |this, _, _, cx| {
            let settings = cx.global::<Settings>().clone();
            let current = settings.build_tools.second.runtime_home.clone();
            let mut homes = vec![String::new()];
            homes.extend(
                this.runtimes
                    .iter()
                    .map(|runtime| runtime.home.display().to_string()),
            );
            let next = homes
                .iter()
                .position(|home| home == &current)
                .map_or(0, |index| (index + 1) % homes.len());
            let next = homes[next].clone();
            this.change(
                |settings| {
                    settings_catalog::set_text(settings, id, &next);
                },
                cx,
            );
        }))
}

/// The Test button of the Git page and the line it brings back.
fn git_test_row(
    version: &Option<String>,
    testing: bool,
    theme: &Theme,
    cx: &mut Context<SettingsView>,
) -> AnyElement {
    let (hover, pressed) = (theme.hover, theme.pressed);
    let label = if testing { "Testing…" } else { "Test" };
    let button = div()
        .id("settings-git-test")
        .debug_selector(|| "settings-git-test".to_owned())
        .flex()
        .items_center()
        .justify_center()
        .flex_none()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.control_border)
        .text_color(theme.text)
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .child(label)
        .on_click(cx.listener(|this, _, _, cx| this.test_git(cx)));
    let found = version.clone().map(|version| {
        div()
            .debug_selector(|| "settings-git-version".to_owned())
            .text_size(px(TEXT_SMALL))
            .text_color(theme.text_secondary)
            .child(version)
    });
    div()
        .flex()
        .items_center()
        .gap(px(SPACE_S))
        .flex_none()
        .child(button)
        .children(found)
        .into_any_element()
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
                        this.page = page.target();
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
            SettingsPage::EditorLiveTemplates => {
                content.push(
                    self.templates_page
                        .render(TemplateSection::Live, window, cx),
                );
            }
            SettingsPage::EditorPostfix => {
                content.push(
                    self.templates_page
                        .render(TemplateSection::Postfix, window, cx),
                );
            }
            SettingsPage::EditorFileTemplates => {
                content.push(
                    self.templates_page
                        .render(TemplateSection::Files, window, cx),
                );
            }
            _ => {
                if self.page == SettingsPage::Project {
                    content.push(div().child(project_sdk_row(&self.sdk, &theme, cx)));
                }
                let mut last_section: Option<&'static str> = None;
                for entry in settings_catalog::all()
                    .iter()
                    .filter(|entry| entry.page == self.page)
                {
                    let section = settings_catalog::section_of(entry.id);
                    if let Some(section) = section
                        && section != last_section.unwrap_or_default()
                    {
                        content.push(heading(section, &theme));
                        last_section = Some(section);
                    }
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
                        SettingKind::Text => {
                            if entry.field == SettingField::BuildToolSecondRuntimeHome {
                                runtime_pill(entry, &settings, &self.runtimes, &theme, cx)
                                    .into_any_element()
                            } else if entry.field == SettingField::BuildToolFirstRepository {
                                field(
                                    entry.id,
                                    &self.repository,
                                    "Read from the settings file",
                                    false,
                                    &theme,
                                )
                                .into_any_element()
                            } else {
                                let id = entry.id;
                                let value =
                                    settings_catalog::text(&settings, id).unwrap_or_default();
                                let hint = text_hint(entry.field);
                                let active = self.editing == Some(id);
                                let start = value.clone();
                                let test = if entry.field == SettingField::GitPath {
                                    Some(git_test_row(
                                        &self.git_version,
                                        self.testing_git,
                                        &theme,
                                        cx,
                                    ))
                                } else {
                                    None
                                };
                                let input = field(id, &value, hint, active, &theme)
                                    .debug_selector(move || {
                                        format!("settings-{}", id.replace('_', "-"))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.editing = Some(id);
                                        this.text_field = TextField::from_text(start.clone());
                                        cx.notify();
                                    }));
                                match test {
                                    Some(test) => div()
                                        .flex()
                                        .items_center()
                                        .gap(px(SPACE_S))
                                        .flex_1()
                                        .child(input)
                                        .child(test)
                                        .into_any_element(),
                                    None => input.into_any_element(),
                                }
                            }
                        }
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
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            )
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

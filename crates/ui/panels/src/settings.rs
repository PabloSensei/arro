//! The Settings window, Ctrl+Alt+S: a titled window with a close button, a search over the
//! pages, and OK, Cancel and Apply. The pages of the settings file keep a draft until
//! Apply or OK; Theme, Keymap, the templates and the Project SDK row apply at once, as
//! their own windows and files do. The window opens with the caret in the search field,
//! and a click on a page or a setting takes it out until the field is clicked again.

use std::path::{Path, PathBuf};

use editor_view::Semantic;
use plugin_host::{Manifest, PluginSettings, default_plugins_dir};
use project_model::RuntimeInfo;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{McpToolChoice, Settings, Workspace};

use crate::project_structure::{ProjectEnvironmentGlobal, ProjectStructureDialog, dialog_button};
use crate::prompt::TextField;
use crate::settings_catalog::{self, Setting, SettingField, SettingKind, SettingsPage};
use crate::settings_keymap::KeymapPage;
use crate::settings_templates::TemplateSection;

const WIDTH: f32 = 800.;
const MIN_WIDTH: f32 = 640.;
const MIN_HEIGHT: f32 = 420.;
/// The grab area of the edge and the corner that change the size.
const RESIZE_GRIP: f32 = 8.;

/// The home folder the MCP client rows read, for a test that keeps its own: the profile of
/// the user of the machine is neither read nor written there.
#[cfg(test)]
#[derive(Clone, Debug)]
pub(crate) struct TestMcpHome(pub PathBuf);

#[cfg(test)]
impl Global for TestMcpHome {}

/// Where the Settings window sits and how large it is, for this session only.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SettingsWindowPlacement {
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
}

impl Global for SettingsWindowPlacement {}

#[derive(Clone, Copy)]
struct PlacementDrag {
    from: Point<Pixels>,
    placement: SettingsWindowPlacement,
}

#[derive(Clone, Copy)]
enum ResizeEdge {
    Right,
    Bottom,
    Corner,
}

#[derive(Clone, Copy)]
struct PlacementResize {
    from: Point<Pixels>,
    placement: SettingsWindowPlacement,
    edge: ResizeEdge,
}

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

/// The home folder the MCP client rows read: the profile of the user, or the one a test
/// set for itself.
fn mcp_home_folder(cx: &App) -> PathBuf {
    #[cfg(not(test))]
    let _ = cx;
    #[cfg(test)]
    if let Some(home) = cx.try_global::<TestMcpHome>() {
        return home.0.clone();
    }
    mcp_discovery::home_folder()
}

/// The address and the token of the MCP server of the project while the window publishes
/// them.
fn mcp_endpoint(cx: &App) -> Option<(String, String)> {
    let panel = cx.try_global::<crate::agents::ActiveAgents>()?.0.clone();
    panel.read(cx).mcp.as_ref().and_then(|mcp| mcp.endpoint())
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
    /// The settings the window shows and edits; nothing of it reaches the window until
    /// Apply or OK.
    draft: Settings,
    /// The settings in force when the draft was last applied, to tell the changed leaves.
    saved: Settings,
    plugins: Vec<PluginRow>,
    /// The switches as they were last applied, to write the file only for a change.
    saved_plugins: Vec<PluginRow>,
    plugin_file: PathBuf,
    pub(crate) keymap_page: KeymapPage,
    pub(crate) templates_page: crate::settings_templates::TemplatesPage,
    page: SettingsPage,
    /// The row the window came for, drawn as chosen while the window is open.
    highlight: Option<&'static str>,
    /// The checks of the language, read when the window opens.
    inspections: Vec<lang_api::InspectionInfo>,
    /// The choices of the user for the checks, as the draft of this window.
    inspection_settings: lang_api::InspectionSettings,
    saved_inspections: lang_api::InspectionSettings,
    /// The check the window came for, drawn as chosen.
    inspection_highlight: Option<String>,
    /// The search over the checks of the inspecting page.
    inspection_search: TextField,
    inspection_editing: bool,
    inspection_root: PathBuf,
    /// The words of the spelling dictionary of the user.
    spelling_words: Vec<String>,
    /// The line about the dictionary the check reads.
    spelling_source: String,
    /// The word the user is typing into the dictionary.
    spelling_word: TextField,
    spelling_editing: bool,
    /// The search over the pages, drawn above the tree.
    search: TextField,
    /// Whether the caret of the search field is shown. The window opens with it in the
    /// field, and a click on a page or a setting takes it out.
    search_editing: bool,
    /// The name of the SDK of the project, empty until the environment is read.
    sdk: String,
    /// The code style file the project uses rather than the window settings.
    project_style: Option<text_core::StyleSource>,
    /// The user turned the project style off for this project.
    project_style_off: bool,
    /// The home folder the MCP client rows read, in a test one of its own.
    mcp_home: PathBuf,
    /// The MCP clients of the user and of the project, read in the background.
    mcp_clients: Vec<mcp_discovery::ClientState>,
    mcp_project_clients: Vec<mcp_discovery::ClientState>,
    mcp_clients_loads: u64,
    mcp_clients_loaded: u64,
    /// The client whose menu of transports is open, and whether it writes the project file.
    mcp_open_client: Option<(mcp_discovery::Client, bool)>,
    /// The transport the menu of a client picks.
    mcp_transport: mcp_discovery::Transport,
    /// The Enable MCP Server question of the page stands over the settings.
    mcp_confirm_enable: bool,
    /// The last failure of an MCP client file, shown under the rows.
    mcp_error: Option<String>,
    /// The SDKs the pages offer where a choice among them is needed.
    runtimes: Vec<RuntimeInfo>,
    /// The local repository the settings file names, read in the background.
    repository: String,
    /// What the Test button of the Git page found.
    git_version: Option<String>,
    testing_git: bool,
    dragging: Option<PlacementDrag>,
    resizing: Option<PlacementResize>,
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
        Self::open(workspace, window, cx, target, None);
    }

    /// Shows the Inspections page with the row of `id` marked, for Alt+Enter.
    pub fn show_inspection(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
        id: &str,
    ) {
        Self::open(workspace, window, cx, None, Some(id.to_owned()));
    }

    fn open(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
        target: Option<&'static str>,
        inspection: Option<String>,
    ) {
        let weak = cx.weak_entity();
        let settings_dir = workspace.settings_dir().to_path_buf();
        let settings = cx.global::<Settings>().clone();
        let font_family = settings.editor_font_family.clone();
        let page = if inspection.is_some() {
            SettingsPage::EditorInspections
        } else {
            target
                .and_then(settings_catalog::find)
                .map_or(SettingsPage::Appearance, |setting| setting.page)
        };
        let root = workspace.root().to_path_buf();
        let inspection_settings = workspace::load_inspections(&root);
        let (spelling_words, spelling_source) = crate::spelling::words_and_source(cx);
        let inspections = cx
            .try_global::<Semantic>()
            .map(|semantic| semantic.0.inspections())
            .unwrap_or_default();
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
        let project_style = text_core::style_source(&root);
        let project_style_off = root.join(text_core::idea_style::DISABLED_FILE).is_file();
        let mcp_home = mcp_home_folder(cx);
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            editing: None,
            text_field: TextField::from_text(font_family),
            draft: settings.clone(),
            saved: settings,
            plugins: Vec::new(),
            saved_plugins: Vec::new(),
            plugin_file: plugin_file.clone(),
            keymap_page,
            templates_page,
            page,
            highlight: target,
            inspections,
            inspection_settings: inspection_settings.clone(),
            saved_inspections: inspection_settings,
            inspection_highlight: inspection,
            inspection_search: TextField::default(),
            inspection_editing: false,
            inspection_root: root,
            spelling_words,
            spelling_source,
            spelling_word: TextField::default(),
            spelling_editing: false,
            search: TextField::default(),
            search_editing: true,
            sdk: "not set".to_owned(),
            project_style,
            project_style_off,
            mcp_home,
            mcp_clients: Vec::new(),
            mcp_project_clients: Vec::new(),
            mcp_clients_loads: 0,
            mcp_clients_loaded: 0,
            mcp_open_client: None,
            mcp_transport: mcp_discovery::Transport::Stdio,
            mcp_confirm_enable: false,
            mcp_error: None,
            runtimes: Vec::new(),
            repository: String::new(),
            git_version: None,
            testing_git: false,
            dragging: None,
            resizing: None,
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
                view.saved_plugins = plugins.clone();
                view.plugins = plugins;
                cx.notify();
            })
            .ok();
        })
        .detach();
        let focus = view.read(cx).focus.clone();
        if page == SettingsPage::Mcp {
            view.update(cx, |view, cx| view.load_mcp_clients(cx));
        }
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    /// Edits the draft only: nothing reaches the window or the settings file until Apply
    /// or OK. The control that was used takes the caret out of the search field.
    fn change(&mut self, change: impl FnOnce(&mut Settings), cx: &mut Context<Self>) {
        change(&mut self.draft);
        self.search_editing = false;
        cx.notify();
    }

    fn toggle_plugin(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(row) = self.plugins.get_mut(index) else {
            return;
        };
        row.enabled = !row.enabled;
        self.search_editing = false;
        cx.notify();
    }

    /// Whether the window holds something Apply would write.
    fn changed(&self) -> bool {
        self.draft != self.saved
            || self.plugins != self.saved_plugins
            || self.inspection_settings != self.saved_inspections
    }

    /// The draft the page holds, for a test of a switch.
    #[cfg(test)]
    pub(crate) fn draft_settings(&self) -> &Settings {
        &self.draft
    }

    /// Flips a row of the Exposed Tools table, for a test of the table.
    #[cfg(test)]
    pub(crate) fn set_tool_switch(
        &mut self,
        name: &'static str,
        enabled: bool,
        router_only: bool,
        cx: &mut Context<Self>,
    ) {
        self.change(
            move |settings| {
                let entry = tool_choice_entry(settings, name);
                entry.enabled = enabled;
                entry.router_only = router_only;
            },
            cx,
        );
    }

    /// Turns the router-only mode on or off, for a test of the table.
    #[cfg(test)]
    pub(crate) fn set_router_mode(&mut self, on: bool, cx: &mut Context<Self>) {
        self.change(|settings| settings.mcp_router_only_mode = on, cx);
    }

    /// The line the Code Style page shows above the window settings: which file of the
    /// project names the style, or that the window settings apply.
    #[cfg(test)]
    pub(crate) fn project_style_note(&self) -> Option<String> {
        if let Some(source) = self.project_style {
            return Some(format!("Using code style from {}", source.label()));
        }
        self.project_style_off
            .then(|| "Using Arro settings".to_owned())
    }

    /// The line and the button of the Code Style page. The button turns the project
    /// style off for this project and records the choice in `.arro` of the project.
    fn project_style_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let Some(source) = self.project_style else {
            return div()
                .debug_selector(|| {
                    if self.project_style_off {
                        "settings-project-style-arro".to_owned()
                    } else {
                        "settings-project-style-none".to_owned()
                    }
                })
                .when(self.project_style_off, |this| {
                    this.text_color(theme.text_secondary)
                        .child("Using Arro settings")
                });
        };
        let weak = self.workspace.clone();
        let root = self.inspection_root.clone();
        div()
            .flex()
            .items_center()
            .justify_between()
            .gap(px(DIALOG_GAP))
            .min_h(px(30.))
            .child(
                div()
                    .debug_selector(|| "settings-project-style".to_owned())
                    .child(format!("Using code style from {}", source.label())),
            )
            .child(
                pill("settings-use-arro-style", "Use Arro settings", true, theme)
                    .debug_selector(|| "settings-use-arro-style".to_owned())
                    .on_click(cx.listener(move |_this, _, _, cx| {
                        let file = root.join(text_core::idea_style::DISABLED_FILE);
                        let write = cx.background_executor().spawn(async move {
                            if let Some(directory) = file.parent()
                                && let Err(error) = std::fs::create_dir_all(directory)
                            {
                                return Err(error.to_string());
                            }
                            std::fs::write(&file, b"").map_err(|error| error.to_string())
                        });
                        let workspace = weak.clone();
                        cx.spawn(async move |this, cx| {
                            let written = write.await;
                            this.update(cx, |this, cx| {
                                if written.is_ok() {
                                    this.project_style = None;
                                    this.project_style_off = true;
                                }
                                cx.notify();
                            })
                            .ok();
                            if written.is_ok() {
                                workspace
                                    .update(cx, |workspace, cx| {
                                        for (_, view) in workspace.open_editors() {
                                            if let Ok(view) =
                                                view.downcast::<editor_view::EditorView>()
                                            {
                                                view.update(cx, |view, cx| {
                                                    view.use_arro_code_style(cx)
                                                });
                                            }
                                        }
                                    })
                                    .ok();
                            }
                        })
                        .detach();
                    })),
            )
    }

    /// Apply or OK: the changed leaves of the draft reach the window, the plugin switches
    /// are written, and the draft starts again from what is in force. OK closes the
    /// window afterwards; Apply leaves it open.
    fn apply_draft(&mut self, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !self.changed() {
            if close {
                self.close(window, cx);
            }
            return;
        }
        let saved = self.saved.clone();
        let draft = self.draft.clone();
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.apply_settings(&saved, &draft, cx);
            })
            .ok();
        let current = cx.global::<Settings>().clone();
        if let Some(semantic) = cx.try_global::<editor_view::Semantic>() {
            semantic
                .0
                .set_dependency_updates(current.check_dependency_updates);
        }
        self.saved = current.clone();
        self.draft = current;
        self.save_plugins(cx);
        self.save_inspections(cx);
        if close {
            self.close(window, cx);
        }
        cx.notify();
    }

    /// Writes the checks of the project and tells the language and the open editors.
    fn save_inspections(&mut self, cx: &mut Context<Self>) {
        if self.inspection_settings == self.saved_inspections {
            return;
        }
        let settings = self.inspection_settings.clone();
        self.saved_inspections = settings.clone();
        let root = self.inspection_root.clone();
        let write = cx
            .background_executor()
            .spawn(async move { workspace::save_inspections(&root, &settings) });
        cx.spawn(async move |_, _| {
            if let Err(error) = write.await {
                eprintln!("the inspections of the project are not saved: {error}");
            }
        })
        .detach();
        self.workspace
            .update(cx, |workspace, cx| {
                crate::inspection::apply(workspace, &self.inspection_settings, cx);
            })
            .ok();
    }

    /// Writes the plugin switches only when one moved since the last Apply or OK.
    fn save_plugins(&mut self, cx: &mut Context<Self>) {
        if self.plugins == self.saved_plugins {
            return;
        }
        let switches = PluginSettings {
            disabled: self
                .plugins
                .iter()
                .filter(|row| !row.enabled)
                .map(|row| row.id.clone())
                .collect(),
        };
        self.saved_plugins = self.plugins.clone();
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
    }

    /// Cancel and the close button: the draft is dropped, nothing is written.
    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.close_menu(window, cx))
            .ok();
    }

    /// Moves the window with a drag of its title bar.
    fn drag_window(&mut self, event: &MouseMoveEvent, cx: &mut Context<Self>) {
        if !event.dragging() {
            return;
        }
        let Some(drag) = self.dragging else {
            return;
        };
        let next = SettingsWindowPlacement {
            left: drag.placement.left + f32::from(event.position.x) - f32::from(drag.from.x),
            top: drag.placement.top + f32::from(event.position.y) - f32::from(drag.from.y),
            ..drag.placement
        };
        cx.set_global(next);
        cx.notify();
    }

    /// Changes the size from the edge or the corner the drag started on.
    fn resize_window(
        &mut self,
        event: &MouseMoveEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if !event.dragging() {
            return;
        }
        let Some(resize) = self.resizing else {
            return;
        };
        let dx = f32::from(event.position.x) - f32::from(resize.from.x);
        let dy = f32::from(event.position.y) - f32::from(resize.from.y);
        let mut next = resize.placement;
        let viewport = window.viewport_size();
        if matches!(resize.edge, ResizeEdge::Right | ResizeEdge::Corner) {
            let room = f32::from(viewport.width) - resize.placement.left;
            next.width = (resize.placement.width + dx)
                .max(MIN_WIDTH)
                .min(room.max(MIN_WIDTH));
        }
        if matches!(resize.edge, ResizeEdge::Bottom | ResizeEdge::Corner) {
            let room = f32::from(viewport.height) - resize.placement.top;
            next.height = (resize.placement.height + dy)
                .max(MIN_HEIGHT)
                .min(room.max(MIN_HEIGHT));
        }
        cx.set_global(next);
        cx.notify();
    }

    /// The button came up after a drag of the title or of an edge.
    fn finish_drag(&mut self, cx: &mut Context<Self>) {
        if self.dragging.take().is_some() || self.resizing.take().is_some() {
            cx.notify();
        }
    }

    /// The search field over the tree of pages.
    fn search_field(&self, theme: &Theme, cx: &mut Context<Self>) -> Stateful<Div> {
        let empty = self.search.text.is_empty();
        let color = if empty {
            theme.text_secondary
        } else {
            theme.text
        };
        div()
            .id("settings-search")
            .debug_selector(|| "settings-search".to_owned())
            .flex()
            .items_center()
            .flex_none()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_2()
            .border_color(if self.search_editing {
                theme.accent
            } else {
                theme.border
            })
            .text_color(color)
            .children(text_field_children_named(
                "settings-search-field",
                &self.search,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                16.,
                self.search_editing,
            ))
            .when(empty, |this| this.child("Search settings"))
            .on_click(cx.listener(|this, _, _, cx| {
                this.search_editing = true;
                this.editing = None;
                cx.notify();
            }))
    }

    /// The search keys. Escape in a filled field clears it; in an empty one it cancels the
    /// window, as the close button does. Up and Down walk the pages the query keeps.
    fn search_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            if self.search.text.is_empty() {
                self.close(window, cx);
            } else {
                self.search = TextField::default();
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        let modifiers = event.keystroke.modifiers;
        if matches!(event.keystroke.key.as_str(), "up" | "down")
            && !modifiers.alt
            && !modifiers.control
            && !modifiers.platform
        {
            let step = if event.keystroke.key == "up" { -1 } else { 1 };
            self.step_page(step, cx);
            cx.stop_propagation();
            return;
        }
        if self.search.key_with_clipboard(event, false, cx) {
            let pages = settings_catalog::pages_matching(&self.search.text);
            if !pages.contains(&self.page) {
                self.page = pages
                    .first()
                    .copied()
                    .unwrap_or(SettingsPage::Appearance)
                    .target();
                self.highlight = None;
            }
            cx.notify();
        }
        cx.stop_propagation();
    }

    /// Up and Down in the search walk the pages of the tree the query keeps, in its order.
    fn step_page(&mut self, step: isize, cx: &mut Context<Self>) {
        let pages = settings_catalog::pages_matching(&self.search.text);
        let Some(last) = pages.len().checked_sub(1) else {
            return;
        };
        let at = pages
            .iter()
            .position(|page| *page == self.page)
            .unwrap_or(0);
        let next = if step < 0 {
            at.saturating_sub(1)
        } else {
            (at + 1).min(last)
        };
        let page = pages[next];
        if page != self.page {
            self.page = page.target();
            self.highlight = None;
            cx.notify();
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.spelling_editing {
            if event.keystroke.key == "escape" {
                self.spelling_editing = false;
            } else if event.keystroke.key == "enter" {
                self.add_spelling_word(cx);
            } else if self.spelling_word.key_with_clipboard(event, false, cx) {
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if self.inspection_editing {
            if event.keystroke.key == "escape" {
                if self.inspection_search.text.is_empty() {
                    self.close(window, cx);
                } else {
                    self.inspection_search = TextField::default();
                    cx.notify();
                }
            } else if self.inspection_search.key_with_clipboard(event, false, cx) {
                cx.notify();
            }
            cx.stop_propagation();
            return;
        }
        if self.search_editing {
            self.search_key(event, window, cx);
            return;
        }
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
            return;
        }
        if event.keystroke.key == "escape" {
            self.close(window, cx);
            cx.stop_propagation();
        }
    }

    /// The picks of the Inspections page: a search over the checks, the tree by group,
    /// a switch and a level for each check, and the reset of every choice.
    fn render_inspections(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<Div> {
        let mut content = Vec::new();
        content.push(note(
            "Compiler problems of the language server apply after it restarts.",
            theme,
        ));
        content.push(
            div().child(
                field(
                    "settings-inspection-search",
                    &self.inspection_search,
                    "Search inspections",
                    self.inspection_editing,
                    theme,
                )
                .debug_selector(|| "settings-inspection-search".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.inspection_editing = true;
                    this.search_editing = false;
                    this.editing = None;
                    cx.notify();
                })),
            ),
        );
        let reset = dialog_button(
            "settings-inspection-reset",
            "settings-inspection-reset".to_owned(),
            "Reset to default",
            false,
            !self.inspection_settings.inspections.is_empty(),
            theme,
        )
        .debug_selector(|| "settings-inspection-reset".to_owned())
        .on_click(cx.listener(|this, _, _, cx| {
            this.inspection_settings.reset();
            this.inspection_editing = false;
            cx.notify();
        }));
        content.push(div().flex().justify_end().child(reset));
        let query = self.inspection_search.text.trim().to_lowercase();
        let shown: Vec<&lang_api::InspectionInfo> = self
            .inspections
            .iter()
            .filter(|info| {
                let text = format!("{} {} {}", info.title, info.id, info.group).to_lowercase();
                query.split_whitespace().all(|word| text.contains(word))
            })
            .collect();
        if shown.is_empty() {
            content.push(
                div()
                    .py(px(SPACE_S))
                    .text_color(theme.text_secondary)
                    .child("No inspections found."),
            );
            return content;
        }
        let mut group = String::new();
        for (index, info) in shown.into_iter().enumerate() {
            if info.group != group {
                group = info.group.clone();
                content.push(
                    div()
                        .debug_selector(|| "settings-inspection-group".to_owned())
                        .mt(px(DIALOG_GAP))
                        .pb(px(SPACE_S))
                        .border_b_1()
                        .border_color(theme.border)
                        .font_weight(FontWeight::MEDIUM)
                        .child(group.clone()),
                );
            }
            let id = info.id.clone();
            let enabled = self
                .inspection_settings
                .setting(&id)
                .is_none_or(|setting| setting.enabled);
            let level = self
                .inspection_settings
                .level_of(&id, info.default_severity);
            let toggle = pill(
                ("inspection-toggle", index),
                if enabled { "On" } else { "Off" },
                enabled,
                theme,
            )
            .debug_selector(move || format!("settings-inspection-toggle-{index}"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_inspection(&id, cx);
            }));
            let level_id = info.id.clone();
            let severity =
                toolbar_dropdown(("inspection-level", index), None, level.label(), theme)
                    .debug_selector(move || format!("settings-inspection-level-{index}"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.cycle_inspection(&level_id, cx);
                    }));
            let chosen = self.inspection_highlight.as_deref() == Some(info.id.as_str());
            let row = div()
                .id(("inspection-row", index))
                .debug_selector(|| "settings-inspection-row".to_owned())
                .flex()
                .items_center()
                .justify_between()
                .gap(px(DIALOG_GAP))
                .min_h(px(30.))
                .px(px(ROW_PADDING_X))
                .rounded(px(CONTROL_RADIUS))
                .when(chosen, |this| this.bg(theme.selection))
                .child(
                    div().flex().flex_col().child(info.title.clone()).child(
                        div()
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_secondary)
                            .child(info.id.clone()),
                    ),
                )
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SPACE_S))
                        .child(toggle)
                        .child(severity),
                );
            content.push(
                div()
                    .debug_selector(move || format!("settings-inspection-{index}"))
                    .child(row),
            );
        }
        content
    }

    /// The words of the spelling dictionary of the user, which the check reads beside
    /// the words of the system, and the line about where the words come from.
    fn render_spelling(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<Div> {
        let mut content = Vec::new();
        content.push(
            div()
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_secondary)
                .child(self.spelling_source.clone()),
        );
        if !crate::spelling::enabled(cx) {
            content.push(note(
                "Spell checking is off: this machine has no dictionary.",
                theme,
            ));
        }
        let add = dialog_button(
            "settings-spelling-add",
            "settings-spelling-add".to_owned(),
            "Add",
            true,
            !self.spelling_word.text.trim().is_empty(),
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| this.add_spelling_word(cx)));
        content.push(
            div()
                .mt(px(DIALOG_GAP))
                .flex()
                .items_center()
                .gap(px(SPACE_M))
                .child(
                    div().flex_1().child(
                        field(
                            "settings-spelling-word",
                            &self.spelling_word,
                            "Add a word",
                            self.spelling_editing,
                            theme,
                        )
                        .debug_selector(|| "settings-spelling-word".to_owned())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.spelling_editing = true;
                            this.search_editing = false;
                            this.inspection_editing = false;
                            this.editing = None;
                            cx.notify();
                        })),
                    ),
                )
                .child(add),
        );
        if self.spelling_words.is_empty() {
            content.push(
                div()
                    .mt(px(DIALOG_GAP))
                    .text_color(theme.text_secondary)
                    .child("No words of your own."),
            );
        }
        for (index, word) in self.spelling_words.iter().enumerate() {
            let chosen = word.clone();
            let remove = dialog_button(
                ("settings-spelling-remove", index),
                format!("settings-spelling-remove-{index}"),
                "Remove",
                false,
                true,
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                crate::spelling::remove_user_word(cx, &chosen);
                this.spelling_words.retain(|entry| entry != &chosen);
                cx.notify();
            }));
            content.push(
                div()
                    .debug_selector(move || format!("settings-spelling-word-{index}"))
                    .mt(px(SPACE_S))
                    .flex()
                    .items_center()
                    .justify_between()
                    .px(px(ROW_PADDING_X))
                    .min_h(px(30.))
                    .rounded(px(CONTROL_RADIUS))
                    .child(word.clone())
                    .child(remove),
            );
        }
        content
    }

    fn add_spelling_word(&mut self, cx: &mut Context<Self>) {
        let word = self.spelling_word.text.trim().to_owned();
        if word.is_empty() {
            return;
        }
        crate::spelling::add_user_word(cx, &word);
        if !self
            .spelling_words
            .iter()
            .any(|entry| entry.eq_ignore_ascii_case(&word))
        {
            self.spelling_words.push(word);
        }
        self.spelling_word = TextField::default();
        self.spelling_editing = true;
        cx.notify();
    }

    fn toggle_inspection(&mut self, id: &str, cx: &mut Context<Self>) {
        let current = self.inspection_settings.setting(id);
        let enabled = current.is_some_and(|setting| !setting.enabled);
        let level = current.and_then(|setting| setting.level);
        self.inspection_settings.set(id, enabled, level);
        self.inspection_editing = false;
        cx.notify();
    }

    fn cycle_inspection(&mut self, id: &str, cx: &mut Context<Self>) {
        let default = self
            .inspections
            .iter()
            .find(|info| info.id == id)
            .map_or(lang_api::InspectionLevel::Warning, |info| {
                info.default_severity
            });
        let next = self.inspection_settings.level_of(id, default).next();
        let level = (next != lang_api::InspectionLevel::Off).then_some(next);
        self.inspection_settings
            .set(id, next != lang_api::InspectionLevel::Off, level);
        self.inspection_editing = false;
        cx.notify();
    }

    /// Reads the local repository of the settings file again, off the UI thread.
    fn refresh_repository(&self, cx: &mut Context<Self>) {
        let file = self
            .draft
            .build_tools
            .first
            .settings_path()
            .map(PathBuf::from)
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
        let configured = self.draft.git_path.trim().to_owned();
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

fn field(
    id: &'static str,
    value: &TextField,
    hint: &'static str,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let empty = value.text.is_empty();
    let color = if empty {
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
        .children(text_field_children_named(
            "settings-field",
            value,
            theme.text,
            theme.text,
            theme.selection,
            theme.caret,
            16.,
            active,
        ))
        .when(empty, |this| this.child(hint))
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
        SettingField::KubectlPath => "Found on PATH",
        SettingField::KubeconfigPath => "~/.kube/config",
        SettingField::McpBinary => "Next to Arro",
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
            let current = this.draft.build_tools.second.runtime_home.clone();
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
    chosen: bool,
    theme: &Theme,
) -> Div {
    let id = entry.id;
    row(entry.title, control, theme)
        .px(px(ROW_PADDING_X))
        .rounded(px(CONTROL_RADIUS))
        .when(chosen, |this| {
            this.debug_selector(move || format!("settings-chosen-{}", id.replace('_', "-")))
        })
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

/// One edge or the corner that changes the size of the window.
fn resize_grip(
    id: &'static str,
    edge: ResizeEdge,
    placement: SettingsWindowPlacement,
    cx: &mut Context<SettingsView>,
) -> Stateful<Div> {
    let grip = div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .absolute()
        .occlude()
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                this.resizing = Some(PlacementResize {
                    from: event.position,
                    placement,
                    edge,
                });
                cx.notify();
            }),
        )
        .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
            this.resize_window(event, window, cx);
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.finish_drag(cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.finish_drag(cx)),
        );
    match edge {
        ResizeEdge::Right => grip
            .top_0()
            .bottom_0()
            .right_0()
            .w(px(RESIZE_GRIP))
            .cursor_col_resize(),
        ResizeEdge::Bottom => grip
            .left_0()
            .right_0()
            .bottom_0()
            .h(px(RESIZE_GRIP))
            .cursor_row_resize(),
        ResizeEdge::Corner => grip
            .right_0()
            .bottom_0()
            .w(px(RESIZE_GRIP))
            .h(px(RESIZE_GRIP))
            .cursor_row_resize(),
    }
}

impl SettingsView {
    /// The themes of the Appearance page: every theme with a strip of its colours, the
    /// light ones apart from the dark ones, the files that did not load under them, and
    /// the Import button.
    fn theme_section(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<Div> {
        let library = ThemeCatalog::library_of(cx);
        let mut rows = vec![
            heading("Themes", theme),
            note(crate::theme_import::THEMES_NOTE, theme),
        ];
        for (dark, title) in [(false, "Light appearance"), (true, "Dark appearance")] {
            let cards: Vec<AnyElement> =
                library
                    .themes()
                    .iter()
                    .filter(|definition| definition.dark == dark)
                    .map(|definition| {
                        let preview = Theme::from_definition(definition);
                        let chosen = self.theme_card_chosen(&definition.id, dark, theme);
                        let id = definition.id.clone();
                        let debug = format!("theme-card-{}", definition.id);
                        let hover = theme.selection_hover;
                        div()
                            .id(SharedString::from(debug.clone()))
                            .debug_selector({
                                let debug = debug.clone();
                                move || debug.clone()
                            })
                            .w(px(196.))
                            .flex()
                            .flex_col()
                            .gap(px(SPACE_XS))
                            .p(px(SPACE_M))
                            .rounded(px(CONTROL_RADIUS))
                            .border_1()
                            .border_color(if chosen {
                                theme.tab_selected_border
                            } else {
                                theme.popup_border
                            })
                            .bg(if chosen {
                                theme.tab_selected
                            } else {
                                theme.popup_bg
                            })
                            .hover(move |style| style.bg(hover))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(SPACE_XS))
                                    .child(colour_strip(preview.main_bg))
                                    .child(colour_strip(preview.text))
                                    .child(colour_strip(preview.accent)),
                            )
                            .child(
                                div()
                                    .text_color(theme.text)
                                    .child(SharedString::from(definition.name.clone())),
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.pick_theme(id.clone(), dark, cx)
                            }))
                            .into_any_element()
                    })
                    .collect();
            if !cards.is_empty() {
                rows.push(
                    div()
                        .pt(px(SPACE_S))
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_secondary)
                        .child(title),
                );
                rows.push(div().flex().flex_wrap().gap(px(SPACE_M)).children(cards));
            }
        }
        if !library.errors().is_empty() {
            rows.push(heading("Theme Problems", theme));
            let errors: Vec<Div> = library
                .errors()
                .iter()
                .map(|error| {
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.warning_mark)
                        .child(error.to_string())
                })
                .collect();
            rows.push(div().flex().flex_col().gap(px(SPACE_XS)).children(errors));
        }
        let hover = theme.hover;
        let import = div()
            .id("theme-import")
            .debug_selector(|| "settings-theme-import".to_owned())
            .flex()
            .items_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_L))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(theme.control_border)
            .hover(move |style| style.bg(hover))
            .child("Import\u{2026}")
            .on_click(cx.listener(|this, _, _, cx| {
                this.workspace
                    .update(cx, |workspace, cx| {
                        crate::theme_import::import(workspace, cx)
                    })
                    .ok();
            }));
        rows.push(div().pt(px(SPACE_S)).child(import));
        rows
    }

    /// The icon sets of the Appearance page: every built-in set and every folder of the
    /// `icons` folder of the settings. A click applies the set at once.
    fn icon_section(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<Div> {
        let mut rows = vec![
            heading("Icons", theme),
            note(
                "An icon set is a folder in the icons folder of the settings. Changes on this page apply at once.",
                theme,
            ),
        ];
        let sets = IconCatalog::sets_of(cx).to_vec();
        let cards: Vec<AnyElement> = sets
            .into_iter()
            .enumerate()
            .map(|(index, set)| {
                let IconSet { id, name } = set;
                let chosen = self.draft.icon_set == id;
                let debug = format!("settings-icon-set-{id}");
                let hover = theme.selection_hover;
                div()
                    .id(("icon-set", index))
                    .debug_selector({
                        let debug = debug.clone();
                        move || debug.clone()
                    })
                    .min_w(px(120.))
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_L))
                    .rounded(px(CONTROL_RADIUS))
                    .border_1()
                    .border_color(if chosen {
                        theme.tab_selected_border
                    } else {
                        theme.popup_border
                    })
                    .bg(if chosen {
                        theme.tab_selected
                    } else {
                        theme.popup_bg
                    })
                    .hover(move |style| style.bg(hover))
                    .child(div().text_color(theme.text).child(name))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.draft.icon_set.clone_from(&id);
                        this.workspace
                            .update(cx, |workspace, cx| workspace.assign_icon_set(&id, cx))
                            .ok();
                        this.search_editing = false;
                        cx.notify();
                    }))
                    .into_any_element()
            })
            .collect();
        rows.push(div().flex().flex_wrap().gap(px(SPACE_M)).children(cards));
        rows
    }

    /// Whether the card of a theme is the one the page marks as chosen: the theme of the
    /// window while it does not follow the system, and the theme of the card's look while
    /// it does.
    pub(crate) fn theme_card_chosen(&self, id: &str, dark: bool, theme: &Theme) -> bool {
        settings_catalog::chosen_theme(&self.draft, dark, theme.id()) == id
    }

    /// The text of the Theme row.
    pub(crate) fn theme_label(&self, theme: &Theme) -> String {
        settings_catalog::theme_label(&self.draft, theme.dark, theme.id())
    }

    /// A click on the Theme row: with Sync with OS on the row shows the theme of the other
    /// look, as it always has; a window that does not follow the system goes to the next
    /// theme of the list.
    fn next_theme(&mut self, theme: &Theme, cx: &mut Context<Self>) {
        self.search_editing = false;
        if self.draft.theme_sync_with_os {
            self.workspace
                .update(cx, |workspace, cx| workspace.toggle_theme(cx))
                .ok();
            // The window no longer follows the system: the draft takes the theme the
            // click chose, so the row does not keep the name of the old one.
            if let Some(current) = cx.try_global::<Settings>() {
                self.draft.theme_sync_with_os = current.theme_sync_with_os;
                self.draft.theme.clone_from(&current.theme);
            }
            cx.notify();
            return;
        }
        let next = {
            let themes = ThemeCatalog::library_of(cx).themes();
            if themes.is_empty() {
                return;
            }
            let chosen = settings_catalog::chosen_theme(&self.draft, theme.dark, theme.id());
            let at = themes
                .iter()
                .position(|definition| definition.id == chosen)
                .unwrap_or(0);
            themes[(at + 1) % themes.len()].id.clone()
        };
        self.draft.theme.clone_from(&next);
        self.workspace
            .update(cx, |workspace, cx| workspace.choose_theme(&next, cx))
            .ok();
        cx.notify();
    }

    /// A card of a theme takes the slot of its look: Sync with OS keeps choosing between
    /// the light and the dark slot, and a window that does not sync takes the theme at
    /// once.
    fn pick_theme(&mut self, id: String, dark: bool, cx: &mut Context<Self>) {
        if dark {
            self.draft.theme_dark.clone_from(&id);
        } else {
            self.draft.theme_light.clone_from(&id);
        }
        if !self.draft.theme_sync_with_os {
            self.draft.theme.clone_from(&id);
        }
        self.workspace
            .update(cx, |workspace, cx| workspace.assign_theme(&id, dark, cx))
            .ok();
        self.search_editing = false;
        cx.notify();
    }

    /// The MCP Server page of IDEA: the switch of the server, the clients it can configure
    /// itself, the strings to copy, the brave mode, the terminal suggestions and the table
    /// of the exposed tools.
    fn mcp_section(
        &mut self,
        settings: &Settings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let mut content = vec![
            note("Changes on this page apply at once.", theme),
            heading("Enable MCP Server", theme),
            self.mcp_enable_row(theme, cx),
        ];
        if self.mcp_confirm_enable {
            content.push(self.mcp_enable_question(theme, cx));
        }
        content.push(heading("Clients Auto-Configuration", theme));
        content.extend(self.mcp_client_rows(false, theme, cx));
        content.push(heading("Project Clients Auto-Configuration", theme));
        content.extend(self.mcp_client_rows(true, theme, cx));
        content.push(heading("Manual Client Configuration", theme));
        content.extend(self.mcp_manual_rows(settings, theme, cx));
        content.push(heading("Command execution", theme));
        content.push(self.mcp_permission_row(settings, theme, cx));
        content.push(heading("Terminal Sessions", theme));
        content.push(self.mcp_terminal_row(settings, theme, cx));
        content.push(heading("Exposed Tools", theme));
        content.push(self.mcp_router_row(settings, theme, cx));
        content.extend(self.mcp_tool_rows(settings, theme, cx));
        if let Some(error) = &self.mcp_error {
            content.push(note_owned(error.clone(), theme));
        }
        content
    }

    fn mcp_enable_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let on = self.draft.mcp_enabled;
        let toggle = pill(
            "settings-mcp-enabled",
            if on { "On" } else { "Off" },
            on,
            theme,
        )
        .debug_selector(|| "settings-mcp-enabled".to_owned())
        .on_click(cx.listener(|this, _, _, cx| {
            if this.draft.mcp_enabled {
                this.change(|settings| settings.mcp_enabled = false, cx);
            } else {
                this.mcp_confirm_enable = true;
                this.search_editing = false;
                cx.notify();
            }
        }));
        row("Enable MCP Server", toggle, theme)
    }

    /// The question of IDEA before the server of the window opens to other programs.
    fn mcp_enable_question(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let yes = dialog_button(
            "settings-mcp-enable-yes",
            "settings-mcp-enable-yes".to_owned(),
            "Enable",
            true,
            true,
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.change(|settings| settings.mcp_enabled = true, cx);
            this.mcp_confirm_enable = false;
            cx.notify();
        }));
        let no = dialog_button(
            "settings-mcp-enable-no",
            "settings-mcp-enable-no".to_owned(),
            "Cancel",
            false,
            true,
            theme,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.mcp_confirm_enable = false;
            cx.notify();
        }));
        div().child(
            div()
                .id("settings-mcp-enable-confirm")
                .debug_selector(|| "settings-mcp-enable-confirm".to_owned())
                .flex()
                .flex_col()
                .gap(px(SPACE_S))
                .p(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .border_1()
                .border_color(theme.accent)
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .child("Enable the MCP server of this window?"),
                )
                .child(
                    div()
                        .text_size(px(TEXT_SMALL))
                        .text_color(theme.text_secondary)
                        .child(
                            "Other programs on this computer will be able to call the tools of \
                         Arro while the project is open.",
                        ),
                )
                .child(
                    div()
                        .flex()
                        .justify_end()
                        .gap(px(SPACE_M))
                        .child(yes)
                        .child(no),
                ),
        )
    }

    fn mcp_client_state(
        &self,
        client: mcp_discovery::Client,
        project: bool,
    ) -> Option<&mcp_discovery::ClientState> {
        let rows = if project {
            &self.mcp_project_clients
        } else {
            &self.mcp_clients
        };
        rows.iter().find(|state| state.client == client)
    }

    fn mcp_client_rows(
        &mut self,
        project: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let mut rows = Vec::new();
        for client in mcp_discovery::CLIENTS {
            rows.extend(self.mcp_client_row(client, project, theme, cx));
        }
        rows
    }

    /// One client of the page: its state, Auto-Configure when the client is on this
    /// machine, and the menu of the transports under the chevron.
    fn mcp_client_row(
        &mut self,
        client: mcp_discovery::Client,
        project: bool,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let state = self.mcp_client_state(client, project).cloned();
        let status = match &state {
            None => "Reading...",
            Some(state) => match state.status {
                mcp_discovery::Status::Missing => "Not detected",
                mcp_discovery::Status::Damaged => "Damaged settings file",
                mcp_discovery::Status::Found if state.configured => "Configured",
                mcp_discovery::Status::Found => "Not configured",
            },
        };
        let found = state
            .as_ref()
            .is_some_and(|state| state.status == mcp_discovery::Status::Found);
        let prefix = if project {
            "settings-mcp-project-client"
        } else {
            "settings-mcp-client"
        };
        let id = client.id();
        let open = self.mcp_open_client == Some((client, project));

        let mut controls = div().flex().items_center().gap(px(SPACE_S)).flex_none();
        if found {
            controls = controls.child(
                pill(format!("{prefix}-{id}-auto"), "Auto-Configure", true, theme)
                    .debug_selector(move || format!("{prefix}-{id}-auto"))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let transport = this.mcp_transport;
                        this.configure_mcp_client(client, project, transport, cx);
                    })),
            );
        }
        let chevron = if open {
            "general/chevronDown"
        } else {
            "general/chevronRight"
        };
        controls = controls.child(
            icon_button(
                SharedString::from(format!("{prefix}-{id}-menu")),
                chevron,
                "Transports, settings file and config",
                None,
                theme,
            )
            .debug_selector(move || format!("{prefix}-{id}-menu"))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.mcp_open_client = if open { None } else { Some((client, project)) };
                this.search_editing = false;
                cx.notify();
            })),
        );

        let row = div()
            .debug_selector(move || format!("{prefix}-{id}"))
            .flex()
            .items_center()
            .gap(px(SPACE_M))
            .min_h(px(30.))
            .px(px(ROW_PADDING_X))
            .child(div().w(px(160.)).flex_none().child(client.title()))
            .child(
                div()
                    .flex_1()
                    .text_size(px(TEXT_SMALL))
                    .text_color(if state.as_ref().is_some_and(|state| state.configured) {
                        theme.text
                    } else {
                        theme.text_secondary
                    })
                    .child(status),
            )
            .child(controls);

        let mut rows = vec![row];
        if open {
            rows.push(self.mcp_client_menu(client, project, prefix, id, theme, cx));
        }
        rows
    }

    fn mcp_client_menu(
        &mut self,
        client: mcp_discovery::Client,
        project: bool,
        prefix: &'static str,
        id: &'static str,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let mut menu = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(px(SPACE_S))
            .px(px(ROW_PADDING_X + SPACE_L))
            .py(px(SPACE_XS));
        for transport in [
            mcp_discovery::Transport::HttpStream,
            mcp_discovery::Transport::Stdio,
            mcp_discovery::Transport::Sse,
        ] {
            let kind = transport_id(transport);
            let chosen = self.mcp_transport == transport;
            menu = menu.child(
                pill(
                    format!("{prefix}-{id}-{kind}"),
                    transport.title(),
                    chosen,
                    theme,
                )
                .debug_selector(move || format!("{prefix}-{id}-{kind}"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.mcp_transport = transport;
                    this.configure_mcp_client(client, project, transport, cx);
                })),
            );
        }
        let open_selector = format!("{prefix}-{id}-open");
        menu = menu.child(
            dialog_button(
                open_selector.clone(),
                open_selector.clone(),
                "Open Client Settings File",
                false,
                true,
                theme,
            )
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_mcp_client_file(client, project, window, cx);
            })),
        );
        let copy_selector = format!("{prefix}-{id}-copy");
        menu = menu.child(
            dialog_button(
                copy_selector.clone(),
                copy_selector.clone(),
                "Copy Config",
                false,
                true,
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                let transport = this.mcp_transport;
                this.copy_mcp_client(client, transport, cx);
            })),
        );
        let remove_selector = format!("{prefix}-{id}-remove");
        menu.child(
            dialog_button(
                remove_selector.clone(),
                remove_selector.clone(),
                "Remove",
                false,
                true,
                theme,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.remove_mcp_client(client, project, cx);
            })),
        )
    }

    fn mcp_manual_rows(
        &mut self,
        settings: &Settings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let endpoint = mcp_endpoint(cx);
        let mut rows = Vec::new();
        for (transport, selector, title) in [
            (
                mcp_discovery::Transport::HttpStream,
                "settings-mcp-copy-http",
                "Copy HTTP Stream Config",
            ),
            (
                mcp_discovery::Transport::Stdio,
                "settings-mcp-copy-stdio",
                "Copy Stdio Config",
            ),
            (
                mcp_discovery::Transport::Sse,
                "settings-mcp-copy-sse",
                "Copy SSE Config",
            ),
        ] {
            let available = transport == mcp_discovery::Transport::Stdio || endpoint.is_some();
            let url = endpoint.as_ref().map(|(url, _)| mcp_url(transport, url));
            let token = endpoint.as_ref().map(|(_, token)| token.clone());
            let config =
                settings_catalog::mcp_config(settings, transport, url.as_deref(), token.as_deref());
            let button = dialog_button(
                selector,
                selector.to_owned(),
                title,
                false,
                available,
                theme,
            );
            let button = if available {
                button.on_click(cx.listener(move |_, _, _, cx| {
                    cx.write_to_clipboard(ClipboardItem::new_string(config.clone()));
                }))
            } else {
                button
            };
            rows.push(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .px(px(ROW_PADDING_X))
                    .child(button)
                    .when(!available, |this| {
                        this.child(note("Enable MCP Server first", theme))
                    }),
            );
        }
        rows.push(self.mcp_binary_row(theme, cx));
        rows
    }

    fn mcp_binary_row(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let id = "mcp_binary";
        let current = self.draft.mcp_binary.clone();
        let active = self.editing == Some(id);
        let start = current.clone();
        let typed;
        let value = if active {
            &self.text_field
        } else {
            typed = TextField::from_text(current);
            &typed
        };
        let input = field(id, value, "Next to Arro", active, theme)
            .debug_selector(|| "settings-mcp-binary".to_owned())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.editing = Some(id);
                this.search_editing = false;
                this.text_field = TextField::from_text(start.clone());
                cx.notify();
            }));
        row("Path to arro-mcp", input, theme)
    }

    fn mcp_permission_row(
        &self,
        settings: &Settings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let label = settings.mcp_execute_permission.label();
        let choice = pill("settings-mcp-brave-mode", label, true, theme)
            .debug_selector(|| "settings-mcp-brave-mode".to_owned())
            .on_click(cx.listener(|this, _, _, cx| {
                this.change(
                    |settings| {
                        settings.mcp_execute_permission = settings.mcp_execute_permission.next();
                    },
                    cx,
                )
            }));
        row(
            "Run shell commands or run configurations without confirmation (brave mode)",
            choice,
            theme,
        )
    }

    fn mcp_terminal_row(&self, settings: &Settings, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let on = settings.mcp_terminal_suggestions;
        let toggle = pill(
            "settings-mcp-terminal",
            if on { "On" } else { "Off" },
            on,
            theme,
        )
        .debug_selector(|| "settings-mcp-terminal".to_owned())
        .on_click(cx.listener(|this, _, _, cx| {
            this.change(
                |settings| {
                    settings.mcp_terminal_suggestions = !settings.mcp_terminal_suggestions;
                },
                cx,
            )
        }));
        row(
            "Show setup suggestions for Codex and Claude terminal sessions",
            toggle,
            theme,
        )
    }

    fn mcp_router_row(&self, settings: &Settings, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let on = settings.mcp_router_only_mode;
        let toggle = pill(
            "settings-mcp-router-only",
            if on { "On" } else { "Off" },
            on,
            theme,
        )
        .debug_selector(|| "settings-mcp-router-only".to_owned())
        .on_click(cx.listener(|this, _, _, cx| {
            this.change(
                |settings| settings.mcp_router_only_mode = !settings.mcp_router_only_mode,
                cx,
            )
        }));
        let scope = pill(
            "settings-mcp-router-scope",
            settings.mcp_router_only_scope.label(),
            true,
            theme,
        )
        .debug_selector(|| "settings-mcp-router-scope".to_owned())
        .on_click(cx.listener(|this, _, _, cx| {
            this.change(
                |settings| settings.mcp_router_only_scope = settings.mcp_router_only_scope.next(),
                cx,
            )
        }));
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_M))
            .min_h(px(30.))
            .px(px(ROW_PADDING_X))
            .child(div().flex_1().child("Enable router-only mode"))
            .child(scope)
            .child(toggle)
    }

    fn mcp_tool_rows(
        &mut self,
        settings: &Settings,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Vec<Div> {
        let mut rows = vec![
            div()
                .flex()
                .items_center()
                .gap(px(SPACE_M))
                .px(px(ROW_PADDING_X))
                .pb(px(SPACE_XS))
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_secondary)
                .child(div().flex_1().child("Tool"))
                .child(div().w(px(80.)).child("Enabled"))
                .child(div().w(px(90.)).child("Router-only")),
        ];
        for group in tool_gateway::TOOL_GROUPS {
            rows.push(heading(group, theme));
            for spec in tool_gateway::all_tools()
                .iter()
                .filter(|spec| tool_gateway::group_of(spec.name) == group)
            {
                let choice = tool_choice(settings, spec.name);
                let name = spec.name;
                let enabled = pill(
                    format!("settings-mcp-tool-{name}-enabled"),
                    "Enabled",
                    choice.enabled,
                    theme,
                )
                .debug_selector(move || format!("settings-mcp-tool-{name}-enabled"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.change(
                        move |settings| {
                            let entry = tool_choice_entry(settings, name);
                            entry.enabled = !entry.enabled;
                        },
                        cx,
                    )
                }));
                let router = pill(
                    format!("settings-mcp-tool-{name}-router"),
                    "Router-only",
                    choice.router_only,
                    theme,
                )
                .debug_selector(move || format!("settings-mcp-tool-{name}-router"))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.change(
                        move |settings| {
                            let entry = tool_choice_entry(settings, name);
                            entry.router_only = !entry.router_only;
                        },
                        cx,
                    )
                }));
                rows.push(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(SPACE_M))
                        .min_h(px(28.))
                        .px(px(ROW_PADDING_X))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w_0()
                                .child(name)
                                .child(
                                    div()
                                        .text_size(px(TEXT_SMALL))
                                        .text_color(theme.text_secondary)
                                        .child(spec.description.lines().next().unwrap_or_default()),
                                ),
                        )
                        .child(div().w(px(80.)).child(enabled))
                        .child(div().w(px(90.)).child(router)),
                );
            }
        }
        rows
    }

    /// Reads the clients of the machine off the UI thread. A test names its own home
    /// folder, so the profile of the user is not touched.
    fn load_mcp_clients(&mut self, cx: &mut Context<Self>) {
        self.mcp_clients_loads += 1;
        let generation = self.mcp_clients_loads;
        let home = self.mcp_home.clone();
        let project = self.inspection_root.clone();
        let read = cx.background_executor().spawn(async move {
            let mut user = Vec::new();
            let mut in_project = Vec::new();
            for client in mcp_discovery::CLIENTS {
                user.push(mcp_discovery::state(
                    client,
                    mcp_discovery::Scope::User,
                    &home,
                    &project,
                ));
                in_project.push(mcp_discovery::state(
                    client,
                    mcp_discovery::Scope::Project,
                    &home,
                    &project,
                ));
            }
            (user, in_project)
        });
        cx.spawn(async move |this, cx| {
            let (user, project) = read.await;
            this.update(cx, |view, cx| {
                if view.mcp_clients_loads == generation {
                    view.mcp_clients = user;
                    view.mcp_project_clients = project;
                    view.mcp_clients_loaded = generation;
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn configure_mcp_client(
        &mut self,
        client: mcp_discovery::Client,
        project: bool,
        transport: mcp_discovery::Transport,
        cx: &mut Context<Self>,
    ) {
        let home = self.mcp_home.clone();
        let root = self.inspection_root.clone();
        let (url, token) = match mcp_endpoint(cx) {
            Some((url, token)) => (Some(mcp_url(transport, &url)), Some(token)),
            None => (None, None),
        };
        let server = mcp_discovery::Server {
            command: settings_catalog::mcp_binary(&self.draft),
            url,
            token,
        };
        let scope = if project {
            mcp_discovery::Scope::Project
        } else {
            mcp_discovery::Scope::User
        };
        let write = cx.background_executor().spawn(async move {
            mcp_discovery::configure(client, scope, transport, &server, &home, &root)
        });
        cx.spawn(async move |this, cx| {
            let written = write.await;
            this.update(cx, |view, cx| {
                view.mcp_error = written.err().map(|error| error.to_string());
                view.load_mcp_clients(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn remove_mcp_client(
        &mut self,
        client: mcp_discovery::Client,
        project: bool,
        cx: &mut Context<Self>,
    ) {
        let home = self.mcp_home.clone();
        let root = self.inspection_root.clone();
        let scope = if project {
            mcp_discovery::Scope::Project
        } else {
            mcp_discovery::Scope::User
        };
        let removed = cx
            .background_executor()
            .spawn(async move { mcp_discovery::remove(client, scope, &home, &root) });
        cx.spawn(async move |this, cx| {
            let removed = removed.await;
            this.update(cx, |view, cx| {
                view.mcp_error = removed.err().map(|error| error.to_string());
                view.load_mcp_clients(cx);
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn open_mcp_client_file(
        &mut self,
        client: mcp_discovery::Client,
        project: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let path = if project {
            client.project_config(&self.inspection_root)
        } else {
            client.user_config(&self.mcp_home)
        };
        let created = path.clone();
        let write = cx.background_executor().spawn(async move {
            if let Some(parent) = created.parent() {
                std::fs::create_dir_all(parent).ok()?;
            }
            if !created.is_file() {
                let text = if client.is_toml() {
                    ""
                } else {
                    "{}
"
                };
                std::fs::write(&created, text).ok()?;
            }
            Some(created)
        });
        let workspace = self.workspace.clone();
        let handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let file = write.await;
            if let Some(path) = file {
                handle
                    .update(cx, |_, window, cx| {
                        workspace
                            .update(cx, |workspace, cx| {
                                workspace.open_file(path, window, cx);
                            })
                            .ok();
                    })
                    .ok();
            } else {
                this.update(cx, |view, cx| {
                    view.mcp_error =
                        Some("the settings file of the client did not open".to_owned());
                    cx.notify();
                })
                .ok();
            }
        })
        .detach();
    }

    fn copy_mcp_client(
        &self,
        client: mcp_discovery::Client,
        transport: mcp_discovery::Transport,
        cx: &mut Context<Self>,
    ) {
        let (url, token) = match mcp_endpoint(cx) {
            Some((url, token)) => (Some(mcp_url(transport, &url)), Some(token)),
            None => (None, None),
        };
        let server = mcp_discovery::Server {
            command: settings_catalog::mcp_binary(&self.draft),
            url,
            token,
        };
        let text = mcp_discovery::snippet(client, transport, &server)
            .unwrap_or_else(|error| error.to_string());
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
}

/// The switch of one row of the Exposed Tools table, as the settings have it.
fn tool_choice(settings: &Settings, name: &str) -> McpToolChoice {
    settings
        .mcp_tool_settings
        .get(name)
        .copied()
        .unwrap_or_else(|| {
            let state = tool_gateway::default_state(name);
            McpToolChoice {
                enabled: state.enabled,
                router_only: state.router_only,
            }
        })
}

/// The row of a tool, made when the user first changes it.
fn tool_choice_entry<'a>(settings: &'a mut Settings, name: &'static str) -> &'a mut McpToolChoice {
    let state = tool_gateway::default_state(name);
    settings
        .mcp_tool_settings
        .entry(name.to_string())
        .or_insert(McpToolChoice {
            enabled: state.enabled,
            router_only: state.router_only,
        })
}

/// The address of the endpoint of one transport: the SSE event stream sits next to the
/// Streamable HTTP endpoint of the same window.
fn mcp_url(transport: mcp_discovery::Transport, url: &str) -> String {
    if transport == mcp_discovery::Transport::Sse {
        return url
            .strip_suffix("/mcp")
            .map_or_else(|| format!("{url}/sse"), |base| format!("{base}/sse"));
    }
    url.to_owned()
}

/// The part of the selector of a transport.
fn transport_id(transport: mcp_discovery::Transport) -> &'static str {
    match transport {
        mcp_discovery::Transport::HttpStream => "http",
        mcp_discovery::Transport::Stdio => "stdio",
        mcp_discovery::Transport::Sse => "sse",
    }
}

/// One colour of the strip a theme card shows: its background, its text and its accent.
fn colour_strip(color: Hsla) -> Div {
    div().w(px(24.)).h(px(10.)).rounded(px(2.)).bg(color)
}

/// A line that says a control of the page does not wait for Apply.
fn note(text: &'static str, theme: &Theme) -> Div {
    div()
        .text_size(px(TEXT_SMALL))
        .text_color(theme.text_secondary)
        .child(text)
}

/// [`note`] for a text that comes from the page rather than from a literal.
fn note_owned(text: String, theme: &Theme) -> Div {
    div()
        .text_size(px(TEXT_SMALL))
        .text_color(theme.text_secondary)
        .child(text)
}

impl Render for SettingsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let settings = self.draft.clone();
        let query = self.search.text.trim().to_owned();
        let viewport = window.viewport_size();
        let default = SettingsWindowPlacement {
            left: (f32::from(viewport.width)
                - f32::from(px(WIDTH).min(viewport.width - px(2. * DIALOG_PADDING))))
                / 2.,
            top: f32::from(viewport.height) * 0.08,
            width: f32::from(px(WIDTH).min(viewport.width - px(2. * DIALOG_PADDING))),
            height: f32::from(viewport.height) * 0.8,
        };
        let placement = cx
            .try_global::<SettingsWindowPlacement>()
            .copied()
            .unwrap_or(default);

        let on_off = |on: bool| if on { "On" } else { "Off" };
        let navigation = settings_catalog::pages_matching(&query);
        let pages: Vec<_> = navigation
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
                        this.search_editing = false;
                        if this.page == SettingsPage::Mcp && this.mcp_clients_loaded == 0 {
                            this.load_mcp_clients(cx);
                        }
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
                content.push(note("Changes on this page apply at once.", &theme));
                content.push(self.keymap_page.render(window, cx));
            }
            SettingsPage::EditorLiveTemplates => {
                content.push(note("Changes on this page apply at once.", &theme));
                content.push(
                    self.templates_page
                        .render(TemplateSection::Live, window, cx),
                );
            }
            SettingsPage::EditorPostfix => {
                content.push(note("Changes on this page apply at once.", &theme));
                content.push(
                    self.templates_page
                        .render(TemplateSection::Postfix, window, cx),
                );
            }
            SettingsPage::EditorFileTemplates => {
                content.push(note("Changes on this page apply at once.", &theme));
                content.push(
                    self.templates_page
                        .render(TemplateSection::Files, window, cx),
                );
            }
            SettingsPage::EditorInspections => {
                content.extend(self.render_inspections(&theme, cx));
            }
            SettingsPage::EditorSpelling => {
                content.extend(self.render_spelling(&theme, cx));
            }
            _ => {
                if self.page == SettingsPage::Project {
                    content.push(div().child(project_sdk_row(&self.sdk, &theme, cx)));
                    content.push(note("The project SDK applies at once.", &theme));
                }
                if self.page == SettingsPage::Appearance {
                    content.push(note("The theme and the icons apply at once.", &theme));
                    content.extend(self.theme_section(&theme, cx));
                }
                if self.page == SettingsPage::Debugger {
                    content.push(note(
                        "Type renderers: type = display | children, one rule per semicolon.",
                        &theme,
                    ));
                }
                if self.page == SettingsPage::EditorCodeStyle {
                    content.push(self.project_style_row(&theme, cx));
                }
                let mut last_section: Option<&'static str> = None;
                for entry in settings_catalog::all()
                    .iter()
                    .filter(|entry| entry.page == self.page)
                {
                    if entry.field == SettingField::IconSet {
                        // The Icons section above draws the choices as cards, as the
                        // themes are drawn: a click applies the set at once.
                        continue;
                    }
                    if self.page == SettingsPage::Mcp
                        && matches!(
                            entry.field,
                            SettingField::McpEnabled
                                | SettingField::McpBinary
                                | SettingField::McpExecutePermission
                                | SettingField::McpRouterOnlyMode
                                | SettingField::McpRouterOnlyScope
                                | SettingField::McpTerminalSuggestions
                        )
                    {
                        // The MCP page draws these itself, in the order of IDEA.
                        continue;
                    }
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
                                let label = self.theme_label(&theme);
                                pill("theme", &label, true, &theme)
                                    .debug_selector(|| "settings-theme".to_owned())
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.next_theme(&theme, cx)
                                    }))
                                    .into_any_element()
                            } else {
                                let id = entry.id;
                                let label = settings_catalog::choice_label(&settings, id)
                                    .unwrap_or_default();
                                pill(id, &label, true, &theme)
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
                                SettingField::RightMargin => workspace::RIGHT_MARGIN_STEP as f32,
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
                                    &TextField::from_text(self.repository.clone()),
                                    "Read from the settings file",
                                    false,
                                    &theme,
                                )
                                .into_any_element()
                            } else {
                                let id = entry.id;
                                let current =
                                    settings_catalog::text(&settings, id).unwrap_or_default();
                                let hint = text_hint(entry.field);
                                let active = self.editing == Some(id);
                                let start = current.clone();
                                let typed;
                                let value = if active {
                                    &self.text_field
                                } else {
                                    typed = TextField::from_text(current);
                                    &typed
                                };
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
                                let input = field(id, value, hint, active, &theme)
                                    .debug_selector(move || {
                                        format!("settings-{}", id.replace('_', "-"))
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.editing = Some(id);
                                        this.search_editing = false;
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
                    let highlighted = self.highlight == Some(entry.id)
                        || (!query.is_empty() && entry.matches(&query));
                    content.push(setting_row(
                        entry,
                        control,
                        highlighted,
                        self.highlight == Some(entry.id),
                        &theme,
                    ));
                }
                if self.page == SettingsPage::Appearance {
                    content.extend(self.icon_section(&theme, cx));
                }
                if self.page == SettingsPage::Mcp {
                    content.extend(self.mcp_section(&settings, &theme, cx));
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
        .flex_1()
        .min_h_0();
        let pages = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .w(px(160.))
            .flex_none()
            .h_full()
            .child(self.search_field(&theme, cx))
            .child(pages);
        let content = scrollable(
            "settings-content",
            &self.scroll_content,
            div()
                .id("settings-content")
                .flex()
                .flex_col()
                .gap(px(SPACE_XS))
                // The space above and below the page is padding: the scroll range counts the
                // padding of the scrolled element, but not the margin of its first child, so
                // the last section stopped under the buttons at the end of the scroll.
                .pt(px(DIALOG_GAP))
                .pb(px(DIALOG_GAP))
                .overflow_y_scroll()
                .track_scroll(&self.scroll_content)
                .child(heading(self.page.title(), &theme).mt(px(0.)))
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

        let changed = self.changed();
        let ok = dialog_button(
            "settings-ok",
            "settings-ok".to_owned(),
            "OK",
            true,
            true,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.apply_draft(true, window, cx)));
        let apply = dialog_button(
            "settings-apply",
            "settings-apply".to_owned(),
            "Apply",
            false,
            changed,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.apply_draft(false, window, cx)));
        let cancel = dialog_button(
            "settings-cancel",
            "settings-cancel".to_owned(),
            "Cancel",
            false,
            true,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)));
        let buttons = div()
            .flex()
            .justify_end()
            .flex_none()
            .gap(px(SPACE_M))
            .child(ok)
            .child(cancel)
            .child(apply);

        let close = icon_button("settings-close", "general/close", "Close", None, &theme)
            .debug_selector(|| "settings-close".to_owned())
            .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)));
        let title = div()
            .id("settings-title")
            .debug_selector(|| "settings-title".to_owned())
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            // The close button is a control of the header height: a shorter title would
            // leave it sticking out of the bar that the pointer drags.
            .h(px(CONTROL_HEIGHT))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |this, event: &MouseDownEvent, _, cx| {
                    this.dragging = Some(PlacementDrag {
                        from: event.position,
                        placement,
                    });
                    cx.notify();
                }),
            )
            .on_mouse_move(
                cx.listener(|this, event: &MouseMoveEvent, _, cx| this.drag_window(event, cx)),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_drag(cx)),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _, _, cx| this.finish_drag(cx)),
            )
            .child(
                div()
                    .text_size(px(TEXT_TITLE))
                    .font_weight(FontWeight::MEDIUM)
                    .child("Settings"),
            )
            .child(close);

        let catcher = (self.dragging.is_some() || self.resizing.is_some()).then(|| {
            div()
                .id("settings-drag-catcher")
                .debug_selector(|| "settings-drag-catcher".to_owned())
                .absolute()
                .left(px(-placement.left))
                .top(px(-placement.top))
                .w(viewport.width)
                .h(viewport.height)
                .occlude()
                .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                    this.drag_window(event, cx);
                    this.resize_window(event, window, cx);
                }))
                .on_mouse_up(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.finish_drag(cx)),
                )
                .on_mouse_up_out(
                    MouseButton::Left,
                    cx.listener(|this, _, _, cx| this.finish_drag(cx)),
                )
        });

        let body = surface(&theme)
            .id("settings")
            .debug_selector(|| "settings".to_owned())
            .key_context("Settings")
            .track_focus(&self.focus)
            .occlude()
            .w(px(placement.width))
            .h(px(placement.height))
            .p(px(DIALOG_PADDING))
            .gap(px(SPACE_XS))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_action(cx.listener(|this, _: &workspace::Dismiss, window, cx| {
                if this.page == SettingsPage::Keymap && this.keymap_page.escape(cx) {
                    return;
                }
                if TemplateSection::section(this.page).is_some() && this.templates_page.escape(cx) {
                    return;
                }
                if this.search.text.is_empty() {
                    this.close(window, cx);
                } else {
                    this.search = TextField::default();
                    cx.notify();
                }
            }))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)),
            )
            .child(title)
            .child(section)
            .child(buttons)
            .child(resize_grip(
                "settings-resize-right",
                ResizeEdge::Right,
                placement,
                cx,
            ))
            .child(resize_grip(
                "settings-resize-bottom",
                ResizeEdge::Bottom,
                placement,
                cx,
            ))
            .child(resize_grip(
                "settings-resize-corner",
                ResizeEdge::Corner,
                placement,
                cx,
            ));
        div()
            .absolute()
            .left(px(placement.left))
            .top(px(placement.top))
            .child(enter("settings-enter", body))
            .children(catcher)
    }
}

#[cfg(test)]
mod tests {
    use plugin_host::PluginSettings;

    use super::{PluginRow, filter_shortcuts, read_plugins};

    #[test]
    fn plugins_are_listed_by_folder_with_their_switch() {
        let dir = test_support::scratch("settings-plugins");
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

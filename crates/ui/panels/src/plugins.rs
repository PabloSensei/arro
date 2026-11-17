//! User plugins in the window: their commands in Find Action and on their keys, their
//! themes, the plugin list, and the answers to what a plugin asks of the window. The
//! plugins themselves run in `services/plugin-host`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::Receiver;
use std::time::Duration;

use editor_view::EditorView;
use plugin_host::{
    ActiveDocument, ContributedCommand, HostRequest, IdeCall, PluginHost, PluginSettings,
    PluginState, PluginTheme, Timeouts, default_plugins_dir,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

const CONTEXT: &str = "ListPopup";
const WIDTH: f32 = 600.;
const HEIGHT: f32 = 420.;
const POLL: Duration = Duration::from_millis(30);
pub(crate) const SETTINGS_FILE: &str = "plugins.toml";
const MANAGE: &str = "plugins.manage";

actions!(plugins, [Previous, Next, Choose]);

/// Switches a plugin on or off from the list.
#[derive(Clone, PartialEq, Action)]
#[action(namespace = plugins, no_json)]
pub struct TogglePlugin {
    pub id: SharedString,
}

macro_rules! plugin_list_keys {
    ($(($keys:literal, $action:ident, $mac:expr)),* $(,)?) => {
        pub fn bind_keys(cx: &mut App) {
            bind_keys_for(cx, keymap::platform());
        }

        /// Binds the keys of one platform, so that a test reads the macOS table on every
        /// runner.
        fn bind_keys_for(cx: &mut App, platform: keymap::Platform) {
            $(
                let row = workspace::KeyRow {
                    keys: $keys,
                    mac: Some($mac),
                    action: concat!("plugins::", stringify!($action)),
                    context: Some(CONTEXT),
                    idea: None,
                };
                for key in row.keys_for(platform) {
                    cx.bind_keys([KeyBinding::new(key, $action, Some(CONTEXT))]);
                }
            )*
        }

        #[cfg(test)]
        pub(crate) const KEYS: &[workspace::KeyRow] = &[
            $(workspace::KeyRow {
                keys: $keys,
                mac: Some($mac),
                action: concat!("plugins::", stringify!($action)),
                context: Some(CONTEXT),
                idea: None,
            }),*
        ];
    };
}

plugin_list_keys![
    ("up", Previous, "up"),
    ("down", Next, "down"),
    ("enter", Choose, "enter"),
];

/// The plugins of the window, for scripts.
pub struct ActivePlugins(pub Entity<Plugins>);

impl Global for ActivePlugins {}

pub struct Plugins {
    host: Arc<PluginHost>,
    commands: Vec<ContributedCommand>,
    themes: Vec<PluginTheme>,
    settings: PluginSettings,
    settings_path: PathBuf,
    /// What went wrong while loading: broken themes, keys that could not be bound.
    problems: Vec<String>,
    /// How the last command ended, for scripts.
    last: Option<String>,
    workspace: WeakEntity<Workspace>,
    window: AnyWindowHandle,
}

/// The registry keeps `&'static str` ids; plugins load once per run of the IDE.
fn leak(id: String) -> &'static str {
    id.leak()
}

impl Plugins {
    /// Loads the plugins and puts their commands, keys and themes into the workspace, so
    /// that Find Action and the keymap reach them like any command of the window.
    pub fn create(
        workspace: &mut Workspace,
        window: AnyWindowHandle,
        cx: &mut Context<Workspace>,
    ) -> Entity<Self> {
        let dir = default_plugins_dir().unwrap_or_default();
        // Beside the folder of the plugins, `%APPDATA%/Arro/plugins.toml`.
        let settings_path = dir.parent().unwrap_or(&dir).join(SETTINGS_FILE);
        let mut problems = Vec::new();
        let settings = PluginSettings::load(&settings_path).unwrap_or_else(|error| {
            problems.push(error);
            PluginSettings::default()
        });
        // Reading the manifests is quick, the processes start with the first command.
        let (host, requests) = PluginHost::load(&dir, &settings, Timeouts::default());
        let mut themes = Vec::new();
        for contributed in host.themes() {
            match contributed.theme {
                Ok(theme) => themes.push(theme),
                Err(error) => problems.push(format!("{}: {error}", contributed.plugin)),
            }
        }
        let commands = host.commands();
        let weak = cx.weak_entity();
        // Without a command no plugin process ever starts, and nothing has to be answered.
        let polling = !commands.is_empty();
        let plugins = cx.new(|cx| {
            if polling {
                cx.spawn(async move |this, cx| {
                    loop {
                        cx.background_executor().timer(POLL).await;
                        let answered = this.update(cx, |this: &mut Self, cx| {
                            this.answer(&requests, cx);
                        });
                        if answered.is_err() {
                            break;
                        }
                    }
                })
                .detach();
            }
            Self {
                host: Arc::new(host),
                commands: commands.clone(),
                themes,
                settings,
                settings_path,
                problems: Vec::new(),
                last: None,
                workspace: weak,
                window,
            }
        });
        problems.extend(register(&plugins, &commands, workspace, cx));
        if !problems.is_empty() {
            workspace.notify_user("Plugins", problems.join("\n"), cx);
        }
        plugins.update(cx, |plugins, _| plugins.problems = problems);
        plugins
    }

    /// Lines for the report of a script.
    pub fn report(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for plugin in self.host.plugins() {
            lines.push(format!(
                "plugin {} {}",
                plugin.id(),
                state_text(&plugin.state())
            ));
        }
        for command in &self.commands {
            lines.push(format!("plugin command {} {}", command.id, command.title));
        }
        for theme in &self.themes {
            lines.push(format!("plugin theme {}", theme.name));
        }
        for problem in &self.problems {
            lines.push(format!("plugin problem {problem}"));
        }
        if let Some(last) = &self.last {
            lines.push(format!("plugin last {last}"));
        }
        lines
    }

    /// The command waits for the plugin, which may ask the window things meanwhile, so it
    /// runs in the background.
    fn execute(&mut self, id: &'static str, cx: &mut Context<Self>) {
        let host = self.host.clone();
        let run = cx
            .background_executor()
            .spawn(async move { host.execute(id) });
        cx.spawn(async move |this, cx| {
            let result = run.await;
            this.update(cx, |this, cx| {
                match result {
                    Ok(()) => this.last = Some(format!("{id} ok")),
                    Err(error) => {
                        this.last = Some(format!("{id} failed: {error}"));
                        this.tell(format!("{id} failed"), error.to_string(), cx);
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// The theme of a plugin over the built-in one, and the tokens it names that do not
    /// exist.
    fn theme(&mut self, index: usize) -> Option<(Theme, Vec<String>)> {
        let theme = self.themes.get(index)?;
        let colors = theme.colors.iter().map(|(name, hex)| (name.as_str(), *hex));
        let applied = Theme::with_colors(theme.dark, colors);
        self.last = Some(format!("theme {}", theme.name));
        Some(applied)
    }

    /// Switching a plugin takes a restart: its commands are bound once per run.
    fn toggle(&mut self, id: &str, cx: &mut Context<Self>) {
        if !self.settings.disabled.remove(id) {
            self.settings.disabled.insert(id.to_owned());
        }
        let off = self.settings.disabled.contains(id);
        let text = match self.settings.save(&self.settings_path) {
            Ok(()) if off => format!("{id} is off after a restart"),
            Ok(()) => format!("{id} is on after a restart"),
            Err(error) => format!("Not saved: {error}"),
        };
        self.tell("Plugins", text, cx);
    }

    fn answer(&mut self, requests: &Receiver<HostRequest>, cx: &mut Context<Self>) {
        while let Ok(request) = requests.try_recv() {
            let (workspace, window) = (self.workspace.clone(), self.window);
            cx.defer(move |cx| {
                let handled = window.update(cx, |_, window, cx| {
                    workspace.update(cx, |workspace, cx| {
                        handle(request, workspace, window, cx);
                    })
                });
                if handled.is_err() {
                    eprintln!("a plugin call came after the window closed");
                }
            });
        }
    }

    fn tell(
        &self,
        title: impl Into<SharedString>,
        text: impl Into<SharedString>,
        cx: &mut Context<Self>,
    ) {
        let (title, text) = (title.into(), text.into());
        let workspace = self.workspace.clone();
        cx.defer(move |cx| {
            workspace
                .update(cx, |workspace, cx| workspace.notify_user(title, text, cx))
                .ok();
        });
    }

    /// The plugin list: each row switches its plugin on or off.
    fn list(&self) -> Vec<PopupEntry> {
        let rows = self.host.plugins().iter().map(|plugin| {
            let id = plugin.id();
            let name = plugin.manifest().map_or(id.clone(), |manifest| {
                format!("{} {}", manifest.name, manifest.version)
            });
            let state = state_text(&plugin.state());
            let wanted = if self.settings.disabled.contains(&id) {
                ", off after a restart"
            } else {
                ""
            };
            PopupEntry {
                title: name.into(),
                hint: Some(format!("{state}{wanted}").into()),
                action: Box::new(TogglePlugin { id: id.into() }),
            }
        });
        rows.collect()
    }
}

/// Commands of the plugins, one per theme and the plugin list, with the keys of the
/// manifests. Returns what could not be bound.
fn register(
    plugins: &Entity<Plugins>,
    commands: &[ContributedCommand],
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) -> Vec<String> {
    let mut problems = Vec::new();
    let mut known = HashSet::new();
    for command in commands {
        let id = leak(command.id.clone());
        known.insert(id);
        let state = plugins.clone();
        workspace.register_command(id, command.title.clone(), move |_, _, cx| {
            state.update(cx, |plugins, cx| plugins.execute(id, cx));
        });
        if let Some(key) = &command.key
            && let Err(error) = workspace.bind_command(key, id)
        {
            problems.push(format!("{id}: {key}: {error}"));
        }
    }
    let bindings = plugins.read(cx).host.keybindings();
    for binding in bindings {
        // The keymap of the window has the workspace context only: a key bound for the
        // editor works there too, as long as the editor does not take it first.
        match known.get(binding.command.as_str()) {
            Some(id) => {
                if let Err(error) = workspace.bind_command(&binding.key, id) {
                    problems.push(format!("{}: {}: {error}", binding.command, binding.key));
                }
            }
            None => problems.push(format!("{}: no such command", binding.command)),
        }
    }
    let names: Vec<String> = plugins
        .read(cx)
        .themes
        .iter()
        .map(|theme| theme.name.clone())
        .collect();
    for (index, name) in names.into_iter().enumerate() {
        let id = leak(format!("plugins.theme.{index}"));
        let state = plugins.clone();
        let title = format!("Theme: {name}");
        workspace.register_command(id, title, move |workspace, _, cx| {
            let Some((theme, unknown)) = state.update(cx, |plugins, _| plugins.theme(index)) else {
                return;
            };
            if !unknown.is_empty() {
                let text = format!("No such tokens: {}", unknown.join(", "));
                workspace.notify_user(name.clone(), text, cx);
            }
            workspace.set_theme(theme, cx);
        });
    }
    let state = plugins.clone();
    workspace.register_command(MANAGE, "Plugins", move |workspace, window, cx| {
        let entries = state.read(cx).list();
        ListPopup::show("Plugins", entries, workspace, window, cx);
    });
    let state = plugins.clone();
    let app: &mut App = cx;
    app.on_action(move |action: &TogglePlugin, cx| {
        let id = action.id.clone();
        state.update(cx, |this, cx| this.toggle(&id, cx));
    });
    problems
}

fn state_text(state: &PluginState) -> String {
    match state {
        PluginState::Broken(reason) => format!("broken: {reason}"),
        PluginState::Disabled => "off".into(),
        PluginState::NotStarted => "not started".into(),
        PluginState::Running => "running".into(),
        PluginState::Failed(reason) => format!("failed: {reason}"),
    }
}

/// One call of a plugin. It gets an answer whatever happens: a dropped request answers
/// with an error by itself.
fn handle(
    request: HostRequest,
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let editor = workspace
        .active_editor()
        .and_then(|(path, view)| Some((path, view.downcast::<EditorView>().ok()?)));
    match request.call.clone() {
        IdeCall::ActiveDocument => {
            let document = editor.and_then(|(path, view)| {
                let view = view.read(cx);
                let (range, _) = view.selection()?;
                Some(ActiveDocument::new(Some(path), view.text()?, range))
            });
            request.answer(document);
        }
        IdeCall::ReplaceSelection { text } | IdeCall::InsertText { text } => {
            let insert = matches!(request.call, IdeCall::InsertText { .. });
            let Some((_, view)) = editor else {
                request.fail("no editor is active");
                return;
            };
            let Some((range, caret)) = view.read(cx).selection() else {
                request.fail("the editor is still loading");
                return;
            };
            let range = if insert { caret..caret } else { range };
            view.update(cx, |view, cx| view.replace_text(range, &text, cx));
            request.done();
        }
        IdeCall::OpenFile { path, .. } => {
            // The line is not used yet: the editor has no way to go to a line from outside.
            let path = if path.is_absolute() {
                path
            } else {
                workspace.root().join(path)
            };
            if !path.is_file() {
                request.fail(format!("{} is no file", path.display()));
                return;
            }
            workspace.open_file(path, window, cx);
            request.done();
        }
        IdeCall::ShowMessage { message, .. } => {
            // There is no status bar text yet, so every message is a notification.
            let title = request.plugin.clone();
            workspace.notify_user(title, message, cx);
            request.done();
        }
    }
}

pub struct PopupEntry {
    title: SharedString,
    hint: Option<SharedString>,
    action: Box<dyn Action>,
}

/// A list in the middle of the window that runs the action of the chosen row.
pub struct ListPopup {
    title: &'static str,
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    selected: usize,
    entries: Vec<PopupEntry>,
    scroll: ScrollHandle,
}

impl ListPopup {
    fn show(
        title: &'static str,
        entries: Vec<PopupEntry>,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            title,
            workspace: weak,
            focus: cx.focus_handle(),
            selected: 0,
            entries,
            scroll: ScrollHandle::new(),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let last = self.entries.len().saturating_sub(1) as isize;
        self.selected = (self.selected as isize + delta).clamp(0, last) as usize;
        cx.notify();
    }

    fn choose(&mut self, row: usize, window: &mut Window, cx: &mut Context<Self>) {
        let action = self
            .entries
            .get(row)
            .map(|entry| entry.action.boxed_clone());
        self.dismiss(window, cx);
        if let Some(action) = action {
            window.dispatch_action(action, cx);
        }
    }

    fn dismiss(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn row(&self, row: usize, theme: &Theme, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let entry = &self.entries[row];
        let selected = row == self.selected;
        let hover = theme.selection_hover;
        div()
            .id(("list-popup-row", row))
            .flex()
            .items_center()
            .justify_between()
            .flex_none()
            .h(px(ROW_HEIGHT))
            .px(px(ROW_PADDING_X))
            .rounded(px(ROW_RADIUS))
            .when(selected, |this| this.bg(theme.selection))
            .when(!selected, |this| this.hover(move |style| style.bg(hover)))
            .on_click(cx.listener(move |this, _, window, cx| this.choose(row, window, cx)))
            .child(entry.title.clone())
            .children(
                entry
                    .hint
                    .clone()
                    .map(|hint| div().text_color(theme.text_secondary).child(hint)),
            )
    }
}

impl Render for ListPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.205;
        let rows: Vec<_> = (0..self.entries.len())
            .map(|row| self.row(row, &theme, cx))
            .collect();
        let empty = self.entries.is_empty();
        let body = surface(&theme)
            .id("list-popup")
            .debug_selector(|| "list-popup".to_owned())
            .key_context(CONTEXT)
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .h(px(HEIGHT))
            .p(px(POPUP_PADDING))
            .on_action(cx.listener(|this, _: &Previous, _, cx| this.move_selection(-1, cx)))
            .on_action(cx.listener(|this, _: &Next, _, cx| this.move_selection(1, cx)))
            .on_action(
                cx.listener(|this, _: &Choose, window, cx| this.choose(this.selected, window, cx)),
            )
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.dismiss(window, cx)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .font_weight(FontWeight::MEDIUM)
                    .child(self.title),
            )
            .child(
                scrollable(
                    "list-popup-rows",
                    &self.scroll,
                    div()
                        .id("list-popup-rows")
                        .flex()
                        .flex_col()
                        .pb(px(POPUP_PADDING))
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows)
                        .when(empty, |this| {
                            this.items_center()
                                .justify_center()
                                .text_color(theme.text_secondary)
                                .child("No plugins")
                        }),
                    &theme,
                    window,
                    cx,
                )
                .flex_1()
                .min_h_0(),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("list-popup-enter", body))
    }
}

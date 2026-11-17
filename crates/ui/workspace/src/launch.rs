//! Opening the application: state, theme, the window and its saved placement.

use std::path::PathBuf;
use std::sync::Arc;

use store::Store;
use ui_kit::*;

use crate::editor::FileIcons;
use crate::external::{OpenRequests, Request};
use crate::layout::{
    Layout, WindowPlacement, load_layout, load_recent, load_theme, remember_project,
};
use crate::settings::{SETTINGS_FILE, Settings};
use crate::window::{Workspace, bind_keys};

const DEFAULT_SIZE: (f32, f32) = (1600., 1000.);
const DATABASE: &str = "state.sqlite";

/// What the About window and the title bar know about this build.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AppInfo {
    pub name: &'static str,
    pub version: &'static str,
    pub commit: &'static str,
    pub date: &'static str,
    pub channel: &'static str,
}

impl Default for AppInfo {
    fn default() -> Self {
        Self {
            name: "Arro",
            version: env!("CARGO_PKG_VERSION"),
            commit: "unknown",
            date: "unknown",
            channel: "dev",
        }
    }
}

impl AppInfo {
    /// The short label next to the project name.
    pub fn channel_label(&self) -> &'static str {
        if self.channel == "dev" {
            "Dev"
        } else {
            "Preview"
        }
    }

    /// One line for a bug report.
    pub fn report_line(&self) -> String {
        format!(
            "{} {} ({}), commit {}, built {}, on {} {}",
            self.name,
            self.version,
            self.channel,
            self.commit,
            self.date,
            std::env::consts::OS,
            std::env::consts::ARCH
        )
    }
}

pub struct Launch {
    /// The project folder.
    pub root: PathBuf,
    /// A file to open on top of the saved layout.
    pub open: Option<PathBuf>,
    /// Folder of the state database.
    pub state_dir: PathBuf,
    /// Folder of the settings file the user edits.
    pub settings_dir: PathBuf,
    pub icons: FileIcons,
    /// Shows the window without activating it: scripted runs must not take the focus.
    pub background: bool,
    /// Version and channel of this build.
    pub info: AppInfo,
}

/// What a window without a project needs: the saved theme, the recent projects and the
/// settings.
pub struct StartContext {
    pub dark: bool,
    /// The theme the first frame shows, by the name the settings save.
    pub theme: Theme,
    /// The themes the settings folder offers.
    pub themes: ThemeCatalog,
    pub recent: Vec<PathBuf>,
    pub settings: Settings,
}

pub fn start_context(state_dir: &std::path::Path, settings_dir: &std::path::Path) -> StartContext {
    let store = Store::open(&state_dir.join(DATABASE)).ok();
    let theme = store.as_ref().map_or_else(Default::default, load_theme);
    let settings = settings_at(settings_dir, theme.chosen);
    let themes = ThemeCatalog::load(settings_dir);
    let id = if settings.theme_sync_with_os {
        settings.theme_for(theme.dark).to_owned()
    } else if settings.theme.is_empty() {
        if theme.dark { "dark" } else { "light" }.to_owned()
    } else {
        settings.theme.clone()
    };
    let shown = Theme::from_library(themes.library(), &id).with_icon_set(&settings.icon_set);
    StartContext {
        dark: theme.dark,
        theme: shown,
        themes,
        recent: store.as_ref().map_or_else(Vec::new, load_recent),
        settings,
    }
}

/// The settings file of a start. A file written before Sync with OS existed keeps a theme
/// the user already chose; a start without one follows the system.
fn settings_at(settings_dir: &std::path::Path, theme_chosen: bool) -> Settings {
    Settings::load_with_theme_choice(&settings_dir.join(SETTINGS_FILE), theme_chosen)
}

/// Starts the program of a project in a window of its own. Blocks only for the start.
pub fn spawn_project(state_dir: &std::path::Path, folder: &std::path::Path) -> std::io::Result<()> {
    spawn_project_after(state_dir, folder, None)
}

/// [`spawn_project`] with the process this one replaces. The new program waits for it
/// to end before it opens the project, so that the same files and folders are free.
pub fn spawn_project_after(
    state_dir: &std::path::Path,
    folder: &std::path::Path,
    replaced: Option<u32>,
) -> std::io::Result<()> {
    spawn_project_from(std::env::current_exe()?, state_dir, folder, replaced)
}

/// [`spawn_project_after`] with the program to start. An update renames the running
/// executable or replaces the bundle around it, so the new process is started from the
/// path the update itself wrote.
pub fn spawn_project_from(
    program: PathBuf,
    state_dir: &std::path::Path,
    folder: &std::path::Path,
    replaced: Option<u32>,
) -> std::io::Result<()> {
    let mut command = spawn_command(program, state_dir);
    command.args(project_arguments(folder, replaced));
    command.spawn().map(|_| ())
}

/// The arguments of a start that opens a project in a window of its own. Such a start
/// does not hand the folder to the program that already listens: that one would start a
/// second process for the folder and leave, a whole start too late. A start after an
/// update is the program that listens next: the old one is on its way out.
fn project_arguments(folder: &std::path::Path, replaced: Option<u32>) -> Vec<std::ffi::OsString> {
    let mut arguments = Vec::with_capacity(3);
    match replaced {
        Some(process_id) => {
            arguments.push("--after-update".into());
            arguments.push(process_id.to_string().into());
        }
        None => arguments.push("--new-instance".into()),
    }
    arguments.push(folder.into());
    arguments
}

/// Starts a program of its own for a path: a project folder, or a file, which opens in
/// the folder that holds it. The path is not handed to a running program: this start is
/// the window of its own.
pub fn spawn_path(state_dir: &std::path::Path, path: &std::path::Path) -> std::io::Result<()> {
    let mut command = spawn_command(std::env::current_exe()?, state_dir);
    command.arg("--new-instance").arg(path).spawn().map(|_| ())
}

/// [`spawn_path`] for the start screen, which closes after it: this program listens for
/// the next start as soon as the start screen lets the state folder go.
pub fn spawn_path_taking_over(
    state_dir: &std::path::Path,
    path: &std::path::Path,
) -> std::io::Result<()> {
    let mut command = spawn_command(std::env::current_exe()?, state_dir);
    command.arg("--take-over").arg(path).spawn().map(|_| ())
}

fn spawn_command(program: PathBuf, state_dir: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new(program);
    command.arg("--state-dir").arg(state_dir);
    command
}

fn open_store(launch: &Launch) -> Option<Arc<Store>> {
    match Store::open(&launch.state_dir.join(DATABASE)) {
        Ok(store) => Some(Arc::new(store)),
        Err(error) => {
            eprintln!("state database is not available: {error}");
            None
        }
    }
}

/// Saved bounds and their display. They count only if that display is still there:
/// GPUI drops bounds that lie outside the display it is given. The layout keeps the
/// bounds in the logical pixels of the display, whatever the interface scale was when
/// they were written, so the window keeps its size on screen after a change of it.
fn restored_bounds(
    placement: Option<WindowPlacement>,
    cx: &App,
) -> Option<(WindowBounds, DisplayId)> {
    let placement = placement?;
    let stored = Bounds::new(
        point(px(placement.x), px(placement.y)),
        size(px(placement.width), px(placement.height)),
    );
    let center = stored.center();
    let displays = cx.displays();
    let display = displays
        .iter()
        .find(|display| display.bounds().contains(&center))?;
    let scale = window_scale(
        cx.try_global::<Settings>()
            .map_or(1., |settings| settings.ui_scale),
    );
    let bounds = Bounds::new(
        point(stored.left() / scale, stored.top() / scale),
        size(stored.size.width / scale, stored.size.height / scale),
    );
    let bounds = if placement.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };
    Some((bounds, display.id()))
}

/// Tells the keymap and the hints which system this process follows: the Command key of
/// macOS, the Control key of the others. Called once, before the first window opens.
pub fn choose_platform_keymap() {
    keymap::set_platform(keymap::Platform::current());
}

/// Runs the application until the window closes. `setup` adds the panels and must
/// give the workspace its editor factory. `requests` brings the paths the system opens
/// and the paths of a second start to the window.
pub fn run(
    launch: Launch,
    requests: OpenRequests,
    setup: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) {
    let store = open_store(&launch);
    let layout = store
        .as_ref()
        .map_or_else(Layout::default, |store| load_layout(store, &launch.root));
    let theme = store
        .as_ref()
        .map_or_else(Default::default, |store| load_theme(store));
    let dark = theme.dark;
    let recent = store.as_ref().map_or_else(Vec::new, |store| {
        if let Err(error) = remember_project(store, &launch.root) {
            eprintln!("recent projects are not saved: {error}");
        }
        load_recent(store)
    });
    let focus = !launch.background;
    let settings = settings_at(&launch.settings_dir, theme.chosen);

    // The Finder hands a document to the delegate of the running application, and the
    // window takes it from the same queue as the paths of a second start.
    let opened = requests.clone();
    let application = ui_kit::app();
    application.on_open_urls(move |urls| {
        for url in urls {
            if let Some(path) = ::platform::file_url_path(&url) {
                opened.push(Request::Open(path));
            }
        }
    });
    let watched = requests;
    application.run(move |cx| {
        ui_kit::init(dark, cx);
        let ui_scale = settings.ui_scale;
        cx.set_global(settings);
        ui_kit::set_ui_scale(ui_scale);
        bind_keys(cx);

        let (width, height) = DEFAULT_SIZE;
        let restored = restored_bounds(layout.window, cx);
        let (window_bounds, display_id) = restored.map_or_else(
            || {
                // The default size is the size on screen at the scale of 1, so a larger
                // interface puts fewer logical pixels in the same window.
                let scale = window_scale(ui_scale);
                let bounds =
                    Bounds::centered(None, size(px(width / scale), px(height / scale)), cx);
                (WindowBounds::Windowed(bounds), None)
            },
            |(bounds, display)| (bounds, Some(display)),
        );
        #[cfg(target_os = "macos")]
        let (title, decorations) = (Some("Arro".into()), None);
        #[cfg(all(unix, not(target_os = "macos")))]
        let (title, decorations) = (Some("Arro".into()), Some(WindowDecorations::Client));
        #[cfg(windows)]
        let (title, decorations) = (None, None);
        let options = WindowOptions {
            window_bounds: Some(window_bounds),
            display_id,
            focus,
            titlebar: Some(TitlebarOptions {
                title,
                appears_transparent: true,
                traffic_light_position: None,
            }),
            // Wayland and X11 have no titlebar of their own here: the window keeps the
            // toolbar of the application as its titlebar. The identifier groups the
            // windows under the icon of the desktop entry in the panel and the switcher.
            window_decorations: decorations,
            app_id: Some("arro".into()),
            #[cfg(unix)]
            app_owns_titlebar_drag: true,
            ..Default::default()
        };
        cx.open_window(options, move |window, cx| {
            cx.new(|cx| {
                let mut workspace = Workspace::new(launch, layout, store, recent, window, cx);
                setup(&mut workspace, window, cx);
                workspace.restore_tabs(window, cx);
                workspace.watch_open_requests(watched, window, cx);
                workspace
            })
        })
        .expect("open window");
    });
}

#[cfg(test)]
mod tests {
    use super::project_arguments;
    use std::ffi::OsString;
    use std::path::Path;

    /// A project that opens from a window is a window of its own at once: it must not
    /// hand the folder to the program that listens, which would start it again.
    #[test]
    fn a_project_opening_is_a_window_of_its_own() {
        let folder = Path::new("shop");
        let arguments = project_arguments(folder, None);
        assert_eq!(
            arguments,
            [
                OsString::from("--new-instance"),
                folder.as_os_str().to_owned()
            ]
        );
    }

    /// The start after an update waits for the old process and becomes the listener of
    /// the state folder: it must not hand the folder to the process that leaves.
    #[test]
    fn a_start_after_an_update_waits_and_listens() {
        let folder = Path::new("shop");
        let arguments = project_arguments(folder, Some(4242));
        assert_eq!(
            arguments,
            [
                OsString::from("--after-update"),
                OsString::from("4242"),
                folder.as_os_str().to_owned(),
            ]
        );
        assert!(!arguments.contains(&OsString::from("--new-instance")));
    }
}

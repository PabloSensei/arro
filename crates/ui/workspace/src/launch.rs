//! Opening the application: state, theme, the window and its saved placement.

use std::path::PathBuf;
use std::sync::Arc;

use store::Store;
use ui_kit::*;

use crate::editor::FileIcons;
use crate::layout::{
    Layout, WindowPlacement, load_dark_theme, load_layout, load_recent, remember_project,
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
    pub recent: Vec<PathBuf>,
    pub settings: Settings,
}

pub fn start_context(state_dir: &std::path::Path, settings_dir: &std::path::Path) -> StartContext {
    let store = Store::open(&state_dir.join(DATABASE)).ok();
    StartContext {
        dark: store.as_ref().is_none_or(load_dark_theme),
        recent: store.as_ref().map_or_else(Vec::new, load_recent),
        settings: Settings::load(&settings_dir.join(SETTINGS_FILE)),
    }
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
    let program = std::env::current_exe()?;
    let mut command = std::process::Command::new(program);
    command.arg("--state-dir").arg(state_dir);
    if let Some(process_id) = replaced {
        command.arg("--after-update").arg(process_id.to_string());
    }
    command.arg(folder).spawn().map(|_| ())
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
/// GPUI drops bounds that lie outside the display it is given.
fn restored_bounds(
    placement: Option<WindowPlacement>,
    cx: &App,
) -> Option<(WindowBounds, DisplayId)> {
    let placement = placement?;
    let bounds = Bounds::new(
        point(px(placement.x), px(placement.y)),
        size(px(placement.width), px(placement.height)),
    );
    let center = bounds.center();
    let displays = cx.displays();
    let display = displays
        .iter()
        .find(|display| display.bounds().contains(&center))?;
    let bounds = if placement.maximized {
        WindowBounds::Maximized(bounds)
    } else {
        WindowBounds::Windowed(bounds)
    };
    Some((bounds, display.id()))
}

/// Runs the application until the window closes. `setup` adds the panels and must
/// give the workspace its editor factory.
pub fn run(
    launch: Launch,
    setup: impl FnOnce(&mut Workspace, &mut Window, &mut Context<Workspace>) + 'static,
) {
    let store = open_store(&launch);
    let layout = store
        .as_ref()
        .map_or_else(Layout::default, |store| load_layout(store, &launch.root));
    let dark = store.as_ref().is_none_or(|store| load_dark_theme(store));
    let recent = store.as_ref().map_or_else(Vec::new, |store| {
        if let Err(error) = remember_project(store, &launch.root) {
            eprintln!("recent projects are not saved: {error}");
        }
        load_recent(store)
    });
    let focus = !launch.background;
    let settings = Settings::load(&launch.settings_dir.join(SETTINGS_FILE));

    ui_kit::app().run(move |cx| {
        ui_kit::init(dark, cx);
        cx.set_global(settings);
        bind_keys(cx);

        let (width, height) = DEFAULT_SIZE;
        let restored = restored_bounds(layout.window, cx);
        let (window_bounds, display_id) = restored.map_or_else(
            || {
                let bounds = Bounds::centered(None, size(px(width), px(height)), cx);
                (WindowBounds::Windowed(bounds), None)
            },
            |(bounds, display)| (bounds, Some(display)),
        );
        #[cfg(unix)]
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
            // toolbar of the application as its titlebar.
            window_decorations: decorations,
            #[cfg(unix)]
            app_owns_titlebar_drag: true,
            ..Default::default()
        };
        cx.open_window(options, move |window, cx| {
            cx.new(|cx| {
                let mut workspace = Workspace::new(launch, layout, store, recent, window, cx);
                setup(&mut workspace, window, cx);
                workspace.restore_tabs(window, cx);
                workspace
            })
        })
        .expect("open window");
    });
}

//! Where the program of a build tool comes from. The order is the same for every tool:
//! the wrapper of the project first, because it pins the version the project was written
//! for, then the home of the settings, then the copy the IDE downloaded, and `PATH` last.
//! The tools differ in the names of their scripts only, so the order lives here once.

use std::env;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use project_model::ToolHome;

use crate::tool_download;

/// The names of the scripts of one build tool.
pub(crate) struct Tool {
    /// The folder of the downloaded copies under `<data>/tools`.
    pub folder: &'static str,
    /// The name of the tool in the messages of the window.
    pub label: &'static str,
    /// The wrappers of the project, the first one is named in a message.
    pub wrappers: &'static [&'static str],
    /// The programs of a home under `bin`, and on `PATH`.
    pub programs: &'static [&'static str],
}

/// Where the tool of a project came from, for the messages of the settings and of a
/// failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ToolOrigin {
    /// The wrapper script of the project.
    Wrapper,
    /// The home of the settings or the folder the user chose.
    Configured,
    /// The copy Arro downloaded.
    Bundled,
    /// The program on `PATH`.
    Path,
}

/// The home of the tool a project runs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ToolLocation {
    /// The program the build starts.
    pub program: PathBuf,
    pub home: PathBuf,
    pub origin: ToolOrigin,
}

/// Why no home was found, in the words the status line and the notification use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MissingTool {
    label: &'static str,
    root: PathBuf,
    home: Option<PathBuf>,
    kind: ToolHome,
    wrapper: &'static str,
    program: &'static str,
}

impl std::fmt::Display for MissingTool {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (label, wrapper, program) = (self.label, self.wrapper, self.program);
        match self.kind {
            ToolHome::Wrapper => write!(formatter, "no {} in {}", wrapper, self.root.display()),
            ToolHome::Custom => match &self.home {
                Some(home) => write!(
                    formatter,
                    "no {} in {}",
                    program,
                    home.join("bin").display()
                ),
                None => write!(formatter, "no {label} home is chosen in the settings"),
            },
            ToolHome::Bundled => write!(
                formatter,
                "no {label} was downloaded yet; download it or choose another {label} home"
            ),
            ToolHome::Path => write!(formatter, "no {program} in PATH"),
            ToolHome::Auto => write!(
                formatter,
                "no {} in {}, no {label} home in the settings, no bundled {label} and no \
                 {program} in PATH",
                wrapper,
                self.root.display()
            ),
        }
    }
}

impl std::error::Error for MissingTool {}

/// The search of one build tool. `path` is the search path; `None` means the machine has
/// no `PATH`.
pub(crate) fn find(
    tool: &Tool,
    root: &Path,
    configured: Option<&Path>,
    data_dir: &Path,
    path: Option<&OsStr>,
    kind: ToolHome,
) -> Result<ToolLocation, MissingTool> {
    let configured_path = configured;
    let wrapper = wrapper_at(root, tool);
    let configured = configured.and_then(|home| home_at(home, tool, ToolOrigin::Configured));
    let bundled = tool_download::newest_home(data_dir, tool.folder, tool.programs)
        .and_then(|home| home_at(&home, tool, ToolOrigin::Bundled));
    let on_path = on_path(path, tool);

    let found = match kind {
        ToolHome::Auto => wrapper.or(configured).or(bundled).or(on_path),
        ToolHome::Wrapper => wrapper,
        ToolHome::Custom => configured,
        ToolHome::Bundled => bundled,
        ToolHome::Path => on_path,
    };
    found.ok_or_else(|| MissingTool {
        label: tool.label,
        root: root.to_path_buf(),
        home: match kind {
            ToolHome::Custom => configured_path.map(Path::to_path_buf),
            _ => None,
        },
        kind,
        wrapper: tool.wrappers.first().copied().unwrap_or_default(),
        program: tool.programs.first().copied().unwrap_or_default(),
    })
}

fn wrapper_at(root: &Path, tool: &Tool) -> Option<ToolLocation> {
    tool.wrappers
        .iter()
        .map(|name| root.join(name))
        .find(|wrapper| wrapper.is_file())
        .map(|wrapper| ToolLocation {
            program: wrapper,
            home: root.to_path_buf(),
            origin: ToolOrigin::Wrapper,
        })
}

/// The program of a home, the first name of the tool the home carries.
fn home_at(home: &Path, tool: &Tool, origin: ToolOrigin) -> Option<ToolLocation> {
    tool.programs
        .iter()
        .map(|name| home.join("bin").join(name))
        .find(|program| program.is_file())
        .map(|program| ToolLocation {
            program,
            home: home.to_path_buf(),
            origin,
        })
}

pub(crate) fn on_path(path: Option<&OsStr>, tool: &Tool) -> Option<ToolLocation> {
    let path = path?;
    env::split_paths(path)
        .flat_map(|directory| {
            tool.programs
                .iter()
                .map(move |name| directory.join(name))
                .collect::<Vec<_>>()
        })
        .find(|file| file.is_file())
        .map(|program| ToolLocation {
            home: program
                .parent()
                .and_then(Path::parent)
                .unwrap_or(Path::new(""))
                .to_path_buf(),
            program,
            origin: ToolOrigin::Path,
        })
}

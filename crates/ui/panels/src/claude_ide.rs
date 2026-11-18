//! The Claude Code IDE protocol of the window: one bridge per project, its variables in
//! every terminal, the diagnostics of Problems and the open editors for its tools, and
//! the status of the connection beside the terminals.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use claude_bridge::{
    Bridge, Diagnostic, Diagnostics, Editors, FileDiagnostics, OpenEditor, Selection, Severity,
};
use ui_kit::*;
use workspace::Workspace;

use crate::outline::OpenEditors as OpenEditorsTable;
use crate::problems::Problem;

/// Where the window looks for the harness, given by the application before the window is
/// built. A window with no application, such as a test, keeps the lock file in its state
/// folder instead, and a scripted run keeps it nowhere.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum ClaudeConfig {
    /// No harness at all: a scripted run must not write into the home of the user.
    Off,
    /// The configuration home of the harness, or none when the home is not known.
    Folder(Option<PathBuf>),
}

impl Global for ClaudeConfig {}

/// The configuration of a window the application builds: a scripted run has no harness,
/// and every other window uses the variable or the home of the user.
pub(crate) fn config_for(
    scripted: bool,
    given: Option<PathBuf>,
    home: Option<PathBuf>,
) -> ClaudeConfig {
    if scripted {
        ClaudeConfig::Off
    } else {
        ClaudeConfig::Folder(claude_bridge::config_home(
            given.as_deref(),
            home.as_deref(),
        ))
    }
}

/// The bridge of the window and the state its tools read. The window owns the state and
/// replaces it as Problems and the editors change.
pub struct ActiveClaude {
    pub bridge: Option<Arc<Bridge>>,
    pub diagnostics: Arc<Diagnostics>,
    pub editors: Arc<Editors>,
    /// Why the bridge did not start, for the status line of the terminal.
    pub error: Option<String>,
}

impl Global for ActiveClaude {}

impl ActiveClaude {
    pub fn environment(&self) -> Vec<(String, String)> {
        self.bridge
            .as_ref()
            .map(|bridge| bridge.environment())
            .unwrap_or_default()
    }

    pub fn connected(&self) -> bool {
        self.bridge
            .as_ref()
            .is_some_and(|bridge| bridge.connected())
    }
}

/// Starts the bridge of the project. A failure leaves the terminals as they were, with
/// the reason in the status line.
pub(crate) fn start(root: &Path, config: ClaudeConfig, cx: &mut App) {
    let diagnostics = Arc::new(Diagnostics::default());
    let editors = Arc::new(Editors::default());
    let (bridge, error) = match config {
        ClaudeConfig::Off => (None, None),
        ClaudeConfig::Folder(None) => (
            None,
            Some("the home folder of the user is not known".to_string()),
        ),
        ClaudeConfig::Folder(Some(folder)) => {
            match Bridge::start(&folder, root, diagnostics.clone(), editors.clone()) {
                Ok(bridge) => (Some(Arc::new(bridge)), None),
                Err(error) => (None, Some(error.to_string())),
            }
        }
    };
    cx.set_global(ActiveClaude {
        bridge,
        diagnostics,
        editors,
        error,
    });
}

fn severity(severity: lang_api::Severity) -> Severity {
    match severity {
        lang_api::Severity::Error => Severity::Error,
        lang_api::Severity::Warning => Severity::Warning,
        lang_api::Severity::Information => Severity::Info,
        lang_api::Severity::Hint => Severity::Hint,
    }
}

/// Replaces what `getDiagnostics` answers with the list of the Problems window.
pub(crate) fn publish_problems(cx: &App, files: impl Iterator<Item = (PathBuf, Vec<Problem>)>) {
    let Some(active) = cx.try_global::<ActiveClaude>() else {
        return;
    };
    let files = files
        .map(|(path, problems)| FileDiagnostics {
            path,
            diagnostics: problems
                .into_iter()
                .map(|problem| Diagnostic {
                    line: problem.line as u32,
                    character: problem.column as u32,
                    severity: severity(problem.severity),
                    message: problem.message,
                    source: None,
                    code: None,
                })
                .collect(),
        })
        .collect();
    active.diagnostics.replace(files);
}

/// Publishes the open editors and the selection of the active one, for `getOpenEditors`
/// and `getCurrentSelection`. The panel calls it every frame; a window with no bridge
/// does no work here, and an unchanged view is not hashed twice.
pub(crate) fn publish_view(workspace: &WeakEntity<Workspace>, cx: &App, last: &mut Option<u64>) {
    let Some(active) = cx.try_global::<ActiveClaude>() else {
        return;
    };
    if active.bridge.is_none() {
        return;
    }
    let active_path = workspace
        .read_with(cx, |workspace, _| workspace.active_path())
        .ok()
        .flatten();
    let views = OpenEditorsTable::all(cx);
    let mut hasher = DefaultHasher::new();
    active_path.hash(&mut hasher);
    for view in &views {
        let view = view.read(cx);
        view.path().hash(&mut hasher);
        view.version().hash(&mut hasher);
        view.selection()
            .map(|(range, _)| (range.start, range.end))
            .hash(&mut hasher);
    }
    let key = hasher.finish();
    if *last == Some(key) {
        return;
    }
    *last = Some(key);
    let editors: Vec<OpenEditor> = views
        .iter()
        .map(|view| {
            let path = view.read(cx).path().to_path_buf();
            let is_active = active_path.as_ref() == Some(&path);
            OpenEditor {
                path,
                active: is_active,
            }
        })
        .collect();
    let selection = active_path.as_ref().and_then(|path| {
        let view = OpenEditorsTable::get(cx, path)?;
        let view = view.read(cx);
        let ((start_line, start_character), (end_line, end_character)) = view.selection_points()?;
        Some(Selection {
            path: path.clone(),
            start_line,
            start_character,
            end_line,
            end_character,
            text: view.selected_text().unwrap_or_default(),
        })
    });
    active.editors.replace(editors, selection);
}

/// The status line of the terminal: the bridge is up and a harness holds its socket, or
/// the reason it does not.
pub(crate) fn status(cx: &App) -> Option<(String, bool)> {
    let active = cx.try_global::<ActiveClaude>()?;
    if active.connected() {
        return Some(("Claude Code connected".to_string(), false));
    }
    active.error.clone().map(|error| (error, true))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{ClaudeConfig, config_for};

    #[test]
    fn a_scripted_run_has_no_harness_and_the_others_use_the_config_folder() {
        let home = PathBuf::from("C:/Users/me");
        let given = if cfg!(windows) {
            PathBuf::from(r"C:\cfg\claude")
        } else {
            PathBuf::from("/cfg/claude")
        };

        assert_eq!(
            config_for(true, None, Some(home.clone())),
            ClaudeConfig::Off
        );
        assert_eq!(
            config_for(false, Some(given.clone()), None),
            ClaudeConfig::Folder(Some(given))
        );
        assert_eq!(
            config_for(false, None, Some(home.clone())),
            ClaudeConfig::Folder(Some(home.join(".claude")))
        );
        assert_eq!(
            config_for(false, None, None),
            ClaudeConfig::Folder(None),
            "a window with no home has no lock file"
        );
    }
}

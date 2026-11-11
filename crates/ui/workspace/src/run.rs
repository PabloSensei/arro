//! The run widget of the toolbar. What a configuration is and how it runs belongs to
//! whoever sets the controls.

use std::path::PathBuf;
use std::rc::Rc;

use ui_kit::*;

#[derive(Clone, Debug, Default, PartialEq)]
pub struct RunState {
    pub configs: Vec<SharedString>,
    pub selected: Option<usize>,
    pub running: bool,
    /// Shown in place of a configuration name when there is none yet.
    pub hint: Option<SharedString>,
    /// The configuration is temporary: it is not in the file and goes away by itself.
    /// The same length as `configs`.
    pub temporary: Vec<bool>,
}

impl RunState {
    pub fn label(&self) -> SharedString {
        let name = self.selected.and_then(|index| self.configs.get(index));
        name.cloned()
            .or_else(|| self.hint.clone())
            .unwrap_or_else(|| "No configurations".into())
    }

    pub fn is_temporary(&self, index: usize) -> bool {
        self.temporary.get(index).copied().unwrap_or(false)
    }

    pub fn can_run(&self) -> bool {
        self.selected.is_some()
    }
}

type Action = Rc<dyn Fn(&mut Window, &mut App)>;
type Select = Rc<dyn Fn(usize, &mut Window, &mut App)>;
/// An action with a flag, such as the force form of Run to Cursor.
type FlagAction = Rc<dyn Fn(bool, &mut Window, &mut App)>;
/// An action that names a file of the project, such as Compile.
type FileAction = Rc<dyn Fn(Option<PathBuf>, &mut Window, &mut App)>;

#[derive(Clone)]
pub struct RunControls {
    pub state: Rc<dyn Fn(&App) -> RunState>,
    pub run: Action,
    pub debug: Action,
    pub stop: Action,
    pub select: Select,
    /// Ctrl+F9: build the project with its build tool.
    pub build: Action,
    /// Ctrl+Shift+F9: compile the module of the file with the build tool.
    pub compile: FileAction,
    /// Ctrl+F5: runs the last configuration or test request again.
    pub rerun: Action,
    /// Alt+Shift+R: runs the last test request again.
    pub rerun_tests: Action,
    /// Ctrl+Shift+F2: stops the background tasks, or asks which one.
    pub stop_background: Action,
    /// Writes the selected temporary configuration into the file, as OK of the dialog
    /// does.
    pub save: Action,
}

/// Where a debug session is, for the toolbar and the keys.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DebugRunState {
    pub active: bool,
    pub stopped: bool,
}

#[derive(Clone)]
pub struct DebugControls {
    pub state: Rc<dyn Fn(&App) -> DebugRunState>,
    /// Shift+F9: debug the selected configuration.
    pub start: Action,
    pub step_over: Action,
    pub step_into: Action,
    /// Alt+Shift+F7: a step into library code too.
    pub force_step_into: Action,
    /// Shift+F7: a step into one of the calls on the line.
    pub smart_step_into: Action,
    pub step_out: Action,
    pub resume: Action,
    /// Alt+F8: evaluate an expression.
    pub evaluate: Action,
    /// Ctrl+Alt+F8: evaluate the text at the caret in a popup.
    pub quick_evaluate: Action,
    /// Ctrl+F8: a breakpoint on the caret line of the active editor.
    pub toggle_breakpoint: Action,
    /// Ctrl+Alt+Shift+F8: a breakpoint that goes away at the first stop.
    pub toggle_temporary_breakpoint: Action,
    /// Ctrl+Shift+F8: the properties of the breakpoint at the caret, or every breakpoint.
    pub edit_breakpoint: Action,
    /// Alt+Shift+F8: a step over that does not stop on the breakpoints of the call.
    pub force_step_over: Action,
    /// Ctrl+Shift+F8: every breakpoint with its properties and the exception filters.
    pub view_breakpoints: Action,
    /// Alt+F9 and Ctrl+Alt+F9: a temporary breakpoint on the caret line and continue.
    pub run_to_cursor: FlagAction,
    /// Alt+F10: opens the file and line of the selected frame.
    pub show_execution_point: Action,
    pub stop: Action,
}

#[cfg(test)]
mod tests {
    use super::RunState;

    #[test]
    fn the_label_falls_back_to_the_hint() {
        let mut state = RunState {
            hint: Some("Loading".into()),
            ..RunState::default()
        };
        assert_eq!(state.label().as_ref(), "Loading");
        assert!(!state.can_run());

        state.configs = vec!["App".into()];
        state.selected = Some(0);
        assert_eq!(state.label().as_ref(), "App");
        assert!(state.can_run());
    }
}

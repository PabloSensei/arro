//! The notice of a language server that stopped because memory is low. The status line
//! names the stop; one notification gives the numbers and the threshold, and offers a
//! start now, the page of the memory settings, turning the memory check off and a close.
//! The notice stays until the user closes it, so a stop that comes while it is on screen
//! does not show it again. A virtual machine does not stop for memory by default: there
//! memory that stays low gets one warning per session instead.

use std::time::Duration;

use platform::{LOW_MEMORY_HOLD, MemoryState, MemoryThresholds};
use ui_kit::gpui::{App, Context, Global, SharedString, Window};
use workspace::{Settings, Workspace};

use crate::settings::SettingsView;

/// The command the status line runs to show the notice of the stop it names again.
pub(crate) const SHOW_COMMAND: &str = "language.show_memory_stop";
/// The command that opens the memory settings, a button of the notices.
const SETTINGS_COMMAND: &str = "settings.memory";
/// The command of the close button of the notices.
const CLOSE_COMMAND: &str = "notification.close";
/// The command that starts the server now, the first button of the notice.
const START_COMMAND: &str = "language.restart_server";
/// The command that turns the memory check off and starts the server.
const OFF_COMMAND: &str = "language.memory_check_off";
/// How often a virtual machine reads its memory for the warning.
pub(crate) const WARNING_POLL: Duration = Duration::from_secs(2);
/// The title of the warning of a virtual machine.
const WARNING_TITLE: &str = "Low memory on this virtual machine";

/// The buttons of a notice: the id, the label and the command that each runs.
type Buttons = Vec<(&'static str, SharedString, &'static str)>;

/// What the notices read about the machine: the memory numbers now and whether the machine
/// is a virtual machine. The window reads the system when nothing sets them; a test sets
/// them.
#[derive(Clone, Copy, Debug)]
pub(crate) struct MachineReading {
    pub(crate) memory: Option<MemoryState>,
    pub(crate) virtual_machine: bool,
}

impl Global for MachineReading {}

/// The machine now: what a test sets, or what the system reports.
fn reading(cx: &App) -> MachineReading {
    match cx.try_global::<MachineReading>() {
        Some(reading) => *reading,
        None => MachineReading {
            memory: MemoryState::now(),
            virtual_machine: ::platform::is_virtual_machine(),
        },
    }
}

/// A status line of a server that stopped for low memory.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Stop<'a> {
    /// The name the line gives the server, such as the language of its project.
    pub(crate) name: &'a str,
    /// Whether the server stays stopped until the user starts it, after too many short
    /// starts for memory.
    pub(crate) manual: bool,
}

/// The stop that a status line names, or `None` for any other line.
pub(crate) fn stop_of(text: &str) -> Option<Stop<'_>> {
    let (name, reason) = text.split_once(": stopped: ")?;
    let reason = reason.strip_prefix("memory is low")?;
    Some(Stop {
        name,
        manual: reason.contains("restart it manually"),
    })
}

/// The title of the notice of a server.
fn title_of(name: &str) -> String {
    if name.is_empty() {
        "Language server stopped: low memory".to_owned()
    } else {
        format!("{name} language server stopped: low memory")
    }
}

/// Megabytes, rounded down, for the numbers of the notices.
fn megabytes(bytes: u64) -> u64 {
    bytes / (1024 * 1024)
}

/// The file that backs memory beyond the physical one, and the advice to make it larger.
fn backing_file() -> (&'static str, &'static str) {
    if cfg!(windows) {
        ("page file", "increase the Windows page file")
    } else {
        ("swap file", "increase the swap file")
    }
}

/// The first part of a notice of memory: the free memory and the total. When the backing
/// file runs out before the physical memory does, the free commit is given instead.
fn free_memory_text(state: Option<MemoryState>) -> String {
    let (backing, _) = backing_file();
    match state {
        Some(state) if state.commit_is_tighter() => format!(
            "Free memory with the {backing}: {} MB of {} MB. ",
            megabytes(state.available_commit),
            megabytes(state.commit_limit),
        ),
        Some(state) => format!(
            "Free memory: {} MB of {} MB. ",
            megabytes(state.available_physical),
            megabytes(state.total_physical),
        ),
        None => "Free memory is low. ".to_owned(),
    }
}

/// The text of the notice for a stop: the free memory that the numbers show and the
/// threshold they are compared with, and what the user can do about it. `manual` names a
/// stop after which the server does not start by itself.
pub(crate) fn notice_text(
    state: Option<MemoryState>,
    limits: MemoryThresholds,
    manual: bool,
) -> String {
    let (_, grow) = backing_file();
    let mut text = free_memory_text(state);
    text.push_str(&format!(
        "The server stops when free memory is below {} MB and below {} % of the total. ",
        megabytes(limits.low_bytes),
        limits.low_percent,
    ));
    let commit_tighter = state.is_some_and(|state| state.commit_is_tighter());
    let advice = match (commit_tighter, manual) {
        (true, false) => {
            format!("Close heavy programs or {grow}: the server starts again when memory frees.")
        }
        (true, true) => format!(
            "Close heavy programs or {grow}, then press Start Anyway. The server does not start by itself."
        ),
        (false, false) => {
            "Close heavy programs: the server starts again when memory frees.".to_owned()
        }
        (false, true) => {
            "Close heavy programs, then press Start Anyway. The server does not start by itself."
                .to_owned()
        }
    };
    text.push_str(&advice);
    text
}

/// The text of the warning of a virtual machine: the same numbers, and why the server is
/// not stopped. The host of a virtual machine adds memory on demand, so the check is off
/// there by default.
pub(crate) fn warning_text(state: Option<MemoryState>, limits: MemoryThresholds) -> String {
    let mut text = free_memory_text(state);
    text.push_str(&format!(
        "The check is off on a virtual machine: the host adds memory on demand, so the server is not stopped for low memory. Turn the check on in Memory Settings to stop the server when free memory is below {} MB and below {} % of the total.",
        megabytes(limits.low_bytes),
        limits.low_percent,
    ));
    text
}

/// The buttons of the notice of a stop.
fn stop_buttons() -> Buttons {
    vec![
        ("start", "Start Anyway".into(), START_COMMAND),
        ("settings", "Memory Settings…".into(), SETTINGS_COMMAND),
        ("off", "Don't Stop on This Machine".into(), OFF_COMMAND),
        ("close", "Close".into(), CLOSE_COMMAND),
    ]
}

/// The buttons of the warning of a virtual machine.
fn warning_buttons() -> Buttons {
    vec![
        ("settings", "Memory Settings…".into(), SETTINGS_COMMAND),
        ("close", "Close".into(), CLOSE_COMMAND),
    ]
}

/// Shows the notice of a stop, with the numbers as they are now.
fn show(name: &str, manual: bool, workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let limits = cx
        .try_global::<Settings>()
        .map(Settings::memory_numbers)
        .unwrap_or_default();
    let text = notice_text(reading(cx).memory, limits, manual);
    workspace.notify_error_with_commands(title_of(name), text, stop_buttons(), cx);
}

/// Shows the notice of a new stop, unless the notice of the same server is on screen.
pub(crate) fn notify(
    name: &str,
    manual: bool,
    workspace: &mut Workspace,
    cx: &mut Context<Workspace>,
) {
    let title = title_of(name);
    let open = workspace.notification_shown()
        && workspace
            .last_notification()
            .is_some_and(|(shown, _)| shown.as_ref() == title);
    if !open {
        show(name, manual, workspace, cx);
    }
}

/// Shows the notice of the stop on the status line again: the click on the line.
fn show_again(workspace: &mut Workspace, cx: &mut Context<Workspace>) {
    let Some(text) = workspace.language_status().map(ToString::to_string) else {
        return;
    };
    if let Some(stop) = stop_of(&text) {
        show(stop.name, stop.manual, workspace, cx);
    }
}

/// Turns the memory check off, keeps the other settings, and starts the server now: the
/// last button of the notice of a stop.
fn turn_off(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let saved = cx.try_global::<Settings>().cloned().unwrap_or_default();
    let mut draft = saved.clone();
    draft.memory_low_check = Some(false);
    workspace.apply_settings(&saved, &draft, cx);
    workspace.run_command(START_COMMAND, window, cx);
}

/// The polls of the watch of a virtual machine, and the memory state that they leave.
#[derive(Debug, Default)]
struct WarningWatch {
    /// Whether the last poll found the memory low, with the hysteresis of the platform.
    low: bool,
    /// The polls in a row that found the memory low.
    low_polls: u32,
}

impl WarningWatch {
    /// Takes one reading, and tells whether the memory has been low for the hold.
    fn observe(&mut self, state: Option<MemoryState>, limits: MemoryThresholds) -> bool {
        self.low = state.is_some_and(|state| state.low_state(limits, self.low));
        self.low_polls = if self.low {
            self.low_polls.saturating_add(1)
        } else {
            0
        };
        u64::from(self.low_polls) * WARNING_POLL.as_secs() >= LOW_MEMORY_HOLD.as_secs()
    }
}

/// One poll of the watch: shows the warning once the memory has been low for the hold.
/// Returns whether the watch is over, because the warning is shown or the user has chosen
/// the memory check, which makes a warning pointless.
fn poll_warning(
    workspace: &mut Workspace,
    watch: &mut WarningWatch,
    cx: &mut Context<Workspace>,
) -> bool {
    let Some(settings) = cx.try_global::<Settings>() else {
        return true;
    };
    if settings.memory_low_check.is_some() {
        return true;
    }
    let limits = settings.memory_numbers();
    let state = reading(cx).memory;
    if !watch.observe(state, limits) {
        return false;
    }
    let text = warning_text(state, limits);
    workspace.notify_warning_with_commands(WARNING_TITLE, text, warning_buttons(), cx);
    true
}

/// Watches the memory of a virtual machine. There the memory check is off by default, so
/// memory that stays low gets one warning per session instead of a stop. Nothing is
/// watched on another machine.
pub(crate) fn watch_warning(cx: &mut Context<Workspace>) {
    if !reading(cx).virtual_machine {
        return;
    }
    cx.spawn(async move |workspace, cx| {
        let mut watch = WarningWatch::default();
        loop {
            cx.background_executor().timer(WARNING_POLL).await;
            let Ok(done) =
                workspace.update(cx, |workspace, cx| poll_warning(workspace, &mut watch, cx))
            else {
                break;
            };
            if done {
                break;
            }
        }
    })
    .detach();
}

/// The commands the notices and their line use.
pub(crate) fn register(workspace: &mut Workspace) {
    workspace.register_command(SHOW_COMMAND, "Show Memory Notice", |workspace, _, cx| {
        show_again(workspace, cx);
    });
    workspace.register_command(
        SETTINGS_COMMAND,
        "Memory Settings",
        |workspace, window, cx| {
            SettingsView::show_at(workspace, window, cx, Some("memory_low_megabytes"));
        },
    );
    workspace.register_command(CLOSE_COMMAND, "Close Notification", |workspace, _, cx| {
        workspace.close_notification(cx);
    });
    workspace.register_command(OFF_COMMAND, "Don't Stop on This Machine", turn_off);
}

#[cfg(test)]
mod tests {
    use platform::{LOW_MEMORY_HOLD, MemoryState, MemoryThresholds};

    use super::{Stop, WARNING_POLL, WarningWatch, notice_text, stop_of, warning_text};

    fn gigabytes(count: u64) -> u64 {
        count * 1024 * 1024 * 1024
    }

    /// Memory with room in the page file, so the physical memory is the tighter side.
    fn physical_bound() -> MemoryState {
        MemoryState {
            available_physical: 400 * 1024 * 1024,
            total_physical: gigabytes(16),
            available_commit: gigabytes(8),
            commit_limit: gigabytes(24),
        }
    }

    /// Memory whose page file runs out before the physical memory does.
    fn commit_bound() -> MemoryState {
        MemoryState {
            available_physical: gigabytes(6),
            total_physical: gigabytes(16),
            available_commit: 512 * 1024 * 1024,
            commit_limit: gigabytes(18),
        }
    }

    /// Memory with plenty free: neither a stop nor a warning.
    fn roomy() -> MemoryState {
        MemoryState {
            available_physical: gigabytes(8),
            total_physical: gigabytes(16),
            available_commit: gigabytes(16),
            commit_limit: gigabytes(24),
        }
    }

    #[test]
    fn a_status_line_names_a_stop_for_memory_and_the_server() {
        assert_eq!(
            stop_of("Fixture: stopped: memory is low, it starts again when memory frees"),
            Some(Stop {
                name: "Fixture",
                manual: false
            })
        );
        assert_eq!(
            stop_of("Fixture: stopped: memory is low, restart it manually"),
            Some(Stop {
                name: "Fixture",
                manual: true
            })
        );
        assert_eq!(stop_of("Fixture: ready"), None);
        assert_eq!(stop_of("Fixture: stopped: exited with 1"), None);
        assert_eq!(stop_of("Fixture: failed: memory is low"), None);
    }

    #[test]
    fn the_notice_gives_the_free_memory_the_threshold_and_what_to_do() {
        let limits = MemoryThresholds::new(5., 512);
        let text = notice_text(Some(physical_bound()), limits, false);
        assert!(text.contains("Free memory: 400 MB of 16384 MB."), "{text}");
        assert!(
            text.contains("below 512 MB and below 5 % of the total"),
            "{text}"
        );
        assert!(text.contains("Close heavy programs"), "{text}");
        assert!(text.contains("starts again when memory frees"), "{text}");
        assert!(!text.contains("page file"), "{text}");
    }

    #[test]
    fn a_commit_that_runs_out_first_names_the_page_file_and_the_advice() {
        let limits = MemoryThresholds::new(5., 512);
        let text = notice_text(Some(commit_bound()), limits, false);
        assert!(text.contains("Free memory with the"), "{text}");
        assert!(text.contains("512 MB of 18432 MB"), "{text}");
        let advice = if cfg!(windows) {
            "increase the Windows page file"
        } else {
            "increase the swap file"
        };
        assert!(text.contains(advice), "{text}");
    }

    #[test]
    fn a_manual_stop_does_not_promise_a_start_by_itself() {
        let limits = MemoryThresholds::new(5., 512);
        let text = notice_text(Some(physical_bound()), limits, true);
        assert!(text.contains("press Start Anyway"), "{text}");
        assert!(text.contains("does not start by itself"), "{text}");
        assert!(!text.contains("starts again"), "{text}");
    }

    #[test]
    fn without_the_numbers_the_notice_still_says_what_happened() {
        let text = notice_text(None, MemoryThresholds::new(5., 512), false);
        assert!(text.starts_with("Free memory is low."), "{text}");
    }

    #[test]
    fn the_warning_gives_the_numbers_and_says_the_check_is_off() {
        let text = warning_text(Some(physical_bound()), MemoryThresholds::new(5., 512));
        assert!(text.contains("Free memory: 400 MB of 16384 MB."), "{text}");
        assert!(
            text.contains("The check is off on a virtual machine"),
            "{text}"
        );
        assert!(
            text.contains("below 512 MB and below 5 % of the total"),
            "{text}"
        );
    }

    #[test]
    fn the_warning_waits_for_the_hold_of_low_polls() {
        let limits = MemoryThresholds::new(5., 512);
        let low = Some(physical_bound());
        let mut watch = WarningWatch::default();
        let polls = LOW_MEMORY_HOLD.as_secs() / WARNING_POLL.as_secs();
        for _ in 1..polls {
            assert!(!watch.observe(low, limits), "a poll short of the hold");
        }
        assert!(
            watch.observe(low, limits),
            "the hold of low polls brings the warning"
        );
        // A recovery starts the count again.
        assert!(!watch.observe(Some(roomy()), limits));
        assert!(!watch.observe(low, limits));
    }
}

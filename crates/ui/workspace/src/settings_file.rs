//! The settings file as the window reads and writes it. A read keeps every value that fits
//! and names the ones that do not. A write changes only the keys that this process changed,
//! over a fresh read of the file, so the keys of other windows and the keys this version
//! does not know stay in the file. The writes of a process run one at a time, last value
//! of a key wins.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};

use serde_json::{Map, Value};
use ui_kit::*;

use crate::settings::Settings;
use crate::window::Workspace;

/// The action of the notifications that names the file.
const OPEN_SETTINGS: &str = "Open settings.json";

/// A place where the settings file does not read: the line and what is wrong there.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsFault {
    /// 1-based.
    pub line: usize,
    pub message: String,
}

impl fmt::Display for SettingsFault {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "line {}: {}", self.line, self.message)
    }
}

/// What a read of the settings file found.
#[derive(Debug)]
pub struct Reading {
    /// The settings in force: the value of every key that fits, the default of the others.
    pub settings: Settings,
    /// The file is not a settings object. Nothing is written to it until it is fixed.
    pub fault: Option<SettingsFault>,
    /// The keys whose values do not fit: their defaults are in force, the file is left as is.
    pub warnings: Vec<SettingsFault>,
}

/// Reads the settings file. A missing or empty file gives the defaults. A file that is not
/// a JSON object is a fault, and the defaults are in force. A value that does not fit its
/// key gives that key its default and a warning; the other keys are read.
///
/// `theme_chosen` is the installation that already chose a theme: a file without the
/// switch of Sync with OS keeps the sync off, see `Settings::load_with_theme_choice`.
pub fn read(path: &Path, theme_chosen: bool) -> Reading {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Reading {
                settings: keep_theme(Settings::default(), theme_chosen, false),
                fault: Some(SettingsFault {
                    line: 1,
                    message: error.to_string(),
                }),
                warnings: Vec::new(),
            };
        }
    };
    if is_blank(&text) {
        return Reading {
            settings: keep_theme(Settings::default(), theme_chosen, false),
            fault: None,
            warnings: Vec::new(),
        };
    }
    let object = match parse_object(&text) {
        Ok(object) => object,
        Err(fault) => {
            return Reading {
                settings: keep_theme(Settings::default(), theme_chosen, false),
                fault: Some(fault),
                warnings: Vec::new(),
            };
        }
    };
    let names_sync = object.contains_key("theme_sync_with_os");
    let (settings, warnings) = settings_of(&object, &text);
    Reading {
        settings: keep_theme(settings, theme_chosen, names_sync),
        fault: None,
        warnings,
    }
}

/// A file written before Sync with OS existed keeps the theme of an installation that
/// already chose one: the sync stays off there.
fn keep_theme(mut settings: Settings, theme_chosen: bool, names_sync: bool) -> Settings {
    if theme_chosen && !names_sync {
        settings.theme_sync_with_os = false;
    }
    settings
}

/// The text of a settings file without the byte order mark that some editors write.
fn body(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

/// Whether a settings file holds nothing but white space.
fn is_blank(text: &str) -> bool {
    body(text).trim().is_empty()
}

/// The keys of a settings file as a JSON object. Text that does not parse, or that parses
/// to a value other than an object, is the fault.
fn parse_object(text: &str) -> Result<Map<String, Value>, SettingsFault> {
    match serde_json::from_str::<Value>(body(text)) {
        Ok(Value::Object(object)) => Ok(object),
        Ok(_) => Err(SettingsFault {
            line: 1,
            message: "the file holds a value that is not an object".to_owned(),
        }),
        Err(error) => Err(SettingsFault {
            line: error.line().max(1),
            message: without_location(&error),
        }),
    }
}

/// The message of a parse error without the line and column that the fault names.
fn without_location(error: &serde_json::Error) -> String {
    let text = error.to_string();
    let location = format!(" at line {} column {}", error.line(), error.column());
    text.strip_suffix(&location).unwrap_or(&text).to_owned()
}

/// The settings that the keys of `object` give, and a warning for each key whose value does
/// not fit. A key that does not fit keeps its default; the others are read as they are.
fn settings_of(object: &Map<String, Value>, text: &str) -> (Settings, Vec<SettingsFault>) {
    if let Ok(settings) = serde_json::from_value::<Settings>(Value::Object(object.clone())) {
        return (settings.clamped(), Vec::new());
    }
    let mut current = match serde_json::to_value(Settings::default()) {
        Ok(Value::Object(defaults)) => defaults,
        _ => Map::new(),
    };
    let mut warnings = Vec::new();
    for (key, value) in object {
        let mut trial = current.clone();
        trial.insert(key.clone(), value.clone());
        match serde_json::from_value::<Settings>(Value::Object(trial.clone())) {
            Ok(_) => current = trial,
            Err(error) => warnings.push(SettingsFault {
                line: line_of(text, key),
                message: format!("\"{key}\": {error}"),
            }),
        }
    }
    let settings = serde_json::from_value::<Settings>(Value::Object(current)).unwrap_or_default();
    (settings.clamped(), warnings)
}

/// The line of the first `"key"` of `text`, or the first line when the key is not there.
fn line_of(text: &str, key: &str) -> usize {
    let needle = format!("\"{key}\"");
    text.find(&needle)
        .map_or(1, |at| text[..at].matches('\n').count() + 1)
}

/// The keys whose values differ between `before` and `after`, with their new values. A key
/// that did not change is not written: the file may hold a newer value of it from another
/// window.
pub fn changes(before: &Settings, after: &Settings) -> Vec<(String, Value)> {
    let (Ok(Value::Object(before)), Ok(Value::Object(after))) =
        (serde_json::to_value(before), serde_json::to_value(after))
    else {
        return Vec::new();
    };
    after
        .into_iter()
        .filter(|(key, value)| before.get(key) != Some(value))
        .collect()
}

/// The keys that a write names when the file does not have them yet, with the values of this
/// process: the switch of Sync with OS. A file that names it is read as written by this
/// version (see `read`). A file that has the key keeps its own value.
pub fn named_keys(settings: &Settings) -> Vec<(String, Value)> {
    vec![(
        "theme_sync_with_os".to_owned(),
        Value::Bool(settings.theme_sync_with_os),
    )]
}

/// Every key of `settings` with its value: what a save of the whole settings writes.
pub fn all_keys(settings: &Settings) -> Vec<(String, Value)> {
    match serde_json::to_value(settings) {
        Ok(Value::Object(object)) => object.into_iter().collect(),
        _ => Vec::new(),
    }
}

/// Why a write did not change the settings file.
#[derive(Debug)]
pub enum WriteError {
    /// The file does not read as a settings object now. It is left as it is.
    Fault(SettingsFault),
    /// The file could not be read or written.
    Io(String),
}

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Fault(fault) => write!(f, "{fault}"),
            Self::Io(message) => f.write_str(message),
        }
    }
}

/// Writes `changes` into the file. The file is read again first, so the keys that another
/// window wrote since stay; the changed keys take their new values, and the keys this
/// version does not know stay too. A key of `named` is added only where the file lacks it.
/// A file that does not read as a settings object is not written. The new text takes the
/// place of the old file in one step.
pub fn write(
    path: &Path,
    changes: &[(String, Value)],
    named: &[(String, Value)],
) -> Result<(), WriteError> {
    let mut object = read_object(path)?;
    for (key, value) in named {
        object.entry(key.clone()).or_insert_with(|| value.clone());
    }
    for (key, value) in changes {
        object.insert(key.clone(), value.clone());
    }
    let text = serde_json::to_string_pretty(&Value::Object(object))
        .map_err(|error| WriteError::Io(error.to_string()))?;
    write_text(path, &text).map_err(|error| WriteError::Io(format!("{}: {error}", path.display())))
}

/// The keys of the file as it is now: none for a missing or empty file.
fn read_object(path: &Path) -> Result<Map<String, Value>, WriteError> {
    match std::fs::read_to_string(path) {
        Ok(text) if is_blank(&text) => Ok(Map::new()),
        Ok(text) => parse_object(&text).map_err(WriteError::Fault),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Map::new()),
        Err(error) => Err(WriteError::Io(format!("{}: {error}", path.display()))),
    }
}

/// Writes the text through a file beside `path` that takes its place, so a stop in the
/// middle leaves the old file whole.
fn write_text(path: &Path, text: &str) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temp = temp_path(path);
    std::fs::write(&temp, text)?;
    if let Err(error) = std::fs::rename(&temp, path) {
        let _ = std::fs::remove_file(&temp);
        return Err(error);
    }
    Ok(())
}

/// A name beside `path` that a save of this process has not taken yet.
fn temp_path(path: &Path) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
    let mut name = path
        .file_name()
        .map_or_else(|| "settings".into(), |name| name.to_os_string());
    name.push(format!(".{}.{sequence}.tmp", std::process::id()));
    path.with_file_name(name)
}

/// The writes of the settings file of this process, shared by its windows.
#[derive(Default)]
pub struct SettingsWriter {
    shared: Arc<Shared>,
}

impl Global for SettingsWriter {}

#[derive(Default)]
struct Shared {
    pending: Mutex<Pending>,
    /// Held while a write or an import changes the file, so that the two never overlap.
    file: Mutex<()>,
    /// Set by an import: this process writes no settings after it.
    stopped: AtomicBool,
}

#[derive(Default)]
struct Pending {
    /// The changes that no write took yet, one value per key.
    changes: Vec<(String, Value)>,
    /// The keys that a write names where the file lacks them, one value per key.
    named: Vec<(String, Value)>,
    /// Whether a task is taking the changes.
    running: bool,
}

/// What one write of the queue did.
enum Outcome {
    Written,
    Stopped,
    Failed(WriteError),
}

/// Queues `changes` for the settings file at `path`, with the `named` keys of this process
/// (see `named_keys`). The queue writes in the background, one write after another. A key
/// that changes again before its write takes the newer value.
pub(crate) fn queue(
    cx: &mut Context<Workspace>,
    path: PathBuf,
    changes: Vec<(String, Value)>,
    named: Vec<(String, Value)>,
) {
    if changes.is_empty() {
        return;
    }
    let shared = cx.default_global::<SettingsWriter>().shared.clone();
    let idle = {
        let mut pending = lock(&shared.pending);
        merge(&mut pending.changes, changes);
        merge(&mut pending.named, named);
        !std::mem::replace(&mut pending.running, true)
    };
    if !idle {
        return;
    }
    let window = cx.weak_entity();
    cx.spawn(async move |_, cx| {
        loop {
            let (changes, named) = {
                let mut pending = lock(&shared.pending);
                if pending.changes.is_empty() {
                    pending.running = false;
                    break;
                }
                (
                    std::mem::take(&mut pending.changes),
                    std::mem::take(&mut pending.named),
                )
            };
            let writer = shared.clone();
            let target = path.clone();
            let outcome = cx
                .background_executor()
                .spawn(async move {
                    let _file = lock(&writer.file);
                    if writer.stopped.load(Ordering::SeqCst) {
                        return Outcome::Stopped;
                    }
                    match write(&target, &changes, &named) {
                        Ok(()) => Outcome::Written,
                        Err(error) => Outcome::Failed(error),
                    }
                })
                .await;
            window
                .update(cx, |workspace, cx| report(workspace, &path, outcome, cx))
                .ok();
        }
    })
    .detach();
}

/// Gives `changes` the place of the older values of their keys, in the order of the keys.
fn merge(pending: &mut Vec<(String, Value)>, changes: Vec<(String, Value)>) {
    for (key, value) in changes {
        match pending.iter_mut().find(|(known, _)| *known == key) {
            Some(slot) => slot.1 = value,
            None => pending.push((key, value)),
        }
    }
}

/// Tells the window what its write found: a file that is not a settings object gets a
/// notification that says it will not be overwritten.
fn report(workspace: &mut Workspace, path: &Path, outcome: Outcome, cx: &mut Context<Workspace>) {
    match outcome {
        Outcome::Written | Outcome::Stopped => {}
        Outcome::Failed(WriteError::Fault(fault)) => notify_fault(workspace, path, &fault, cx),
        Outcome::Failed(WriteError::Io(message)) => {
            eprintln!("settings are not saved: {message}");
        }
    }
}

fn notify_fault(
    workspace: &mut Workspace,
    path: &Path,
    fault: &SettingsFault,
    cx: &mut Context<Workspace>,
) {
    let text = format!(
        "settings.json has an error at {fault}; Arro will not overwrite it until it is fixed"
    );
    workspace.notify_error_with_action_and_commands(
        "Settings",
        text,
        OPEN_SETTINGS,
        path.to_path_buf(),
        Vec::new(),
        cx,
    );
}

/// Tells the window what its start found in the settings file: a file that does not read,
/// or values that do not fit.
pub(crate) fn announce(
    workspace: &mut Workspace,
    path: &Path,
    reading: &Reading,
    cx: &mut Context<Workspace>,
) {
    if let Some(fault) = &reading.fault {
        notify_fault(workspace, path, fault, cx);
    } else if !reading.warnings.is_empty() {
        let lines: Vec<String> = reading.warnings.iter().map(ToString::to_string).collect();
        let text = format!(
            "The defaults are in force for these values:\n{}",
            lines.join("\n")
        );
        workspace.notify_user_with_action("Settings", text, OPEN_SETTINGS, path.to_path_buf(), cx);
    }
}

/// Stops the writes of this process from now on, and runs `work` in the background once a
/// write in progress has finished. An import of the settings file runs through it, so that
/// no later save of this process undoes what the import wrote.
pub fn stop_settings_writes<T: Send + 'static>(
    cx: &mut App,
    work: impl FnOnce() -> T + Send + 'static,
) -> Task<T> {
    let shared = cx.default_global::<SettingsWriter>().shared.clone();
    shared.stopped.store(true, Ordering::SeqCst);
    cx.background_executor().spawn(async move {
        let _file = lock(&shared.file);
        work()
    })
}

/// Lets this process write its settings again. An import that failed wrote nothing, so the
/// settings of the window are kept as they were before it.
pub fn resume_settings_writes(cx: &mut App) {
    let shared = cx.default_global::<SettingsWriter>().shared.clone();
    shared.stopped.store(false, Ordering::SeqCst);
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poison| poison.into_inner())
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use serde_json::Value;

    use super::{WriteError, changes, merge, named_keys, read, write};
    use crate::settings::Settings;

    fn file_in(name: &str) -> PathBuf {
        test_support::scratch(name).join("settings.json")
    }

    fn text_of(path: &Path) -> String {
        std::fs::read_to_string(path).unwrap()
    }

    #[test]
    fn a_file_that_is_not_an_object_is_a_fault_and_is_not_written() {
        let path = file_in("settings-fault");
        let broken = "{\n  \"font_size\": 15.0,\n";
        std::fs::write(&path, broken).unwrap();

        let reading = read(&path, false);
        assert_eq!(reading.settings, Settings::default());
        let fault = reading.fault.expect("the file does not read");
        assert_eq!(fault.line, 3, "{fault}");

        let changed = [("soft_wrap".to_owned(), Value::Bool(true))];
        assert!(matches!(
            write(&path, &changed, &[]),
            Err(WriteError::Fault(_))
        ));
        assert_eq!(text_of(&path), broken, "the damaged file stays as it is");
    }

    #[test]
    fn a_value_that_does_not_fit_keeps_the_other_keys() {
        let path = file_in("settings-bad-value");
        std::fs::write(
            &path,
            "{\n  \"font_size\": 15.0,\n  \"tab_size\": \"big\",\n  \"soft_wrap\": false\n}",
        )
        .unwrap();

        let reading = read(&path, false);
        assert!(reading.fault.is_none());
        assert_eq!(reading.settings.font_size, 15.);
        assert!(!reading.settings.soft_wrap);
        assert_eq!(reading.settings.tab_size, Settings::default().tab_size);
        assert_eq!(reading.warnings.len(), 1, "{:?}", reading.warnings);
        assert_eq!(reading.warnings[0].line, 3);
        assert!(reading.warnings[0].message.contains("tab_size"));
    }

    #[test]
    fn a_byte_order_mark_is_not_a_fault() {
        let path = file_in("settings-bom");
        std::fs::write(&path, "\u{feff}{\"soft_wrap\": true}").unwrap();

        let reading = read(&path, false);
        assert!(reading.fault.is_none());
        assert!(reading.settings.soft_wrap);

        write(&path, &[("font_size".to_owned(), Value::from(14.))], &[]).unwrap();
        let stored = Settings::load(&path);
        assert!(stored.soft_wrap);
        assert_eq!(stored.font_size, 14.);
    }

    #[test]
    fn two_processes_that_change_different_keys_keep_both_changes() {
        let path = file_in("settings-two-processes");
        let start = Settings::default();

        let mut first = start.clone();
        first.soft_wrap = !start.soft_wrap;
        let mut second = start.clone();
        second.font_size = 16.;

        write(&path, &changes(&start, &first), &named_keys(&first)).unwrap();
        write(&path, &changes(&start, &second), &named_keys(&second)).unwrap();

        let stored = Settings::load(&path);
        assert_eq!(stored.soft_wrap, first.soft_wrap, "the first change stays");
        assert_eq!(stored.font_size, 16., "the second change is written");
    }

    #[test]
    fn a_key_this_version_does_not_know_survives_a_save() {
        let path = file_in("settings-unknown");
        std::fs::write(
            &path,
            r#"{"future_setting": {"level": 2}, "font_size": 14.0}"#,
        )
        .unwrap();

        write(&path, &[("soft_wrap".to_owned(), Value::Bool(true))], &[]).unwrap();

        let stored: Value = serde_json::from_str(&text_of(&path)).unwrap();
        assert_eq!(stored["future_setting"]["level"], 2);
        assert_eq!(stored["font_size"], 14.0);
        assert_eq!(stored["soft_wrap"], true);
    }

    #[test]
    fn a_later_value_of_a_key_replaces_the_earlier_one() {
        let mut pending = Vec::new();
        merge(
            &mut pending,
            vec![
                ("font_size".to_owned(), Value::from(14.)),
                ("soft_wrap".to_owned(), Value::Bool(true)),
            ],
        );
        merge(
            &mut pending,
            vec![("font_size".to_owned(), Value::from(15.))],
        );
        assert_eq!(
            pending,
            [
                ("font_size".to_owned(), Value::from(15.)),
                ("soft_wrap".to_owned(), Value::Bool(true)),
            ]
        );
    }

    #[test]
    fn a_change_writes_only_the_changed_keys() {
        let start = Settings::default();
        let mut after = start.clone();
        after.line_spacing = start.line_spacing + 0.5;

        let names: Vec<String> = changes(&start, &after)
            .into_iter()
            .map(|(key, _)| key)
            .collect();
        assert_eq!(names, ["line_spacing"]);
    }

    #[test]
    fn a_named_key_is_written_only_where_the_file_lacks_it() {
        let path = file_in("settings-named");
        std::fs::write(&path, r#"{"font_size": 14.0}"#).unwrap();

        let off = Settings {
            theme_sync_with_os: false,
            ..Settings::default()
        };
        write(&path, &[], &named_keys(&off)).unwrap();
        assert!(
            !Settings::load(&path).theme_sync_with_os,
            "a file without the switch takes the value of this process"
        );

        let on = Settings {
            theme_sync_with_os: true,
            ..Settings::default()
        };
        write(&path, &[], &named_keys(&on)).unwrap();
        assert!(
            !Settings::load(&path).theme_sync_with_os,
            "a file with the switch keeps its own value"
        );
    }

    #[test]
    fn two_processes_keep_the_switch_and_the_font() {
        let path = file_in("settings-two-switch");
        let start = Settings::default();

        // The first process turns Sync with OS off. The second one still has it on and
        // changes the font.
        let mut first = start.clone();
        first.theme_sync_with_os = false;
        let mut second = start.clone();
        second.font_size = 16.;

        write(&path, &changes(&start, &first), &named_keys(&first)).unwrap();
        write(&path, &changes(&start, &second), &named_keys(&second)).unwrap();

        let stored = Settings::load(&path);
        assert!(
            !stored.theme_sync_with_os,
            "the switch of the first process stays"
        );
        assert_eq!(
            stored.font_size, 16.,
            "the font of the second process is written"
        );
    }

    #[test]
    fn a_write_creates_a_missing_file_with_the_changed_keys() {
        let path = test_support::scratch("settings-created")
            .join("nested")
            .join("settings.json");
        write(&path, &[("soft_wrap".to_owned(), Value::Bool(true))], &[]).unwrap();
        assert!(Settings::load(&path).soft_wrap);
    }
}

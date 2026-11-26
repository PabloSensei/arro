//! The failure of a build import, as the server reports it.
//!
//! The server logs the exception text of a failed Gradle or Maven import and, in
//! some builds, also publishes an error diagnostic on the build script. The log is
//! the source that always arrives; the diagnostic, when it comes, names the same
//! file and line.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base::program::executable;
use lsp_client::lsp_types::{Diagnostic, DiagnosticSeverity};
use lsp_client::uri::{file_path, server_uri};
use serde_json::Value;
use tokio::sync::watch;

/// A build script the server imports before it knows the project.
const BUILD_FILES: &[&str] = &[
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "pom.xml",
];

/// The build tool that could not import the project.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BuildTool {
    Maven,
    Gradle,
}

/// Why the server could not import the project.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportFailure {
    /// The build script the server named, or an empty path when the report names none.
    pub file: PathBuf,
    /// The line of the script, counted from zero.
    pub line: u32,
    /// The whole text of the diagnostic.
    pub message: String,
    /// The line that names the cause, for the status bar and the notification.
    pub summary: String,
    /// The JDK the build tool tried to start with and did not find, when the report
    /// says that the folder of a removed JDK is still there.
    pub jdk: Option<PathBuf>,
    /// The tool that failed, when the report names it.
    pub tool: Option<BuildTool>,
    /// One more import of the same project may clear this failure without the user
    /// seeing it. Set for every refusal of an import attempt that names an
    /// initialization script the server asks for before it unpacks it, as long as the
    /// repeat import has not begun. A refusal after that begin is a real error.
    pub retry: bool,
}

impl ImportFailure {
    fn from_diagnostic(file: &Path, diagnostic: &Diagnostic) -> Self {
        let message = diagnostic.message.clone();
        Self {
            file: file.to_path_buf(),
            line: diagnostic.range.start.line,
            summary: short(&message),
            message,
            jdk: None,
            tool: tool_of(&diagnostic.message, file),
            retry: false,
        }
    }
}

/// The last line is the cause in both the log and the diagnostic. A log line carries
/// the marker fields after it, which are not part of the cause. A report without the
/// marker is a stack trace, where the deepest `Caused by` names the cause.
fn short(message: &str) -> String {
    let line = message
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    if let Some((head, _)) = ["; code:", "; resource:"]
        .iter()
        .find_map(|marker| line.split_once(marker))
    {
        return head.trim().to_owned();
    }
    let cause = message
        .lines()
        .map(str::trim)
        .rev()
        .find_map(|line| line.strip_prefix("Caused by:"))
        .map(str::trim);
    match cause {
        // The exception class is not the cause; the text after it is.
        Some(cause) => cause
            .split_once(": ")
            .map_or(cause, |(_, text)| text)
            .to_owned(),
        None => line.to_owned(),
    }
}

/// The build script and the line a Gradle failure names, from the text of the server's
/// error. The marker of the server names the project folder instead of the script in
/// some builds, while the exception itself still names the script.
fn build_file(message: &str) -> Option<(PathBuf, u32)> {
    let after = message.rsplit_once("Build file '")?.1;
    let (path, rest) = after.split_once('\'')?;
    let line = rest
        .split_once("line: ")?
        .1
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((PathBuf::from(path), line))
}

/// The script and line of a marker field. It names a folder in some builds.
fn marker_file(message: &str) -> Option<(PathBuf, u32)> {
    let after = message.rsplit_once("; resource: ")?.1;
    let (path, rest) = after.split_once(';')?;
    let path = PathBuf::from(path.trim());
    if !is_build_file(&path) {
        return None;
    }
    let line = rest
        .split_once("line: ")?
        .1
        .split(|c: char| !c.is_ascii_digit())
        .next()?
        .parse()
        .ok()?;
    Some((path, line))
}

/// The JDK of a report that says the build tool could not start because the folder of
/// a removed JDK is still there. Gradle names the executable it tried:
/// `Tried location: C:\Program Files\Amazon Corretto\jdk25.0.3_9\bin\java.exe`.
fn java_home_problem(message: &str) -> Option<PathBuf> {
    let after = message.rsplit_once("Tried location:")?.1;
    let tried = after.lines().next()?.trim();
    let (bin, file) = tried.rsplit_once(['\\', '/'])?;
    if file != "java" && file != "java.exe" {
        return None;
    }
    let (home, last) = bin.rsplit_once(['\\', '/'])?;
    if !last.eq_ignore_ascii_case("bin") {
        return None;
    }
    Some(PathBuf::from(home))
}

/// The build tool a report or a diagnostic names.
fn tool_of(message: &str, file: &Path) -> Option<BuildTool> {
    let name = file
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    if message.contains("Gradle")
        || name.starts_with("build.gradle")
        || name.starts_with("settings.gradle")
    {
        return Some(BuildTool::Gradle);
    }
    if message.contains("Maven") || name == "pom.xml" {
        return Some(BuildTool::Maven);
    }
    None
}

/// The header of the report the server publishes when a build of the workspace left
/// errors. Its entries follow as `message:` lines, the refusal of the build tool first.
const WORKSPACE_BUILD: &str = "Error occured while building workspace";

/// Reports that a build tool could not import the project. A report of another kind,
/// such as the stack trace of a failed request, is not an import failure. The report of
/// a build of the workspace is read apart: it names the refusal of the build tool and
/// then every source of the project that does not compile, and only the refusal is an
/// import failure.
fn import_report(message: &str) -> bool {
    [
        "JavaHomeException",
        "; resource:",
        "Could not fetch model",
        "failed due to an error connecting to the",
        "failed due to an error in the referenced",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

/// The entries of a build report of the workspace. The server puts the refusal of the
/// build tool first and then one line for every source that does not compile.
fn build_report(message: &str) -> Option<Vec<&str>> {
    let body = message.split_once(WORKSPACE_BUILD)?.1;
    let mut entries = Vec::new();
    let mut rest = body;
    while let Some((_, after)) = rest.split_once(" message: ") {
        let end = after.find("\n message: ").unwrap_or(after.len());
        entries.push(after[..end].trim_end());
        rest = &after[end..];
    }
    (!entries.is_empty()).then_some(entries)
}

/// A source of the project that does not compile names its line and its file in fields.
/// A build script names them the same way and is still the failure of the import.
fn source_error(entry: &str) -> bool {
    entry.contains("; line: ") && marker_file(entry).is_none()
}

/// The text of a report entry without the fields the server appends to it.
fn cause_body(entry: &str) -> &str {
    entry.split_once("; code: ").map_or(entry, |(body, _)| body)
}

/// An import failure whose cause is `cause`; `text` is the whole text of the report.
fn failure_of(cause: &str, text: &str) -> ImportFailure {
    let (file, line) = build_file(cause)
        .or_else(|| marker_file(cause))
        .map(|(file, line)| (file, line.saturating_sub(1)))
        .unwrap_or_default();
    let jdk = java_home_problem(cause);
    let summary = match &jdk {
        Some(home) => format!(
            "JDK at {} was not removed completely: {} is missing",
            home.display(),
            home.join("bin").join(executable("java")).display()
        ),
        None => short(cause),
    };
    ImportFailure {
        tool: tool_of(cause, &file),
        file,
        line,
        summary,
        message: text.to_owned(),
        jdk,
        retry: false,
    }
}

/// An import failure from `window/logMessage` or `window/showMessage`, where the server
/// puts the exception text. The marker report of the workspace build names the script in
/// its fields; the report of a failed model query, which comes without the marker, names
/// it in the exception when it names one at all. A report about the JDK of the build tool
/// names no script: the summary then names the JDK that is gone.
fn from_log(message: &str) -> Option<ImportFailure> {
    // A build of the workspace that left sources without a type lists the refusal of the
    // import first and then one line per source. The source errors are diagnostics of
    // the project, which the editor shows like any other, so only the first entry that
    // names no source is the failure of the import.
    if let Some(entries) = build_report(message) {
        let cause = entries.iter().find(|entry| !source_error(entry))?;
        return Some(failure_of(cause, cause_body(cause)));
    }
    import_report(message).then(|| failure_of(message, message))
}

/// The progress titles of a Gradle import. A new import makes the old failure say
/// nothing: it either succeeds, and the failure is gone, or it fails again and the
/// report of the new failure replaces it. Other work of the server, such as a build
/// or a refresh, carries other titles.
const GRADLE_IMPORT_TITLES: &[&str] = &[
    "Synchronize Gradle projects with workspace",
    "Synchronizing projects",
    "Importing Gradle project(s)",
];

fn import_started(params: &Value) -> bool {
    let value = params.get("value");
    if value
        .and_then(|value| value.get("kind"))
        .and_then(Value::as_str)
        != Some("begin")
    {
        return false;
    }
    value
        .and_then(|value| value.get("title"))
        .and_then(Value::as_str)
        .is_some_and(|title| GRADLE_IMPORT_TITLES.contains(&title))
}

/// A build script, such as `build.gradle` or `pom.xml`. Only a script can carry an
/// import failure; a diagnostic of the project folder is not one.
pub fn is_build_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| BUILD_FILES.contains(&name))
}

/// The failures of the imports of the server, and the imports that run now.
///
/// The status bar reads both: a failure says why the project is not ready, and an
/// import in flight says `importing` instead of a line the server left behind.
pub(crate) struct ImportState {
    failures: watch::Sender<Option<ImportFailure>>,
    /// The tokens of the Gradle imports whose progress began and has not ended.
    running: Mutex<HashSet<String>>,
    running_now: watch::Sender<bool>,
    /// The Gradle imports that began in this server session, in order. A repeat import
    /// raises it, so a reader tells the refusals of the first attempt from the refusals
    /// of the repeat.
    starts: watch::Sender<u64>,
}

impl ImportState {
    pub(crate) fn new() -> Self {
        let (failures, _) = watch::channel(None);
        let (running_now, _) = watch::channel(false);
        let (starts, _) = watch::channel(0);
        Self {
            failures,
            running: Mutex::new(HashSet::new()),
            running_now,
            starts,
        }
    }

    pub(crate) fn watch_failure(&self) -> watch::Receiver<Option<ImportFailure>> {
        self.failures.subscribe()
    }

    /// Whether a Gradle import runs now. A retry of a failed import shows `importing`
    /// on this line, the way the first start of the project does.
    pub(crate) fn watch_running(&self) -> watch::Receiver<bool> {
        self.running_now.subscribe()
    }

    /// How many Gradle imports have begun in this server session. A refusal that comes
    /// after a begin newer than the one the repeat was asked for belongs to the repeat.
    pub(crate) fn watch_starts(&self) -> watch::Receiver<u64> {
        self.starts.subscribe()
    }

    /// A new server imports the project again, so an old failure says nothing.
    pub(crate) fn reset(&self) {
        self.failures.send_replace(None);
        self.running
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clear();
        self.running_now.send_replace(false);
    }

    /// Takes the failure out of the messages of the server: the error and warning lines
    /// of the log, the error and warning balloons, and the diagnostics on a build script.
    /// A new import of the project clears the failure, so that an old failure is not
    /// shown while the new one is not known yet. A project status of `OK` does not clear
    /// it: the server sends that status for the project it imported before a reimport
    /// fails, so a failure that arrives while a project is still known would vanish at
    /// once.
    pub(crate) fn observe(&self, method: &str, params: &Value) {
        if method == "$/progress" {
            self.progress(params);
            return;
        }
        // The server reports the failed import of a Gradle project as a warning, not as
        // an error, so every type of a log line can carry one.
        if method == "window/logMessage" {
            let Some(message) = params.get("message").and_then(Value::as_str) else {
                return;
            };
            self.store(from_log(message));
            return;
        }
        if method == "window/showMessage" {
            let Some(severity) = params.get("type").and_then(Value::as_i64) else {
                return;
            };
            if !matches!(severity, 1 | 2) {
                return;
            }
            let Some(message) = params.get("message").and_then(Value::as_str) else {
                return;
            };
            self.store(from_log(message));
            return;
        }
        if method != "textDocument/publishDiagnostics" {
            return;
        }
        let Some(uri) = params.get("uri").and_then(Value::as_str) else {
            return;
        };
        let Some(file) = server_uri(uri).and_then(|uri| file_path(&uri)) else {
            return;
        };
        if !is_build_file(&file) {
            return;
        }
        let Some(diagnostics) = params.get("diagnostics").cloned() else {
            return;
        };
        let Ok(diagnostics) = serde_json::from_value::<Vec<Diagnostic>>(diagnostics) else {
            return;
        };
        let failure = diagnostics
            .iter()
            .find(|diagnostic| diagnostic.severity == Some(DiagnosticSeverity::ERROR))
            .map(|diagnostic| ImportFailure::from_diagnostic(&file, diagnostic));
        let current = self.failures.borrow().clone();
        match failure {
            Some(failure) if current.as_ref() != Some(&failure) => {
                self.failures.send_replace(Some(failure));
            }
            None if current.is_some_and(|current| current.file == file) => {
                self.failures.send_replace(None);
            }
            _ => {}
        }
    }

    /// A Gradle import keeps the line at `importing` from its begin to its end, and a
    /// new import makes the failure of the old one say nothing.
    fn progress(&self, params: &Value) {
        let Some(value) = params.get("value") else {
            return;
        };
        let Some(kind) = value.get("kind").and_then(Value::as_str) else {
            return;
        };
        let Some(token) = params.get("token") else {
            return;
        };
        let token = token.to_string();
        match kind {
            "begin" => {
                if !import_started(params) {
                    return;
                }
                self.starts.send_modify(|starts| *starts += 1);
                self.set_running(&token, true);
                if self.failures.borrow().is_some() {
                    self.failures.send_replace(None);
                }
            }
            "end" => self.set_running(&token, false),
            _ => {}
        }
    }

    fn set_running(&self, token: &str, running: bool) {
        let now = {
            let mut active = self
                .running
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            if running {
                active.insert(token.to_owned());
            } else {
                active.remove(token);
            }
            !active.is_empty()
        };
        self.running_now.send_if_modified(|flag| {
            let changed = *flag != now;
            *flag = now;
            changed
        });
    }

    /// Stores a failure found in a message, and does nothing when the message carries none.
    fn store(&self, failure: Option<ImportFailure>) {
        let Some(failure) = failure else {
            return;
        };
        if self.failures.borrow().as_ref() != Some(&failure) {
            self.failures.send_replace(Some(failure));
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::{Value, json};

    use super::{ImportFailure, ImportState};

    fn observe(method: &str, params: &Value, state: &ImportState) {
        state.observe(method, params);
    }

    fn diagnostics(uri: &str, diagnostics: serde_json::Value) -> serde_json::Value {
        json!({"uri": uri, "diagnostics": diagnostics})
    }

    fn error(message: &str, line: u32) -> serde_json::Value {
        json!({
            "range": {
                "start": {"line": line, "character": 0},
                "end": {"line": line, "character": 0},
            },
            "severity": 1,
            "source": "Java",
            "message": message,
        })
    }

    #[test]
    fn every_build_script_and_no_source_file_is_a_build_file() {
        for name in [
            "pom.xml",
            "build.gradle",
            "build.gradle.kts",
            "settings.gradle",
            "settings.gradle.kts",
        ] {
            assert!(super::is_build_file(Path::new(name)), "{name}");
        }
        assert!(!super::is_build_file(Path::new("Main.java")));
        assert!(!super::is_build_file(Path::new("notes.txt")));
    }

    #[cfg(windows)]
    #[test]
    fn an_error_on_a_build_script_names_the_file_line_and_cause() {
        let failures = ImportState::new();
        observe(
            "textDocument/publishDiagnostics",
            &diagnostics(
                "file:///c:/p/build.gradle.kts",
                json!([error(
                    "Could not configure the build.\nTask with name 'postRelease' not found.",
                    24
                )]),
            ),
            &failures,
        );
        let failure = failures.failures.borrow().clone().unwrap();
        assert!(
            failure.file.ends_with("build.gradle.kts"),
            "{:?}",
            failure.file
        );
        assert!(failure.file.is_absolute());
        assert_eq!(failure.line, 24);
        assert_eq!(failure.summary, "Task with name 'postRelease' not found.");
        assert!(failure.message.starts_with("Could not configure"));
    }

    #[test]
    fn diagnostics_of_sources_and_of_warnings_are_not_import_failures() {
        let failures = ImportState::new();
        observe(
            "textDocument/publishDiagnostics",
            &diagnostics("file:///c:/p/src/App.java", json!([error("bad", 3)])),
            &failures,
        );
        observe(
            "textDocument/publishDiagnostics",
            &diagnostics(
                "file:///c:/p/build.gradle",
                json!([{
                    "range": {
                        "start": {"line": 1, "character": 0},
                        "end": {"line": 1, "character": 0},
                    },
                    "severity": 2,
                    "message": "a deprecation",
                }]),
            ),
            &failures,
        );
        assert!(failures.failures.borrow().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn an_empty_list_of_a_script_clears_its_failure_and_keeps_another() {
        let failures = ImportState::new();
        failures.failures.send_replace(Some(ImportFailure {
            file: "C:\\p\\build.gradle.kts".into(),
            line: 4,
            message: "old".into(),
            summary: "old".into(),
            jdk: None,
            tool: None,
            retry: false,
        }));

        observe(
            "textDocument/publishDiagnostics",
            &diagnostics("file:///c:/p/build.gradle.kts", json!([])),
            &failures,
        );
        assert!(failures.failures.borrow().is_none());

        failures.failures.send_replace(Some(ImportFailure {
            file: "C:\\p\\pom.xml".into(),
            line: 1,
            message: "old".into(),
            summary: "old".into(),
            jdk: None,
            tool: None,
            retry: false,
        }));
        observe(
            "textDocument/publishDiagnostics",
            &diagnostics("file:///c:/p/build.gradle.kts", json!([])),
            &failures,
        );
        assert!(
            failures
                .failures
                .borrow()
                .as_ref()
                .is_some_and(|failure| failure.file.ends_with("pom.xml"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_marker_report_in_the_log_names_the_script_its_line_and_cause() {
        let failures = ImportState::new();
        let message = "Nov 6, 2026 Error occured while building workspace. Details: \r\n \
             message: The supplied phased action failed with an exception.\r\n\
             Build file 'C:\\p\\build.gradle.kts' line: 25\r\n\
             Task with name 'postRelease' not found in root project 'demo'.; \
             code: 0; resource: C:/p;";
        observe(
            "window/logMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        let failure = failures.failures.borrow().clone().unwrap();
        assert!(failure.file.ends_with("build.gradle.kts"));
        assert_eq!(failure.line, 24);
        assert_eq!(
            failure.summary,
            "Task with name 'postRelease' not found in root project 'demo'."
        );
    }

    #[test]
    fn a_stack_trace_without_an_import_report_is_ignored() {
        let failures = ImportState::new();
        observe(
            "window/logMessage",
            &json!({
                "type": 1,
                "message": "Could not compile the build.\nCaused by: Build file 'C:\\p\\build.gradle' line: 3\n\tat demo.Run"
            }),
            &failures,
        );
        assert!(failures.failures.borrow().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn a_report_without_the_marker_names_the_script_line_and_deepest_cause() {
        let failures = ImportState::new();
        let message = "Nov 6, 2026, 7:04:17 PM Could not fetch model of type 'Map' using \
             connection to Gradle distribution 'https://services.gradle.org/distributions/gradle-9.7.1-bin.zip'.\n\
             org.gradle.tooling.BuildException: Could not fetch model\r\n\
             Caused by: org.gradle.internal.exceptions.LocationAwareException: Build file \
             'C:\\p\\build.gradle' line: 5\r\n\
             A problem occurred evaluating root project 'demo'.\r\n\
             Caused by: org.gradle.api.UnknownTaskException: Task with name 'noSuchTask' not \
             found in root project 'demo'.\r\n\
             \tat demo.Run\r\n\
             \t... 194 more\r\n";
        observe(
            "window/logMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        let failure = failures.failures.borrow().clone().unwrap();
        assert!(failure.file.ends_with("build.gradle"));
        assert_eq!(failure.line, 4);
        assert_eq!(
            failure.summary,
            "Task with name 'noSuchTask' not found in root project 'demo'."
        );
    }

    #[test]
    fn the_start_of_a_gradle_import_clears_a_failure_from_the_log() {
        let failures = ImportState::new();
        let message = "Nov 6, 2026 Error occured while building workspace. Details: \r\n \
             Build file 'C:\\p\\build.gradle.kts' line: 25\r\n \
             Task with name 'postRelease' not found in root project 'demo'.; \
             code: 0; resource: C:/p;";
        observe(
            "window/logMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        assert!(failures.failures.borrow().is_some());

        observe(
            "$/progress",
            &json!({
                "token": "1",
                "value": {"kind": "begin", "title": "Synchronize Gradle projects with workspace"}
            }),
            &failures,
        );
        assert!(failures.failures.borrow().is_none());
    }

    #[test]
    fn other_progress_and_source_diagnostics_keep_a_failure() {
        let failures = ImportState::new();
        failures.failures.send_replace(Some(ImportFailure {
            file: "C:\\p\\build.gradle.kts".into(),
            line: 4,
            message: "old".into(),
            summary: "old".into(),
            jdk: None,
            tool: None,
            retry: false,
        }));
        observe(
            "$/progress",
            &json!({"token": "1", "value": {"kind": "begin", "title": "Building"}}),
            &failures,
        );
        observe(
            "textDocument/publishDiagnostics",
            &diagnostics("file:///c:/p/src/App.java", json!([])),
            &failures,
        );
        observe(
            "textDocument/publishDiagnostics",
            &diagnostics("file:///c:/p/src/App.java", json!([error("bad", 3)])),
            &failures,
        );
        assert!(failures.failures.borrow().is_some());
    }

    #[test]
    fn a_warning_of_a_gradle_import_names_the_removed_jdk() {
        let failures = ImportState::new();
        let message = "Nov 19, 2026, 7:43:43 PM Synchronize project petclinic failed due to \
             an error connecting to the Gradle build.\n\
             org.gradle.tooling.GradleConnectionException: Could not run phased build action.\n\
             \tat org.gradle.tooling.internal.consumer.ResultHandlerAdapter.onFailure\n\
             Caused by: org.gradle.internal.jvm.JavaHomeException: The supplied javaHome \
             seems to be invalid. I cannot find the java executable. Tried location: \
             C:\\Program Files\\Amazon Corretto\\jdk25.0.3_9\\bin\\java.exe\n\
             \tat org.gradle.internal.jvm.Jvm.findExecutable(Jvm.java:191)\n";
        observe(
            "window/logMessage",
            &json!({"type": 2, "message": message}),
            &failures,
        );
        let failure = failures.failures.borrow().clone().unwrap();
        assert_eq!(failure.tool, Some(super::BuildTool::Gradle));
        assert_eq!(
            failure.jdk.as_deref(),
            Some(Path::new(r"C:\Program Files\Amazon Corretto\jdk25.0.3_9"))
        );
        assert!(
            failure
                .summary
                .contains(r"C:\Program Files\Amazon Corretto\jdk25.0.3_9"),
            "{}",
            failure.summary
        );
        assert!(
            failure.summary.contains("not removed completely"),
            "{}",
            failure.summary
        );
        assert!(failure.file.as_os_str().is_empty());
    }

    #[test]
    fn a_project_status_of_ok_does_not_hide_a_failure_of_a_reimport() {
        let failures = ImportState::new();
        let message = "Could not fetch model of type 'Map' using connection to Gradle \
             distribution 'https://services.gradle.org/distributions/gradle-9.7.1-bin.zip'.\n\
             Caused by: Task with name 'noSuchTask' not found in root project 'demo'.";
        observe(
            "window/logMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        assert!(failures.failures.borrow().is_some());
        // The server reports `OK` for the project it imported before the failed reimport,
        // so the failure must stay until a new import starts.
        observe(
            "language/status",
            &json!({"type": "ProjectStatus", "message": "OK"}),
            &failures,
        );
        assert!(failures.failures.borrow().is_some());

        observe(
            "$/progress",
            &json!({
                "token": "1",
                "value": {"kind": "begin", "title": "Synchronize Gradle projects with workspace"}
            }),
            &failures,
        );
        assert!(
            failures.failures.borrow().is_none(),
            "a new import clears the old failure"
        );
    }

    #[test]
    fn an_error_balloon_can_carry_the_failure_too() {
        let failures = ImportState::new();
        let message = "Could not fetch model of type 'Map'.\n\
             Caused by: java.lang.IllegalStateException: the build cannot start";
        observe(
            "window/showMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        let failure = failures.failures.borrow().clone().unwrap();
        assert_eq!(failure.summary, "the build cannot start");
    }

    #[test]
    fn another_method_is_ignored() {
        let failures = ImportState::new();
        observe(
            "window/logMessage",
            &json!({"type": 1, "message": "Build file 'C:\\p\\build.gradle' line: 2"}),
            &failures,
        );
        assert!(failures.failures.borrow().is_none());
    }

    #[test]
    fn the_build_report_of_the_workspace_names_the_refusal_not_a_source_error() {
        // The session of 2026-11-21 11:54 of `petclinic-check`: the Gradle import refused
        // the JVM of the project, and the build that followed listed every source the
        // missing class path left without a type. The last line of the report was a test
        // source, which used to become the summary shown as the failed import.
        let failures = ImportState::new();
        let message = "Error occured while building workspace. Details: \r\n \
             message: Could not run phased build action using connection to Gradle distribution \
             'https://services.gradle.org/distributions/gradle-9.5.1-bin.zip'.\r\n\
             Gradle requires JVM 17 or later to run. Your build is currently configured to use \
             JVM 8.; code: 0; resource: E:/java-ide-live/petclinic-check;\r\n \
             message: The import org.junit cannot be resolved; code: 268435846; resource: \
             E:/java-ide-live/petclinic-check/src/test/java/org/springframework/samples/petclinic/vet/VetTests.java; \
             line: 18\r\n \
             message: Test cannot be resolved to a type; code: 16777218; resource: \
             E:/java-ide-live/petclinic-check/src/test/java/org/springframework/samples/petclinic/vet/VetTests.java; \
             line: 28";
        observe(
            "window/logMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        let failure = failures.failures.borrow().clone().unwrap();
        assert_eq!(failure.tool, Some(super::BuildTool::Gradle));
        assert_eq!(
            failure.summary,
            "Gradle requires JVM 17 or later to run. Your build is currently configured to use JVM 8."
        );
        assert!(
            !failure.summary.contains("Test cannot be resolved"),
            "a source error is a diagnostic, not the cause of the failure: {}",
            failure.summary
        );
        assert!(
            !failure.message.contains("; code:"),
            "the raw fields of the report are not the text of the failure: {}",
            failure.message
        );
        assert!(failure.file.as_os_str().is_empty());
    }

    #[test]
    fn a_build_report_of_sources_alone_is_not_an_import_failure() {
        let failures = ImportState::new();
        let message = "Error occured while building workspace. Details: \r\n \
             message: The import org.junit cannot be resolved; code: 268435846; \
             resource: E:/p/src/test/java/demo/AppTests.java; line: 18\r\n \
             message: Test cannot be resolved to a type; code: 16777218; \
             resource: E:/p/src/test/java/demo/AppTests.java; line: 28";
        observe(
            "window/showMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        assert!(
            failures.failures.borrow().is_none(),
            "the source errors reach Problems as diagnostics"
        );
    }

    #[test]
    fn a_build_report_of_a_build_script_is_an_import_failure() {
        let failures = ImportState::new();
        let message = "Error occured while building workspace. Details: \r\n \
             message: Could not configure the build.; code: 0; \
             resource: E:/p/build.gradle; line: 24";
        observe(
            "window/logMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        let failure = failures.failures.borrow().clone().unwrap();
        assert!(failure.file.ends_with("build.gradle"));
        assert_eq!(failure.line, 23);
        assert_eq!(failure.summary, "Could not configure the build.");
    }

    #[test]
    fn a_gradle_import_runs_from_its_begin_to_its_end() {
        let failures = ImportState::new();
        let running = failures.watch_running();
        observe(
            "$/progress",
            &json!({
                "token": "1",
                "value": {"kind": "begin", "title": "Importing Gradle project(s)"}
            }),
            &failures,
        );
        assert!(*running.borrow(), "the import began");

        observe(
            "$/progress",
            &json!({"token": "1", "value": {"kind": "report", "message": "half"}}),
            &failures,
        );
        assert!(*running.borrow(), "a report does not end the import");

        observe(
            "$/progress",
            &json!({"token": "1", "value": {"kind": "end"}}),
            &failures,
        );
        assert!(!*running.borrow(), "the import ended");
    }

    #[test]
    fn every_gradle_import_begin_is_counted() {
        let failures = ImportState::new();
        let starts = failures.watch_starts();
        assert_eq!(*starts.borrow(), 0);

        observe(
            "$/progress",
            &json!({"token": "1", "value": {"kind": "begin", "title": "Building"}}),
            &failures,
        );
        assert_eq!(
            *starts.borrow(),
            0,
            "another task of the server is no import"
        );

        observe(
            "$/progress",
            &json!({
                "token": "1",
                "value": {"kind": "begin", "title": "Importing Gradle project(s)"}
            }),
            &failures,
        );
        assert_eq!(*starts.borrow(), 1, "the first import began");

        observe(
            "$/progress",
            &json!({
                "token": "2",
                "value": {"kind": "begin", "title": "Synchronize Gradle projects with workspace"}
            }),
            &failures,
        );
        assert_eq!(*starts.borrow(), 2, "the repeat import began");

        observe(
            "$/progress",
            &json!({"token": "2", "value": {"kind": "end"}}),
            &failures,
        );
        assert_eq!(*starts.borrow(), 2, "the end of an import counts nothing");
    }

    #[test]
    fn another_progress_does_not_count_as_an_import() {
        let failures = ImportState::new();
        let running = failures.watch_running();
        observe(
            "$/progress",
            &json!({"token": "1", "value": {"kind": "begin", "title": "Building"}}),
            &failures,
        );
        assert!(!*running.borrow());
        observe(
            "$/progress",
            &json!({"token": "1", "value": {"kind": "end"}}),
            &failures,
        );
        assert!(!*running.borrow());
    }
}

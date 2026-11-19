//! The failure of a build import, as the server reports it.
//!
//! The server logs the exception text of a failed Gradle or Maven import and, in
//! some builds, also publishes an error diagnostic on the build script. The log is
//! the source that always arrives; the diagnostic, when it comes, names the same
//! file and line.

use std::path::{Path, PathBuf};
use std::str::FromStr;

use base::program::executable;
use lsp_client::lsp_types::{Diagnostic, DiagnosticSeverity, Uri};
use lsp_client::uri::file_path;
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

/// Reports that a build tool could not import the project. A report of another kind,
/// such as the stack trace of a failed request, is not an import failure.
fn import_report(message: &str) -> bool {
    [
        "JavaHomeException",
        "; resource:",
        "Could not fetch model",
        "Error occured while building workspace",
        "failed due to an error connecting to the",
        "failed due to an error in the referenced",
    ]
    .iter()
    .any(|marker| message.contains(marker))
}

/// An import failure from `window/logMessage` or `window/showMessage`, where the server
/// puts the exception text. The marker report of the workspace build names the script in
/// its fields; the report of a failed model query, which comes without the marker, names
/// it in the exception when it names one at all. A report about the JDK of the build tool
/// names no script: the summary then names the JDK that is gone.
fn from_log(message: &str) -> Option<ImportFailure> {
    if !import_report(message) {
        return None;
    }
    let (file, line) = build_file(message)
        .or_else(|| marker_file(message))
        .map(|(file, line)| (file, line.saturating_sub(1)))
        .unwrap_or_default();
    let jdk = java_home_problem(message);
    let summary = match &jdk {
        Some(home) => format!(
            "JDK at {} was not removed completely: {} is missing",
            home.display(),
            home.join("bin").join(executable("java")).display()
        ),
        None => short(message),
    };
    Some(ImportFailure {
        tool: tool_of(message, &file),
        file,
        line,
        summary,
        message: message.to_owned(),
        jdk,
    })
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

/// Takes the failure out of the messages of the server: the error and warning lines of
/// the log, the error and warning balloons, and the diagnostics on a build script. A new
/// import of the project clears the failure, so that an old failure is not shown while
/// the new one is not known yet. A project status of `OK` does not clear it: the server
/// sends that status for the project it imported before a reimport fails, so a failure
/// that arrives while a project is still known would vanish at once.
pub(crate) fn observe(
    method: &str,
    params: &Value,
    failures: &watch::Sender<Option<ImportFailure>>,
) {
    if method == "$/progress" {
        if import_started(params) && failures.borrow().is_some() {
            failures.send_replace(None);
        }
        return;
    }
    // The server reports the failed import of a Gradle project as a warning, not as an
    // error, so every type of a log line can carry one.
    if method == "window/logMessage" {
        let Some(message) = params.get("message").and_then(Value::as_str) else {
            return;
        };
        store(from_log(message), failures);
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
        store(from_log(message), failures);
        return;
    }
    if method != "textDocument/publishDiagnostics" {
        return;
    }
    let Some(uri) = params.get("uri").and_then(Value::as_str) else {
        return;
    };
    let Some(file) = Uri::from_str(uri).ok().as_ref().and_then(file_path) else {
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
    let current = failures.borrow().clone();
    match failure {
        Some(failure) if current.as_ref() != Some(&failure) => {
            failures.send_replace(Some(failure));
        }
        None if current.is_some_and(|current| current.file == file) => {
            failures.send_replace(None);
        }
        _ => {}
    }
}

/// Stores a failure found in a message, and does nothing when the message carries none.
fn store(failure: Option<ImportFailure>, failures: &watch::Sender<Option<ImportFailure>>) {
    let Some(failure) = failure else {
        return;
    };
    if failures.borrow().as_ref() != Some(&failure) {
        failures.send_replace(Some(failure));
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use serde_json::json;
    use tokio::sync::watch;

    use super::{ImportFailure, observe};

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
        let (failures, watch) = watch::channel(None);
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
        let failure = watch.borrow().clone().unwrap();
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
        let (failures, watch) = watch::channel(None);
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
        assert!(watch.borrow().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn an_empty_list_of_a_script_clears_its_failure_and_keeps_another() {
        let (failures, watch) = watch::channel(Some(ImportFailure {
            file: "C:\\p\\build.gradle.kts".into(),
            line: 4,
            message: "old".into(),
            summary: "old".into(),
            jdk: None,
            tool: None,
        }));

        observe(
            "textDocument/publishDiagnostics",
            &diagnostics("file:///c:/p/build.gradle.kts", json!([])),
            &failures,
        );
        assert!(watch.borrow().is_none());

        failures.send_replace(Some(ImportFailure {
            file: "C:\\p\\pom.xml".into(),
            line: 1,
            message: "old".into(),
            summary: "old".into(),
            jdk: None,
            tool: None,
        }));
        observe(
            "textDocument/publishDiagnostics",
            &diagnostics("file:///c:/p/build.gradle.kts", json!([])),
            &failures,
        );
        assert!(
            watch
                .borrow()
                .as_ref()
                .is_some_and(|failure| failure.file.ends_with("pom.xml"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_marker_report_in_the_log_names_the_script_its_line_and_cause() {
        let (failures, watch) = watch::channel(None);
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
        let failure = watch.borrow().clone().unwrap();
        assert!(failure.file.ends_with("build.gradle.kts"));
        assert_eq!(failure.line, 24);
        assert_eq!(
            failure.summary,
            "Task with name 'postRelease' not found in root project 'demo'."
        );
    }

    #[test]
    fn a_stack_trace_without_an_import_report_is_ignored() {
        let (failures, watch) = watch::channel(None);
        observe(
            "window/logMessage",
            &json!({
                "type": 1,
                "message": "Could not compile the build.\nCaused by: Build file 'C:\\p\\build.gradle' line: 3\n\tat demo.Run"
            }),
            &failures,
        );
        assert!(watch.borrow().is_none());
    }

    #[cfg(windows)]
    #[test]
    fn a_report_without_the_marker_names_the_script_line_and_deepest_cause() {
        let (failures, watch) = watch::channel(None);
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
        let failure = watch.borrow().clone().unwrap();
        assert!(failure.file.ends_with("build.gradle"));
        assert_eq!(failure.line, 4);
        assert_eq!(
            failure.summary,
            "Task with name 'noSuchTask' not found in root project 'demo'."
        );
    }

    #[test]
    fn the_start_of_a_gradle_import_clears_a_failure_from_the_log() {
        let (failures, watch) = watch::channel(None);
        let message = "Nov 6, 2026 Error occured while building workspace. Details: \r\n \
             Build file 'C:\\p\\build.gradle.kts' line: 25\r\n \
             Task with name 'postRelease' not found in root project 'demo'.; \
             code: 0; resource: C:/p;";
        observe(
            "window/logMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        assert!(watch.borrow().is_some());

        observe(
            "$/progress",
            &json!({
                "token": "1",
                "value": {"kind": "begin", "title": "Synchronize Gradle projects with workspace"}
            }),
            &failures,
        );
        assert!(watch.borrow().is_none());
    }

    #[test]
    fn other_progress_and_source_diagnostics_keep_a_failure() {
        let (failures, watch) = watch::channel(Some(ImportFailure {
            file: "C:\\p\\build.gradle.kts".into(),
            line: 4,
            message: "old".into(),
            summary: "old".into(),
            jdk: None,
            tool: None,
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
        assert!(watch.borrow().is_some());
    }

    #[test]
    fn a_warning_of_a_gradle_import_names_the_removed_jdk() {
        let (failures, watch) = watch::channel(None);
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
        let failure = watch.borrow().clone().unwrap();
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
        let (failures, watch) = watch::channel(None);
        let message = "Could not fetch model of type 'Map' using connection to Gradle \
             distribution 'https://services.gradle.org/distributions/gradle-9.7.1-bin.zip'.\n\
             Caused by: Task with name 'noSuchTask' not found in root project 'demo'.";
        observe(
            "window/logMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        assert!(watch.borrow().is_some());
        // The server reports `OK` for the project it imported before the failed reimport,
        // so the failure must stay until a new import starts.
        observe(
            "language/status",
            &json!({"type": "ProjectStatus", "message": "OK"}),
            &failures,
        );
        assert!(watch.borrow().is_some());

        observe(
            "$/progress",
            &json!({
                "token": "1",
                "value": {"kind": "begin", "title": "Synchronize Gradle projects with workspace"}
            }),
            &failures,
        );
        assert!(
            watch.borrow().is_none(),
            "a new import clears the old failure"
        );
    }

    #[test]
    fn an_error_balloon_can_carry_the_failure_too() {
        let (failures, watch) = watch::channel(None);
        let message = "Could not fetch model of type 'Map'.\n\
             Caused by: java.lang.IllegalStateException: the build cannot start";
        observe(
            "window/showMessage",
            &json!({"type": 1, "message": message}),
            &failures,
        );
        let failure = watch.borrow().clone().unwrap();
        assert_eq!(failure.summary, "the build cannot start");
    }

    #[test]
    fn another_method_is_ignored() {
        let (failures, watch) = watch::channel(None);
        observe(
            "window/logMessage",
            &json!({"type": 1, "message": "Build file 'C:\\p\\build.gradle' line: 2"}),
            &failures,
        );
        assert!(watch.borrow().is_none());
    }
}

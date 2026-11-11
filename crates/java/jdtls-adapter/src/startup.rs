//! Checks before a start of the server and the reason a start ended.
//!
//! The bundles of the server need a JDK new enough for them, which the `release` file
//! of the JDK names. A start that ends writes its cause into the log of the
//! configuration folder, one file per run.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use crate::{JDTLS_VERSION, MIN_JAVA_VERSION};

/// The major version in the `release` file at the root of a JDK, 8 for `1.8.0_492`.
pub fn java_version(home: &Path) -> Option<u32> {
    let release = fs::read_to_string(home.join("release")).ok()?;
    let version = release
        .lines()
        .find_map(|line| line.strip_prefix("JAVA_VERSION="))?
        .trim()
        .trim_matches('"');
    major(version)
}

fn major(version: &str) -> Option<u32> {
    let mut parts = version.split(['.', '-', '_', '+']);
    let first: u32 = parts.next()?.parse().ok()?;
    if first == 1 {
        parts.next()?.parse().ok()
    } else {
        Some(first)
    }
}

/// Why the server cannot run on this JDK, when its `release` file says it is too old.
/// A JDK without a readable `release` is not refused here: it fails with its own log.
pub fn version_problem(home: &Path) -> Option<String> {
    let found = java_version(home)?;
    (found < MIN_JAVA_VERSION).then(|| {
        format!(
            "jdtls {JDTLS_VERSION} needs Java {MIN_JAVA_VERSION} or newer, found Java {found} at {}",
            home.display()
        )
    })
}

/// The capability line that names the missing Java version.
const CAPABILITY: &str = "Unresolved requirement: Require-Capability: osgi.ee";

/// The cause of a failed start from the newest log of `config_dir` written after
/// `since`. The bundle that cannot resolve names the missing capability; another
/// message marked as an error is the next choice.
pub fn crash_reason(config_dir: &Path, since: Option<SystemTime>) -> Option<String> {
    let log = newest_log(config_dir, since)?;
    let text = fs::read_to_string(log).ok()?;
    reason_in(&text)
}

/// The newest `.log` of the folder, when it was written after `since`.
fn newest_log(dir: &Path, since: Option<SystemTime>) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .path()
                .extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("log"))
        })
        .filter(|entry| match since {
            Some(since) => entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .is_ok_and(|written| written >= since),
            None => true,
        })
        .max_by_key(|entry| {
            entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
        })
        .map(|entry| entry.path())
}

/// The text of a log line without the marker the log puts in front of a message.
fn message(line: &str) -> String {
    line.trim()
        .strip_prefix("!MESSAGE ")
        .unwrap_or(line.trim())
        .trim()
        .to_owned()
}

fn reason_in(text: &str) -> Option<String> {
    if let Some(line) = text.lines().find(|line| line.contains(CAPABILITY)) {
        return Some(message(line));
    }
    text.lines()
        .filter(|line| line.trim_start().starts_with("!MESSAGE "))
        .map(message)
        .find(|line| is_error(line))
}

fn is_error(message: &str) -> bool {
    let message = message.to_lowercase();
    message.contains("error")
        || message.contains("exception")
        || message.contains("could not")
        || message.contains("failed")
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{crash_reason, java_version, version_problem};

    fn jdk(name: &str, release: &str) -> std::path::PathBuf {
        let home =
            std::env::temp_dir().join(format!("jdtls-startup-{}-{name}", std::process::id()));
        fs::create_dir_all(&home).unwrap();
        fs::write(home.join("release"), release).unwrap();
        home
    }

    #[test]
    fn a_release_file_gives_the_major_version() {
        assert_eq!(
            java_version(&jdk("new", "JAVA_VERSION=\"25.0.3\"\n")),
            Some(25)
        );
        assert_eq!(
            java_version(&jdk("old", "JAVA_VERSION=\"1.8.0_492\"\n")),
            Some(8)
        );
        assert_eq!(
            java_version(&jdk("bare", "JAVA_VERSION=\"21\"\n")),
            Some(21)
        );
        assert_eq!(java_version(&std::path::PathBuf::from(r"Z:\nowhere")), None);
    }

    #[test]
    fn an_old_jdk_says_what_the_server_needs_and_where_it_is() {
        let home = jdk("refused", "JAVA_VERSION=\"21.0.5\"\n");
        let problem = version_problem(&home).unwrap();
        assert!(problem.contains("needs Java 25 or newer"), "{problem}");
        assert!(problem.contains("found Java 21 at"), "{problem}");
        let path = home.to_string_lossy().into_owned();
        assert!(problem.contains(&path), "{problem}");
        assert!(version_problem(&jdk("newer", "JAVA_VERSION=\"25.0.1\"\n")).is_none());
    }

    #[test]
    fn the_first_capability_line_of_the_log_is_the_cause() {
        let dir = std::env::temp_dir().join(format!("jdtls-log-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let log = dir.join("1678886400000.log");
        fs::write(
            &log,
            "!ENTRY org.eclipse.osgi 4 0 2026-11-10 10:00:00.000\n\
             !MESSAGE Error\n\
             !SUBENTRY 1 org.eclipse.osgi 4 0 2026-11-10 10:00:00.000\n\
             !MESSAGE Could not resolve module: org.eclipse.core.filesystem\n\
             !SUBENTRY 2 org.eclipse.core.filesystem 4 0 2026-11-10 10:00:00.000\n\
             !MESSAGE Unresolved requirement: Require-Capability: osgi.ee; \
             filter:=\"(&(osgi.ee=JavaSE)(version=25))\"\n",
        )
        .unwrap();
        let reason = crash_reason(&dir, None).unwrap();
        assert_eq!(
            reason,
            "Unresolved requirement: Require-Capability: osgi.ee; \
             filter:=\"(&(osgi.ee=JavaSE)(version=25))\""
        );
    }

    #[test]
    fn a_log_without_the_capability_gives_the_first_error_message() {
        let dir = std::env::temp_dir().join(format!("jdtls-log-error-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("1.log"),
            "!ENTRY org.eclipse.osgi 4 0 2026-11-10 10:00:00.000\n\
             !MESSAGE A warning only\n\
             !MESSAGE Application error: the launcher cannot find the jar\n",
        )
        .unwrap();
        assert_eq!(
            crash_reason(&dir, None).as_deref(),
            Some("Application error: the launcher cannot find the jar")
        );
    }

    #[test]
    fn a_log_written_before_the_start_is_not_the_cause() {
        let dir = std::env::temp_dir().join(format!("jdtls-log-old-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        fs::write(
            dir.join("1.log"),
            "!MESSAGE Unresolved requirement: Require-Capability: osgi.ee; filter:=\"x\"\n",
        )
        .unwrap();
        let later = fs::metadata(dir.join("1.log")).unwrap().modified().unwrap()
            + std::time::Duration::from_secs(60);
        assert_eq!(crash_reason(&dir, Some(later)), None);
    }
}

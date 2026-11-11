//! The inputs of a build and the stamp of the last successful one. Maven and Gradle
//! share it: a run of the tool is only needed when the sources or the build files
//! changed since the outputs were made.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use base::Result;
use proc_sup::ProcessSpec;

/// Files whose change makes the previous outputs stale.
const BUILD_FILES: &[&str] = &[
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "settings.gradle",
    "settings.gradle.kts",
    "gradle.properties",
    "mvnw",
    "mvnw.cmd",
    "gradlew",
    "gradlew.bat",
];
/// Folders that only hold the outputs of a build.
const OUTPUT_FOLDERS: &[&str] = &["target", "build", "out", ".gradle", ".git"];
/// Source-like files outside `src`, a build script for one.
const SOURCE_EXTENSIONS: &[&str] = &["java", "kt", "kts", "groovy", "gradle", "xml", "properties"];

/// The builds that started and have not reported success yet, so a stamp can be
/// written for the inputs that were used.
pub(crate) struct BuildStamp {
    pending: Mutex<Option<(PathBuf, u64)>>,
}

impl BuildStamp {
    pub(crate) fn new() -> Self {
        Self {
            pending: Mutex::new(None),
        }
    }

    /// `None` when a successful build already made the outputs current. Otherwise the
    /// command of `assemble`, with the fingerprint of the inputs kept for [`built`].
    pub(crate) fn needed(
        &self,
        root: &Path,
        stamp: &Path,
        has_outputs: bool,
        assemble: impl FnOnce() -> Result<ProcessSpec>,
    ) -> Result<Option<ProcessSpec>> {
        let fingerprint = fingerprint(root)?;
        if has_outputs && read_stamp(stamp) == Some(fingerprint) {
            *self
                .pending
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = None;
            return Ok(None);
        }
        *self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = Some((root.to_path_buf(), fingerprint));
        assemble().map(Some)
    }

    /// A build of `root` finished with success: its inputs become the stamp.
    pub(crate) fn built(&self, root: &Path, stamp: &Path) {
        let pending = self
            .pending
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .take();
        let fingerprint = match pending {
            Some((path, fingerprint)) if path == root => Some(fingerprint),
            _ => fingerprint(root).ok(),
        };
        if let Some(fingerprint) = fingerprint {
            write_stamp(stamp, fingerprint);
        }
    }
}

/// A cheap fingerprint of everything a build reads: the build scripts and the sources.
/// Outputs and version control data are left out, so a build does not invalidate itself.
pub(crate) fn fingerprint(root: &Path) -> Result<u64> {
    let mut files = Vec::new();
    collect_inputs(root, root, &mut files)?;
    files.sort();
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    let mut add = |value: &[u8]| {
        for byte in value {
            hash = (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    for path in files {
        let relative = path.strip_prefix(root).unwrap_or(&path);
        add(relative.to_string_lossy().as_bytes());
        if let Ok(metadata) = fs::metadata(&path) {
            add(&metadata.len().to_le_bytes());
            let modified = metadata
                .modified()
                .ok()
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or(0, |time| time.as_nanos());
            add(&modified.to_le_bytes());
        }
    }
    Ok(hash)
}

fn collect_inputs(root: &Path, folder: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    for entry in fs::read_dir(folder)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if entry.file_type()?.is_dir() {
            if !OUTPUT_FOLDERS.contains(&name.as_ref()) {
                collect_inputs(root, &path, files)?;
            }
            continue;
        }
        let relative = path.strip_prefix(root).unwrap_or(&path);
        let in_sources = relative.components().any(|part| {
            part.as_os_str() == "src" || part.as_os_str() == ".mvn" || part.as_os_str() == "gradle"
        });
        let extension = path
            .extension()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        let is_script =
            BUILD_FILES.contains(&name.as_ref()) || SOURCE_EXTENSIONS.contains(&extension.as_str());
        if in_sources || is_script {
            files.push(path);
        }
    }
    Ok(())
}

/// Whether there are compiled classes to run: a `classes` folder with a class file.
pub(crate) fn has_classes(root: &Path) -> bool {
    fn dirs(folder: &Path) -> bool {
        let Ok(entries) = fs::read_dir(folder) else {
            return false;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|kind| kind.is_dir()) {
                if path.file_name().is_some_and(|name| name == "classes") && has_class(&path) {
                    return true;
                }
                if dirs(&path) {
                    return true;
                }
            }
        }
        false
    }
    dirs(root)
}

fn has_class(folder: &Path) -> bool {
    let Ok(entries) = fs::read_dir(folder) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let path = entry.path();
        class_file(&path)
    })
}

fn class_file(path: &Path) -> bool {
    if path.is_dir() {
        return has_class(path);
    }
    path.extension().is_some_and(|ext| ext == "class")
}

pub(crate) fn read_stamp(file: &Path) -> Option<u64> {
    fs::read_to_string(file).ok()?.trim().parse().ok()
}

fn write_stamp(file: &Path, fingerprint: u64) {
    if let Some(parent) = file.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(file, fingerprint.to_string());
}

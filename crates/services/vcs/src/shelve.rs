//! Shelf patches: the chosen files as a patch against `HEAD`, and putting a patch back.
//! The index of the repository is never touched: a temporary one holds the chosen files.

use std::collections::HashSet;
use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{Hunk, Repository, line_hunks};

/// The id git gives the tree of no files.
const EMPTY_TREE: &str = "4b825dc642cb6eb9a060e54bf8d69288fbee4904";

/// A file as `HEAD` has it and as the shelf patch makes it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShelfDiff {
    pub base: String,
    pub current: String,
    pub hunks: Vec<Hunk>,
}

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

impl Repository {
    /// The chosen files, new and deleted ones too, as a binary patch against `HEAD`.
    pub fn shelve(&self, paths: &[String]) -> io::Result<Vec<u8>> {
        let paths = self.shelve_paths(paths)?;
        let head = self.status()?.head.is_some();
        let index = temp_path("index");
        let _ = std::fs::remove_file(&index);
        let env = [("GIT_INDEX_FILE", index.as_os_str())];
        let base = if head {
            self.run_with_env(&["read-tree", "HEAD"], &env)?;
            "HEAD".to_string()
        } else {
            self.run_with_env(&["read-tree", "--empty"], &env)?;
            EMPTY_TREE.to_string()
        };
        let mut add = vec!["add", "-A", "--"];
        add.extend(paths.iter().map(String::as_str));
        self.run_with_env(&add, &env)?;
        let mut diff = vec!["diff", "--cached", "--binary", "--full-index", &base];
        if !paths.is_empty() {
            diff.push("--");
            diff.extend(paths.iter().map(String::as_str));
        }
        let patch = self.run_with_env(&diff, &env);
        let _ = std::fs::remove_file(&index);
        patch
    }

    /// Puts `patch` back into the working tree, only the named files when `paths` says so.
    /// The patch is checked first: a conflict leaves every file as it was.
    pub fn apply_patch(&self, patch: &[u8], paths: Option<&[String]>) -> io::Result<()> {
        let includes: Vec<String> = paths
            .unwrap_or_default()
            .iter()
            .map(|path| format!("--include={path}"))
            .collect();
        let mut args = vec!["apply", "--binary", "--whitespace=nowarn"];
        args.extend(includes.iter().map(String::as_str));
        let mut check = args.clone();
        check.push("--check");
        if let Err(error) = self.git().run_with_input(self.root(), &check, Some(patch)) {
            let files = patch_files(patch);
            if files.is_empty() {
                return Err(error);
            }
            return Err(io::Error::other(format!(
                "conflict in {}: {error}",
                files.join(", ")
            )));
        }
        self.git()
            .run_with_input(self.root(), &args, Some(patch))
            .map(drop)
    }

    /// The file as `HEAD` has it and as `patch` makes it, for the shelf preview and the
    /// diff window. `None` when the patch does not touch the file.
    pub fn shelf_diff(&self, patch: &[u8], path: &str) -> io::Result<Option<ShelfDiff>> {
        let mut blobs = self.blobs()?;
        let base = blobs.read(&format!("HEAD:{path}"))?.unwrap_or_default();
        let base = text_core::decode(&base).text;
        let Some(current) = patched_text(&base, patch, path) else {
            return Ok(None);
        };
        let hunks = line_hunks(&base, &current);
        Ok(Some(ShelfDiff {
            base,
            current,
            hunks,
        }))
    }

    /// The text of `path` after `patch`, when the last commit does not have the file and
    /// the patch adds it. Empty for a deleted file.
    pub fn shelf_text(&self, patch: &[u8], path: &str) -> io::Result<Option<String>> {
        let mut blobs = self.blobs()?;
        let base = blobs.read(&format!("HEAD:{path}"))?.unwrap_or_default();
        let base = text_core::decode(&base).text;
        Ok(patched_text(&base, patch, path))
    }

    /// The chosen files and the old name of a rename, so that the patch sees both sides.
    fn shelve_paths(&self, paths: &[String]) -> io::Result<Vec<String>> {
        let status = self.status()?;
        let mut all: Vec<String> = paths.to_vec();
        for file in &status.files {
            if paths.contains(&file.path)
                && let Some(original) = &file.original
            {
                all.push(original.clone());
            }
        }
        all.sort();
        all.dedup();
        Ok(all)
    }

    /// Takes the chosen files back to `HEAD`. A file the last commit does not have goes
    /// from the disk, because the shelf now holds it.
    pub fn reset_paths_to_head(&self, paths: &[String]) -> io::Result<()> {
        let status = self.status()?;
        let head = status.head.is_some();
        let paths = self.shelve_paths(paths)?;
        let mut removed = HashSet::new();
        for path in paths {
            let file = status.files.iter().find(|file| file.path == path);
            let added = file.is_some_and(|file| {
                file.index == crate::Change::Added || file.worktree == crate::Change::Untracked
            });
            if added {
                if head && file.is_some_and(|file| file.index == crate::Change::Added) {
                    self.git().run(self.root(), ["reset", "-q", "--", &path])?;
                }
                if removed.insert(path.clone()) {
                    let _ = std::fs::remove_file(self.root().join(&path));
                }
                continue;
            }
            if head {
                self.git()
                    .run(self.root(), ["checkout", "HEAD", "--", &path])?;
            }
        }
        Ok(())
    }

    fn run_with_env(&self, args: &[&str], env: &[(&str, &OsStr)]) -> io::Result<Vec<u8>> {
        let mut command = self.git().command(self.root());
        command.args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        crate::GitRunner::checked(command).map(|output| output.stdout)
    }
}

/// The text of `path` after the part of `patch` that touches it. The patch is a unified
/// diff against `base`, so the hunks are applied to the lines of `base`.
pub fn patched_text(base: &str, patch: &[u8], path: &str) -> Option<String> {
    let text = String::from_utf8_lossy(patch);
    let section = patch_section(&text, path)?;
    Some(apply_section(base, section))
}

/// The part of a patch that describes `path`, from its `diff --git` line to the next one.
fn patch_section<'a>(patch: &'a str, path: &str) -> Option<&'a str> {
    let mut start = None;
    for (at, _) in patch.match_indices("diff --git ") {
        if let Some(start) = start {
            return Some(&patch[start..at]);
        }
        let end = patch[at..]
            .find('\n')
            .map_or(patch.len(), |end| at + end + 1);
        let header = &patch[at..end];
        if header.contains(&format!("a/{path} ")) || header.contains(&format!("b/{path} ")) {
            start = Some(at);
        }
    }
    start.map(|start| &patch[start..])
}

/// Applies the hunks of one file section to `base`.
fn apply_section(base: &str, section: &str) -> String {
    let lines: Vec<&str> = base.split('\n').collect();
    let mut out: Vec<String> = Vec::new();
    let mut cursor = 0usize;
    let mut in_hunk = false;
    for line in section.lines() {
        if line.starts_with("@@") {
            let target = hunk_start(line).saturating_sub(1);
            while cursor < target && cursor < lines.len() {
                out.push(lines[cursor].to_string());
                cursor += 1;
            }
            in_hunk = true;
            continue;
        }
        if !in_hunk || line.starts_with('\\') {
            continue;
        }
        let Some(sign) = line.chars().next() else {
            continue;
        };
        let rest = &line[sign.len_utf8()..];
        match sign {
            ' ' => {
                if cursor < lines.len() {
                    out.push(lines[cursor].to_string());
                    cursor += 1;
                }
            }
            '-' => cursor += 1,
            '+' => out.push(rest.to_string()),
            _ => {}
        }
    }
    while cursor < lines.len() {
        out.push(lines[cursor].to_string());
        cursor += 1;
    }
    out.join("\n")
}

/// The first line of a hunk header, ones based: `@@ -12,3 +12,4 @@`.
fn hunk_start(header: &str) -> usize {
    header
        .strip_prefix("@@ -")
        .and_then(|rest| rest.split([',', ' ']).next())
        .and_then(|line| line.parse().ok())
        .unwrap_or(1)
}

/// The files a patch touches, in the order of the patch.
pub fn patch_files(patch: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(patch);
    let mut files = Vec::new();
    let mut take = |path: &str| {
        if path == "/dev/null" || path.is_empty() {
            return;
        }
        let path = path.to_owned();
        if !files.contains(&path) {
            files.push(path);
        }
    };
    for line in text.lines() {
        if let Some(path) = line.strip_prefix("--- a/") {
            take(path);
        } else if let Some(path) = line.strip_prefix("+++ b/") {
            take(path);
        }
    }
    files
}

/// A file name of the temporary folder that no other call of this process holds.
fn temp_path(kind: &str) -> PathBuf {
    let serial = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ide-shelf-{}-{serial}.{kind}", std::process::id()))
}

#[cfg(test)]
mod tests {
    use super::{patch_files, patched_text};

    #[test]
    fn the_files_of_a_patch_are_named_once() {
        let patch = b"diff --git a/src/a.txt b/src/a.txt\n--- a/src/a.txt\n+++ b/src/a.txt\n\
diff --git a/new.txt b/new.txt\n--- /dev/null\n+++ b/new.txt\n";
        assert_eq!(patch_files(patch), ["src/a.txt", "new.txt"]);
    }

    #[test]
    fn a_deleted_file_comes_from_the_old_side() {
        let patch = b"--- a/old.txt\n+++ /dev/null\n";
        assert_eq!(patch_files(patch), ["old.txt"]);
    }

    #[test]
    fn a_patch_makes_the_text_of_one_file_from_its_base() {
        let base = "one\ntwo\nthree\n";
        let patch = b"diff --git a/a.txt b/a.txt\n\
--- a/a.txt\n+++ b/a.txt\n\
@@ -1,3 +1,3 @@\n one\n-two\n+TWO\n three\n";
        assert_eq!(
            patched_text(base, patch, "a.txt").as_deref(),
            Some("one\nTWO\nthree\n")
        );
    }

    #[test]
    fn a_patch_of_a_new_file_starts_from_no_lines() {
        let patch = b"diff --git a/new.txt b/new.txt\n\
new file mode 100644\n--- /dev/null\n+++ b/new.txt\n\
@@ -0,0 +1,2 @@\n+one\n+two\n";
        assert_eq!(
            patched_text("", patch, "new.txt").as_deref(),
            Some("one\ntwo\n")
        );
        assert_eq!(patched_text("", patch, "other.txt"), None);
    }

    #[test]
    fn a_patch_of_a_deleted_file_empties_it() {
        let patch = b"diff --git a/old.txt b/old.txt\n\
deleted file mode 100644\n--- a/old.txt\n+++ /dev/null\n\
@@ -1,2 +0,0 @@\n-one\n-two\n";
        assert_eq!(
            patched_text("one\ntwo\n", patch, "old.txt").as_deref(),
            Some("")
        );
    }
}

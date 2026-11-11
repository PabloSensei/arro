//! A repository of its own beside the project: separate `--git-dir` and index, so the repository
//! of the user never sees a thing, and ignored files stay out because `.gitignore` still applies.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::git::GitRunner;

/// `core.bigFileThreshold=1` packs a whole `git add` into one file instead of one per object,
/// which takes the first snapshot of 15k files from 70 to 18 s (measured in S7).
const SETTINGS: &[(&str, &str)] = &[
    ("core.autocrlf", "false"),
    ("core.safecrlf", "false"),
    ("core.bigFileThreshold", "1"),
    ("core.preloadindex", "true"),
    ("core.fscache", "true"),
    ("core.untrackedCache", "true"),
    ("core.fsync", "none"),
    ("core.looseCompression", "1"),
    ("feature.manyFiles", "true"),
    ("gc.auto", "0"),
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    /// Relative to the project, with forward slashes.
    pub path: String,
    pub kind: ChangeKind,
}

pub struct HiddenRepo {
    git: GitRunner,
    git_dir: PathBuf,
    index: PathBuf,
    work_tree: PathBuf,
}

impl HiddenRepo {
    /// Creates the repository in `state_dir` on first use.
    pub fn open(git: GitRunner, state_dir: &Path, work_tree: &Path) -> io::Result<Self> {
        let git_dir = state_dir.join("git");
        let repo = Self {
            git,
            index: state_dir.join("index"),
            git_dir,
            work_tree: work_tree.to_path_buf(),
        };
        if !repo.git_dir.join("HEAD").is_file() {
            fs::create_dir_all(state_dir)?;
            repo.init()?;
        }
        Ok(repo)
    }

    fn init(&self) -> io::Result<()> {
        let mut command = self.git.command(&self.work_tree);
        command.args(["init", "--bare", "-q"]).arg(&self.git_dir);
        Self::isolate(&mut command);
        GitRunner::checked(command)?;
        self.run(&["config", "core.bare", "false"])?;
        for (key, value) in SETTINGS {
            self.run(&["config", key, value])?;
        }
        Ok(())
    }

    /// The settings of the user must not change what a snapshot contains.
    fn isolate(command: &mut Command) {
        command
            .env("GIT_CONFIG_GLOBAL", "NUL")
            .env("GIT_CONFIG_NOSYSTEM", "1");
    }

    fn run(&self, arguments: &[&str]) -> io::Result<String> {
        let mut command = self.git.command(&self.work_tree);
        Self::isolate(&mut command);
        command
            .arg(format!("--git-dir={}", self.git_dir.display()))
            .arg(format!("--work-tree={}", self.work_tree.display()))
            .env("GIT_INDEX_FILE", &self.index)
            .args(arguments);
        let output = GitRunner::checked(command)?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Takes the working tree without ignored files and returns the id of the tree.
    pub fn snapshot(&self) -> io::Result<String> {
        self.run(&["add", "-A", "."])?;
        Ok(self.run(&["write-tree"])?.trim().to_string())
    }

    pub fn changed(&self, from: &str, to: &str) -> io::Result<Vec<ChangedFile>> {
        let arguments = [
            "diff-tree",
            "-r",
            "--no-renames",
            "--name-status",
            "-z",
            from,
            to,
        ];
        Ok(parse_changes(&self.run(&arguments)?))
    }
}

/// `-z` output: a status and a path, each ended by NUL.
fn parse_changes(output: &str) -> Vec<ChangedFile> {
    let mut changes = Vec::new();
    let mut fields = output.split('\0');
    while let (Some(status), Some(path)) = (fields.next(), fields.next()) {
        let kind = match status {
            "A" => ChangeKind::Added,
            "D" => ChangeKind::Deleted,
            _ => ChangeKind::Modified,
        };
        changes.push(ChangedFile {
            path: path.to_string(),
            kind,
        });
    }
    changes
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn name_status_output_is_parsed() {
        let changes = parse_changes("M\0src/a.txt\0A\0с пробелом.txt\0D\0gone\0");

        assert_eq!(changes.len(), 3);
        assert_eq!(changes[0].kind, ChangeKind::Modified);
        assert_eq!(changes[1].path, "с пробелом.txt");
        assert_eq!(changes[1].kind, ChangeKind::Added);
        assert_eq!(changes[2].kind, ChangeKind::Deleted);
    }

    #[test]
    fn no_changes_mean_an_empty_list() {
        assert!(parse_changes("").is_empty());
    }
}

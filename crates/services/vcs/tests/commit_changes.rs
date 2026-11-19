//! The files a commit changed with the change of each, on real repositories in a scratch
//! folder: an added file is added, a deleted one deleted, an edited one modified, and a
//! moved one is a single entry under its new name.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use vcs::{Change, GitRunner, Repository};

fn git() -> GitRunner {
    GitRunner::from_env().expect("git in PATH")
}

fn plain_git(cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn plain_ok(cwd: &Path, args: &[&str]) -> String {
    let output = plain_git(cwd, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// A repository with an identity of its own, so the git configuration of the machine is
/// neither read nor written.
fn init(directory: &Path) {
    plain_ok(directory, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.invalid"),
        ("core.autocrlf", "false"),
        ("commit.gpgsign", "false"),
    ] {
        plain_ok(directory, &["config", key, value]);
    }
}

/// Stages every change, commits it and returns the id of the commit.
fn commit_all(directory: &Path, message: &str) -> String {
    plain_ok(directory, &["add", "-A"]);
    plain_ok(directory, &["commit", "-q", "-m", message]);
    plain_ok(directory, &["rev-parse", "HEAD"])
        .trim()
        .to_owned()
}

/// The changes of a commit against its first parent, sorted by path.
fn changes_of(directory: &Path, commit: &str) -> Vec<(String, Change)> {
    let repo = Repository::open(git(), directory).unwrap();
    let mut changes: Vec<(String, Change)> = repo
        .commit_changes(commit)
        .unwrap()
        .into_iter()
        .map(|change| (change.path, change.change))
        .collect();
    changes.sort_by(|left, right| left.0.cmp(&right.0));
    changes
}

#[test]
fn commit_changes_lists_each_file_with_the_change_it_made() {
    let directory = test_support::scratch("vcs-commit-changes-kinds");
    init(&directory);
    fs::write(directory.join("keep.txt"), "one\n").unwrap();
    fs::write(directory.join("gone.txt"), "gone\n").unwrap();
    fs::write(directory.join("old.txt"), "moved\n").unwrap();
    let first = commit_all(&directory, "base");

    fs::write(directory.join("keep.txt"), "two\n").unwrap();
    fs::remove_file(directory.join("gone.txt")).unwrap();
    plain_ok(&directory, &["mv", "old.txt", "new.txt"]);
    fs::write(directory.join("added.txt"), "added\n").unwrap();
    let second = commit_all(&directory, "change");

    assert_eq!(
        changes_of(&directory, &second),
        [
            ("added.txt".to_owned(), Change::Added),
            ("gone.txt".to_owned(), Change::Deleted),
            ("keep.txt".to_owned(), Change::Modified),
            ("new.txt".to_owned(), Change::Renamed),
        ]
    );
    assert_eq!(
        changes_of(&directory, &first),
        [
            ("gone.txt".to_owned(), Change::Added),
            ("keep.txt".to_owned(), Change::Added),
            ("old.txt".to_owned(), Change::Added),
        ]
    );
}

#[test]
fn commit_changes_of_a_root_commit_has_the_paths_of_commit_files() {
    let directory = test_support::scratch("vcs-commit-changes-root");
    init(&directory);
    fs::create_dir_all(directory.join("src")).unwrap();
    fs::write(directory.join("src/Main.rs"), "fn main() {}\n").unwrap();
    fs::write(directory.join("README.md"), "read me\n").unwrap();
    let root = commit_all(&directory, "root");

    let repo = Repository::open(git(), &directory).unwrap();
    let mut paths: Vec<String> = repo
        .commit_changes(&root)
        .unwrap()
        .into_iter()
        .map(|change| change.path)
        .collect();
    paths.sort();
    let mut files = repo.commit_files(&root).unwrap();
    files.sort();
    assert_eq!(paths, files);
    assert_eq!(paths, ["README.md", "src/Main.rs"]);
}

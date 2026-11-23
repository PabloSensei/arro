//! The whole message of the last commit, which Amend puts in the Commit window: the subject,
//! the blank line and the body come back as git holds them, and a branch with no commit
//! has none.

use std::fs;
use std::path::Path;
use std::process::Output;

use vcs::{GitRunner, Repository};

fn git() -> GitRunner {
    GitRunner::from_env().expect("git in PATH")
}

fn plain_git(cwd: &Path, args: &[&str]) -> Output {
    std::process::Command::new("git")
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

#[test]
fn last_commit_message_gives_the_whole_message_of_the_last_commit() {
    let directory = test_support::scratch("vcs-last-message");
    init(&directory);
    fs::write(directory.join("main.txt"), "one\n").unwrap();
    plain_ok(&directory, &["add", "-A"]);
    // The message comes from a file outside the repository: subject, blank line, body.
    let message = test_support::scratch("vcs-last-message-text").join("message.txt");
    fs::write(
        &message,
        "Fix the parser\n\nKeep the old rules.\nAdd a test for them.\n",
    )
    .unwrap();
    plain_ok(
        &directory,
        &["commit", "-q", "-F", message.to_str().unwrap()],
    );

    let repo = Repository::open(git(), &directory).unwrap();
    assert_eq!(
        repo.last_commit_message().unwrap().as_deref(),
        Some("Fix the parser\n\nKeep the old rules.\nAdd a test for them."),
        "the message comes back whole, without the line ends after it"
    );
}

#[test]
fn last_commit_message_is_none_on_a_branch_with_no_commit() {
    let directory = test_support::scratch("vcs-last-message-empty");
    init(&directory);

    let repo = Repository::open(git(), &directory).unwrap();
    assert_eq!(repo.last_commit_message().unwrap(), None);
}

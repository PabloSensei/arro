//! The details of one commit on a real repository: its whole message, the person who
//! committed it when that person differs from the author, and every branch whose history
//! contains it. The identity of the commit lives in its repository, so the git
//! configuration of the machine is neither read nor written.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use vcs::{GitRunner, Repository};

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
fn the_details_keep_the_body_the_committer_and_both_branches() {
    let directory = test_support::scratch("vcs-commit-details");
    init(&directory);
    fs::write(directory.join("a.txt"), "one\n").unwrap();
    plain_ok(&directory, &["add", "a.txt"]);
    // The author and the committer are different people, as after an amend or a rebase.
    let output = Command::new("git")
        .current_dir(&directory)
        .args(["commit", "-q", "-m", "subject line\n\nbody line"])
        .env("GIT_AUTHOR_NAME", "Author")
        .env("GIT_AUTHOR_EMAIL", "author@example.invalid")
        .env("GIT_AUTHOR_DATE", "2023-05-06 07:08:09 +0000")
        .env("GIT_COMMITTER_NAME", "Committer")
        .env("GIT_COMMITTER_EMAIL", "committer@example.invalid")
        .env("GIT_COMMITTER_DATE", "2024-01-02 03:04:05 +0000")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let head = plain_ok(&directory, &["rev-parse", "HEAD"])
        .trim()
        .to_owned();
    plain_ok(&directory, &["branch", "feature"]);

    let repo = Repository::open(git(), &directory).unwrap();
    let details = repo.commit_details(&head).unwrap();

    assert_eq!(details.message, "subject line\n\nbody line");
    assert_eq!(details.committer, "Committer");
    assert_eq!(details.committer_email, "committer@example.invalid");
    assert_eq!(details.committer_time, 1_704_164_645);
    assert_eq!(details.branches, ["feature", "main"]);
}

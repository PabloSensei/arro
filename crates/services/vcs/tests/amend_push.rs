//! Push after an Amend of a commit the upstream has: the lease push replaces the remote commit
//! only while the remote branch is where this repository last saw it, and the check before
//! the amend tells a pushed commit from one the remote does not have.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;

use vcs::{CommitOptions, GitRunner, Repository};

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

/// What `what` names in the repository, as git writes it.
fn rev(directory: &Path, what: &str) -> String {
    plain_ok(directory, &["rev-parse", what]).trim().to_owned()
}

fn commit_file(directory: &Path, name: &str, text: &str, message: &str) {
    fs::write(directory.join(name), text).unwrap();
    plain_ok(directory, &["add", "-A"]);
    plain_ok(directory, &["commit", "-q", "-m", message]);
}

/// A repository whose one commit is pushed to a bare remote, which its `main` tracks. The
/// bare remote and the repository are returned.
fn repository_with_remote(name: &str) -> (PathBuf, PathBuf) {
    let bare = test_support::scratch(&format!("{name}-remote"));
    plain_ok(&bare, &["init", "-q", "--bare"]);
    // A clone of the remote checks out `main` whatever the default branch of the machine is.
    plain_ok(&bare, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    let directory = test_support::scratch(name);
    init(&directory);
    plain_ok(
        &directory,
        &["remote", "add", "origin", bare.to_str().unwrap()],
    );
    commit_file(&directory, "main.txt", "one\n", "base");
    plain_ok(&directory, &["push", "-q", "-u", "origin", "HEAD"]);
    (directory, bare)
}

#[test]
fn head_on_upstream_tells_a_pushed_commit_from_one_the_remote_does_not_have() {
    let (directory, _bare) = repository_with_remote("amend-push-flags");
    let repo = Repository::open(git(), &directory).unwrap();
    assert!(
        repo.head_on_upstream().unwrap(),
        "HEAD equal to the upstream is on it"
    );

    commit_file(&directory, "second.txt", "two\n", "second");
    assert!(
        !repo.head_on_upstream().unwrap(),
        "a commit the upstream does not have is not on it"
    );

    plain_ok(&directory, &["checkout", "-q", "-b", "feature"]);
    assert!(
        !repo.head_on_upstream().unwrap(),
        "a branch with no upstream has nothing on a remote through it"
    );
}

#[test]
fn push_with_lease_sends_the_amended_commit_in_place_of_the_pushed_one() {
    let (directory, bare) = repository_with_remote("amend-push-lease");
    let pushed = rev(&directory, "HEAD");
    let repo = Repository::open(git(), &directory).unwrap();
    assert!(repo.head_on_upstream().unwrap());

    repo.amend_message("base reworded", &CommitOptions::default())
        .unwrap();
    repo.push_with_lease().unwrap();

    let amended = rev(&directory, "HEAD");
    assert_ne!(amended, pushed, "the amend made a new commit");
    assert_eq!(
        rev(&bare, "refs/heads/main"),
        amended,
        "the remote branch takes the amended commit"
    );
}

#[test]
fn push_with_lease_is_refused_when_the_remote_moved_since_the_last_fetch() {
    let (directory, bare) = repository_with_remote("amend-push-refused");
    // Another clone pushes a commit this repository has not fetched.
    let other = test_support::scratch("amend-push-refused-other");
    plain_ok(&other, &["clone", "-q", bare.to_str().unwrap(), "."]);
    for (key, value) in [
        ("user.name", "Other"),
        ("user.email", "other@example.invalid"),
    ] {
        plain_ok(&other, &["config", key, value]);
    }
    commit_file(&other, "other.txt", "other\n", "other work");
    plain_ok(&other, &["push", "-q"]);
    let remote = rev(&bare, "refs/heads/main");

    let repo = Repository::open(git(), &directory).unwrap();
    repo.amend_message("base reworded", &CommitOptions::default())
        .unwrap();
    let error = repo
        .push_with_lease()
        .expect_err("the lease refuses a remote branch this repository has not seen");
    assert!(
        error.to_string().contains("rejected"),
        "the git text is in the error: {error}"
    );
    assert_eq!(
        rev(&bare, "refs/heads/main"),
        remote,
        "the remote keeps the commit of the other clone"
    );
}

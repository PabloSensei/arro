//! An Amend without files on a real repository: the last commit gets the new message and
//! keeps its tree, and a staged file and an unstaged edit stay where they were.

use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use vcs::{CommitOptions, GitRunner, Repository};

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

/// What `what` names in the repository, as git writes it.
fn rev(directory: &Path, what: &str) -> String {
    plain_ok(directory, &["rev-parse", what]).trim().to_owned()
}

#[test]
fn amend_message_rewrites_the_message_and_keeps_the_tree_and_the_index() {
    let directory = test_support::scratch("vcs-amend-message");
    init(&directory);
    fs::write(directory.join("main.txt"), "one\n").unwrap();
    plain_ok(&directory, &["add", "-A"]);
    plain_ok(&directory, &["commit", "-q", "-m", "base"]);
    let before = rev(&directory, "HEAD");
    let tree = rev(&directory, "HEAD^{tree}");

    // A staged new file and an unstaged edit: neither goes into the amended commit.
    fs::write(directory.join("staged.txt"), "staged\n").unwrap();
    plain_ok(&directory, &["add", "staged.txt"]);
    fs::write(directory.join("main.txt"), "two\n").unwrap();

    let repo = Repository::open(git(), &directory).unwrap();
    let id = repo
        .amend_message("reworded", &CommitOptions::default())
        .unwrap();

    assert_eq!(
        id,
        rev(&directory, "HEAD"),
        "the returned id is the new HEAD"
    );
    assert_ne!(id, before, "the last commit is a new one");
    assert_eq!(plain_ok(&directory, &["log", "--format=%s"]), "reworded\n");
    assert_eq!(
        plain_ok(&directory, &["rev-list", "--count", "HEAD"]),
        "1\n",
        "the last commit is rewritten, none is added"
    );
    assert_eq!(
        rev(&directory, "HEAD^{tree}"),
        tree,
        "the tree of the commit stays"
    );
    assert_eq!(
        plain_ok(&directory, &["ls-tree", "-r", "--name-only", "HEAD"]),
        "main.txt\n",
        "the staged file stays out of the commit"
    );
    assert_eq!(
        fs::read_to_string(directory.join("main.txt")).unwrap(),
        "two\n"
    );
    let status = plain_ok(&directory, &["status", "--porcelain"]);
    assert!(
        status.contains("A  staged.txt"),
        "the staged file stays staged: {status}"
    );
    assert!(
        status.contains(" M main.txt"),
        "the unstaged edit stays unstaged: {status}"
    );
}

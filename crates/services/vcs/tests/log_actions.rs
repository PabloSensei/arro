//! The actions of the Git Log on real repositories in a temporary directory: checkout of
//! a revision, a branch or a tag at a commit, cherry-pick and revert with their conflicts,
//! and the ways of resetting the branch. The identity of a commit lives in its repository,
//! so the git configuration of the machine is neither read nor written.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vcs::{Change, CommitRequest, GitRunner, Operation, RefKind, Repository, ResetMode};

fn git() -> GitRunner {
    GitRunner::from_env().expect("git in PATH")
}

fn temp_dir(name: &str) -> PathBuf {
    test_support::scratch(&format!("vcs-log-{name}"))
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

fn open(directory: &Path) -> Repository {
    Repository::open(git(), directory).unwrap()
}

fn commit(repo: &Repository, message: &str) -> String {
    repo.commit(&CommitRequest {
        message: message.into(),
        paths: Vec::new(),
        amend: false,
    })
    .unwrap()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

/// A repository with two commits: `one` in `a.txt`, then `two`.
fn two_commits(name: &str) -> (PathBuf, Repository, String) {
    let root = temp_dir(name);
    init(&root);
    fs::write(root.join("a.txt"), "one\n").unwrap();
    let repo = open(&root);
    commit(&repo, "base");
    let base = repo.log(1, 0).unwrap()[0].id.clone();
    fs::write(root.join("a.txt"), "two\n").unwrap();
    commit(&repo, "second");
    (root, repo, base)
}

#[test]
fn checkout_of_a_commit_leaves_the_head_detached() {
    let (root, repo, base) = two_commits("checkout");
    repo.checkout_commit(&base).unwrap();

    let status = repo.status().unwrap();
    assert_eq!(status.head.as_deref(), Some(base.as_str()));
    assert_eq!(status.branch, None, "the head is detached");
    assert_eq!(read(&root.join("a.txt")), "one\n");
    assert_eq!(
        plain_ok(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).trim(),
        "HEAD"
    );
}

#[test]
fn a_cherry_pick_replays_the_commit_on_the_current_branch() {
    let root = temp_dir("cherry-pick");
    init(&root);
    fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let repo = open(&root);
    commit(&repo, "base");
    fs::write(root.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    commit(&repo, "change");
    let change = repo.log(1, 0).unwrap()[0].id.clone();
    fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    commit(&repo, "restore");

    let report = repo.cherry_pick(std::slice::from_ref(&change)).unwrap();
    assert_eq!(report.operation, Operation::None);
    assert!(report.conflicts.is_empty());
    assert_eq!(read(&root.join("a.txt")), "one\nTWO\nthree\n");
    assert_eq!(repo.log(1, 0).unwrap()[0].subject, "change");
}

#[test]
fn a_conflicting_cherry_pick_is_aborted() {
    let root = temp_dir("cherry-pick-conflict");
    init(&root);
    fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let repo = open(&root);
    commit(&repo, "base");
    fs::write(root.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    commit(&repo, "change");
    let change = repo.log(1, 0).unwrap()[0].id.clone();
    fs::write(root.join("a.txt"), "one\n2\nthree\n").unwrap();
    commit(&repo, "other");
    let head = repo.log(1, 0).unwrap()[0].id.clone();

    let report = repo.cherry_pick(std::slice::from_ref(&change)).unwrap();
    assert_eq!(report.operation, Operation::CherryPick);
    assert_eq!(report.conflicts, ["a.txt"]);
    assert!(read(&root.join("a.txt")).contains("<<<<<<<"));

    repo.abort().unwrap();
    assert_eq!(repo.operation().unwrap(), Operation::None);
    assert!(repo.conflicts().unwrap().is_empty());
    assert_eq!(repo.log(1, 0).unwrap()[0].id, head);
    assert_eq!(read(&root.join("a.txt")), "one\n2\nthree\n");
}

#[test]
fn a_revert_adds_the_opposite_commit() {
    let root = temp_dir("revert");
    init(&root);
    fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    let repo = open(&root);
    commit(&repo, "base");
    fs::write(root.join("a.txt"), "one\nTWO\n").unwrap();
    commit(&repo, "change");
    let change = repo.log(1, 0).unwrap()[0].id.clone();

    let report = repo.revert_commit(&change).unwrap();
    assert_eq!(report.operation, Operation::None);
    assert!(report.conflicts.is_empty());
    assert_eq!(read(&root.join("a.txt")), "one\ntwo\n");
    let log = repo.log(2, 0).unwrap();
    assert!(log[0].subject.starts_with("Revert"), "{}", log[0].subject);
    assert_eq!(log[0].parents, [change]);
}

#[test]
fn a_conflicting_revert_is_aborted() {
    let root = temp_dir("revert-conflict");
    init(&root);
    fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let repo = open(&root);
    commit(&repo, "base");
    fs::write(root.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    commit(&repo, "change");
    let change = repo.log(1, 0).unwrap()[0].id.clone();
    fs::write(root.join("a.txt"), "one\n2\nthree\n").unwrap();
    commit(&repo, "other");
    let head = repo.log(1, 0).unwrap()[0].id.clone();

    let report = repo.revert_commit(&change).unwrap();
    assert_eq!(report.operation, Operation::Revert);
    assert_eq!(report.conflicts, ["a.txt"]);

    repo.abort().unwrap();
    assert_eq!(repo.operation().unwrap(), Operation::None);
    assert_eq!(repo.log(1, 0).unwrap()[0].id, head);
    assert_eq!(read(&root.join("a.txt")), "one\n2\nthree\n");
}

#[test]
fn reset_soft_keeps_the_change_staged() {
    let (root, repo, base) = two_commits("reset-soft");
    repo.reset(&base, ResetMode::Soft).unwrap();

    let status = repo.status().unwrap();
    assert_eq!(status.head.as_deref(), Some(base.as_str()));
    assert_eq!(read(&root.join("a.txt")), "two\n", "the working tree keeps");
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].index, Change::Modified);
    assert_eq!(status.files[0].worktree, Change::Unmodified);
}

#[test]
fn reset_mixed_keeps_the_change_unstaged() {
    let (root, repo, base) = two_commits("reset-mixed");
    repo.reset(&base, ResetMode::Mixed).unwrap();

    let status = repo.status().unwrap();
    assert_eq!(status.head.as_deref(), Some(base.as_str()));
    assert_eq!(read(&root.join("a.txt")), "two\n");
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].index, Change::Unmodified);
    assert_eq!(status.files[0].worktree, Change::Modified);
}

#[test]
fn reset_hard_throws_the_changes_away() {
    let (root, repo, base) = two_commits("reset-hard");
    repo.reset(&base, ResetMode::Hard).unwrap();

    let status = repo.status().unwrap();
    assert_eq!(status.head.as_deref(), Some(base.as_str()));
    assert!(status.files.is_empty(), "the working tree is clean");
    assert_eq!(read(&root.join("a.txt")), "one\n");
}

#[test]
fn reset_keep_leaves_other_files_alone() {
    let (root, repo, base) = two_commits("reset-keep");
    fs::write(root.join("b.txt"), "keep\n").unwrap();
    repo.reset(&base, ResetMode::Keep).unwrap();

    assert_eq!(read(&root.join("a.txt")), "one\n");
    assert_eq!(read(&root.join("b.txt")), "keep\n");
    assert_eq!(repo.status().unwrap().head.as_deref(), Some(base.as_str()));
}

#[test]
fn a_tag_points_at_the_chosen_commit() {
    let (_root, repo, base) = two_commits("tag");
    repo.create_tag("v1", &base).unwrap();

    let label = repo
        .labels()
        .unwrap()
        .into_iter()
        .find(|label| label.name == "v1")
        .expect("the tag is a label of the log");
    assert_eq!(label.kind, RefKind::Tag);
    assert_eq!(label.commit, base);
}

#[test]
fn a_branch_starts_at_the_chosen_commit_and_is_checked_out_when_asked() {
    let (root, repo, base) = two_commits("branch");
    repo.branch_from("feature", &base, false).unwrap();

    let branch = repo
        .branches()
        .unwrap()
        .into_iter()
        .find(|branch| branch.name == "feature")
        .expect("the branch is there");
    assert_eq!(branch.head, base);
    assert!(!branch.current, "the branch waits until it is checked out");
    assert_eq!(repo.status().unwrap().branch.as_deref(), Some("main"));

    repo.branch_from("at-base", &base, true).unwrap();
    let status = repo.status().unwrap();
    assert_eq!(status.branch.as_deref(), Some("at-base"));
    assert_eq!(status.head.as_deref(), Some(base.as_str()));
    assert_eq!(read(&root.join("a.txt")), "one\n");
}

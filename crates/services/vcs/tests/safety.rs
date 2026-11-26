//! Git commands that touch the files of the user, on real repositories in a temporary
//! directory: a side of a rebase or a merge conflict, Revert of a file of a commit, and the
//! part of a shelf that an Unshelve takes.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vcs::{ConflictSide, FileRevert, GitRunner, Operation, Repository, patch_without};

fn git() -> GitRunner {
    GitRunner::from_env().expect("git in PATH")
}

fn scratch(name: &str) -> PathBuf {
    test_support::scratch(&format!("vcs-safety-{name}"))
}

/// Plain git, the way a user in a terminal calls it. A caller reads the status itself.
fn plain(cwd: &Path, args: &[&str]) -> Output {
    Command::new("git")
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap()
}

fn plain_ok(cwd: &Path, args: &[&str]) -> String {
    let output = plain(cwd, args);
    assert!(
        output.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn new_repository(directory: &Path) -> Repository {
    plain_ok(directory, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Тест"),
        ("user.email", "test@example.invalid"),
        ("core.autocrlf", "false"),
        ("commit.gpgsign", "false"),
    ] {
        plain_ok(directory, &["config", key, value]);
    }
    Repository::open(git(), directory).unwrap()
}

/// Writes `text` into `path` and commits that file; returns the id of the commit.
fn commit_text(directory: &Path, path: &str, text: &str, message: &str) -> String {
    fs::write(directory.join(path), text).unwrap();
    plain_ok(directory, &["add", "--", path]);
    plain_ok(directory, &["commit", "-q", "-m", message]);
    plain_ok(directory, &["rev-parse", "HEAD"])
        .trim()
        .to_owned()
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap()
}

/// `main` and `feature` changed the same line of `f.txt` in two ways; `main` is checked out.
fn diverged(name: &str) -> (PathBuf, Repository) {
    let root = scratch(name);
    let repo = new_repository(&root);
    commit_text(&root, "f.txt", "one\ntwo\nthree\n", "base");
    plain_ok(&root, &["switch", "-q", "-c", "feature"]);
    commit_text(&root, "f.txt", "one\nmine\nthree\n", "the user's change");
    plain_ok(&root, &["switch", "-q", "main"]);
    commit_text(
        &root,
        "f.txt",
        "one\nupstream\nthree\n",
        "the upstream change",
    );
    (root, repo)
}

#[test]
fn safety_a_rebase_conflict_takes_the_user_or_the_upstream_side() {
    let cases = [
        (ConflictSide::Yours, "one\nmine\nthree\n"),
        (ConflictSide::Theirs, "one\nupstream\nthree\n"),
    ];
    for (number, (side, expected)) in cases.into_iter().enumerate() {
        let (root, repo) = diverged(&format!("rebase-side-{number}"));
        plain_ok(&root, &["switch", "-q", "feature"]);
        // The rebase stops on the conflict; git answers the failure with a non-zero code.
        let _ = plain(&root, &["rebase", "main"]);

        assert_eq!(repo.operation().unwrap(), Operation::Rebase);
        assert_eq!(repo.conflicts().unwrap(), ["f.txt"]);
        let mut blobs = repo.blobs().unwrap();
        let versions = repo.conflict_versions(&mut blobs, "f.txt").unwrap();
        assert_eq!(
            versions.yours.as_deref(),
            Some(&b"one\nmine\nthree\n"[..]),
            "Yours is the commit of the user in a rebase"
        );
        drop(blobs);

        repo.accept_side("f.txt", side).unwrap();

        assert_eq!(read(&root.join("f.txt")), expected);
        assert!(repo.conflicts().unwrap().is_empty());
    }
}

#[test]
fn safety_a_merge_conflict_keeps_ours_as_the_user_side() {
    let (root, repo) = diverged("merge-sides");
    // The merge stops on the conflict; git answers the failure with a non-zero code.
    let _ = plain(&root, &["merge", "--no-edit", "feature"]);

    assert_eq!(repo.operation().unwrap(), Operation::Merge);
    {
        let mut blobs = repo.blobs().unwrap();
        let versions = repo.conflict_versions(&mut blobs, "f.txt").unwrap();
        assert_eq!(
            versions.yours.as_deref(),
            Some(&b"one\nupstream\nthree\n"[..]),
            "in a merge Yours is the branch that is checked out"
        );
    }

    repo.accept_side("f.txt", ConflictSide::Yours).unwrap();

    assert_eq!(read(&root.join("f.txt")), "one\nupstream\nthree\n");
}

/// `f.txt` has ten lines; the first commit changes line 2 and the second changes line 10.
fn history_of_a_file(name: &str) -> (PathBuf, Repository, String) {
    let root = scratch(name);
    let repo = new_repository(&root);
    let lines: String = (1..=10).map(|number| format!("l{number}\n")).collect();
    commit_text(&root, "f.txt", &lines, "base");
    let first = commit_text(&root, "f.txt", &lines.replace("l2\n", "A2\n"), "first");
    commit_text(
        &root,
        "f.txt",
        &lines.replace("l2\n", "A2\n").replace("l10\n", "J10\n"),
        "second",
    );
    (root, repo, first)
}

#[test]
fn safety_revert_of_a_file_keeps_later_commits_and_uncommitted_lines() {
    let (root, repo, first) = history_of_a_file("revert-keeps");
    fs::write(
        root.join("f.txt"),
        "l1\nA2\nl3\nl4\nE5\nl6\nl7\nl8\nl9\nJ10\n",
    )
    .unwrap();

    let outcome = repo.revert_file_of_commit(&first, "f.txt").unwrap();

    assert_eq!(outcome, FileRevert::Reverted);
    assert_eq!(
        read(&root.join("f.txt")),
        "l1\nl2\nl3\nl4\nE5\nl6\nl7\nl8\nl9\nJ10\n",
        "the change of the commit goes, the uncommitted line and the later commit stay"
    );
    assert!(
        plain_ok(&root, &["diff", "--cached", "--name-only"])
            .trim()
            .is_empty(),
        "the index is left alone"
    );
}

#[test]
fn safety_revert_of_a_file_that_meets_a_later_change_stages_a_conflict() {
    let root = scratch("revert-conflict");
    let repo = new_repository(&root);
    commit_text(&root, "f.txt", "l1\nl2\nl3\n", "base");
    let first = commit_text(&root, "f.txt", "l1\nA2\nl3\n", "first");
    commit_text(&root, "f.txt", "l1\nB2\nl3\n", "second");

    let outcome = repo.revert_file_of_commit(&first, "f.txt").unwrap();

    assert_eq!(outcome, FileRevert::Conflict);
    assert_eq!(
        read(&root.join("f.txt")),
        "l1\nB2\nl3\n",
        "the working file keeps its text"
    );
    let stages = plain_ok(&root, &["ls-files", "-u", "--", "f.txt"]);
    let mut numbers: Vec<&str> = stages
        .lines()
        .filter_map(|line| line.split_whitespace().nth(2))
        .collect();
    numbers.sort_unstable();
    assert_eq!(
        numbers,
        ["1", "2", "3"],
        "the three stages are in the index"
    );
    assert_eq!(repo.conflicts().unwrap(), ["f.txt"]);
}

#[test]
fn safety_revert_refuses_a_file_the_commit_adds_and_touches_nothing() {
    let root = scratch("revert-adds");
    let repo = new_repository(&root);
    commit_text(&root, "f.txt", "one\n", "base");
    let added = commit_text(&root, "new.txt", "fresh\n", "adds a file");

    let error = repo.revert_file_of_commit(&added, "new.txt").unwrap_err();

    assert!(error.to_string().contains("adds"), "{error}");
    assert_eq!(read(&root.join("new.txt")), "fresh\n");
}

#[test]
fn safety_revert_restores_a_file_the_commit_deleted() {
    let root = scratch("revert-deletes");
    let repo = new_repository(&root);
    commit_text(&root, "f.txt", "one\n", "base");
    commit_text(&root, "gone.txt", "kept text\n", "adds gone.txt");
    plain_ok(&root, &["rm", "-q", "gone.txt"]);
    plain_ok(&root, &["commit", "-q", "-m", "removes gone.txt"]);
    let deleted = plain_ok(&root, &["rev-parse", "HEAD"]).trim().to_owned();

    let outcome = repo.revert_file_of_commit(&deleted, "gone.txt").unwrap();

    assert_eq!(outcome, FileRevert::Reverted);
    assert_eq!(read(&root.join("gone.txt")), "kept text\n");
}

#[test]
fn safety_a_shelf_patch_without_some_files_applies_the_rest() {
    let root = scratch("shelf-part");
    let repo = new_repository(&root);
    commit_text(&root, "a.txt", "one\n", "base a");
    commit_text(&root, "b.txt", "two\n", "base b");
    fs::write(root.join("a.txt"), "ONE\n").unwrap();
    fs::write(root.join("b.txt"), "TWO\n").unwrap();
    let binary = [0u8, 255, 10, 13, 0, 200];
    fs::write(root.join("img.bin"), binary).unwrap();
    let paths: Vec<String> = ["a.txt", "b.txt", "img.bin"].map(str::to_owned).to_vec();
    let patch = repo.shelve(&paths).unwrap();
    plain_ok(&root, &["checkout", "--", "a.txt", "b.txt"]);
    fs::remove_file(root.join("img.bin")).unwrap();

    let rest = patch_without(&patch, &["a.txt".to_owned()]);
    repo.apply_patch(&rest, None).unwrap();

    assert_eq!(
        read(&root.join("a.txt")),
        "one\n",
        "the chosen file stays out"
    );
    assert_eq!(read(&root.join("b.txt")), "TWO\n");
    assert_eq!(fs::read(root.join("img.bin")).unwrap(), binary);
}

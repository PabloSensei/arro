//! Shelves and the reword of a commit on a scratch repository.

use std::fs;
use std::path::{Path, PathBuf};

use vcs::{CommitRequest, GitRunner, Repository};

fn scratch(name: &str) -> PathBuf {
    test_support::scratch(&format!("vcs-shelf-{name}"))
}

fn run(root: &Path, args: &[&str]) -> String {
    let output = GitRunner::from_env()
        .expect("git is on the PATH")
        .run(root, args)
        .unwrap();
    String::from_utf8_lossy(&output).into_owned()
}

fn repository(name: &str) -> (Repository, PathBuf) {
    let root = scratch(name);
    run(&root, &["init", "-q", "-b", "main"]);
    for (key, value) in [
        ("user.name", "Shelf Test"),
        ("user.email", "shelf@example.invalid"),
        ("core.autocrlf", "false"),
        ("commit.gpgsign", "false"),
    ] {
        run(&root, &["config", key, value]);
    }
    (Repository::open(git(), &root).unwrap(), root)
}

fn git() -> GitRunner {
    GitRunner::from_env().expect("git is on the PATH")
}

fn write(root: &Path, path: &str, text: &str) {
    if let Some(parent) = root.join(path).parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(root.join(path), text).unwrap();
}

fn commit(repo: &Repository, root: &Path, message: &str) {
    write(root, "a.txt", message);
    run(root, &["add", "-A"]);
    repo.commit(&CommitRequest {
        message: message.into(),
        paths: Vec::new(),
        amend: false,
    })
    .unwrap();
}

fn text(root: &Path, path: &str) -> String {
    fs::read_to_string(root.join(path))
        .unwrap()
        .replace("\r\n", "\n")
}

#[test]
fn a_shelf_holds_the_files_and_puts_them_back() {
    let (repo, root) = repository("round-trip");
    write(&root, "a.txt", "one\n");
    run(&root, &["add", "-A"]);
    commit(&repo, &root, "base");
    write(&root, "a.txt", "two\n");
    write(&root, "new.txt", "new\n");

    let paths = vec!["a.txt".to_string(), "new.txt".to_string()];
    let patch = repo.shelve(&paths).unwrap();
    assert!(vcs::patch_files(&patch).contains(&"a.txt".to_string()));
    assert!(vcs::patch_files(&patch).contains(&"new.txt".to_string()));
    repo.reset_paths_to_head(&paths).unwrap();
    assert_eq!(text(&root, "a.txt"), "base");
    assert!(!root.join("new.txt").exists(), "the new file is shelved");

    // The dialog may take only some files of the shelf.
    repo.apply_patch(&patch, Some(&["a.txt".to_string()]))
        .unwrap();
    assert_eq!(text(&root, "a.txt"), "two\n");
    assert!(!root.join("new.txt").exists());
    repo.apply_patch(&patch, Some(&["new.txt".to_string()]))
        .unwrap();
    assert_eq!(text(&root, "new.txt"), "new\n");
}

#[test]
fn a_conflict_names_the_files_and_changes_nothing() {
    let (repo, root) = repository("conflict");
    write(&root, "a.txt", "one\n");
    run(&root, &["add", "-A"]);
    commit(&repo, &root, "base");
    write(&root, "a.txt", "two\n");
    let patch = repo.shelve(&["a.txt".to_string()]).unwrap();
    repo.reset_paths_to_head(&["a.txt".to_string()]).unwrap();

    write(&root, "a.txt", "something else\n");
    let error = repo.apply_patch(&patch, None).unwrap_err().to_string();
    assert!(error.contains("a.txt"), "{error}");
    assert_eq!(text(&root, "a.txt"), "something else\n");
}

#[test]
fn head_gets_another_message_and_keeps_its_tree() {
    let (repo, root) = repository("reword-head");
    commit(&repo, &root, "base");
    commit(&repo, &root, "second");
    let before = repo.log(1, 0).unwrap();

    assert!(repo.is_own_commit(&before[0].id).unwrap());
    assert!(!repo.is_pushed(&before[0].id).unwrap());
    repo.reword(&before[0].id, "second again").unwrap();

    let after = repo.log(2, 0).unwrap();
    assert_eq!(after[0].subject, "second again");
    assert_eq!(after.len(), 2);
    assert_eq!(text(&root, "a.txt"), "second");
    assert!(repo.status().unwrap().files.is_empty());
}

#[test]
fn an_older_commit_is_reworded_without_an_editor() {
    let (repo, root) = repository("reword-older");
    commit(&repo, &root, "base");
    commit(&repo, &root, "second");
    commit(&repo, &root, "third");
    let middle = repo.log(3, 0).unwrap()[1].id.clone();

    repo.reword(&middle, "base again").unwrap();

    let subjects: Vec<String> = repo
        .log(3, 0)
        .unwrap()
        .into_iter()
        .map(|commit| commit.subject)
        .collect();
    assert_eq!(subjects, ["third", "base again", "base"]);
    assert_eq!(text(&root, "a.txt"), "third");
    assert!(repo.status().unwrap().files.is_empty());
}

#[test]
fn a_commit_a_remote_has_is_not_reworded() {
    let (repo, root) = repository("pushed");
    let bare = scratch("pushed-origin");
    run(&bare, &["init", "--bare"]);
    commit(&repo, &root, "base");
    run(
        &root,
        &[
            "remote",
            "add",
            "origin",
            bare.to_str().expect("a UTF-8 path"),
        ],
    );
    run(&root, &["push", "-u", "origin", "HEAD"]);

    let head = repo.log(1, 0).unwrap()[0].id.clone();
    assert!(repo.is_pushed(&head).unwrap());
    let error = repo.reword(&head, "no").unwrap_err().to_string();
    assert!(error.contains("pushed"), "{error}");
    assert_eq!(repo.log(1, 0).unwrap()[0].subject, "base");
}

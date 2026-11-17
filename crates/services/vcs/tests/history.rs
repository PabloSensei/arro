//! The history of a range of lines and the submodules on real repositories in a
//! temporary directory. Every remote is a bare folder, so the machine is neither read
//! nor written outside the scratch folder.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vcs::{GitRunner, Repository, SubmoduleState, UpdateMode};

fn git() -> GitRunner {
    GitRunner::from_env().expect("git in PATH")
}

fn temp_dir(name: &str) -> PathBuf {
    test_support::scratch(&format!("vcs-history-{name}"))
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

/// A repository with an identity of its own, so the configuration of the machine is not
/// read.
fn init(directory: &Path) {
    fs::create_dir_all(directory).unwrap();
    plain_ok(directory, &["init", "-q", "-b", "main"]);
    configure(directory);
}

fn configure(directory: &Path) {
    for (key, value) in [
        ("user.name", "Test"),
        ("user.email", "test@example.invalid"),
        ("core.autocrlf", "false"),
        ("commit.gpgsign", "false"),
        // A submodule of the test is a folder of the same scratch root.
        ("protocol.file.allow", "always"),
    ] {
        plain_ok(directory, &["config", key, value]);
    }
}

fn open(directory: &Path) -> Repository {
    Repository::open(git(), directory).unwrap()
}

fn write(path: &Path, text: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    fs::write(path, text).unwrap();
}

fn commit_all(repo: &Repository, message: &str) -> String {
    repo.commit(&vcs::CommitRequest {
        message: message.into(),
        paths: Vec::new(),
        amend: false,
    })
    .unwrap()
}

#[test]
fn only_the_commits_that_touched_the_lines_are_listed() {
    let root = temp_dir("lines");
    init(&root);
    write(&root.join("f.txt"), "one\ntwo\nthree\nfour\nfive\n");
    let repo = open(&root);
    commit_all(&repo, "base");
    write(&root.join("f.txt"), "one\nTWO\nthree\nfour\nfive\n");
    commit_all(&repo, "second");
    write(&root.join("f.txt"), "one\nTWO\nthree\nFOUR\nfive\n");
    commit_all(&repo, "third");

    let history = repo.line_history("f.txt", 2, 2).unwrap();

    let subjects: Vec<&str> = history
        .iter()
        .map(|entry| entry.commit.subject.as_str())
        .collect();
    assert_eq!(subjects, ["second", "base"]);
    assert_eq!(history[0].before, "two\n");
    assert_eq!(history[0].after, "TWO\n");
    assert_eq!(history[0].line, 2);
    assert_eq!(history[0].path, "f.txt");

    let history = repo.line_history("f.txt", 4, 4).unwrap();
    let subjects: Vec<&str> = history
        .iter()
        .map(|entry| entry.commit.subject.as_str())
        .collect();
    assert_eq!(subjects, ["third", "base"]);
}

#[test]
fn renames_are_followed_and_each_commit_keeps_its_path() {
    let root = temp_dir("rename");
    init(&root);
    write(&root.join("f.txt"), "one\ntwo\nthree\n");
    let repo = open(&root);
    commit_all(&repo, "base");
    write(&root.join("f.txt"), "one\ntwo\nTHREE\n");
    commit_all(&repo, "edit");
    plain_ok(&root, &["mv", "f.txt", "g.txt"]);
    commit_all(&repo, "rename");
    write(&root.join("g.txt"), "one\ntwo\nTHREE!\n");
    commit_all(&repo, "edit again");

    let history = repo.line_history("g.txt", 3, 3).unwrap();

    let entries: Vec<(&str, &str)> = history
        .iter()
        .map(|entry| (entry.commit.subject.as_str(), entry.path.as_str()))
        .collect();
    assert_eq!(
        entries,
        [
            ("edit again", "g.txt"),
            ("edit", "f.txt"),
            ("base", "f.txt")
        ]
    );
    assert_eq!(history[0].before, "THREE\n");
    assert_eq!(history[0].after, "THREE!\n");
}

#[test]
fn a_line_outside_the_file_is_an_error_of_git() {
    let root = temp_dir("outside");
    init(&root);
    write(&root.join("f.txt"), "one\n");
    let repo = open(&root);
    commit_all(&repo, "base");

    assert!(repo.line_history("f.txt", 99, 99).is_err());
    assert!(repo.line_history("missing.txt", 1, 1).is_err());
}

/// A parent with a submodule of its own and both working copies ready to change.
fn project_with_submodule(name: &str) -> (PathBuf, PathBuf) {
    let root = temp_dir(name);
    let sub = root.join("sub");
    init(&sub);
    write(&sub.join("inside.txt"), "first\n");
    let sub_repo = open(&sub);
    commit_all(&sub_repo, "sub base");

    let parent = root.join("parent");
    init(&parent);
    write(&parent.join("main.txt"), "parent\n");
    let parent_repo = open(&parent);
    commit_all(&parent_repo, "base");
    plain_ok(
        &parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            sub.to_str().unwrap(),
            "sub",
        ],
    );
    configure(&parent.join("sub"));
    commit_all(&parent_repo, "add submodule");
    (parent.clone(), parent.join("sub"))
}

#[test]
fn every_state_of_a_submodule_is_read() {
    let (parent, sub) = project_with_submodule("states");
    let repo = open(&parent);

    let submodules = repo.submodules().unwrap();
    assert_eq!(submodules.len(), 1);
    assert_eq!(submodules[0].path, "sub");
    assert_eq!(submodules[0].state, SubmoduleState::Initialized);

    // A commit inside the submodule makes the pointer of the parent differ.
    write(&sub.join("inside.txt"), "second\n");
    let sub_repo = open(&sub);
    commit_all(&sub_repo, "sub change");
    let submodules = repo.submodules().unwrap();
    assert_eq!(submodules[0].state, SubmoduleState::Modified);

    plain_ok(&parent, &["submodule", "deinit", "-f", "sub"]);
    let submodules = repo.submodules().unwrap();
    assert_eq!(submodules[0].state, SubmoduleState::Uninitialized);
}

#[test]
fn a_changed_pointer_names_the_commits_between_the_two_versions() {
    let (parent, sub) = project_with_submodule("pointer");
    let repo = open(&parent);
    let recorded = plain_ok(&parent, &["rev-parse", "HEAD:sub"])
        .trim()
        .to_owned();
    write(&sub.join("inside.txt"), "second\n");
    let sub_repo = open(&sub);
    let working = commit_all(&sub_repo, "sub change");

    let (old, new) = repo.submodule_pointer("sub").unwrap().unwrap();
    assert_eq!(old.as_deref(), Some(recorded.as_str()));
    assert_eq!(new.as_deref(), Some(working.as_str()));

    let commits = repo
        .submodule_commits("sub", old.as_deref(), new.as_deref())
        .unwrap();
    let subjects: Vec<&str> = commits
        .iter()
        .map(|commit| commit.subject.as_str())
        .collect();
    assert_eq!(subjects, ["sub change"]);
}

#[test]
fn every_root_has_its_own_status_and_the_update_reaches_it() {
    let root = temp_dir("roots");
    let sub_origin = root.join("sub-origin.git");
    plain_ok(
        &root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            sub_origin.to_str().unwrap(),
        ],
    );
    let sub_work = root.join("sub-work");
    plain_ok(
        &root,
        &[
            "clone",
            "-q",
            sub_origin.to_str().unwrap(),
            sub_work.to_str().unwrap(),
        ],
    );
    configure(&sub_work);
    write(&sub_work.join("inside.txt"), "first\n");
    let sub_repo = open(&sub_work);
    commit_all(&sub_repo, "sub base");
    plain_ok(&sub_work, &["push", "-q", "-u", "origin", "HEAD"]);

    let parent = root.join("parent");
    let parent_origin = root.join("parent-origin.git");
    plain_ok(
        &root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            parent_origin.to_str().unwrap(),
        ],
    );
    init(&parent);
    write(&parent.join("main.txt"), "parent\n");
    let parent_repo = open(&parent);
    commit_all(&parent_repo, "base");
    plain_ok(
        &parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            sub_origin.to_str().unwrap(),
            "sub",
        ],
    );
    configure(&parent.join("sub"));
    plain_ok(&parent.join("sub"), &["switch", "main"]);
    plain_ok(
        &parent.join("sub"),
        &["branch", "--set-upstream-to=origin/main", "main"],
    );
    commit_all(&parent_repo, "add submodule");
    plain_ok(
        &parent,
        &["remote", "add", "origin", parent_origin.to_str().unwrap()],
    );
    plain_ok(&parent, &["push", "-q", "-u", "origin", "HEAD"]);

    let roots = parent_repo.roots().unwrap();
    let names: Vec<&str> = roots.iter().map(|(path, _)| path.as_str()).collect();
    assert_eq!(names, ["", "sub"]);
    let (prefix, sub_clone) = parent_repo.root_of("sub/inside.txt").unwrap();
    assert_eq!(prefix, "sub");
    assert_eq!(sub_clone.root(), parent.join("sub"));

    write(&parent.join("main.txt"), "changed\n");
    write(&parent.join("sub").join("inside.txt"), "changed\n");
    let status = parent_repo.status().unwrap();
    assert!(
        status.files.iter().any(|file| file.path == "main.txt"),
        "the parent sees its own file: {:?}",
        status.files
    );
    let sub_status = sub_clone.status().unwrap();
    assert_eq!(sub_status.files.len(), 1, "the submodule sees its file");
    plain_ok(&parent.join("sub"), &["checkout", "--", "inside.txt"]);

    // A newer commit of the submodule on its remote, recorded by the parent.
    write(&sub_work.join("inside.txt"), "third\n");
    let sub_repo = open(&sub_work);
    let pushed = commit_all(&sub_repo, "sub change");
    plain_ok(&sub_work, &["push", "-q", "origin", "HEAD"]);
    plain_ok(&parent.join("sub"), &["fetch", "-q", "origin"]);
    plain_ok(&parent.join("sub"), &["checkout", "-q", &pushed]);
    commit_all(&parent_repo, "pointer");
    plain_ok(&parent, &["push", "-q", "origin", "HEAD"]);
    let recorded = plain_ok(&parent, &["rev-parse", "HEAD:sub"])
        .trim()
        .to_owned();
    // The working tree of the submodule still stands at the older commit.
    plain_ok(&parent.join("sub"), &["checkout", "-q", &recorded]);

    let update = parent_repo.update_all(UpdateMode::Merge, true).unwrap();
    assert!(update.errors.is_empty(), "{:?}", update.errors);
    assert_eq!(
        plain_ok(&parent.join("sub"), &["rev-parse", "HEAD"]).trim(),
        pushed
    );
}

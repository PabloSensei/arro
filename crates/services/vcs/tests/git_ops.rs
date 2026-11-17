//! Stash, fetch, merge, rebase, conflicts and the file log on real repositories in a
//! temporary directory. Every remote is a bare folder, and the identity of a commit lives
//! in its repository, so the git configuration of the machine is neither read nor written.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vcs::{Change, CommitRequest, ConflictSide, GitRunner, Operation, Repository, UpdateMode};

fn git() -> GitRunner {
    GitRunner::from_env().expect("git in PATH")
}

fn temp_dir(name: &str) -> PathBuf {
    test_support::scratch(&format!("vcs-ops-{name}"))
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
    configure(directory, "Test", "test@example.invalid");
}

fn configure(directory: &Path, name: &str, email: &str) {
    for (key, value) in [
        ("user.name", name),
        ("user.email", email),
        ("core.autocrlf", "false"),
        ("commit.gpgsign", "false"),
    ] {
        plain_ok(directory, &["config", key, value]);
    }
}

fn open(directory: &Path) -> Repository {
    Repository::open(git(), directory).unwrap()
}

fn commit(repo: &Repository, message: &str, paths: &[&str]) -> String {
    repo.commit(&CommitRequest {
        message: message.into(),
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        amend: false,
    })
    .unwrap()
}

fn clone(origin: &Path, into: &Path, name: &str, email: &str) {
    plain_ok(
        origin,
        &[
            "clone",
            "-q",
            origin.to_str().unwrap(),
            into.to_str().unwrap(),
        ],
    );
    configure(into, name, email);
}

/// A bare origin and a clone with one commit on `main`, pushed.
fn repository_with_origin(name: &str) -> (PathBuf, PathBuf, PathBuf, Repository) {
    let root = temp_dir(name);
    let bare = root.join("origin.git");
    let work = root.join("work");
    fs::create_dir_all(&work).unwrap();
    plain_ok(
        &root,
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    init(&work);
    fs::write(work.join("main.txt"), "base\n").unwrap();
    let repo = open(&work);
    commit(&repo, "base", &[]);
    plain_ok(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
    plain_ok(&work, &["push", "-q", "-u", "origin", "HEAD"]);
    (root, bare, work, repo)
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap().replace("\r\n", "\n")
}

#[test]
fn stash_is_pushed_listed_applied_popped_and_dropped() {
    let root = temp_dir("stash");
    init(&root);
    fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    let repo = open(&root);
    commit(&repo, "base", &[]);

    fs::write(root.join("a.txt"), "one\nTWO\n").unwrap();
    repo.stash_push("my work").unwrap();
    assert_eq!(read(&root.join("a.txt")), "one\ntwo\n");
    let stashes = repo.stash_list().unwrap();
    assert_eq!(stashes.len(), 1);
    assert_eq!(stashes[0].index, 0);
    assert_eq!(stashes[0].message, "On main: my work");

    repo.stash_apply(0).unwrap();
    assert_eq!(read(&root.join("a.txt")), "one\nTWO\n");
    assert_eq!(repo.stash_list().unwrap().len(), 1, "apply keeps the stash");

    plain_ok(&root, &["checkout", "--", "a.txt"]);
    repo.stash_drop(0).unwrap();
    assert!(repo.stash_list().unwrap().is_empty());

    fs::write(root.join("a.txt"), "one\nTHREE\n").unwrap();
    repo.stash_push("second").unwrap();
    repo.stash_pop(0).unwrap();
    assert_eq!(read(&root.join("a.txt")), "one\nTHREE\n");
    assert!(repo.stash_list().unwrap().is_empty(), "pop drops it");
}

#[test]
fn fetch_brings_the_remote_branch_without_touching_the_worktree() {
    let (root, bare, work, repo) = repository_with_origin("fetch");

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("remote.txt"), "remote\n").unwrap();
    plain_ok(&other, &["add", "remote.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "remote change"]);
    plain_ok(&other, &["push", "-q"]);

    repo.fetch().unwrap();
    let status = repo.status().unwrap();
    assert_eq!((status.ahead, status.behind), (0, 1));
    assert!(!work.join("remote.txt").exists(), "fetch does not merge");
    assert_eq!(read(&work.join("main.txt")), "base\n");
}

#[test]
fn a_diverged_branch_merges_without_a_conflict() {
    let (root, bare, work, repo) = repository_with_origin("merge");

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("remote.txt"), "remote\n").unwrap();
    plain_ok(&other, &["add", "remote.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "remote change"]);
    plain_ok(&other, &["push", "-q"]);

    fs::write(work.join("local.txt"), "local\n").unwrap();
    commit(&repo, "local change", &[]);

    let report = repo.update_with(UpdateMode::Merge).unwrap();
    assert_eq!(report.operation, Operation::None);
    assert!(report.conflicts.is_empty());
    let log = repo.log(1, 0).unwrap();
    assert_eq!(log[0].parents.len(), 2, "the update made a merge commit");
    assert_eq!(read(&work.join("remote.txt")), "remote\n");
    assert_eq!(read(&work.join("local.txt")), "local\n");
}

#[test]
fn uncommitted_changes_are_stashed_around_the_update_and_come_back() {
    let (root, bare, work, repo) = repository_with_origin("auto-stash");

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("remote.txt"), "remote\n").unwrap();
    plain_ok(&other, &["add", "remote.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "remote change"]);
    plain_ok(&other, &["push", "-q"]);

    fs::write(work.join("local.txt"), "local\n").unwrap();
    commit(&repo, "local change", &[]);
    fs::write(work.join("main.txt"), "uncommitted\n").unwrap();

    let report = repo.update_with(UpdateMode::Merge).unwrap();
    assert_eq!(report.operation, Operation::None);
    assert!(report.conflicts.is_empty());
    assert!(repo.stash_list().unwrap().is_empty(), "the stash came back");
    assert_eq!(read(&work.join("main.txt")), "uncommitted\n");
    assert_eq!(read(&work.join("remote.txt")), "remote\n");
}

#[test]
fn a_merge_conflict_takes_a_side_and_ends_with_a_merge_commit() {
    let (root, bare, work, repo) = repository_with_origin("merge-conflict");

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("main.txt"), "other\n").unwrap();
    plain_ok(&other, &["add", "main.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "other change"]);
    plain_ok(&other, &["push", "-q"]);

    fs::write(work.join("main.txt"), "local\n").unwrap();
    commit(&repo, "local change", &[]);

    let report = repo.update_with(UpdateMode::Merge).unwrap();
    assert_eq!(report.operation, Operation::Merge);
    assert_eq!(report.conflicts, ["main.txt"]);
    assert!(!report.merge_message.is_empty(), "git has a merge message");
    assert!(read(&work.join("main.txt")).contains("<<<<<<<"));

    repo.accept_side("main.txt", ConflictSide::Theirs).unwrap();
    assert!(repo.conflicts().unwrap().is_empty());
    assert_eq!(read(&work.join("main.txt")), "other\n");

    repo.finish("Merge the remote change").unwrap();
    assert_eq!(repo.operation().unwrap(), Operation::None);
    let log = repo.log(1, 0).unwrap();
    assert_eq!(log[0].subject, "Merge the remote change");
    assert_eq!(log[0].parents.len(), 2);
    assert_eq!(read(&work.join("main.txt")), "other\n");
}

#[test]
fn a_rebase_conflict_continues_after_a_side_is_taken() {
    let (root, bare, work, repo) = repository_with_origin("rebase");

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("main.txt"), "remote\n").unwrap();
    plain_ok(&other, &["add", "main.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "remote change"]);
    plain_ok(&other, &["push", "-q"]);

    fs::write(work.join("main.txt"), "local\n").unwrap();
    commit(&repo, "local change", &[]);

    let report = repo.update_with(UpdateMode::Rebase).unwrap();
    assert_eq!(report.operation, Operation::Rebase);
    assert_eq!(report.conflicts, ["main.txt"]);

    repo.accept_side("main.txt", ConflictSide::Theirs).unwrap();
    repo.finish("").unwrap();
    assert_eq!(repo.operation().unwrap(), Operation::None);
    let log = repo.log(1, 0).unwrap();
    assert_eq!(
        log[0].subject, "local change",
        "the local commit is replayed"
    );
    assert_eq!(log[0].parents.len(), 1);
    assert_eq!(read(&work.join("main.txt")), "local\n");
}

#[test]
fn abort_puts_the_branch_back_as_it_was() {
    let (root, bare, work, repo) = repository_with_origin("abort");

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("main.txt"), "remote\n").unwrap();
    plain_ok(&other, &["add", "main.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "remote change"]);
    plain_ok(&other, &["push", "-q"]);

    fs::write(work.join("main.txt"), "local\n").unwrap();
    commit(&repo, "local change", &[]);

    let report = repo.update_with(UpdateMode::Rebase).unwrap();
    assert_eq!(report.operation, Operation::Rebase);
    repo.abort().unwrap();
    assert_eq!(repo.operation().unwrap(), Operation::None);
    assert_eq!(repo.log(1, 0).unwrap()[0].subject, "local change");
    assert_eq!(read(&work.join("main.txt")), "local\n");
}

#[test]
fn the_log_the_file_log_and_a_commit_diff_name_what_changed() {
    let root = temp_dir("log");
    init(&root);
    fs::write(root.join("a.txt"), "one\n").unwrap();
    let repo = open(&root);
    let first = commit(&repo, "add a", &[]);
    fs::write(root.join("a.txt"), "one\ntwo\n").unwrap();
    let second = commit(&repo, "extend a", &["a.txt"]);
    fs::write(root.join("b.txt"), "b\n").unwrap();
    let third = commit(&repo, "add b", &[]);

    let log = repo.log(10, 0).unwrap();
    let subjects: Vec<_> = log.iter().map(|commit| commit.subject.as_str()).collect();
    assert_eq!(subjects, ["add b", "extend a", "add a"]);
    assert_eq!(log[0].id, third);
    assert_eq!(log[1].id, second);

    let file_log = repo.file_log("a.txt", 10, 0).unwrap();
    let subjects: Vec<_> = file_log
        .iter()
        .map(|commit| commit.subject.as_str())
        .collect();
    assert_eq!(subjects, ["extend a", "add a"], "only commits of the file");

    assert_eq!(repo.commit_files(&second).unwrap(), ["a.txt"]);
    assert_eq!(repo.commit_files(&third).unwrap(), ["b.txt"]);
    let mut blobs = repo.blobs().unwrap();
    let diff = repo.commit_file_diff(&mut blobs, &second, "a.txt").unwrap();
    assert_eq!(diff.base, "one\n");
    assert_eq!(diff.hunks, vcs::line_hunks("one\n", "one\ntwo\n"));
    assert!(diff.base != "one\ntwo\n", "the first commit is the base");
    let root_commit = repo.commit_files(&first).unwrap();
    assert_eq!(root_commit, ["a.txt"], "the root commit lists its files");
}

#[test]
fn an_ordinary_change_shows_as_conflicted_until_it_is_resolved() {
    let root = temp_dir("mark");
    init(&root);
    fs::write(root.join("a.txt"), "one\n").unwrap();
    let repo = open(&root);
    commit(&repo, "base", &[]);

    fs::write(root.join("a.txt"), "two\n").unwrap();
    repo.mark_resolved("a.txt").unwrap();
    let status = repo.status().unwrap();
    assert_eq!(status.files[0].index, Change::Modified);
}

/// A diverged branch with a conflict in `main.txt` and a tracked `keep.txt` that the local
/// side commits and then changes without committing.
fn repository_with_a_conflict_and_a_local_change(
    name: &str,
) -> (PathBuf, PathBuf, PathBuf, Repository) {
    let (root, bare, work, repo) = repository_with_origin(name);

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("main.txt"), "other\n").unwrap();
    plain_ok(&other, &["add", "main.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "other change"]);
    plain_ok(&other, &["push", "-q"]);

    fs::write(work.join("main.txt"), "local\n").unwrap();
    fs::write(work.join("keep.txt"), "keep\n").unwrap();
    commit(&repo, "local change", &[]);
    fs::write(work.join("keep.txt"), "uncommitted\n").unwrap();
    (root, bare, work, repo)
}

#[test]
fn the_autostash_of_a_merge_conflict_comes_back_after_the_merge_commit() {
    let (_root, _bare, work, repo) =
        repository_with_a_conflict_and_a_local_change("merge-autostash");

    let report = repo.update_with(UpdateMode::Merge).unwrap();
    assert_eq!(report.operation, Operation::Merge);
    assert!(report.autostash, "git keeps the local change for the merge");
    assert!(report.stash.is_none(), "it is not a stash entry yet");
    assert!(repo.stash_list().unwrap().is_empty());
    assert_eq!(
        read(&work.join("keep.txt")),
        "keep\n",
        "the change is hidden"
    );

    repo.accept_side("main.txt", ConflictSide::Theirs).unwrap();
    let stash = repo.finish("Merge the remote change").unwrap();
    assert!(stash.is_none(), "the autostash applied cleanly");
    assert_eq!(repo.operation().unwrap(), Operation::None);
    assert!(!repo.autostash().unwrap());
    assert!(repo.stash_list().unwrap().is_empty());
    assert_eq!(read(&work.join("keep.txt")), "uncommitted\n");
    assert_eq!(read(&work.join("main.txt")), "other\n");
}

#[test]
fn the_autostash_of_a_merge_conflict_comes_back_after_abort() {
    let (_root, _bare, work, repo) =
        repository_with_a_conflict_and_a_local_change("merge-autostash-abort");

    let report = repo.update_with(UpdateMode::Merge).unwrap();
    assert_eq!(report.operation, Operation::Merge);
    assert!(report.autostash);

    let stash = repo.abort().unwrap();
    assert!(stash.is_none());
    assert_eq!(repo.operation().unwrap(), Operation::None);
    assert_eq!(read(&work.join("keep.txt")), "uncommitted\n");
    assert_eq!(read(&work.join("main.txt")), "local\n");
    let log = repo.log(1, 0).unwrap();
    assert_eq!(log[0].subject, "local change", "HEAD is where it was");
    assert_eq!(log[0].parents.len(), 1);
    assert!(repo.stash_list().unwrap().is_empty());
}

#[test]
fn the_autostash_of_a_rebase_conflict_comes_back_after_continue() {
    let (_root, _bare, work, repo) =
        repository_with_a_conflict_and_a_local_change("rebase-autostash");

    let report = repo.update_with(UpdateMode::Rebase).unwrap();
    assert_eq!(report.operation, Operation::Rebase);
    assert!(report.autostash);
    assert!(repo.stash_list().unwrap().is_empty());

    repo.accept_side("main.txt", ConflictSide::Theirs).unwrap();
    let stash = repo.finish("").unwrap();
    assert!(stash.is_none(), "the autostash applied cleanly");
    assert_eq!(repo.operation().unwrap(), Operation::None);
    assert!(!repo.autostash().unwrap());
    assert_eq!(read(&work.join("keep.txt")), "uncommitted\n");
    assert!(repo.stash_list().unwrap().is_empty());
}

#[test]
fn the_autostash_of_a_rebase_conflict_comes_back_after_abort() {
    let (_root, _bare, work, repo) =
        repository_with_a_conflict_and_a_local_change("rebase-autostash-abort");

    let report = repo.update_with(UpdateMode::Rebase).unwrap();
    assert_eq!(report.operation, Operation::Rebase);
    assert!(report.autostash);

    let stash = repo.abort().unwrap();
    assert!(stash.is_none());
    assert_eq!(repo.operation().unwrap(), Operation::None);
    assert_eq!(read(&work.join("keep.txt")), "uncommitted\n");
    assert_eq!(read(&work.join("main.txt")), "local\n");
    let log = repo.log(1, 0).unwrap();
    assert_eq!(log[0].subject, "local change");
    assert!(repo.stash_list().unwrap().is_empty());
}

#[test]
fn an_older_user_stash_stays_untouched_through_the_update() {
    let (root, bare, work, repo) = repository_with_origin("user-stash");

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("main.txt"), "other\n").unwrap();
    plain_ok(&other, &["add", "main.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "other change"]);
    plain_ok(&other, &["push", "-q"]);

    fs::write(work.join("main.txt"), "user work\n").unwrap();
    repo.stash_push("my work").unwrap();
    fs::write(work.join("main.txt"), "local\n").unwrap();
    fs::write(work.join("keep.txt"), "keep\n").unwrap();
    commit(&repo, "local change", &[]);
    fs::write(work.join("keep.txt"), "uncommitted\n").unwrap();

    let report = repo.update_with(UpdateMode::Merge).unwrap();
    assert_eq!(report.operation, Operation::Merge);
    assert!(report.autostash);
    assert_eq!(repo.stash_list().unwrap().len(), 1, "only the user stash");

    repo.accept_side("main.txt", ConflictSide::Theirs).unwrap();
    assert!(repo.finish("Merge the remote change").unwrap().is_none());
    let stashes = repo.stash_list().unwrap();
    assert_eq!(stashes.len(), 1, "the user stash is not applied or dropped");
    assert_eq!(stashes[0].message, "On main: my work");
    assert_eq!(read(&work.join("keep.txt")), "uncommitted\n");
}

#[test]
fn a_merge_that_cannot_put_the_local_change_back_reports_its_stash() {
    let (root, bare, work, repo) = repository_with_origin("autostash-conflict");

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("main.txt"), "remote\n").unwrap();
    plain_ok(&other, &["add", "main.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "remote change"]);
    plain_ok(&other, &["push", "-q"]);

    fs::write(work.join("other.txt"), "local\n").unwrap();
    commit(&repo, "local change", &[]);
    fs::write(work.join("main.txt"), "uncommitted\n").unwrap();

    let report = repo.update_with(UpdateMode::Merge).unwrap();
    assert_eq!(report.operation, Operation::None, "the merge itself ended");
    assert!(!report.autostash);
    let stash = report.stash.expect("the change waits in a stash");
    assert_eq!(stash.index, 0);
    assert!(
        stash.message.contains("autostash"),
        "git names the stash: {}",
        stash.message
    );
    assert_eq!(repo.stash_list().unwrap().len(), 1);
    assert!(
        read(&work.join("main.txt")).contains("<<<<<<<"),
        "the failed apply leaves the markers for the user"
    );
    assert!(repo.conflicts().unwrap().contains(&"main.txt".to_owned()));
}

#[test]
fn a_rebase_continue_that_cannot_put_the_local_change_back_reports_its_stash() {
    let (root, bare, work, repo) = repository_with_origin("rebase-autostash-conflict");

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("main.txt"), "remote\n").unwrap();
    plain_ok(&other, &["add", "main.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "remote change"]);
    plain_ok(&other, &["push", "-q"]);

    fs::write(work.join("main.txt"), "local\n").unwrap();
    commit(&repo, "local change", &[]);
    fs::write(work.join("main.txt"), "fixup\n").unwrap();

    let report = repo.update_with(UpdateMode::Rebase).unwrap();
    assert_eq!(report.operation, Operation::Rebase);
    assert!(report.autostash);
    assert!(report.stash.is_none(), "the apply waits for the end");

    repo.accept_side("main.txt", ConflictSide::Yours).unwrap();
    let stash = repo
        .finish("")
        .unwrap()
        .expect("the change waits in a stash");
    assert_eq!(stash.index, 0);
    assert!(
        stash.message.contains("autostash"),
        "git names the stash: {}",
        stash.message
    );
    assert_eq!(repo.operation().unwrap(), Operation::None);
    assert_eq!(repo.stash_list().unwrap().len(), 1);
    assert!(read(&work.join("main.txt")).contains("<<<<<<<"));
}

#[test]
fn branches_name_their_remote_and_count_the_commits_on_both_sides() {
    let (root, bare, work, repo) = repository_with_origin("branch-track");

    let other = root.join("other");
    clone(&bare, &other, "Other", "other@example.invalid");
    fs::write(other.join("remote.txt"), "remote\n").unwrap();
    plain_ok(&other, &["add", "remote.txt"]);
    plain_ok(&other, &["commit", "-q", "-m", "remote change"]);
    plain_ok(&other, &["push", "-q"]);
    repo.fetch().unwrap();

    let branches = repo.branches().unwrap();
    let main = branches
        .iter()
        .find(|branch| branch.name == "main")
        .unwrap();
    assert!(!main.remote);
    assert!(main.current);
    assert_eq!(main.upstream.as_deref(), Some("origin/main"));
    assert_eq!((main.ahead, main.behind), (0, 1));
    let remote = branches
        .iter()
        .find(|branch| branch.name == "origin/main")
        .unwrap();
    assert!(remote.remote);
    assert!(!remote.current);

    fs::write(work.join("local.txt"), "local\n").unwrap();
    commit(&repo, "local change", &[]);
    let main = repo
        .branches()
        .unwrap()
        .into_iter()
        .find(|branch| branch.name == "main")
        .unwrap();
    assert_eq!((main.ahead, main.behind), (1, 1));
    let _ = bare;
}

#[test]
fn an_unmerged_branch_is_deleted_only_by_force_and_comes_back_by_its_head() {
    let (_root, _bare, work, repo) = repository_with_origin("branch-delete");
    fs::write(work.join("side.txt"), "side\n").unwrap();
    commit(&repo, "side change", &[]);
    plain_ok(&work, &["branch", "side"]);
    plain_ok(&work, &["reset", "-q", "--hard", "HEAD~1"]);

    assert!(!repo.is_branch_merged("side").unwrap());
    assert!(repo.delete_branch("side").is_err(), "git -d refuses it");
    let head = repo.branch_head("side").unwrap();

    repo.force_delete_branch("side").unwrap();
    assert!(
        repo.branches()
            .unwrap()
            .iter()
            .all(|branch| branch.name != "side")
    );

    repo.restore_branch("side", &head).unwrap();
    let side = repo
        .branches()
        .unwrap()
        .into_iter()
        .find(|branch| branch.name == "side")
        .unwrap();
    assert_eq!(side.head, head);

    repo.merge_branch("side").unwrap();
    assert!(repo.is_branch_merged("side").unwrap());
    repo.delete_branch("side").unwrap();
    assert!(
        repo.branches()
            .unwrap()
            .iter()
            .all(|branch| branch.name != "side")
    );
}

#[test]
fn a_branch_merges_and_rebases_into_the_current_one() {
    let (_root, _bare, work, repo) = repository_with_origin("branch-merge");
    plain_ok(&work, &["switch", "-q", "-c", "side"]);
    fs::write(work.join("side.txt"), "side\n").unwrap();
    plain_ok(&work, &["add", "side.txt"]);
    plain_ok(&work, &["commit", "-q", "-m", "side change"]);
    plain_ok(&work, &["switch", "-q", "main"]);
    fs::write(work.join("main.txt"), "main\n").unwrap();
    commit(&repo, "main change", &[]);

    let report = repo.merge_branch("side").unwrap();
    assert_eq!(report.operation, Operation::None);
    assert!(report.conflicts.is_empty());
    let log = repo.log(1, 0).unwrap();
    assert_eq!(log[0].parents.len(), 2, "a merge commit was made");
    assert_eq!(read(&work.join("side.txt")), "side\n");

    let (_root, _bare, work, repo) = repository_with_origin("branch-rebase");
    plain_ok(&work, &["switch", "-q", "-c", "side"]);
    fs::write(work.join("side.txt"), "side\n").unwrap();
    plain_ok(&work, &["add", "side.txt"]);
    plain_ok(&work, &["commit", "-q", "-m", "side change"]);
    plain_ok(&work, &["switch", "-q", "main"]);
    fs::write(work.join("main.txt"), "main\n").unwrap();
    commit(&repo, "main change", &[]);

    let report = repo.rebase_onto("side").unwrap();
    assert_eq!(report.operation, Operation::None);
    assert!(report.conflicts.is_empty());
    let log = repo.log(1, 0).unwrap();
    assert_eq!(log[0].subject, "main change");
    assert_eq!(log[0].parents.len(), 1, "the commit was replayed");
    assert_eq!(read(&work.join("main.txt")), "main\n");
    assert_eq!(read(&work.join("side.txt")), "side\n");
}

#[test]
fn compare_shows_the_commits_and_files_of_both_branches() {
    let (_root, _bare, work, repo) = repository_with_origin("branch-compare");
    plain_ok(&work, &["switch", "-q", "-c", "side"]);
    fs::write(work.join("side.txt"), "side\n").unwrap();
    plain_ok(&work, &["add", "side.txt"]);
    plain_ok(&work, &["commit", "-q", "-m", "side change"]);
    plain_ok(&work, &["switch", "-q", "main"]);
    fs::write(work.join("main.txt"), "main\n").unwrap();
    commit(&repo, "main change", &[]);

    let compare = repo.compare_with("side").unwrap();
    assert_eq!(
        compare
            .only_base
            .iter()
            .map(|commit| commit.subject.as_str())
            .collect::<Vec<_>>(),
        ["side change"]
    );
    assert_eq!(
        compare
            .only_current
            .iter()
            .map(|commit| commit.subject.as_str())
            .collect::<Vec<_>>(),
        ["main change"]
    );
    assert_eq!(compare.files, ["main.txt", "side.txt"]);

    fs::write(work.join("main.txt"), "working\n").unwrap();
    let compare = repo.working_tree_compare("side").unwrap();
    assert_eq!(compare.only_current, Vec::new());
    assert!(compare.files.contains(&"main.txt".to_owned()));
}

#[test]
fn blame_names_the_commit_of_every_line_and_the_message_of_each_commit() {
    let root = temp_dir("blame");
    init(&root);
    fs::write(root.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    let repo = open(&root);
    let first = commit(&repo, "first", &[]);
    configure(&root, "Second Author", "second@example.invalid");
    fs::write(root.join("a.txt"), "one\nTWO\nthree\n").unwrap();
    let second = commit(&repo, "second", &[]);

    let lines = repo.blame("a.txt").unwrap();
    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].commit, first);
    assert_eq!(lines[0].author, "Test");
    assert_eq!(lines[2].commit, first);
    assert_eq!(lines[1].commit, second);
    assert_eq!(lines[1].author, "Second Author");
    assert_eq!(lines[1].email, "second@example.invalid");
    assert_eq!(
        lines.iter().map(|line| line.line).collect::<Vec<_>>(),
        [1, 2, 3]
    );

    // A line that only lives in the working tree has no commit.
    fs::write(root.join("a.txt"), "one\nTWO\nthree\nfour\n").unwrap();
    let lines = repo.blame("a.txt").unwrap();
    assert_eq!(lines.len(), 4);
    assert_eq!(lines[3].author, "Not Committed Yet");
    assert!(!lines[3].is_committed());

    let messages = repo
        .commit_messages(&[first.clone(), second.clone()])
        .unwrap();
    assert_eq!(messages.get(&first).map(String::as_str), Some("first"));
    assert_eq!(messages.get(&second).map(String::as_str), Some("second"));
}

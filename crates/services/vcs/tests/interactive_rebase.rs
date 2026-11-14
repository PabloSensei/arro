//! The interactive rebase on real repositories in a temporary directory: the plan of the
//! dialog, its reorder, drop, squash, fixup and reword, a stop for an edit, and a conflict
//! that Abort undoes. The identity of a commit lives in its repository, so the git
//! configuration of the machine is neither read nor written.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use vcs::{CommitRequest, GitRunner, Operation, RebaseAction, RebasePlan, Repository};

fn git() -> GitRunner {
    GitRunner::from_env().expect("git in PATH")
}

fn temp_dir(name: &str) -> PathBuf {
    test_support::scratch(&format!("vcs-rebase-{name}"))
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

/// A repository with a `base` commit and the commits `one`, `two` and `three`, each of
/// them adding a file of its own name.
fn linear(name: &str) -> (PathBuf, Repository) {
    let root = temp_dir(name);
    init(&root);
    let repo = open(&root);
    for subject in ["base", "one", "two", "three"] {
        fs::write(root.join(format!("{subject}.txt")), format!("{subject}\n")).unwrap();
        commit(&repo, subject);
    }
    (root, repo)
}

/// The plan from the commit with `subject`, which every test changes as the dialog does.
fn plan_from(repo: &Repository, subject: &str) -> RebasePlan {
    repo.rebase_plan(&id_of(repo, subject)).unwrap()
}

fn id_of(repo: &Repository, subject: &str) -> String {
    repo.log(20, 0)
        .unwrap()
        .into_iter()
        .find(|commit| commit.subject == subject)
        .unwrap_or_else(|| panic!("no commit {subject}"))
        .id
}

fn subjects(repo: &Repository) -> Vec<String> {
    repo.log(20, 0)
        .unwrap()
        .into_iter()
        .map(|commit| commit.subject)
        .collect()
}

fn step_index(plan: &RebasePlan, subject: &str) -> usize {
    plan.steps
        .iter()
        .position(|step| step.subject == subject)
        .unwrap_or_else(|| panic!("no step {subject} in the plan"))
}

fn message_of(repo: &Repository, subject: &str) -> String {
    let id = id_of(repo, subject);
    repo.commit_messages(&[id])
        .unwrap()
        .into_values()
        .next()
        .expect("the message of the commit")
}

#[test]
fn the_plan_lists_the_commits_from_the_chosen_one_to_head_oldest_first() {
    let (_root, repo) = linear("plan");
    let base = id_of(&repo, "base");
    let plan = plan_from(&repo, "one");

    assert_eq!(plan.base.as_deref(), Some(base.as_str()));
    let subjects: Vec<&str> = plan
        .steps
        .iter()
        .map(|step| step.subject.as_str())
        .collect();
    assert_eq!(subjects, ["one", "two", "three"]);
    assert!(
        plan.steps
            .iter()
            .all(|step| step.action == RebaseAction::Pick)
    );
    assert_eq!(plan.steps[0].author, "Test");
    assert!(plan.steps[0].message.contains("one"));
    plan.check().unwrap();

    let root = plan_from(&repo, "base");
    assert_eq!(root.base, None, "the root commit starts at the root");
    assert_eq!(root.steps.len(), 4);
}

#[test]
fn a_plan_of_a_commit_of_another_branch_is_refused() {
    let (root, repo) = linear("plan-branch");
    plain_ok(&root, &["switch", "-q", "-c", "side"]);
    fs::write(root.join("side.txt"), "side\n").unwrap();
    let side = commit(&repo, "side");
    plain_ok(&root, &["switch", "-q", "main"]);

    let error = repo.rebase_plan(&side).unwrap_err();
    assert!(
        error.to_string().contains("not on the current branch"),
        "{error}"
    );
}

#[test]
fn a_rebase_drops_the_commit_the_plan_leaves_out() {
    let (root, repo) = linear("drop");
    let mut plan = plan_from(&repo, "one");
    let two = step_index(&plan, "two");
    plan.steps[two].action = RebaseAction::Drop;

    let report = repo.rebase(&plan).unwrap();
    assert_eq!(report.operation, Operation::None);
    assert!(report.conflicts.is_empty());
    assert!(report.stopped.is_none());
    assert_eq!(subjects(&repo), ["three", "one", "base"]);
    assert!(
        !root.join("two.txt").exists(),
        "the file of the drop is gone"
    );
    assert_eq!(read(&root.join("three.txt")), "three\n");
}

#[test]
fn a_rebase_replays_the_reordered_commits_in_the_new_order() {
    let (_root, repo) = linear("reorder");
    let mut plan = plan_from(&repo, "one");
    let last = plan.steps.len() - 1;
    plan.steps.swap(0, last);

    let report = repo.rebase(&plan).unwrap();
    assert_eq!(report.operation, Operation::None);
    assert_eq!(subjects(&repo), ["one", "two", "three", "base"]);
}

#[test]
fn squash_joins_two_commits_and_keeps_both_messages() {
    let (_root, repo) = linear("squash");
    let mut plan = plan_from(&repo, "one");
    let two = step_index(&plan, "two");
    plan.steps[two].action = RebaseAction::Squash;

    let report = repo.rebase(&plan).unwrap();
    assert_eq!(report.operation, Operation::None);
    assert_eq!(subjects(&repo), ["three", "one", "base"]);
    let message = message_of(&repo, "one");
    assert!(message.contains("two"), "the message of both: {message}");
}

#[test]
fn fixup_joins_two_commits_and_keeps_the_message_of_the_first() {
    let (root, repo) = linear("fixup");
    let mut plan = plan_from(&repo, "one");
    let two = step_index(&plan, "two");
    plan.steps[two].action = RebaseAction::Fixup;

    let report = repo.rebase(&plan).unwrap();
    assert_eq!(report.operation, Operation::None);
    assert_eq!(subjects(&repo), ["three", "one", "base"]);
    let message = message_of(&repo, "one");
    assert!(
        !message.contains("two"),
        "the message of the first: {message}"
    );
    assert_eq!(read(&root.join("two.txt")), "two\n");
}

#[test]
fn reword_gives_the_commit_another_message() {
    let (_root, repo) = linear("reword");
    let mut plan = plan_from(&repo, "one");
    let index = step_index(&plan, "two");
    plan.steps[index].action = RebaseAction::Reword;
    plan.steps[index].message = "a better subject\n\nwith a body\n".to_owned();

    let report = repo.rebase(&plan).unwrap();
    assert_eq!(report.operation, Operation::None);
    assert_eq!(
        subjects(&repo),
        ["three", "a better subject", "one", "base"]
    );
    let message = message_of(&repo, "a better subject");
    assert!(message.contains("with a body"), "{message}");
    let log = repo.log(10, 0).unwrap();
    let renamed = log
        .iter()
        .find(|commit| commit.subject == "a better subject")
        .unwrap();
    assert_eq!(renamed.author, "Test", "the author stays");
}

#[test]
fn edit_stops_the_rebase_and_continue_ends_it() {
    let (root, repo) = linear("edit");
    let two = id_of(&repo, "two");
    let mut plan = plan_from(&repo, "one");
    let edit = step_index(&plan, "two");
    plan.steps[edit].action = RebaseAction::Edit;

    let report = repo.rebase(&plan).unwrap();
    assert_eq!(report.operation, Operation::Rebase);
    assert!(report.conflicts.is_empty());
    assert_eq!(report.stopped.as_deref(), Some(two.as_str()));
    assert_eq!(
        subjects(&repo),
        ["two", "one", "base"],
        "the commit of the stop is applied"
    );

    fs::write(root.join("two.txt"), "edited\n").unwrap();
    let request = CommitRequest {
        message: "two edited".into(),
        paths: Vec::new(),
        amend: true,
    };
    repo.commit(&request).unwrap();
    repo.finish("").unwrap();

    assert_eq!(repo.operation().unwrap(), Operation::None);
    assert_eq!(subjects(&repo), ["three", "two edited", "one", "base"]);
    assert_eq!(read(&root.join("two.txt")), "edited\n");
}

#[test]
fn a_conflict_of_a_reorder_is_aborted_and_the_branch_comes_back() {
    let root = temp_dir("conflict");
    init(&root);
    fs::write(root.join("a.txt"), "one\n").unwrap();
    let repo = open(&root);
    commit(&repo, "base");
    fs::write(root.join("a.txt"), "two\n").unwrap();
    commit(&repo, "first change");
    fs::write(root.join("a.txt"), "three\n").unwrap();
    let head = commit(&repo, "second change");

    let mut plan = plan_from(&repo, "first change");
    plan.steps.swap(0, 1);

    let report = repo.rebase(&plan).unwrap();
    assert_eq!(report.operation, Operation::Rebase);
    assert_eq!(report.conflicts, ["a.txt"]);
    assert!(report.stopped.is_none());

    repo.abort().unwrap();
    assert_eq!(repo.operation().unwrap(), Operation::None);
    assert_eq!(id_of(&repo, "second change"), head, "HEAD is where it was");
    assert_eq!(read(&root.join("a.txt")), "three\n");
    assert_eq!(subjects(&repo), ["second change", "first change", "base"]);
}

#[test]
fn a_second_plan_is_refused_while_a_rebase_waits_for_the_user() {
    let (_root, repo) = linear("busy");
    let mut plan = plan_from(&repo, "one");
    plan.steps[1].action = RebaseAction::Edit;
    let report = repo.rebase(&plan).unwrap();
    assert_eq!(report.operation, Operation::Rebase);

    let error = repo.rebase(&plan).unwrap_err();
    assert!(error.to_string().contains("another operation"), "{error}");
    repo.finish("").unwrap();
    assert_eq!(repo.operation().unwrap(), Operation::None);
}

#[test]
fn a_merge_in_the_range_of_the_plan_is_refused() {
    let (root, repo) = linear("merge");
    plain_ok(&root, &["switch", "-q", "-c", "side"]);
    fs::write(root.join("side.txt"), "side\n").unwrap();
    commit(&repo, "side change");
    plain_ok(&root, &["switch", "-q", "main"]);
    plain_ok(&root, &["merge", "-q", "--no-ff", "-m", "a merge", "side"]);

    let error = repo.rebase_plan(&id_of(&repo, "two")).unwrap_err();
    assert!(
        error.to_string().contains("merge commit"),
        "the reason is named: {error}"
    );
}

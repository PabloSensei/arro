//! A whole edit, commit and push cycle on real repositories in a temporary directory.
//! Push goes only to a local bare repository, no server and no credentials take part.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;

use vcs::{Change, CommitRequest, GitRunner, LineChangeKind, Repository, gutter, revert_hunk};

fn git() -> GitRunner {
    GitRunner::from_env().expect("git in PATH")
}

fn temp_dir(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("vcs-{}-{name}", std::process::id()));
    let _ = fs::remove_dir_all(&path);
    fs::create_dir_all(&path).unwrap();
    path
}

/// Plain git, the way a harness or the user in a terminal calls it.
fn plain_git(cwd: &Path, args: &[&str]) -> std::process::Output {
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

fn request(message: &str, paths: &[&str]) -> CommitRequest {
    CommitRequest {
        message: message.into(),
        paths: paths.iter().map(|path| (*path).to_owned()).collect(),
        amend: false,
    }
}

fn apply(text: &str, edit: &vcs::TextEdit) -> String {
    format!(
        "{}{}{}",
        &text[..edit.range.start],
        edit.text,
        &text[edit.range.end..]
    )
}

#[test]
fn edit_status_diff_commit_and_push() {
    let root = temp_dir("cycle");
    let work = root.join("рабочая копия");
    fs::create_dir_all(&work).unwrap();
    let repo = new_repository(&work);
    let file = "src/Главный файл.txt";
    fs::create_dir_all(work.join("src")).unwrap();
    fs::write(work.join(file), "один\nдва\nтри\n").unwrap();

    // A new file is untracked, the first commit takes it.
    let status = repo.status().unwrap();
    assert_eq!(status.head, None);
    assert_eq!(status.files[0].path, file);
    assert_eq!(status.files[0].worktree, Change::Untracked);
    let first = repo.commit(&request("Первый коммит", &[])).unwrap();
    assert!(repo.status().unwrap().files.is_empty());

    // An edit shows in the status and in the gutter.
    let edited = "один\nДВА\nтри\nчетыре\n";
    fs::write(work.join(file), edited).unwrap();
    let status = repo.status().unwrap();
    assert_eq!(status.files.len(), 1);
    assert_eq!(status.files[0].worktree, Change::Modified);
    let mut blobs = repo.blobs().unwrap();
    let diff = repo.file_diff(&mut blobs, file, edited).unwrap();
    assert_eq!(diff.base, "один\nдва\nтри\n");
    let changes = gutter(&diff.hunks);
    let kinds: Vec<_> = changes.iter().map(|change| change.kind).collect();
    assert_eq!(kinds, [LineChangeKind::Modified, LineChangeKind::Added]);

    // Reverting the first hunk leaves only the added line.
    let reverted = apply(edited, &revert_hunk(&diff.base, edited, &diff.hunks[0]));
    assert_eq!(reverted, "один\nдва\nтри\nчетыре\n");
    fs::write(work.join(file), &reverted).unwrap();

    // Only the chosen file goes into the commit.
    fs::write(work.join("other.txt"), "x\n").unwrap();
    let second = repo.commit(&request("Второй: четыре", &[file])).unwrap();
    let status = repo.status().unwrap();
    let left: Vec<_> = status.files.iter().map(|file| file.path.as_str()).collect();
    assert_eq!(left, ["other.txt"]);
    let log = repo.log(10, 0).unwrap();
    let subjects: Vec<_> = log.iter().map(|commit| commit.subject.as_str()).collect();
    assert_eq!(subjects, ["Второй: четыре", "Первый коммит"]);
    assert_eq!(log[0].id, second);
    assert_eq!(log[0].parents, [first]);
    assert_eq!(log[0].author, "Тест");
    assert_eq!(repo.log(10, 1).unwrap().len(), 1);

    // The first push sets the upstream, the next one uses it.
    let bare = root.join("remote.git");
    plain_ok(
        &root,
        &["init", "-q", "--bare", "-b", "main", bare.to_str().unwrap()],
    );
    plain_ok(&work, &["remote", "add", "origin", bare.to_str().unwrap()]);
    repo.push().unwrap();
    assert_eq!(
        repo.status().unwrap().upstream.as_deref(),
        Some("origin/main")
    );
    let third = repo.commit(&request("Третий", &["other.txt"])).unwrap();
    repo.push().unwrap();
    let remote_head = plain_ok(&bare, &["rev-parse", "main"]);
    assert_eq!(remote_head.trim(), third);

    // Someone else pushes, Ctrl+T brings it in.
    let clone = root.join("clone");
    plain_ok(
        &root,
        &[
            "clone",
            "-q",
            bare.to_str().unwrap(),
            clone.to_str().unwrap(),
        ],
    );
    plain_ok(
        &clone,
        &[
            "-c",
            "user.name=B",
            "-c",
            "user.email=b@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "from the clone",
        ],
    );
    plain_ok(&clone, &["push", "-q"]);
    repo.update().unwrap();
    assert_eq!(repo.log(1, 0).unwrap()[0].subject, "from the clone");
    let _ = fs::remove_dir_all(root);
}

#[test]
fn branches_are_listed_created_switched_and_deleted() {
    let root = temp_dir("branches");
    let repo = new_repository(&root);
    fs::write(root.join("a.txt"), "a\n").unwrap();
    repo.commit(&request("start", &[])).unwrap();

    repo.create_branch("фича", "HEAD").unwrap();
    let branches = repo.branches().unwrap();
    let current: Vec<_> = branches.iter().filter(|branch| branch.current).collect();
    assert_eq!(current.len(), 1);
    assert_eq!(current[0].name, "фича");
    assert_eq!(branches.len(), 2);

    repo.switch("main").unwrap();
    assert_eq!(repo.status().unwrap().branch.as_deref(), Some("main"));
    repo.delete_branch("фича").unwrap();
    assert_eq!(repo.branches().unwrap().len(), 1);
    let _ = fs::remove_dir_all(root);
}

#[test]
fn an_empty_repository_has_an_empty_log() {
    let root = temp_dir("empty");
    let repo = new_repository(&root);

    assert!(repo.log(10, 0).unwrap().is_empty());
    let _ = fs::remove_dir_all(root);
}

#[test]
fn a_bad_commit_reports_what_git_said() {
    let root = temp_dir("nothing");
    let repo = new_repository(&root);
    fs::write(root.join("a.txt"), "a\n").unwrap();
    repo.commit(&request("start", &[])).unwrap();

    let error = repo.commit(&request("nothing to commit", &[])).unwrap_err();

    assert!(error.to_string().starts_with("git:"), "{error}");
    let _ = fs::remove_dir_all(root);
}

/// Our status and diff run while a harness commits in the same repository. Ours must not
/// take `index.lock` from under it.
#[test]
fn a_harness_running_git_at_the_same_time_sees_no_lock_errors() {
    let root = temp_dir("parallel");
    let repo = Arc::new(new_repository(&root));
    fs::write(root.join("ours.txt"), "a\n").unwrap();
    repo.commit(&request("start", &[])).unwrap();
    // A stale index makes a plain status want to refresh it, and so to take the lock.
    fs::write(root.join("ours.txt"), "b\n").unwrap();

    let done = Arc::new(AtomicBool::new(false));
    let reader = {
        let (repo, done) = (repo.clone(), done.clone());
        thread::spawn(move || {
            let mut blobs = repo.blobs().unwrap();
            let mut calls = 0;
            while !done.load(Ordering::Relaxed) {
                repo.status().unwrap();
                repo.file_diff(&mut blobs, "ours.txt", "b\n").unwrap();
                calls += 1;
            }
            calls
        })
    };
    let mut failures = Vec::new();
    for round in 0..40 {
        let name = format!("harness-{round}.txt");
        fs::write(root.join(&name), "h\n").unwrap();
        for args in [
            vec!["add", name.as_str()],
            vec!["commit", "-q", "-m", "harness"],
        ] {
            let output = plain_git(&root, &args);
            if !output.status.success() {
                failures.push(String::from_utf8_lossy(&output.stderr).into_owned());
            }
        }
    }
    done.store(true, Ordering::Relaxed);
    let calls = reader.join().unwrap();

    assert!(failures.is_empty(), "{failures:?}");
    assert!(calls > 10, "only {calls} reads ran alongside");
    let _ = fs::remove_dir_all(root);
}

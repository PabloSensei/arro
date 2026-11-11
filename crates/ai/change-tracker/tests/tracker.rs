//! Real `git` on real files.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread::sleep;
use std::time::{Duration, Instant};

use agent_core::AgentEvent;
use change_tracker::{ChangeKind, ChangeSet, ChangeTracker, GitRunner, TrackerState};

const WAIT: Duration = Duration::from_secs(30);

struct Fixture {
    root: PathBuf,
    project: PathBuf,
    tracker: ChangeTracker,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let root = std::env::temp_dir().join(format!("ide-tracker-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&root);
        let project = root.join("project");
        fs::create_dir_all(project.join("src")).expect("project");
        fs::write(project.join(".gitignore"), "output/\n").expect("ignore");
        fs::write(project.join("src/kept.txt"), "same\n").expect("file");
        fs::write(project.join("src/edited.txt"), "before\n").expect("file");
        fs::write(project.join("src/deleted.txt"), "before\n").expect("file");
        let path = std::env::var_os("PATH").unwrap_or_default();
        let git = GitRunner::find(&path).expect("git on PATH");
        let tracker = ChangeTracker::open(git, &root.join("state"), &project).expect("open");
        wait_for(&tracker, |changes| changes.state == TrackerState::Ready);
        Self {
            root,
            project,
            tracker,
        }
    }

    fn write(&self, path: &str, text: &str) {
        let file = self.project.join(path);
        fs::create_dir_all(file.parent().expect("parent")).expect("directory");
        fs::write(file, text).expect("file");
    }

    fn run_turn(&self, edit: impl FnOnce(&Path)) -> std::sync::Arc<ChangeSet> {
        let before = self.tracker.changes().version;
        self.tracker.observe(&AgentEvent::TurnStarted);
        wait_for(&self.tracker, |changes| changes.version > before);
        edit(&self.project);
        let started = self.tracker.changes().version;
        self.tracker.observe(&AgentEvent::TurnEnded);
        wait_for(&self.tracker, |changes| changes.version > started)
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn wait_for(
    tracker: &ChangeTracker,
    check: impl Fn(&ChangeSet) -> bool,
) -> std::sync::Arc<ChangeSet> {
    let deadline = Instant::now() + WAIT;
    loop {
        let changes = tracker.changes();
        if check(&changes) {
            return changes;
        }
        assert!(Instant::now() < deadline, "{changes:?}");
        sleep(Duration::from_millis(20));
    }
}

fn entry(changes: &[change_tracker::ChangedFile], path: &str) -> Option<ChangeKind> {
    let found = changes.iter().find(|change| change.path == path);
    found.map(|change| change.kind)
}

#[test]
fn an_early_idle_is_taken_later_and_not_lost() {
    let fixture = Fixture::new("idle");
    fixture.tracker.observe(&AgentEvent::Idle);
    fixture.write("src/typed.txt", "by hand\n");

    // Comes right after the first snapshot, within the gap of checkpoints.
    fixture.tracker.observe(&AgentEvent::Idle);

    let changes = wait_for(&fixture.tracker, |changes| {
        entry(&changes.session, "src/typed.txt").is_some()
    });
    assert_eq!(
        entry(&changes.session, "src/typed.txt"),
        Some(ChangeKind::Added)
    );
}

#[test]
fn edits_of_a_turn_are_listed_by_kind() {
    let fixture = Fixture::new("kinds");

    let changes = fixture.run_turn(|project| {
        fs::write(project.join("src/edited.txt"), "after\n").expect("edit");
        fs::write(project.join("src/added.txt"), "new\n").expect("add");
        fs::remove_file(project.join("src/deleted.txt")).expect("delete");
    });

    let files = &changes.session;
    assert_eq!(entry(files, "src/edited.txt"), Some(ChangeKind::Modified));
    assert_eq!(entry(files, "src/added.txt"), Some(ChangeKind::Added));
    assert_eq!(entry(files, "src/deleted.txt"), Some(ChangeKind::Deleted));
    assert_eq!(entry(files, "src/kept.txt"), None);
}

#[test]
fn ignored_files_stay_out() {
    let fixture = Fixture::new("ignored");

    let changes = fixture.run_turn(|project| {
        fs::create_dir_all(project.join("output")).expect("directory");
        fs::write(project.join("output/generated.bin"), "x").expect("file");
    });

    assert!(changes.session.is_empty(), "{changes:?}");
}

#[test]
fn a_turn_lists_its_own_files_and_the_session_all_of_them() {
    let fixture = Fixture::new("turns");
    fixture.run_turn(|project| {
        fs::write(project.join("one.txt"), "1").expect("file");
    });

    let changes = fixture.run_turn(|project| {
        fs::write(project.join("two.txt"), "2").expect("file");
    });

    assert_eq!(entry(&changes.turn, "one.txt"), None);
    assert_eq!(entry(&changes.turn, "two.txt"), Some(ChangeKind::Added));
    assert_eq!(entry(&changes.session, "one.txt"), Some(ChangeKind::Added));
    assert_eq!(entry(&changes.session, "two.txt"), Some(ChangeKind::Added));
}

#[test]
fn the_repository_of_the_user_stays_untouched() {
    let fixture = Fixture::new("user-repo");
    assert!(!fixture.project.join(".git").exists());

    fixture.write("a.txt", "a");
    fixture.run_turn(|_| {});

    assert!(!fixture.project.join(".git").exists());
}

#[test]
fn a_missing_git_is_reported_and_not_hidden() {
    let root = std::env::temp_dir().join(format!("ide-tracker-nogit-{}", std::process::id()));
    fs::create_dir_all(&root).expect("root");
    let git = GitRunner::new(root.join("no-such-git.exe"));

    let tracker = ChangeTracker::open(git, &root.join("state"), &root).expect("open");

    wait_for(&tracker, |changes| {
        matches!(changes.state, TrackerState::Failed(_))
    });
    let _ = fs::remove_dir_all(&root);
}

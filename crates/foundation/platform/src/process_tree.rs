//! The programs below a process, and how to end them.
//!
//! A job holds what the process starts, but not everything: a shell from the Store, a
//! program that breaks away from a job, or a service that starts a child on behalf of
//! another one leave programs outside it. Such a program keeps the id of its parent in
//! the snapshot, so the tree is walked by parent ids rather than by a job.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use crate::job::Job;
use crate::process_stats::{is_console_process, process_created, process_table, terminate_process};

/// Rounds of the cleanup, and the pause between them. A program started in the moment the
/// root died is found by the next round.
const CLEANUP_ROUNDS: usize = 5;
const CLEANUP_PAUSE: Duration = Duration::from_millis(50);

/// Which programs of the tree a cleanup ends.
#[derive(Clone, Copy)]
enum Cleanup {
    /// Every program the tree has found, for a job that goes away.
    All,
    /// Only the programs that were outside the job when the tree found them: a program
    /// inside the job goes with it, and may still be wanted.
    Escaped,
}

impl Cleanup {
    fn wants(self, process: &TrackedProcess) -> bool {
        match self {
            Self::All => true,
            Self::Escaped => process.outside,
        }
    }
}

/// The tree of one process: found by walking the snapshot from the root and from every
/// program found before, and kept up to date by [`ProcessTree::adopt`].
///
/// A clone is a handle to the same tree: a cleanup that continues on its own thread needs
/// the programs the caller found, and the caller may drop its handle at once.
#[derive(Clone)]
pub struct ProcessTree {
    root: u32,
    /// When the root was created. A later process that took its id is not a child.
    started: Option<u64>,
    /// The programs the tree has found, in the order they were found.
    tracked: Arc<Mutex<Vec<TrackedProcess>>>,
    /// The pause between two rounds of the cleanup.
    pause: Duration,
}

/// A program of the tree. The creation time tells a reused id apart from the process that
/// was seen first; `None` means the process was gone before it was read.
#[derive(Clone, Copy)]
struct TrackedProcess {
    pid: u32,
    created: Option<u64>,
    /// Whether the program was outside `job` when the tree found it. Such a program does
    /// not go with the job and needs the cleanup rounds of its own.
    outside: bool,
}

impl ProcessTree {
    /// The tree of `root`, read now: a root that has already exited still leaves its id as
    /// the parent of the programs it started.
    pub fn new(root: u32) -> Self {
        Self {
            root,
            started: process_created(root),
            tracked: Arc::new(Mutex::new(Vec::new())),
            pause: CLEANUP_PAUSE,
        }
    }

    /// The pause between the cleanup rounds. The tests make it long, so that a caller that
    /// slept on its own thread is seen without a threshold that depends on the machine.
    pub fn with_pause(mut self, pause: Duration) -> Self {
        self.pause = pause;
        self
    }

    /// Finds the console programs below the root and pulls them into `job`. Window
    /// programs are left alone, and so is everything they start: an editor or a browser
    /// the user launched does not belong to the tree and must not die with it, and neither
    /// does an image whose subsystem cannot be read. Returns how many programs were added.
    pub fn adopt(&self, job: &Job) -> usize {
        let entries = process_table();
        // What the job holds now, from one query: a failed query leaves every program
        // counted as one of the job, which is the safe side.
        let in_job = job.process_ids().ok();
        let mut below: HashMap<u32, Vec<u32>> = HashMap::new();
        let mut live: HashSet<u32> = HashSet::new();
        for entry in &entries {
            live.insert(entry.pid);
            if let Some(parent) = entry.parent {
                below.entry(parent).or_default().push(entry.pid);
            }
        }
        let mut created: HashMap<u32, Option<u64>> = HashMap::new();
        let mut tracked = self
            .tracked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        // A walk starts from the root and from every program already found. The id of a
        // dead parent stays in the snapshot, so orphans of the root are still reachable.
        let mut pending = Vec::new();
        if let Some(started) = self.started {
            match creation_of(self.root, &mut created) {
                Some(actual) if actual == started => pending.push(self.root),
                Some(_) => {}
                // Gone from the snapshot: the id is only a parent link now.
                None if !live.contains(&self.root) => pending.push(self.root),
                None => {}
            }
        } else {
            pending.push(self.root);
        }
        for process in tracked.iter_mut() {
            if process.created.is_none() {
                process.created = creation_of(process.pid, &mut created);
            }
            let ours = match creation_of(process.pid, &mut created) {
                Some(actual) => process.created == Some(actual),
                // Gone: its children may still name it as their parent.
                None => !live.contains(&process.pid),
            };
            if ours {
                pending.push(process.pid);
            }
        }
        let mut seen: HashSet<u32> = tracked.iter().map(|process| process.pid).collect();
        seen.insert(self.root);
        let mut added = 0;
        while let Some(parent) = pending.pop() {
            let Some(children) = below.get(&parent) else {
                continue;
            };
            for &pid in children {
                if !seen.insert(pid) {
                    continue;
                }
                let created = creation_of(pid, &mut created);
                // A program of the tree is never older than the root it belongs to.
                if let (Some(started), Some(created)) = (self.started, created)
                    && created < started
                {
                    continue;
                }
                // Only the console tree goes with the root. A window program and
                // everything below it stay alive, and so does an image whose subsystem
                // cannot be read: nothing of someone else's is ended on a guess.
                if is_console_process(pid) != Some(true) {
                    continue;
                }
                let outside = match &in_job {
                    Some(ids) => !ids.contains(&pid),
                    None => false,
                };
                let _ = job.assign_id(pid);
                tracked.push(TrackedProcess {
                    pid,
                    created,
                    outside,
                });
                pending.push(pid);
                added += 1;
            }
        }
        added
    }

    /// Ends every program of the tree. One round runs here, without a pause; the rounds
    /// that a program found by it may still need run on a thread of their own, because the
    /// caller may be the window thread or a poll loop. `job` is held by that thread until
    /// it finishes, so the programs assigned to it do not go early.
    pub fn kill(&self, job: &Arc<Job>) {
        self.clean_now(job, Cleanup::All);
    }

    /// Ends only the programs that were outside the job when the tree found them: a
    /// program inside the job goes with it, and may still be wanted after its parent.
    /// The rounds left over run on a thread of their own, as in [`Self::kill`].
    ///
    /// Without the creation time of the root an id may name another process, and the tree
    /// of that one is not ours to end, so nothing is done then.
    pub fn kill_escaped(&self, job: &Arc<Job>) {
        if self.started.is_none() {
            return;
        }
        self.clean_now(job, Cleanup::Escaped);
    }

    /// Ends every program of the tree, all rounds on this thread. For a caller that waits
    /// for the process anyway, such as the thread of a terminal session.
    pub fn kill_blocking(&self, job: &Job) {
        self.clean_rounds(job, Cleanup::All, 0);
    }

    /// The one round a caller does on its own thread, and the rest on a reaper thread when
    /// the round found a program that may have started another.
    fn clean_now(&self, job: &Arc<Job>, cleanup: Cleanup) {
        if self.round(job, cleanup) == 0 {
            return;
        }
        let background = self.clone();
        let background_job = Arc::clone(job);
        let spawned = thread::Builder::new()
            .name("proc-tree-reap".to_string())
            .spawn(move || background.clean_rounds(&background_job, cleanup, 1));
        if spawned.is_err() {
            // No thread to hand the rounds to: they run here, as they did before.
            self.clean_rounds(job, cleanup, 1);
        }
    }

    fn clean_rounds(&self, job: &Job, cleanup: Cleanup, first: usize) {
        for round in first..CLEANUP_ROUNDS {
            // The first round of a caller has run already, and never pauses.
            if round > 0 {
                thread::sleep(self.pause);
            }
            let added = self.round(job, cleanup);
            // A round without a new id means no parent is left that could start another.
            if added == 0 && round > 0 {
                return;
            }
        }
    }

    /// One walk and the end of what it wants: the immediate part of a cleanup. Returns how
    /// many programs the walk added to the tree.
    fn round(&self, job: &Job, cleanup: Cleanup) -> usize {
        let added = self.adopt(job);
        let known: Vec<TrackedProcess> = self
            .tracked
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .iter()
            .copied()
            .filter(|process| cleanup.wants(process))
            .collect();
        for process in known {
            if let Some(created) = process.created {
                let _ = terminate_process(process.pid, created);
            }
        }
        added
    }

    /// A tree with programs in it and a root that no walk can reach. For tests.
    #[cfg(test)]
    fn seeded(root: u32, started: Option<u64>, found: Vec<TrackedProcess>) -> Self {
        Self {
            root,
            started,
            tracked: Arc::new(Mutex::new(found)),
            pause: CLEANUP_PAUSE,
        }
    }
}

/// The creation time of `pid`, read once per snapshot.
fn creation_of(pid: u32, cache: &mut HashMap<u32, Option<u64>>) -> Option<u64> {
    *cache.entry(pid).or_insert_with(|| process_created(pid))
}

#[cfg(test)]
mod tests {
    use super::{ProcessTree, TrackedProcess};
    use crate::job::Job;
    use crate::process_stats::{process_created, process_table};
    use std::process::{Child, Command, Stdio};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    const PATIENCE: Duration = Duration::from_secs(10);

    /// A console program of a test: ended by the test even when it fails before its last
    /// line.
    struct Run(Child);

    impl Run {
        fn start(program: &str, arguments: &[&str]) -> Self {
            let child = Command::new(program)
                .args(arguments)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap_or_else(|error| panic!("start {program}: {error}"));
            Self(child)
        }

        fn pid(&self) -> u32 {
            self.0.id()
        }
    }

    impl Drop for Run {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    fn wait_until(mut condition: impl FnMut() -> bool) {
        let started = Instant::now();
        while !condition() {
            assert!(started.elapsed() < PATIENCE, "condition not met in time");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    /// A console program under a shell, so that a walk from the shell finds it.
    fn shell_with_a_child() -> (Run, u32) {
        let shell = Run::start("cmd.exe", &["/d", "/c", "ping -t 127.0.0.1"]);
        let shell_pid = shell.pid();
        let started = Instant::now();
        let child = loop {
            let child = process_table()
                .into_iter()
                .find(|process| {
                    process.parent == Some(shell_pid)
                        && process.name.eq_ignore_ascii_case("PING.EXE")
                })
                .map(|process| process.pid);
            if let Some(child) = child {
                break child;
            }
            assert!(started.elapsed() < PATIENCE, "the shell started no program");
            std::thread::sleep(Duration::from_millis(10));
        };
        (shell, child)
    }

    #[test]
    fn a_walk_finds_the_console_program_of_a_shell_and_ends_it() {
        let (shell, program) = shell_with_a_child();
        let tree = ProcessTree::new(shell.pid());
        let job = Arc::new(Job::new().expect("a job"));

        assert_eq!(tree.adopt(&job), 1, "the program of the shell is found");
        assert_eq!(job.process_ids().expect("the job"), [program]);

        tree.kill(&job);

        wait_until(|| !process_table().iter().any(|process| process.pid == program));
        // The shell is ended by the test itself: `kill` ends the programs of the tree, and
        // the root belongs to the caller of the walk.
        drop(shell);
    }

    /// A tree with programs in it whose root no walk can reach: the id belongs to no
    /// process, so only the seeded programs are seen.
    fn seeded(found: Vec<(u32, Option<u64>, bool)>) -> ProcessTree {
        let found = found
            .into_iter()
            .map(|(pid, created, outside)| TrackedProcess {
                pid,
                created,
                outside,
            })
            .collect();
        ProcessTree::seeded(0, Some(1), found)
    }

    #[test]
    fn an_id_that_names_another_process_is_left_alone() {
        let mut program = Run::start("ping.exe", &["-t", "127.0.0.1"]);
        let created = process_created(program.pid()).expect("the creation time");
        let job = Arc::new(Job::new().expect("a job"));

        // The tree remembers the id with a creation time of its own: the process that
        // took the id later is not the one the tree found, and is not its to end.
        seeded(vec![(program.pid(), Some(created + 1), true)]).kill_escaped(&job);
        assert!(
            program.0.try_wait().expect("state").is_none(),
            "a process with a creation time of another process was ended"
        );

        seeded(vec![(program.pid(), Some(created), true)]).kill_escaped(&job);
        wait_until(|| program.0.try_wait().expect("state").is_some());
        assert_eq!(
            program
                .0
                .try_wait()
                .expect("state")
                .and_then(|status| status.code()),
            Some(1),
            "the process the tree found was not ended"
        );
    }

    #[test]
    fn a_program_of_the_job_waits_for_the_job() {
        let mut program = Run::start("ping.exe", &["-t", "127.0.0.1"]);
        let created = process_created(program.pid()).expect("the creation time");

        let escaped = seeded(vec![(program.pid(), Some(created), false)]);
        escaped.kill_escaped(&Arc::new(Job::new().expect("a job")));
        assert!(
            program.0.try_wait().expect("state").is_none(),
            "a program that is in the job was ended before the job"
        );

        escaped.kill(&Arc::new(Job::new().expect("a job")));
        wait_until(|| program.0.try_wait().expect("state").is_some());
        assert_eq!(
            program
                .0
                .try_wait()
                .expect("state")
                .and_then(|status| status.code()),
            Some(1)
        );
    }
}

//! The journal of the git commands the IDE ran: the time, the working directory, the
//! arguments, the exit code, the duration and the output of each of them, with the
//! passwords and the tokens replaced by [`HIDDEN`]. Every working directory has a journal
//! of its own, so the commands of one project never push the commands of another out;
//! a journal keeps the last [`COMMAND_LIMIT`] commands in memory and knows nothing of the
//! window.

use std::collections::{HashMap, VecDeque};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// The commands one journal keeps. The older ones go first.
pub const COMMAND_LIMIT: usize = 500;
/// The journals the console keeps: one per working directory. A session that visits many
/// repositories drops the journal that ran a command longest ago, so the projects that
/// are in use keep theirs.
const JOURNAL_LIMIT: usize = 128;
/// What stands in place of a password or a token.
pub const HIDDEN: &str = "***";
/// The most output one command keeps. A long log is not what the Console is read for, so
/// a stream above this is cut and says so, and the whole journal stays small.
const OUTPUT_LIMIT: usize = 64 * 1024;
/// The most output a command of the IDE itself keeps. Such commands run all the time and
/// mostly answer with a long list, and the Console reads them only to see what failed.
const BACKGROUND_OUTPUT_LIMIT: usize = 4 * 1024;

/// The commands the IDE runs on its own to keep the window up to date. They go to the
/// journal like the rest, but the Console hides them unless the user asks for them.
const BACKGROUND: &[&str] = &[
    "blame",
    "cat-file",
    "check-ignore",
    "config",
    "describe",
    "diff",
    "for-each-ref",
    "hash-object",
    "log",
    "ls-files",
    "ls-tree",
    "merge-base",
    "rev-list",
    "rev-parse",
    "show",
    "show-ref",
    "status",
    "symbolic-ref",
];

/// One git command that ran, as the Console tab shows it.
#[derive(Clone, Debug)]
pub struct CommandRecord {
    /// The number the journal gave the command: unique and growing with every command.
    pub id: u64,
    /// The start of the command, in milliseconds since the Unix epoch.
    pub time: u64,
    /// The working directory of the command.
    pub directory: PathBuf,
    /// The arguments after `git`, with the secrets replaced by [`HIDDEN`].
    pub args: Vec<String>,
    /// The exit code, `None` when git did not start.
    pub code: Option<i32>,
    pub duration: Duration,
    pub stdout: String,
    pub stderr: String,
    /// A command the IDE runs by itself, such as `status` or `rev-parse`.
    pub background: bool,
}

impl CommandRecord {
    /// The record of one command. `code`, `stdout` and `stderr` are what git answered, or
    /// `None` and the error when git did not start.
    pub fn new<S: AsRef<OsStr>>(
        directory: &Path,
        args: &[S],
        code: Option<i32>,
        duration: Duration,
        stdout: &[u8],
        stderr: &[u8],
    ) -> Self {
        let args: Vec<String> = args
            .iter()
            .map(|arg| hidden_argument(&arg.as_ref().to_string_lossy()))
            .collect();
        let background = is_background(&args);
        let limit = if background {
            BACKGROUND_OUTPUT_LIMIT
        } else {
            OUTPUT_LIMIT
        };
        Self {
            id: 0,
            time: now_millis(),
            directory: directory.to_path_buf(),
            background,
            args,
            code,
            duration,
            stdout: clipped(stdout, limit),
            stderr: clipped(stderr, limit),
        }
    }

    /// The command as the Console shows it: `git commit -m "first commit"`.
    pub fn line(&self) -> String {
        let mut line = String::from("git");
        for arg in &self.args {
            line.push(' ');
            if arg.is_empty() || arg.contains(' ') {
                line.push('"');
                line.push_str(arg);
                line.push('"');
            } else {
                line.push_str(arg);
            }
        }
        line
    }

    /// A command that failed, or that did not start at all.
    pub fn failed(&self) -> bool {
        self.code != Some(0)
    }

    /// The command, its exit code and its output, as Copy puts it on the clipboard.
    pub fn text(&self) -> String {
        let code = match self.code {
            Some(code) => format!("exit {code}"),
            None => "not started".to_owned(),
        };
        let mut text = format!(
            "{}\n{code} · {} ms\n",
            self.line(),
            self.duration.as_millis()
        );
        for stream in [&self.stdout, &self.stderr] {
            if stream.is_empty() {
                continue;
            }
            text.push_str(stream);
            if !stream.ends_with('\n') {
                text.push('\n');
            }
        }
        text
    }
}

/// The journal of the last git commands of the process, one journal per working directory.
pub struct CommandConsole {
    journals: Mutex<HashMap<PathBuf, VecDeque<CommandRecord>>>,
    revision: AtomicU64,
}

impl Default for CommandConsole {
    fn default() -> Self {
        Self::new()
    }
}

impl CommandConsole {
    pub fn new() -> Self {
        Self {
            journals: Mutex::new(HashMap::new()),
            revision: AtomicU64::new(0),
        }
    }

    /// Adds a command to the journal of its working directory, dropping the oldest one
    /// when that journal is full, and answers with the number the command got.
    pub fn record(&self, mut record: CommandRecord) -> u64 {
        record.id = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let id = record.id;
        let directory = record.directory.clone();
        let mut journals = self.lock();
        if journals.len() >= JOURNAL_LIMIT && !journals.contains_key(&directory) {
            forget_oldest(&mut journals);
        }
        let entries = journals.entry(directory).or_default();
        if entries.len() == COMMAND_LIMIT {
            entries.pop_front();
        }
        entries.push_back(record);
        id
    }

    /// The commands of every journal, oldest first.
    pub fn entries(&self) -> Vec<CommandRecord> {
        let journals = self.lock();
        let mut entries: Vec<CommandRecord> = journals.values().flatten().cloned().collect();
        entries.sort_by_key(|entry| entry.id);
        entries
    }

    /// The commands of the directories inside `root`: the repository itself and its
    /// submodules, oldest first. The commands of every other project stay out.
    pub fn entries_under(&self, root: &Path) -> Vec<CommandRecord> {
        let journals = self.lock();
        let mut entries: Vec<CommandRecord> = journals
            .iter()
            .filter(|(directory, _)| directory.starts_with(root))
            .flat_map(|(_, entries)| entries.iter().cloned())
            .collect();
        entries.sort_by_key(|entry| entry.id);
        entries
    }

    /// Grows with every recorded command and every clear, so a view knows when to read
    /// the journal again.
    pub fn revision(&self) -> u64 {
        self.revision.load(Ordering::SeqCst)
    }

    /// Forgets every command.
    pub fn clear(&self) {
        self.lock().clear();
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    /// Forgets the commands of the directories inside `root`, and leaves the commands of
    /// every other project alone.
    pub fn clear_under(&self, root: &Path) {
        self.lock()
            .retain(|directory, _| !directory.starts_with(root));
        self.revision.fetch_add(1, Ordering::SeqCst);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<PathBuf, VecDeque<CommandRecord>>> {
        // A view that panicked while reading the journal must not take the journal with
        // it: the commands of the rest of the window are still worth keeping.
        self.journals
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// Drops the journal of the directory that ran a command longest ago.
fn forget_oldest(journals: &mut HashMap<PathBuf, VecDeque<CommandRecord>>) {
    let oldest = journals
        .iter()
        .filter_map(|(directory, entries)| {
            entries.back().map(|entry| (entry.id, directory.clone()))
        })
        .min_by_key(|(id, _)| *id)
        .map(|(_, directory)| directory);
    if let Some(directory) = oldest {
        journals.remove(&directory);
    }
}

/// The journal of the whole process.
pub fn command_console() -> &'static CommandConsole {
    static CONSOLE: OnceLock<CommandConsole> = OnceLock::new();
    CONSOLE.get_or_init(CommandConsole::new)
}

/// Whether `args` are a command of the IDE itself, such as `status` or `rev-parse`. The
/// global options git gets before the subcommand are skipped.
pub fn is_background<S: AsRef<str>>(args: &[S]) -> bool {
    let mut words = args.iter().map(AsRef::as_ref);
    loop {
        match words.next() {
            // `-c name=value` and the other options that take a value of their own.
            Some("-c") | Some("-C") => {
                words.next();
            }
            Some(word) if word.starts_with('-') => {}
            Some(command) => return BACKGROUND.contains(&command),
            None => return true,
        }
    }
}

/// One argument as the journal keeps it: the password and the token of a URL and the
/// value of an option that carries a secret are replaced by [`HIDDEN`].
pub fn hidden_argument(argument: &str) -> String {
    hide_option_value(hide_url_credentials(argument))
}

/// `https://user:token@host/repo` with the whole user information hidden.
fn hide_url_credentials(argument: &str) -> String {
    let Some(scheme) = argument.find("://") else {
        return argument.to_owned();
    };
    let rest = &argument[scheme + 3..];
    let Some(at) = rest.find('@') else {
        return argument.to_owned();
    };
    // A name after the path is no user information: `https://host/a@b` has no secret.
    let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    if at > host_end {
        return argument.to_owned();
    }
    format!("{}{HIDDEN}@{}", &argument[..scheme + 3], &rest[at + 1..])
}

/// `--password=hunter2` with the value hidden; an option that names no secret stays. The
/// name and the value are both read, as `-c http.extraHeader=Authorization: ...` carries
/// the word of the secret on the right of the `=`.
fn hide_option_value(argument: String) -> String {
    let Some((name, value)) = argument.split_once('=') else {
        return argument;
    };
    let secret = [name, value].iter().any(|part| {
        let part = part.to_ascii_lowercase();
        [
            "password",
            "passwd",
            "token",
            "secret",
            "api-key",
            "apikey",
            "authorization",
        ]
        .iter()
        .any(|word| part.contains(word))
    });
    if secret {
        format!("{name}={HIDDEN}")
    } else {
        argument
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis() as u64)
}

/// The output kept for one command, cut at a character boundary when it is too long. The
/// bytes are cut before the conversion, so a long stream is never copied whole.
fn clipped(bytes: &[u8], limit: usize) -> String {
    if bytes.len() <= limit {
        return String::from_utf8_lossy(bytes).into_owned();
    }
    let mut cut = limit;
    while cut > 0 && bytes[cut] & 0b1100_0000 == 0b1000_0000 {
        cut -= 1;
    }
    let mut text = String::from_utf8_lossy(&bytes[..cut]).into_owned();
    text.push_str("\n… output cut");
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(console: &CommandConsole, command: &str) {
        console.record(CommandRecord::new(
            Path::new("work"),
            &[command],
            Some(0),
            Duration::from_millis(3),
            b"",
            b"",
        ));
    }

    fn record_in(console: &CommandConsole, directory: &str, command: &str) {
        console.record(CommandRecord::new(
            Path::new(directory),
            &[command],
            Some(0),
            Duration::from_millis(3),
            b"",
            b"",
        ));
    }

    #[test]
    fn a_command_keeps_its_code_its_duration_and_its_output() {
        let console = CommandConsole::new();
        console.record(CommandRecord::new(
            Path::new("work"),
            &["commit", "-m", "first"],
            Some(1),
            Duration::from_millis(12),
            b"out\n",
            b"fatal: nothing to commit\n",
        ));

        let entries = console.entries();
        assert_eq!(entries.len(), 1, "the journal has the command");
        let entry = &entries[0];
        assert_eq!(entry.line(), "git commit -m first");
        assert_eq!(entry.directory, PathBuf::from("work"));
        assert_eq!(entry.code, Some(1));
        assert_eq!(entry.duration, Duration::from_millis(12));
        assert_eq!(entry.stdout, "out\n");
        assert_eq!(entry.stderr, "fatal: nothing to commit\n");
        assert!(entry.time > 0, "the command has the time it started");
        assert!(entry.id > 0, "the journal numbers the command");
        assert!(entry.failed(), "a non-zero code is a failure");
        assert!(entry.text().contains("fatal: nothing to commit"));
    }

    #[test]
    fn a_quiet_command_is_no_failure() {
        let console = CommandConsole::new();
        console.record(CommandRecord::new(
            Path::new("work"),
            &["push"],
            Some(0),
            Duration::from_millis(1),
            b"Everything up-to-date\n",
            b"",
        ));

        assert!(!console.entries()[0].failed());
        assert!(console.entries()[0].text().contains("exit 0"));
    }

    #[test]
    fn a_password_in_a_url_is_hidden() {
        let entry = CommandRecord::new(
            Path::new("work"),
            &["clone", "https://user:s3cret@example.com/repo.git"],
            Some(0),
            Duration::ZERO,
            b"",
            b"",
        );

        assert_eq!(entry.args[1], "https://***@example.com/repo.git");
        assert!(
            !entry.line().contains("s3cret"),
            "the line of the console keeps no password: {}",
            entry.line()
        );
        assert_eq!(
            hidden_argument("https://ghp_token@example.com/repo.git"),
            "https://***@example.com/repo.git"
        );
        assert_eq!(hidden_argument("--password=hunter2"), "--password=***");
        assert_eq!(
            hidden_argument("http.extraHeader=Authorization: Basic dXNlcg=="),
            "http.extraHeader=***"
        );
    }

    #[test]
    fn a_url_without_a_secret_and_a_name_is_kept() {
        assert_eq!(
            hidden_argument("https://example.com/repo.git"),
            "https://example.com/repo.git"
        );
        assert_eq!(
            hidden_argument("https://example.com/a@b"),
            "https://example.com/a@b"
        );
        assert_eq!(
            hidden_argument("git@example.com:repo.git"),
            "git@example.com:repo.git"
        );
        assert_eq!(hidden_argument("--depth=1"), "--depth=1");
    }

    #[test]
    fn the_journal_keeps_the_last_five_hundred_commands() {
        let console = CommandConsole::new();
        for index in 0..COMMAND_LIMIT + 2 {
            record(&console, &format!("command-{index}"));
        }

        let entries = console.entries();
        assert_eq!(entries.len(), COMMAND_LIMIT, "the journal is limited");
        assert_eq!(entries[0].args, ["command-2"], "the oldest commands go");
        assert_eq!(
            entries[entries.len() - 1].args,
            [format!("command-{}", COMMAND_LIMIT + 1)]
        );
    }

    #[test]
    fn clearing_forgets_every_command_and_wakes_the_views() {
        let console = CommandConsole::new();
        record(&console, "status");
        let revision = console.revision();

        console.clear();

        assert!(console.entries().is_empty());
        assert!(console.revision() > revision, "a clear is a change too");
    }

    #[test]
    fn the_console_of_a_project_shows_only_its_commands() {
        let console = CommandConsole::new();
        record_in(&console, "one", "commit");
        record_in(&console, "two", "push");
        record_in(&console, "one/sub", "add");
        record_in(&console, "one-more", "pull");

        let args = |entries: Vec<CommandRecord>| {
            entries
                .into_iter()
                .map(|entry| entry.args.join(" "))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            args(console.entries_under(Path::new("one"))),
            ["commit", "add"],
            "the project and its submodules, in the order they ran"
        );
        assert!(
            console
                .entries_under(Path::new("one"))
                .iter()
                .all(|entry| entry.directory != Path::new("one-more")),
            "a folder whose name starts the same is no part of the project"
        );
        assert!(console.entries_under(Path::new("three")).is_empty());
    }

    #[test]
    fn the_commands_of_another_project_push_nothing_out_of_this_one() {
        let console = CommandConsole::new();
        record_in(&console, "mine", "commit");
        for index in 0..COMMAND_LIMIT + 50 {
            record_in(&console, "other", &format!("status-{index}"));
        }

        let entries = console.entries_under(Path::new("mine"));
        assert_eq!(entries.len(), 1, "the commit of the project is still there");
        assert_eq!(entries[0].args, ["commit"]);
        assert_eq!(
            console.entries_under(Path::new("other")).len(),
            COMMAND_LIMIT,
            "the journal of a busy project keeps its own last commands"
        );
    }

    #[test]
    fn clearing_a_project_leaves_the_other_journals_alone() {
        let console = CommandConsole::new();
        record_in(&console, "one", "commit");
        record_in(&console, "two", "push");

        console.clear_under(Path::new("one"));

        assert!(console.entries_under(Path::new("one")).is_empty());
        assert_eq!(
            console.entries_under(Path::new("two")).len(),
            1,
            "the other project keeps its command"
        );
    }

    #[test]
    fn a_session_that_visits_many_folders_keeps_the_last_journals() {
        let console = CommandConsole::new();
        for index in 0..JOURNAL_LIMIT + 1 {
            record_in(&console, &format!("dir-{index}"), "commit");
        }

        assert!(
            console.entries_under(Path::new("dir-0")).is_empty(),
            "the folder visited longest ago goes"
        );
        assert_eq!(
            console
                .entries_under(Path::new(&format!("dir-{JOURNAL_LIMIT}")))
                .len(),
            1,
            "the last folder stays"
        );
    }

    #[test]
    fn the_commands_of_the_ide_itself_are_background() {
        assert!(is_background(&["status", "--porcelain=v1"]));
        assert!(is_background(&["rev-parse", "HEAD"]));
        assert!(is_background(&["for-each-ref", "refs/heads"]));
        assert!(is_background(&["--version"]));
        assert!(!is_background(&["commit", "-m", "first"]));
        assert!(!is_background(&["push"]));
        assert!(
            !is_background(&["-c", "core.editor=true", "rebase", "--continue"]),
            "the global options before the subcommand are skipped"
        );
        assert!(
            CommandRecord::new(
                Path::new("work"),
                &["status"],
                Some(0),
                Duration::ZERO,
                b"",
                b""
            )
            .background
        );
        assert!(
            !CommandRecord::new(
                Path::new("work"),
                &["commit", "-m", "first"],
                Some(0),
                Duration::ZERO,
                b"",
                b""
            )
            .background
        );
    }

    #[test]
    fn a_command_the_runner_runs_reaches_the_journal() {
        let git = crate::GitRunner::from_env().expect("git in PATH");
        let console = command_console();
        let before = console.revision();

        git.run(Path::new("."), ["--version"]).unwrap();

        assert!(console.revision() > before, "the runner added a command");
        let entry = console
            .entries()
            .into_iter()
            .rev()
            .take(4)
            .find(|entry| entry.args == ["--version"])
            .expect("the version query is in the journal");
        assert_eq!(entry.code, Some(0));
        assert!(
            entry.stdout.starts_with("git version"),
            "the journal keeps the output: {}",
            entry.stdout
        );
    }

    #[test]
    fn a_long_output_is_cut() {
        let text = "x".repeat(OUTPUT_LIMIT + 10);

        let entry = CommandRecord::new(
            Path::new("work"),
            &["log"],
            Some(0),
            Duration::ZERO,
            text.as_bytes(),
            b"",
        );

        assert!(entry.stdout.len() < OUTPUT_LIMIT + 32);
        assert!(entry.stdout.ends_with("… output cut"));
    }

    #[test]
    fn a_background_command_keeps_less_output() {
        let text = "x".repeat(BACKGROUND_OUTPUT_LIMIT + 10);

        let entry = CommandRecord::new(
            Path::new("work"),
            &["status", "--porcelain=v1"],
            Some(0),
            Duration::ZERO,
            text.as_bytes(),
            b"",
        );

        assert!(
            entry.stdout.len() < BACKGROUND_OUTPUT_LIMIT + 32,
            "a command of the ide keeps a short output: {} bytes",
            entry.stdout.len()
        );
        assert!(entry.stdout.ends_with("… output cut"));
    }
}

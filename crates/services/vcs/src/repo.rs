use std::collections::HashSet;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::{BlobReader, Change, GitRunner, Hunk, Status, line_hunks, parse_status};

const FIELD: char = '\u{1f}';
const RECORD: char = '\u{1e}';
const LOG_FORMAT: &str = "--format=%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%s%x1e";
const BRANCH_FORMAT: &str =
    "--format=%(HEAD)%1f%(refname)%1f%(objectname)%1f%(upstream:short)%1f%(upstream:track)%1e";
const LABEL_FORMAT: &str = "--format=%(HEAD)%1f%(refname)%1f%(objectname)%1f%(*objectname)%1e";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Branch {
    /// `main` or `origin/main`.
    pub name: String,
    pub remote: bool,
    pub current: bool,
    pub head: String,
    pub upstream: Option<String>,
    /// Commits the branch has that the upstream does not, as `%(upstream:track)` counts.
    pub ahead: u32,
    pub behind: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Commit {
    pub id: String,
    pub parents: Vec<String>,
    pub author: String,
    pub email: String,
    /// Seconds since 1970.
    pub time: i64,
    pub subject: String,
}

/// A reference the log shows as a label: a local branch, a remote branch or a tag.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RefLabel {
    /// The commit the reference points at.
    pub commit: String,
    /// `main`, `origin/main` or `v0.1.12`.
    pub name: String,
    pub kind: RefKind,
    /// HEAD points at this branch.
    pub current: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefKind {
    Local,
    Remote,
    Tag,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitRequest {
    pub message: String,
    /// Relative to the root. Empty means every change.
    pub paths: Vec<String>,
    pub amend: bool,
}

/// Two branches side by side: the commits only one of them has, and the files they
/// differ in. `base` is the branch the user compares with the current one.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Compare {
    pub only_base: Vec<Commit>,
    pub only_current: Vec<Commit>,
    pub files: Vec<String>,
}

/// The extra switches of the commit options popup.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CommitOptions {
    pub sign_off: bool,
    /// `Name <email>` as `--author` takes it.
    pub author: Option<String>,
}

/// The three versions of a file git left in conflict: the stages of the index.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ConflictVersions {
    /// `:1:path`, the common ancestor. `None` when both sides added the file.
    pub base: Option<Vec<u8>>,
    /// `:2:path`, ours. `None` when we deleted the file.
    pub yours: Option<Vec<u8>>,
    /// `:3:path`, theirs. `None` when they deleted the file.
    pub theirs: Option<Vec<u8>>,
}

/// The current text of a file against the last commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileDiff {
    /// Empty for a file the last commit does not have.
    pub base: String,
    /// The version the diff compares against `base`.
    pub current: String,
    pub hunks: Vec<Hunk>,
}

/// One entry of `git stash list`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Stash {
    /// The number in `stash@{0}`.
    pub index: usize,
    pub message: String,
}

/// A command that git left unfinished and that the user has to end or abort.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Operation {
    #[default]
    None,
    Merge,
    Rebase,
    CherryPick,
    Revert,
}

/// What Reset Current Branch to Here does with the index and the working tree.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ResetMode {
    /// Keeps the changes, staged for commit.
    #[default]
    Soft,
    /// Keeps the changes, not staged.
    Mixed,
    /// Throws the changes away.
    Hard,
    /// Keeps the changes and leaves them not staged.
    Keep,
}

impl ResetMode {
    pub fn flag(self) -> &'static str {
        match self {
            Self::Soft => "--soft",
            Self::Mixed => "--mixed",
            Self::Hard => "--hard",
            Self::Keep => "--keep",
        }
    }

    /// The line the Reset dialog shows under the name of the mode.
    pub fn hint(self) -> &'static str {
        match self {
            Self::Soft => "Keep all local changes. Changes will be staged for commit.",
            Self::Mixed => "Keep all local changes. Changes will not be staged for commit.",
            Self::Hard => "Discard all local changes.",
            Self::Keep => "Keep all local changes. Staged changes will be unstaged and kept.",
        }
    }

    /// The name of the mode in the menu of git.
    pub fn label(self) -> &'static str {
        match self {
            Self::Soft => "Soft",
            Self::Mixed => "Mixed",
            Self::Hard => "Hard",
            Self::Keep => "Keep",
        }
    }
}

/// How Update Project brings the upstream in.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum UpdateMode {
    #[default]
    Merge,
    Rebase,
}

/// What Update Project left behind: the operation still going, the files in conflict and
/// the message git wrote for a merge commit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdateReport {
    pub operation: Operation,
    pub conflicts: Vec<String>,
    pub merge_message: String,
    /// Git is holding the local changes and puts them back when the operation ends.
    pub autostash: bool,
    /// Local changes git could not put back; they wait in this stash.
    pub stash: Option<Stash>,
}

/// Which side of a conflict a file takes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConflictSide {
    Yours,
    Theirs,
}

pub struct Repository {
    git: GitRunner,
    root: PathBuf,
    /// Reads of the status, so a test tells the read of the window from the reads of an event.
    status_reads: AtomicU64,
}

impl Repository {
    /// The repository that holds `directory`.
    pub fn open(git: GitRunner, directory: &Path) -> io::Result<Self> {
        let output = git.run(directory, ["rev-parse", "--show-toplevel"])?;
        let root = PathBuf::from(String::from_utf8_lossy(&output).trim());
        Ok(Self {
            git,
            root,
            status_reads: AtomicU64::new(0),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn git(&self) -> &GitRunner {
        &self.git
    }

    /// How many times the status was read. A test uses it to prove that an idle window does
    /// not ask git at all.
    pub fn status_reads(&self) -> u64 {
        self.status_reads.load(Ordering::Relaxed)
    }

    fn run<const N: usize>(&self, args: [&str; N]) -> io::Result<String> {
        let output = self.git.run(&self.root, args)?;
        Ok(String::from_utf8_lossy(&output).into_owned())
    }

    pub fn status(&self) -> io::Result<Status> {
        self.status_reads.fetch_add(1, Ordering::Relaxed);
        let args = [
            "status",
            "--porcelain=v2",
            "-z",
            "--branch",
            "--untracked-files=all",
        ];
        Ok(parse_status(&self.git.run(&self.root, args)?))
    }

    /// Starts a reader for [`Repository::file_diff`], one per thread that asks.
    pub fn blobs(&self) -> io::Result<BlobReader> {
        BlobReader::start(&self.git, &self.root)
    }

    /// `current` is the text as the editor holds it, with `\n` only.
    pub fn file_diff(
        &self,
        blobs: &mut BlobReader,
        path: &str,
        current: &str,
    ) -> io::Result<FileDiff> {
        self.file_diff_with(blobs, "HEAD", path, current)
    }

    /// Like [`Repository::file_diff`], against `revision` instead of the last commit.
    pub fn file_diff_with(
        &self,
        blobs: &mut BlobReader,
        revision: &str,
        path: &str,
        current: &str,
    ) -> io::Result<FileDiff> {
        let base = blobs
            .read(&format!("{revision}:{path}"))?
            .unwrap_or_default();
        let base = text_core::decode(&base).text;
        let hunks = line_hunks(&base, current);
        Ok(FileDiff {
            base,
            current: current.to_owned(),
            hunks,
        })
    }

    /// Takes the chosen files as they are on disk, new and deleted ones too, and nothing
    /// else, whatever the index held before. Returns the new commit.
    pub fn commit(&self, request: &CommitRequest) -> io::Result<String> {
        self.commit_with(request, &CommitOptions::default())
    }

    /// Like [`Repository::commit`], with the switches of the commit options popup.
    pub fn commit_with(
        &self,
        request: &CommitRequest,
        options: &CommitOptions,
    ) -> io::Result<String> {
        let mut add = vec!["add", "-A", "--"];
        add.extend(request.paths.iter().map(String::as_str));
        self.git.run(&self.root, &add)?;
        let mut commit = vec!["commit", "-F", "-"];
        if request.amend {
            commit.push("--amend");
        }
        if options.sign_off {
            commit.push("--signoff");
        }
        let author = options
            .author
            .as_deref()
            .map(str::trim)
            .filter(|author| !author.is_empty());
        if let Some(author) = author {
            commit.push("--author");
            commit.push(author);
        }
        if !request.paths.is_empty() {
            commit.push("--");
            commit.extend(request.paths.iter().map(String::as_str));
        }
        let message = request.message.as_bytes();
        self.git
            .run_with_input(&self.root, &commit, Some(message))?;
        Ok(self.run(["rev-parse", "HEAD"])?.trim().to_owned())
    }

    /// Whether the commit belongs to the user of the repository.
    pub fn is_own_commit(&self, commit: &str) -> io::Result<bool> {
        let email = self.run(["show", "-s", "--format=%ae", commit])?;
        let email = email.trim();
        let configured = self
            .run(["config", "user.email"])
            .map(|email| email.trim().to_owned())
            .unwrap_or_default();
        Ok(!email.is_empty() && email == configured)
    }

    /// Whether the commit is already on a remote branch.
    pub fn is_pushed(&self, commit: &str) -> io::Result<bool> {
        let branches = self.run(["branch", "-r", "--contains", commit])?;
        Ok(!branches.trim().is_empty())
    }

    /// Gives the commit another message, as F2 of the log does. The author and the tree
    /// stay; the index of the repository does not take part. Only a commit of the user on
    /// the current branch that no remote has is allowed.
    pub fn reword(&self, commit: &str, message: &str) -> io::Result<()> {
        let message = message.trim();
        if message.is_empty() {
            return Err(io::Error::other("the message is empty"));
        }
        if !self.is_own_commit(commit)? {
            return Err(io::Error::other("the commit belongs to another author"));
        }
        if self.is_pushed(commit)? {
            return Err(io::Error::other("the commit is already pushed"));
        }
        if self
            .git
            .run(&self.root, ["merge-base", "--is-ancestor", commit, "HEAD"])
            .is_err()
        {
            return Err(io::Error::other("the commit is not on the current branch"));
        }
        // A short id from the log is compared with HEAD and named in the todo list in full.
        let full = self
            .run(["rev-parse", "--verify", &format!("{commit}^{{commit}}")])?
            .trim()
            .to_owned();
        let commit = full.as_str();
        let head = self.run(["rev-parse", "HEAD"])?.trim().to_owned();
        if head == commit {
            let index = temp_path("reword-index");
            let _ = std::fs::remove_file(&index);
            let env = [("GIT_INDEX_FILE", index.as_os_str())];
            let base = self.run_env_check(&["read-tree", "HEAD"], &env);
            let result = base.and_then(|()| {
                self.run_env_with_input(
                    &["commit", "--amend", "-F", "-"],
                    &env,
                    Some(message.as_bytes()),
                )
            });
            let _ = std::fs::remove_file(&index);
            return result.map(drop);
        }
        // A rebase of the user stays theirs: the abort below would throw it away.
        for state in ["rebase-merge", "rebase-apply"] {
            let path = self.run(["rev-parse", "--git-path", state])?;
            if self.root.join(path.trim()).exists() {
                return Err(io::Error::other("a rebase is in progress"));
            }
        }
        // The todo list below is flat, so a merge after the commit would be lost.
        let merges = self.run(["rev-list", "--merges", &format!("{commit}..HEAD")])?;
        if !merges.trim().is_empty() {
            return Err(io::Error::other(
                "a merge commit follows the commit, reword it with git itself",
            ));
        }
        let parent = self.run(["rev-parse", &format!("{commit}^")]);
        let descendants = self.run(["rev-list", "--reverse", &format!("{commit}..HEAD")])?;
        let mut ids = vec![commit.to_owned()];
        ids.extend(
            descendants
                .lines()
                .map(str::trim)
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
        );
        let mut todo = String::new();
        for (index, id) in ids.iter().enumerate() {
            let subject = self.run(["show", "-s", "--format=%s", id])?;
            let subject = subject.trim().replace(['\r', '\n'], " ");
            let verb = if index == 0 { "reword" } else { "pick" };
            todo.push_str(&format!("{verb} {id} {subject}\n"));
        }
        let folder = temp_path("reword");
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder)?;
        let todo_file = folder.join("todo.txt");
        let message_file = folder.join("message.txt");
        std::fs::write(&todo_file, todo)?;
        std::fs::write(&message_file, message)?;
        let mut command = self.git.command(&self.root);
        match &parent {
            Ok(_) => {
                command.args(["rebase", "-i", &format!("{commit}^")]);
            }
            Err(_) => {
                command.args(["rebase", "-i", "--root"]);
            }
        }
        let output = command
            .env("GIT_SEQUENCE_EDITOR", copy_command(&todo_file))
            .env("GIT_EDITOR", copy_command(&message_file))
            .output();
        let _ = std::fs::remove_dir_all(&folder);
        match output {
            Ok(output) if output.status.success() => Ok(()),
            Ok(output) => {
                let _ = self.git.run(&self.root, ["rebase", "--abort"]);
                let stderr = String::from_utf8_lossy(&output.stderr);
                Err(io::Error::other(format!("git: {}", stderr.trim())))
            }
            Err(error) => {
                let _ = self.git.run(&self.root, ["rebase", "--abort"]);
                Err(error)
            }
        }
    }

    fn run_env_check(&self, args: &[&str], env: &[(&str, &std::ffi::OsStr)]) -> io::Result<()> {
        let mut command = self.git.command(&self.root);
        command.args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        crate::GitRunner::checked(command).map(drop)
    }

    fn run_env_with_input(
        &self,
        args: &[&str],
        env: &[(&str, &std::ffi::OsStr)],
        input: Option<&[u8]>,
    ) -> io::Result<Vec<u8>> {
        let mut command = self.git.command(&self.root);
        command.args(args);
        for (name, value) in env {
            command.env(name, value);
        }
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn()?;
        if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
            stdin.write_all(input)?;
        }
        let output = child.wait_with_output()?;
        if output.status.success() {
            return Ok(output.stdout);
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        Err(io::Error::other(format!("git: {}", stderr.trim())))
    }

    /// Sends the current branch. A branch with no upstream gets one on the first remote.
    pub fn push(&self) -> io::Result<()> {
        if self.status()?.upstream.is_some() {
            self.run(["push"])?;
            return Ok(());
        }
        let remotes = self.run(["remote"])?;
        let remote = remotes
            .lines()
            .next()
            .ok_or_else(|| io::Error::other("no remote"))?;
        self.git
            .run(&self.root, ["push", "--set-upstream", remote, "HEAD"])?;
        Ok(())
    }

    /// Ctrl+T: fetch, then merge the upstream in. Kept for callers that want the default.
    pub fn update(&self) -> io::Result<UpdateReport> {
        self.update_with(UpdateMode::Merge)
    }

    /// Fetches the remote branches without touching the working tree.
    pub fn fetch(&self) -> io::Result<()> {
        self.run(["fetch", "--prune"]).map(drop)
    }

    /// Fetches and brings the upstream in. Git hides the local changes itself
    /// (`--autostash`) and puts them back when the operation ends, even after a restart. A
    /// conflict is not an error: it is in the report with the operation that waits for the
    /// user. Local changes that cannot be put back wait in the reported stash.
    pub fn update_with(&self, mode: UpdateMode) -> io::Result<UpdateReport> {
        self.fetch()?;
        let status = self.status()?;
        if status.upstream.is_none() {
            return Ok(UpdateReport::default());
        }
        let stashes = self.stash_ids()?;
        let result = match mode {
            UpdateMode::Merge => self.run(["merge", "--autostash", "@{upstream}"]),
            UpdateMode::Rebase => self.run(["rebase", "--autostash", "@{upstream}"]),
        };
        self.report_update(result, stashes)
    }

    /// Merges `branch` into the current one, with the local changes stashed around it.
    /// A conflict is in the report with the operation that waits for the user.
    pub fn merge_branch(&self, branch: &str) -> io::Result<UpdateReport> {
        let stashes = self.stash_ids()?;
        let result = self.run(["merge", "--autostash", branch]);
        self.report_update(result, stashes)
    }

    /// Replays the current branch onto `branch`, with the local changes stashed around it.
    pub fn rebase_onto(&self, branch: &str) -> io::Result<UpdateReport> {
        let stashes = self.stash_ids()?;
        let result = self.run(["rebase", "--autostash", branch]);
        self.report_update(result, stashes)
    }

    /// What a merge or a rebase left behind, whichever way git ended.
    fn report_update(
        &self,
        result: io::Result<String>,
        before: HashSet<String>,
    ) -> io::Result<UpdateReport> {
        let operation = self.operation()?;
        let conflicts = self.conflicts()?;
        // A failed apply of the autostash is a new stash entry, not a failure of git.
        let stash = self.stash_since(&before)?;
        if let Err(error) = result
            && operation == Operation::None
        {
            return Err(match stash {
                Some(stash) => stash_error(error, &stash),
                None => error,
            });
        }
        let merge_message = match operation {
            Operation::Merge => self.merge_message()?,
            _ => String::new(),
        };
        Ok(UpdateReport {
            operation,
            conflicts,
            merge_message,
            autostash: operation != Operation::None && self.autostash()?,
            stash,
        })
    }

    /// Stashes the tracked changes with `message`; an empty message leaves git's own.
    pub fn stash_push(&self, message: &str) -> io::Result<()> {
        let message = message.trim();
        if message.is_empty() {
            self.run(["stash", "push"]).map(drop)
        } else {
            self.run(["stash", "push", "-m", message]).map(drop)
        }
    }

    /// Newest first.
    pub fn stash_list(&self) -> io::Result<Vec<Stash>> {
        Ok(self
            .stash_entries()?
            .into_iter()
            .map(|(_, stash)| stash)
            .collect())
    }

    /// The stashes with the ids of their commits, newest first.
    fn stash_entries(&self) -> io::Result<Vec<(String, Stash)>> {
        let output = self.run(["stash", "list", "--format=%gd%x1f%H%x1f%s%x1e"])?;
        let mut entries = Vec::new();
        for record in output.split(RECORD) {
            let mut fields = record.trim_start_matches('\n').split(FIELD);
            let Some(reference) = fields.next().filter(|text| !text.is_empty()) else {
                continue;
            };
            let Some(id) = fields.next().filter(|text| !text.is_empty()) else {
                continue;
            };
            let Some(message) = fields.next() else {
                continue;
            };
            let Some(index) = stash_index(reference) else {
                continue;
            };
            entries.push((
                id.to_owned(),
                Stash {
                    index,
                    message: message.to_owned(),
                },
            ));
        }
        Ok(entries)
    }

    fn stash_ids(&self) -> io::Result<HashSet<String>> {
        Ok(self
            .stash_entries()?
            .into_iter()
            .map(|(id, _)| id)
            .collect())
    }

    /// The newest stash that was not in `before`, if any.
    fn stash_since(&self, before: &HashSet<String>) -> io::Result<Option<Stash>> {
        Ok(self
            .stash_entries()?
            .into_iter()
            .find(|(id, _)| !before.contains(id))
            .map(|(_, stash)| stash))
    }

    /// The stash with this commit id, when git left it behind.
    fn stash_with_id(&self, id: Option<String>) -> io::Result<Option<Stash>> {
        let Some(id) = id else {
            return Ok(None);
        };
        Ok(self
            .stash_entries()?
            .into_iter()
            .find(|(entry, _)| *entry == id)
            .map(|(_, stash)| stash))
    }

    pub fn stash_apply(&self, index: usize) -> io::Result<()> {
        let reference = stash_ref(index);
        self.run(["stash", "apply", reference.as_str()]).map(drop)
    }

    pub fn stash_pop(&self, index: usize) -> io::Result<()> {
        let reference = stash_ref(index);
        self.run(["stash", "pop", reference.as_str()]).map(drop)
    }

    pub fn stash_drop(&self, index: usize) -> io::Result<()> {
        let reference = stash_ref(index);
        self.run(["stash", "drop", reference.as_str()]).map(drop)
    }

    /// Whether git is holding local changes it puts back when the operation ends.
    pub fn autostash(&self) -> io::Result<bool> {
        Ok(self.autostash_id()?.is_some())
    }

    /// The stash commit git keeps for the operation it waits to end.
    fn autostash_id(&self) -> io::Result<Option<String>> {
        for name in [
            "MERGE_AUTOSTASH",
            "rebase-merge/autostash",
            "rebase-apply/autostash",
        ] {
            let Ok(text) = std::fs::read_to_string(self.git_path(name)?) else {
                continue;
            };
            let id = text.trim();
            if !id.is_empty() {
                return Ok(Some(id.to_owned()));
            }
        }
        Ok(None)
    }

    /// `merge --abort` or `rebase --abort`, whichever is going on. Returns the stash when
    /// the local changes did not apply cleanly and still wait there.
    pub fn abort(&self) -> io::Result<Option<Stash>> {
        let autostash = self.autostash_id()?;
        match self.operation()? {
            Operation::Merge => self.run(["merge", "--abort"]).map(drop)?,
            Operation::Rebase => self.run(["rebase", "--abort"]).map(drop)?,
            Operation::CherryPick => self.run(["cherry-pick", "--abort"]).map(drop)?,
            Operation::Revert => self.run(["revert", "--abort"]).map(drop)?,
            Operation::None => return Ok(None),
        }
        self.stash_with_id(autostash)
    }

    /// The files git left in conflict, sorted as the status has them.
    pub fn conflicts(&self) -> io::Result<Vec<String>> {
        let status = self.status()?;
        Ok(status
            .files
            .into_iter()
            .filter(|file| file.index == Change::Conflicted || file.worktree == Change::Conflicted)
            .map(|file| file.path)
            .collect())
    }

    /// Takes one side of a conflict and marks the file resolved, as IDEA does.
    pub fn accept_side(&self, path: &str, side: ConflictSide) -> io::Result<()> {
        let flag = match side {
            ConflictSide::Yours => "--ours",
            ConflictSide::Theirs => "--theirs",
        };
        self.git.run(&self.root, ["checkout", flag, "--", path])?;
        self.mark_resolved(path)
    }

    /// `git add`: the user edited the file by hand.
    pub fn mark_resolved(&self, path: &str) -> io::Result<()> {
        self.git.run(&self.root, ["add", "--", path]).map(drop)
    }

    /// The three versions of a conflicted file from the index stages: the common
    /// ancestor, ours and theirs. A stage the index has no entry for is `None`, as for a
    /// file one side added or deleted.
    pub fn conflict_versions(
        &self,
        blobs: &mut BlobReader,
        path: &str,
    ) -> io::Result<ConflictVersions> {
        Ok(ConflictVersions {
            base: blobs.read(&format!(":1:{path}"))?,
            yours: blobs.read(&format!(":2:{path}"))?,
            theirs: blobs.read(&format!(":3:{path}"))?,
        })
    }

    /// Reads `.git/MERGE_MSG`, the message a merge commit starts with.
    pub fn merge_message(&self) -> io::Result<String> {
        let path = self.git_path("MERGE_MSG")?;
        Ok(std::fs::read_to_string(path)
            .unwrap_or_default()
            .trim()
            .to_owned())
    }

    /// Ends a merge with `message`, or continues a rebase, a cherry-pick or a revert. A
    /// merge has to have no conflicts. Returns the stash when the local changes did not
    /// apply cleanly and still wait there.
    pub fn finish(&self, message: &str) -> io::Result<Option<Stash>> {
        let autostash = self.autostash_id()?;
        match self.operation()? {
            Operation::Merge => {
                let message = message.trim();
                if message.is_empty() {
                    self.run(["commit", "--no-edit"]).map(drop)?;
                } else {
                    self.git
                        .run_with_input(&self.root, ["commit", "-F", "-"], Some(message.as_bytes()))
                        .map(drop)?;
                }
            }
            Operation::Rebase => self
                .run(["-c", "core.editor=true", "rebase", "--continue"])
                .map(drop)?,
            Operation::CherryPick => self
                .run(["-c", "core.editor=true", "cherry-pick", "--continue"])
                .map(drop)?,
            Operation::Revert => self
                .run(["-c", "core.editor=true", "revert", "--continue"])
                .map(drop)?,
            Operation::None => return Ok(None),
        }
        self.stash_with_id(autostash)
    }

    /// The files a commit changed, against its first parent.
    pub fn commit_files(&self, commit: &str) -> io::Result<Vec<String>> {
        let line = self.run(["rev-list", "--parents", "-n", "1", commit])?;
        let fields: Vec<&str> = line.split_whitespace().collect();
        let mut args = vec!["diff-tree", "--no-commit-id", "--name-only", "-r"];
        match fields.get(1) {
            Some(parent) => {
                args.push(parent);
                args.push(commit);
            }
            None => {
                args.push("--root");
                args.push(commit);
            }
        }
        let output = self.git.run(&self.root, &args)?;
        Ok(String::from_utf8_lossy(&output)
            .lines()
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(str::to_owned)
            .collect())
    }

    /// The text of `path` at `commit` against its first parent, for the diff popup.
    pub fn commit_file_diff(
        &self,
        blobs: &mut BlobReader,
        commit: &str,
        path: &str,
    ) -> io::Result<FileDiff> {
        let base = blobs
            .read(&format!("{commit}^:{path}"))?
            .unwrap_or_default();
        let base = text_core::decode(&base).text;
        let current = blobs.read(&format!("{commit}:{path}"))?.unwrap_or_default();
        let current = text_core::decode(&current).text;
        let hunks = line_hunks(&base, &current);
        Ok(FileDiff {
            base,
            current,
            hunks,
        })
    }

    /// The log of one file, newest first.
    pub fn file_log(&self, path: &str, limit: usize, skip: usize) -> io::Result<Vec<Commit>> {
        self.log_with(Some(path), limit, skip)
    }

    fn git_path(&self, name: &str) -> io::Result<PathBuf> {
        let output = self.run(["rev-parse", "--git-path", name])?;
        let path = PathBuf::from(output.trim());
        Ok(if path.is_absolute() {
            path
        } else {
            self.root.join(path)
        })
    }

    /// What git waits for: a merge, a rebase, a cherry-pick, a revert, or nothing.
    pub fn operation(&self) -> io::Result<Operation> {
        let rebase = self.git_path("rebase-merge")?;
        if rebase.exists() || self.git_path("rebase-apply")?.exists() {
            return Ok(Operation::Rebase);
        }
        if self.git_path("MERGE_HEAD")?.exists() {
            return Ok(Operation::Merge);
        }
        if self.git_path("CHERRY_PICK_HEAD")?.exists() {
            return Ok(Operation::CherryPick);
        }
        if self.git_path("REVERT_HEAD")?.exists() {
            return Ok(Operation::Revert);
        }
        Ok(Operation::None)
    }

    pub fn branches(&self) -> io::Result<Vec<Branch>> {
        let output = self.run(["for-each-ref", BRANCH_FORMAT, "refs/heads", "refs/remotes"])?;
        let branches = output.split(RECORD).filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split(FIELD);
            let current = fields.next()? == "*";
            let reference = fields.next()?;
            let head = fields.next()?.to_owned();
            let upstream = fields
                .next()
                .filter(|name| !name.is_empty())
                .map(str::to_owned);
            let (ahead, behind) = track_counts(fields.next().unwrap_or_default());
            let (name, remote) = match reference.strip_prefix("refs/heads/") {
                Some(name) => (name, false),
                None => (reference.strip_prefix("refs/remotes/")?, true),
            };
            // `origin/HEAD` only points at another branch.
            if remote && name.ends_with("/HEAD") {
                return None;
            }
            let name = name.to_owned();
            Some(Branch {
                name,
                remote,
                current,
                head,
                upstream,
                ahead,
                behind,
            })
        });
        Ok(branches.collect())
    }

    /// The branches and tags of the repository in one call, for the labels of the log.
    /// The branch HEAD points at is marked current.
    pub fn labels(&self) -> io::Result<Vec<RefLabel>> {
        let output = self.run([
            "for-each-ref",
            LABEL_FORMAT,
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ])?;
        let labels = output.split(RECORD).filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split(FIELD);
            let current = fields.next()? == "*";
            let reference = fields.next()?;
            let object = fields.next()?;
            // An annotated tag names the tag object; the peeled field is its commit.
            let peeled = fields.next().unwrap_or_default();
            let commit = if peeled.is_empty() { object } else { peeled }.to_owned();
            let (name, kind) = if let Some(name) = reference.strip_prefix("refs/heads/") {
                (name, RefKind::Local)
            } else if let Some(name) = reference.strip_prefix("refs/remotes/") {
                (name, RefKind::Remote)
            } else {
                (reference.strip_prefix("refs/tags/")?, RefKind::Tag)
            };
            // `origin/HEAD` only points at another branch.
            if kind == RefKind::Remote && name.ends_with("/HEAD") {
                return None;
            }
            let name = name.to_owned();
            Some(RefLabel {
                commit,
                name,
                kind,
                current,
            })
        });
        Ok(labels.collect())
    }

    pub fn switch(&self, branch: &str) -> io::Result<()> {
        self.git.run(&self.root, ["switch", branch]).map(drop)
    }

    /// Checks a remote branch out. A local branch of that name is created to track it,
    /// or, when it exists already, simply checked out.
    pub fn checkout_remote(&self, branch: &str) -> io::Result<()> {
        let local = branch.split_once('/').map_or(branch, |(_, name)| name);
        let reference = format!("refs/heads/{local}");
        if self
            .run(["rev-parse", "--verify", "--quiet", &reference])
            .is_ok()
        {
            return self.switch(local);
        }
        self.run(["switch", "--create", local, "--track", branch])
            .map(drop)
    }

    pub fn create_branch(&self, name: &str, start: &str) -> io::Result<()> {
        self.git
            .run(&self.root, ["switch", "-c", name, start])
            .map(drop)
    }

    /// Only a branch that is merged, as `git branch -d` does.
    pub fn delete_branch(&self, name: &str) -> io::Result<()> {
        self.git.run(&self.root, ["branch", "-d", name]).map(drop)
    }

    /// Deletes a branch that is not merged, as the Force Delete button does.
    pub fn force_delete_branch(&self, name: &str) -> io::Result<()> {
        self.git.run(&self.root, ["branch", "-D", name]).map(drop)
    }

    /// Puts a deleted branch back at the commit it pointed at.
    pub fn restore_branch(&self, name: &str, id: &str) -> io::Result<()> {
        self.git.run(&self.root, ["branch", name, id]).map(drop)
    }

    /// The commit a local branch points at, for the Restore of a deleted one.
    pub fn branch_head(&self, name: &str) -> io::Result<String> {
        Ok(self
            .run(["rev-parse", "--verify", &format!("refs/heads/{name}")])?
            .trim()
            .to_owned())
    }

    /// Whether every commit of the branch is already on the current one.
    pub fn is_branch_merged(&self, name: &str) -> io::Result<bool> {
        Ok(self
            .git
            .run(&self.root, ["merge-base", "--is-ancestor", name, "HEAD"])
            .is_ok())
    }

    /// Deletes a branch on its remote, as Delete of a remote branch does.
    pub fn delete_remote_branch(&self, remote: &str, branch: &str) -> io::Result<()> {
        self.git
            .run(&self.root, ["push", remote, "--delete", branch])
            .map(drop)
    }

    /// Gives a local branch another name, keeping the one checked out current.
    pub fn rename_branch(&self, from: &str, to: &str) -> io::Result<()> {
        self.git
            .run(&self.root, ["branch", "-m", from, to])
            .map(drop)
    }

    /// Takes the named files back to the state of the last commit. A file the last
    /// commit does not have is only unstaged and stays on disk, as IDEA's Rollback does
    /// by default. Other files of the working tree stay as they are.
    pub fn revert_paths(&self, paths: &[String]) -> io::Result<()> {
        let status = self.status()?;
        let head = status.head.is_some();
        for path in paths {
            let file = status.files.iter().find(|file| &file.path == path);
            let added = file.is_some_and(|file| {
                file.index == Change::Added || file.worktree == Change::Untracked
            });
            if added {
                // A file the last commit does not have: unstage it, keep its text.
                if head && file.is_some_and(|file| file.index == Change::Added) {
                    self.git.run(&self.root, ["reset", "-q", "--", path])?;
                }
                continue;
            }
            if head {
                self.git.run(&self.root, ["checkout", "HEAD", "--", path])?;
            }
        }
        Ok(())
    }

    /// Newest first, `skip` for the next page.
    pub fn log(&self, limit: usize, skip: usize) -> io::Result<Vec<Commit>> {
        self.log_with(None, limit, skip)
    }

    fn log_with(&self, path: Option<&str>, limit: usize, skip: usize) -> io::Result<Vec<Commit>> {
        if self.status()?.head.is_none() {
            return Ok(Vec::new());
        }
        let limit = format!("--max-count={limit}");
        let skip = format!("--skip={skip}");
        // A parent always follows its children, so the graph of the window has no line
        // that has to go back up.
        let mut args = vec!["log", LOG_FORMAT, "--topo-order", &limit, &skip];
        if let Some(path) = path {
            args.push("--");
            args.push(path);
        }
        let output = self.git.run(&self.root, &args)?;
        Ok(commits_of(&String::from_utf8_lossy(&output)))
    }

    /// The commits of a revision range such as `main..feature`, newest first.
    pub fn commits_in(&self, range: &str, limit: usize) -> io::Result<Vec<Commit>> {
        let limit = format!("--max-count={limit}");
        let output = self
            .git
            .run(&self.root, ["log", LOG_FORMAT, &limit, range])?;
        Ok(commits_of(&String::from_utf8_lossy(&output)))
    }

    /// The commits only one of the two branches has, and the files they differ in.
    pub fn compare_with(&self, branch: &str) -> io::Result<Compare> {
        let only_base = self.commits_in(&format!("HEAD..{branch}"), COMPARE_LIMIT)?;
        let only_current = self.commits_in(&format!("{branch}..HEAD"), COMPARE_LIMIT)?;
        let mut files = Vec::new();
        let base = self.run(["merge-base", branch, "HEAD"]).unwrap_or_default();
        let base = base.trim();
        if base.is_empty() {
            self.collect_files(&mut files, &[branch, "HEAD"])?;
        } else {
            self.collect_files(&mut files, &[base, branch])?;
            self.collect_files(&mut files, &[base, "HEAD"])?;
        }
        files.sort();
        files.dedup();
        Ok(Compare {
            only_base,
            only_current,
            files,
        })
    }

    /// What the working tree changed against `branch`, for Show Diff with Working Tree.
    pub fn working_tree_compare(&self, branch: &str) -> io::Result<Compare> {
        let only_base = self.commits_in(&format!("HEAD..{branch}"), COMPARE_LIMIT)?;
        let mut files = Vec::new();
        self.collect_files(&mut files, &[branch])?;
        files.sort();
        files.dedup();
        Ok(Compare {
            only_base,
            only_current: Vec::new(),
            files,
        })
    }

    /// Adds the files whose text differs between `revisions`, as git diff names them.
    fn collect_files(&self, files: &mut Vec<String>, revisions: &[&str]) -> io::Result<()> {
        let mut args = vec!["diff", "--name-only"];
        args.extend(revisions.iter().copied());
        let output = self.git.run(&self.root, &args)?;
        let output = String::from_utf8_lossy(&output);
        files.extend(
            output
                .lines()
                .map(str::trim)
                .filter(|path| !path.is_empty())
                .map(str::to_owned),
        );
        Ok(())
    }

    /// Checks out the revision itself, leaving HEAD detached, as Checkout Revision of the
    /// log does. Local changes git cannot keep are an error, as they are for a branch.
    pub fn checkout_commit(&self, commit: &str) -> io::Result<()> {
        self.git
            .run(&self.root, ["switch", "--detach", commit])
            .map(drop)
    }

    /// New Tag of the log: a lightweight tag at `commit`.
    pub fn create_tag(&self, name: &str, commit: &str) -> io::Result<()> {
        self.git.run(&self.root, ["tag", name, commit]).map(drop)
    }

    /// New Branch of the log: a branch at `commit`, checked out when `checkout` is set.
    pub fn branch_from(&self, name: &str, commit: &str, checkout: bool) -> io::Result<()> {
        self.git.run(&self.root, ["branch", name, commit])?;
        if checkout {
            self.switch(name)?;
        }
        Ok(())
    }

    /// Replays `commits` onto the current branch, the oldest first, as Cherry-Pick does.
    /// A conflict is not an error: it is in the report with `Operation::CherryPick`, which
    /// [`Repository::finish`] or [`Repository::abort`] ends.
    pub fn cherry_pick(&self, commits: &[String]) -> io::Result<UpdateReport> {
        if commits.is_empty() {
            return Err(io::Error::other("no commit is chosen"));
        }
        let before = self.stash_ids()?;
        let mut args = vec!["cherry-pick"];
        let order = self.oldest_first(commits)?;
        args.extend(order.iter().map(String::as_str));
        let result = self
            .git
            .run(&self.root, &args)
            .map(|output| String::from_utf8_lossy(&output).into_owned());
        self.report_update(result, before)
    }

    /// Reverts `commit` with the message git writes itself, as Revert Commit does. A
    /// conflict comes back with `Operation::Revert`.
    pub fn revert_commit(&self, commit: &str) -> io::Result<UpdateReport> {
        let before = self.stash_ids()?;
        let result = self.run(["revert", "--no-edit", commit]);
        self.report_update(result, before)
    }

    /// Moves the current branch to `commit`, keeping or throwing the changes away as the
    /// Reset dialog chose.
    pub fn reset(&self, commit: &str, mode: ResetMode) -> io::Result<()> {
        self.git
            .run(&self.root, ["reset", mode.flag(), commit])
            .map(drop)
    }

    /// The commits from the oldest to the newest: the log lists them the other way, and
    /// cherry-pick replays them in this order.
    fn oldest_first(&self, commits: &[String]) -> io::Result<Vec<String>> {
        let mut dated = Vec::with_capacity(commits.len());
        for (position, commit) in commits.iter().enumerate() {
            let time = self.run(["show", "-s", "--format=%ct", commit])?;
            let time: i64 = time.trim().parse().unwrap_or(0);
            dated.push((time, position, commit.clone()));
        }
        dated.sort_by_key(|(time, position, _)| (*time, *position));
        Ok(dated.into_iter().map(|(_, _, commit)| commit).collect())
    }
}

/// How many commits of each side Compare shows.
const COMPARE_LIMIT: usize = 200;

/// The commits of `git log` output in [`LOG_FORMAT`], newest first.
fn commits_of(output: &str) -> Vec<Commit> {
    output
        .split(RECORD)
        .filter_map(|record| {
            let mut fields = record.trim_start_matches('\n').split(FIELD);
            Some(Commit {
                id: fields.next().filter(|id| !id.is_empty())?.to_owned(),
                parents: fields
                    .next()?
                    .split_whitespace()
                    .map(str::to_owned)
                    .collect(),
                author: fields.next()?.to_owned(),
                email: fields.next()?.to_owned(),
                time: fields.next()?.parse().ok()?,
                subject: fields.next()?.to_owned(),
            })
        })
        .collect()
}

/// The ahead and behind counts of `%(upstream:track)`: `[ahead 1, behind 2]`, `[gone]`
/// or empty.
fn track_counts(track: &str) -> (u32, u32) {
    let mut ahead = 0;
    let mut behind = 0;
    let track = track.trim();
    if track.is_empty() {
        return (ahead, behind);
    }
    for part in track.trim_matches(['[', ']']).split(',') {
        let part = part.trim();
        if let Some(count) = part.strip_prefix("ahead ") {
            ahead = count.parse().unwrap_or(0);
        } else if let Some(count) = part.strip_prefix("behind ") {
            behind = count.parse().unwrap_or(0);
        }
    }
    (ahead, behind)
}

fn stash_ref(index: usize) -> String {
    format!("stash@{{{index}}}")
}

/// A unique path in the temporary folder for an index or a script of a reword.
fn temp_path(kind: &str) -> PathBuf {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let serial = NEXT.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("ide-{kind}-{}-{serial}", std::process::id()))
}

/// An editor for git that puts `source` in place of the file git opens. Git runs the
/// editor through its own shell and adds the file as the last argument.
fn copy_command(source: &Path) -> String {
    format!("cp \"{}\"", source.to_string_lossy().replace('\\', "/"))
}

/// The original failure plus where git kept the local changes, so the window can say it.
fn stash_error(error: io::Error, stash: &Stash) -> io::Error {
    io::Error::other(format!(
        "{error}; local changes are kept in {}: {}",
        stash_ref(stash.index),
        stash.message
    ))
}

fn stash_index(reference: &str) -> Option<usize> {
    reference
        .strip_prefix("stash@{")?
        .strip_suffix('}')?
        .parse()
        .ok()
}

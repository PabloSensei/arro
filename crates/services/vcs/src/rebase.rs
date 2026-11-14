//! The plan of an interactive rebase: the commits from one commit to HEAD, the action of
//! each of them and the todo list the dialog of the Git Log hands to git.

use std::io;
use std::path::{Path, PathBuf};

use crate::git::GitRunner;

/// The folder of the todo list and the messages of a Reword, inside the git directory. It
/// lives there and not in the worktree: git stashes the worktree itself at
/// `--autostash`, and a file of the plan must survive that.
pub(crate) const FOLDER: &str = "ide-rebase";

const FIELD: char = '\0';
const RECORD: char = '\u{1e}';
const LOG_FORMAT: &str = "--format=%H%x00%an%x00%s%x00%B%x1e";

/// What git does with a commit of the plan, as the Rebase dialog of IDEA offers it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum RebaseAction {
    #[default]
    Pick,
    Edit,
    Reword,
    Squash,
    Fixup,
    Drop,
}

impl RebaseAction {
    /// The name in the action cell of a row and in the dropdown.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pick => "Pick",
            Self::Edit => "Edit",
            Self::Reword => "Reword",
            Self::Squash => "Squash",
            Self::Fixup => "Fixup",
            Self::Drop => "Drop",
        }
    }

    /// The command of the todo list of git.
    pub fn verb(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Edit => "edit",
            Self::Reword => "pick",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Drop => "drop",
        }
    }

    /// The name of the action in a key or a debug selector. Unlike the verb of the todo,
    /// every action has one of its own.
    pub fn name(self) -> &'static str {
        match self {
            Self::Pick => "pick",
            Self::Edit => "edit",
            Self::Reword => "reword",
            Self::Squash => "squash",
            Self::Fixup => "fixup",
            Self::Drop => "drop",
        }
    }

    /// The key that picks the action in the dialog, as IDEA has it.
    pub fn key(self) -> char {
        match self {
            Self::Pick => 'p',
            Self::Edit => 'e',
            Self::Reword => 'r',
            Self::Squash => 's',
            Self::Fixup => 'f',
            Self::Drop => 'd',
        }
    }

    /// The action of a key of the dialog, whatever its case.
    pub fn from_key(key: char) -> Option<Self> {
        let key = key.to_ascii_lowercase();
        [
            Self::Pick,
            Self::Edit,
            Self::Reword,
            Self::Squash,
            Self::Fixup,
            Self::Drop,
        ]
        .into_iter()
        .find(|action| action.key() == key)
    }

    /// The actions of the dropdown, in the order IDEA lists them.
    pub const ALL: [Self; 6] = [
        Self::Pick,
        Self::Edit,
        Self::Reword,
        Self::Squash,
        Self::Fixup,
        Self::Drop,
    ];
}

/// One commit of the plan and what the rebase does with it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebaseStep {
    pub id: String,
    pub subject: String,
    pub author: String,
    pub action: RebaseAction,
    /// The whole message of the commit, which Reword edits.
    pub message: String,
}

/// The commits of an interactive rebase, oldest first as the dialog lists them, and the
/// commit git replays them onto.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RebasePlan {
    /// The commit the plan starts after; `None` when it starts at the root commit.
    pub base: Option<String>,
    /// The commits from the oldest to HEAD.
    pub steps: Vec<RebaseStep>,
}

impl RebasePlan {
    /// Whether git can run the plan, and what is wrong with it when it cannot.
    pub fn check(&self) -> Result<(), String> {
        if self.steps.is_empty() {
            return Err("the plan has no commits".to_owned());
        }
        let mut picked = false;
        for step in &self.steps {
            if matches!(step.action, RebaseAction::Squash | RebaseAction::Fixup) && !picked {
                return Err(
                    "the first commit of the plan cannot be squashed or fixed up".to_owned(),
                );
            }
            if step.action != RebaseAction::Drop {
                picked = true;
            }
        }
        if !picked {
            return Err("the plan drops every commit".to_owned());
        }
        Ok(())
    }
}

/// What an interactive rebase left behind: the operation it waits for, the files in
/// conflict and the commit it stopped at for an edit.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RebaseReport {
    pub operation: crate::Operation,
    pub conflicts: Vec<String>,
    /// Git is holding the local changes and puts them back when the rebase ends.
    pub autostash: bool,
    /// Local changes git could not put back; they wait in this stash.
    pub stash: Option<crate::Stash>,
    /// The commit of an Edit that stopped the rebase, when nothing is in conflict.
    pub stopped: Option<String>,
}

/// The plan of the rebase that starts at `commit` and ends at HEAD. Merges are refused:
/// the todo list below is flat, and git would drop a merge commit of the range.
pub(crate) fn plan(git: &GitRunner, root: &Path, commit: &str) -> io::Result<RebasePlan> {
    let resolved = git.run(
        root,
        ["rev-parse", "--verify", &format!("{commit}^{{commit}}")],
    )?;
    let id = String::from_utf8_lossy(&resolved).trim().to_owned();
    git.run(root, ["merge-base", "--is-ancestor", &id, "HEAD"])
        .map_err(|_| io::Error::other("the commit is not on the current branch"))?;
    let parents = git.run(root, ["rev-list", "--parents", "-n", "1", &id])?;
    let parents: Vec<String> = String::from_utf8_lossy(&parents)
        .split_whitespace()
        .skip(1)
        .map(str::to_owned)
        .collect();
    if parents.len() > 1 {
        return Err(io::Error::other(
            "the commit is a merge commit, rebase it with git itself",
        ));
    }
    let base = parents.first().cloned();
    let range = match &base {
        Some(base) => format!("{base}..HEAD"),
        None => "HEAD".to_owned(),
    };
    let merges = git.run(root, ["rev-list", "--merges", &range])?;
    if !String::from_utf8_lossy(&merges).trim().is_empty() {
        return Err(io::Error::other(
            "the range has a merge commit, rebase it with git itself",
        ));
    }
    let output = git.run(root, ["log", "--reverse", LOG_FORMAT, &range])?;
    let output = String::from_utf8_lossy(&output);
    let mut steps = Vec::new();
    for record in output.split(RECORD) {
        let mut fields = record.trim_start_matches('\n').split(FIELD);
        let Some(id) = fields.next().filter(|id| !id.is_empty()) else {
            continue;
        };
        let Some(author) = fields.next() else {
            continue;
        };
        let Some(subject) = fields.next() else {
            continue;
        };
        let Some(message) = fields.next() else {
            continue;
        };
        steps.push(RebaseStep {
            id: id.to_owned(),
            subject: subject.replace(['\r', '\n'], " "),
            author: author.to_owned(),
            action: RebaseAction::Pick,
            message: message.trim_end_matches('\n').to_owned(),
        });
    }
    Ok(RebasePlan { base, steps })
}

/// Writes the todo list of the plan, with a file for each message a Reword writes, and
/// returns the path of the todo.
pub(crate) fn write_todo(plan: &RebasePlan, folder: &Path) -> io::Result<PathBuf> {
    plan.check().map_err(io::Error::other)?;
    let _ = std::fs::remove_dir_all(folder);
    std::fs::create_dir_all(folder)?;
    let mut todo = String::new();
    for (index, step) in plan.steps.iter().enumerate() {
        if step.action == RebaseAction::Drop {
            continue;
        }
        todo.push_str(&format!(
            "{} {} {}\n",
            step.action.verb(),
            step.id,
            step.subject
        ));
        if step.action == RebaseAction::Reword && !step.message.trim().is_empty() {
            let message = folder.join(format!("message-{index}.txt"));
            std::fs::write(&message, step.message.as_bytes())?;
            todo.push_str(&format!(
                "exec git commit --amend --allow-empty -F {}\n",
                shell_quote(&message)
            ));
        }
    }
    let path = folder.join("todo.txt");
    std::fs::write(&path, todo)?;
    Ok(path)
}

/// An editor for git that puts `source` in place of the file git opens. Git runs the
/// editor through its own shell and adds the file as the last argument, so `cp` works on
/// Windows, Linux and macOS.
pub(crate) fn copy_command(source: &Path) -> String {
    format!("cp {}", shell_quote(source))
}

/// Removes the todo list and the messages of a rebase that ended.
pub(crate) fn clean(folder: &Path) {
    let _ = std::fs::remove_dir_all(folder);
}

/// A path in double quotes for the shell of git: the separators are forward slashes and
/// the characters the shell reads inside double quotes are escaped.
fn shell_quote(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    let mut quoted = String::from("\"");
    for ch in text.chars() {
        if matches!(ch, '"' | '`' | '$' | '\\') {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('"');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(action: RebaseAction) -> RebaseStep {
        RebaseStep {
            id: "abc".to_owned(),
            subject: "subject".to_owned(),
            author: "Test".to_owned(),
            action,
            message: String::new(),
        }
    }

    fn plan_of(actions: &[RebaseAction]) -> RebasePlan {
        RebasePlan {
            base: None,
            steps: actions.iter().copied().map(step).collect(),
        }
    }

    #[test]
    fn the_first_commit_cannot_be_squashed_or_fixed_up() {
        for action in [RebaseAction::Squash, RebaseAction::Fixup] {
            let plan = plan_of(&[action, RebaseAction::Pick]);
            assert!(plan.check().is_err());
        }
        assert!(
            plan_of(&[RebaseAction::Pick, RebaseAction::Squash])
                .check()
                .is_ok()
        );
    }

    #[test]
    fn a_squash_after_a_dropped_commit_has_nothing_to_squash() {
        let plan = plan_of(&[RebaseAction::Drop, RebaseAction::Squash]);
        assert!(plan.check().is_err());
        let plan = plan_of(&[RebaseAction::Drop, RebaseAction::Pick, RebaseAction::Squash]);
        assert!(plan.check().is_ok());
    }

    #[test]
    fn a_plan_that_drops_every_commit_is_refused() {
        let plan = plan_of(&[RebaseAction::Drop, RebaseAction::Drop]);
        assert!(plan.check().is_err());
    }

    #[test]
    fn every_action_has_its_letter_and_its_verb() {
        for action in RebaseAction::ALL {
            assert_eq!(RebaseAction::from_key(action.key()), Some(action));
            assert!(!action.label().is_empty());
            assert!(!action.verb().is_empty());
            assert!(!action.name().is_empty());
        }
        let names: std::collections::HashSet<&str> = RebaseAction::ALL
            .iter()
            .map(|action| action.name())
            .collect();
        assert_eq!(names.len(), RebaseAction::ALL.len(), "a name of its own");
        assert_eq!(RebaseAction::from_key('R'), Some(RebaseAction::Reword));
    }

    #[test]
    fn a_path_of_the_shell_keeps_its_spaces_and_quotes() {
        let quoted = shell_quote(Path::new("C:\\a b\\o'b\"x"));
        assert_eq!(quoted, "\"C:/a b/o'b\\\"x\"");
    }
}

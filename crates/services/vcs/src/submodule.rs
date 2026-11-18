//! The submodules of a repository: their state, their own roots and the update of every
//! root at once.

use std::io;
use std::sync::atomic::AtomicU64;

use crate::repo::{LOG_FORMAT, commits_of};
use crate::{Commit, Repository, UpdateMode, UpdateReport};

/// What `git submodule status` says about the working tree of one submodule.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SubmoduleState {
    /// Checked out at the recorded commit.
    Initialized,
    /// Not checked out yet: `git submodule update --init` fills it.
    Uninitialized,
    /// Checked out at a commit other than the recorded one.
    Modified,
    /// The two sides of a conflict changed the pointer.
    Conflict,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Submodule {
    /// Relative to the root, with `/`.
    pub path: String,
    /// The commit the parent records, or the one the working tree has for a modified
    /// submodule.
    pub head: String,
    pub state: SubmoduleState,
}

/// Reads the lines of `git submodule status --recursive`.
pub fn parse_submodule_status(output: &str) -> Vec<Submodule> {
    output
        .lines()
        .filter_map(|line| {
            let line = line.trim_end_matches('\r');
            let marker = line.chars().next()?;
            let state = match marker {
                '-' => SubmoduleState::Uninitialized,
                '+' => SubmoduleState::Modified,
                'U' => SubmoduleState::Conflict,
                _ => SubmoduleState::Initialized,
            };
            let rest = &line[marker.len_utf8()..];
            let (head, rest) = rest.split_once(' ')?;
            let path = rest.split_once(" (").map_or(rest, |(path, _)| path).trim();
            if path.is_empty() {
                return None;
            }
            Some(Submodule {
                path: path.to_owned(),
                head: head.to_owned(),
                state,
            })
        })
        .collect()
}

/// What [`Repository::update_all`] left behind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RootsUpdate {
    /// The reports of the roots together: every conflict and the first stash.
    pub report: UpdateReport,
    /// The roots that failed, each line naming the root.
    pub errors: Vec<String>,
    /// The command of the journal that failed last, when one of the errors is its answer,
    /// so a notification opens the Console on it.
    pub failed_command: Option<u64>,
}

impl Repository {
    /// The submodules of this repository, recursively, in the order git lists them.
    pub fn submodules(&self) -> io::Result<Vec<Submodule>> {
        let output = self
            .git
            .run(&self.root, ["submodule", "status", "--recursive"])?;
        Ok(parse_submodule_status(&String::from_utf8_lossy(&output)))
    }

    /// The root itself and every checked-out submodule, each with its own repository.
    /// A path is the prefix inside this repository, empty for the root itself.
    pub fn roots(&self) -> io::Result<Vec<(String, Repository)>> {
        let mut roots = vec![(String::new(), self.fork())];
        for submodule in self.submodules()? {
            if submodule.state == SubmoduleState::Uninitialized {
                continue;
            }
            let directory = self.root.join(&submodule.path);
            if let Ok(repository) = Repository::open(self.git.clone(), &directory) {
                roots.push((submodule.path, repository));
            }
        }
        Ok(roots)
    }

    /// The repository a path relative to this root belongs to: this one, or the
    /// submodule whose prefix is the longest.
    pub fn root_of(&self, relative: &str) -> io::Result<(String, Repository)> {
        let mut chosen = (String::new(), self.fork());
        for (prefix, repository) in self.roots()? {
            if !prefix.is_empty()
                && (relative == prefix || relative.starts_with(&format!("{prefix}/")))
                && prefix.len() > chosen.0.len()
            {
                chosen = (prefix, repository);
            }
        }
        Ok(chosen)
    }

    /// The commit the parent records for the submodule and the one its working tree has.
    /// `None` when the path is no submodule.
    pub fn submodule_pointer(
        &self,
        path: &str,
    ) -> io::Result<Option<(Option<String>, Option<String>)>> {
        if !self
            .submodules()?
            .iter()
            .any(|submodule| submodule.path == path)
        {
            return Ok(None);
        }
        let recorded = self
            .run(["rev-parse", "--verify", &format!("HEAD:{path}")])
            .ok()
            .map(|id| id.trim().to_owned())
            .filter(|id| !id.is_empty());
        let working = self
            .git
            .run(&self.root.join(path), ["rev-parse", "--verify", "HEAD"])
            .ok()
            .map(|id| String::from_utf8_lossy(&id).trim().to_owned())
            .filter(|id| !id.is_empty());
        Ok(Some((recorded, working)))
    }

    /// The commits of the submodule between the recorded commit and the one the working
    /// tree has, oldest first, for the diff of a changed pointer.
    pub fn submodule_commits(
        &self,
        path: &str,
        from: Option<&str>,
        to: Option<&str>,
    ) -> io::Result<Vec<Commit>> {
        let directory = self.root.join(path);
        let repository = Repository::open(self.git.clone(), &directory)?;
        let mut args = vec!["log", LOG_FORMAT, "--max-count=200"];
        let range = match (from, to) {
            (Some(from), Some(to)) => Some(format!("{from}..{to}")),
            (Some(from), None) => Some(format!("{from}..HEAD")),
            (None, Some(to)) => Some(to.to_owned()),
            (None, None) => None,
        };
        if let Some(range) = range.as_deref() {
            args.push(range);
        }
        let output = repository.git.run(&repository.root, &args)?;
        let mut commits = commits_of(&String::from_utf8_lossy(&output));
        commits.reverse();
        Ok(commits)
    }

    /// Fetches and brings the upstream into every root, then initializes and updates the
    /// submodules. A root without an upstream keeps its work. Conflicts and a stash are
    /// in one report; a root that failed is named in `errors`, the others still update.
    pub fn update_all(&self, mode: UpdateMode, submodules: bool) -> io::Result<RootsUpdate> {
        let mut update = RootsUpdate::default();
        for (prefix, repository) in self.roots()? {
            match repository.update_with(mode) {
                Ok(report) => merge_report(&mut update.report, report, &prefix),
                Err(error) => {
                    update.failed_command = crate::command_id(&error).or(update.failed_command);
                    update
                        .errors
                        .push(format!("{}: {error}", label_of(&prefix)));
                }
            }
        }
        if submodules
            && let Err(error) = self
                .git
                .run(&self.root, ["submodule", "update", "--init", "--recursive"])
        {
            update.failed_command = crate::command_id(&error).or(update.failed_command);
            update.errors.push(error.to_string());
        }
        Ok(update)
    }

    pub fn fork(&self) -> Repository {
        Repository {
            git: self.git.clone(),
            root: self.root.clone(),
            status_reads: AtomicU64::new(0),
        }
    }
}

/// The name of a root in a notification: the project itself, or the submodule path.
fn label_of(prefix: &str) -> &str {
    if prefix.is_empty() { "root" } else { prefix }
}

fn merge_report(into: &mut UpdateReport, report: UpdateReport, prefix: &str) {
    if report.operation != crate::Operation::None && into.operation == crate::Operation::None {
        into.operation = report.operation;
    }
    for file in report.conflicts {
        into.conflicts.push(if prefix.is_empty() {
            file
        } else {
            format!("{prefix}/{file}")
        });
    }
    if into.merge_message.is_empty() {
        into.merge_message = report.merge_message;
    }
    into.autostash |= report.autostash;
    if into.stash.is_none() {
        into.stash = report.stash;
    }
}

#[cfg(test)]
mod tests {
    use super::{Submodule, SubmoduleState, parse_submodule_status};

    #[test]
    fn every_marker_of_the_status_is_read() {
        let output = [
            " 1111111111111111111111111111111111111111 sub (heads/main)",
            "+2222222222222222222222222222222222222222 nested",
            "-3333333333333333333333333333333333333333 missing",
            "U4444444444444444444444444444444444444444 both",
        ]
        .join("\n");

        let submodules = parse_submodule_status(&output);

        assert_eq!(
            submodules,
            [
                Submodule {
                    path: "sub".into(),
                    head: "1111111111111111111111111111111111111111".into(),
                    state: SubmoduleState::Initialized,
                },
                Submodule {
                    path: "nested".into(),
                    head: "2222222222222222222222222222222222222222".into(),
                    state: SubmoduleState::Modified,
                },
                Submodule {
                    path: "missing".into(),
                    head: "3333333333333333333333333333333333333333".into(),
                    state: SubmoduleState::Uninitialized,
                },
                Submodule {
                    path: "both".into(),
                    head: "4444444444444444444444444444444444444444".into(),
                    state: SubmoduleState::Conflict,
                },
            ]
        );
    }

    #[test]
    fn a_line_without_a_path_is_skipped() {
        assert!(parse_submodule_status("-").is_empty());
        assert!(parse_submodule_status("").is_empty());
    }
}

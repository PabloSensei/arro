//! A file an agent offers for review: the text on disk against the offered one, and the
//! hunks the user has decided. `propose_edit` of the MCP server and a harness that offers a
//! file through `openDiff` of the IDE protocol share this view, with the hunks, the marks
//! and the window of one hunk. Nothing here writes; the panel writes the text the user
//! kept once every hunk is decided and answers whoever waits.

use std::path::PathBuf;

use change_tracker::FileReview;

/// Who waits for the decision and hears its answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Waiter {
    /// The `propose_edit` call of an MCP client, waiting on the edit requests of the panel.
    Tool(u64),
    /// A harness that offered the file through `openDiff`, waiting on the bridge.
    Bridge(u64),
}

pub(crate) struct Proposal {
    /// Unique among the proposals of the window.
    pub id: u64,
    /// Who waits for the answer.
    pub waiter: Waiter,
    /// Absolute, inside the project.
    pub path: PathBuf,
    /// Relative to the project, with forward slashes.
    pub relative: String,
    /// The disk text against the proposed one.
    pub review: FileReview,
    /// `None` while the user has not decided a hunk, `Some(true)` for kept.
    decisions: Vec<Option<bool>>,
}

impl Proposal {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: u64,
        waiter: Waiter,
        path: PathBuf,
        relative: String,
        base: Option<String>,
        proposed: Option<String>,
    ) -> Self {
        let review = FileReview::new(
            relative.clone(),
            base.map(String::into_bytes),
            proposed.map(String::into_bytes),
        );
        let decisions = vec![None; review.hunks.len()];
        Self {
            id,
            waiter,
            path,
            relative,
            review,
            decisions,
        }
    }

    /// The hunks that still wait for the user, by their number in the review.
    pub fn undecided(&self) -> Vec<usize> {
        (0..self.decisions.len())
            .filter(|index| self.decisions[*index].is_none())
            .collect()
    }

    pub fn decided(&self) -> bool {
        self.decisions.iter().all(Option::is_some)
    }

    pub fn decide(&mut self, index: usize, accepted: bool) -> bool {
        if index >= self.decisions.len() || self.decisions[index].is_some() {
            return false;
        }
        self.decisions[index] = Some(accepted);
        true
    }

    pub fn decide_all(&mut self, accepted: bool) {
        for decision in &mut self.decisions {
            if decision.is_none() {
                *decision = Some(accepted);
            }
        }
    }

    /// What the review panel shows: the hunks that still wait for the user.
    pub fn view(&self) -> FileReview {
        let mut review = self.review.clone();
        review.hunks = self
            .undecided()
            .into_iter()
            .map(|index| self.review.hunks[index].clone())
            .collect();
        review
    }

    /// How many hunks the user kept.
    pub fn kept(&self) -> usize {
        self.decisions
            .iter()
            .filter(|decision| decision == &&Some(true))
            .count()
    }

    /// The text the user kept so far, `None` when the file should not exist.
    pub fn text(&self) -> Option<String> {
        let accepted: Vec<bool> = self
            .decisions
            .iter()
            .map(|decision| decision == &Some(true))
            .collect();
        self.review.text_with(&accepted)
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::{Proposal, Waiter};

    fn proposal() -> Proposal {
        Proposal::new(
            1,
            Waiter::Tool(0),
            PathBuf::from("src/A.txt"),
            "src/A.txt".to_string(),
            Some("alpha\nbeta\ngamma\n".to_string()),
            Some("ALPHA\nbeta\nGAMMA\n".to_string()),
        )
    }

    #[test]
    fn a_proposal_starts_with_every_hunk_undecided() {
        let proposal = proposal();

        assert_eq!(proposal.review.hunks.len(), 2);
        assert_eq!(proposal.undecided(), vec![0, 1]);
        assert!(!proposal.decided());
        assert_eq!(proposal.text().as_deref(), Some("alpha\nbeta\ngamma\n"));
    }

    #[test]
    fn kept_hunks_of_a_proposal_make_the_text_the_user_keeps() {
        let mut proposal = proposal();

        assert!(proposal.decide(0, true));
        assert!(!proposal.decided());
        assert_eq!(proposal.kept(), 1);
        assert_eq!(proposal.text().as_deref(), Some("ALPHA\nbeta\ngamma\n"));

        assert!(proposal.decide(1, false));
        assert!(proposal.decided());
        assert_eq!(proposal.view().hunks.len(), 0);
        assert_eq!(proposal.text().as_deref(), Some("ALPHA\nbeta\ngamma\n"));
    }

    #[test]
    fn a_hunk_does_not_take_a_second_decision() {
        let mut proposal = proposal();

        assert!(proposal.decide(0, true));
        assert!(!proposal.decide(0, false));
        assert!(!proposal.decide(5, true));
        assert_eq!(proposal.kept(), 1);
    }
}

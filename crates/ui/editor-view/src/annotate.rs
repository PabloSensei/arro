//! The blame column left of the text: what one line says about itself, and the hint over
//! its label. The column follows the text, so an edit moves the labels with their lines.

/// The blame of one line, as the column of the gutter shows it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameNote {
    /// The commit that changed the line; empty for a line that is not committed yet.
    pub commit: String,
    /// The date and the author, as the column shows them.
    pub label: String,
    /// What a hint over the label shows: the commit, its author, its date and its message.
    pub details: String,
}

impl BlameNote {
    /// Whether a click on the label opens a commit.
    pub fn has_commit(&self) -> bool {
        !self.commit.is_empty()
    }
}

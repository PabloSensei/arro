//! A single replacement and where it came from.

use std::ops::Range;

/// Replaces `range` with `text`. The edits of one change are sorted and do not overlap,
/// and all their ranges refer to the text before the change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Edit {
    pub range: Range<usize>,
    pub text: String,
}

impl Edit {
    pub fn new(range: Range<usize>, text: impl Into<String>) -> Self {
        Self {
            range,
            text: text.into(),
        }
    }

    pub fn insert(offset: usize, text: impl Into<String>) -> Self {
        Self::new(offset..offset, text)
    }

    pub fn delete(range: Range<usize>) -> Self {
        Self::new(range, "")
    }

    /// `offset` after this edit, for an offset at or after the end of its range.
    pub(crate) fn delta(&self) -> isize {
        self.text.len() as isize - self.range.len() as isize
    }
}

/// Who made a change. Reviewing what an agent did depends on this.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditSource {
    User,
    Agent(u64),
    External,
}

/// Decides how a change joins the previous one in the undo history.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditKind {
    /// Typing and deleting single characters, grouped within 300 ms.
    Typing,
    /// Everything else is an undo step of its own.
    Command,
}

/// One edit turning `old` into `new`: everything between the common prefix and suffix.
pub fn difference(old: &str, new: &str) -> Option<Edit> {
    if old == new {
        return None;
    }
    let mut prefix = old
        .bytes()
        .zip(new.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let longest = old.len().min(new.len()) - prefix;
    let mut suffix = old
        .bytes()
        .rev()
        .zip(new.bytes().rev())
        .take(longest)
        .take_while(|(a, b)| a == b)
        .count();
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    let range = prefix..old.len() - suffix;
    Some(Edit::new(range, &new[prefix..new.len() - suffix]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_difference_covers_only_what_changed_and_keeps_characters_whole() {
        assert_eq!(difference("abc", "abc"), None);
        assert_eq!(difference("abcd", "abXd"), Some(Edit::new(2..3, "X")));
        assert_eq!(difference("aa", "aaa"), Some(Edit::new(2..2, "a")));
        assert_eq!(difference("aaa", "a"), Some(Edit::new(1..3, "")));
        // `я` and `ю` share their first byte.
        assert_eq!(difference("xя", "xю"), Some(Edit::new(1..3, "ю")));
    }
}

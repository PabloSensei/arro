//! The history of a range of lines: `git log -L` follows the lines through the commits
//! that changed them and through the renames of the file.

use std::io;

use crate::{Commit, Repository};

const RECORD: char = '\u{1e}';
const FIELD: char = '\u{1f}';
/// The header of a commit of `git log -L`. The record marker leads the format, so the
/// patch of one commit ends where the header of the next one begins.
const HISTORY_FORMAT: &str = "--format=%x1e%H%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%s";

/// One commit that changed the lines, with the lines of the diff of that commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineHistory {
    pub commit: Commit,
    /// The path of the file at that commit, relative to the root, as git names it.
    pub path: String,
    /// The path before a rename, when the commit renamed the file.
    pub previous_path: Option<String>,
    /// The lines of the older version of the changed pieces, `\n` separated. Lines that
    /// did not change around them are not part of this text.
    pub before: String,
    /// The same lines of the newer version.
    pub after: String,
    /// The first line of the change in the newer version, counted from one.
    pub line: u32,
}

impl Repository {
    /// The commits that changed the lines `start..=end` of `path`, newest first. The
    /// numbers are counted from one, as the editor lists them. Renames come with the
    /// path the file had then.
    pub fn line_history(&self, path: &str, start: u32, end: u32) -> io::Result<Vec<LineHistory>> {
        let range = format!("{start},{end}:{path}");
        let output = self.run(["log", "-L", &range, HISTORY_FORMAT])?;
        Ok(parse_line_history(&output))
    }
}

/// The records of `git log -L` in [`HISTORY_FORMAT`].
fn parse_line_history(output: &str) -> Vec<LineHistory> {
    output
        .split(RECORD)
        .filter(|record| !record.trim().is_empty())
        .filter_map(parse_record)
        .collect()
}

fn parse_record(record: &str) -> Option<LineHistory> {
    let (header, patch) = record.split_once('\n').unwrap_or((record, ""));
    let mut fields = header.split(FIELD);
    let id = fields.next().filter(|id| !id.is_empty())?.trim().to_owned();
    let parents = fields
        .next()?
        .split_whitespace()
        .map(str::to_owned)
        .collect();
    let author = fields.next()?.to_owned();
    let email = fields.next()?.to_owned();
    let time = fields.next()?.trim().parse().ok()?;
    let subject = fields.next()?.trim_end_matches('\r').to_owned();
    let commit = Commit {
        id,
        parents,
        author,
        email,
        time,
        subject,
    };
    let mut path = None;
    let mut previous = None;
    let mut before = String::new();
    let mut after = String::new();
    let mut line = 0;
    let mut in_hunk = false;
    for text in patch.lines() {
        if text.starts_with("@@ ") {
            in_hunk = true;
            if line == 0
                && let Some(start) = hunk_start(text)
            {
                line = start;
            }
            continue;
        }
        if text.starts_with("diff --git ") {
            in_hunk = false;
            continue;
        }
        if !in_hunk {
            if let Some(name) = text.strip_prefix("+++ b/") {
                path.get_or_insert_with(|| name.to_owned());
            } else if let Some(name) = text.strip_prefix("--- a/")
                && previous.is_none()
            {
                previous = Some(name.to_owned());
            }
            continue;
        }
        let (side, text) = match text.chars().next() {
            Some(' ') => (Side::Both, &text[1..]),
            Some('-') => (Side::Before, &text[1..]),
            Some('+') => (Side::After, &text[1..]),
            // `\ No newline at end of file` is not a line of either version.
            _ => continue,
        };
        if matches!(side, Side::Before | Side::Both) {
            before.push_str(text);
            before.push('\n');
        }
        if matches!(side, Side::After | Side::Both) {
            after.push_str(text);
            after.push('\n');
        }
    }
    let path = path.or_else(|| previous.clone())?;
    // A commit that only renamed the file changes no line and has nothing to show.
    if before.is_empty() && after.is_empty() {
        return None;
    }
    let previous_path = previous.filter(|old| *old != path);
    Some(LineHistory {
        commit,
        path,
        previous_path,
        before,
        after,
        line,
    })
}

#[derive(Clone, Copy)]
enum Side {
    Both,
    Before,
    After,
}

/// The first line of a hunk in the newer version of the file: `@@ -2,1 +2,1 @@`.
fn hunk_start(header: &str) -> Option<u32> {
    let new = header.split_whitespace().nth(2)?;
    let new = new.strip_prefix('+')?;
    let start = new.split(',').next()?;
    start.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::{hunk_start, parse_record};

    #[test]
    fn a_hunk_header_names_the_line_of_the_new_version() {
        assert_eq!(hunk_start("@@ -2,1 +2,1 @@"), Some(2));
        assert_eq!(hunk_start("@@ -1 +1,3 @@ fn main"), Some(1));
        assert_eq!(hunk_start("@@ -1 +0,0 @@"), Some(0));
        assert_eq!(hunk_start("not a hunk"), None);
    }

    #[test]
    fn a_record_without_a_patch_is_not_a_commit_of_the_lines() {
        let record = "abc\u{1f}\u{1f}A\u{1f}a@b\u{1f}1\u{1f}rename\n";
        assert!(parse_record(record).is_none());
    }
}

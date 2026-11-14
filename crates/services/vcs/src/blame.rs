//! `git blame`: the commit that last changed every line of a file, as the annotation
//! column of the editor shows it.

use std::collections::HashMap;
use std::io;

use crate::Repository;

/// One line of `git blame`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlameLine {
    /// All zeros for a line that is not committed yet.
    pub commit: String,
    pub author: String,
    /// Without the angle brackets of the porcelain format.
    pub email: String,
    /// Seconds since 1970.
    pub time: i64,
    /// The time zone of the author, in minutes east of UTC.
    pub time_zone: i32,
    /// The subject of the commit.
    pub summary: String,
    /// The line of the file, from one.
    pub line: usize,
}

impl BlameLine {
    /// Whether a commit of the repository changed the line. A line that is not committed
    /// yet has no commit to open.
    pub fn is_committed(&self) -> bool {
        !self.commit.is_empty() && self.commit.bytes().any(|byte| byte != b'0')
    }
}

/// What the porcelain format gives for one commit: the first block of the commit carries
/// it, the later blocks of the same commit leave it out.
#[derive(Clone, Debug, Default)]
struct Meta {
    commit: String,
    author: String,
    email: String,
    time: i64,
    time_zone: i32,
    summary: String,
}

impl Meta {
    fn line(&self, line: usize) -> BlameLine {
        BlameLine {
            commit: self.commit.clone(),
            author: self.author.clone(),
            email: self.email.clone(),
            time: self.time,
            time_zone: self.time_zone,
            summary: self.summary.clone(),
            line,
        }
    }
}

/// The lines of `git blame --porcelain` output, in the order of the file.
pub fn parse_blame(output: &str) -> Vec<BlameLine> {
    let mut known: HashMap<String, Meta> = HashMap::new();
    let mut current: Option<Meta> = None;
    let mut number = 0;
    let mut lines = Vec::new();
    for raw in output.split('\n') {
        let raw = raw.strip_suffix('\r').unwrap_or(raw);
        // The text of a line of the file, with a tab before it. A line that starts with a
        // tab of its own has two.
        if raw.starts_with('\t') {
            if let Some(meta) = &current {
                known
                    .entry(meta.commit.clone())
                    .or_insert_with(|| meta.clone());
                lines.push(meta.line(number));
            }
            continue;
        }
        let mut fields = raw.split(' ');
        let Some(commit) = fields.next().filter(|field| is_commit(field)) else {
            if let Some((key, value)) = raw.split_once(' ')
                && let Some(meta) = &mut current
            {
                match key {
                    "author" => meta.author = value.to_owned(),
                    "author-mail" => meta.email = email_of(value),
                    "author-time" => meta.time = value.parse().unwrap_or(0),
                    "author-tz" => meta.time_zone = zone_of(value),
                    "summary" => meta.summary = value.to_owned(),
                    _ => {}
                }
            }
            continue;
        };
        let (Some(_source), Some(line)) = (fields.next(), fields.next()) else {
            continue;
        };
        let Ok(line) = line.parse() else {
            continue;
        };
        number = line;
        current = Some(known.get(commit).cloned().unwrap_or_else(|| Meta {
            commit: commit.to_owned(),
            ..Meta::default()
        }));
    }
    lines
}

/// A commit id of the porcelain format: the field is hex and long enough for a digest.
fn is_commit(field: &str) -> bool {
    field.len() >= 40 && field.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// `<alice@example.invalid>` as the metadata writes it.
fn email_of(value: &str) -> String {
    value
        .trim()
        .trim_start_matches('<')
        .trim_end_matches('>')
        .to_owned()
}

/// `+0200` in minutes east of UTC.
fn zone_of(value: &str) -> i32 {
    let value = value.trim();
    let (sign, rest) = match value.split_at_checked(1) {
        Some(("-", rest)) => (-1, rest),
        Some(("+", rest)) => (1, rest),
        _ => (1, value),
    };
    if rest.len() != 4 {
        return 0;
    }
    let hours: i32 = rest[..2].parse().unwrap_or(0);
    let minutes: i32 = rest[2..].parse().unwrap_or(0);
    sign * (hours * 60 + minutes)
}

/// How many commits one call of [`Repository::commit_messages`] asks about.
const MESSAGE_CHUNK: usize = 50;

impl Repository {
    /// The commit that last changed every line of `path`, as the working tree has it.
    pub fn blame(&self, path: &str) -> io::Result<Vec<BlameLine>> {
        let output = self
            .git()
            .run(self.root(), ["blame", "--porcelain", "--", path])?;
        Ok(parse_blame(&String::from_utf8_lossy(&output)))
    }

    /// The full message of every commit of `commits`, by id, for the hint of a label.
    pub fn commit_messages(&self, commits: &[String]) -> io::Result<HashMap<String, String>> {
        let mut messages = HashMap::new();
        for chunk in commits.chunks(MESSAGE_CHUNK) {
            let mut args = vec!["log", "--no-walk", "--format=%H%x1f%B%x1e"];
            args.extend(chunk.iter().map(String::as_str));
            let output = self.git().run(self.root(), &args)?;
            for record in String::from_utf8_lossy(&output).split('\u{1e}') {
                let Some((id, message)) = record.trim_start_matches('\n').split_once('\u{1f}')
                else {
                    continue;
                };
                messages.insert(id.to_owned(), message.trim_end().to_owned());
            }
        }
        Ok(messages)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Two committed lines of one commit, a line whose commit comes again later, and a
    /// line that is not committed yet. The second line starts with a tab of its own.
    const PORCELAIN: &str = "\
1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b 1 1 2
author Alice Smith
author-mail <alice@example.invalid>
author-time 1700000000
author-tz +0200
committer Alice Smith
committer-mail <alice@example.invalid>
committer-time 1700000000
committer-tz +0200
summary first change
previous 0000000000000000000000000000000000000000 sample.txt
filename sample.txt
\tfirst
1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b 2 2
\t\tindented
0000000000000000000000000000000000000000 3 3 1
author Not Committed Yet
author-mail <not.committed.yet>
author-time 1700000100
author-tz +0000
committer Not Committed Yet
committer-mail <not.committed.yet>
committer-time 1700000100
committer-tz +0000
summary Version of sample.txt from sample.txt
filename sample.txt
\tthird
1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b 4 4
\tfourth
";

    #[test]
    fn a_commit_that_comes_again_keeps_its_metadata() {
        let lines = parse_blame(PORCELAIN);

        assert_eq!(lines.len(), 4);
        assert_eq!(
            lines[0],
            BlameLine {
                commit: "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b".to_owned(),
                author: "Alice Smith".to_owned(),
                email: "alice@example.invalid".to_owned(),
                time: 1_700_000_000,
                time_zone: 120,
                summary: "first change".to_owned(),
                line: 1,
            }
        );
        assert_eq!(lines[1].author, "Alice Smith");
        assert_eq!(lines[1].line, 2);
        assert_eq!(lines[3].line, 4);
        assert_eq!(lines[3].summary, "first change");
    }

    #[test]
    fn a_line_that_is_not_committed_yet_has_no_commit() {
        let lines = parse_blame(PORCELAIN);

        assert_eq!(lines[2].author, "Not Committed Yet");
        assert_eq!(lines[2].email, "not.committed.yet");
        assert_eq!(lines[2].line, 3);
        assert!(!lines[2].is_committed());
        assert!(lines[0].is_committed());
    }

    #[test]
    fn a_line_of_an_unknown_commit_gets_what_the_block_says() {
        let text = "\
0000000000000000000000000000000000000000 1 1 1
author Bob
author-mail <bob@example.invalid>
author-time 1700000200
author-tz -0530
summary a subject with  spaces
\tone
";
        let lines = parse_blame(text);

        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].author, "Bob");
        assert_eq!(lines[0].time_zone, -330);
        assert_eq!(lines[0].summary, "a subject with  spaces");
        assert!(!lines[0].is_committed());
    }

    #[test]
    fn a_time_zone_is_read_as_minutes_east_of_utc() {
        assert_eq!(zone_of("+0200"), 120);
        assert_eq!(zone_of("-0530"), -330);
        assert_eq!(zone_of("+0000"), 0);
        assert_eq!(zone_of("weird"), 0);
    }

    #[test]
    fn an_empty_output_has_no_lines() {
        assert!(parse_blame("").is_empty());
    }
}

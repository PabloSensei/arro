/// One side of a change, from the `XY` letters of `git status`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Change {
    Unmodified,
    Modified,
    TypeChanged,
    Added,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Conflicted,
}

impl Change {
    fn from_letter(letter: u8) -> Self {
        match letter {
            b'M' => Self::Modified,
            b'T' => Self::TypeChanged,
            b'A' => Self::Added,
            b'D' => Self::Deleted,
            b'R' => Self::Renamed,
            b'C' => Self::Copied,
            b'U' => Self::Conflicted,
            _ => Self::Unmodified,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileStatus {
    /// Relative to the repository root, with `/`.
    pub path: String,
    /// The old path of a rename or copy.
    pub original: Option<String>,
    pub index: Change,
    pub worktree: Change,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Status {
    /// `None` on a detached head.
    pub branch: Option<String>,
    /// `None` before the first commit.
    pub head: Option<String>,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub files: Vec<FileStatus>,
}

/// Reads `git status --porcelain=v2 -z --branch`. Records the format does not know are
/// skipped: a newer git must not break the status.
pub fn parse_status(output: &[u8]) -> Status {
    let mut status = Status::default();
    let mut records = output
        .split(|byte| *byte == 0)
        .filter(|record| !record.is_empty());
    while let Some(record) = records.next() {
        let text = String::from_utf8_lossy(record);
        let mut fields = text.splitn(2, ' ');
        let (kind, rest) = (fields.next().unwrap_or(""), fields.next().unwrap_or(""));
        match kind {
            "#" => header(&mut status, rest),
            "1" => status.files.extend(entry(rest, 7, None, false)),
            "2" => {
                let original = records
                    .next()
                    .map(|path| String::from_utf8_lossy(path).into_owned());
                status.files.extend(entry(rest, 8, original, false));
            }
            "u" => status.files.extend(entry(rest, 9, None, true)),
            "?" => status.files.push(FileStatus {
                path: rest.to_owned(),
                original: None,
                index: Change::Unmodified,
                worktree: Change::Untracked,
            }),
            _ => {}
        }
    }
    status
}

fn header(status: &mut Status, line: &str) {
    let Some((name, value)) = line.split_once(' ') else {
        return;
    };
    match name {
        "branch.oid" if value != "(initial)" => status.head = Some(value.to_owned()),
        "branch.head" if value != "(detached)" => status.branch = Some(value.to_owned()),
        "branch.upstream" => status.upstream = Some(value.to_owned()),
        "branch.ab" => {
            let mut counts = value
                .split(' ')
                .map(|count| count.trim_start_matches(['+', '-']));
            status.ahead = counts
                .next()
                .and_then(|count| count.parse().ok())
                .unwrap_or(0);
            status.behind = counts
                .next()
                .and_then(|count| count.parse().ok())
                .unwrap_or(0);
        }
        _ => {}
    }
}

/// `fields` columns from `XY` on before the path, which may itself hold spaces.
fn entry(
    rest: &str,
    fields: usize,
    original: Option<String>,
    conflicted: bool,
) -> Option<FileStatus> {
    let mut parts = rest.splitn(fields + 1, ' ');
    let letters = parts.next()?.as_bytes();
    let path = parts.nth(fields - 1)?;
    let side = |letter| match conflicted {
        true => Change::Conflicted,
        false => Change::from_letter(letter),
    };
    Some(FileStatus {
        path: path.to_owned(),
        original,
        index: side(*letters.first()?),
        worktree: side(*letters.get(1)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str, index: Change, worktree: Change) -> FileStatus {
        FileStatus {
            path: path.into(),
            original: None,
            index,
            worktree,
        }
    }

    #[test]
    fn every_kind_of_record_is_read() {
        let output = [
            "# branch.oid 1234abcd",
            "# branch.head main",
            "# branch.upstream origin/main",
            "# branch.ab +2 -1",
            "1 .M N... 100644 100644 100644 aaa bbb src/a file.txt",
            "1 A. N... 000000 100644 100644 000 ccc новый.txt",
            "2 R. N... 100644 100644 100644 ddd eee R100 new.txt",
            "old.txt",
            "u UU N... 100644 100644 100644 100644 f1 f2 f3 both.txt",
            "? untracked.txt",
            "! ignored.txt",
            "",
        ]
        .join("\0");

        let status = parse_status(output.as_bytes());

        assert_eq!(status.head.as_deref(), Some("1234abcd"));
        assert_eq!(status.branch.as_deref(), Some("main"));
        assert_eq!(status.upstream.as_deref(), Some("origin/main"));
        assert_eq!((status.ahead, status.behind), (2, 1));
        let renamed = FileStatus {
            original: Some("old.txt".into()),
            ..file("new.txt", Change::Renamed, Change::Unmodified)
        };
        let expected = vec![
            file("src/a file.txt", Change::Unmodified, Change::Modified),
            file("новый.txt", Change::Added, Change::Unmodified),
            renamed,
            file("both.txt", Change::Conflicted, Change::Conflicted),
            file("untracked.txt", Change::Unmodified, Change::Untracked),
        ];
        assert_eq!(status.files, expected);
    }

    #[test]
    fn a_new_repository_has_no_head_and_a_detached_one_no_branch() {
        let status = parse_status(b"# branch.oid (initial)\0# branch.head (detached)\0");

        assert_eq!((status.head, status.branch), (None, None));
    }

    #[test]
    fn unknown_records_are_skipped() {
        let status = parse_status(b"# branch.future x\0z something new\0? a.txt\0");

        assert_eq!(status.files.len(), 1);
    }
}

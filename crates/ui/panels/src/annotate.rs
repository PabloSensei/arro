//! The blame column of an editor: the label of a line as the column shows it, and the
//! hint with the commit behind it.

use std::collections::HashMap;

use editor_view::BlameNote;
use vcs::BlameLine;
use workspace::{AnnotateNames, Settings};

/// What the column shows and how it names the author, as Options | View of its menu says.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AnnotateView {
    pub author: bool,
    pub date: bool,
    pub commit: bool,
    pub names: AnnotateNames,
}

impl AnnotateView {
    pub(crate) fn of(settings: &Settings) -> Self {
        Self {
            author: settings.annotate_author,
            date: settings.annotate_date,
            commit: settings.annotate_commit,
            names: settings.annotate_names,
        }
    }
}

/// The label and the hint of every blamed line, by line from one.
pub(crate) fn notes(
    lines: &[BlameLine],
    messages: &HashMap<String, String>,
    view: AnnotateView,
    now: i64,
) -> Vec<(u32, BlameNote)> {
    lines
        .iter()
        .map(|line| (line.line as u32, note_of(line, messages, view, now)))
        .collect()
}

fn note_of(
    line: &BlameLine,
    messages: &HashMap<String, String>,
    view: AnnotateView,
    now: i64,
) -> BlameNote {
    if !line.is_committed() {
        return BlameNote {
            commit: String::new(),
            label: "Not Committed Yet".to_owned(),
            details: "The line is not committed yet".to_owned(),
            time: now,
        };
    }
    let (date, time) = date_time(line.time, line.time_zone);
    let mut parts = Vec::new();
    if view.date {
        parts.push(date_label(line.time, line.time_zone, now));
    }
    if view.author {
        parts.push(view.names.format(&line.author, &line.email));
    }
    if view.commit {
        parts.push(short_commit(&line.commit));
    }
    let label = parts.join(" ");
    let message = messages
        .get(&line.commit)
        .map(String::as_str)
        .unwrap_or(&line.summary);
    let details = format!(
        "{}\n\n{} <{}>\n{} {}\n\n{}",
        line.commit,
        line.author,
        line.email,
        date,
        time,
        message.trim()
    );
    BlameNote {
        commit: line.commit.clone(),
        label,
        details,
        time: line.time,
    }
}

/// `Today` or `Yesterday` for a fresh commit, the short date of the system otherwise.
fn date_label(seconds: i64, zone: i32, now: i64) -> String {
    let days = (seconds + i64::from(zone) * 60).div_euclid(86_400);
    let today = (now + i64::from(zone) * 60).div_euclid(86_400);
    if days == today {
        "Today".to_owned()
    } else if days == today - 1 {
        "Yesterday".to_owned()
    } else {
        short_date(&date_time(seconds, zone).0)
    }
}

/// The first eight characters of a commit id, as the column shows it.
fn short_commit(commit: &str) -> String {
    commit.chars().take(8).collect()
}

/// `DD.MM.YYYY` as `DD.MM.YY`, the room of the column being short.
fn short_date(date: &str) -> String {
    match date.rsplit_once('.') {
        Some((rest, year)) if year.len() == 4 => format!("{rest}.{}", &year[2..]),
        _ => date.to_owned(),
    }
}

/// The date and the time of a commit in the zone of its author, from seconds since 1970.
fn date_time(seconds: i64, zone: i32) -> (String, String) {
    let seconds = seconds + i64::from(zone) * 60;
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil(days);
    (
        format!("{day:02}.{month:02}.{year:04}"),
        format!("{:02}:{:02}", rest / 3600, rest % 3600 / 60),
    )
}

/// The civil date of the days since 1970-01-01, as the calendar repeats every 400 years.
fn civil(days: i64) -> (i64, i64, i64) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

/// The time of this run, in seconds since 1970, for the date of a fresh commit.
pub(crate) fn now_seconds() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: AnnotateView = AnnotateView {
        author: true,
        date: true,
        commit: false,
        names: AnnotateNames::FullName,
    };
    /// 2023-11-14 22:13 UTC.
    const NOW: i64 = 1_700_000_000;

    fn line(commit: &str, author: &str, time: i64, zone: i32) -> BlameLine {
        BlameLine {
            commit: commit.to_owned(),
            author: author.to_owned(),
            email: format!("{}@example.invalid", author.replace(' ', ".")),
            time,
            time_zone: zone,
            summary: "a subject".to_owned(),
            line: 1,
        }
    }

    #[test]
    fn a_label_holds_the_date_and_the_author() {
        let blame = line(
            "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b",
            "Alice Smith",
            NOW,
            0,
        );
        let note = note_of(&blame, &HashMap::new(), DEFAULT, NOW);

        assert_eq!(note.commit, "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b");
        assert_eq!(note.label, "Today Alice Smith");
        assert_eq!(note.time, NOW);
        assert!(
            note.details
                .starts_with("1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b\n\nAlice Smith <Alice.Smith@example.invalid>\n")
        );
        assert!(note.details.ends_with("a subject"));
    }

    #[test]
    fn a_commit_of_yesterday_and_an_older_one_show_their_date() {
        assert_eq!(date_label(NOW - 86_400, 0, NOW), "Yesterday");
        assert_eq!(date_label(NOW - 3 * 86_400, 0, NOW), "11.11.23");
    }

    #[test]
    fn the_zone_of_the_author_moves_the_date() {
        let (date, time) = date_time(1_700_000_000, 120);
        assert_eq!((date.as_str(), time.as_str()), ("15.11.2023", "00:13"));
        // The day of the author may already be today while UTC is still yesterday.
        assert_eq!(date_label(NOW, 120, NOW), "Today");
    }

    #[test]
    fn the_view_chooses_the_parts_of_the_label() {
        let blame = line(
            "1a2b3c4d5e6f7a8b9c0d1e2f3a4b5c6d7e8f9a0b",
            "Alice Smith",
            NOW,
            0,
        );
        let date_only = AnnotateView {
            author: false,
            date: true,
            commit: false,
            names: AnnotateNames::LastName,
        };
        assert_eq!(
            note_of(&blame, &HashMap::new(), date_only, NOW).label,
            "Today"
        );
        let commit_only = AnnotateView {
            author: false,
            date: false,
            commit: true,
            names: AnnotateNames::LastName,
        };
        assert_eq!(
            note_of(&blame, &HashMap::new(), commit_only, NOW).label,
            "1a2b3c4d"
        );
    }

    #[test]
    fn the_names_follow_the_names_style() {
        let blame = line("1a2b3c", "Alice Mary Smith", NOW, 0);
        let label = |names| {
            note_of(
                &blame,
                &HashMap::new(),
                AnnotateView {
                    author: true,
                    date: false,
                    commit: false,
                    names,
                },
                NOW,
            )
            .label
        };
        assert_eq!(label(AnnotateNames::LastName), "Smith");
        assert_eq!(label(AnnotateNames::FirstName), "Alice");
        assert_eq!(label(AnnotateNames::FullName), "Alice Mary Smith");
        assert_eq!(label(AnnotateNames::Initials), "A.S.");
        assert_eq!(
            label(AnnotateNames::Email),
            "Alice.Mary.Smith@example.invalid"
        );
    }

    #[test]
    fn a_line_without_a_commit_has_no_commit_to_open() {
        let blame = line("0000000000000000000000000000000000000000", "x", 0, 0);
        let note = note_of(&blame, &HashMap::new(), DEFAULT, NOW);

        assert_eq!(note.label, "Not Committed Yet");
        assert!(!note.has_commit());
    }

    #[test]
    fn the_full_message_of_a_commit_reaches_the_hint() {
        let blame = line("1a2b3c", "Alice", NOW, 0);
        let messages = HashMap::from([("1a2b3c".to_owned(), "a subject\n\nand a body".to_owned())]);
        let note = note_of(&blame, &messages, DEFAULT, NOW);

        assert!(note.details.ends_with("a subject\n\nand a body"));
    }
}

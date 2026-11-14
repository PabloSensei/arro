//! The blame column of an editor: the label of a line as the column shows it, and the
//! hint with the commit behind it.

use std::collections::HashMap;

use editor_view::BlameNote;
use vcs::BlameLine;

/// How many characters of the date and the author the label holds before it is cut. The
/// code font of the label is narrow, so the whole column fits about this many.
const LABEL_CHARS: usize = 24;

/// The label and the hint of every blamed line, by line from one.
pub(crate) fn notes(
    lines: &[BlameLine],
    messages: &HashMap<String, String>,
) -> Vec<(u32, BlameNote)> {
    lines
        .iter()
        .map(|line| (line.line as u32, note_of(line, messages)))
        .collect()
}

fn note_of(line: &BlameLine, messages: &HashMap<String, String>) -> BlameNote {
    if !line.is_committed() {
        return BlameNote {
            commit: String::new(),
            label: "Not Committed Yet".to_owned(),
            details: "The line is not committed yet".to_owned(),
        };
    }
    let (date, time) = date_time(line.time, line.time_zone);
    let label = clip(
        &format!("{} {} {}", short_date(&date), time, line.author),
        LABEL_CHARS,
    );
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
    }
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

/// `text` cut to `chars` characters, the last one of the cut being an ellipsis.
fn clip(text: &str, chars: usize) -> String {
    if text.chars().count() <= chars {
        return text.to_owned();
    }
    let mut clipped: String = text.chars().take(chars.saturating_sub(1)).collect();
    clipped.push('…');
    clipped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(commit: &str, author: &str, time: i64, zone: i32) -> BlameLine {
        BlameLine {
            commit: commit.to_owned(),
            author: author.to_owned(),
            email: format!("{author}@example.invalid"),
            time,
            time_zone: zone,
            summary: "a subject".to_owned(),
            line: 1,
        }
    }

    #[test]
    fn a_label_holds_the_date_the_time_and_the_author() {
        let blame = line("1a2b3c", "Alice", 1_700_000_000, 0);
        let note = note_of(&blame, &HashMap::new());

        assert_eq!(note.commit, "1a2b3c");
        assert_eq!(note.label, "14.11.23 22:13 Alice");
        assert!(
            note.details
                .starts_with("1a2b3c\n\nAlice <Alice@example.invalid>\n")
        );
        assert!(note.details.ends_with("a subject"));
    }

    #[test]
    fn the_zone_of_the_author_moves_the_date() {
        let (date, time) = date_time(1_700_000_000, 120);
        assert_eq!((date.as_str(), time.as_str()), ("15.11.2023", "00:13"));
    }

    #[test]
    fn a_line_without_a_commit_has_no_commit_to_open() {
        let blame = line("0000000000000000000000000000000000000000", "x", 0, 0);
        let note = note_of(&blame, &HashMap::new());

        assert_eq!(note.label, "Not Committed Yet");
        assert!(!note.has_commit());
    }

    #[test]
    fn the_full_message_of_a_commit_reaches_the_hint() {
        let blame = line("1a2b3c", "Alice", 1_700_000_000, 0);
        let messages = HashMap::from([("1a2b3c".to_owned(), "a subject\n\nand a body".to_owned())]);
        let note = note_of(&blame, &messages);

        assert!(note.details.ends_with("a subject\n\nand a body"));
    }

    #[test]
    fn a_long_label_ends_with_an_ellipsis() {
        assert_eq!(clip("abcdef", 4), "abc…");
        assert_eq!(clip("abc", 4), "abc");
    }
}

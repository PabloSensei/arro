//! What the user chose from the completion list, by the prefix it was chosen for. The
//! list ranks an entry it saw chosen above an entry of the same kind it never saw; the
//! table goes to a file of the state folder so that it outlives a restart.

use std::path::{Path, PathBuf};

use ui_kit::*;

/// How many choices the table keeps. The least used, oldest entry goes first.
pub(crate) const MAX_USAGE: usize = 200;
/// The file under the state folder.
const FILE: &str = "completion-usage.tsv";

/// One choice: how often `label` was taken for `prefix`, and when it was taken last.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct UsageEntry {
    pub prefix: String,
    pub label: String,
    /// How many times the entry was taken for the prefix.
    pub count: u32,
    /// The tick of the last choice: a bigger number is more recent.
    pub last: u64,
}

/// The choices of the completion list of the window, and the file that keeps them.
#[derive(Default)]
pub struct CompletionUsage {
    entries: Vec<UsageEntry>,
    path: Option<PathBuf>,
    /// Grows with every choice, so a newer one wins between two entries of one count.
    clock: u64,
}

impl Global for CompletionUsage {}

impl CompletionUsage {
    /// The choices, as the list reads them.
    pub(crate) fn snapshot(&self) -> Vec<UsageEntry> {
        self.entries.clone()
    }

    /// Notes that `label` was chosen for `prefix`. Returns whether the table changed.
    pub(crate) fn remember(&mut self, prefix: &str, label: &str) -> bool {
        let prefix = prefix.trim();
        let label = label.trim();
        if prefix.is_empty() || label.is_empty() {
            return false;
        }
        self.clock = self.clock.saturating_add(1);
        record(&mut self.entries, prefix, label, self.clock);
        true
    }

    /// The file of the state folder and the text that keeps the table for the next start.
    pub(crate) fn file(&self) -> Option<(PathBuf, String)> {
        self.path.clone().map(|path| (path, render(&self.entries)))
    }
}

/// Adds one choice to `entries`, dropping the least used, oldest row over the limit.
fn record(entries: &mut Vec<UsageEntry>, prefix: &str, label: &str, clock: u64) {
    if let Some(entry) = entries
        .iter_mut()
        .find(|entry| entry.prefix == prefix && entry.label == label)
    {
        entry.count = entry.count.saturating_add(1);
        entry.last = clock;
    } else {
        entries.push(UsageEntry {
            prefix: prefix.to_owned(),
            label: label.to_owned(),
            count: 1,
            last: clock,
        });
    }
    while entries.len() > MAX_USAGE {
        let Some((index, _)) = entries
            .iter()
            .enumerate()
            .min_by_key(|(_, entry)| (entry.count, entry.last))
        else {
            return;
        };
        entries.remove(index);
    }
}

/// The table as the lines of the file: the count, the tick, the prefix and the label.
fn render(entries: &[UsageEntry]) -> String {
    let mut body = String::new();
    for entry in entries {
        body.push_str(&format!(
            "{}\t{}\t{}\t{}\n",
            entry.count, entry.last, entry.prefix, entry.label
        ));
    }
    body
}

/// The table of a file; a damaged line is left out rather than failing the whole file.
fn parse(body: &str) -> Vec<UsageEntry> {
    body.lines()
        .filter_map(|line| {
            let mut parts = line.splitn(4, '\t');
            let count = parts.next()?.parse().ok()?;
            let last = parts.next()?.parse().ok()?;
            let prefix = parts.next()?.trim();
            let label = parts.next()?.trim();
            if prefix.is_empty() || label.is_empty() {
                return None;
            }
            Some(UsageEntry {
                prefix: prefix.to_owned(),
                label: label.to_owned(),
                count,
                last,
            })
        })
        .take(MAX_USAGE)
        .collect()
}

/// Writes the table off the window thread.
pub(crate) fn save(path: PathBuf, body: String, cx: &App) {
    cx.background_executor()
        .spawn(async move {
            if let Some(folder) = path.parent() {
                let _ = std::fs::create_dir_all(folder);
            }
            let _ = std::fs::write(path, body);
        })
        .detach();
}

/// Reads the choices the window kept in `state_dir` and puts them in place. The window
/// calls this once, before the first editor is built; a second window keeps the table it
/// already has.
pub fn install_completion_usage(state_dir: &Path, cx: &mut App) {
    if cx.try_global::<CompletionUsage>().is_some() {
        return;
    }
    let path = state_dir.join(FILE);
    let entries = std::fs::read_to_string(&path)
        .map(|body| parse(&body))
        .unwrap_or_default();
    let clock = entries.iter().map(|entry| entry.last).max().unwrap_or(0);
    cx.set_global(CompletionUsage {
        entries,
        path: Some(path),
        clock,
    });
}

#[cfg(test)]
mod tests {
    use super::{MAX_USAGE, UsageEntry, parse, record, render};

    fn count_of(entries: &[UsageEntry], prefix: &str, label: &str) -> u32 {
        entries
            .iter()
            .find(|entry| entry.prefix == prefix && entry.label == label)
            .map_or(0, |entry| entry.count)
    }

    #[test]
    fn a_choice_is_counted_and_a_newer_one_has_a_bigger_tick() {
        let mut entries = Vec::new();
        record(&mut entries, "A", "ArrayList", 1);
        record(&mut entries, "A", "ArrayList", 2);
        record(&mut entries, "Str", "String", 3);
        assert_eq!(entries.len(), 2);
        assert_eq!(count_of(&entries, "A", "ArrayList"), 2);
        assert_eq!(count_of(&entries, "Str", "String"), 1);
        let array = entries
            .iter()
            .find(|entry| entry.label == "ArrayList")
            .unwrap();
        assert_eq!(array.last, 2, "the last choice keeps the newer tick");
    }

    #[test]
    fn a_table_reads_back_as_it_was_written() {
        let mut entries = Vec::new();
        record(&mut entries, "A", "ArrayList", 4);
        record(&mut entries, "A", "ArrayList", 5);
        record(&mut entries, "Str", "String", 6);
        let body = render(&entries);
        assert_eq!(parse(&body), entries);
        assert!(
            parse("broken\n1\t2\n\t\t\n").is_empty(),
            "a damaged file is empty"
        );
    }

    #[test]
    fn the_table_stops_at_its_limit_with_the_least_used_rows_gone() {
        let mut entries = Vec::new();
        for index in 0..MAX_USAGE {
            record(&mut entries, "A", &format!("Type{index}"), index as u64);
        }
        // The first row is chosen again and again: it must survive the overflow.
        for tick in 0..20 {
            record(&mut entries, "A", "Type0", (MAX_USAGE + tick) as u64);
        }
        record(&mut entries, "A", "Late", 1000);
        assert_eq!(entries.len(), MAX_USAGE, "the table keeps its limit");
        assert_eq!(count_of(&entries, "A", "Type0"), 21);
        assert!(
            entries.iter().any(|entry| entry.label == "Type0"),
            "a frequent row is not dropped"
        );
        assert!(
            !entries.iter().any(|entry| entry.label == "Type1"),
            "the least used rows go first"
        );
        assert!(parse(&render(&entries)).len() <= MAX_USAGE);
    }
}

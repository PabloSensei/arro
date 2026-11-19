//! The queries of the find bar: one list for every editor of the window, newest first,
//! kept in a file of the state folder so that it outlives a restart.

use std::path::{Path, PathBuf};

use ui_kit::*;

/// How many queries the list keeps.
const MAX_QUERIES: usize = 20;
/// The file under the state folder.
const FILE: &str = "find-history.txt";
/// The first line of the file of this version. A query holds lines of its own, so the
/// queries are escaped and the file says which way it is written. A file of an earlier
/// run has no such line and keeps one query per line.
const VERSION: &str = "#find-history-v2";

/// The queries of the find bar of the window, newest first.
#[derive(Default)]
pub struct FindHistory {
    queries: Vec<String>,
    path: Option<PathBuf>,
}

impl Global for FindHistory {}

impl FindHistory {
    /// The queries, newest first.
    pub fn entries(&self) -> &[String] {
        &self.queries
    }

    /// Puts `query` at the top of the list, dropping an older copy and the oldest entry
    /// over the limit. Returns whether the list changed. Only a query of blanks is left
    /// out; the blanks around and inside a query are a part of it.
    pub(crate) fn remember(&mut self, query: &str) -> bool {
        if query.trim().is_empty() || self.queries.first().is_some_and(|first| first == query) {
            return false;
        }
        self.queries.retain(|kept| kept != query);
        self.queries.insert(0, query.to_owned());
        self.queries.truncate(MAX_QUERIES);
        true
    }

    /// The file of the state folder and the text that keeps the list for the next start.
    pub(crate) fn file(&self) -> Option<(PathBuf, String)> {
        self.path.clone().map(|path| (path, encode(&self.queries)))
    }
}

/// The text of the file: the version line, then a line per query, with the characters
/// that a line cannot hold written as escapes.
fn encode(queries: &[String]) -> String {
    let mut body = String::from(VERSION);
    for query in queries {
        body.push('\n');
        for character in query.chars() {
            match character {
                '\\' => body.push_str("\\\\"),
                '\n' => body.push_str("\\n"),
                '\r' => body.push_str("\\r"),
                other => body.push(other),
            }
        }
    }
    body
}

/// The queries of a saved body. A body of an earlier run has no version line: its lines
/// are the queries already, without escapes and with the blanks around them trimmed.
fn decode(body: &str) -> Vec<String> {
    let escaped = body.starts_with(VERSION);
    let rest = if escaped {
        body[VERSION.len()..].strip_prefix('\n').unwrap_or("")
    } else {
        body
    };
    rest.lines()
        .map(|line| {
            if escaped {
                unescape(line)
            } else {
                line.trim().to_owned()
            }
        })
        .filter(|query| !query.trim().is_empty())
        .take(MAX_QUERIES)
        .collect()
}

/// A line of the file turned back into the query it holds.
fn unescape(line: &str) -> String {
    let mut query = String::with_capacity(line.len());
    let mut characters = line.chars();
    while let Some(character) = characters.next() {
        if character != '\\' {
            query.push(character);
            continue;
        }
        match characters.next() {
            Some('n') => query.push('\n'),
            Some('r') => query.push('\r'),
            Some('\\') => query.push('\\'),
            Some(other) => {
                query.push('\\');
                query.push(other);
            }
            None => query.push('\\'),
        }
    }
    query
}

/// Writes the history off the window thread.
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

/// Reads the queries the window kept in `state_dir` and puts them in place. The window
/// calls this once, before the first editor is built; a second window keeps the list it
/// already has.
pub fn install_find_history(state_dir: &Path, cx: &mut App) {
    if cx.try_global::<FindHistory>().is_some() {
        return;
    }
    let path = state_dir.join(FILE);
    let queries = std::fs::read_to_string(&path)
        .map(|body| decode(&body))
        .unwrap_or_default();
    cx.set_global(FindHistory {
        queries,
        path: Some(path),
    });
}

#[cfg(test)]
mod tests {
    use super::{FindHistory, decode, encode};

    #[test]
    fn a_query_with_lines_and_blanks_comes_back_as_it_was() {
        let mut history = FindHistory::default();
        assert!(history.remember("  a\n b "));
        assert!(history.remember("plain"));
        assert_eq!(history.entries(), ["plain", "  a\n b "]);

        let body = encode(history.entries());
        assert_eq!(decode(&body), ["plain", "  a\n b "]);
    }

    #[test]
    fn a_line_of_the_old_file_is_a_query() {
        assert_eq!(decode("alpha\n beta \n\n"), ["alpha", "beta"]);
        assert_eq!(decode(""), Vec::<String>::new());
    }

    #[test]
    fn a_backslash_is_a_part_of_its_query() {
        let mut history = FindHistory::default();
        assert!(history.remember("a\\nb"));
        assert!(history.remember("C:\\temp\\"));
        let body = encode(history.entries());
        assert_eq!(decode(&body), ["C:\\temp\\", "a\\nb"]);
    }

    #[test]
    fn a_query_of_blanks_is_not_remembered() {
        let mut history = FindHistory::default();
        assert!(!history.remember(""));
        assert!(!history.remember("   "));
        assert!(!history.remember("\n\t"));
        assert!(history.entries().is_empty());
    }

    #[test]
    fn the_newest_query_is_first_once() {
        let mut history = FindHistory::default();
        assert!(history.remember("a"));
        assert!(history.remember("b"));
        assert!(!history.remember("b"), "the newest query is kept once");
        assert!(history.remember("a"));
        assert_eq!(history.entries(), ["a", "b"]);
    }
}

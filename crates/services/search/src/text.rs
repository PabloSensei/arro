//! Find in Files and Replace in Files. No index: the files of the project are read on a
//! pool of threads by the ripgrep searcher, open documents are searched in their current
//! text instead of the disk.

use std::collections::HashMap;
use std::fmt;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use base::Cancel;
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::sinks::Lossy;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder};
use regex::{Regex, RegexBuilder};
use text_core::Edit;

use crate::symbols::pool_size;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextQuery {
    pub pattern: String,
    pub regex: bool,
    pub match_case: bool,
    pub whole_words: bool,
}

impl TextQuery {
    pub fn plain(pattern: impl Into<String>) -> Self {
        Self {
            pattern: pattern.into(),
            ..Self::default()
        }
    }

    fn source(&self) -> String {
        let pattern = if self.regex {
            self.pattern.clone()
        } else {
            regex::escape(&self.pattern)
        };
        if self.whole_words {
            format!(r"\b(?:{pattern})\b")
        } else {
            pattern
        }
    }

    fn matcher(&self) -> Result<RegexMatcher, SearchError> {
        RegexMatcherBuilder::new()
            .case_insensitive(!self.match_case)
            .line_terminator(Some(b'\n'))
            .build(&self.source())
            .map_err(|error| SearchError::Pattern(error.to_string()))
    }

    fn regex(&self) -> Result<Regex, SearchError> {
        RegexBuilder::new(&self.source())
            .case_insensitive(!self.match_case)
            .multi_line(true)
            .build()
            .map_err(|error| SearchError::Pattern(error.to_string()))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SearchError {
    Pattern(String),
}

impl fmt::Display for SearchError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Pattern(message) => write!(f, "bad pattern: {message}"),
        }
    }
}

impl std::error::Error for SearchError {}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LineHit {
    /// 1-based.
    pub line: u64,
    /// The line without its line end.
    pub text: String,
    /// Byte ranges in `text`.
    pub matches: Vec<Range<usize>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileHits {
    pub path: PathBuf,
    pub lines: Vec<LineHit>,
}

/// Searches `files`, taking the text of `open` documents over the disk. Blocks, runs off
/// the UI thread. `on_file` gets every file with hits, from the pool threads, in no order.
pub fn find_in_files(
    query: &TextQuery,
    files: &[PathBuf],
    open: &HashMap<PathBuf, String>,
    cancel: &Cancel,
    on_file: &(dyn Fn(FileHits) + Sync),
) -> Result<(), SearchError> {
    let started = std::time::Instant::now();
    let result = find_in_files_inner(query, files, open, cancel, on_file);
    base::perf::observe("search.files", started.elapsed());
    result
}

fn find_in_files_inner(
    query: &TextQuery,
    files: &[PathBuf],
    open: &HashMap<PathBuf, String>,
    cancel: &Cancel,
    on_file: &(dyn Fn(FileHits) + Sync),
) -> Result<(), SearchError> {
    let matcher = query.matcher()?;
    let next = AtomicUsize::new(0);
    thread::scope(|scope| {
        for _ in 0..pool_size().min(files.len()) {
            let matcher = matcher.clone();
            let next = &next;
            scope.spawn(move || {
                let mut searcher = SearcherBuilder::new()
                    .binary_detection(BinaryDetection::quit(0))
                    .line_number(true)
                    .build();
                while !cancel.is_cancelled() {
                    let Some(path) = files.get(next.fetch_add(1, Ordering::Relaxed)) else {
                        break;
                    };
                    let text = open.get(path).map(String::as_str);
                    let hits = search_one(&mut searcher, &matcher, path, text, cancel);
                    if !hits.is_empty() {
                        let path = path.clone();
                        on_file(FileHits { path, lines: hits });
                    }
                }
            });
        }
    });
    Ok(())
}

fn search_one(
    searcher: &mut Searcher,
    matcher: &RegexMatcher,
    path: &Path,
    text: Option<&str>,
    cancel: &Cancel,
) -> Vec<LineHit> {
    let mut hits = Vec::new();
    let sink = Lossy(|line: u64, text: &str| {
        let text = text.trim_end_matches(['\n', '\r']);
        let mut matches = Vec::new();
        let _ = matcher.find_iter(text.as_bytes(), |found| {
            if !found.is_empty() {
                matches.push(found.start()..found.end());
            }
            true
        });
        if !matches.is_empty() {
            let text = text.to_owned();
            hits.push(LineHit {
                line,
                text,
                matches,
            });
        }
        Ok(!cancel.is_cancelled())
    });
    // An unreadable file has no hits; the watcher reports it if it changes.
    let _ = match text {
        Some(text) => searcher.search_slice(matcher, text.as_bytes(), sink),
        None => searcher.search_path(matcher, path, sink),
    };
    hits
}

/// The edits Replace makes in one text. With a regex, `$1` and `${name}` in `replacement`
/// refer to groups. The edits are sorted and do not overlap.
pub fn replace_edits(
    query: &TextQuery,
    replacement: &str,
    text: &str,
) -> Result<Vec<Edit>, SearchError> {
    let regex = query.regex()?;
    let mut edits = Vec::new();
    for captures in regex.captures_iter(text) {
        let Some(found) = captures.get(0).filter(|found| !found.is_empty()) else {
            continue;
        };
        let mut new_text = String::new();
        if query.regex {
            captures.expand(replacement, &mut new_text);
        } else {
            new_text.push_str(replacement);
        }
        edits.push(Edit::new(found.range(), new_text));
    }
    Ok(edits)
}

/// Finds `query` only inside `range` of `text`, which is the selection of an open
/// editor. The line numbers are those of the whole file. Blocks, runs off the UI thread.
pub fn find_in_selection(
    query: &TextQuery,
    path: &Path,
    text: &str,
    range: Range<usize>,
) -> Result<FileHits, SearchError> {
    let matcher = query.matcher()?;
    let start = range.start.min(text.len());
    let end = range.end.max(start).min(text.len());
    let start_line = text[..start].bytes().filter(|byte| *byte == b'\n').count() as u64;
    let mut searcher = SearcherBuilder::new()
        .binary_detection(BinaryDetection::quit(0))
        .line_number(true)
        .build();
    let cancel = Cancel::new();
    let mut lines = search_one(
        &mut searcher,
        &matcher,
        path,
        Some(&text[start..end]),
        &cancel,
    );
    for hit in &mut lines {
        hit.line += start_line;
    }
    Ok(FileHits {
        path: path.to_path_buf(),
        lines,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_and_case_are_honoured() {
        let text = "Owner owner owners";
        let query = TextQuery {
            pattern: "owner".into(),
            whole_words: true,
            ..TextQuery::default()
        };
        let edits = replace_edits(&query, "x", text).unwrap();
        assert_eq!(edits.len(), 2);

        let query = TextQuery {
            match_case: true,
            ..query
        };
        let edits = replace_edits(&query, "x", text).unwrap();
        assert_eq!(edits, [Edit::new(6..11, "x")]);
    }

    #[test]
    fn a_plain_pattern_is_not_a_regex_and_groups_work_in_a_regex() {
        let plain = TextQuery::plain("a.b");
        assert_eq!(replace_edits(&plain, "$1", "axb a.b").unwrap().len(), 1);
        assert_eq!(
            replace_edits(&plain, "$1", "a.b").unwrap()[0].text,
            "$1",
            "a plain replacement is taken as is"
        );

        let regex = TextQuery {
            pattern: r"get(\w+)\(\)".into(),
            regex: true,
            match_case: true,
            ..TextQuery::default()
        };
        let edits = replace_edits(&regex, "read$1()", "x.getName();").unwrap();
        assert_eq!(edits, [Edit::new(2..11, "readName()")]);
    }

    #[test]
    fn a_selection_search_keeps_the_lines_of_the_whole_file() {
        let text = "one\ntwo one\nthree\none\n";
        let start = text.find("two one").unwrap();
        let end = start + "two one".len();
        let hits = find_in_selection(
            &TextQuery::plain("one"),
            Path::new("a.txt"),
            text,
            start..end,
        )
        .unwrap();
        assert_eq!(hits.lines.len(), 1);
        assert_eq!(hits.lines[0].line, 2);
        assert_eq!(hits.lines[0].text, "two one");
    }

    #[test]
    fn a_broken_regex_is_reported() {
        let query = TextQuery {
            pattern: "(".into(),
            regex: true,
            ..TextQuery::default()
        };
        assert!(matches!(
            replace_edits(&query, "", "x"),
            Err(SearchError::Pattern(_))
        ));
    }
}

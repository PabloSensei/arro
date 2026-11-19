//! The query over the lines of a console: the search bar, its options, the lines a query
//! matched and the lines a filter keeps. Both consoles of the window use it, so Ctrl+F,
//! F3 and the funnel behave the same in Run and Debug.

use std::ops::Range;
use std::path::Path;

use search::{TextQuery, find_in_selection};
use ui_kit::{AnyElement, TextField, Theme};

/// An option of the search bar.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SearchToggle {
    Case,
    Word,
    Regex,
}

/// A line a query matched, with the byte ranges of the matches inside it.
#[derive(Clone, Debug, Default)]
pub(crate) struct LineMatch {
    pub line: usize,
    pub ranges: Vec<Range<usize>>,
}

/// The state of the search bar and of the filter bar of one console.
#[derive(Default)]
pub(crate) struct ConsoleSearch {
    /// The search bar is open.
    pub open: bool,
    pub field: TextField,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
    /// The lines that hold the query, in the order of the console.
    matches: Vec<LineMatch>,
    /// The place in `matches` of the line the view moved to.
    current: Option<usize>,
    /// Why the pattern did not run.
    error: Option<String>,
    /// The filter bar is open.
    pub filter_open: bool,
    /// The text the filter keeps lines by.
    pub filter: TextField,
    /// `filter.text` in lower case, so a line is tested without copying the query.
    needle: String,
    /// The lines the filter keeps, in the order of the console; `None` for no filter.
    kept: Option<Vec<usize>>,
}

impl ConsoleSearch {
    /// Runs the query over `lines` and keeps only the lines the filter shows.
    pub fn run(&mut self, lines: &[&str]) {
        self.matches.clear();
        self.current = None;
        self.error = None;
        if self.field.text.is_empty() {
            return;
        }
        let query = TextQuery {
            pattern: self.field.text.clone(),
            regex: self.regex,
            match_case: self.case_sensitive,
            whole_words: self.whole_word,
            preserve_case: false,
        };
        let text = lines.join("\n");
        let hits = match find_in_selection(&query, Path::new("console"), &text, 0..text.len()) {
            Ok(hits) => hits,
            Err(error) => {
                self.error = Some(error.to_string());
                return;
            }
        };
        let mut matches = Vec::with_capacity(hits.lines.len());
        for hit in hits.lines {
            let Some(line) = (hit.line as usize).checked_sub(1) else {
                continue;
            };
            if self.keeps(line) {
                matches.push(LineMatch {
                    line,
                    ranges: hit.matches,
                });
            }
        }
        self.matches = matches;
        if !self.matches.is_empty() {
            self.current = Some(0);
        }
    }

    /// Moves to the next match, or to the previous one at the end.
    pub fn step(&mut self, forward: bool) {
        let count = self.matches.len();
        if count == 0 {
            self.current = None;
            return;
        }
        let next = match self.current {
            Some(current) if forward => (current + 1) % count,
            Some(current) => (current + count - 1) % count,
            None if forward => 0,
            None => count - 1,
        };
        self.current = Some(next);
    }

    /// The line the search moved to, counted from zero.
    pub fn current_line(&self) -> Option<usize> {
        self.current
            .and_then(|current| self.matches.get(current))
            .map(|found| found.line)
    }

    /// Moves the search to the first match at or after `line`, when the query still finds
    /// it: new output does not move the view away from the match the user reads.
    pub fn focus_line(&mut self, line: usize) {
        if let Some(current) = self.matches.iter().position(|found| found.line >= line) {
            self.current = Some(current);
        }
    }

    /// The byte ranges of the matches of a line, for its colours.
    pub fn ranges(&self, line: usize) -> &[Range<usize>] {
        match self.matches.binary_search_by_key(&line, |found| found.line) {
            Ok(index) => &self.matches[index].ranges,
            Err(_) => &[],
        }
    }

    /// Forgets the matches and the error, as a console that starts a new run does.
    pub fn clear(&mut self) {
        self.matches.clear();
        self.current = None;
        self.error = None;
    }

    /// Empties the filter, as a closed filter bar does.
    pub fn clear_filter(&mut self) {
        self.filter = TextField::default();
        self.needle.clear();
        self.kept = None;
    }

    /// The text of the counter and whether it is an error. Empty before a query.
    pub fn status(&self) -> (String, bool) {
        if let Some(error) = &self.error {
            return (error.clone(), true);
        }
        if self.field.text.is_empty() {
            return (String::new(), false);
        }
        if self.matches.is_empty() {
            return ("No matches".to_owned(), false);
        }
        let current = self.current.map_or(1, |current| current + 1);
        (format!("{current}/{}", self.matches.len()), false)
    }

    /// Turns an option of the search bar on or off.
    pub fn toggle(&mut self, toggle: SearchToggle) {
        match toggle {
            SearchToggle::Case => self.case_sensitive = !self.case_sensitive,
            SearchToggle::Word => self.whole_word = !self.whole_word,
            SearchToggle::Regex => self.regex = !self.regex,
        }
    }

    pub fn filtering(&self) -> bool {
        self.kept.is_some()
    }

    /// Rebuilds the lines the filter keeps from `lines`.
    pub fn apply_filter(&mut self, lines: &[&str]) {
        self.needle = self.filter.text.trim().to_lowercase();
        self.kept = if self.needle.is_empty() {
            None
        } else {
            Some(
                lines
                    .iter()
                    .enumerate()
                    .filter(|(_, text)| self.accepts_text(text))
                    .map(|(index, _)| index)
                    .collect(),
            )
        };
    }

    /// Whether the filter keeps a line with this text.
    pub fn accepts_text(&self, text: &str) -> bool {
        self.needle.is_empty() || text.to_lowercase().contains(&self.needle)
    }

    /// Whether the filter keeps the line at `index`.
    fn keeps(&self, index: usize) -> bool {
        self.kept
            .as_ref()
            .is_none_or(|kept| kept.binary_search(&index).is_ok())
    }

    /// How many of `total` lines the filter keeps.
    pub fn kept_count(&self, total: usize) -> usize {
        self.kept.as_ref().map_or(total, Vec::len)
    }

    /// The line of the console a row of the filtered list shows.
    pub fn line_of_row(&self, total: usize, row: usize) -> usize {
        self.kept.as_ref().map_or(row, |kept| {
            kept.get(row)
                .copied()
                .unwrap_or_else(|| total.saturating_sub(1))
        })
    }

    /// The row of the filtered list that shows `line`, when the filter keeps it.
    pub fn row_of_line(&self, line: usize) -> Option<usize> {
        match &self.kept {
            None => Some(line),
            Some(kept) => kept.binary_search(&line).ok(),
        }
    }
}

/// The drawn parts of the query field of a console or of its filter: `name` marks the
/// caret and the selection for the tests of the window.
pub(crate) fn field_parts(
    name: &'static str,
    field: &TextField,
    theme: &Theme,
    show_caret: bool,
) -> Vec<AnyElement> {
    ui_kit::text_field_children_named(
        name,
        field,
        theme.text,
        theme.text,
        theme.selection,
        theme.caret,
        16.,
        show_caret,
    )
}

#[cfg(test)]
mod tests {
    use super::ConsoleSearch;

    fn search(text: &str) -> ConsoleSearch {
        ConsoleSearch {
            field: ui_kit::TextField::from_text(text),
            ..ConsoleSearch::default()
        }
    }

    #[test]
    fn a_query_counts_the_matching_lines_and_steps_through_them() {
        let mut search = search("boom");
        search.run(&["alpha", "boom one", "beta", "boom two"]);

        assert_eq!(search.status().0, "1/2");
        assert_eq!(search.ranges(1).len(), 1);
        search.step(true);
        assert_eq!(search.current_line(), Some(3));
        assert_eq!(search.status().0, "2/2");
        search.step(true);
        assert_eq!(search.current_line(), Some(1), "the last match leads back");
    }

    #[test]
    fn the_options_change_what_the_query_finds() {
        let mut search = search("owner");
        search.run(&["owners", "owner"]);
        assert_eq!(search.status().0, "1/2");

        search.toggle(super::SearchToggle::Word);
        search.run(&["owners", "owner"]);
        assert_eq!(search.status().0, "1/1");
        assert_eq!(search.current_line(), Some(1), "the whole word only");

        search.toggle(super::SearchToggle::Word);
        search.toggle(super::SearchToggle::Case);
        search.run(&["Owner"]);
        assert_eq!(search.status().0, "No matches");
    }

    #[test]
    fn the_filter_hides_lines_from_the_rows_and_from_the_query() {
        let mut search = ConsoleSearch {
            filter: ui_kit::TextField::from_text("keep"),
            ..ConsoleSearch::default()
        };
        search.apply_filter(&["keep one", "drop", "KEEP two"]);
        assert_eq!(search.kept_count(3), 2);
        assert_eq!(search.line_of_row(3, 1), 2);
        assert_eq!(search.row_of_line(2), Some(1));

        search.field = ui_kit::TextField::from_text("two");
        search.run(&["keep one", "drop", "KEEP two"]);
        assert_eq!(search.status().0, "1/1", "a hidden line is no match");
    }
}

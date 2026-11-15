//! Searching the text of the scrollback. The engine hands over the rows it owns; this
//! module turns a query and those rows into the runs the panel paints.

use regex::RegexBuilder;

use crate::screen::{SearchMatch, SearchResults};
use crate::session::SearchQuery;

/// One row of the terminal as text, with the column every character starts at.
pub(crate) struct RowText {
    /// Counted like [`SearchMatch::line`]: negative above the live screen.
    pub(crate) line: i32,
    pub(crate) text: String,
    /// The first column of every character of `text`, in its order.
    columns: Vec<u16>,
    /// The column one past the last half of every character of `text`.
    ends: Vec<u16>,
}

impl RowText {
    /// `cells` yields `(column, character, wide)` of a row, in the order of its columns.
    pub(crate) fn new(line: i32, cells: impl Iterator<Item = (u16, char, bool)>) -> Self {
        let mut text = String::new();
        let mut columns = Vec::new();
        let mut ends = Vec::new();
        for (column, ch, wide) in cells {
            text.push(ch);
            columns.push(column);
            ends.push(column + if wide { 2 } else { 1 });
        }
        // Blanks at the end of a row are not text: a match cannot end on them.
        while text.ends_with(' ') {
            text.pop();
            columns.pop();
            ends.pop();
        }
        Self {
            line,
            text,
            columns,
            ends,
        }
    }

    fn columns_of(&self, range: std::ops::Range<usize>) -> Option<(u16, u16)> {
        let start = self.text[..range.start].chars().count();
        let end = self.text[..range.end].chars().count();
        if start >= end || end > self.columns.len() {
            return None;
        }
        Some((self.columns[start], self.ends[end - 1]))
    }
}

/// Every run `query` finds in `rows`. A query that is not a regular expression is
/// searched for as the text the user typed.
pub(crate) fn search_rows(rows: &[RowText], query: &SearchQuery, generation: u64) -> SearchResults {
    if query.text.is_empty() {
        return SearchResults {
            generation,
            ..SearchResults::default()
        };
    }
    let pattern = if query.regex {
        query.text.clone()
    } else {
        regex::escape(&query.text)
    };
    let regex = match RegexBuilder::new(&pattern)
        .case_insensitive(!query.case_sensitive)
        .build()
    {
        Ok(regex) => regex,
        Err(error) => {
            return SearchResults {
                generation,
                matches: Vec::new(),
                error: Some(first_line(&error.to_string())),
            };
        }
    };
    let mut matches = Vec::new();
    for row in rows {
        for found in regex.find_iter(&row.text) {
            let Some((start_col, end_col)) = row.columns_of(found.range()) else {
                continue;
            };
            matches.push(SearchMatch {
                line: row.line,
                start_col,
                end_col,
            });
        }
    }
    SearchResults {
        generation,
        matches,
        error: None,
    }
}

/// The offset that puts `line` at the top of the screen, out of the history there is.
pub(crate) fn reveal_offset(line: i32, history: usize) -> usize {
    if line >= 0 {
        return 0;
    }
    (-line as usize).min(history)
}

fn first_line(text: &str) -> String {
    text.lines().next().unwrap_or_default().to_string()
}

#[cfg(test)]
mod tests {
    use super::{RowText, reveal_offset, search_rows};
    use crate::session::SearchQuery;

    fn row(line: i32, text: &str) -> RowText {
        let cells = text
            .chars()
            .enumerate()
            .map(|(index, ch)| (index as u16, ch, false));
        RowText::new(line, cells)
    }

    fn plain(text: &str) -> SearchQuery {
        SearchQuery {
            text: text.to_string(),
            case_sensitive: false,
            regex: false,
        }
    }

    #[test]
    fn a_search_finds_runs_in_the_scrollback_and_on_the_screen() {
        let rows = [
            row(-40, "before"),
            row(-12, "the error is here"),
            row(3, "and the error again"),
        ];

        let found = search_rows(&rows, &plain("error"), 7);

        assert_eq!(found.generation, 7);
        let lines: Vec<i32> = found.matches.iter().map(|found| found.line).collect();
        assert_eq!(
            lines,
            [-12, 3],
            "a line of the history is as good as one on the screen"
        );
        assert_eq!(
            (found.matches[0].start_col, found.matches[0].end_col),
            (4, 9)
        );
        assert!(found.error.is_none());
    }

    #[test]
    fn a_search_reads_case_and_regular_expressions_as_the_user_asked() {
        let rows = [row(0, "Error error ERROR")];

        assert_eq!(search_rows(&rows, &plain("error"), 1).matches.len(), 3);
        let mut exact = plain("Error");
        exact.case_sensitive = true;
        assert_eq!(search_rows(&rows, &exact, 1).matches.len(), 1);

        let mut pattern = plain("e(r+)or");
        pattern.regex = true;
        assert_eq!(search_rows(&rows, &pattern, 1).matches.len(), 3);

        let mut broken = plain("e(");
        broken.regex = true;
        let found = search_rows(&rows, &broken, 1);
        assert!(found.matches.is_empty());
        assert!(found.error.is_some());
    }

    #[test]
    fn an_empty_query_finds_nothing_and_a_broken_one_says_why() {
        assert!(
            search_rows(&[row(0, "text")], &plain(""), 3)
                .matches
                .is_empty()
        );

        let mut broken = plain("[");
        broken.regex = true;
        assert!(search_rows(&[row(0, "text")], &broken, 3).error.is_some());
    }

    #[test]
    fn trailing_blanks_are_not_part_of_a_row() {
        let found = search_rows(&[row(0, "text   ")], &plain("   "), 1);
        assert!(found.matches.is_empty(), "{found:?}");
    }

    #[test]
    fn wide_glyphs_take_two_columns() {
        let cells = [(0, '語', true), (2, ' ', false)].into_iter();
        let row = RowText::new(0, cells);
        let found = search_rows(&[row], &plain("語"), 1);
        assert_eq!(
            (found.matches[0].start_col, found.matches[0].end_col),
            (0, 2)
        );
    }

    #[test]
    fn a_revealed_line_scrolls_the_screen_to_it() {
        assert_eq!(reveal_offset(-30, 100), 30);
        assert_eq!(
            reveal_offset(4, 100),
            0,
            "a line of the screen needs no scroll"
        );
        assert_eq!(reveal_offset(-300, 100), 100, "the history has a top");
    }
}

//! The layout of the commit graph of a log: the dot of every commit and the lines
//! between the rows. The layout is a pure function of the log, so the window computes
//! it once when the log is loaded and never while it draws.

use crate::Commit;

/// The colours the caller has for the lines. A lane keeps `column % COLORS`, so a line
/// does not change its colour from row to row.
pub const COLORS: usize = 8;

/// A line of one row: it leaves the row at the column `from` and reaches the next row at
/// the column `to`. A line of a lane that only passes through has `from == to` and runs
/// through the whole row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Line {
    pub from: usize,
    pub to: usize,
}

/// The dot of one commit and the lines that leave its row.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Row {
    /// The lane of the dot, from zero.
    pub column: usize,
    /// The colour of the dot and of its lines, `column % COLORS`.
    pub color: usize,
    /// The line of this commit starts on this row: no lane above waited for it. The window
    /// leaves the room above the dot empty then.
    pub starts: bool,
    /// The lines of this row down to the next one: the lanes that pass through, in column
    /// order, then the lines of the parents in their order.
    pub lines: Vec<Line>,
}

/// The rows of the graph of `commits`, which come newest first as the log reads them.
///
/// The first parent continues the line of its commit, so the line of the branch does not
/// move. Every other parent takes the leftmost free lane and the dot reaches it with a
/// diagonal. A lane is freed on the row of the commit it waited for, and it is never
/// shifted: columns stay where they are, and a new line reuses the leftmost free one. A
/// commit no lane waited for, such as the first one or one whose parent is not part of
/// the shown log, starts a line of its own.
pub fn layout(commits: &[Commit]) -> Vec<Row> {
    let mut rows = Vec::with_capacity(commits.len());
    // The commit each lane waits for, `None` for a free lane.
    let mut lanes: Vec<Option<&str>> = Vec::new();
    for commit in commits {
        let id = commit.id.as_str();
        let held = lanes.iter().position(|lane| *lane == Some(id));
        let column = match held {
            Some(column) => column,
            None => free(&lanes),
        };
        // The commit of the line has met: the lane is free again until a parent takes it.
        if column == lanes.len() {
            lanes.push(None);
        }
        lanes[column] = None;

        // The first parent keeps the line of the commit; the others branch off. A parent
        // another lane already waits for joins that lane instead of opening a second one.
        let mut started = Vec::with_capacity(commit.parents.len());
        for (index, parent) in commit.parents.iter().enumerate() {
            let preferred = (index == 0).then_some(column);
            started.push(wait_for(&mut lanes, parent, preferred));
        }

        // Every other busy lane goes through this row without a dot.
        let mut lines = Vec::new();
        for (lane, waiting) in lanes.iter().enumerate() {
            if waiting.is_some() && lane != column && !started.contains(&lane) {
                lines.push(Line {
                    from: lane,
                    to: lane,
                });
            }
        }
        for to in started {
            lines.push(Line { from: column, to });
        }

        rows.push(Row {
            column,
            color: column % COLORS,
            starts: held.is_none(),
            lines,
        });
    }
    rows
}

/// The lane that waits for `id`, taking a free one when no lane does: `preferred` when it
/// is free, otherwise the leftmost free lane.
fn wait_for<'a>(lanes: &mut Vec<Option<&'a str>>, id: &'a str, preferred: Option<usize>) -> usize {
    if let Some(column) = lanes.iter().position(|lane| *lane == Some(id)) {
        return column;
    }
    let column = preferred
        .filter(|column| lanes.get(*column).is_some_and(Option::is_none))
        .unwrap_or_else(|| free(lanes));
    if column == lanes.len() {
        lanes.push(None);
    }
    lanes[column] = Some(id);
    column
}

/// The leftmost lane that waits for nothing.
fn free(lanes: &[Option<&str>]) -> usize {
    lanes
        .iter()
        .position(Option::is_none)
        .unwrap_or(lanes.len())
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use super::{Line, Row, layout};
    use crate::Commit;

    fn commit(id: &str, parents: &[&str]) -> Commit {
        Commit {
            id: id.to_owned(),
            parents: parents.iter().map(|parent| (*parent).to_owned()).collect(),
            author: "Test".to_owned(),
            email: "test@example.com".to_owned(),
            time: 0,
            subject: id.to_owned(),
        }
    }

    /// The rows of a log written as `id: parent, parent`.
    fn rows(log: &[(&str, &[&str])]) -> Vec<Row> {
        let commits: Vec<Commit> = log
            .iter()
            .map(|(id, parents)| commit(id, parents))
            .collect();
        layout(&commits)
    }

    fn lines(pairs: &[(usize, usize)]) -> Vec<Line> {
        pairs
            .iter()
            .map(|(from, to)| Line {
                from: *from,
                to: *to,
            })
            .collect()
    }

    fn columns(rows: &[Row]) -> Vec<usize> {
        rows.iter().map(|row| row.column).collect()
    }

    #[test]
    fn a_linear_history_keeps_one_column() {
        let rows = rows(&[("c", &["b"]), ("b", &["a"]), ("a", &[])]);
        assert_eq!(columns(&rows), [0, 0, 0]);
        assert_eq!(rows[0].lines, lines(&[(0, 0)]));
        assert_eq!(rows[1].lines, lines(&[(0, 0)]));
        assert!(rows[2].lines.is_empty(), "the root ends the line");
        assert_eq!(
            rows.iter().map(|row| row.starts).collect::<Vec<_>>(),
            [true, false, false]
        );
        assert!(rows.iter().all(|row| row.color == 0));
    }

    #[test]
    fn a_branch_branches_off_and_joins_again_with_a_diagonal() {
        // The merge first, then the two sides, then their common parent.
        let rows = rows(&[
            ("merge", &["main", "side"]),
            ("main", &["base"]),
            ("side", &["base"]),
            ("base", &[]),
        ]);
        assert_eq!(columns(&rows), [0, 0, 1, 0]);
        assert_eq!(rows[0].lines, lines(&[(0, 0), (0, 1)]));
        assert_eq!(rows[1].lines, lines(&[(1, 1), (0, 0)]));
        assert_eq!(
            rows[2].lines,
            lines(&[(1, 0)]),
            "the side line joins the main one"
        );
        assert!(rows[3].lines.is_empty());
        assert_eq!(rows[2].color, 1, "the side line keeps its colour");
    }

    #[test]
    fn two_merges_in_a_row_take_two_free_lanes() {
        let rows = rows(&[
            ("first", &["second", "side"]),
            ("second", &["base", "other"]),
            ("base", &["root"]),
            ("other", &["root"]),
            ("side", &["root"]),
            ("root", &[]),
        ]);
        assert_eq!(columns(&rows), [0, 0, 0, 2, 1, 0]);
        assert_eq!(rows[0].lines, lines(&[(0, 0), (0, 1)]));
        assert_eq!(
            rows[1].lines,
            lines(&[(1, 1), (0, 0), (0, 2)]),
            "the second merge branches off to the right of the side line"
        );
        assert_eq!(rows[3].lines, lines(&[(1, 1), (2, 0)]));
        assert_eq!(rows[4].lines, lines(&[(1, 0)]));
        assert!(rows[5].lines.is_empty());
    }

    #[test]
    fn an_octopus_merge_takes_one_lane_for_each_parent() {
        let rows = rows(&[
            ("octopus", &["first", "second", "third"]),
            ("first", &["root"]),
            ("second", &["root"]),
            ("third", &["root"]),
            ("root", &[]),
        ]);
        assert_eq!(columns(&rows), [0, 0, 1, 2, 0]);
        assert_eq!(rows[0].lines, lines(&[(0, 0), (0, 1), (0, 2)]));
        assert_eq!(rows[2].color, 1);
        assert_eq!(rows[3].color, 2);
        assert_eq!(rows[2].lines, lines(&[(2, 2), (1, 0)]));
        assert_eq!(rows[3].lines, lines(&[(2, 0)]));
        assert!(rows[4].lines.is_empty());
    }

    #[test]
    fn a_branch_that_never_merges_ends_on_its_root() {
        let rows = rows(&[
            ("top", &["main", "side"]),
            ("main", &["main root"]),
            ("side", &[]),
            ("main root", &[]),
        ]);
        assert_eq!(columns(&rows), [0, 0, 1, 0]);
        assert_eq!(
            rows[2].lines,
            lines(&[(0, 0)]),
            "the side line stops at its root while the main line goes on"
        );
        assert!(rows[3].lines.is_empty());
    }

    #[test]
    fn a_new_line_takes_the_leftmost_free_lane_and_the_others_stay() {
        // The side line ends on the third row; the branch of the next merge takes its
        // lane again, and the main line never moves.
        let rows = rows(&[
            ("top", &["main", "side"]),
            ("main", &["second"]),
            ("side", &[]),
            ("second", &["third", "fresh"]),
            ("third", &["root"]),
            ("fresh", &["root"]),
            ("root", &[]),
        ]);
        assert_eq!(columns(&rows), [0, 0, 1, 0, 0, 1, 0]);
        assert_eq!(rows[2].lines, lines(&[(0, 0)]));
        assert_eq!(
            rows[3].lines,
            lines(&[(0, 0), (0, 1)]),
            "the new line reuses the lane the side line freed"
        );
        assert_eq!(rows[4].lines, lines(&[(1, 1), (0, 0)]));
        assert_eq!(rows[5].lines, lines(&[(1, 0)]));
        assert!(rows[6].lines.is_empty());
    }

    #[test]
    fn a_log_of_one_file_may_name_parents_it_does_not_show() {
        // The parents of the last shown commit are outside the window: their lanes stay
        // open below the list and no row loses its column.
        let rows = rows(&[("second", &["first"]), ("first", &["outside"])]);
        assert_eq!(columns(&rows), [0, 0]);
        assert_eq!(rows[1].lines, lines(&[(0, 0)]));
        assert_eq!(rows[1].color, 0);
    }

    #[test]
    fn a_commit_without_a_child_in_the_log_opens_a_lane() {
        // The file log skips commits, so a shown commit may start without a child above.
        let rows = rows(&[
            ("newest", &["middle"]),
            ("middle", &["oldest"]),
            ("older", &["oldest"]),
            ("oldest", &[]),
        ]);
        assert_eq!(columns(&rows), [0, 0, 1, 0]);
        assert!(rows[2].starts, "no lane waited for the older commit");
        assert_eq!(rows[2].lines, lines(&[(1, 0)]));
    }

    #[test]
    fn ten_thousand_commits_lay_out_far_below_a_noticeable_time() {
        // A long log with a side line every few commits: the layout is one pass over the
        // commits and their parents and does not depend on the window.
        let mut commits = Vec::with_capacity(10_000);
        for index in 0..10_000 {
            let id = format!("commit-{index}");
            let mut parents = Vec::new();
            if index + 1 < 10_000 {
                parents.push(format!("commit-{}", index + 1));
            }
            if index % 50 == 0 && index + 3 < 10_000 {
                parents.push(format!("commit-{}", index + 3));
            }
            commits.push(Commit {
                id,
                parents,
                author: "Test".to_owned(),
                email: "test@example.com".to_owned(),
                time: 0,
                subject: String::new(),
            });
        }
        let started = Instant::now();
        let rows = layout(&commits);
        let elapsed = started.elapsed();
        assert_eq!(rows.len(), 10_000);
        // A debug build lays these out in a few milliseconds; a quarter of a second is a
        // margin that still catches a layout that walks the history for every row.
        assert!(
            elapsed < Duration::from_millis(250),
            "10 000 commits took {elapsed:?}"
        );
    }
}

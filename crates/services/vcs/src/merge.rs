//! A three-way merge of one text: the base version, ours and theirs.
//!
//! The versions come from the index stages of a conflict (`:1:`, `:2:`, `:3:`), decoded
//! to text with `\n` line ends. [`merge`] splits them into aligned segments and builds the
//! first text of the result: every non-conflicting change of both sides applied over the
//! base, a conflict left as the base lines.

use std::ops::Range;

use crate::line_hunks;

/// Where one segment of the merge comes from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SegmentKind {
    /// Neither side changed these lines.
    Unchanged,
    /// Only ours changed them.
    Yours,
    /// Only theirs changed them.
    Theirs,
    /// Both sides made the same change.
    Both,
    /// Both sides changed the same lines in different ways.
    Conflict,
}

/// One place of the three versions, in lines with their line ends.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Segment {
    pub kind: SegmentKind,
    /// Lines of the base version.
    pub base: Range<usize>,
    /// Lines of ours.
    pub yours: Range<usize>,
    /// Lines of theirs.
    pub theirs: Range<usize>,
}

/// The three versions split into aligned segments, and the text the result starts with.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThreeWay {
    pub segments: Vec<Segment>,
    /// The base with every non-conflicting change applied; a conflict keeps base lines.
    pub result: String,
}

/// One hunk of one side: the base lines it replaces, and the lines it puts there.
#[derive(Clone, Debug)]
struct Change {
    base: Range<usize>,
    side: Range<usize>,
}

/// A place of the base that at least one side changed, with the changes inside it.
#[derive(Clone, Debug)]
struct Area {
    base: Range<usize>,
    ours: Vec<usize>,
    theirs: Vec<usize>,
}

/// The lines of a text with their line ends. A line carries its line end, so the last
/// line without one differs from the same line with it, and an empty text has no lines.
fn lines(text: &str) -> Vec<&str> {
    text.split_inclusive('\n').collect()
}

/// The number of lines a piece of text takes.
fn line_count(text: &str) -> usize {
    text.split_inclusive('\n').count()
}

/// The hunks of one side against the base, in the line numbers of both texts.
fn changes(base: &str, side: &str) -> Vec<Change> {
    line_hunks(base, side)
        .into_iter()
        .map(|hunk| Change {
            base: hunk.before.start as usize..hunk.before.end as usize,
            side: hunk.after.start as usize..hunk.after.end as usize,
        })
        .collect()
}

/// The places of the base that the two sides changed. Ranges that overlap or contain one
/// another are one place, so the texts of both sides can be compared whole; ranges that
/// only touch stay apart, as do changes at different places.
fn areas(ours: &[Change], theirs: &[Change]) -> Vec<Area> {
    let mut ranges = Vec::new();
    for (index, change) in ours.iter().enumerate() {
        if !change.base.is_empty() {
            ranges.push((change.base.start, change.base.end, true, index));
        }
    }
    for (index, change) in theirs.iter().enumerate() {
        if !change.base.is_empty() {
            ranges.push((change.base.start, change.base.end, false, index));
        }
    }
    ranges.sort_by_key(|(start, end, _, _)| (*start, *end));
    let mut merged: Vec<Area> = Vec::new();
    for (start, end, is_ours, index) in ranges {
        match merged.last_mut() {
            Some(area) if start < area.base.end => {
                area.base.end = area.base.end.max(end);
                if is_ours {
                    area.ours.push(index);
                } else {
                    area.theirs.push(index);
                }
            }
            _ => {
                let mut area = Area {
                    base: start..end,
                    ours: Vec::new(),
                    theirs: Vec::new(),
                };
                if is_ours {
                    area.ours.push(index);
                } else {
                    area.theirs.push(index);
                }
                merged.push(area);
            }
        }
    }

    // Insertions at one point. An insertion inside a replaced range belongs to it, so a
    // change that only inserts a line conflicts with a change that replaces the line it
    // lands between. Two insertions at one point meet there and are compared.
    let mut points: Vec<(usize, Vec<usize>, Vec<usize>)> = Vec::new();
    for (is_ours, side) in [(true, ours), (false, theirs)] {
        for (index, change) in side.iter().enumerate() {
            if !change.base.is_empty() {
                continue;
            }
            let point = change.base.start;
            match points.iter_mut().find(|(at, _, _)| *at == point) {
                Some((_, ours_at, theirs_at)) => {
                    if is_ours {
                        ours_at.push(index);
                    } else {
                        theirs_at.push(index);
                    }
                }
                None => {
                    let mut entry = (point, Vec::new(), Vec::new());
                    if is_ours {
                        entry.1.push(index);
                    } else {
                        entry.2.push(index);
                    }
                    points.push(entry);
                }
            }
        }
    }
    for (point, ours_at, theirs_at) in points {
        match merged
            .iter_mut()
            .find(|area| area.base.start < point && point < area.base.end)
        {
            Some(area) => {
                area.ours.extend(ours_at);
                area.theirs.extend(theirs_at);
            }
            None => merged.push(Area {
                base: point..point,
                ours: ours_at,
                theirs: theirs_at,
            }),
        }
    }
    // An insertion at the start of a replaced range comes before it.
    merged.sort_by_key(|area| (area.base.start, (area.base.start != area.base.end) as u8));
    merged
}

/// The text of one area of one side, walking the base lines from the start to the end of
/// the area and putting the lines of the side over the base wherever it changed them.
fn area_text(
    base: &[&str],
    side: &[&str],
    area: &Area,
    changes: &[Change],
    mine: &[usize],
) -> String {
    let mut text = String::new();
    let append = |text: &mut String, range: &Range<usize>| {
        if let Some(lines) = side.get(range.clone()) {
            for line in lines {
                text.push_str(line);
            }
        }
    };
    if area.base.is_empty() {
        for index in mine {
            append(&mut text, &changes[*index].side);
        }
        return text;
    }
    let mut line = area.base.start;
    while line < area.base.end {
        for index in mine {
            let change = &changes[*index];
            if change.base.is_empty() && change.base.start == line {
                append(&mut text, &change.side);
            }
        }
        let replaced = mine.iter().find_map(|index| {
            let change = &changes[*index];
            (!change.base.is_empty() && change.base.contains(&line)).then_some(change)
        });
        match replaced {
            Some(change) => {
                append(&mut text, &change.side);
                line = change.base.end;
            }
            None => {
                if let Some(base_line) = base.get(line) {
                    text.push_str(base_line);
                }
                line += 1;
            }
        }
    }
    text
}

/// Splits `base`, `yours` and `theirs` into aligned segments and builds the first result.
///
/// All three texts have `\n` line ends. A line carries its line end, so the last line
/// without one differs from the same line with it.
pub fn merge(base: &str, yours: &str, theirs: &str) -> ThreeWay {
    let base_lines = lines(base);
    let yours_lines = lines(yours);
    let theirs_lines = lines(theirs);
    let ours = changes(base, yours);
    let theirs = changes(base, theirs);
    let areas = areas(&ours, &theirs);

    let mut segments = Vec::with_capacity(areas.len() * 2 + 1);
    let mut result = String::new();
    let (mut base_at, mut yours_at, mut theirs_at) = (0usize, 0usize, 0usize);

    for area in &areas {
        if base_at < area.base.start {
            let gap = area.base.start - base_at;
            segments.push(Segment {
                kind: SegmentKind::Unchanged,
                base: base_at..area.base.start,
                yours: yours_at..yours_at + gap,
                theirs: theirs_at..theirs_at + gap,
            });
            for line in &base_lines[base_at..area.base.start] {
                result.push_str(line);
            }
            yours_at += gap;
            theirs_at += gap;
        }

        let yours_text = area_text(&base_lines, &yours_lines, area, &ours, &area.ours);
        let theirs_text = area_text(&base_lines, &theirs_lines, area, &theirs, &area.theirs);
        let kind = match (area.ours.is_empty(), area.theirs.is_empty()) {
            (true, true) => SegmentKind::Unchanged,
            (false, true) => SegmentKind::Yours,
            (true, false) => SegmentKind::Theirs,
            (false, false) if yours_text == theirs_text => SegmentKind::Both,
            (false, false) => SegmentKind::Conflict,
        };
        segments.push(Segment {
            kind,
            base: area.base.clone(),
            yours: yours_at..yours_at + line_count(&yours_text),
            theirs: theirs_at..theirs_at + line_count(&theirs_text),
        });
        match kind {
            SegmentKind::Unchanged | SegmentKind::Conflict => {
                for line in &base_lines[area.base.clone()] {
                    result.push_str(line);
                }
            }
            SegmentKind::Yours | SegmentKind::Both => result.push_str(&yours_text),
            SegmentKind::Theirs => result.push_str(&theirs_text),
        }
        base_at = area.base.end;
        yours_at += line_count(&yours_text);
        theirs_at += line_count(&theirs_text);
    }

    if base_at < base_lines.len() {
        let gap = base_lines.len() - base_at;
        segments.push(Segment {
            kind: SegmentKind::Unchanged,
            base: base_at..base_lines.len(),
            yours: yours_at..yours_at + gap,
            theirs: theirs_at..theirs_at + gap,
        });
        for line in &base_lines[base_at..] {
            result.push_str(line);
        }
    }

    ThreeWay { segments, result }
}

#[cfg(test)]
mod tests {
    use super::{SegmentKind, ThreeWay, merge};

    fn kinds(merged: &ThreeWay) -> Vec<SegmentKind> {
        merged.segments.iter().map(|segment| segment.kind).collect()
    }

    fn conflicts(merged: &ThreeWay) -> usize {
        kinds(merged)
            .into_iter()
            .filter(|kind| *kind == SegmentKind::Conflict)
            .count()
    }

    #[test]
    fn changes_of_different_places_of_the_file_are_both_applied() {
        let merged = merge("a\nb\nc\n", "A\nb\nc\n", "a\nb\nC\n");

        assert_eq!(merged.result, "A\nb\nC\n");
        assert_eq!(conflicts(&merged), 0);
        assert!(kinds(&merged).contains(&SegmentKind::Yours));
        assert!(kinds(&merged).contains(&SegmentKind::Theirs));
    }

    #[test]
    fn one_conflict_keeps_the_base_lines_in_the_result() {
        let merged = merge("a\nb\nc\n", "a\nY\nc\n", "a\nT\nc\n");

        assert_eq!(merged.result, "a\nb\nc\n");
        assert_eq!(conflicts(&merged), 1);
    }

    #[test]
    fn adjacent_changes_of_the_two_sides_do_not_conflict() {
        let merged = merge("a\nb\nc\nd\n", "a\nY\nc\nd\n", "a\nb\nT\nd\n");

        assert_eq!(merged.result, "a\nY\nT\nd\n");
        assert_eq!(conflicts(&merged), 0);
    }

    #[test]
    fn the_same_change_on_both_sides_is_taken_once() {
        let merged = merge("a\nb\n", "a\nB\n", "a\nB\n");

        assert_eq!(merged.result, "a\nB\n");
        assert_eq!(conflicts(&merged), 0);
        assert_eq!(
            kinds(&merged)
                .into_iter()
                .filter(|kind| *kind == SegmentKind::Both)
                .count(),
            1
        );
    }

    #[test]
    fn a_deletion_against_a_change_is_a_conflict() {
        let merged = merge("a\nb\nc\n", "a\nc\n", "a\nT\nc\n");

        assert_eq!(merged.result, "a\nb\nc\n");
        assert_eq!(conflicts(&merged), 1);
    }

    #[test]
    fn an_empty_base_is_a_conflict_or_one_addition() {
        let different = merge("", "yours\n", "theirs\n");
        assert_eq!(different.result, "");
        assert_eq!(conflicts(&different), 1);

        let same = merge("", "added\n", "added\n");
        assert_eq!(same.result, "added\n");
        assert_eq!(conflicts(&same), 0);
    }
}

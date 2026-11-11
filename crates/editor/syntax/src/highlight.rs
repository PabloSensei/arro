//! Highlight query results flattened into spans that do not overlap.

use std::ops::Range;

use text_core::Edit;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HighlightKind {
    Keyword,
    String,
    Number,
    Comment,
    DocComment,
    DocTag,
    Field,
    Constant,
    Function,
    Annotation,
}

impl HighlightKind {
    /// Capture names of the highlight query. Unknown captures stay plain text.
    pub(crate) fn from_capture(name: &str) -> Option<Self> {
        Some(match name {
            "keyword" => Self::Keyword,
            "string" => Self::String,
            "number" => Self::Number,
            "comment" => Self::Comment,
            "comment.doc" => Self::DocComment,
            "comment.tag" => Self::DocTag,
            "field" => Self::Field,
            "constant" => Self::Constant,
            "function" => Self::Function,
            "annotation" => Self::Annotation,
            _ => return None,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HighlightSpan {
    pub start: u32,
    pub end: u32,
    pub kind: HighlightKind,
}

/// A capture of the query. Among overlapping captures the narrower one wins, then the
/// one from the later pattern.
pub(crate) struct Capture {
    pub range: Range<usize>,
    pub pattern: usize,
    pub kind: HighlightKind,
}

pub(crate) fn flatten(mut captures: Vec<Capture>) -> Vec<HighlightSpan> {
    captures.sort_by_key(|capture| {
        (
            capture.range.start,
            std::cmp::Reverse(capture.range.end),
            capture.pattern,
        )
    });
    let mut spans: Vec<HighlightSpan> = Vec::with_capacity(captures.len());
    let mut open: Vec<Capture> = Vec::new();
    let mut position = 0;
    let mut captures = captures.into_iter().peekable();

    fn push(spans: &mut Vec<HighlightSpan>, range: Range<usize>, kind: HighlightKind) {
        if range.is_empty() {
            return;
        }
        match spans.last_mut() {
            Some(last) if last.end as usize == range.start && last.kind == kind => {
                last.end = range.end as u32
            }
            _ => spans.push(HighlightSpan {
                start: range.start as u32,
                end: range.end as u32,
                kind,
            }),
        }
    }

    while !open.is_empty() || captures.peek().is_some() {
        let next_start = captures.peek().map(|capture| capture.range.start);
        let next_end = open.iter().map(|capture| capture.range.end).min();
        let boundary = match (next_start, next_end) {
            (Some(start), Some(end)) => start.min(end),
            (Some(start), None) => start,
            (None, Some(end)) => end,
            (None, None) => break,
        };
        if let Some(top) = open.last() {
            push(&mut spans, position..boundary, top.kind);
        }
        position = boundary;
        open.retain(|capture| capture.range.end > position);
        while captures
            .peek()
            .is_some_and(|capture| capture.range.start == position)
        {
            if let Some(capture) = captures.next() {
                open.push(capture);
            }
        }
    }
    spans
}

/// Moves `spans` along an edit, so that colours stay in place until the next parse.
/// A span that crosses a replaced range keeps its free ends and leaves the inserted
/// text out, so spans never overlap after the edit.
pub fn shift_spans(spans: &mut Vec<HighlightSpan>, edits: &[Edit]) {
    spans.sort_by_key(|span| (span.end, span.start));
    for edit in edits.iter().rev() {
        let (start, end) = (edit.range.start as u32, edit.range.end as u32);
        let new_len = edit.text.len() as u32;
        let delta = i64::from(new_len) - i64::from(end - start);
        let first = spans.partition_point(|span| span.end <= start);
        for span in &mut spans[first..] {
            if span.start >= end {
                span.start = (i64::from(span.start) + delta) as u32;
                span.end = (i64::from(span.end) + delta) as u32;
            } else {
                span.start = span.start.min(start);
                span.end = if span.end <= end {
                    start
                } else {
                    (i64::from(span.end) + delta) as u32
                };
            }
        }
        spans.retain(|span| span.start < span.end);
    }
}

/// Sorts spans, cuts them to `len` and drops the parts that overlap a span before them.
/// A stale span from an older text can reach far past the end of the new one.
pub fn normalize_spans(spans: &mut Vec<HighlightSpan>, len: usize) {
    let len = len.min(u32::MAX as usize) as u32;
    spans.sort_by_key(|span| (span.start, span.end));
    let mut taken = 0u32;
    spans.retain_mut(|span| {
        if span.start >= len || span.end > len {
            return false;
        }
        span.start = span.start.max(taken);
        if span.start >= span.end {
            return false;
        }
        taken = span.end;
        true
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn capture(range: Range<usize>, pattern: usize, kind: HighlightKind) -> Capture {
        Capture {
            range,
            pattern,
            kind,
        }
    }

    #[test]
    fn the_inner_capture_wins_and_the_outer_one_continues() {
        let spans = flatten(vec![
            capture(0..10, 0, HighlightKind::Comment),
            capture(2..4, 1, HighlightKind::DocTag),
        ]);
        let kinds: Vec<_> = spans
            .iter()
            .map(|span| (span.start, span.end, span.kind))
            .collect();
        assert_eq!(
            kinds,
            vec![
                (0, 2, HighlightKind::Comment),
                (2, 4, HighlightKind::DocTag),
                (4, 10, HighlightKind::Comment),
            ]
        );
    }

    #[test]
    fn the_later_pattern_wins_on_the_same_range() {
        let spans = flatten(vec![
            capture(0..3, 0, HighlightKind::Field),
            capture(0..3, 5, HighlightKind::Constant),
        ]);
        assert_eq!(
            spans,
            vec![HighlightSpan {
                start: 0,
                end: 3,
                kind: HighlightKind::Constant
            }]
        );
    }

    #[test]
    fn spans_follow_an_insertion() {
        let mut spans = vec![
            HighlightSpan {
                start: 0,
                end: 2,
                kind: HighlightKind::Keyword,
            },
            HighlightSpan {
                start: 5,
                end: 7,
                kind: HighlightKind::String,
            },
        ];
        shift_spans(&mut spans, &[Edit::insert(3, "xx")]);
        assert_eq!(spans[1].start, 7);
        assert_eq!(spans[0].end, 2);
    }

    #[test]
    fn a_replaced_range_leaves_no_overlapping_spans() {
        let mut spans = vec![
            HighlightSpan {
                start: 0,
                end: 10,
                kind: HighlightKind::Keyword,
            },
            HighlightSpan {
                start: 10,
                end: 20,
                kind: HighlightKind::String,
            },
        ];
        // Replace 5..15, which crosses the point where the two spans meet.
        shift_spans(&mut spans, &[Edit::new(5..15, "x\ny\nz")]);
        assert_eq!(spans[0].end, 5);
        assert_eq!(spans[1].start, 5);
        assert_eq!(spans[1].end, 15);
        for pair in spans.windows(2) {
            assert!(pair[0].end <= pair[1].start, "{spans:?}");
        }
    }

    #[test]
    fn normalize_cuts_stale_spans_to_the_text_and_keeps_them_apart() {
        let mut spans = vec![
            HighlightSpan {
                start: 20,
                end: 30,
                kind: HighlightKind::Keyword,
            },
            HighlightSpan {
                start: 0,
                end: 974,
                kind: HighlightKind::String,
            },
            HighlightSpan {
                start: 8,
                end: 12,
                kind: HighlightKind::Field,
            },
        ];
        normalize_spans(&mut spans, 54);
        assert_eq!(spans.len(), 2);
        assert!(spans.iter().all(|span| span.end <= 54));
        for pair in spans.windows(2) {
            assert!(pair[0].end <= pair[1].start, "{spans:?}");
        }
    }
}

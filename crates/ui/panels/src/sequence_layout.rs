//! The geometry of a sequence diagram: one column per participant, one row per message
//! in its order, the frames of the fragments around their messages and the activation
//! bars of the calls that have a return.

use lang_api::{SequenceDiagram, SequenceFragmentKind, SequenceMessageKind};

/// The width of a participant header.
pub const PARTICIPANT_WIDTH: f32 = 160.;
/// The height of a participant header.
pub const PARTICIPANT_HEIGHT: f32 = 38.;
/// The space between two columns.
pub const COLUMN_GAP: f32 = 48.;
/// The margin around the whole diagram.
pub const MARGIN: f32 = 40.;
/// The space above the headers, where the title of the method goes.
pub const TOP: f32 = 24.;
/// The space between the headers and the first message.
pub const HEADER_GAP: f32 = 32.;
/// The height of one message row and the space around its arrow.
pub const ROW_HEIGHT: f32 = 46.;
/// The space a frame leaves around its messages.
pub const FRAME_PAD: f32 = 10.;

/// One activation bar: the life of a participant from a call to its return.
#[derive(Clone, Debug, PartialEq)]
pub struct Activation {
    pub participant: usize,
    pub top: f32,
    pub bottom: f32,
}

/// One frame of a fragment, with the lines between its sections.
#[derive(Clone, Debug, PartialEq)]
pub struct PlacedFragment {
    pub fragment: usize,
    pub left: f32,
    pub top: f32,
    pub width: f32,
    pub height: f32,
    /// Where the dashed line of every section after the first starts.
    pub separators: Vec<f32>,
}

/// Where everything of the diagram is drawn, in diagram units.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Geometry {
    pub width: f32,
    pub height: f32,
    /// The middle of every participant column.
    pub columns: Vec<f32>,
    /// The middle of every message row.
    pub rows: Vec<f32>,
    pub activations: Vec<Activation>,
    pub fragments: Vec<PlacedFragment>,
}

/// Places the participants and the messages of a diagram.
pub fn compute(diagram: &SequenceDiagram) -> Geometry {
    let count = diagram.participants.len();
    let columns: Vec<f32> = (0..count)
        .map(|index| {
            MARGIN + PARTICIPANT_WIDTH / 2. + index as f32 * (PARTICIPANT_WIDTH + COLUMN_GAP)
        })
        .collect();
    let width = if count == 0 {
        MARGIN * 2.
    } else {
        MARGIN * 2.
            + count as f32 * PARTICIPANT_WIDTH
            + (count.saturating_sub(1)) as f32 * COLUMN_GAP
    };
    let first_row = TOP + PARTICIPANT_HEIGHT + HEADER_GAP + ROW_HEIGHT / 2.;
    let rows: Vec<f32> = (0..diagram.messages.len())
        .map(|index| first_row + index as f32 * ROW_HEIGHT)
        .collect();
    let height = rows
        .last()
        .map_or(TOP + PARTICIPANT_HEIGHT + MARGIN, |last| {
            last + ROW_HEIGHT / 2. + MARGIN
        });
    let activations = activations(diagram, &rows);
    let fragments = fragments(diagram, &columns, &rows);
    Geometry {
        width,
        height,
        columns,
        rows,
        activations,
        fragments,
    }
}

fn activations(diagram: &SequenceDiagram, rows: &[f32]) -> Vec<Activation> {
    let mut open: Vec<(usize, usize, usize)> = Vec::new();
    let mut found = Vec::new();
    for (index, message) in diagram.messages.iter().enumerate() {
        match message.kind {
            SequenceMessageKind::Call
            | SequenceMessageKind::SelfCall
            | SequenceMessageKind::Creation => {
                open.push((index, message.from, message.to));
            }
            SequenceMessageKind::Return => {
                let at = open
                    .iter()
                    .rposition(|(_, from, to)| *from == message.from && *to == message.to);
                if let Some(position) = at {
                    let (start, _, to) = open.remove(position);
                    push_activation(&mut found, to, start, index, rows);
                }
            }
        }
    }
    for (start, _, to) in open {
        let end = rows.len().saturating_sub(1);
        push_activation(&mut found, to, start, end, rows);
    }
    found.sort_by(|a, b| a.top.total_cmp(&b.top));
    found
}

fn push_activation(
    found: &mut Vec<Activation>,
    participant: usize,
    start: usize,
    end: usize,
    rows: &[f32],
) {
    let (Some(top), Some(bottom)) = (rows.get(start), rows.get(end)) else {
        return;
    };
    found.push(Activation {
        participant,
        top: *top - ROW_HEIGHT / 4.,
        bottom: *bottom + ROW_HEIGHT / 4.,
    });
}

fn fragments(diagram: &SequenceDiagram, columns: &[f32], rows: &[f32]) -> Vec<PlacedFragment> {
    let mut found = Vec::new();
    for (index, fragment) in diagram.fragments.iter().enumerate() {
        let start = fragment
            .sections
            .iter()
            .map(|section| section.messages.start)
            .min()
            .unwrap_or(0);
        let end = fragment
            .sections
            .iter()
            .map(|section| section.messages.end)
            .max()
            .unwrap_or(0);
        if start >= end || start >= rows.len() {
            continue;
        }
        let end = end.min(rows.len());
        let mut left = f32::MAX;
        let mut right = f32::MIN;
        for message in &diagram.messages[start..end] {
            for participant in [message.from, message.to] {
                if let Some(column) = columns.get(participant) {
                    left = left.min(column - PARTICIPANT_WIDTH / 2.);
                    right = right.max(column + PARTICIPANT_WIDTH / 2.);
                }
            }
        }
        if right <= left {
            continue;
        }
        let top = rows[start] - ROW_HEIGHT / 2. - FRAME_PAD;
        let bottom = rows[end - 1] + ROW_HEIGHT / 2. + FRAME_PAD;
        let separators = fragment
            .sections
            .iter()
            .skip(1)
            .filter(|section| section.messages.start < end)
            .filter_map(|section| {
                rows.get(section.messages.start)
                    .map(|row| row - ROW_HEIGHT / 2.)
            })
            .collect();
        found.push(PlacedFragment {
            fragment: index,
            left: left - FRAME_PAD,
            top,
            width: right - left + FRAME_PAD * 2.,
            height: bottom - top,
            separators,
        });
    }
    found
}

/// The label of a fragment header, such as `alt flag`.
pub fn fragment_label(kind: SequenceFragmentKind, condition: &str) -> String {
    let kind = match kind {
        SequenceFragmentKind::Alt => "alt",
        SequenceFragmentKind::Opt => "opt",
        SequenceFragmentKind::Loop => "loop",
    };
    format!("{kind} {condition}")
}

#[cfg(test)]
mod tests {
    use lang_api::{
        SequenceFragment, SequenceMessage, SequenceParticipant, SequenceParticipantKind,
        SequenceSection,
    };

    use super::*;

    fn participant(name: &str) -> SequenceParticipant {
        SequenceParticipant {
            name: name.to_owned(),
            qualified: name.to_owned(),
            kind: SequenceParticipantKind::Class,
            place: None,
        }
    }

    fn message(from: usize, to: usize, kind: SequenceMessageKind) -> SequenceMessage {
        SequenceMessage {
            from,
            to,
            kind,
            label: String::new(),
            returns: None,
            place: None,
            fragments: Vec::new(),
            depth: 1,
        }
    }

    #[test]
    fn the_columns_and_the_rows_follow_the_order_of_the_model() {
        let diagram = SequenceDiagram {
            participants: vec![participant("A"), participant("B")],
            messages: vec![
                message(0, 1, SequenceMessageKind::Call),
                message(1, 0, SequenceMessageKind::Return),
            ],
            fragments: Vec::new(),
            title: String::new(),
        };
        let geometry = compute(&diagram);
        assert!(geometry.columns[0] < geometry.columns[1]);
        assert!(geometry.rows[0] < geometry.rows[1]);
        assert!(geometry.height > geometry.rows[1]);
    }

    #[test]
    fn a_call_and_its_return_draw_one_activation() {
        let diagram = SequenceDiagram {
            participants: vec![participant("A"), participant("B")],
            messages: vec![
                message(0, 1, SequenceMessageKind::Call),
                message(1, 0, SequenceMessageKind::Return),
            ],
            fragments: Vec::new(),
            title: String::new(),
        };
        let geometry = compute(&diagram);
        assert_eq!(geometry.activations.len(), 1);
        assert_eq!(geometry.activations[0].participant, 1);
        assert!(geometry.activations[0].bottom > geometry.activations[0].top);
    }

    #[test]
    fn a_frame_of_an_alt_keeps_the_messages_of_both_sections() {
        let diagram = SequenceDiagram {
            participants: vec![participant("A"), participant("B")],
            messages: vec![
                message(0, 1, SequenceMessageKind::Call),
                message(0, 1, SequenceMessageKind::Call),
            ],
            fragments: vec![SequenceFragment {
                kind: SequenceFragmentKind::Alt,
                sections: vec![
                    SequenceSection {
                        condition: "flag".to_owned(),
                        messages: 0..1,
                    },
                    SequenceSection {
                        condition: "else".to_owned(),
                        messages: 1..2,
                    },
                ],
                depth: 0,
            }],
            title: String::new(),
        };
        let geometry = compute(&diagram);
        assert_eq!(geometry.fragments.len(), 1);
        let frame = &geometry.fragments[0];
        assert!(frame.top < geometry.rows[0]);
        assert!(frame.top + frame.height > geometry.rows[1]);
        assert_eq!(frame.separators.len(), 1);
    }
}

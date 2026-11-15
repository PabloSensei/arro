//! The sequence diagram as text for PlantUML and Mermaid and as a standalone SVG file.
//! The texts keep the order of the messages and the frames of the fragments, so that a
//! diagram pasted into a document reads the same way as the tab.

use lang_api::{
    SequenceDiagram, SequenceFragmentKind, SequenceMessageKind, SequenceParticipantKind,
};

use crate::diagram_svg::escape;
use crate::sequence_layout::{Geometry, PARTICIPANT_HEIGHT, PARTICIPANT_WIDTH, fragment_label};

/// The diagram as a PlantUML document.
pub fn plantuml(diagram: &SequenceDiagram) -> String {
    let mut lines = vec!["@startuml".to_owned()];
    if !diagram.title.is_empty() {
        lines.push(format!("title {}", diagram.title));
    }
    for participant in &diagram.participants {
        let keyword = if participant.kind == SequenceParticipantKind::Actor {
            "actor"
        } else {
            "participant"
        };
        lines.push(format!("{keyword} {}", participant.name));
    }
    write_messages(
        diagram,
        |kind| match kind {
            TextMessage::Call => " -> ",
            TextMessage::Return => " --> ",
        },
        " : ",
        &mut lines,
    );
    lines.push("@enduml".to_owned());
    lines.join("\n") + "\n"
}

/// The diagram as a Mermaid `sequenceDiagram`.
pub fn mermaid(diagram: &SequenceDiagram) -> String {
    let mut lines = vec!["sequenceDiagram".to_owned()];
    for participant in &diagram.participants {
        let keyword = if participant.kind == SequenceParticipantKind::Actor {
            "actor"
        } else {
            "participant"
        };
        lines.push(format!("{keyword} {}", participant.name));
    }
    write_messages(
        diagram,
        |kind| match kind {
            TextMessage::Call => "->>",
            TextMessage::Return => "-->>",
        },
        ": ",
        &mut lines,
    );
    lines.join("\n") + "\n"
}

#[derive(Clone, Copy)]
enum TextMessage {
    Call,
    Return,
}

/// The participants and the messages of the texts in order, with the frames of the
/// fragments opened before their first message and closed after their last one.
fn write_messages(
    diagram: &SequenceDiagram,
    arrow: impl Fn(TextMessage) -> &'static str,
    colon: &str,
    lines: &mut Vec<String>,
) {
    let mut open: Vec<(usize, usize)> = Vec::new();
    for (index, message) in diagram.messages.iter().enumerate() {
        let wanted = &message.fragments;
        let common = open
            .iter()
            .zip(wanted.iter())
            .take_while(|((fragment, _), wanted)| fragment == *wanted)
            .count();
        while open.len() > common {
            open.pop();
            lines.push("end".to_owned());
        }
        for fragment in &wanted[common..] {
            let Some(found) = diagram.fragments.get(*fragment) else {
                continue;
            };
            let section = section_of(found, index);
            let condition = found
                .sections
                .get(section)
                .map(|section| section.condition.as_str())
                .unwrap_or("");
            lines.push(fragment_label(found.kind, condition));
            open.push((*fragment, section));
        }
        if let Some((fragment, section)) = open.last_mut()
            && let Some(found) = diagram.fragments.get(*fragment)
        {
            let wanted = section_of(found, index);
            if wanted > *section {
                for next in *section + 1..=wanted {
                    let condition = found
                        .sections
                        .get(next)
                        .map(|section| section.condition.as_str())
                        .unwrap_or("");
                    lines.push(format!("else {condition}"));
                }
                *section = wanted;
            }
        }
        let from = diagram
            .participants
            .get(message.from)
            .map(|participant| participant.name.as_str())
            .unwrap_or("");
        let to = diagram
            .participants
            .get(message.to)
            .map(|participant| participant.name.as_str())
            .unwrap_or("");
        let kind = if message.kind == SequenceMessageKind::Return {
            TextMessage::Return
        } else {
            TextMessage::Call
        };
        lines.push(format!("{from}{}{to}{colon}{}", arrow(kind), message.label));
    }
    while open.pop().is_some() {
        lines.push("end".to_owned());
    }
}

fn section_of(fragment: &lang_api::SequenceFragment, message: usize) -> usize {
    fragment
        .sections
        .iter()
        .position(|section| section.messages.contains(&message))
        .unwrap_or_else(|| fragment.sections.len().saturating_sub(1))
}

/// The picture of the diagram, with the same columns and rows the window draws.
pub fn svg(diagram: &SequenceDiagram, geometry: &Geometry) -> String {
    let mut svg = String::new();
    svg.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    svg.push_str(&format!(
        "<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"{:.0}\" height=\"{:.0}\" viewBox=\"0 0 {:.0} {:.0}\">\n",
        geometry.width, geometry.height, geometry.width, geometry.height
    ));
    svg.push_str("<style>text{font-family:'Segoe UI',sans-serif;fill:#1c1b1a}.name{font-weight:600;font-size:13px}.message{font-size:12px}.condition{font-size:11px}.frame{fill:none;stroke:#a8a39a;stroke-width:1}.frame-alt{stroke:#5e5a54}.lifeline{stroke:#b8b3ab;stroke-width:1}.arrow{stroke:#5e5a54;stroke-width:1.2;fill:none}.return{stroke-dasharray:6 4}.activation{fill:#e9e4dc;stroke:#a8a39a}</style>\n");
    svg.push_str(&format!(
        "<rect class=\"background\" x=\"0\" y=\"0\" width=\"{:.0}\" height=\"{:.0}\" fill=\"#fdfcfa\"/>\n",
        geometry.width, geometry.height
    ));
    if !diagram.title.is_empty() {
        svg.push_str(&format!(
            "<text class=\"name\" x=\"{:.1}\" y=\"{:.1}\">{}</text>\n",
            crate::sequence_layout::MARGIN,
            crate::sequence_layout::TOP - 6.,
            escape(&diagram.title)
        ));
    }
    let bottom = geometry.height - crate::sequence_layout::MARGIN;
    for (index, participant) in diagram.participants.iter().enumerate() {
        let Some(column) = geometry.columns.get(index) else {
            continue;
        };
        let left = column - PARTICIPANT_WIDTH / 2.;
        let top = crate::sequence_layout::TOP;
        let (fill, stroke) = if participant.kind == SequenceParticipantKind::External {
            ("#f4f1ec", "#a8a39a")
        } else {
            ("#ffffff", "#5e5a54")
        };
        svg.push_str(&format!(
            "<g class=\"participant\" data-name=\"{}\">\n",
            escape(&participant.name)
        ));
        svg.push_str(&format!(
            "<rect x=\"{left:.1}\" y=\"{top:.1}\" width=\"{PARTICIPANT_WIDTH:.1}\" height=\"{PARTICIPANT_HEIGHT:.1}\" rx=\"4\" fill=\"{fill}\" stroke=\"{stroke}\"/>\n"
        ));
        svg.push_str(&format!(
            "<text class=\"name\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>\n",
            column,
            top + 24.,
            escape(&participant.name)
        ));
        svg.push_str(&format!(
            "<line class=\"lifeline\" x1=\"{column:.1}\" y1=\"{:.1}\" x2=\"{column:.1}\" y2=\"{bottom:.1}\"/>\n",
            top + PARTICIPANT_HEIGHT
        ));
        svg.push_str("</g>\n");
    }
    for frame in &geometry.fragments {
        let Some(fragment) = diagram.fragments.get(frame.fragment) else {
            continue;
        };
        let condition = fragment
            .sections
            .first()
            .map(|section| section.condition.as_str())
            .unwrap_or("");
        let class = match fragment.kind {
            SequenceFragmentKind::Alt => "frame frame-alt",
            _ => "frame",
        };
        svg.push_str(&format!(
            "<rect class=\"{class}\" x=\"{:.1}\" y=\"{:.1}\" width=\"{:.1}\" height=\"{:.1}\"/>\n",
            frame.left, frame.top, frame.width, frame.height
        ));
        svg.push_str(&format!(
            "<text class=\"condition\" x=\"{:.1}\" y=\"{:.1}\">{}</text>\n",
            frame.left + 6.,
            frame.top + 14.,
            escape(&fragment_label(fragment.kind, condition))
        ));
        for separator in &frame.separators {
            svg.push_str(&format!(
                "<line class=\"frame\" x1=\"{:.1}\" y1=\"{separator:.1}\" x2=\"{:.1}\" y2=\"{separator:.1}\"/>\n",
                frame.left,
                frame.left + frame.width
            ));
        }
        for section in fragment.sections.iter().skip(1) {
            let Some(row) = geometry.rows.get(section.messages.start) else {
                continue;
            };
            svg.push_str(&format!(
                "<text class=\"condition\" x=\"{:.1}\" y=\"{:.1}\">[{}]</text>\n",
                frame.left + 6.,
                row - crate::sequence_layout::ROW_HEIGHT / 2.,
                escape(&section.condition)
            ));
        }
    }
    for activation in &geometry.activations {
        let Some(column) = geometry.columns.get(activation.participant) else {
            continue;
        };
        svg.push_str(&format!(
            "<rect class=\"activation\" x=\"{:.1}\" y=\"{:.1}\" width=\"10\" height=\"{:.1}\"/>\n",
            column - 5.,
            activation.top,
            activation.bottom - activation.top
        ));
    }
    for (index, message) in diagram.messages.iter().enumerate() {
        let (Some(from), Some(to), Some(row)) = (
            geometry.columns.get(message.from),
            geometry.columns.get(message.to),
            geometry.rows.get(index),
        ) else {
            continue;
        };
        let class = if message.kind == SequenceMessageKind::Return {
            "arrow return"
        } else {
            "arrow"
        };
        svg.push_str(&format!(
            "<line class=\"{class}\" x1=\"{from:.1}\" y1=\"{row:.1}\" x2=\"{to:.1}\" y2=\"{row:.1}\"/>\n"
        ));
        svg.push_str(&format!(
            "<text class=\"message\" x=\"{:.1}\" y=\"{:.1}\" text-anchor=\"middle\">{}</text>\n",
            (from + to) / 2.,
            row - 6.,
            escape(&message.label)
        ));
    }
    svg.push_str("</svg>\n");
    svg
}

#[cfg(test)]
mod tests {
    use lang_api::{
        SequenceFragment, SequenceMessage, SequenceParticipant, SequenceSection, SequenceSettings,
    };

    use super::*;
    use crate::sequence_layout::compute;

    fn participant(name: &str, kind: SequenceParticipantKind) -> SequenceParticipant {
        SequenceParticipant {
            name: name.to_owned(),
            qualified: name.to_owned(),
            kind,
            place: None,
        }
    }

    fn message(from: usize, to: usize, kind: SequenceMessageKind, label: &str) -> SequenceMessage {
        SequenceMessage {
            from,
            to,
            kind,
            label: label.to_owned(),
            returns: None,
            place: None,
            fragments: Vec::new(),
            depth: 1,
        }
    }

    fn small() -> SequenceDiagram {
        let mut call = message(0, 1, SequenceMessageKind::Call, "findById(id)");
        call.returns = Some("Owner".to_owned());
        let mut back = message(1, 0, SequenceMessageKind::Return, "Owner");
        back.fragments = Vec::new();
        SequenceDiagram {
            participants: vec![
                participant("Controller", SequenceParticipantKind::Class),
                participant("Repository", SequenceParticipantKind::Interface),
            ],
            messages: vec![call, back],
            fragments: Vec::new(),
            title: String::new(),
        }
    }

    #[test]
    fn plantuml_has_the_participants_the_call_and_the_return() {
        assert_eq!(
            plantuml(&small()),
            "@startuml\n\
             participant Controller\n\
             participant Repository\n\
             Controller -> Repository : findById(id)\n\
             Repository --> Controller : Owner\n\
             @enduml\n"
        );
    }

    #[test]
    fn mermaid_has_the_participants_the_call_and_the_return() {
        assert_eq!(
            mermaid(&small()),
            "sequenceDiagram\n\
             participant Controller\n\
             participant Repository\n\
             Controller->>Repository: findById(id)\n\
             Repository-->>Controller: Owner\n"
        );
    }

    #[test]
    fn the_texts_open_the_frames_with_the_condition_of_the_section() {
        let mut diagram = small();
        diagram.fragments = vec![SequenceFragment {
            kind: SequenceFragmentKind::Alt,
            sections: vec![
                SequenceSection {
                    condition: "found".to_owned(),
                    messages: 0..1,
                },
                SequenceSection {
                    condition: "else".to_owned(),
                    messages: 1..2,
                },
            ],
            depth: 0,
        }];
        diagram.messages[0].fragments = vec![0];
        diagram.messages[1].fragments = vec![0];
        let text = plantuml(&diagram);
        assert!(text.contains("alt found\n"), "{text}");
        assert!(text.contains("else else\n"), "{text}");
        assert!(text.contains("end\n@enduml"), "{text}");
        let text = mermaid(&diagram);
        assert!(text.contains("alt found\n"), "{text}");
        assert!(text.contains("else else\n"), "{text}");
    }

    #[test]
    fn the_svg_names_every_participant_and_message() {
        let diagram = small();
        let geometry = compute(&diagram);
        let svg = svg(&diagram, &geometry);
        assert!(svg.contains("data-name=\"Controller\""));
        assert!(svg.contains("data-name=\"Repository\""));
        assert!(svg.contains("findById(id)"));
        assert!(svg.contains("class=\"arrow return\""));
        assert!(svg.contains("class=\"lifeline\""));
    }

    #[test]
    fn the_default_settings_go_three_calls_deep() {
        assert_eq!(SequenceSettings::default().depth, 3);
    }
}

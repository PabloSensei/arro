//! A template body turned into text with field ranges.

use std::ops::Range;

use crate::overrides::TemplateField;
use crate::template::Template;
use crate::variables::{Values, Variables};

const END: &str = "END";
const SELECTION: &str = "SELECTION";
const EXPR: &str = "EXPR";

/// A field and every place it occurs, relative to the start of the text. The first range
/// is the one the user types in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ranges: Vec<Range<usize>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Expansion {
    pub text: String,
    /// In the order Tab visits them.
    pub fields: Vec<Field>,
    /// Where the caret goes after the last field.
    pub end: usize,
}

enum Piece<'a> {
    Text(&'a str),
    Variable(&'a str),
}

fn pieces(body: &str) -> Vec<Piece<'_>> {
    let mut pieces = Vec::new();
    let mut rest = body;
    while let Some(start) = rest.find('$') {
        pieces.push(Piece::Text(&rest[..start]));
        let after = &rest[start + 1..];
        if let Some(tail) = after.strip_prefix('$') {
            pieces.push(Piece::Text("$"));
            rest = tail;
            continue;
        }
        match after.find('$') {
            Some(end) => {
                pieces.push(Piece::Variable(&after[..end]));
                rest = &after[end + 1..];
            }
            None => {
                pieces.push(Piece::Text(&rest[start..]));
                rest = "";
            }
        }
    }
    pieces.push(Piece::Text(rest));
    pieces
}

/// The names of the variables of `body`, once each, in the order they occur.
pub(crate) fn field_names(body: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for piece in pieces(body) {
        if let Piece::Variable(name) = piece
            && !names.iter().any(|known| known == name)
        {
            names.push(name.to_owned());
        }
    }
    names
}

/// Where an expansion goes and what its variables read besides the template.
pub struct ExpansionSite<'a> {
    /// The indentation of the line the text starts on. It goes in front of every
    /// following line.
    pub indent: &'a str,
    /// What a tab at the start of a body line is.
    pub unit: &'a str,
    /// The selected lines a surround template wraps.
    pub selection: Option<&'a str>,
    /// The expression a postfix template follows.
    pub expression: Option<&'a str>,
    /// The values of the moment, for the variables that ask for them.
    pub variables: Variables,
}

/// A variable while it is being placed: its value and the ranges it occupies.
struct Slot {
    name: String,
    value: String,
    /// The value came from an expression and the user needs no field for it.
    skip: bool,
    ranges: Vec<Range<usize>>,
}

/// Builds the text. `indent` is the indentation of the line the text starts on and goes
/// in front of every following line; a tab at the start of a body line is one `unit`.
pub fn expand(
    template: &Template,
    indent: &str,
    unit: &str,
    selection: Option<&str>,
    expression: Option<&str>,
) -> Expansion {
    let fields: Vec<TemplateField> = template
        .fields
        .iter()
        .map(|(name, default)| TemplateField {
            name: (*name).to_owned(),
            default: (*default).to_owned(),
            ..TemplateField::default()
        })
        .collect();
    expand_fields(
        template,
        &fields,
        &ExpansionSite {
            indent,
            unit,
            selection,
            expression,
            variables: Variables::default(),
        },
    )
}

/// Builds the text with the expressions of the variables. A variable with a value and
/// Skip if defined is no field: Tab walks past it.
pub fn expand_fields(
    template: &Template,
    fields: &[TemplateField],
    site: &ExpansionSite<'_>,
) -> Expansion {
    let (indent, unit) = (site.indent, site.unit);
    let mut values = Values::new(fields, &site.variables);
    let mut slots: Vec<Slot> = fields
        .iter()
        .map(|field| {
            let value = values.value_of(&field.name);
            Slot {
                name: field.name.clone(),
                skip: field.skip && !value.is_empty(),
                value,
                ranges: Vec::new(),
            }
        })
        .collect();
    let mut text = String::new();
    let mut end = None;
    let mut line_indent = indent.to_string();
    let mut at_line_start = false;
    for piece in pieces(template.body) {
        match piece {
            Piece::Text(chunk) => {
                for character in chunk.chars() {
                    match character {
                        '\n' => {
                            text.push('\n');
                            text.push_str(indent);
                            line_indent = indent.to_string();
                            at_line_start = true;
                        }
                        '\t' if at_line_start => {
                            text.push_str(unit);
                            line_indent.push_str(unit);
                        }
                        other => {
                            text.push(other);
                            at_line_start = false;
                        }
                    }
                }
            }
            Piece::Variable(name) => {
                at_line_start = false;
                match name {
                    END => end = Some(text.len()),
                    SELECTION => {
                        let lines = site.selection.unwrap_or_default().split('\n');
                        for (index, line) in lines.enumerate() {
                            if index > 0 {
                                text.push('\n');
                                if !line.is_empty() {
                                    text.push_str(&line_indent);
                                }
                            }
                            text.push_str(line);
                        }
                    }
                    EXPR => text.push_str(site.expression.unwrap_or_default()),
                    name => {
                        let start = text.len();
                        match slots.iter_mut().find(|slot| slot.name == name) {
                            Some(slot) => {
                                text.push_str(&slot.value);
                                if !slot.skip {
                                    slot.ranges.push(start..text.len());
                                }
                            }
                            None => {
                                let mut slot = Slot {
                                    name: name.to_string(),
                                    value: String::new(),
                                    skip: false,
                                    ranges: Vec::new(),
                                };
                                slot.ranges.push(start..text.len());
                                slots.push(slot);
                            }
                        }
                    }
                }
            }
        }
    }
    let fields: Vec<Field> = slots
        .into_iter()
        .filter(|slot| !slot.ranges.is_empty())
        .map(|slot| Field {
            name: slot.name,
            ranges: slot.ranges,
        })
        .collect();
    let end = end.unwrap_or(text.len());
    Expansion { text, fields, end }
}

/// The common indentation of the non-blank lines.
fn common_indent(lines: &str) -> &str {
    let mut common: Option<&str> = None;
    for line in lines.split('\n').filter(|line| !line.trim().is_empty()) {
        let indent = &line[..line.len() - line.trim_start().len()];
        common = Some(match common {
            None => indent,
            Some(known) => {
                let shared = known
                    .bytes()
                    .zip(indent.bytes())
                    .take_while(|(left, right)| left == right)
                    .count();
                &known[..shared]
            }
        });
    }
    common.unwrap_or_default()
}

/// Wraps whole `lines` in a surround template: the lines lose their common indentation,
/// which becomes the indentation of the result.
pub fn surround(template: &Template, lines: &str, unit: &str) -> Expansion {
    let fields: Vec<TemplateField> = template
        .fields
        .iter()
        .map(|(name, default)| TemplateField {
            name: (*name).to_owned(),
            default: (*default).to_owned(),
            ..TemplateField::default()
        })
        .collect();
    surround_fields(template, &fields, lines, unit, Variables::default())
}

/// Wraps whole `lines` with the expressions of the variables.
pub fn surround_fields(
    template: &Template,
    fields: &[TemplateField],
    lines: &str,
    unit: &str,
    variables: Variables,
) -> Expansion {
    let indent = common_indent(lines);
    let dedented: Vec<&str> = lines
        .split('\n')
        .map(|line| line.strip_prefix(indent).unwrap_or(line.trim_start()))
        .collect();
    let dedented = dedented.join("\n");
    let mut expansion = expand_fields(
        template,
        fields,
        &ExpansionSite {
            indent,
            unit,
            selection: Some(&dedented),
            expression: None,
            variables,
        },
    );
    expansion.text.insert_str(0, indent);
    let shift = indent.len();
    for field in &mut expansion.fields {
        for range in &mut field.ranges {
            *range = range.start + shift..range.end + shift;
        }
    }
    expansion.end += shift;
    expansion
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::template::{ANY, Kind};

    fn template(body: &'static str, fields: &'static [(&'static str, &'static str)]) -> Template {
        Template {
            key: "t",
            description: "",
            kind: Kind::Abbreviation,
            body,
            fields,
            contexts: &[ANY],
        }
    }

    #[test]
    fn fields_mirrors_and_the_end_take_their_places() {
        let fori = template(
            "for (int $I$ = 0; $I$ < $LIMIT$; $I$++) {\n\t$END$\n}",
            &[("I", "i")],
        );

        let expansion = expand(&fori, "  ", "    ", None, None);

        assert_eq!(expansion.text, "for (int i = 0; i < ; i++) {\n      \n  }");
        assert_eq!(expansion.fields[0].name, "I");
        assert_eq!(expansion.fields[0].ranges, [9..10, 16..17, 22..23]);
        assert_eq!(expansion.fields[1].ranges, vec![20..20]);
        assert_eq!(
            &expansion.text[..expansion.end],
            "for (int i = 0; i < ; i++) {\n      "
        );
    }

    #[test]
    fn a_dollar_pair_is_a_dollar_and_a_postfix_takes_the_expression() {
        let price = template("$EXPR$ + \"$$\"$END$", &[]);

        let expansion = expand(&price, "", "    ", None, Some("cost"));

        assert_eq!(expansion.text, "cost + \"$\"");
        assert!(expansion.fields.is_empty());
        assert_eq!(expansion.end, expansion.text.len());
    }

    #[test]
    fn surround_keeps_the_relative_indentation_of_the_lines() {
        let wrap = template("if ($C$) {\n\t$SELECTION$\n}$END$", &[]);

        let expansion = surround(&wrap, "    a();\n        b();\n\n    c();", "    ");

        assert_eq!(
            expansion.text,
            "    if () {\n        a();\n            b();\n\n        c();\n    }"
        );
        assert_eq!(expansion.fields[0].ranges, vec![8..8]);
        assert_eq!(expansion.end, expansion.text.len());
    }

    #[test]
    fn an_expression_fills_a_field_and_a_skipped_one_takes_no_stop() {
        let body = template("void $NAME$($END$) {\n}", &[]);
        let fields = [
            crate::overrides::TemplateField {
                name: "NAME".to_owned(),
                expression: "decapitalize(camelCase(\"Save Order\"))".to_owned(),
                ..crate::overrides::TemplateField::default()
            },
            crate::overrides::TemplateField {
                name: "KIND".to_owned(),
                expression: "user()".to_owned(),
                skip: true,
                ..crate::overrides::TemplateField::default()
            },
        ];

        let site = ExpansionSite {
            indent: "",
            unit: "    ",
            selection: None,
            expression: None,
            variables: Variables {
                user: "me".to_owned(),
                ..Variables::default()
            },
        };
        let expansion = expand_fields(&body, &fields, &site);

        assert_eq!(expansion.text, "void saveOrder() {\n}");
        assert_eq!(expansion.fields.len(), 1);
        assert_eq!(expansion.fields[0].name, "NAME");
        assert_eq!(
            &expansion.text[expansion.fields[0].ranges[0].clone()],
            "saveOrder"
        );
    }
}

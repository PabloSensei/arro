//! Import of a colour scheme of the other IDE (`.icls`, XML): the colours of the code and
//! of the editor become the overrides of a theme whose parent is the current theme.
//!
//! The scheme also carries fonts, effects, backgrounds of single attributes and the name
//! of its own parent. Those have no token here and stay behind; the report of the task
//! lists them.

use std::collections::BTreeMap;

use roxmltree::{Document, Node};

use crate::Color;
use crate::format::{ThemeError, ThemeFile};

/// The tokens a scheme may change in the editor itself, by the names of its `colors`
/// section.
const EDITOR_COLORS: &[(&str, &str)] = &[
    ("CARET_COLOR", "caret"),
    ("CARET_ROW_COLOR", "caret_row"),
    ("SELECTION_BACKGROUND", "selection"),
    ("INDENT_GUIDE_COLOR", "indent_guide"),
    ("LINE_NUMBERS_COLOR", "line_number"),
    ("ERROR_STRIPE_COLOR", "stripe_error"),
    ("FOLDED_TEXT_BACKGROUND", "fold_bg"),
];

/// The tokens a scheme may change in the code, by the names of its `attributes` section.
const ATTRIBUTES: &[(&str, &str)] = &[
    ("DEFAULT_TEXT", "code"),
    ("DEFAULT_KEYWORD", "keyword"),
    ("DEFAULT_STRING", "string"),
    ("DEFAULT_NUMBER", "number"),
    ("DEFAULT_LINE_COMMENT", "comment"),
    ("DEFAULT_BLOCK_COMMENT", "comment"),
    ("DEFAULT_DOC_COMMENT", "doc"),
    ("DEFAULT_DOC_COMMENT_TAG", "doc_tag"),
    ("DEFAULT_DOC_MARKUP", "doc"),
    ("DEFAULT_FUNCTION_CALL", "function"),
    ("DEFAULT_FUNCTION_DECLARATION", "function"),
    ("DEFAULT_CLASS_NAME", "semantic_type"),
    ("DEFAULT_CLASS_REFERENCE", "semantic_type"),
    ("DEFAULT_INTERFACE_NAME", "semantic_type"),
    ("DEFAULT_INSTANCE_FIELD", "field"),
    ("DEFAULT_STATIC_FIELD", "field"),
    ("DEFAULT_LOCAL_VARIABLE", "semantic_local"),
    ("DEFAULT_PARAMETER", "semantic_parameter"),
    ("DEFAULT_METHOD_CALL", "semantic_method"),
    ("DEFAULT_METHOD_DECLARATION", "semantic_method"),
    ("DEFAULT_CONSTANT", "semantic_constant"),
    ("DEFAULT_ANNOTATION_NAME", "annotation"),
];

/// Reads a scheme of the other IDE into a theme over `parent`, of the look `dark`.
pub fn from_icls(xml: &str, parent: &str, dark: bool) -> Result<ThemeFile, ThemeError> {
    let document = Document::parse(xml)
        .map_err(|error| ThemeError::new(format!("not an XML file: {error}")))?;
    let scheme = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "scheme")
        .ok_or_else(|| ThemeError::new("no scheme in the file"))?;
    let name = scheme
        .attribute("name")
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Imported")
        .to_owned();
    let mut ui = BTreeMap::new();
    let mut syntax = BTreeMap::new();
    for section in scheme.children().filter(Node::is_element) {
        match section.tag_name().name() {
            "colors" => {
                for option in options(section) {
                    let Some((_, token)) = EDITOR_COLORS
                        .iter()
                        .find(|(name, _)| *name == option_name(&option))
                    else {
                        continue;
                    };
                    if let Some(color) = option_value(&option) {
                        ui.insert((*token).to_owned(), color);
                    }
                }
            }
            "attributes" => {
                for option in options(section) {
                    let Some((_, token)) = ATTRIBUTES
                        .iter()
                        .find(|(name, _)| *name == option_name(&option))
                    else {
                        continue;
                    };
                    let Some(value) = option
                        .children()
                        .find(|child| child.is_element() && child.tag_name().name() == "value")
                    else {
                        continue;
                    };
                    let Some(color) = value
                        .children()
                        .find(|child| {
                            child.is_element()
                                && child.tag_name().name() == "option"
                                && child.attribute("name") == Some("FOREGROUND")
                        })
                        .and_then(|foreground| foreground.attribute("value"))
                        .and_then(read_color)
                    else {
                        continue;
                    };
                    syntax.insert((*token).to_owned(), color);
                }
            }
            _ => {}
        }
    }
    Ok(ThemeFile {
        name,
        dark,
        parent: Some(parent.to_owned()),
        ui,
        syntax,
    })
}

/// The `option` rows directly under a section.
fn options<'a>(section: Node<'a, 'a>) -> impl Iterator<Item = Node<'a, 'a>> {
    section
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "option")
}

fn option_name<'a>(option: &'a Node<'a, 'a>) -> &'a str {
    option.attribute("name").unwrap_or_default()
}

/// The colour of a plain `option`, as a theme file writes it.
fn option_value(option: &Node) -> Option<String> {
    option.attribute("value").and_then(read_color)
}

fn read_color(text: &str) -> Option<String> {
    Color::from_text(text).map(Color::to_text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Tokens;

    /// A small scheme with the parts a real file has: an editor colour, a keyword, a
    /// function, a font effect that has no token and a name.
    const SAMPLE: &str = r##"<?xml version="1.0" encoding="UTF-8"?>
<scheme name="Sample" version="142" parent_scheme="Darcula">
  <metaInfo>
    <property name="created">2024-01-01</property>
  </metaInfo>
  <colors>
    <option name="CARET_ROW_COLOR" value="2b2b2b" />
    <option name="SELECTION_BACKGROUND" value="214283" />
    <option name="RIGHT_MARGIN_COLOR" value="555555" />
  </colors>
  <attributes>
    <option name="DEFAULT_KEYWORD">
      <value>
        <option name="FOREGROUND" value="cc7832" />
        <option name="FONT_TYPE" value="1" />
      </value>
    </option>
    <option name="DEFAULT_FUNCTION_DECLARATION">
      <value>
        <option name="FOREGROUND" value="ffc66d" />
      </value>
    </option>
    <option name="DEFAULT_TEXT">
      <value>
        <option name="FOREGROUND" value="a9b7c6" />
        <option name="BACKGROUND" value="2b2b2b" />
      </value>
    </option>
  </attributes>
</scheme>
"##;

    #[test]
    fn a_scheme_becomes_a_theme_over_the_parent() {
        let file = from_icls(SAMPLE, "dark", true).unwrap();
        assert_eq!(file.name, "Sample");
        assert_eq!(file.parent.as_deref(), Some("dark"));
        assert!(file.dark);
        assert_eq!(file.ui.get("caret_row"), Some(&"#2b2b2b".to_owned()));
        assert_eq!(file.ui.get("selection"), Some(&"#214283".to_owned()));
        assert!(
            !file.ui.contains_key("right_margin"),
            "no token to carry it"
        );
        assert_eq!(file.syntax.get("keyword"), Some(&"#cc7832".to_owned()));
        assert_eq!(
            file.syntax.get("function"),
            Some(&"#ffc66d".to_owned()),
            "the function of a declaration"
        );
        assert_eq!(file.syntax.get("code"), Some(&"#a9b7c6".to_owned()));
        let (tokens, warnings) = file.resolve(Some(Tokens::dark())).unwrap();
        assert!(warnings.is_empty(), "{warnings:?}");
        assert_eq!(tokens.keyword, Color::from_text("#cc7832").unwrap());
        assert_eq!(
            tokens.text,
            Tokens::dark().text,
            "the rest comes from the parent"
        );
    }

    #[test]
    fn a_file_that_is_no_scheme_is_refused() {
        assert!(from_icls("<html><body/></html>", "dark", true).is_err());
        assert!(from_icls("not xml", "dark", true).is_err());
    }
}

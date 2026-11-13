//! Highlighting of the text formats around a project: JSON, XML, YAML, shell, Windows
//! batch and properties files. Each format brings a tree-sitter grammar and our own
//! highlight query. There are no folds for the formats where a fold would not help.

use std::path::Path;

use syntax::{FoldStyle, LanguageConfig};

const BRACED_FOLD_STYLES: &[FoldStyle] = &[FoldStyle {
    capture: "fold",
    placeholder: "{...}",
    keep: 0,
    joined: false,
    collapsed_on_open: false,
}];

const TAG_FOLD_STYLES: &[FoldStyle] = &[FoldStyle {
    capture: "fold",
    placeholder: "<...>",
    keep: 0,
    joined: false,
    collapsed_on_open: false,
}];

pub fn json() -> LanguageConfig {
    LanguageConfig {
        name: "JSON",
        grammar: tree_sitter_json::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/json/highlights.scm"),
        folds: include_str!("../../../../queries/json/folds.scm"),
        indents: include_str!("../../../../queries/json/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: None,
        block_comment: None,
        statements: None,
        indent_unit: "  ",
        file_mask: "*.json",
    }
}

pub fn xml() -> LanguageConfig {
    LanguageConfig {
        name: "XML",
        grammar: tree_sitter_xml::LANGUAGE_XML.into(),
        highlights: include_str!("../../../../queries/xml/highlights.scm"),
        folds: include_str!("../../../../queries/xml/folds.scm"),
        indents: include_str!("../../../../queries/xml/indents.scm"),
        fold_styles: TAG_FOLD_STYLES,
        line_comment: None,
        block_comment: Some(("<!--", "-->")),
        statements: None,
        indent_unit: "  ",
        file_mask: "*.xml",
    }
}

pub fn yaml() -> LanguageConfig {
    LanguageConfig {
        name: "YAML",
        grammar: tree_sitter_yaml::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/yaml/highlights.scm"),
        folds: include_str!("../../../../queries/yaml/folds.scm"),
        indents: include_str!("../../../../queries/yaml/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("#"),
        block_comment: None,
        statements: None,
        indent_unit: "  ",
        file_mask: "*.yml *.yaml",
    }
}

pub fn shell() -> LanguageConfig {
    LanguageConfig {
        name: "Shell",
        grammar: tree_sitter_bash::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/shell/highlights.scm"),
        folds: include_str!("../../../../queries/shell/folds.scm"),
        indents: include_str!("../../../../queries/shell/indents.scm"),
        fold_styles: &[],
        line_comment: Some("#"),
        block_comment: None,
        statements: None,
        indent_unit: "  ",
        file_mask: "*.sh",
    }
}

pub fn batch() -> LanguageConfig {
    LanguageConfig {
        name: "Windows Batch",
        grammar: tree_sitter_batch::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/batch/highlights.scm"),
        folds: include_str!("../../../../queries/batch/folds.scm"),
        indents: include_str!("../../../../queries/batch/indents.scm"),
        fold_styles: &[],
        line_comment: Some("REM"),
        block_comment: None,
        statements: None,
        indent_unit: "  ",
        file_mask: "*.bat *.cmd",
    }
}

pub fn properties() -> LanguageConfig {
    LanguageConfig {
        name: "Properties",
        grammar: tree_sitter_properties::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/properties/highlights.scm"),
        folds: include_str!("../../../../queries/properties/folds.scm"),
        indents: include_str!("../../../../queries/properties/indents.scm"),
        fold_styles: &[],
        line_comment: Some("#"),
        block_comment: None,
        statements: None,
        indent_unit: "  ",
        file_mask: "*.properties",
    }
}

/// The format of `path` by its extension, whatever its case.
pub fn language_for(path: &Path) -> Option<LanguageConfig> {
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "json" | "jsonc" => json(),
        "xml" | "xsd" | "xsl" | "svg" | "fxml" | "iml" => xml(),
        "yml" | "yaml" => yaml(),
        "sh" | "bash" | "zsh" => shell(),
        "bat" | "cmd" => batch(),
        "properties" => properties(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use tree_sitter::StreamingIterator;

    use super::*;

    /// Parses `text`, checks that every query of the language compiles and lists the
    /// captures as `name text` pairs.
    fn captures(config: &LanguageConfig, text: &str) -> Vec<String> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&config.grammar).unwrap();
        let tree = parser.parse(text, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}: {}",
            config.name,
            tree.root_node().to_sexp()
        );
        let query = tree_sitter::Query::new(&config.grammar, config.highlights).unwrap();
        tree_sitter::Query::new(&config.grammar, config.folds).unwrap();
        tree_sitter::Query::new(&config.grammar, config.indents).unwrap();

        let mut cursor = tree_sitter::QueryCursor::new();
        let mut found = Vec::new();
        let mut matches = cursor.matches(&query, tree.root_node(), text.as_bytes());
        while let Some(found_match) = matches.next() {
            for capture in found_match.captures() {
                let name = query.capture_names()[capture.index as usize];
                let node = &text[capture.node.byte_range()];
                found.push(format!("{name} {node}"));
            }
        }
        found
    }

    fn has(found: &[String], wanted: &str) {
        assert!(
            found.iter().any(|item| item == wanted),
            "{wanted} in {found:?}"
        );
    }

    /// Parses `text` and lists the nodes the fold query captures.
    fn folds_of(config: &LanguageConfig, text: &str) -> Vec<String> {
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&config.grammar).unwrap();
        let tree = parser.parse(text, None).unwrap();
        assert!(!tree.root_node().has_error());
        let query = tree_sitter::Query::new(&config.grammar, config.folds).unwrap();
        let mut cursor = tree_sitter::QueryCursor::new();
        let mut found = Vec::new();
        let mut matches = cursor.matches(&query, tree.root_node(), text.as_bytes());
        while let Some(found_match) = matches.next() {
            for capture in found_match.captures() {
                found.push(text[capture.node.byte_range()].to_owned());
            }
        }
        found
    }

    #[test]
    fn blocks_of_json_xml_and_yaml_fold() {
        let json = folds_of(&json(), "{\n  \"a\": 1,\n  \"b\": 2\n}\n");
        assert!(
            json.iter()
                .any(|fold| fold.starts_with('{') && fold.ends_with('}')),
            "{json:?}"
        );

        let xml = folds_of(&xml(), "<root>\n  <child/>\n</root>\n");
        assert!(
            xml.iter()
                .any(|fold| fold.starts_with("<root>") && fold.ends_with("</root>")),
            "{xml:?}"
        );

        let yaml = folds_of(&yaml(), "root:\n  name: app\n  port: 8080\n");
        assert!(
            yaml.iter().any(|fold| fold.contains("name: app")),
            "{yaml:?}"
        );
    }

    #[test]
    fn json_keys_strings_and_numbers_are_coloured() {
        let text = "{\n  \"name\": \"app\",\n  \"port\": 8080,\n  \"open\": true\n}\n";
        let found = captures(&json(), text);
        has(&found, "field \"name\"");
        has(&found, "string \"app\"");
        has(&found, "number 8080");
        has(&found, "constant true");
    }

    #[test]
    fn xml_tags_attributes_and_comments_are_coloured() {
        let text = "<?xml version=\"1.0\"?>\n<root id=\"7\">\n  <!-- a note -->\n  <child>text</child>\n</root>\n";
        let found = captures(&xml(), text);
        has(&found, "keyword root");
        has(&found, "keyword child");
        has(&found, "field id");
        has(&found, "string \"7\"");
        has(&found, "comment <!-- a note -->");
    }

    #[test]
    fn yaml_keys_and_scalars_are_coloured() {
        let text = "name: app\nport: 8080\nitems:\n  - one\n";
        let found = captures(&yaml(), text);
        has(&found, "field name");
        has(&found, "string app");
        has(&found, "number 8080");
        has(&found, "field items");
    }

    #[test]
    fn shell_commands_and_variables_are_coloured() {
        let text = "#!/bin/sh\nset -e\ncount=1\necho \"$HOME\"\n";
        let found = captures(&shell(), text);
        has(&found, "function echo");
        has(&found, "constant HOME");
        has(&found, "comment #!/bin/sh");
    }

    #[test]
    fn batch_comments_and_variables_are_coloured() {
        let text = "@echo off\nREM build the app\nset NAME=world\necho %NAME%\n";
        let found = captures(&batch(), text);
        assert!(
            found
                .iter()
                .any(|item| item.starts_with("comment ") && item.contains("REM")),
            "{found:?}"
        );
        has(&found, "constant %NAME%");
    }

    #[test]
    fn properties_keys_and_values_are_coloured() {
        let text = "# a note\nname=app\nport:8080\n";
        let found = captures(&properties(), text);
        has(&found, "field name");
        has(&found, "string app");
        has(&found, "field port");
        has(&found, "string 8080");
    }

    #[test]
    fn comment_marks_follow_the_format() {
        assert_eq!(json().line_comment, None);
        assert_eq!(json().block_comment, None);
        assert_eq!(xml().line_comment, None);
        assert_eq!(xml().block_comment, Some(("<!--", "-->")));
        for config in [yaml(), shell(), properties()] {
            assert_eq!(config.line_comment, Some("#"), "{}", config.name);
        }
        assert_eq!(batch().line_comment, Some("REM"));
    }

    #[test]
    fn paths_choose_the_format() {
        for (name, expected) in [
            ("data.json", "JSON"),
            ("data.jsonc", "JSON"),
            ("layout.xml", "XML"),
            ("schema.xsd", "XML"),
            ("style.xsl", "XML"),
            ("image.svg", "XML"),
            ("view.fxml", "XML"),
            ("module.iml", "XML"),
            ("config.yml", "YAML"),
            ("config.yaml", "YAML"),
            ("run.sh", "Shell"),
            ("run.bash", "Shell"),
            ("run.zsh", "Shell"),
            ("run.bat", "Windows Batch"),
            ("run.cmd", "Windows Batch"),
            ("app.properties", "Properties"),
        ] {
            let config = language_for(Path::new(name)).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(config.name, expected, "{name}");
        }
        assert_eq!(
            language_for(Path::new("NOTES.JSON")).map(|config| config.name),
            Some("JSON")
        );
        assert_eq!(
            language_for(Path::new("SAMPLE.XML")).map(|config| config.name),
            Some("XML")
        );
        assert_eq!(
            language_for(Path::new("run.CMD")).map(|config| config.name),
            Some("Windows Batch")
        );
        assert!(language_for(Path::new("Main.kt")).is_none());
        assert!(language_for(Path::new("README.md")).is_none());
        assert!(language_for(Path::new("Makefile")).is_none());
    }
}

//! Highlighting of Dockerfiles: the grammar of `tree-sitter-dockerfile-updated` and our own
//! highlight query. No folds and no indents yet.

use std::path::Path;

use syntax::LanguageConfig;

pub fn config() -> LanguageConfig {
    LanguageConfig {
        name: "Dockerfile",
        grammar: tree_sitter_dockerfile_updated::language(),
        highlights: include_str!("../../../../queries/dockerfile/highlights.scm"),
        folds: include_str!("../../../../queries/dockerfile/folds.scm"),
        indents: include_str!("../../../../queries/dockerfile/indents.scm"),
        fold_styles: &[],
        line_comment: Some("#"),
        block_comment: None,
        statements: None,
        indent_unit: "    ",
        file_mask: "Dockerfile",
    }
}

/// `Dockerfile`, `Containerfile`, `Dockerfile.dev` or `app.dockerfile`.
pub fn is_dockerfile(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let name = name.to_ascii_lowercase();
    let stem = name.split('.').next().unwrap_or_default();
    stem == "dockerfile" || stem == "containerfile" || name.ends_with(".dockerfile")
}

pub fn language_for(path: &Path) -> Option<LanguageConfig> {
    is_dockerfile(path).then(config)
}

#[cfg(test)]
mod tests {
    use tree_sitter::StreamingIterator;

    use super::*;

    #[test]
    fn dockerfiles_are_known_by_name() {
        for name in [
            "Dockerfile",
            "Containerfile",
            "Dockerfile.dev",
            "app.dockerfile",
        ] {
            assert!(is_dockerfile(Path::new(name)), "{name}");
        }
        assert!(!is_dockerfile(Path::new("docker-compose.yml")));
        assert!(!is_dockerfile(Path::new("Main.txt")));
    }

    #[test]
    fn the_query_fits_the_grammar_and_finds_instructions() {
        let config = config();
        let mut parser = tree_sitter::Parser::new();
        parser.set_language(&config.grammar).unwrap();
        let text = "# base\nFROM eclipse-temurin:21 AS run\nARG PORT=8080\nCMD [\"app\"]\n";
        let tree = parser.parse(text, None).unwrap();
        assert!(
            !tree.root_node().has_error(),
            "{}",
            tree.root_node().to_sexp()
        );

        let query = tree_sitter::Query::new(&config.grammar, config.highlights).unwrap();
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
        for wanted in [
            "comment # base",
            "keyword FROM",
            "keyword ARG",
            "keyword CMD",
            "string \"app\"",
        ] {
            assert!(
                found.iter().any(|item| item == wanted),
                "{wanted} in {found:?}"
            );
        }
    }
}

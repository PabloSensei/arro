//! Highlighting of the text formats around a project: JSON, XML, YAML, shell, Windows
//! batch, properties, Groovy, Kotlin, TOML, SQL, HTML, CSS, JavaScript, TypeScript and
//! Markdown files, plus the source languages Python, C, C++, C#, Go, Rust, Scala, PHP,
//! Ruby, Swift, Lua, PowerShell, Protobuf, INI, diff and Makefile. Each format brings a
//! tree-sitter grammar and our own highlight query. There are no folds for the formats
//! where a fold would not help.

use std::path::Path;

use syntax::{FoldStyle, Injection, LanguageConfig};

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

const BRACKET_FOLD_STYLES: &[FoldStyle] = &[
    FoldStyle {
        capture: "fold",
        placeholder: "[...]",
        keep: 0,
        joined: false,
        collapsed_on_open: false,
    },
    FoldStyle {
        capture: "fold.braces",
        placeholder: "{...}",
        keep: 0,
        joined: false,
        collapsed_on_open: false,
    },
];

const PLAIN_FOLD_STYLES: &[FoldStyle] = &[FoldStyle {
    capture: "fold",
    placeholder: "...",
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
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.json",
        injections: Vec::new(),
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
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.xml",
        injections: Vec::new(),
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
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.yml *.yaml",
        injections: Vec::new(),
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
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.sh",
        injections: Vec::new(),
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
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.bat *.cmd",
        injections: Vec::new(),
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
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.properties",
        injections: Vec::new(),
    }
}

pub fn groovy() -> LanguageConfig {
    LanguageConfig {
        name: "Groovy",
        grammar: tree_sitter_groovy::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/groovy/highlights.scm"),
        folds: include_str!("../../../../queries/groovy/folds.scm"),
        indents: include_str!("../../../../queries/groovy/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "    ",
        file_mask: "*.groovy *.gradle Jenkinsfile",
        injections: Vec::new(),
    }
}

pub fn kotlin() -> LanguageConfig {
    LanguageConfig {
        name: "Kotlin",
        grammar: tree_sitter_kotlin_ng::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/kotlin/highlights.scm"),
        folds: include_str!("../../../../queries/kotlin/folds.scm"),
        indents: include_str!("../../../../queries/kotlin/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "    ",
        file_mask: "*.kt *.kts",
        injections: Vec::new(),
    }
}

pub fn python() -> LanguageConfig {
    LanguageConfig {
        name: "Python",
        grammar: tree_sitter_python::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/python/highlights.scm"),
        folds: include_str!("../../../../queries/python/folds.scm"),
        indents: include_str!("../../../../queries/python/indents.scm"),
        fold_styles: PLAIN_FOLD_STYLES,
        line_comment: Some("#"),
        block_comment: None,
        statements: None,
        move_units: "",
        indent_unit: "    ",
        file_mask: "*.py *.pyi",
        injections: Vec::new(),
    }
}

pub fn c() -> LanguageConfig {
    LanguageConfig {
        name: "C",
        grammar: tree_sitter_c::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/c/highlights.scm"),
        folds: include_str!("../../../../queries/c/folds.scm"),
        indents: include_str!("../../../../queries/c/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "    ",
        file_mask: "*.c *.h",
        injections: Vec::new(),
    }
}

pub fn cpp() -> LanguageConfig {
    LanguageConfig {
        name: "C++",
        grammar: tree_sitter_cpp::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/cpp/highlights.scm"),
        folds: include_str!("../../../../queries/cpp/folds.scm"),
        indents: include_str!("../../../../queries/cpp/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "    ",
        file_mask: "*.cpp *.cc *.cxx *.hpp *.hh *.hxx",
        injections: Vec::new(),
    }
}

pub fn csharp() -> LanguageConfig {
    LanguageConfig {
        name: "C#",
        grammar: tree_sitter_c_sharp::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/csharp/highlights.scm"),
        folds: include_str!("../../../../queries/csharp/folds.scm"),
        indents: include_str!("../../../../queries/csharp/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "    ",
        file_mask: "*.cs",
        injections: Vec::new(),
    }
}

pub fn go() -> LanguageConfig {
    LanguageConfig {
        name: "Go",
        grammar: tree_sitter_go::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/go/highlights.scm"),
        folds: include_str!("../../../../queries/go/folds.scm"),
        indents: include_str!("../../../../queries/go/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "\t",
        file_mask: "*.go",
        injections: Vec::new(),
    }
}

pub fn rust() -> LanguageConfig {
    LanguageConfig {
        name: "Rust",
        grammar: tree_sitter_rust::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/rust/highlights.scm"),
        folds: include_str!("../../../../queries/rust/folds.scm"),
        indents: include_str!("../../../../queries/rust/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "    ",
        file_mask: "*.rs",
        injections: Vec::new(),
    }
}

pub fn scala() -> LanguageConfig {
    LanguageConfig {
        name: "Scala",
        grammar: tree_sitter_scala::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/scala/highlights.scm"),
        folds: include_str!("../../../../queries/scala/folds.scm"),
        indents: include_str!("../../../../queries/scala/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.scala *.sc *.sbt",
        injections: Vec::new(),
    }
}

pub fn php() -> LanguageConfig {
    LanguageConfig {
        name: "PHP",
        grammar: tree_sitter_php::LANGUAGE_PHP.into(),
        highlights: include_str!("../../../../queries/php/highlights.scm"),
        folds: include_str!("../../../../queries/php/folds.scm"),
        indents: include_str!("../../../../queries/php/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "    ",
        file_mask: "*.php",
        injections: Vec::new(),
    }
}

pub fn ruby() -> LanguageConfig {
    LanguageConfig {
        name: "Ruby",
        grammar: tree_sitter_ruby::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/ruby/highlights.scm"),
        folds: include_str!("../../../../queries/ruby/folds.scm"),
        indents: include_str!("../../../../queries/ruby/indents.scm"),
        fold_styles: PLAIN_FOLD_STYLES,
        line_comment: Some("#"),
        block_comment: None,
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.rb Gemfile Rakefile",
        injections: Vec::new(),
    }
}

pub fn swift() -> LanguageConfig {
    LanguageConfig {
        name: "Swift",
        grammar: tree_sitter_swift::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/swift/highlights.scm"),
        folds: include_str!("../../../../queries/swift/folds.scm"),
        indents: include_str!("../../../../queries/swift/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "    ",
        file_mask: "*.swift",
        injections: Vec::new(),
    }
}

pub fn lua() -> LanguageConfig {
    LanguageConfig {
        name: "Lua",
        grammar: tree_sitter_lua::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/lua/highlights.scm"),
        folds: include_str!("../../../../queries/lua/folds.scm"),
        indents: include_str!("../../../../queries/lua/indents.scm"),
        fold_styles: PLAIN_FOLD_STYLES,
        line_comment: Some("--"),
        block_comment: Some(("--[[", "]]")),
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.lua",
        injections: Vec::new(),
    }
}

pub fn powershell() -> LanguageConfig {
    LanguageConfig {
        name: "PowerShell",
        grammar: tree_sitter_powershell::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/powershell/highlights.scm"),
        folds: include_str!("../../../../queries/powershell/folds.scm"),
        indents: include_str!("../../../../queries/powershell/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("#"),
        block_comment: Some(("<#", "#>")),
        statements: None,
        move_units: "",
        indent_unit: "    ",
        file_mask: "*.ps1 *.psm1",
        injections: Vec::new(),
    }
}

pub fn protobuf() -> LanguageConfig {
    LanguageConfig {
        name: "Protobuf",
        grammar: tree_sitter_proto::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/protobuf/highlights.scm"),
        folds: include_str!("../../../../queries/protobuf/folds.scm"),
        indents: include_str!("../../../../queries/protobuf/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.proto",
        injections: Vec::new(),
    }
}

pub fn ini() -> LanguageConfig {
    LanguageConfig {
        name: "INI",
        grammar: tree_sitter_ini::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/ini/highlights.scm"),
        folds: include_str!("../../../../queries/ini/folds.scm"),
        indents: include_str!("../../../../queries/ini/indents.scm"),
        fold_styles: BRACKET_FOLD_STYLES,
        line_comment: Some("#"),
        block_comment: None,
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.ini *.cfg",
        injections: Vec::new(),
    }
}

pub fn diff() -> LanguageConfig {
    LanguageConfig {
        name: "Diff",
        grammar: tree_sitter_diff::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/diff/highlights.scm"),
        folds: include_str!("../../../../queries/diff/folds.scm"),
        indents: include_str!("../../../../queries/diff/indents.scm"),
        fold_styles: PLAIN_FOLD_STYLES,
        line_comment: None,
        block_comment: None,
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.diff *.patch",
        injections: Vec::new(),
    }
}

pub fn make() -> LanguageConfig {
    LanguageConfig {
        name: "Makefile",
        grammar: tree_sitter_make::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/make/highlights.scm"),
        folds: include_str!("../../../../queries/make/folds.scm"),
        indents: include_str!("../../../../queries/make/indents.scm"),
        fold_styles: PLAIN_FOLD_STYLES,
        line_comment: Some("#"),
        block_comment: None,
        statements: None,
        move_units: "",
        indent_unit: "\t",
        file_mask: "Makefile *.mk",
        injections: Vec::new(),
    }
}

pub fn toml() -> LanguageConfig {
    LanguageConfig {
        name: "TOML",
        grammar: tree_sitter_toml_ng::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/toml/highlights.scm"),
        folds: include_str!("../../../../queries/toml/folds.scm"),
        indents: include_str!("../../../../queries/toml/indents.scm"),
        fold_styles: BRACKET_FOLD_STYLES,
        line_comment: Some("#"),
        block_comment: None,
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.toml",
        injections: Vec::new(),
    }
}

pub fn sql() -> LanguageConfig {
    LanguageConfig {
        name: "SQL",
        grammar: tree_sitter_sequel::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/sql/highlights.scm"),
        folds: include_str!("../../../../queries/sql/folds.scm"),
        indents: include_str!("../../../../queries/sql/indents.scm"),
        fold_styles: &[],
        line_comment: Some("--"),
        block_comment: None,
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.sql",
        injections: Vec::new(),
    }
}

pub fn html() -> LanguageConfig {
    LanguageConfig {
        name: "HTML",
        grammar: tree_sitter_html::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/html/highlights.scm"),
        folds: include_str!("../../../../queries/html/folds.scm"),
        indents: include_str!("../../../../queries/html/indents.scm"),
        fold_styles: TAG_FOLD_STYLES,
        line_comment: None,
        block_comment: Some(("<!--", "-->")),
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.html *.htm",
        injections: Vec::new(),
    }
}

pub fn css() -> LanguageConfig {
    LanguageConfig {
        name: "CSS",
        grammar: tree_sitter_css::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/css/highlights.scm"),
        folds: include_str!("../../../../queries/css/folds.scm"),
        indents: include_str!("../../../../queries/css/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: None,
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.css",
        injections: Vec::new(),
    }
}

pub fn javascript() -> LanguageConfig {
    LanguageConfig {
        name: "JavaScript",
        grammar: tree_sitter_javascript::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/javascript/highlights.scm"),
        folds: include_str!("../../../../queries/javascript/folds.scm"),
        indents: include_str!("../../../../queries/javascript/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.js *.mjs *.cjs",
        injections: Vec::new(),
    }
}

/// TypeScript with the plain grammar, where `.ts` files live. Angle-bracket type
/// assertions parse here but not in TSX, so the two files get their own grammar.
pub fn typescript() -> LanguageConfig {
    LanguageConfig {
        name: "TypeScript",
        grammar: tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        highlights: include_str!("../../../../queries/typescript/highlights.scm"),
        folds: include_str!("../../../../queries/typescript/folds.scm"),
        indents: include_str!("../../../../queries/typescript/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.ts",
        injections: Vec::new(),
    }
}

/// TypeScript with JSX. The highlights are the same: JSX elements stay plain.
pub fn tsx() -> LanguageConfig {
    LanguageConfig {
        name: "TypeScript",
        grammar: tree_sitter_typescript::LANGUAGE_TSX.into(),
        highlights: include_str!("../../../../queries/typescript/highlights.scm"),
        folds: include_str!("../../../../queries/typescript/folds.scm"),
        indents: include_str!("../../../../queries/typescript/indents.scm"),
        fold_styles: BRACED_FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.tsx",
        injections: Vec::new(),
    }
}

pub fn markdown() -> LanguageConfig {
    LanguageConfig {
        name: "Markdown",
        grammar: tree_sitter_md::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/markdown/highlights.scm"),
        folds: include_str!("../../../../queries/markdown/folds.scm"),
        indents: include_str!("../../../../queries/markdown/indents.scm"),
        fold_styles: PLAIN_FOLD_STYLES,
        line_comment: None,
        block_comment: Some(("<!--", "-->")),
        statements: None,
        move_units: "",
        indent_unit: "  ",
        file_mask: "*.md *.markdown",
        injections: vec![
            // The inline grammar reads the text inside the block grammar's paragraphs,
            // headings and list items.
            Injection {
                node: "inline",
                text_node: None,
                name_node: None,
                skip_nodes: &["block_continuation"],
                language: Some(tree_sitter_md::INLINE_LANGUAGE.into()),
                highlights: Some(include_str!(
                    "../../../../queries/markdown/highlights_inline.scm"
                )),
                resolve: None,
                replace: false,
            },
            // A fenced block names its language in the info string.
            Injection {
                node: "fenced_code_block",
                text_node: Some("code_fence_content"),
                name_node: Some("info_string"),
                skip_nodes: &["block_continuation"],
                language: None,
                highlights: None,
                resolve: Some(language_by_name),
                replace: true,
            },
        ],
    }
}

/// The language a name of a document stands for, such as the language of a fenced
/// block. The name is a file extension or a common word for one.
pub fn language_by_name(name: &str) -> Option<LanguageConfig> {
    let name = name.split_whitespace().next()?.to_ascii_lowercase();
    language_for(Path::new(&format!("fence.{name}")))
}

/// The format of `path` by its extension, whatever its case. `Jenkinsfile` has no
/// extension and is the build script of a Jenkins project; `Makefile`, `Gemfile`,
/// `Rakefile` and `.editorconfig` are known by name for the same reason.
pub fn language_for(path: &Path) -> Option<LanguageConfig> {
    if let Some(name) = path.file_name().and_then(|name| name.to_str()) {
        if name.eq_ignore_ascii_case("jenkinsfile") {
            return Some(groovy());
        }
        if name.eq_ignore_ascii_case("makefile") {
            return Some(make());
        }
        if name.eq_ignore_ascii_case("gemfile") || name.eq_ignore_ascii_case("rakefile") {
            return Some(ruby());
        }
        if name.eq_ignore_ascii_case(".editorconfig") {
            return Some(ini());
        }
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    Some(match extension.as_str() {
        "json" | "jsonc" => json(),
        "xml" | "xsd" | "xsl" | "svg" | "fxml" | "iml" => xml(),
        "yml" | "yaml" => yaml(),
        "sh" | "bash" | "zsh" | "shell" => shell(),
        "bat" | "cmd" => batch(),
        "properties" => properties(),
        "groovy" | "gradle" => groovy(),
        "kt" | "kts" | "kotlin" => kotlin(),
        "toml" => toml(),
        "sql" => sql(),
        "html" | "htm" => html(),
        "css" => css(),
        "js" | "mjs" | "cjs" | "javascript" => javascript(),
        "ts" | "typescript" => typescript(),
        "tsx" => tsx(),
        "md" | "markdown" => markdown(),
        "py" | "pyi" | "python" => python(),
        "c" | "h" => c(),
        "cpp" | "cc" | "cxx" | "hpp" | "hh" | "hxx" | "c++" => cpp(),
        "cs" | "csharp" | "c#" => csharp(),
        "go" => go(),
        "rs" | "rust" => rust(),
        "scala" | "sc" | "sbt" => scala(),
        "php" => php(),
        "rb" | "ruby" => ruby(),
        "swift" => swift(),
        "lua" => lua(),
        "ps1" | "psm1" | "powershell" => powershell(),
        "proto" | "protobuf" => protobuf(),
        "ini" | "cfg" | "editorconfig" => ini(),
        "diff" | "patch" => diff(),
        "mk" | "make" | "makefile" => make(),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc::{Receiver, channel};
    use std::time::Duration;

    use syntax::{DocumentId, HighlightKind, HighlightSpan, SyntaxService, SyntaxSnapshot};
    use text_core::{Change, Edit, RopeStore, TextStore};
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
            ("build.gradle", "Groovy"),
            ("script.groovy", "Groovy"),
            ("Jenkinsfile", "Groovy"),
            ("build.gradle.kts", "Kotlin"),
            ("settings.gradle.kts", "Kotlin"),
            ("Main.kt", "Kotlin"),
            ("libs.versions.toml", "TOML"),
            ("schema.sql", "SQL"),
            ("index.html", "HTML"),
            ("index.htm", "HTML"),
            ("app.css", "CSS"),
            ("app.js", "JavaScript"),
            ("app.mjs", "JavaScript"),
            ("app.cjs", "JavaScript"),
            ("app.ts", "TypeScript"),
            ("view.tsx", "TypeScript"),
            ("README.md", "Markdown"),
            ("guide.markdown", "Markdown"),
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
        assert_eq!(
            language_for(Path::new("BUILD.GRADLE")).map(|config| config.name),
            Some("Groovy")
        );
        assert_eq!(
            language_for(Path::new("JENKINSFILE")).map(|config| config.name),
            Some("Groovy")
        );
        assert_eq!(
            language_for(Path::new("README.MD")).map(|config| config.name),
            Some("Markdown")
        );
        assert_eq!(
            language_for(Path::new("APP.JS")).map(|config| config.name),
            Some("JavaScript")
        );
        assert!(language_for(Path::new("README.txt")).is_none());
        assert!(language_for(Path::new("archive.zip")).is_none());
    }

    #[test]
    fn groovy_build_scripts_are_coloured() {
        let text = "package app\n\
import groovy.transform.CompileStatic\n\
\n\
// a build step\n\
@CompileStatic\n\
class Build {\n\
    String name = \"app\";\n\
\n\
    /* always succeeds */\n\
    def run(int count) {\n\
        if (count > 0) {\n\
            println(\"built\")\n\
        }\n\
        return true\n\
    }\n\
}\n";
        let found = captures(&groovy(), text);
        has(&found, "keyword class");
        has(&found, "keyword def");
        has(&found, "annotation CompileStatic");
        has(&found, "string \"app\"");
        has(&found, "number 0");
        has(&found, "comment // a build step");
        has(&found, "comment /* always succeeds */");
        has(&found, "function run");
        has(&found, "field name");
        has(&found, "keyword true");
    }

    #[test]
    fn kotlin_build_scripts_are_coloured() {
        let text = "package app\n\
\n\
import app.io.File\n\
\n\
// a build step\n\
@Deprecated(\"old\")\n\
class Build {\n\
    val name: String = \"app\"\n\
\n\
    fun run(count: Int): Boolean {\n\
        if (count > 0) {\n\
            println(\"built\")\n\
        }\n\
        return true\n\
    }\n\
}\n";
        let found = captures(&kotlin(), text);
        has(&found, "keyword class");
        has(&found, "keyword fun");
        has(&found, "annotation Deprecated");
        has(&found, "string \"app\"");
        has(&found, "number 0");
        has(&found, "comment // a build step");
        has(&found, "function run");
        has(&found, "field name");
        has(&found, "constant true");
    }

    #[test]
    fn toml_tables_and_values_are_coloured() {
        let text = "# the build file\n\
[package]\n\
name = \"app\"\n\
edition = 2024\n\
open = true\n\
\n\
[features]\n\
items = [\n\
  \"derive\",\n\
  \"std\",\n\
]\n";
        let found = captures(&toml(), text);
        has(&found, "comment # the build file");
        has(&found, "field package");
        has(&found, "field name");
        has(&found, "string \"app\"");
        has(&found, "number 2024");
        has(&found, "constant true");
        let folds = folds_of(&toml(), text);
        assert!(
            folds
                .iter()
                .any(|fold| fold.starts_with('[') && fold.contains("derive")),
            "{folds:?}"
        );
    }

    #[test]
    fn sql_keywords_literals_and_names_are_coloured() {
        let text = "-- build the table\n\
CREATE TABLE app (\n\
    id INT PRIMARY KEY,\n\
    name VARCHAR(64) NOT NULL\n\
);\n\
\n\
INSERT INTO app (name) VALUES ('demo');\n\
\n\
SELECT id, COUNT(id) FROM app WHERE id = 7;\n";
        let found = captures(&sql(), text);
        has(&found, "keyword CREATE");
        has(&found, "keyword TABLE");
        has(&found, "keyword SELECT");
        has(&found, "keyword WHERE");
        has(&found, "keyword NOT");
        has(&found, "comment -- build the table");
        has(&found, "string 'demo'");
        has(&found, "number 7");
        has(&found, "field app");
        has(&found, "field id");
        has(&found, "function COUNT");
    }

    #[test]
    fn html_elements_attributes_and_comments_are_coloured() {
        let text = "<!DOCTYPE html>\n\
<html>\n\
  <head>\n\
    <title>App</title>\n\
  </head>\n\
  <body id=\"main\">\n\
    <!-- a note -->\n\
    <p>Hello</p>\n\
  </body>\n\
</html>\n";
        let found = captures(&html(), text);
        has(&found, "keyword html");
        has(&found, "keyword title");
        has(&found, "field id");
        has(&found, "string main");
        has(&found, "comment <!-- a note -->");
        has(&found, "constant <!DOCTYPE html>");
        let folds = folds_of(&html(), text);
        assert!(
            folds
                .iter()
                .any(|fold| fold.starts_with("<html>") && fold.ends_with("</html>")),
            "{folds:?}"
        );
    }

    #[test]
    fn css_selectors_properties_and_values_are_coloured() {
        let text = "/* app styles */\n\
body {\n\
  margin: 0;\n\
  color: #ff0000;\n\
}\n\
\n\
.card > .title {\n\
  font-size: 12px;\n\
}\n";
        let found = captures(&css(), text);
        has(&found, "comment /* app styles */");
        has(&found, "keyword body");
        has(&found, "field card");
        has(&found, "field title");
        has(&found, "field margin");
        has(&found, "field font-size");
        has(&found, "number 0");
        has(&found, "number 12px");
        has(&found, "keyword px");
        has(&found, "constant #ff0000");
    }

    #[test]
    fn javascript_functions_and_literals_are_coloured() {
        let text = "// build the app\n\
const name = \"app\";\n\
\n\
function run(count) {\n\
  if (count > 0) {\n\
    console.log(name);\n\
  }\n\
  return true;\n\
}\n\
\n\
const build = { name: \"app\", run };\n";
        let found = captures(&javascript(), text);
        has(&found, "keyword const");
        has(&found, "keyword function");
        has(&found, "comment // build the app");
        has(&found, "string \"app\"");
        has(&found, "number 0");
        has(&found, "function run");
        has(&found, "field name");
        has(&found, "constant true");
        let folds = folds_of(&javascript(), text);
        assert!(
            folds
                .iter()
                .any(|fold| fold.starts_with('{') && fold.contains("return true")),
            "{folds:?}"
        );
    }

    #[test]
    fn typescript_types_and_functions_are_coloured() {
        let text = "// typed build\n\
interface Build {\n\
  name: string;\n\
  run(count: number): boolean;\n\
}\n\
\n\
type Step = \"fast\" | \"test\";\n\
\n\
function run(count: number): boolean {\n\
  const name: string = \"app\";\n\
  return true;\n\
}\n";
        let found = captures(&typescript(), text);
        has(&found, "keyword interface");
        has(&found, "keyword type");
        has(&found, "keyword string");
        has(&found, "keyword Build");
        has(&found, "keyword Step");
        has(&found, "comment // typed build");
        has(&found, "string \"app\"");
        has(&found, "function run");
        has(&found, "field name");
        has(&found, "constant true");
    }

    #[test]
    fn tsx_parses_jsx_without_errors() {
        let text = "const App = () => <div className=\"app\">Hello</div>;\n";
        let found = captures(&tsx(), text);
        has(&found, "keyword const");
        has(&found, "string \"app\"");
    }

    #[test]
    fn markdown_headings_and_code_blocks_are_coloured() {
        let text = "# Build\n\
\n\
Some text.\n\
\n\
```groovy\n\
def build() {\n\
    println(\"ok\")\n\
}\n\
```\n\
\n\
## Steps\n\
\n\
- one\n\
- two\n";
        let found = captures(&markdown(), text);
        has(&found, "keyword Build");
        has(&found, "keyword Steps");
        has(&found, "annotation groovy");
        has(&found, "keyword ##");
        assert!(
            found.iter().any(|item| item.starts_with("keyword -")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|item| item.starts_with("string ") && item.contains("println")),
            "{found:?}"
        );
        let folds = folds_of(&markdown(), text);
        assert!(
            folds.iter().any(|fold| fold.contains("println")),
            "{folds:?}"
        );
    }

    #[test]
    fn gradle_and_web_comment_marks_follow_the_format() {
        for config in [groovy(), kotlin(), javascript(), typescript(), tsx()] {
            assert_eq!(config.line_comment, Some("//"), "{}", config.name);
            assert_eq!(config.block_comment, Some(("/*", "*/")), "{}", config.name);
        }
        assert_eq!(css().line_comment, None);
        assert_eq!(css().block_comment, Some(("/*", "*/")));
        assert_eq!(toml().line_comment, Some("#"));
        assert_eq!(toml().block_comment, None);
        assert_eq!(sql().line_comment, Some("--"));
        assert_eq!(sql().block_comment, None);
        assert_eq!(html().line_comment, None);
        assert_eq!(html().block_comment, Some(("<!--", "-->")));
        assert_eq!(markdown().line_comment, None);
        assert_eq!(markdown().block_comment, Some(("<!--", "-->")));
    }

    /// A syntax thread with one document, and the snapshots of its parses.
    fn syntax(config: LanguageConfig, text: &str) -> (SyntaxService, Receiver<SyntaxSnapshot>) {
        let service = SyntaxService::new();
        let (sender, receiver) = channel();
        service.open(DocumentId(1), config, move |snapshot| {
            sender.send(snapshot).ok();
        });
        service.update(
            DocumentId(1),
            1,
            RopeStore::from_text(text),
            &Change::default(),
        );
        (service, receiver)
    }

    fn snapshot(receiver: &Receiver<SyntaxSnapshot>) -> SyntaxSnapshot {
        receiver.recv_timeout(Duration::from_secs(10)).unwrap()
    }

    /// The text of the spans of `kind`.
    fn texts(spans: &[HighlightSpan], text: &str, kind: HighlightKind) -> Vec<String> {
        spans
            .iter()
            .filter(|span| span.kind == kind)
            .map(|span| text[span.start as usize..span.end as usize].to_owned())
            .collect()
    }

    fn coloured(spans: &[HighlightSpan], text: &str, kind: HighlightKind, wanted: &str) {
        let found = texts(spans, text, kind);
        assert!(
            found.iter().any(|item| item == wanted),
            "{wanted} as {kind:?} in {found:?}"
        );
    }

    #[test]
    fn markdown_inline_markup_is_coloured() {
        let text = "**b** *i* `c` [t](u) ~~s~~\n";
        let (service, receiver) = syntax(markdown(), text);
        let snapshot = snapshot(&receiver);
        drop(service);
        let spans = &snapshot.highlights;
        coloured(spans, text, HighlightKind::Strong, "b");
        coloured(spans, text, HighlightKind::Emphasis, "i");
        coloured(spans, text, HighlightKind::InlineCode, "c");
        coloured(spans, text, HighlightKind::Link, "t");
        coloured(spans, text, HighlightKind::LinkUrl, "u");
        coloured(spans, text, HighlightKind::Strikethrough, "s");
        assert!(
            texts(spans, text, HighlightKind::Marker).contains(&"*".to_owned()),
            "the emphasis marks are muted: {:?}",
            texts(spans, text, HighlightKind::Marker)
        );
        assert!(
            texts(spans, text, HighlightKind::Marker).contains(&"[".to_owned()),
            "the link marks are muted: {:?}",
            texts(spans, text, HighlightKind::Marker)
        );
    }

    #[test]
    fn markdown_fenced_code_takes_the_language_of_its_info_string() {
        let text = "```json\n{\"name\": \"app\", \"port\": 8080}\n```\n\n```nope\nplain\n```\n";
        let (service, receiver) = syntax(markdown(), text);
        let snapshot = snapshot(&receiver);
        drop(service);
        let spans = &snapshot.highlights;
        coloured(spans, text, HighlightKind::Field, "\"name\"");
        coloured(spans, text, HighlightKind::String, "\"app\"");
        coloured(spans, text, HighlightKind::Number, "8080");
        let strings = texts(spans, text, HighlightKind::String);
        assert!(
            !strings.iter().any(|item| item.contains("port")),
            "a known language replaces the plain string of its block: {strings:?}"
        );
        assert!(
            strings.iter().any(|item| item.trim() == "plain"),
            "an unknown language stays a plain block: {strings:?}"
        );
    }

    #[test]
    fn markdown_markup_inside_a_block_quote_skips_the_quote_marks() {
        let text = "> first line\n> second **bold** line\n\n> ```json\n> {\"k\": 1}\n> ```\n";
        let (service, receiver) = syntax(markdown(), text);
        let snapshot = snapshot(&receiver);
        drop(service);
        let spans = &snapshot.highlights;
        coloured(spans, text, HighlightKind::Strong, "bold");
        coloured(spans, text, HighlightKind::Field, "\"k\"");
        coloured(spans, text, HighlightKind::Number, "1");
    }

    #[test]
    fn an_edit_inside_an_inline_range_updates_its_spans() {
        let text = "Some **bold** words\n";
        let (service, receiver) = syntax(markdown(), text);
        let first = snapshot(&receiver);
        assert!(texts(&first.highlights, text, HighlightKind::Strong).contains(&"bold".to_owned()));

        let edited = "Some **brave** words\n";
        service.update(
            DocumentId(1),
            2,
            RopeStore::from_text(edited),
            &Change {
                steps: vec![vec![Edit::new(7..11, "brave")]],
            },
        );
        let second = snapshot(&receiver);
        assert!(
            texts(&second.highlights, edited, HighlightKind::Strong).contains(&"brave".to_owned()),
            "{:?}",
            second.highlights
        );
        assert!(
            !texts(&second.highlights, edited, HighlightKind::Strong).contains(&"bold".to_owned())
        );
    }

    #[test]
    fn source_paths_choose_the_language() {
        for (name, expected) in [
            ("script.py", "Python"),
            ("types.pyi", "Python"),
            ("main.c", "C"),
            ("main.h", "C"),
            ("main.cpp", "C++"),
            ("main.cc", "C++"),
            ("main.cxx", "C++"),
            ("main.hpp", "C++"),
            ("main.hh", "C++"),
            ("main.hxx", "C++"),
            ("App.cs", "C#"),
            ("main.go", "Go"),
            ("main.rs", "Rust"),
            ("Main.scala", "Scala"),
            ("Main.sc", "Scala"),
            ("build.sbt", "Scala"),
            ("index.php", "PHP"),
            ("app.rb", "Ruby"),
            ("main.swift", "Swift"),
            ("init.lua", "Lua"),
            ("run.ps1", "PowerShell"),
            ("run.psm1", "PowerShell"),
            ("app.proto", "Protobuf"),
            ("settings.ini", "INI"),
            ("app.cfg", "INI"),
            ("app.editorconfig", "INI"),
            ("change.diff", "Diff"),
            ("change.patch", "Diff"),
            ("build.mk", "Makefile"),
            ("Makefile", "Makefile"),
            ("Gemfile", "Ruby"),
            ("Rakefile", "Ruby"),
            (".editorconfig", "INI"),
        ] {
            let config = language_for(Path::new(name)).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(config.name, expected, "{name}");
        }
        for (name, expected) in [
            ("MAIN.PY", "Python"),
            ("MAIN.C", "C"),
            ("MAIN.CPP", "C++"),
            ("APP.CS", "C#"),
            ("MAIN.GO", "Go"),
            ("MAIN.RS", "Rust"),
            ("MAIN.SCALA", "Scala"),
            ("INDEX.PHP", "PHP"),
            ("APP.RB", "Ruby"),
            ("MAIN.SWIFT", "Swift"),
            ("INIT.LUA", "Lua"),
            ("RUN.PS1", "PowerShell"),
            ("APP.PROTO", "Protobuf"),
            ("APP.INI", "INI"),
            ("CHANGE.DIFF", "Diff"),
            ("BUILD.MK", "Makefile"),
            ("MAKEFILE", "Makefile"),
            ("GEMFILE", "Ruby"),
            ("RAKEFILE", "Ruby"),
        ] {
            let config = language_for(Path::new(name)).unwrap_or_else(|| panic!("{name}"));
            assert_eq!(config.name, expected, "{name}");
        }
        for name in ["python", "c++", "c#", "cpp", "cs", "go", "rust", "scala"] {
            assert!(
                language_by_name(name).is_some(),
                "the fence language {name}"
            );
        }
    }

    #[test]
    fn python_source_is_coloured() {
        let text = "# a helper\n\
class Build:\n\
    count = 42\n\
\n\
    @staticmethod\n\
    def run(count: int) -> bool:\n\
        print(\"app\")\n\
        return True\n";
        let found = captures(&python(), text);
        has(&found, "keyword class");
        has(&found, "keyword return");
        has(&found, "keyword Build");
        has(&found, "annotation staticmethod");
        has(&found, "function run");
        has(&found, "function print");
        has(&found, "field count");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment # a helper");
        has(&found, "constant True");
    }

    #[test]
    fn c_source_is_coloured() {
        let text = "#include <stdio.h>\n\
\n\
// a helper\n\
struct Build {\n\
    int count;\n\
};\n\
\n\
int main(void) {\n\
    struct Build build = {42};\n\
    printf(\"app\");\n\
    return 0;\n\
}\n";
        let found = captures(&c(), text);
        has(&found, "keyword #include");
        has(&found, "keyword int");
        has(&found, "keyword struct");
        has(&found, "keyword Build");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment // a helper");
        has(&found, "function main");
        has(&found, "function printf");
    }

    #[test]
    fn cpp_source_is_coloured() {
        let text = "#include <iostream>\n\
\n\
// a helper\n\
class Build {\n\
public:\n\
    int count = 42;\n\
    void run() {\n\
        std::cout << \"app\";\n\
    }\n\
};\n\
\n\
int main() {\n\
    Build build;\n\
    build.run();\n\
    return 0;\n\
}\n";
        let found = captures(&cpp(), text);
        has(&found, "keyword #include");
        has(&found, "keyword class");
        has(&found, "keyword public");
        has(&found, "keyword Build");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment // a helper");
        has(&found, "function run");
        has(&found, "function main");
    }

    #[test]
    fn csharp_source_is_coloured() {
        let text = "// a helper\n\
namespace App {\n\
    class Build {\n\
        public int Count = 42;\n\
\n\
        public void Run() {\n\
            Console.WriteLine(\"app\");\n\
        }\n\
    }\n\
}\n";
        let found = captures(&csharp(), text);
        has(&found, "keyword namespace");
        has(&found, "keyword class");
        has(&found, "keyword public");
        has(&found, "keyword Build");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment // a helper");
        has(&found, "function Run");
        has(&found, "function WriteLine");
    }

    #[test]
    fn go_source_is_coloured() {
        let text = "package app\n\
\n\
// a helper\n\
type Build struct {\n\
    Count int\n\
}\n\
\n\
func run(count int) int {\n\
    return count + 42\n\
}\n\
\n\
func main() {\n\
    run(1)\n\
    println(\"app\")\n\
}\n";
        let found = captures(&go(), text);
        has(&found, "keyword package");
        has(&found, "keyword func");
        has(&found, "keyword Build");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment // a helper");
        has(&found, "function run");
        has(&found, "function main");
        has(&found, "function println");
    }

    #[test]
    fn rust_source_is_coloured() {
        let text = "// a helper\n\
struct Build {\n\
    count: u32,\n\
}\n\
\n\
fn run(count: u32) -> u32 {\n\
    let name = \"app\";\n\
    println!(\"{}\", 42);\n\
    count\n\
}\n\
\n\
fn main() {\n\
    let build = Build { count: 1 };\n\
    run(build.count);\n\
}\n";
        let found = captures(&rust(), text);
        has(&found, "keyword struct");
        has(&found, "keyword fn");
        has(&found, "keyword let");
        has(&found, "keyword Build");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment // a helper");
        has(&found, "function run");
        has(&found, "function main");
        has(&found, "field count");
    }

    #[test]
    fn scala_source_is_coloured() {
        let text = "// a helper\n\
class Build {\n\
  val count: Int = 42\n\
\n\
  def run(name: String): Boolean = {\n\
    println(\"app\")\n\
    true\n\
  }\n\
}\n";
        let found = captures(&scala(), text);
        has(&found, "keyword class");
        has(&found, "keyword def");
        has(&found, "keyword Build");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment // a helper");
        has(&found, "function run");
        has(&found, "function println");
        has(&found, "constant true");
    }

    #[test]
    fn php_source_is_coloured() {
        let text = "<?php\n\
// a helper\n\
class Build {\n\
    public int $count = 42;\n\
\n\
    public function run(): void {\n\
        echo \"app\";\n\
    }\n\
}\n\
\n\
$build = new Build();\n\
$build->run();\n";
        let found = captures(&php(), text);
        has(&found, "keyword class");
        has(&found, "keyword public");
        has(&found, "keyword function");
        has(&found, "keyword Build");
        has(&found, "keyword int");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment // a helper");
        has(&found, "function run");
    }

    #[test]
    fn ruby_source_is_coloured() {
        let text = "# a helper\n\
class Build\n\
  def run(count)\n\
    name = \"app\"\n\
    puts name\n\
    42\n\
  end\n\
end\n\
\n\
Build.new.run(1)\n";
        let found = captures(&ruby(), text);
        has(&found, "keyword class");
        has(&found, "keyword def");
        has(&found, "keyword Build");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment # a helper");
        has(&found, "function run");
        has(&found, "function puts");
    }

    #[test]
    fn swift_source_is_coloured() {
        let text = "// a helper\n\
class Build {\n\
    let count = 42\n\
\n\
    func run() -> String {\n\
        return \"app\"\n\
    }\n\
}\n\
\n\
let build = Build()\n\
build.run()\n";
        let found = captures(&swift(), text);
        has(&found, "keyword class");
        has(&found, "keyword func");
        has(&found, "keyword Build");
        has(&found, "string app");
        has(&found, "number 42");
        has(&found, "comment // a helper");
        has(&found, "function run");
    }

    #[test]
    fn lua_source_is_coloured() {
        let text = "-- a helper\n\
local function run(count)\n\
    local name = \"app\"\n\
    print(name, 42)\n\
end\n\
\n\
run(1)\n";
        let found = captures(&lua(), text);
        has(&found, "keyword local");
        has(&found, "keyword function");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment -- a helper");
        has(&found, "function run");
    }

    #[test]
    fn powershell_script_is_coloured() {
        let text = "# a helper\n\
function Run-Build {\n\
    param([int]$count = 42)\n\
    Write-Output \"app\"\n\
}\n\
\n\
Run-Build -count 1\n";
        let found = captures(&powershell(), text);
        has(&found, "keyword function");
        has(&found, "keyword param");
        has(&found, "string \"app\"");
        has(&found, "number 42");
        has(&found, "comment # a helper");
        has(&found, "function Run-Build");
        has(&found, "function Write-Output");
    }

    #[test]
    fn protobuf_schema_is_coloured() {
        let text = "// a helper\n\
syntax = \"proto3\";\n\
\n\
message Build {\n\
  int32 count = 1;\n\
  string name = 2;\n\
}\n\
\n\
service Builder {\n\
  rpc Run (Build) returns (Build);\n\
}\n";
        let found = captures(&protobuf(), text);
        has(&found, "keyword message");
        has(&found, "keyword service");
        has(&found, "keyword Build");
        has(&found, "string \"proto3\"");
        has(&found, "number 1");
        has(&found, "comment // a helper");
        has(&found, "function Run");
        has(&found, "field count");
    }

    #[test]
    fn ini_settings_are_coloured() {
        let text = "# a helper\n\
[build]\n\
name=app\n\
count=42\n";
        let found = captures(&ini(), text);
        has(&found, "keyword build");
        has(&found, "field name");
        has(&found, "string app");
        has(&found, "string 42");
        assert!(
            found
                .iter()
                .any(|item| item.starts_with("comment # a helper")),
            "{found:?}"
        );
    }

    #[test]
    fn diff_changes_are_coloured() {
        let text = "# a helper\n\
diff --git a/app.py b/app.py\n\
index 1234567..89abcde 100644\n\
--- a/app.py\n\
+++ b/app.py\n\
@@ -1,3 +1,4 @@\n\
-old line\n\
+new line\n";
        let found = captures(&diff(), text);
        has(&found, "keyword -old line");
        has(&found, "string +new line");
        has(&found, "constant 1234567");
        has(&found, "annotation @@ -1,3 +1,4 @@");
        has(&found, "number -1,3");
        has(&found, "comment # a helper");
    }

    #[test]
    fn makefile_rules_are_coloured() {
        let text = "# a helper\n\
NAME := $(shell echo app)\n\
FLAGS := -O2\n\
\n\
all: build\n\
\n\
build:\n\
\techo \"app\"\n";
        let found = captures(&make(), text);
        has(&found, "field NAME");
        has(&found, "field FLAGS");
        has(&found, "function shell");
        has(&found, "constant build");
        has(&found, "comment # a helper");
    }

    #[test]
    fn source_blocks_fold() {
        let python = folds_of(&python(), "def run():\n    print(\"app\")\n");
        assert!(
            python.iter().any(|fold| fold.contains("print")),
            "{python:?}"
        );

        let cpp = folds_of(&cpp(), "int main() {\n    return 0;\n}\n");
        assert!(
            cpp.iter()
                .any(|fold| fold.starts_with('{') && fold.ends_with('}')),
            "{cpp:?}"
        );

        let rust = folds_of(&rust(), "fn main() {\n    let a = 1;\n}\n");
        assert!(
            rust.iter()
                .any(|fold| fold.starts_with('{') && fold.ends_with('}')),
            "{rust:?}"
        );

        let scala = folds_of(&scala(), "class Build {\n  val a = 1\n}\n");
        assert!(
            scala
                .iter()
                .any(|fold| fold.starts_with('{') && fold.ends_with('}')),
            "{scala:?}"
        );
    }

    #[test]
    fn a_markdown_fence_of_python_takes_the_python_grammar() {
        let text = "```python\ndef run(count):\n    # a helper\n    print(\"app\", 42)\n```\n";
        let (service, receiver) = syntax(markdown(), text);
        let snapshot = snapshot(&receiver);
        drop(service);
        let spans = &snapshot.highlights;
        coloured(spans, text, HighlightKind::Keyword, "def");
        coloured(spans, text, HighlightKind::Comment, "# a helper");
        coloured(spans, text, HighlightKind::String, "\"app\"");
        coloured(spans, text, HighlightKind::Number, "42");
        coloured(spans, text, HighlightKind::Function, "print");
    }
}

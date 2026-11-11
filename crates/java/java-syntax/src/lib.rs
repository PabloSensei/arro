//! Grammar and queries of the language.

mod edit;

use std::sync::OnceLock;

pub use edit::{
    Deletion, DeletionKind, UnwrapEdit, deletion, enclosing_type_at, first_type_name,
    first_type_offset, method_at, testable_methods, type_declares_method, unwrap_edits,
};

use syntax::{FoldStyle, LanguageConfig, StatementRules, TagSymbols};

const FOLD_STYLES: &[FoldStyle] = &[
    FoldStyle {
        capture: "fold.imports",
        placeholder: "...",
        keep: "import ".len(),
        joined: true,
        collapsed_on_open: true,
    },
    FoldStyle {
        capture: "fold.doc",
        placeholder: "/** ... */",
        keep: 0,
        joined: false,
        collapsed_on_open: false,
    },
    FoldStyle {
        capture: "fold.comment",
        placeholder: "/* ... */",
        keep: 0,
        joined: false,
        collapsed_on_open: false,
    },
    FoldStyle {
        capture: "fold",
        placeholder: "{...}",
        keep: 0,
        joined: false,
        collapsed_on_open: false,
    },
];

const STATEMENTS: StatementRules = StatementRules {
    terminator: ";",
    block_words: &[
        "if",
        "else",
        "for",
        "while",
        "do",
        "switch",
        "try",
        "catch",
        "finally",
        "synchronized",
        "class",
        "interface",
        "enum",
        "record",
    ],
};

pub fn config() -> LanguageConfig {
    LanguageConfig {
        name: "Java",
        grammar: tree_sitter_java_orchard::LANGUAGE.into(),
        highlights: include_str!("../../../../queries/java/highlights.scm"),
        folds: include_str!("../../../../queries/java/folds.scm"),
        indents: include_str!("../../../../queries/java/indents.scm"),
        fold_styles: FOLD_STYLES,
        line_comment: Some("//"),
        block_comment: Some(("/*", "*/")),
        statements: Some(STATEMENTS),
        indent_unit: "    ",
        file_mask: "*.java !*Test.java",
    }
}

/// Declarations of a source file, for Go to Class and the file structure.
pub fn symbols() -> &'static TagSymbols {
    static SYMBOLS: OnceLock<TagSymbols> = OnceLock::new();
    SYMBOLS.get_or_init(|| {
        let query = include_str!("../../../../queries/java/symbols.scm");
        TagSymbols::new(tree_sitter_java_orchard::LANGUAGE.into(), query)
            .expect("the symbols query fits the grammar")
    })
}

//! Live templates, postfix templates and Surround With of the language, as data for the
//! neutral `templates` crate. Bodies follow IntelliJ IDEA's default Java set.

use std::path::Path;
use std::sync::OnceLock;

use templates::{ANY, Kind, Template, TemplateLanguage};

use crate::is_source;

const STATEMENT: &[&str] = &["statement"];
const DECLARATION: &[&str] = &["declaration"];

/// `statement` inside method bodies, `declaration` in class bodies, `expression` inside
/// arguments and parentheses.
const CONTEXTS: &str = r#"
(class_body) @declaration
(interface_body) @declaration
(enum_body_declarations) @declaration
(annotation_type_body) @declaration
(block) @statement
(constructor_body) @statement
(switch_block_statement_group) @statement
(argument_list) @expression
(parenthesized_expression) @expression
"#;

const fn abbreviation(
    key: &'static str,
    description: &'static str,
    body: &'static str,
    fields: &'static [(&'static str, &'static str)],
    contexts: &'static [&'static str],
) -> Template {
    Template {
        key,
        description,
        kind: Kind::Abbreviation,
        body,
        fields,
        contexts,
    }
}

const fn postfix(
    key: &'static str,
    description: &'static str,
    body: &'static str,
    fields: &'static [(&'static str, &'static str)],
) -> Template {
    Template {
        key,
        description,
        kind: Kind::Postfix,
        body,
        fields,
        contexts: STATEMENT,
    }
}

const fn surround(
    key: &'static str,
    description: &'static str,
    body: &'static str,
    fields: &'static [(&'static str, &'static str)],
) -> Template {
    Template {
        key,
        description,
        kind: Kind::Surround,
        body,
        fields,
        contexts: STATEMENT,
    }
}

/// A template of the Surround with Live Template list, not of Surround With.
const fn surround_live(
    key: &'static str,
    description: &'static str,
    body: &'static str,
    fields: &'static [(&'static str, &'static str)],
) -> Template {
    Template {
        key,
        description,
        kind: Kind::SurroundLive,
        body,
        fields,
        contexts: STATEMENT,
    }
}

const MAIN: &str = "public static void main(String[] args) {\n\t$END$\n}";
const FORI: &str = "for (int $INDEX$ = 0; $INDEX$ < $LIMIT$; $INDEX$++) {\n\t$END$\n}";

const TEMPLATES: &[Template] = &[
    abbreviation(
        "sout",
        "Prints a string to System.out",
        "System.out.println($END$);",
        &[],
        STATEMENT,
    ),
    abbreviation(
        "soutv",
        "Prints a value to System.out",
        "System.out.println(\"$VALUE$ = \" + $VALUE$);$END$",
        &[("VALUE", "value")],
        STATEMENT,
    ),
    abbreviation(
        "souf",
        "Prints a formatted string to System.out",
        "System.out.printf(\"$END$\");",
        &[],
        STATEMENT,
    ),
    abbreviation("psvm", "main() method declaration", MAIN, &[], DECLARATION),
    abbreviation("main", "main() method declaration", MAIN, &[], DECLARATION),
    abbreviation(
        "fori",
        "Creates an iteration loop",
        FORI,
        &[("INDEX", "i"), ("LIMIT", "")],
        STATEMENT,
    ),
    abbreviation(
        "iter",
        "Iterates over an Iterable or an array",
        "for ($TYPE$ $VAR$ : $ITERABLE$) {\n\t$END$\n}",
        &[("ITERABLE", ""), ("TYPE", "var"), ("VAR", "item")],
        STATEMENT,
    ),
    abbreviation(
        "itar",
        "Iterates over the elements of an array",
        "for (int $INDEX$ = 0; $INDEX$ < $ARRAY$.length; $INDEX$++) {\n\t$TYPE$ $VAR$ = $ARRAY$[$INDEX$];\n\t$END$\n}",
        &[
            ("ARRAY", "array"),
            ("INDEX", "i"),
            ("TYPE", "var"),
            ("VAR", "item"),
        ],
        STATEMENT,
    ),
    abbreviation(
        "ifn",
        "Inserts an 'if null' statement",
        "if ($VAR$ == null) {\n\t$END$\n}",
        &[("VAR", "")],
        STATEMENT,
    ),
    abbreviation(
        "inn",
        "Inserts an 'if not null' statement",
        "if ($VAR$ != null) {\n\t$END$\n}",
        &[("VAR", "")],
        STATEMENT,
    ),
    abbreviation("thr", "throw new", "throw new $END$", &[], STATEMENT),
    abbreviation("St", "String", "String $END$", &[], &[ANY]),
    abbreviation(
        "prsf",
        "private static final",
        "private static final $END$",
        &[],
        DECLARATION,
    ),
    abbreviation(
        "psf",
        "public static final",
        "public static final $END$",
        &[],
        DECLARATION,
    ),
    postfix(
        "var",
        "Introduces a local variable",
        "var $NAME$ = $EXPR$;$END$",
        &[("NAME", "value")],
    ),
    postfix(
        "for",
        "Iterates over an Iterable or an array",
        "for (var $VAR$ : $EXPR$) {\n\t$END$\n}",
        &[("VAR", "item")],
    ),
    postfix(
        "fori",
        "Iterates up to a number",
        "for (int $INDEX$ = 0; $INDEX$ < $EXPR$; $INDEX$++) {\n\t$END$\n}",
        &[("INDEX", "i")],
    ),
    postfix("if", "Checks a condition", "if ($EXPR$) {\n\t$END$\n}", &[]),
    postfix(
        "null",
        "Checks that the expression is null",
        "if ($EXPR$ == null) {\n\t$END$\n}",
        &[],
    ),
    postfix(
        "nn",
        "Checks that the expression is not null",
        "if ($EXPR$ != null) {\n\t$END$\n}",
        &[],
    ),
    postfix("not", "Negates the expression", "!$EXPR$$END$", &[]),
    postfix("return", "Returns the value", "return $EXPR$;$END$", &[]),
    postfix(
        "sout",
        "Prints the value",
        "System.out.println($EXPR$);$END$",
        &[],
    ),
    surround("if", "if", "if ($CONDITION$) {\n\t$SELECTION$\n}$END$", &[]),
    surround(
        "try",
        "try / catch",
        "try {\n\t$SELECTION$\n} catch ($EXCEPTION$ e) {\n\tthrow new RuntimeException(e);$END$\n}",
        &[("EXCEPTION", "Exception")],
    ),
    surround(
        "for",
        "for",
        "for (int $INDEX$ = 0; $INDEX$ < $LIMIT$; $INDEX$++) {\n\t$SELECTION$\n}$END$",
        &[("INDEX", "i"), ("LIMIT", "")],
    ),
    surround(
        "while",
        "while",
        "while ($CONDITION$) {\n\t$SELECTION$\n}$END$",
        &[],
    ),
    surround(
        "synchronized",
        "synchronized",
        "synchronized ($LOCK$) {\n\t$SELECTION$\n}$END$",
        &[("LOCK", "this")],
    ),
    surround_live(
        "C",
        "Callable",
        "new java.util.concurrent.Callable<$TYPE$>() {\n\t@Override\n\tpublic $TYPE$ call() throws Exception {\n\t\t$SELECTION$\n\t\treturn $END$;\n\t}\n}",
        &[("TYPE", "Object")],
    ),
    surround_live(
        "RL",
        "readLock / unlock",
        "$LOCK$.readLock().lock();\ntry {\n\t$SELECTION$\n} finally {\n\t$LOCK$.readLock().unlock();\n}$END$",
        &[("LOCK", "lock")],
    ),
    surround_live(
        "WL",
        "writeLock / unlock",
        "$LOCK$.writeLock().lock();\ntry {\n\t$SELECTION$\n} finally {\n\t$LOCK$.writeLock().unlock();\n}$END$",
        &[("LOCK", "lock")],
    ),
];

fn language() -> &'static TemplateLanguage {
    static LANGUAGE: OnceLock<TemplateLanguage> = OnceLock::new();
    LANGUAGE.get_or_init(|| TemplateLanguage {
        templates: TEMPLATES,
        contexts: CONTEXTS,
        grammar: || tree_sitter_java_orchard::LANGUAGE.into(),
        indent_unit: "    ",
        receiver_prefixes: &["new"],
        query: OnceLock::new(),
    })
}

/// The templates for `path`, for the editor.
pub fn templates_for(path: &Path) -> Option<&'static TemplateLanguage> {
    is_source(path).then(language)
}

#[cfg(test)]
mod tests {
    use templates::{context_at, expand};
    use tree_sitter::Parser;

    use super::*;

    fn context(source: &str, marker: &str) -> &'static str {
        let mut parser = Parser::new();
        parser
            .set_language(&tree_sitter_java_orchard::LANGUAGE.into())
            .unwrap();
        let tree = parser.parse(source, None).unwrap();
        let offset = source.find(marker).unwrap() + marker.len();
        context_at(language(), Some(&tree), source.as_bytes(), offset)
    }

    #[test]
    fn the_context_query_compiles_and_tells_bodies_apart() {
        assert!(language().query().is_some());
        let source =
            "class A {\n    psvm\n    void f() {\n        sout\n        g(St);\n    }\n}\n";
        assert_eq!(context(source, "psvm"), "declaration");
        assert_eq!(context(source, "sout"), "statement");
        assert_eq!(context(source, "g(St"), "expression");
        assert_eq!(context("psvm", "psvm"), "other");
    }

    #[test]
    fn every_template_fits_somewhere_and_expands() {
        let java = language();
        assert!(
            java.find(Kind::Abbreviation, "psvm", "declaration")
                .is_some()
        );
        assert!(java.find(Kind::Abbreviation, "psvm", "statement").is_none());
        assert!(java.find(Kind::Abbreviation, "St", "expression").is_some());
        for template in TEMPLATES {
            let expansion = expand(template, "", "    ", Some("a();"), Some("x"));
            assert!(!expansion.text.contains('$'), "{}", template.key);
        }
        let sout = java.find(Kind::Abbreviation, "sout", "statement").unwrap();
        let expansion = expand(sout, "", "    ", None, None);
        assert_eq!(&expansion.text[..expansion.end], "System.out.println(");
    }
}

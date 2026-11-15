//! What the spelling check reads in a source file: the comments, the strings and the
//! names of the declarations. The scan is native, so the check needs no server.

use lang_api::{LanguageSupport, SpellPlace, SpellQuery, SpellingProvider};

use crate::JavaProject;

impl SpellingProvider for JavaProject {
    fn places(&self, query: &SpellQuery) -> Option<Vec<SpellPlace>> {
        if !LanguageSupport::matches(self, &query.path) {
            return None;
        }
        Some(
            java_syntax::spelling_places(&query.text)
                .into_iter()
                .map(|place| SpellPlace {
                    start: place.range.start,
                    end: place.range.end,
                    declaration: place.declaration,
                })
                .collect(),
        )
    }
}

#[cfg(test)]
mod tests {
    use spelling::{Region, Speller};

    const SOURCE: &str = r#"package com.example;

import com.example.util.Server;

// The calcualte of the total.
class Calculator {
    private Server calcualteServer;

    void calcualteTotal(Server server) {
        String note = "the calcualte of the total";
        calcualteTotal(server);
    }
}
"#;

    fn speller() -> Speller {
        Speller::with_words(
            [
                "calculate",
                "calculator",
                "total",
                "server",
                "string",
                "note",
                "the",
                "of",
                "package",
                "com",
                "example",
                "util",
                "private",
                "void",
                "class",
            ]
            .into_iter()
            .map(str::to_owned),
        )
    }

    fn check(source: &str) -> Vec<spelling::Misspelling> {
        let found = java_syntax::spelling_places(source);
        let regions: Vec<Region> = found
            .into_iter()
            .map(|place| Region {
                range: place.range,
                declaration: place.declaration,
            })
            .collect();
        speller().check(source, &regions)
    }

    #[test]
    fn the_check_reads_comments_strings_and_declaration_names() {
        let found = check(SOURCE);
        let words: Vec<&str> = found.iter().map(|miss| miss.word.as_str()).collect();
        assert!(
            words.iter().filter(|word| **word == "calcualte").count() >= 3,
            "the comment, the string and the names are read: {words:?}"
        );
        assert!(
            found.iter().any(|miss| miss.declaration),
            "a name of a declaration is a place of its own"
        );
    }

    #[test]
    fn a_use_a_keyword_and_an_import_are_no_places() {
        let found = check(SOURCE);
        let used = SOURCE.rfind("calcualteTotal").unwrap();
        assert!(
            found
                .iter()
                .all(|miss| !(miss.range.start <= used && used < miss.range.end)),
            "the call of the method is no place"
        );
        for keyword in ["package", "import", "private", "void"] {
            let at = SOURCE.find(keyword).unwrap();
            assert!(
                found
                    .iter()
                    .all(|miss| !(miss.range.start <= at && at < miss.range.end)),
                "the keyword {keyword} is no place"
            );
        }
        let import = SOURCE.find("com.example.util.Server").unwrap();
        assert!(
            found
                .iter()
                .all(|miss| !(miss.range.start <= import && import < miss.range.end)),
            "an import is no place"
        );
    }
}

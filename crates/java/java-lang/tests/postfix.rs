//! Postfix templates end to end on the editor state: what each key expands to, where the
//! caret goes, and which expression the expansion follows.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use editor_view::{EditorState, KeyUse, LiveTemplates};
use keymap::{Keystroke, editor as actions};
use syntax::{DocumentId, SyntaxService, SyntaxSnapshot};
use text_core::{Change, FileFormat, TextStore};

const SIGNATURE: &str = "    void run(String[] args, java.util.List<String> names, int count, boolean flag, String title) {\n";
const CLASS: &str = "class Owner {\n    void run(String[] args, java.util.List<String> names, int count, boolean flag, String title) {\n        \n    }\n}\n";

struct Fixture {
    state: EditorState,
    live: LiveTemplates,
    service: SyntaxService,
    results: mpsc::Receiver<SyntaxSnapshot>,
}

impl Fixture {
    fn new(text: &str) -> Self {
        let path = Path::new("Owner.java");
        let language = java_lang::language_for(path).unwrap();
        let service = SyntaxService::new();
        let (sender, results) = mpsc::channel();
        service.open(DocumentId(1), language.clone(), move |snapshot| {
            let _ = sender.send(snapshot);
        });
        let mut fixture = Self {
            state: EditorState::new(text, FileFormat::default(), Some(language), false),
            live: LiveTemplates::new(java_lang::templates_for(path)),
            service,
            results,
        };
        fixture.parse(&Change::default());
        fixture
    }

    fn parse(&mut self, change: &Change) {
        let version = self.state.version();
        self.service
            .update(DocumentId(1), version, self.state.snapshot(), change);
        let snapshot = self
            .results
            .recv_timeout(Duration::from_secs(10))
            .expect("a parse result");
        assert!(self.state.accept_syntax(snapshot));
    }

    fn changed(&mut self, change: Option<Change>) {
        if let Some(change) = self.live.follow(&mut self.state, change) {
            self.parse(&change);
        }
    }

    fn caret_after(&mut self, marker: &str) {
        let at = self.state.text().find(marker).unwrap() + marker.len();
        self.state.select_range(at..at);
    }

    fn type_text(&mut self, text: &str) {
        let change = self.state.type_text(text);
        self.changed(change);
    }

    fn perform(&mut self, action: &str) {
        let change = match self.live.action(&mut self.state, action) {
            Some(change) => change,
            None => self.state.perform(action, 20),
        };
        self.changed(change);
    }

    fn key(&mut self, key: &str) -> bool {
        let stroke = Keystroke::parse(key).unwrap();
        match self.live.key(&mut self.state, &stroke) {
            KeyUse::Unused => false,
            KeyUse::Used(change) => {
                self.changed(change);
                true
            }
        }
    }

    fn selected(&self) -> String {
        let range = self.state.selections().primary().range();
        self.state.store().slice(range)
    }

    fn postfix(&mut self, expression: &str, key: &str) {
        // The empty line of the method, after its indentation.
        let at = self.state.text().find("        \n").unwrap() + "        ".len();
        self.state.select_range(at..at);
        self.type_text(&format!("{expression}.{key}"));
        self.perform(actions::TAB);
    }
}

/// The whole class with `expansion` on the empty line of the method.
fn expected(expansion: &str) -> String {
    format!("class Owner {{\n{SIGNATURE}        {expansion}\n    }}\n}}\n")
}

fn check(expression: &str, key: &str, expansion: &str, field: &str) {
    let mut editor = Fixture::new(CLASS);
    editor.postfix(expression, key);
    assert_eq!(
        editor.state.text(),
        expected(expansion),
        "{key} after {expression}"
    );
    assert_eq!(editor.selected(), field, "the field of {key}");
}

#[test]
fn every_postfix_template_expands_with_its_fields_and_caret() {
    check("args", "var", "var value = args;", "value");
    check("args", "field", "private Object field = args;", "Object");
    check("flag", "if", "if (flag) {\n            \n        }", "");
    check("flag", "else", "else {\n            \n        }", "");
    check("flag", "not", "!flag", "");
    check(
        "names",
        "null",
        "if (names == null) {\n            \n        }",
        "",
    );
    check(
        "names",
        "notnull",
        "if (names != null) {\n            \n        }",
        "",
    );
    check(
        "names",
        "nn",
        "if (names != null) {\n            \n        }",
        "",
    );
    check(
        "args",
        "for",
        "for (var item : args) {\n            \n        }",
        "item",
    );
    check(
        "count",
        "fori",
        "for (int i = 0; i < count; i++) {\n            \n        }",
        "i",
    );
    check(
        "count",
        "forr",
        "for (int i = count - 1; i >= 0; i--) {\n            \n        }",
        "i",
    );
    check(
        "names",
        "iter",
        "for (var item : names) {\n            \n        }",
        "item",
    );
    check(
        "flag",
        "while",
        "while (flag) {\n            \n        }",
        "",
    );
    check(
        "names",
        "try",
        "try {\n            names;\n        } catch (Exception e) {\n            throw new RuntimeException(e);\n        }",
        "Exception",
    );
    check(
        "names",
        "twr",
        "try (resource = names) {\n            \n        }",
        "resource",
    );
    check("names", "return", "return names;", "");
    check("names", "throw", "throw names;", "");
    check("names", "sout", "System.out.println(names);", "");
    check(
        "names",
        "soutv",
        "System.out.println(\"names = \" + names);",
        "",
    );
    check("names", "serr", "System.err.println(names);", "");
    check("names", "par", "(names)", "");
    check("names", "cast", "((Object) names)", "Object");
    check("names", "new", "new Object(names)", "Object");
    check("flag", "lambda", "() -> flag", "");
    check("names", "opt", "java.util.Optional.ofNullable(names)", "");
    check("names", "stream", "names.stream()", "");
    check(
        "count",
        "switch",
        "switch (count) {\n            case :\n                \n                break;\n        }",
        "",
    );
    check(
        "names",
        "synchronized",
        "synchronized (names) {\n            \n        }",
        "",
    );
    check("title", "format", "String.format(title, )", "");
    check("names", "arg", "names", "");
    check(
        "names",
        "inst",
        "if (names instanceof Object) {\n            \n        }",
        "Object",
    );
}

#[test]
fn the_whole_expression_in_front_of_the_key_is_taken() {
    let mut editor = Fixture::new(CLASS);
    editor.postfix("args.length", "var");
    assert_eq!(editor.state.text(), expected("var value = args.length;"));
    assert_eq!(editor.selected(), "value");
}

#[test]
fn several_expressions_of_known_types_ask_which_one_to_take() {
    let source = "class Owner {\n    java.util.List<String> list;\n    String get(int index) { return null; }\n    void run() {\n        \n    }\n}\n";
    let mut editor = Fixture::new(source);
    editor.postfix("list.get(0)", "var");
    let list = editor.live.list().expect("the expression list");
    assert_eq!(list.expressions.len(), 2, "the call and its argument call");
    assert!(editor.key("enter"));
    assert!(
        editor
            .state
            .text()
            .contains("        var value = list.get(0);\n"),
        "{}",
        editor.state.text()
    );
    assert_eq!(editor.selected(), "value");
}

#[test]
fn a_postfix_key_that_does_not_apply_leaves_the_text_alone() {
    let mut editor = Fixture::new(CLASS);
    editor.postfix("count", "for");
    assert!(
        editor.state.text().contains("count.for"),
        "{}",
        editor.state.text()
    );
    assert!(!editor.live.is_active());
}

#[test]
fn the_completion_list_carries_a_postfix_template_with_its_description() {
    let mut editor = Fixture::new(CLASS);
    editor.caret_after("        \n");
    editor.type_text("        names.");
    let entries = editor.live.postfix_completions(&editor.state);
    let keys: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
    assert!(keys.contains(&"for"), "{keys:?}");
    assert!(keys.contains(&"stream"), "{keys:?}");
    assert!(!keys.contains(&"fori"), "a List is not a number: {keys:?}");
    let stream = entries
        .iter()
        .find(|entry| entry.label == "stream")
        .unwrap();
    assert_eq!(stream.kind, lang_api::CompletionKind::Snippet);
    assert!(stream.snippet);
    assert_eq!(stream.insert_text.as_deref(), Some("names.stream()$0"));
    assert_eq!(
        stream
            .label_details
            .as_ref()
            .and_then(|details| details.description.as_deref()),
        Some("Turns the collection into a stream")
    );
    assert!(
        stream
            .documentation
            .as_deref()
            .is_some_and(|text| text.contains("names.stream()"))
    );
    assert!(stream.span.is_some(), "the entry replaces the expression");
}

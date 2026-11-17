//! Live templates end to end on the editor state, with the real grammar and no window.

use std::path::Path;
use std::sync::mpsc;
use std::time::Duration;

use editor_view::{EditorState, KeyUse, LiveTemplates};
use keymap::{Keystroke, editor as actions};
use syntax::{DocumentId, SyntaxService, SyntaxSnapshot};
use text_core::{Change, FileFormat, TextStore};

const CLASS: &str = "class Owner {\n    void run() {\n        int size = 1;\n\n    }\n\n}\n";

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

    /// Puts the caret right after `marker`.
    fn caret_after(&mut self, marker: &str) {
        let at = self.state.text().find(marker).unwrap() + marker.len();
        self.state.select_range(at..at);
    }

    fn type_text(&mut self, text: &str) {
        let change = self.state.type_text(text);
        self.changed(change);
    }

    /// The editor's way of running an action: templates first.
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

    /// The text with `|` at the caret.
    fn shown(&self) -> String {
        let mut text = self.state.text();
        text.insert(self.state.selections().primary().head, '|');
        text
    }
}

#[test]
fn sout_expands_by_tab_and_undo_takes_back_the_expansion_in_one_step() {
    let mut editor = Fixture::new(CLASS);
    editor.caret_after("int size = 1;\n");
    editor.type_text("        sout");
    editor.perform(actions::TAB);

    assert!(editor.shown().contains("        System.out.println(|);\n"));
    editor.perform(actions::UNDO);
    assert!(
        editor.shown().contains("        sout|\n"),
        "{}",
        editor.shown()
    );
}

#[test]
fn psvm_fits_a_class_body_and_not_a_method() {
    let mut editor = Fixture::new(CLASS);
    editor.caret_after("    }\n");
    editor.type_text("    psvm");
    editor.perform(actions::TAB);
    let expected = "    public static void main(String[] args) {\n        |\n    }\n}\n";
    assert!(editor.shown().ends_with(expected), "{}", editor.shown());

    let mut inside = Fixture::new(CLASS);
    inside.caret_after("int size = 1;");
    inside.type_text(" psvm");
    inside.perform(actions::TAB);
    assert!(inside.state.text().contains("psvm"), "{}", inside.shown());
    assert!(!inside.state.text().contains("main("));
}

#[test]
fn fori_fields_take_tab_and_a_typed_index_fills_its_copies() {
    let mut editor = Fixture::new(CLASS);
    editor.caret_after("int size = 1;\n");
    editor.type_text("        fori");
    editor.perform(actions::TAB);
    assert_eq!(editor.selected(), "i");
    assert!(editor.live.is_active());

    editor.type_text("j");
    assert!(editor.state.text().contains("for (int j = 0; j < ; j++) {"));
    assert!(editor.key("tab"));
    editor.type_text("size");
    assert!(editor.key("shift-tab"));
    assert_eq!(editor.selected(), "j");
    assert!(editor.key("tab"));
    assert!(editor.key("tab"));
    assert!(!editor.live.is_active());
    let expected = "        for (int j = 0; j < size; j++) {\n            |\n        }\n";
    assert!(editor.shown().contains(expected), "{}", editor.shown());
}

#[test]
fn var_postfix_introduces_a_variable_for_the_expression() {
    let mut editor = Fixture::new(CLASS);
    editor.caret_after("int size = 1;\n");
    editor.type_text("        Integer.valueOf(size).var");
    editor.perform(actions::TAB);

    assert!(
        editor
            .state
            .text()
            .contains("        var value = Integer.valueOf(size);\n")
    );
    assert_eq!(editor.selected(), "value");
}

#[test]
fn surround_with_try_wraps_the_selected_line_as_one_step() {
    let mut editor = Fixture::new(CLASS);
    let start = editor.state.text().find("        int size").unwrap();
    let end = start + "        int size = 1;\n".len();
    editor.state.select_range(start..end);
    editor.perform(actions::SURROUND_WITH);
    let keys: Vec<_> = editor
        .live
        .list()
        .unwrap()
        .items
        .iter()
        .map(|it| it.key)
        .collect();
    assert_eq!(keys, ["if", "try", "for", "while", "synchronized"]);

    assert!(editor.key("down"));
    assert!(editor.key("enter"));
    let wrapped = "    void run() {\n        try {\n            int size = 1;\n        } catch (Exception e) {\n            throw new RuntimeException(e);\n        }\n\n    }\n";
    assert!(editor.state.text().contains(wrapped), "{}", editor.shown());
    assert_eq!(editor.selected(), "Exception");

    editor.key("escape");
    editor.perform(actions::UNDO);
    assert_eq!(editor.state.text(), CLASS);
}

#[test]
fn ctrl_j_lists_the_templates_that_fit_the_typed_prefix() {
    let mut editor = Fixture::new(CLASS);
    editor.caret_after("int size = 1;\n");
    editor.type_text("        so");
    editor.perform(actions::INSERT_TEMPLATE);
    let keys: Vec<_> = editor
        .live
        .list()
        .unwrap()
        .items
        .iter()
        .map(|it| it.key)
        .collect();
    assert_eq!(keys, ["sout", "soutv", "souf"]);

    assert!(editor.key("down"));
    assert!(editor.key("enter"));
    assert_eq!(editor.selected(), "value");
    editor.type_text("size");
    let expected = "System.out.println(\"size = \" + size);";
    assert!(editor.state.text().contains(expected), "{}", editor.shown());
}

#[test]
fn a_file_indented_with_tabs_gets_tabs_in_the_expansion() {
    let mut editor = Fixture::new("class Owner {\n\tvoid run() {\n\t\tint size = 1;\n\n\t}\n}\n");
    editor.caret_after("int size = 1;\n");
    editor.type_text("\t\tfori");
    editor.perform(actions::TAB);
    editor.key("escape");

    let expected = "\t\tfor (int i = 0; i < ; i++) {\n\t\t\t\n\t\t}\n";
    assert!(editor.state.text().contains(expected), "{}", editor.shown());
}

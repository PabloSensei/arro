//! The editor commands end to end, without a window.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc;
use std::time::Duration;

use editor_view::EditorState;
use keymap::{Shortcut, default_keymap, editor as actions};
use syntax::{DocumentId, SyntaxService, SyntaxSnapshot};
use text_core::{Change, Edit, FileFormat, TextStore};

const JAVA: &str =
    "import a.B;\nimport a.C;\n\nclass A {\n    void run() {\n        int size = 1;\n    }\n}\n";

fn temp_file(name: &str, bytes: &[u8]) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let folder = std::env::temp_dir().join(format!(
        "editor-view-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

struct Fixture {
    state: EditorState,
    service: SyntaxService,
    results: mpsc::Receiver<SyntaxSnapshot>,
}

impl Fixture {
    fn new(state: EditorState) -> Self {
        let service = SyntaxService::new();
        let (sender, results) = mpsc::channel();
        let language = state.language().cloned().expect("a language");
        service.open(DocumentId(1), language, move |snapshot| {
            let _ = sender.send(snapshot);
        });
        let mut fixture = Self {
            state,
            service,
            results,
        };
        fixture.parse(&Change::default());
        fixture
    }

    fn java(text: &str) -> Self {
        Self::new(EditorState::new(
            text,
            FileFormat::default(),
            Some(java_lang::language_for(Path::new("A.java")).unwrap()),
            false,
        ))
    }

    fn parse(&mut self, change: &Change) {
        self.service.update(
            DocumentId(1),
            self.state.version(),
            self.state.snapshot(),
            change,
        );
        let snapshot = self
            .results
            .recv_timeout(Duration::from_secs(10))
            .expect("a parse result");
        assert!(self.state.accept_syntax(snapshot));
    }

    fn perform(&mut self, action: &str) {
        if let Some(change) = self.state.perform(action, 20) {
            self.parse(&change);
        }
    }

    fn type_text(&mut self, text: &str) {
        if let Some(change) = self.state.type_text(text) {
            self.parse(&change);
        }
    }

    fn caret_at(&mut self, needle: &str) {
        let at = self
            .state
            .text()
            .find(needle)
            .expect("the needle is in the text");
        self.state.click(at, false, false, 1);
    }

    fn selected(&self) -> String {
        self.state
            .store()
            .slice(self.state.selections().primary().range())
    }
}

#[test]
fn server_edits_apply_at_once_and_move_the_caret() {
    let mut fixture = Fixture::java(JAVA);
    fixture.caret_at("size");
    let import = JAVA.find("import a.C;\n").unwrap();
    let body = JAVA.find("int size").unwrap();
    let edits = vec![
        Edit::new(import..import + "import a.C;\n".len(), ""),
        Edit::new(body..body, "final "),
    ];
    let change = fixture.state.apply_edits(edits);
    assert!(change.is_some());
    let text = fixture.state.text();
    assert!(!text.contains("import a.C;"));
    assert!(text.contains("final int size"));
    let caret = fixture.state.selections().primary().head;
    assert_eq!(&text[caret..caret + 4], "size");
}

fn bytes(path: &Path) -> Vec<u8> {
    std::fs::read(path).unwrap()
}

fn save(state: &mut EditorState, path: &Path) {
    let saved = EditorState::save_snapshot(path, state.snapshot_for_save());
    assert!(saved.result.is_ok());
    state.finish_save(&saved);
}

#[test]
fn open_edit_undo_save_keeps_the_file_right() {
    let source = b"class A {\r\n    int a;\r\n}\r\n";
    let path = temp_file("A.java", source);
    let mut state = EditorState::load(&path, None).unwrap();
    assert_eq!(state.eol_name(), "CRLF");
    assert_eq!(state.encoding_name(), "UTF-8");

    state.click(4, false, false, 1);
    state.type_text("x");
    assert!(state.is_dirty());
    save(&mut state, &path);
    assert!(!state.is_dirty());
    assert_eq!(bytes(&path), b"clasxs A {\r\n    int a;\r\n}\r\n");

    state.perform(actions::UNDO, 20);
    assert!(state.is_dirty());
    save(&mut state, &path);
    assert_eq!(bytes(&path), source);
}

#[test]
fn a_save_of_an_old_version_leaves_the_document_dirty() {
    let path = temp_file("A.java", b"class A {}\n");
    let mut state = EditorState::load(&path, None).unwrap();
    state.type_text("a");
    let saved = EditorState::save_snapshot(&path, state.snapshot_for_save());
    state.type_text("b");
    state.finish_save(&saved);
    assert!(state.is_dirty());
}

#[test]
fn imports_collapse_when_the_first_parse_arrives() {
    let fixture = Fixture::java(JAVA);
    let display = fixture.state.display();
    assert_eq!(display.line(0).text, "import ...");
    assert_eq!(display.row_to_line(1), 2);
}

#[test]
fn enter_after_a_brace_indents_one_level_deeper() {
    let mut fixture = Fixture::java(JAVA);
    fixture.caret_at("\n        int size");
    fixture.perform(actions::NEWLINE);
    assert!(
        fixture
            .state
            .text()
            .contains("void run() {\n        \n        int size")
    );
}

#[test]
fn ctrl_w_grows_along_the_tree_and_ctrl_shift_w_goes_back() {
    let mut fixture = Fixture::java(JAVA);
    fixture.caret_at("size = 1");
    fixture.perform(actions::EXTEND_SELECTION);
    assert_eq!(fixture.selected(), "size");
    fixture.perform(actions::EXTEND_SELECTION);
    assert_eq!(fixture.selected(), "size = 1");
    fixture.perform(actions::EXTEND_SELECTION);
    assert_eq!(fixture.selected(), "int size = 1;");
    fixture.perform(actions::SHRINK_SELECTION);
    assert_eq!(fixture.selected(), "size = 1");
}

#[test]
fn ctrl_slash_comments_the_line_in_the_first_column() {
    let mut fixture = Fixture::java(JAVA);
    fixture.caret_at("int size");
    fixture.perform(actions::TOGGLE_COMMENT);
    assert!(fixture.state.text().contains("//        int size"));
    fixture.perform(actions::TOGGLE_COMMENT);
    assert_eq!(fixture.state.text(), JAVA);
}

#[test]
fn the_line_commands_of_the_task_work() {
    let mut fixture = Fixture::java("first\nsecond\nthird\n");
    fixture.caret_at("second");
    fixture.perform(actions::DUPLICATE);
    assert_eq!(fixture.state.text(), "first\nsecond\nsecond\nthird\n");
    fixture.perform(actions::DELETE_LINE);
    assert_eq!(fixture.state.text(), "first\nsecond\nthird\n");
    fixture.caret_at("second");
    fixture.perform(actions::MOVE_LINE_DOWN);
    assert_eq!(fixture.state.text(), "first\nthird\nsecond\n");
    fixture.perform(actions::MOVE_LINE_UP);
    assert_eq!(fixture.state.text(), "first\nsecond\nthird\n");
}

#[test]
fn alt_j_adds_carets_and_typing_goes_to_all_of_them() {
    let mut fixture = Fixture::java("int abc;\nint abc2 = abc;\n");
    fixture.caret_at("abc;");
    fixture.perform(actions::SELECT_NEXT_OCCURRENCE);
    fixture.perform(actions::SELECT_NEXT_OCCURRENCE);
    assert_eq!(fixture.state.selections().len(), 2);
    fixture.perform(actions::UNSELECT_LAST_OCCURRENCE);
    assert_eq!(fixture.state.selections().len(), 1);
    fixture.perform(actions::SELECT_NEXT_OCCURRENCE);
    fixture.type_text("x");
    assert_eq!(fixture.state.text(), "int x;\nint x2 = abc;\n");
    fixture.perform(actions::UNDO);
    assert_eq!(fixture.state.text(), "int abc;\nint abc2 = abc;\n");
}

#[test]
fn vertical_moves_keep_the_goal_column() {
    let mut fixture = Fixture::java("long line\n\nlong line\n");
    fixture.caret_at(" line");
    fixture.perform(actions::MOVE_DOWN);
    assert_eq!(fixture.state.caret(), (1, 0));
    fixture.perform(actions::MOVE_DOWN);
    assert_eq!(fixture.state.caret(), (2, 4));
}

#[test]
fn a_caret_skips_a_collapsed_fold() {
    let mut fixture = Fixture::java(JAVA);
    fixture.state.click(7, false, false, 1);
    fixture.perform(actions::MOVE_RIGHT);
    assert_eq!(
        fixture.state.selections().primary().head,
        "import a.B;\nimport a.C;".len()
    );
    fixture.perform(actions::MOVE_LEFT);
    assert_eq!(fixture.state.selections().primary().head, 7);
}

#[test]
fn typing_inside_a_collapsed_fold_expands_it() {
    let mut fixture = Fixture::java(JAVA);
    fixture.state.click("import a.B".len(), false, false, 1);
    fixture.state.expand_fold("import a.B".len());
    fixture.type_text("x");
    assert_eq!(fixture.state.display().line(0).text, "import a.Bx;");
}

#[test]
fn cut_without_a_selection_takes_the_line() {
    let mut fixture = Fixture::java("one\ntwo\n");
    fixture.caret_at("two");
    assert_eq!(fixture.state.copy_text(), "two\n");
    fixture.state.cut();
    assert_eq!(fixture.state.text(), "one\n");
}

#[test]
fn an_ime_composition_replaces_itself() {
    let mut fixture = Fixture::java("ab");
    fixture.state.click(1, false, false, 1);
    fixture.state.replace_range(None, "ж", true);
    assert_eq!(fixture.state.marked(), Some(1..3));
    fixture.state.replace_range(None, "жу", true);
    assert_eq!(fixture.state.text(), "aжуb");
    fixture.state.replace_range(None, "и", false);
    assert_eq!(fixture.state.text(), "aиb");
    assert_eq!(fixture.state.marked(), None);
}

#[test]
fn a_read_only_document_refuses_edits() {
    let mut state = EditorState::new("text", FileFormat::default(), None, true);
    assert!(state.type_text("x").is_none());
    assert!(state.perform(actions::DELETE_LINE, 20).is_none());
    assert_eq!(state.text(), "text");
}

#[test]
fn every_editor_action_has_a_default_binding() {
    let (registry, keymap) = default_keymap();
    for (id, action) in actions::IDEA_IDS {
        assert!(registry.contains(action));
        let shortcuts = keymap.shortcuts_for(action);
        assert!(!shortcuts.is_empty(), "{id}");
        if let Some(Shortcut::Keys(keys)) = shortcuts.first()
            && let [key] = keys.as_slice()
        {
            assert_eq!(keymap.resolve(key, actions::CONTEXT), Some(*action));
        }
    }
}

#[test]
fn a_clean_buffer_takes_the_disk_and_a_dirty_one_refuses() {
    let mut state = EditorState::new("one\ntwo\n", FileFormat::default(), None, false);
    let decoded = text_core::decode(b"one\n2\n");
    let change = state.reload(decoded).unwrap().unwrap();
    assert_eq!(state.text(), "one\n2\n");
    assert!(!state.is_dirty());
    assert_eq!(change.steps.len(), 1);

    state.type_text("x").unwrap();
    assert_eq!(
        state.reload(text_core::decode(b"theirs\n")),
        Err(editor_view::Conflict)
    );
    assert!(state.text().starts_with('x'));
}

//! The whole path without a window: open a file, edit it, get the events, save it.

use std::fs;
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::thread;

use platform::CanonPath;
use workspace_core::{
    CommandRegistry, Delivery, Diagnostic, DiagnosticsStore, DocumentId, DocumentStore, Edit,
    EditSource, EditorSnapshot, Editors, Event, LoadedFile, Place, Reload, Selection, Severity,
    Subscription, TextStore, TransactionError, WorkspaceTransaction,
};

const AGENT: EditSource = EditSource::Agent(7);

struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let unique = NEXT.fetch_add(1, Ordering::Relaxed);
        let name = format!("workspace-core-{}-{unique}", std::process::id());
        let path = std::env::temp_dir().join(name);
        fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::write(&path, text).unwrap();
        path
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn open(store: &mut DocumentStore, path: &Path) -> DocumentId {
    store.open(LoadedFile::read(path).unwrap())
}

fn text_of(store: &DocumentStore, document: DocumentId) -> String {
    store.state(document).unwrap().text.text()
}

fn drain(subscription: &mut Subscription<Event>) -> Vec<Event> {
    let mut events = Vec::new();
    while let Some(delivery) = subscription.try_next() {
        let Delivery::Event(event) = delivery else {
            panic!("lost events: {delivery:?}");
        };
        events.push(event);
    }
    events
}

#[test]
fn open_edit_and_save_without_a_window() {
    let directory = TempDir::new();
    let path = directory.file("Main.txt", "hello world");
    let mut store = DocumentStore::new();
    let mut events = store.events().subscribe(16);

    let document = open(&mut store, &path);
    let edit = Edit::new(0..5, "goodbye");
    store
        .edit(document, EditSource::User, vec![edit.clone()])
        .unwrap();

    assert_eq!(text_of(&store, document), "goodbye world");
    let state = store.state(document).unwrap();
    assert_eq!(state.version, 1);
    assert_eq!(state.last_source, Some(EditSource::User));
    assert!(state.is_dirty());
    assert_eq!(fs::read_to_string(&path).unwrap(), "hello world");

    let saved = store.save_request(document).unwrap().write().unwrap();
    store.saved(saved);

    assert_eq!(fs::read_to_string(&path).unwrap(), "goodbye world");
    assert!(!store.state(document).unwrap().is_dirty());

    let path = store.state(document).unwrap().path.clone();
    let expected = [
        Event::Opened { document, path },
        Event::Changed {
            document,
            version: 1,
            source: EditSource::User,
            edits: Arc::from([edit]),
        },
        Event::Saved {
            document,
            version: 1,
        },
    ];
    assert_eq!(drain(&mut events), expected);
}

#[test]
fn a_transaction_changes_all_documents_or_none() {
    let directory = TempDir::new();
    let mut store = DocumentStore::new();
    let first = open(&mut store, &directory.file("a.txt", "one"));
    let second = open(&mut store, &directory.file("b.txt", "two"));
    let mut events = store.events().subscribe(16);

    let bad = WorkspaceTransaction::new(AGENT)
        .edit(first, vec![Edit::new(0..3, "ONE")])
        .edit(second, vec![Edit::new(0..99, "TWO")]);
    let range = 0..99;
    assert_eq!(
        store.apply(bad),
        Err(TransactionError::BadRange {
            document: second,
            range
        })
    );
    assert_eq!(text_of(&store, first), "one");
    assert!(drain(&mut events).is_empty());

    let good = WorkspaceTransaction::new(AGENT)
        .edit(first, vec![Edit::new(0..3, "ONE")])
        .edit(second, vec![Edit::new(0..3, "TWO")]);
    store.apply(good).unwrap();

    assert_eq!(text_of(&store, first), "ONE");
    assert_eq!(text_of(&store, second), "TWO");
    assert_eq!(store.state(second).unwrap().last_source, Some(AGENT));
    assert_eq!(drain(&mut events).len(), 2);
}

#[test]
fn a_change_for_an_old_version_is_refused() {
    let directory = TempDir::new();
    let mut store = DocumentStore::new();
    let document = open(&mut store, &directory.file("a.txt", "text"));
    let stale = WorkspaceTransaction::new(AGENT).edit_at(document, 0, vec![Edit::new(0..0, "x")]);
    store
        .edit(document, EditSource::User, vec![Edit::new(4..4, "!")])
        .unwrap();

    assert_eq!(
        store.apply(stale),
        Err(TransactionError::StaleVersion {
            document,
            expected: 0,
            actual: 1
        })
    );
    assert_eq!(text_of(&store, document), "text!");
}

#[test]
fn the_inverse_takes_a_whole_transaction_back() {
    let directory = TempDir::new();
    let mut store = DocumentStore::new();
    let first = open(&mut store, &directory.file("a.txt", "alpha beta gamma"));
    let second = open(&mut store, &directory.file("b.txt", "привет"));

    let applied = store
        .apply(
            WorkspaceTransaction::new(AGENT)
                .edit(
                    first,
                    vec![Edit::new(11..16, "G"), Edit::new(0..5, "a longer word")],
                )
                .edit(second, vec![Edit::new(0..12, "")]),
        )
        .unwrap();
    assert_eq!(text_of(&store, first), "a longer word beta G");
    assert_eq!(text_of(&store, second), "");

    store.apply(applied.inverse).unwrap();

    assert_eq!(text_of(&store, first), "alpha beta gamma");
    assert_eq!(text_of(&store, second), "привет");
}

#[test]
fn the_inverse_refuses_text_that_moved_on() {
    let directory = TempDir::new();
    let mut store = DocumentStore::new();
    let document = open(&mut store, &directory.file("a.txt", "text"));
    let applied = store
        .edit(document, AGENT, vec![Edit::new(0..1, "T")])
        .unwrap();
    store
        .edit(document, EditSource::User, vec![Edit::new(4..4, "!")])
        .unwrap();

    let refused = store.apply(applied.inverse);

    assert!(matches!(
        refused,
        Err(TransactionError::StaleVersion { .. })
    ));
}

#[test]
fn edits_that_make_no_sense_are_refused() {
    let directory = TempDir::new();
    let mut store = DocumentStore::new();
    let document = open(&mut store, &directory.file("a.txt", "яблоко"));

    for edits in [
        vec![Edit::new(1..2, "x")],
        vec![Edit::new(0..4, "x"), Edit::new(2..6, "y")],
        vec![Edit::new(0..100, "x")],
    ] {
        let refused = store.edit(document, AGENT, edits);

        assert!(matches!(refused, Err(TransactionError::BadRange { .. })));
    }
    let unknown = DocumentId::from_raw(1000);
    assert_eq!(
        store.edit(unknown, AGENT, Vec::new()),
        Err(TransactionError::NoSuchDocument(unknown))
    );
    let twice = WorkspaceTransaction::new(AGENT)
        .edit(document, Vec::new())
        .edit(document, Vec::new());
    assert_eq!(
        store.apply(twice),
        Err(TransactionError::RepeatedDocument(document))
    );
    assert_eq!(store.state(document).unwrap().version, 0);
}

#[test]
fn opening_a_file_twice_keeps_its_unsaved_edits() {
    let directory = TempDir::new();
    let path = directory.file("a.txt", "text");
    let mut store = DocumentStore::new();
    let document = open(&mut store, &path);
    store
        .edit(document, EditSource::User, vec![Edit::new(0..0, ">")])
        .unwrap();

    let again = open(&mut store, &path);

    assert_eq!(again, document);
    assert_eq!(text_of(&store, document), ">text");
}

#[test]
fn another_thread_reads_without_the_store() {
    let directory = TempDir::new();
    let mut store = DocumentStore::new();
    let document = open(&mut store, &directory.file("a.txt", "text"));
    let reader = store.reader(document).unwrap();
    store
        .edit(document, AGENT, vec![Edit::new(0..0, ">")])
        .unwrap();

    let version = thread::spawn(move || reader.latest().version)
        .join()
        .unwrap();

    assert_eq!(version, 1);
}

#[test]
fn a_save_of_an_old_version_leaves_the_document_dirty() {
    let directory = TempDir::new();
    let path = directory.file("a.txt", "text");
    let mut store = DocumentStore::new();
    let document = open(&mut store, &path);
    store
        .edit(document, EditSource::User, vec![Edit::new(0..0, "1")])
        .unwrap();
    let request = store.save_request(document).unwrap();
    store
        .edit(document, EditSource::User, vec![Edit::new(0..0, "2")])
        .unwrap();

    store.saved(request.write().unwrap());

    let state = store.state(document).unwrap();
    assert_eq!((state.version, state.saved_version), (2, 1));
    assert!(state.is_dirty());
}

#[test]
fn a_save_leaves_only_the_file() {
    let directory = TempDir::new();
    let path = directory.file("a.txt", "text");
    let mut store = DocumentStore::new();
    let document = open(&mut store, &path);
    store
        .edit(document, EditSource::User, vec![Edit::new(0..4, "new")])
        .unwrap();

    store.save_request(document).unwrap().write().unwrap();

    assert_eq!(fs::read_to_string(&path).unwrap(), "new");
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}

#[test]
fn a_save_that_fails_keeps_the_old_file_and_the_edits() {
    let directory = TempDir::new();
    let path = directory.file("a.txt", "text");
    let mut store = DocumentStore::new();
    let document = open(&mut store, &path);
    store
        .edit(document, EditSource::User, vec![Edit::new(0..4, "new")])
        .unwrap();
    let request = store.save_request(document).unwrap();
    // A file held without sharing cannot be swapped out.
    let open_file = fs::OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&path)
        .unwrap();

    assert!(request.write().is_err());

    drop(open_file);
    assert_eq!(fs::read_to_string(&path).unwrap(), "text");
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
    assert!(store.state(document).unwrap().is_dirty());
}

#[test]
fn closing_ends_the_document() {
    let directory = TempDir::new();
    let path = directory.file("a.txt", "text");
    let mut store = DocumentStore::new();
    let document = open(&mut store, &path);
    let mut events = store.events().subscribe(16);

    store.close(document);

    assert!(store.state(document).is_none());
    assert_eq!(drain(&mut events), [Event::Closed { document }]);
    assert_ne!(open(&mut store, &path), document);
}

#[test]
fn a_file_outside_utf8_opens_edits_and_saves_in_its_encoding() {
    let directory = TempDir::new();
    let path = directory.0.join("a.txt");
    let (source, _, _) = encoding_rs::WINDOWS_1251.encode("привет мир, это файл с кириллицей\r\n");
    fs::write(&path, &source).unwrap();
    let mut store = DocumentStore::new();
    let document = open(&mut store, &path);

    assert_eq!(
        text_of(&store, document),
        "привет мир, это файл с кириллицей\n"
    );
    assert!(!store.state(document).unwrap().read_only);

    let edit = Edit::new(0..0, "я\r\n");
    store.edit(document, EditSource::User, vec![edit]).unwrap();
    assert_eq!(
        text_of(&store, document),
        "я\nпривет мир, это файл с кириллицей\n"
    );
    store.save_request(document).unwrap().write().unwrap();

    let (expected, _, _) =
        encoding_rs::WINDOWS_1251.encode("я\r\nпривет мир, это файл с кириллицей\r\n");
    assert_eq!(fs::read(&path).unwrap(), expected.as_ref());
}

#[test]
fn mixed_line_ends_survive_an_edit_on_another_line() {
    let directory = TempDir::new();
    let path = directory.0.join("a.txt");
    fs::write(&path, "one\r\ntwo\nthree\r\n").unwrap();
    let mut store = DocumentStore::new();
    let document = open(&mut store, &path);

    let at = text_of(&store, document).find("two").unwrap();
    store
        .edit(document, EditSource::User, vec![Edit::new(at..at, "new\n")])
        .unwrap();
    store.save_request(document).unwrap().write().unwrap();

    let bytes = fs::read(&path).unwrap();
    assert!(bytes.starts_with(b"one\r\nnew"));
    assert!(bytes.ends_with(b"two\nthree\r\n"));
}

#[test]
fn a_document_that_is_read_only_refuses_edits_and_saves() {
    let directory = TempDir::new();
    let path = directory.0.join("large.txt");
    let size = text_core::MAX_EDITABLE_BYTES as usize + 1;
    fs::write(&path, vec![b'a'; size]).unwrap();
    let mut store = DocumentStore::new();
    let document = open(&mut store, &path);

    assert_eq!(
        store.edit(document, EditSource::User, vec![Edit::new(0..0, "x")]),
        Err(TransactionError::ReadOnly(document))
    );
    assert!(store.save_request(document).is_none());
}

#[test]
fn a_clean_document_takes_an_external_change_as_one_edit() {
    let directory = TempDir::new();
    let path = directory.file("A.txt", "one\ntwo\nthree\n");
    let mut store = DocumentStore::new();
    let document = open(&mut store, &path);
    let mut events = store.events().subscribe(16);

    fs::write(&path, "one\n2\nthree\n").unwrap();
    let reload = store.reload(LoadedFile::read(&path).unwrap());

    let edit = Edit::new(4..7, "2");
    assert_eq!(
        reload,
        Reload::Changed {
            document,
            edit: edit.clone()
        }
    );
    assert_eq!(text_of(&store, document), "one\n2\nthree\n");
    let state = store.state(document).unwrap();
    assert!(!state.is_dirty());
    assert_eq!(state.last_source, Some(EditSource::External));
    let events = drain(&mut events);
    let [Event::Changed { source, edits, .. }] = events.as_slice() else {
        panic!("one change");
    };
    assert_eq!((*source, &edits[..]), (EditSource::External, &[edit][..]));

    let unchanged = store.reload(LoadedFile::read(&path).unwrap());
    assert_eq!(unchanged, Reload::Unchanged(document));
}

#[test]
fn a_dirty_document_keeps_its_edits_over_the_disk() {
    let directory = TempDir::new();
    let path = directory.file("A.txt", "one\n");
    let mut store = DocumentStore::new();
    let document = open(&mut store, &path);
    store
        .edit(document, EditSource::User, vec![Edit::insert(0, "my ")])
        .unwrap();

    fs::write(&path, "theirs\n").unwrap();
    let reload = store.reload(LoadedFile::read(&path).unwrap());

    assert_eq!(reload, Reload::Conflict(document));
    assert_eq!(text_of(&store, document), "my one\n");
}

#[test]
fn diagnostics_of_a_source_replace_each_other_and_old_ones_lose() {
    let directory = TempDir::new();
    let path = CanonPath::new(directory.file("A.txt", "text")).unwrap();
    let problem = |start, severity| Diagnostic {
        range: start..start + 1,
        severity,
        message: "bad".into(),
        code: None,
    };
    let mut diagnostics = DiagnosticsStore::new();

    diagnostics.publish(path.clone(), "native", 2, vec![problem(3, Severity::Error)]);
    diagnostics.publish(
        path.clone(),
        "server",
        1,
        vec![problem(0, Severity::Warning)],
    );
    assert!(!diagnostics.publish(path.clone(), "native", 1, Vec::new()));

    let found: Vec<(&str, usize)> = diagnostics
        .for_path(&path)
        .into_iter()
        .map(|(source, diagnostic)| (source, diagnostic.range.start))
        .collect();
    assert_eq!(found, [("server", 0), ("native", 3)]);
    assert_eq!(diagnostics.count(Severity::Error), 1);

    diagnostics.clear(&path, "native");
    assert_eq!(diagnostics.count(Severity::Error), 0);
    assert_eq!(diagnostics.paths().count(), 1);
}

#[test]
fn editors_report_the_active_one_recent_files_and_the_way_back() {
    let directory = TempDir::new();
    let mut store = DocumentStore::new();
    let first = open(&mut store, &directory.file("A.txt", "a"));
    let second = open(&mut store, &directory.file("B.txt", "b"));
    let path = |document| store.state(document).unwrap().path.clone();
    let snapshot = |document, offset| EditorSnapshot {
        document,
        path: path(document),
        version: 0,
        selections: vec![Selection::caret(offset)],
        visible_lines: 0..40,
    };
    let mut editors = Editors::new();
    let mut commands = CommandRegistry::new();
    commands.register("workspace.back", "Back", "workspace");

    editors.update(snapshot(first, 0));
    editors.activate(first);
    editors.jumped_from(Place {
        path: path(first),
        offset: 0,
    });
    editors.update(snapshot(second, 1));
    editors.activate(second);

    assert_eq!(editors.active().unwrap().document, second);
    let recent: Vec<_> = editors.recent().cloned().collect();
    assert_eq!(recent, [path(second), path(first)]);

    let here = Place {
        path: path(second),
        offset: 1,
    };
    let back = editors.back(here.clone()).unwrap();
    assert_eq!(back.path, path(first));
    assert_eq!(editors.forward(back), Some(here));
    assert_eq!(commands.get("workspace.back").unwrap().title, "Back");
}

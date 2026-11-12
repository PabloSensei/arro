//! The M2 path on PetClinic without a window: find a file, text and a class, go there,
//! come back, read what the editor shows, see a change made outside the IDE.
//!
//! PetClinic lives in `corpus/` next to the repository, outside git, so the tests are
//! ignored by default and a missing corpus fails them with a message instead of passing
//! them empty. Run them with
//! `IDE_CORPUS=<folder that holds src/petclinic> cargo test -p java-lang --test petclinic -- --ignored`;
//! without `IDE_CORPUS` the `corpus/` folder of the checkout is used.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use base::Cancel;
use fs_watch::{FileTable, FsChange, FsWatcher};
use lang_api::SymbolProvider;
use platform::CanonPath;
use search::{GotoQuery, SymbolIndex, TextQuery, find_in_files, index_files, rank_files};
use workspace_core::{
    DocumentId, DocumentStore, EditorSnapshot, Editors, LoadedFile, Place, Reload, Selection,
    TextStore,
};

/// The corpus root, or why there is none.
fn petclinic() -> PathBuf {
    let own = std::env::var_os("IDE_CORPUS").map(PathBuf::from);
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    let candidates: Vec<PathBuf> = match own {
        Some(folder) => vec![folder.join("src/petclinic")],
        // `corpus/` sits in the main checkout, an ancestor of a worktree too.
        None => manifest
            .ancestors()
            .map(|folder| folder.join("corpus/src/petclinic"))
            .collect(),
    };
    let found = candidates.iter().find(|root| root.join("pom.xml").exists());
    let Some(found) = found else {
        panic!(
            "PetClinic is missing, looked for pom.xml in {candidates:?}; clone it with tools/corpus/corpus.ps1 or point IDE_CORPUS at the folder that holds src/petclinic"
        );
    };
    fs::canonicalize(found).expect("the corpus folder can be read")
}

fn provider_for(path: &Path) -> Option<&'static dyn SymbolProvider> {
    java_lang::symbols_for(path)
}

/// Opens `path` at `offset` the way the window does, remembering where the caret was.
fn go_to(
    store: &mut DocumentStore,
    editors: &mut Editors,
    path: &Path,
    offset: usize,
) -> DocumentId {
    if let Some(current) = editors.active() {
        let from = Place {
            path: current.path.clone(),
            offset: current.selections[0].head,
        };
        editors.jumped_from(from);
    }
    let document = store.open(LoadedFile::read(path).unwrap());
    show(store, editors, document, offset);
    document
}

fn show(store: &DocumentStore, editors: &mut Editors, document: DocumentId, offset: usize) {
    let state = store.state(document).unwrap();
    let line = state.text.offset_to_line(offset) as u32;
    editors.update(EditorSnapshot {
        document,
        path: state.path.clone(),
        version: state.version,
        selections: vec![Selection::caret(offset)],
        visible_lines: line.saturating_sub(10)..line + 30,
    });
    editors.activate(document);
}

#[test]
#[ignore = "needs the PetClinic corpus: run with --ignored, IDE_CORPUS sets its folder"]
fn find_go_there_and_back_on_petclinic() {
    let root = petclinic();
    let excludes = vec![
        root.join("target"),
        root.join("build"),
        root.join(".gradle"),
    ];
    let table = FileTable::scan(&root, excludes).unwrap();
    let files: Vec<PathBuf> = table
        .files()
        .iter()
        .map(|entry| entry.path.clone())
        .collect();
    assert!(files.iter().any(|path| path.ends_with("pom.xml")));

    // Ctrl+Shift+N: a file with a line.
    let query = GotoQuery::parse("OwnerContr:40");
    let found = rank_files(&query.pattern, &root, files.iter().map(PathBuf::as_path), 5);
    let controller = found[0].item.clone();
    assert!(controller.ends_with("owner/OwnerController.java"));
    assert_eq!(query.line, Some(40));

    // Ctrl+Shift+F: text.
    let hits = Mutex::new(Vec::new());
    let on_file = |file| hits.lock().unwrap().push(file);
    let text = TextQuery::plain("findByLastName");
    find_in_files(&text, &files, &HashMap::new(), &Cancel::new(), &on_file).unwrap();
    let hits = hits.into_inner().unwrap();
    assert!(
        hits.iter()
            .any(|file| file.path.ends_with("OwnerRepository.java"))
    );

    // Ctrl+N: a class, from the index of every source.
    let mut index = SymbolIndex::new();
    for (path, symbols) in index_files(&files, &provider_for, &Cancel::new()) {
        index.update(path, symbols);
    }
    let classes = index.search("OwnCon", |symbol| symbol.kind.is_type(), 5);
    let class = &classes[0].item;
    assert_eq!(class.symbol.name, "OwnerController");
    assert_eq!(
        class.symbol.container.as_deref(),
        Some("org.springframework.samples.petclinic.owner")
    );

    // Go there, then to Owner, then back.
    let mut store = DocumentStore::new();
    let mut editors = Editors::new();
    let first = go_to(
        &mut store,
        &mut editors,
        &class.path,
        class.symbol.name_range.start,
    );
    let owner = index.search("Owner", |symbol| symbol.kind.is_type(), 1);
    let owner = &owner[0].item;
    go_to(
        &mut store,
        &mut editors,
        &owner.path,
        owner.symbol.name_range.start,
    );

    // Ctrl+F12 on Owner, from the same index.
    let structure = index.file(&owner.path).unwrap();
    assert!(structure.iter().any(|symbol| symbol.name == "getPets"));

    let active = editors.active().unwrap();
    let here = Place {
        path: active.path.clone(),
        offset: active.selections[0].head,
    };
    let back = editors.back(here).unwrap();
    assert_eq!(back.path, CanonPath::new(&class.path).unwrap());
    let document = store.document_at(&back.path).unwrap();
    show(&store, &mut editors, document, back.offset);

    // An agent reads what the editor shows.
    let active = editors.active().unwrap();
    assert_eq!(active.document, first);
    let state = store.state(active.document).unwrap();
    let caret = active.selections[0].head;
    assert_eq!(&state.text.text()[caret..caret + 15], "OwnerController");
    let recent: Vec<&CanonPath> = editors.recent().collect();
    assert_eq!(recent.len(), 2);
}

#[test]
#[ignore = "needs the PetClinic corpus: run with --ignored, IDE_CORPUS sets its folder"]
fn a_change_on_disk_reaches_the_open_document() {
    let root = petclinic();
    // A copy, so the corpus stays as it is.
    let source = root.join("src/main/java/org/springframework/samples/petclinic/owner/Pet.java");
    let directory = test_support::scratch("petclinic-watch");
    let directory = fs::canonicalize(directory).unwrap();
    let path = directory.join("Pet.java");
    fs::copy(&source, &path).unwrap();

    let mut table = FileTable::scan(&directory, Vec::new()).unwrap();
    let mut store = DocumentStore::new();
    let document = store.open(LoadedFile::read(&path).unwrap());
    let (sender, batches) = mpsc::channel();
    let _watcher = FsWatcher::start(&directory, move |batch| {
        let _ = sender.send(batch);
    })
    .unwrap();

    let original = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        original.replace("class Pet ", "class Pet /* edited */ "),
    )
    .unwrap();

    let deadline = Instant::now() + Duration::from_secs(10);
    let mut reloaded = None;
    while reloaded.is_none() {
        let left = deadline.saturating_duration_since(Instant::now());
        let batch = batches
            .recv_timeout(left)
            .expect("the watcher reports the change");
        for change in table.apply(batch) {
            if let FsChange::Changed(changed) = change
                && changed == path
            {
                reloaded = Some(store.reload(LoadedFile::read(&changed).unwrap()));
            }
        }
    }

    assert!(matches!(reloaded, Some(Reload::Changed { .. })));
    let text = store.state(document).unwrap().text.text();
    assert!(text.contains("class Pet /* edited */ "));
}

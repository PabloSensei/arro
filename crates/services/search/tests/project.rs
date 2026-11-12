//! A small project on disk: files, text with open documents, symbols and Replace.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base::Cancel;
use lang_api::{Symbol, SymbolKind, SymbolProvider};
use search::{
    ActionItem, FileHits, Found, GotoQuery, Sources, SymbolIndex, Tab, TextQuery, find_in_files,
    index_files, rank_files, replace_edits, search_everywhere,
};
use workspace_core::{DocumentStore, EditSource, LoadedFile, TextStore, WorkspaceTransaction};

struct Project(PathBuf, #[allow(dead_code)] test_support::TempDir);

impl Project {
    fn new() -> Self {
        let folder = test_support::TempDir::new("search");
        Self(fs::canonicalize(&folder).unwrap(), folder)
    }

    fn file(&self, name: &str, text: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, text).unwrap();
        path
    }
}

/// `type Name` and `fn name` lines, enough to stand in for a language.
struct Lines;

impl SymbolProvider for Lines {
    fn file_symbols(&self, _path: &Path, text: &str) -> Vec<Symbol> {
        let mut symbols = Vec::new();
        let mut offset = 0;
        for line in text.split_inclusive('\n') {
            let words: Vec<&str> = line.split_whitespace().collect();
            if let [keyword, name, ..] = words.as_slice() {
                let kind = match *keyword {
                    "type" => Some(SymbolKind::Class),
                    "fn" => Some(SymbolKind::Method),
                    _ => None,
                };
                if let Some(kind) = kind {
                    let start = offset + line.find(name).unwrap();
                    symbols.push(Symbol {
                        name: (*name).to_owned(),
                        kind,
                        container: None,
                        depth: u32::from(kind == SymbolKind::Method),
                        range: offset..offset + line.len(),
                        name_range: start..start + name.len(),
                    });
                }
            }
            offset += line.len();
        }
        symbols
    }
}

fn sample() -> (Project, Vec<PathBuf>) {
    let project = Project::new();
    let files = vec![
        project.file(
            "src/owner/Owner.txt",
            "type Owner\nfn getName\nfn getPets\n",
        ),
        project.file(
            "src/owner/OwnerController.txt",
            "type OwnerController\nfn showOwner\nOwner owner = find();\n",
        ),
        project.file("src/vet/Vet.txt", "type Vet\nfn getName\n"),
        project.file("README.md", "Owners and vets.\n"),
    ];
    (project, files)
}

fn find(query: &TextQuery, files: &[PathBuf], open: &HashMap<PathBuf, String>) -> Vec<FileHits> {
    let found = Mutex::new(Vec::new());
    let on_file = |hits| found.lock().unwrap().push(hits);
    find_in_files(query, files, open, &Cancel::new(), &on_file).unwrap();
    let mut found = found.into_inner().unwrap();
    found.sort_by(|a, b| a.path.cmp(&b.path));
    found
}

#[test]
fn files_are_found_by_name_with_a_line_to_go_to() {
    let (project, files) = sample();
    let query = GotoQuery::parse("ownctr:2");
    let found = rank_files(
        &query.pattern,
        &project.0,
        files.iter().map(|p| p.as_path()),
        5,
    );
    assert_eq!(found[0].item, files[1]);
    assert_eq!(query.line, Some(2));
}

#[test]
fn text_is_found_on_disk_and_in_open_documents() {
    let (_project, files) = sample();
    let query = TextQuery {
        pattern: "owner".into(),
        whole_words: true,
        ..TextQuery::default()
    };

    let found = find(&query, &files, &HashMap::new());
    let paths: Vec<&Path> = found.iter().map(|hits| hits.path.as_path()).collect();
    assert_eq!(paths, [files[0].as_path(), files[1].as_path()]);
    let line = &found[1].lines[0];
    assert_eq!(
        (line.line, line.text.as_str()),
        (3, "Owner owner = find();")
    );
    assert_eq!(line.matches, [0..5, 6..11]);

    // The open document of Vet says Owner, though the disk does not.
    let open = HashMap::from([(files[2].clone(), "type Vet\nfn owner\n".to_owned())]);
    let found = find(&query, &files, &open);
    assert!(found.iter().any(|hits| hits.path == files[2]));
}

#[test]
fn symbols_are_indexed_and_searched_by_kind() {
    let (project, files) = sample();
    let provider_for = |path: &Path| {
        let has_symbols = path.extension().is_some_and(|extension| extension == "txt");
        has_symbols.then_some(&Lines as &dyn SymbolProvider)
    };
    let mut index = SymbolIndex::new();
    for (path, symbols) in index_files(&files, &provider_for, &Cancel::new()) {
        index.update(path, symbols);
    }
    assert_eq!(index.file_count(), 3);

    let classes = index.search("oc", |symbol| symbol.kind.is_type(), 10);
    assert_eq!(classes[0].item.symbol.name, "OwnerController");

    let structure = index.file(&files[0]).unwrap();
    let names: Vec<&str> = structure
        .iter()
        .map(|symbol| symbol.name.as_str())
        .collect();
    assert_eq!(names, ["Owner", "getName", "getPets"]);

    let actions = [ActionItem {
        id: "workspace.find_in_files",
        title: "Find in Files".into(),
    }];
    let sources = Sources {
        root: &project.0,
        files: &files,
        symbols: &index,
        actions: &actions,
    };
    let groups = search_everywhere("owner", Tab::All, &sources, 3);
    let tabs: Vec<Tab> = groups.iter().map(|group| group.tab).collect();
    assert_eq!(tabs, [Tab::Classes, Tab::Files, Tab::Symbols]);
    let Found::Symbol(best) = &groups[0].items[0].item else {
        panic!("classes come first");
    };
    assert_eq!(
        (best.symbol.name.as_str(), &best.path),
        ("Owner", &files[0])
    );

    let groups = search_everywhere("fif", Tab::Actions, &sources, 3);
    assert_eq!(
        groups[0].items[0].item,
        Found::Action("workspace.find_in_files")
    );
}

#[test]
fn replace_in_files_applies_to_every_document_or_none() {
    let (_project, files) = sample();
    let query = TextQuery {
        pattern: "getName".into(),
        match_case: true,
        ..TextQuery::default()
    };
    let found = find(&query, &files, &HashMap::new());
    assert_eq!(found.len(), 2);

    let mut store = DocumentStore::new();
    let mut transaction = WorkspaceTransaction::new(EditSource::User);
    let mut documents = Vec::new();
    for hits in &found {
        let document = store.open(LoadedFile::read(&hits.path).unwrap());
        let state = store.state(document).unwrap();
        let edits = replace_edits(&query, "fullName", &state.text.text()).unwrap();
        transaction = transaction.edit_at(document, state.version, edits);
        documents.push(document);
    }

    // A stale version anywhere refuses the whole change.
    let mut stale = transaction.clone();
    stale.changes[1].expected_version = Some(7);
    assert!(store.apply(stale).is_err());
    assert!(
        store
            .state(documents[0])
            .unwrap()
            .text
            .text()
            .contains("getName")
    );

    store.apply(transaction).unwrap();
    for document in &documents {
        let request = store.save_request(*document).unwrap();
        let saved = request.write().unwrap();
        store.saved(saved);
    }
    let query = TextQuery::plain("fullName");
    assert_eq!(find(&query, &files, &HashMap::new()).len(), 2);
    assert!(find(&TextQuery::plain("getName"), &files, &HashMap::new()).is_empty());
}

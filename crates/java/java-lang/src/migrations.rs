//! The migrations of the project for the Database window: the files of Flyway and
//! Liquibase from the model of the project, their state against the history table of a
//! connection, and the changelogs of Liquibase as a language of the editor, which
//! completes their tags and checks their changesets and includes.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex};
use std::thread;

use base::Cancel;
use lang_api::{
    Answer, CompletionEntry, CompletionKind, CompletionList, CompletionProvider, Confidence,
    DiagnosticEntry, DiagnosticsProvider, DiagnosticsQuery, DisabledInspections, DocumentSpot,
    LanguageId, LanguageSupport, MigrationAction, MigrationHistoryRows, MigrationIssue,
    MigrationKind, MigrationModel, MigrationRow, MigrationState, MigrationSupport, MigrationTool,
    NextMigration, Provider, Queries, Reply, SemanticServer, Severity, SourceId, SyntaxProvider,
    TextSpan,
};
use lsp_client::lsp_types::Uri;
use lsp_client::uri::file_path;
use tree_sitter_language::LanguageFn;

use crate::DatabaseSupport;

const SOURCE: SourceId = SourceId::new("migrations");

/// The changes of a changelog the editor completes, with the attributes each one takes.
const CHANGES: &[(&str, &[&str])] = &[
    ("createTable", &["tableName", "schemaName", "remarks"]),
    ("addColumn", &["tableName", "columnName", "type"]),
    ("dropTable", &["tableName", "cascadeConstraints"]),
    ("createIndex", &["indexName", "tableName", "unique"]),
    (
        "addForeignKeyConstraint",
        &[
            "baseTableName",
            "baseColumnNames",
            "referencedTableName",
            "referencedColumnNames",
            "constraintName",
        ],
    ),
    ("sql", &["sql", "stripComments", "splitStatements"]),
    ("rollback", &[]),
    ("preConditions", &["onFail", "onError"]),
    ("insert", &["tableName"]),
    ("update", &["tableName"]),
    ("delete", &["tableName"]),
    (
        "addUniqueConstraint",
        &["tableName", "columnNames", "constraintName"],
    ),
    ("dropColumn", &["tableName", "columnName"]),
    ("renameTable", &["oldTableName", "newTableName"]),
    (
        "addDefaultValue",
        &["tableName", "columnName", "defaultValue"],
    ),
    ("createSequence", &["sequenceName"]),
    ("createView", &["viewName"]),
];

/// The children of `databaseChangeLog` a changelog may name.
const TOP_LEVEL: &[&str] = &["changeSet", "include", "includeAll", "property"];

/// The attributes of a `changeSet`.
const CHANGE_SET_ATTRIBUTES: &[&str] = &[
    "id",
    "author",
    "runAlways",
    "runOnChange",
    "context",
    "labels",
    "dbms",
    "failOnError",
    "comment",
];

/// The keys of a changeset change, as the attribute list of the XML form names them.
fn attributes_of(change: &str) -> &'static [&'static str] {
    CHANGES
        .iter()
        .find(|(name, _)| *name == change)
        .map(|(_, attributes)| *attributes)
        .unwrap_or(&[])
}

/// The migrations of the project, for the Database window and the tree of the project.
impl MigrationSupport for DatabaseSupport {
    fn model(&self) -> MigrationModel {
        convert(self.scan_migrations())
    }

    fn with_history(&self, histories: &[MigrationHistoryRows]) -> MigrationModel {
        let mut model = self.scan_migrations();
        let rows: Vec<java_project::HistoryRows> = histories
            .iter()
            .map(|history| java_project::HistoryRows {
                tool: tool_of(history.tool),
                columns: history.columns.clone(),
                rows: history.rows.clone(),
            })
            .collect();
        model.apply_history(&rows);
        convert(model)
    }

    fn new_migration_name(&self, description: &str) -> Option<String> {
        let model = self.scan_migrations();
        let next = model.next?;
        Some(java_project::new_file_name(&next.version, description))
    }
}

fn tool_of(tool: MigrationTool) -> java_project::Tool {
    match tool {
        MigrationTool::Flyway => java_project::Tool::Flyway,
        MigrationTool::Liquibase => java_project::Tool::Liquibase,
    }
}

fn tool_from(tool: java_project::Tool) -> MigrationTool {
    match tool {
        java_project::Tool::Flyway => MigrationTool::Flyway,
        java_project::Tool::Liquibase => MigrationTool::Liquibase,
    }
}

fn kind_from(kind: java_project::Kind) -> MigrationKind {
    match kind {
        java_project::Kind::Baseline => MigrationKind::Baseline,
        java_project::Kind::Versioned => MigrationKind::Versioned,
        java_project::Kind::Undo => MigrationKind::Undo,
        java_project::Kind::Repeatable => MigrationKind::Repeatable,
        java_project::Kind::Changelog => MigrationKind::Changelog,
        java_project::Kind::ChangeSet => MigrationKind::ChangeSet,
        java_project::Kind::Include => MigrationKind::Include,
    }
}

fn state_from(state: java_project::State) -> MigrationState {
    match state {
        java_project::State::Pending => MigrationState::Pending,
        java_project::State::Applied => MigrationState::Applied,
        java_project::State::Failed => MigrationState::Failed,
        java_project::State::Changed => MigrationState::Changed,
    }
}

fn convert(model: java_project::Model) -> MigrationModel {
    MigrationModel {
        tools: model.tools.iter().copied().map(tool_from).collect(),
        rows: model
            .files
            .into_iter()
            .map(|file| MigrationRow {
                path: file.path,
                label: file.label,
                tool: tool_from(file.tool),
                kind: kind_from(file.kind),
                version: file.version,
                description: file.description,
                depth: file.depth,
                parent: file.parent,
                target: file.target,
                state: state_from(file.state),
            })
            .collect(),
        issues: model
            .issues
            .into_iter()
            .map(|issue| MigrationIssue {
                path: issue.path,
                line: issue.line,
                id: issue.id.to_owned(),
                message: issue.message,
            })
            .collect(),
        histories: model
            .histories
            .into_iter()
            .map(|history| lang_api::MigrationHistory {
                tool: tool_from(history.tool),
                table: history.table,
                sql: history.sql,
            })
            .collect(),
        actions: model
            .actions
            .into_iter()
            .map(|action| MigrationAction {
                tool: tool_from(action.tool),
                kind: action_kind_from(action.kind),
                goal: action.goal,
                task: action.task,
                unavailable: action.unavailable,
            })
            .collect(),
        folders: model.folders,
        next: model.next.map(|next| NextMigration {
            folder: next.folder,
            version: next.version,
        }),
    }
}

fn action_kind_from(kind: java_project::ActionKind) -> lang_api::MigrationActionKind {
    match kind {
        java_project::ActionKind::Migrate => lang_api::MigrationActionKind::Migrate,
        java_project::ActionKind::Info => lang_api::MigrationActionKind::Info,
        java_project::ActionKind::Validate => lang_api::MigrationActionKind::Validate,
        java_project::ActionKind::Clean => lang_api::MigrationActionKind::Clean,
    }
}

/// The changelog language of a project at `root`, for the editor and the tests.
pub fn migrations_language(root: PathBuf) -> Arc<MigrationFiles> {
    MigrationFiles::new(root)
}

/// The changelogs of Liquibase as a language of the editor: the completion of their tags
/// and attributes, and the checks of their changesets and includes.
#[derive(Clone)]
pub struct MigrationFiles {
    root: PathBuf,
    documents: Arc<Mutex<HashMap<String, Arc<str>>>>,
    checks: Arc<Mutex<DisabledInspections>>,
}

impl MigrationFiles {
    pub fn new(root: PathBuf) -> Arc<Self> {
        Arc::new(Self {
            root,
            documents: Arc::new(Mutex::new(HashMap::new())),
            checks: Arc::new(Mutex::new(DisabledInspections::default())),
        })
    }

    pub fn set_inspection_checks(&self, checks: DisabledInspections) {
        *self
            .checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = checks;
    }

    fn check_enabled(&self, id: &str) -> bool {
        self.checks
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .enabled(id)
    }

    fn document(&self, uri: &str) -> Option<Arc<str>> {
        self.documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .get(uri)
            .cloned()
    }

    fn entries(&self, query: &DocumentSpot) -> Vec<CompletionEntry> {
        let Some(text) = self.document(&query.uri) else {
            return Vec::new();
        };
        let offset = lang_api::offset_in(&text, query.point);
        let Some(path) = Uri::from_str(&query.uri)
            .ok()
            .and_then(|uri| file_path(&uri))
        else {
            return Vec::new();
        };
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "xml" => xml_entries(&text, offset),
            "yaml" | "yml" => yaml_entries(&text, offset),
            "json" => json_entries(&text, offset),
            _ => Vec::new(),
        }
    }

    /// The checks of one changelog: the same id and author twice, and an include that
    /// names no file. The file is read from the document the editor has open.
    fn diagnostics(&self, uri: &str) -> Vec<DiagnosticEntry> {
        let Some(text) = self.document(uri) else {
            return Vec::new();
        };
        let Some(path) = Uri::from_str(uri).ok().and_then(|uri| file_path(&uri)) else {
            return Vec::new();
        };
        let Ok(changelog) = java_project::parse_changelog(&path, &text) else {
            return Vec::new();
        };
        let mut entries = Vec::new();
        let mut seen: HashMap<(String, String), usize> = HashMap::new();
        for changeset in &changelog.changesets {
            let key = (changeset.id.clone(), changeset.author.clone());
            if let Some(first) = seen.insert(key, changeset.line) {
                if !self.check_enabled(java_project::DUPLICATE_CHANGESET) {
                    continue;
                }
                let message = if first == changeset.line {
                    format!(
                        "Changeset {}:{} is declared twice in this changelog",
                        changeset.id, changeset.author
                    )
                } else {
                    format!(
                        "Changeset {}:{} is declared twice in this changelog, first on line {first}",
                        changeset.id, changeset.author
                    )
                };
                entries.push(DiagnosticEntry {
                    span: line_span(&text, changeset.line),
                    severity: Severity::Warning,
                    message,
                    code: Some(java_project::DUPLICATE_CHANGESET.to_owned()),
                    unnecessary: false,
                });
            }
        }
        if self.check_enabled(java_project::MISSING_INCLUDE) {
            for include in &changelog.includes {
                let target = include_target(&path, &include.path, include.relative);
                let exists = if include.all {
                    target.is_dir()
                } else {
                    target.is_file()
                };
                if exists {
                    continue;
                }
                entries.push(DiagnosticEntry {
                    span: line_span(&text, include.line),
                    severity: Severity::Warning,
                    message: format!("{} names no file", target.display()),
                    code: Some(java_project::MISSING_INCLUDE.to_owned()),
                    unnecessary: false,
                });
            }
        }
        entries
    }
}

/// Where an include points from an open changelog: next to it, or from the root of the
/// search path the changelog sits in.
fn include_target(from: &Path, path: &str, relative: bool) -> PathBuf {
    let path = path.trim().trim_start_matches("classpath:");
    let path = Path::new(path);
    if path.is_absolute() {
        return path.to_path_buf();
    }
    let folder = from.parent().unwrap_or(from);
    if relative {
        return folder.join(path);
    }
    let beside = folder.join(path);
    if beside.exists() {
        return beside;
    }
    let root = folder
        .ancestors()
        .find(|ancestor| {
            ancestor
                .file_name()
                .is_some_and(|name| name.eq_ignore_ascii_case("resources"))
        })
        .unwrap_or(folder);
    root.join(path)
}

fn line_span(text: &str, line: usize) -> TextSpan {
    let line = line.max(1);
    let start = text
        .match_indices('\n')
        .nth(line.saturating_sub(2))
        .map_or(0, |(index, _)| index + 1);
    let end = text[start..]
        .find('\n')
        .map_or(text.len(), |offset| start + offset);
    TextSpan {
        start: point_at(text, start),
        end: point_at(text, end),
    }
}

fn point_at(text: &str, offset: usize) -> lang_api::TextPoint {
    let offset = offset.min(text.len());
    let before = &text[..offset];
    let line = before.bytes().filter(|byte| *byte == b'\n').count() as u32;
    let start = before.rfind('\n').map_or(0, |index| index + 1);
    lang_api::TextPoint {
        line,
        character: text[start..offset].encode_utf16().count() as u32,
    }
}

/// The entries of a tag or of an attribute of an XML changelog. The caret inside a tag
/// after its name completes the attributes of that change; the caret where a tag starts
/// completes the names of the changes.
fn xml_entries(text: &str, offset: usize) -> Vec<CompletionEntry> {
    let offset = offset.min(text.len());
    let Some(open) = text[..offset].rfind('<') else {
        return Vec::new();
    };
    if text[..offset].rfind('>').is_some_and(|close| close > open) {
        return Vec::new();
    }
    let tag = &text[open + 1..offset];
    if tag.starts_with('/') || tag.starts_with('?') || tag.starts_with('!') {
        return Vec::new();
    }
    let name: String = tag
        .chars()
        .take_while(|character| character.is_alphanumeric() || *character == '_')
        .collect();
    let in_change_set = inside_change_set(text, open);
    if name.is_empty() {
        let names = if in_change_set {
            CHANGES.iter().map(|(name, _)| *name).collect::<Vec<_>>()
        } else {
            TOP_LEVEL.to_vec()
        };
        return names
            .into_iter()
            .map(|name| tag_entry(name, "change", attributes_of(name)))
            .collect();
    }
    let attributes: &[&str] = if name == "changeSet" {
        CHANGE_SET_ATTRIBUTES
    } else {
        attributes_of(&name)
    };
    if attributes.is_empty() {
        return Vec::new();
    }
    let typed = tag
        .rsplit_once(char::is_whitespace)
        .map_or("", |(_, typed)| typed);
    let typed_name = typed.split_once('=').map_or(typed, |(name, _)| name);
    attributes
        .iter()
        .filter(|attribute| attribute.starts_with(typed_name))
        .map(|attribute| CompletionEntry {
            label: (*attribute).to_owned(),
            detail: Some(name.clone()),
            insert_text: Some(format!("{attribute}=\"\"")),
            kind: CompletionKind::Property,
            ..CompletionEntry::default()
        })
        .collect()
}

fn tag_entry(name: &str, detail: &str, attributes: &[&str]) -> CompletionEntry {
    CompletionEntry {
        label: name.to_owned(),
        detail: Some(detail.to_owned()),
        insert_text: Some(if attributes.is_empty() {
            format!("{name}>")
        } else {
            format!("{name} {}=\"\"", attributes[0])
        }),
        kind: CompletionKind::Keyword,
        ..CompletionEntry::default()
    }
}

/// Whether the tag that starts at `open` sits inside a `changeSet` element.
fn inside_change_set(text: &str, open: usize) -> bool {
    let before = &text[..open];
    let Some(start) = before.rfind("<changeSet") else {
        return false;
    };
    let close = &before[start..];
    !close.contains("</changeSet>")
}

/// The entries of a key of a YAML changelog. The key of the line above says what the caret
/// sits in: the top of the changelog takes its changesets and includes, a `changes` list
/// takes the changes, and a changeSet takes the keys of its attributes.
fn yaml_entries(text: &str, offset: usize) -> Vec<CompletionEntry> {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let line = &text[line_start..offset];
    let trimmed = line.trim_start();
    let indent = line.len() - trimmed.len();
    let parent = yaml_parent(text, line_start, indent);
    let Some(item) = trimmed.strip_prefix('-') else {
        if !(line.ends_with(':') || trimmed.is_empty()) {
            return Vec::new();
        }
        return match parent.as_deref() {
            Some("databaseChangeLog") => {
                yaml_entries_of_names(TOP_LEVEL.iter().copied(), "changelog")
            }
            Some("changes") => Vec::new(),
            Some(_) => CHANGE_SET_ATTRIBUTES
                .iter()
                .map(|attribute| CompletionEntry {
                    label: (*attribute).to_owned(),
                    detail: Some("changeSet".to_owned()),
                    insert_text: Some(format!("{attribute}: ")),
                    kind: CompletionKind::Property,
                    ..CompletionEntry::default()
                })
                .collect(),
            None => Vec::new(),
        };
    };
    if item.contains(':') && !item.trim_end().ends_with(':') {
        return Vec::new();
    }
    match parent.as_deref() {
        Some("databaseChangeLog") => yaml_entries_of_names(TOP_LEVEL.iter().copied(), "changelog"),
        Some("changes") => yaml_entries_of_names(CHANGES.iter().map(|(name, _)| *name), "change"),
        _ => Vec::new(),
    }
}

/// The names as the items of a YAML list, each one a key with its colon.
fn yaml_entries_of_names(
    names: impl Iterator<Item = &'static str>,
    detail: &str,
) -> Vec<CompletionEntry> {
    names
        .map(|name| CompletionEntry {
            label: name.to_owned(),
            detail: Some(detail.to_owned()),
            insert_text: Some(format!("- {name}:")),
            kind: CompletionKind::Keyword,
            ..CompletionEntry::default()
        })
        .collect()
}

/// The key of the line above the caret that holds it: the nearest line with a smaller
/// indent, with the dash of a list item and the colon of the key taken away.
fn yaml_parent(text: &str, line_start: usize, indent: usize) -> Option<String> {
    for line in text[..line_start].lines().rev() {
        let trimmed = line.trim_end();
        if trimmed.trim().is_empty() {
            continue;
        }
        let current = trimmed.len() - trimmed.trim_start().len();
        if current >= indent {
            continue;
        }
        let key = trimmed.trim_start().trim_start_matches("- ");
        return Some(key.trim_end_matches(':').trim().to_owned());
    }
    None
}

/// The entries of a change of a JSON changelog: the key with an empty change, which the
/// user fills with the attributes of the change.
fn json_entries(text: &str, offset: usize) -> Vec<CompletionEntry> {
    let offset = offset.min(text.len());
    let line_start = text[..offset].rfind('\n').map_or(0, |index| index + 1);
    let line = &text[line_start..offset];
    if !line.trim_start().starts_with('"') {
        return Vec::new();
    }
    CHANGES
        .iter()
        .map(|(name, _)| CompletionEntry {
            label: format!("\"{name}\""),
            detail: Some("change".to_owned()),
            insert_text: Some(format!("\"{name}\": {{}}")),
            kind: CompletionKind::Keyword,
            ..CompletionEntry::default()
        })
        .collect()
}

impl SyntaxProvider for MigrationFiles {
    fn grammar(&self) -> LanguageFn {
        // The editor colours these files with `file-syntax`; the provider exists for the
        // completion and the checks of a changelog.
        unsafe { LanguageFn::from_raw(no_grammar) }
    }

    fn queries(&self) -> &Queries {
        &NO_QUERIES
    }
}

unsafe extern "C" fn no_grammar() -> *const () {
    std::ptr::null()
}

const NO_QUERIES: Queries = Queries {
    highlights: "",
    folds: "",
    indents: "",
    locals: "",
};

impl LanguageSupport for MigrationFiles {
    fn id(&self) -> LanguageId {
        LANGUAGE_ID
    }

    fn matches(&self, path: &Path) -> bool {
        if !path.starts_with(&self.root) {
            return false;
        }
        let extension = path
            .extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "xml" | "yaml" | "yml" | "json" => {
                name.contains("changelog")
                    || path.components().any(|component| {
                        component
                            .as_os_str()
                            .to_string_lossy()
                            .eq_ignore_ascii_case("changelog")
                    })
            }
            "sql" => {
                name.starts_with("db.changelog")
                    || path.components().any(|component| {
                        let name = component.as_os_str().to_string_lossy();
                        name.eq_ignore_ascii_case("migration")
                            || name.eq_ignore_ascii_case("changelog")
                    })
            }
            _ => false,
        }
    }

    fn syntax(&self) -> &dyn SyntaxProvider {
        self
    }

    fn server(&self) -> Option<&dyn SemanticServer> {
        Some(self)
    }

    fn completion(&self) -> Option<&CompletionProvider> {
        Some(self)
    }

    fn diagnostics(&self) -> Option<&DiagnosticsProvider> {
        Some(self)
    }
}

const LANGUAGE_ID: LanguageId = LanguageId("migrations");

impl SemanticServer for MigrationFiles {
    fn open(&self, uri: &str, _version: i32, text: Arc<str>) {
        self.documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), text);
    }

    fn change(
        &self,
        uri: &str,
        _version: i32,
        text: Arc<str>,
        _edits: Vec<lang_api::DocumentEdit>,
    ) {
        self.documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(uri.to_owned(), text);
    }

    fn save(&self, _uri: &str) {}

    fn version(&self, uri: &str) -> Option<i32> {
        self.documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains_key(uri)
            .then_some(0)
    }

    fn close(&self, uri: &str) {
        self.documents
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .remove(uri);
    }
}

impl Provider<DocumentSpot, CompletionList> for MigrationFiles {
    fn start(&self, query: DocumentSpot, _: Cancel, mut reply: Reply<CompletionList>) {
        let entries = self.entries(&query);
        let _ = reply.send(Answer::new(
            CompletionList {
                entries,
                complete: true,
            },
            Confidence::Certain,
            SOURCE,
        ));
    }
}

impl Provider<DiagnosticsQuery, Vec<DiagnosticEntry>> for MigrationFiles {
    fn start(&self, query: DiagnosticsQuery, _: Cancel, mut reply: Reply<Vec<DiagnosticEntry>>) {
        let support = self.clone();
        thread::spawn(move || {
            let entries = support.diagnostics(&query.0);
            let _ = reply.send(Answer::new(entries, Confidence::Certain, SOURCE));
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn support() -> Arc<MigrationFiles> {
        MigrationFiles::new(PathBuf::from("C:/shop"))
    }

    fn completion(text: &str, line: u32, character: u32) -> Vec<CompletionEntry> {
        completion_of("db.changelog-master.xml", text, line, character)
    }

    fn completion_of(name: &str, text: &str, line: u32, character: u32) -> Vec<CompletionEntry> {
        let support = support();
        let uri = format!("file:///C:/shop/src/main/resources/db/changelog/{name}");
        support.open(&uri, 1, Arc::from(text));
        let query = DocumentSpot {
            uri,
            version: 1,
            point: lang_api::TextPoint { line, character },
        };
        support.entries(&query)
    }

    #[test]
    fn an_xml_changelog_completes_the_tags_of_a_change_set() {
        let text = "<databaseChangeLog>\n    <changeSet id=\"1\" author=\"bob\">\n        <\n";
        let entries = completion(text, 2, 9);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert!(labels.contains(&"createTable"), "{labels:?}");
        assert!(labels.contains(&"addForeignKeyConstraint"), "{labels:?}");
        let create = entries
            .iter()
            .find(|entry| entry.label == "createTable")
            .unwrap();
        assert_eq!(
            create.insert_text.as_deref(),
            Some("createTable tableName=\"\"")
        );
    }

    #[test]
    fn an_xml_change_set_completes_its_attributes() {
        let text = "<databaseChangeLog>\n    <changeSet \n";
        let entries = completion(text, 1, 15);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert!(labels.contains(&"id"), "{labels:?}");
        assert!(labels.contains(&"author"), "{labels:?}");
    }

    #[test]
    fn a_yaml_changelog_completes_the_keys_of_a_change() {
        let text = "databaseChangeLog:\n  - changeSet:\n      id: 1\n      author: bob\n      changes:\n        - \n";
        let entries = completion_of("db.changelog-master.yaml", text, 5, 10);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert!(labels.contains(&"createTable"), "{labels:?}");
        assert!(labels.contains(&"sql"), "{labels:?}");
        let create = entries
            .iter()
            .find(|entry| entry.label == "createTable")
            .unwrap();
        assert_eq!(
            create.insert_text.as_deref(),
            Some("- createTable:"),
            "a change is a YAML key, not the attributes of the XML form"
        );
    }

    #[test]
    fn the_top_of_a_yaml_changelog_completes_its_changesets_and_includes() {
        let text = "databaseChangeLog:\n  - \n";
        let entries = completion_of("db.changelog-master.yaml", text, 1, 4);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, ["changeSet", "include", "includeAll", "property"]);

        // Inside a changeSet the caret takes the keys of the attributes of the changeset.
        let text = "databaseChangeLog:\n  - changeSet:\n      \n";
        let entries = completion_of("db.changelog-master.yaml", text, 2, 6);
        let labels: Vec<&str> = entries.iter().map(|entry| entry.label.as_str()).collect();
        assert!(labels.contains(&"id"), "{labels:?}");
        assert!(labels.contains(&"author"), "{labels:?}");
    }

    #[test]
    fn a_json_changelog_keeps_the_shape_of_its_changes() {
        let text = "{\"databaseChangeLog\": [\n    {\"changeSet\": {\"id\": \"1\", \"changes\": [\n        \"\n";
        let entries = completion_of("db.changelog-master.json", text, 2, 9);
        let create = entries
            .iter()
            .find(|entry| entry.label == "\"createTable\"")
            .expect("the changes of a changeset");
        assert_eq!(create.insert_text.as_deref(), Some("\"createTable\": {}"));
    }

    #[test]
    fn a_repeated_changeset_of_the_open_changelog_is_a_warning() {
        let folder = test_support::TempDir::new("migrations-language");
        let root = folder.path();
        let path = root.join("db/changelog/db.changelog-master.xml");
        let support = MigrationFiles::new(root.to_path_buf());
        let uri = platform::CanonPath::new(&path).unwrap().to_uri();
        let text = "<databaseChangeLog>\n\
             <changeSet id=\"1\" author=\"bob\"/>\n\
             <changeSet id=\"1\" author=\"bob\"/>\n\
             </databaseChangeLog>";
        support.open(&uri, 1, Arc::from(text));

        let entries = support.diagnostics(&uri);

        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(
            entries[0].code.as_deref(),
            Some(java_project::DUPLICATE_CHANGESET)
        );
        assert_eq!(entries[0].span.start.line, 2);
    }

    #[test]
    fn an_include_of_the_open_changelog_that_names_no_file_is_a_warning() {
        let folder = test_support::TempDir::new("migrations-language-include");
        let root = folder.path();
        let path = root.join("db/changelog/db.changelog-master.xml");
        std::fs::create_dir_all(root.join("db/changelog")).unwrap();
        let support = MigrationFiles::new(root.to_path_buf());
        let uri = platform::CanonPath::new(&path).unwrap().to_uri();
        let text = "<databaseChangeLog>\n\
             <include file=\"gone.xml\" relativeToChangelogFile=\"true\"/>\n\
             </databaseChangeLog>";
        support.open(&uri, 1, Arc::from(text));

        let entries = support.diagnostics(&uri);

        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(
            entries[0].code.as_deref(),
            Some(java_project::MISSING_INCLUDE)
        );
    }

    #[test]
    fn only_the_changelogs_of_the_project_are_matched() {
        let support = support();
        assert!(support.matches(Path::new(
            "C:/shop/src/main/resources/db/changelog/db.changelog-master.xml"
        )));
        assert!(support.matches(Path::new(
            "C:/shop/src/main/resources/db/migration/V1__init.sql"
        )));
        assert!(!support.matches(Path::new("C:/shop/src/main/java/Shop.java")));
        assert!(!support.matches(Path::new("C:/other/db.changelog-master.xml")));
    }
}

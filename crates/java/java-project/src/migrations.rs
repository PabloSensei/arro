//! The database migrations of a project: the files Flyway and Liquibase run, the order
//! each tool gives them, the checks over their names and includes, the changelogs of
//! Liquibase in its four formats, and the state of those files against a history table.
//!
//! Nothing here talks to a database. [`Model::apply_history`] takes the rows of the
//! history table as the window of the database reads them.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use project_model::{Module, ProjectModel};

use crate::frameworks::{Framework, frameworks_of};

/// The folder Flyway reads when the configuration names none.
const FLYWAY_LOCATION: &str = "db/migration";
/// The changelog Liquibase runs when the configuration names none, without its extension.
const LIQUIBASE_CHANGELOG: &str = "db/changelog/db.changelog-master";
/// The extensions of a Liquibase changelog, in the order the default looks for them.
const CHANGELOG_EXTENSIONS: &[&str] = &["xml", "yaml", "yml", "json", "sql"];
/// The table Flyway keeps its history in when the configuration names none.
pub const FLYWAY_TABLE: &str = "flyway_schema_history";
/// The table Liquibase keeps its history in when the configuration names none.
pub const LIQUIBASE_TABLE: &str = "DATABASECHANGELOG";

/// The check of two files with one version.
pub const DUPLICATE_VERSION: &str = "flyway.duplicateVersion";
/// The check of a file name Flyway does not read.
pub const INVALID_NAME: &str = "flyway.invalidName";
/// The check of a file the database ran and the project changed.
pub const CHANGED_MIGRATION: &str = "flyway.changedMigration";
/// The check of two changesets of one changelog with one id and author.
pub const DUPLICATE_CHANGESET: &str = "liquibase.duplicateChangeSet";
/// The check of an include that names no file.
pub const MISSING_INCLUDE: &str = "liquibase.includeNotFound";

/// A tool that runs the migrations of a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Tool {
    Flyway,
    Liquibase,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Self::Flyway => "Flyway",
            Self::Liquibase => "Liquibase",
        }
    }

    /// The table this tool keeps its history in by default.
    pub fn table(self) -> &'static str {
        match self {
            Self::Flyway => FLYWAY_TABLE,
            Self::Liquibase => LIQUIBASE_TABLE,
        }
    }
}

/// What a row of the tree is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Baseline,
    Versioned,
    Undo,
    Repeatable,
    Changelog,
    ChangeSet,
    Include,
}

impl Kind {
    pub fn is_flyway(self) -> bool {
        matches!(
            self,
            Self::Baseline | Self::Versioned | Self::Undo | Self::Repeatable
        )
    }
}

/// The state of one file against the history table of a database.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum State {
    /// The database has no row for it.
    #[default]
    Pending,
    Applied,
    /// The database has a row and the run failed, or Liquibase left it running.
    Failed,
    /// The database ran another text of the same version: Flyway's checksum or the
    /// description of a repeatable migration differs.
    Changed,
}

impl State {
    pub fn glyph(self) -> &'static str {
        match self {
            Self::Pending => "○",
            Self::Applied => "✓",
            Self::Failed => "✗",
            Self::Changed => "≠",
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::Pending => "Pending",
            Self::Applied => "Applied",
            Self::Failed => "Failed",
            Self::Changed => "Checksum differs",
        }
    }
}

/// The actions the build tool of the project offers for a migration tool.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum ActionKind {
    Migrate,
    Info,
    Validate,
    Clean,
}

impl ActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Migrate => "Migrate",
            Self::Info => "Info",
            Self::Validate => "Validate",
            Self::Clean => "Clean",
        }
    }
}

/// One row of the node of the migrations in the tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    pub path: PathBuf,
    pub module: String,
    pub tool: Tool,
    pub kind: Kind,
    /// The version of a versioned file, empty for a repeatable one.
    pub version: String,
    pub description: String,
    /// The name the tree shows.
    pub label: String,
    /// How far the row is indented: a changeset or an include sits under its changelog.
    pub depth: usize,
    /// The changelog a row below one belongs to.
    pub parent: Option<PathBuf>,
    /// The file an include names.
    pub target: Option<PathBuf>,
    pub state: State,
}

/// A problem of one file of the migrations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Issue {
    pub path: PathBuf,
    /// The line, from one; zero when the format does not give one.
    pub line: usize,
    /// The name of the check in the settings.
    pub id: &'static str,
    pub message: String,
}

/// The statement that reads the history of one tool and the name of its table.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct History {
    pub tool: Tool,
    pub table: String,
    pub sql: String,
}

/// One action of the build tool and the reason it is not offered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Action {
    pub tool: Tool,
    pub kind: ActionKind,
    /// The line of goals of the build tool.
    pub goal: Option<String>,
    /// The task of the build tool.
    pub task: Option<String>,
    /// Set when the project cannot run the action.
    pub unavailable: Option<String>,
}

/// The file the next versioned migration goes into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Next {
    pub folder: PathBuf,
    /// The version after the last one of the project.
    pub version: String,
}

/// Everything the window shows about the migrations of a project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Model {
    pub tools: Vec<Tool>,
    pub files: Vec<File>,
    pub issues: Vec<Issue>,
    pub histories: Vec<History>,
    pub actions: Vec<Action>,
    /// The folders and changelogs the tools read, for the tree of the project.
    pub folders: Vec<PathBuf>,
    pub next: Option<Next>,
}

impl Model {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The first problem of one file, which the tree underlines.
    pub fn issue_of(&self, path: &Path) -> Option<&Issue> {
        self.issues.iter().find(|issue| issue.path == path)
    }

    /// The files of one tool, in the order the tree shows them.
    pub fn files_of(&self, tool: Tool) -> impl Iterator<Item = &File> {
        self.files.iter().filter(move |file| file.tool == tool)
    }

    /// Reads the rows of the history tables into the state of every file and adds the
    /// check of the files the database ran another text of.
    pub fn apply_history(&mut self, histories: &[HistoryRows]) {
        if histories.is_empty() {
            return;
        }
        let states = states(&self.files, histories);
        for file in &mut self.files {
            if let Some(state) = states.get(&file.path) {
                file.state = *state;
            }
        }
        self.issues.retain(|issue| issue.id != CHANGED_MIGRATION);
        for file in &self.files {
            if file.state == State::Changed {
                self.issues.push(Issue {
                    path: file.path.clone(),
                    line: 0,
                    id: CHANGED_MIGRATION,
                    message: format!("{} was changed after the database applied it", file.label),
                });
            }
        }
    }
}

/// The rows of one history table, as the window of the database reads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryRows {
    pub tool: Tool,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// The state of every file a history table names.
pub fn states(files: &[File], histories: &[HistoryRows]) -> HashMap<PathBuf, State> {
    let mut found = HashMap::new();
    for history in histories {
        match history.tool {
            Tool::Flyway => flyway_states(files, history, &mut found),
            Tool::Liquibase => liquibase_states(files, history, &mut found),
        }
    }
    found
}

fn column(history: &HistoryRows, name: &str) -> Option<usize> {
    history
        .columns
        .iter()
        .position(|column| column.eq_ignore_ascii_case(name))
}

fn cell(row: &[String], at: Option<usize>) -> &str {
    at.and_then(|at| row.get(at))
        .map(String::as_str)
        .unwrap_or_default()
        .trim()
}

fn flyway_states(files: &[File], history: &HistoryRows, found: &mut HashMap<PathBuf, State>) {
    let version = column(history, "version");
    let description = column(history, "description");
    let checksum = column(history, "checksum");
    let success = column(history, "success");
    for file in files.iter().filter(|file| file.tool == Tool::Flyway) {
        let Some(row) = history.rows.iter().find(|row| match file.kind {
            Kind::Versioned => {
                let applied = cell(row, version);
                same_version(&file.version, applied)
            }
            Kind::Repeatable => {
                cell(row, version).is_empty() && same_description(file, cell(row, description))
            }
            _ => false,
        }) else {
            continue;
        };
        let state = if !success_row(cell(row, success)) {
            State::Failed
        } else if checksum_differs(file, cell(row, checksum)) {
            State::Changed
        } else {
            State::Applied
        };
        found.insert(file.path.clone(), state);
    }
}

fn success_row(text: &str) -> bool {
    !matches!(
        text.to_ascii_lowercase().as_str(),
        "false" | "f" | "0" | "no"
    )
}

/// Whether the description of a history row is the one of the file: Flyway writes the
/// underscores of the file name as spaces.
fn same_description(file: &File, applied: &str) -> bool {
    applied.eq_ignore_ascii_case(&file.description.replace('_', " "))
}

fn checksum_differs(file: &File, applied: &str) -> bool {
    if applied.is_empty() {
        return false;
    }
    let Ok(applied) = applied.parse::<i64>() else {
        return false;
    };
    let Ok(text) = fs::read_to_string(&file.path) else {
        return false;
    };
    i64::from(checksum(&text)) != applied
}

fn liquibase_states(files: &[File], history: &HistoryRows, found: &mut HashMap<PathBuf, State>) {
    let id = column(history, "id");
    let author = column(history, "author");
    let filename = column(history, "filename");
    let exec_type = column(history, "exectype");
    for file in files
        .iter()
        .filter(|file| file.tool == Tool::Liquibase && file.kind == Kind::ChangeSet)
    {
        let Some(row) = history.rows.iter().find(|row| {
            cell(row, id) == change_set_id(file)
                && cell(row, author) == change_set_author(file)
                && same_file(&file.path, cell(row, filename))
        }) else {
            continue;
        };
        let state = if cell(row, exec_type).eq_ignore_ascii_case("FAILED")
            || cell(row, exec_type).eq_ignore_ascii_case("RUNNING")
        {
            State::Failed
        } else {
            State::Applied
        };
        found.insert(file.path.clone(), state);
    }
}

/// The id of a changeset row, which the label of the row carries as `id:author`.
fn change_set_id(file: &File) -> &str {
    file.label
        .split_once(':')
        .map_or(file.label.as_str(), |(id, _)| id)
}

fn change_set_author(file: &File) -> &str {
    file.label.split_once(':').map_or("", |(_, author)| author)
}

fn same_version(left: &str, right: &str) -> bool {
    match (
        MigrationVersion::parse(left),
        MigrationVersion::parse(right),
    ) {
        (Some(left), Some(right)) => left == right,
        _ => left == right,
    }
}

/// Whether the file a history row names is the changeset file: Liquibase writes the path
/// from the root of the search path, so the shorter of the two ends the longer one.
fn same_file(path: &Path, row: &str) -> bool {
    if row.is_empty() {
        return false;
    }
    let normalize = |text: &str| text.replace('\\', "/");
    let file = normalize(&path.to_string_lossy());
    let row = normalize(row.trim_start_matches("classpath:"));
    if row.is_empty() {
        return false;
    }
    file == row || file.ends_with(&format!("/{row}")) || row.ends_with(&format!("/{file}"))
}

/// The CRC32 Flyway stores as the checksum of a file: the lines without their end of
/// line, the byte order mark of the first line taken away, and the bytes of the lines one
/// after another. A file with no line checksums as zero.
pub fn checksum(text: &str) -> i32 {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut crc = 0xffff_ffffu32;
    for line in text.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        for byte in line.as_bytes() {
            crc ^= u32::from(*byte);
            for _ in 0..8 {
                let carry = crc & 1;
                crc >>= 1;
                if carry != 0 {
                    crc ^= 0xedb8_8320;
                }
            }
        }
    }
    !crc as i32
}

/// A Flyway version: the numeric parts of a number, compared part by part as Flyway
/// compares them, so that `1.10` comes after `1.9` and `1` is the version `1.0`.
#[derive(Clone, Debug)]
pub struct MigrationVersion {
    parts: Vec<u64>,
}

impl MigrationVersion {
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        if text.is_empty() {
            return None;
        }
        let mut parts = Vec::new();
        for part in text.split(['.', '_']) {
            parts.push(part.parse::<u64>().ok()?);
        }
        (!parts.is_empty()).then_some(Self { parts })
    }

    pub fn to_text(&self) -> String {
        self.parts
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }

    /// The text without the zero parts at its end, which name the same version as the
    /// shorter one: `1.0` is `1`.
    pub fn canonical(&self) -> String {
        let mut parts = self.parts.as_slice();
        while parts.len() > 1 && parts.last() == Some(&0) {
            parts = &parts[..parts.len() - 1];
        }
        parts
            .iter()
            .map(u64::to_string)
            .collect::<Vec<_>>()
            .join(".")
    }

    /// The version after this one: the last part counts up, so `1.1` becomes `1.2` and
    /// `1` becomes `2`.
    pub fn next(&self) -> Self {
        let mut parts = self.parts.clone();
        match parts.last_mut() {
            Some(number) => *number += 1,
            None => parts.push(1),
        }
        Self { parts }
    }
}

impl PartialEq for MigrationVersion {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == std::cmp::Ordering::Equal
    }
}

impl Eq for MigrationVersion {}

impl PartialOrd for MigrationVersion {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for MigrationVersion {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        let length = self.parts.len().max(other.parts.len());
        for index in 0..length {
            let left = self.parts.get(index).copied().unwrap_or(0);
            let right = other.parts.get(index).copied().unwrap_or(0);
            let order = left.cmp(&right);
            if order != std::cmp::Ordering::Equal {
                return order;
            }
        }
        std::cmp::Ordering::Equal
    }
}

/// Why Flyway does not read a file name.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    /// The name has no prefix of a kind, such as `init.sql`.
    Prefix,
    /// The name has no two underscores between its version and its description.
    Separator,
    /// The description is empty.
    Description,
    /// The text before the separator is not a version, or a repeatable name carries one.
    Version,
}

impl NameError {
    fn text(self) -> &'static str {
        match self {
            Self::Prefix => "the name does not begin with V, U, R or B",
            Self::Separator => "the version and the description need two underscores between them",
            Self::Description => "the description is empty",
            Self::Version => "the version is not a number, or a repeatable name carries one",
        }
    }
}

/// The name of one migration file, as Flyway reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Name {
    pub kind: Kind,
    pub version: Option<MigrationVersion>,
    pub description: String,
}

/// Reads the name of a Flyway file: `V1.1__init.sql`, `R__views.sql`, `U2__undo.sql`,
/// `B1__baseline.sql`.
pub fn parse_name(file: &str) -> Result<Name, NameError> {
    let stem = Path::new(file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or(file);
    let mut characters = stem.chars();
    let prefix = characters.next().ok_or(NameError::Prefix)?;
    let kind = match prefix {
        'V' | 'v' => Kind::Versioned,
        'U' | 'u' => Kind::Undo,
        'B' | 'b' => Kind::Baseline,
        'R' | 'r' => Kind::Repeatable,
        _ => return Err(NameError::Prefix),
    };
    let rest = characters.as_str();
    let (head, description) = rest.split_once("__").ok_or(NameError::Separator)?;
    if description.is_empty() {
        return Err(NameError::Description);
    }
    if kind == Kind::Repeatable {
        if !head.is_empty() {
            return Err(NameError::Version);
        }
        return Ok(Name {
            kind,
            version: None,
            description: description.to_owned(),
        });
    }
    let version = MigrationVersion::parse(head).ok_or(NameError::Version)?;
    Ok(Name {
        kind,
        version: Some(version),
        description: description.to_owned(),
    })
}

/// The name of the next versioned file from the description the user typed.
pub fn new_file_name(version: &str, description: &str) -> String {
    format!("V{version}__{}.sql", snake_case(description))
}

/// The description as a file name uses it: words in lower case, joined by underscores.
pub fn snake_case(description: &str) -> String {
    let mut name = String::with_capacity(description.len());
    let mut pending = false;
    for character in description.trim().chars() {
        if character.is_alphanumeric() {
            if pending && !name.is_empty() {
                name.push('_');
            }
            pending = false;
            for lower in character.to_lowercase() {
                name.push(lower);
            }
        } else {
            pending = true;
        }
    }
    if name.is_empty() {
        "migration".to_owned()
    } else {
        name
    }
}

/// The modules of the model, or one module at the root of a project without a model.
fn modules_of<'a>(
    root: &'a Path,
    model: Option<&'a ProjectModel>,
) -> Vec<(String, &'a Path, Vec<PathBuf>)> {
    let Some(model) = model else {
        return vec![(
            String::new(),
            root,
            vec![root.join("src").join("main").join("resources")],
        )];
    };
    model
        .modules
        .iter()
        .map(|module| {
            let resources = resource_roots(module);
            (module.id.clone(), module.dir.as_path(), resources)
        })
        .collect()
}

fn resource_roots(module: &Module) -> Vec<PathBuf> {
    let mut roots: Vec<PathBuf> = module
        .units
        .iter()
        .flat_map(|unit| unit.resource_roots.iter().cloned())
        .collect();
    if roots.is_empty() {
        roots.push(module.dir.join("src").join("main").join("resources"));
    }
    roots
}

/// The configuration of a module: the `application` files of its resources, the last
/// value of a key winning as Spring reads them.
fn settings_of(resources: &[PathBuf]) -> Vec<(String, String)> {
    let mut settings = Vec::new();
    for folder in resources {
        let Ok(entries) = fs::read_dir(folder) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        for path in files {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            let lower = name.to_ascii_lowercase();
            if !lower.starts_with("application") {
                continue;
            }
            let Ok(text) = fs::read_to_string(&path) else {
                continue;
            };
            if lower.ends_with(".properties") {
                properties_settings(&text, &mut settings);
            } else if lower.ends_with(".yml") || lower.ends_with(".yaml") {
                yaml_settings(&text, &mut settings);
            }
        }
    }
    settings
}

fn properties_settings(text: &str, settings: &mut Vec<(String, String)>) {
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        let Some((key, value)) = line.split_once(['=', ':']) else {
            continue;
        };
        settings.push((key.trim().to_owned(), value.trim().to_owned()));
    }
}

/// A flat reader of the YAML keys the configuration files need: the nesting of plain
/// `key:` lines is kept, list items are skipped.
fn yaml_settings(text: &str, settings: &mut Vec<(String, String)>) {
    let mut stack: Vec<(usize, String)> = Vec::new();
    for line in text.lines() {
        let without_comment = line.split('#').next().unwrap_or(line);
        let trimmed = without_comment.trim_end();
        if trimmed.trim().is_empty() || trimmed.trim_start().starts_with('-') {
            continue;
        }
        let indent = trimmed.len() - trimmed.trim_start().len();
        while stack.last().is_some_and(|(level, _)| *level >= indent) {
            stack.pop();
        }
        let Some((key, value)) = trimmed.trim_start().split_once(':') else {
            continue;
        };
        let key = key.trim();
        if key.is_empty() {
            continue;
        }
        let value = value.trim();
        let full = stack
            .iter()
            .map(|(_, key)| key.as_str())
            .chain(std::iter::once(key))
            .collect::<Vec<_>>()
            .join(".");
        if value.is_empty() {
            stack.push((indent, key.to_owned()));
        } else {
            settings.push((full, value.trim_matches(['"', '\'']).to_owned()));
        }
    }
}

fn setting<'a>(settings: &'a [(String, String)], key: &str) -> Option<&'a str> {
    settings
        .iter()
        .rev()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
}

/// The folders Flyway reads for one module: the default, or the locations of the
/// configuration.
fn flyway_locations(
    dir: &Path,
    resources: &[PathBuf],
    settings: &[(String, String)],
) -> Vec<PathBuf> {
    let configured = setting(settings, "spring.flyway.locations")
        .or_else(|| setting(settings, "flyway.locations"));
    let Some(configured) = configured else {
        return resources
            .iter()
            .map(|root| root.join(FLYWAY_LOCATION))
            .collect();
    };
    let mut folders = Vec::new();
    for location in configured.split(',') {
        let location = location.trim();
        if location.is_empty() {
            continue;
        }
        if let Some(rest) = location.strip_prefix("classpath:") {
            for root in resources {
                folders.push(root.join(rest));
            }
        } else if let Some(rest) = location.strip_prefix("filesystem:") {
            let path = Path::new(rest);
            folders.push(if path.is_absolute() {
                path.to_path_buf()
            } else {
                dir.join(path)
            });
        } else {
            let path = Path::new(location);
            folders.push(if path.is_absolute() {
                path.to_path_buf()
            } else {
                dir.join(path)
            });
        }
    }
    if folders.is_empty() {
        resources
            .iter()
            .map(|root| root.join(FLYWAY_LOCATION))
            .collect()
    } else {
        folders
    }
}

/// The changelog Liquibase runs for one module, from the configuration or the default.
fn liquibase_changelog(
    resources: &[PathBuf],
    settings: &[(String, String)],
) -> Option<(PathBuf, PathBuf)> {
    if let Some(configured) = setting(settings, "spring.liquibase.change-log") {
        let configured = configured.trim().trim_start_matches("classpath:");
        if !configured.is_empty() {
            let path = Path::new(configured);
            if path.is_absolute() {
                let folder = path.parent().unwrap_or(path).to_path_buf();
                return Some((path.to_path_buf(), folder));
            }
            for root in resources {
                let candidate = root.join(path);
                if candidate.is_file() {
                    return Some((candidate.clone(), root.clone()));
                }
            }
            let folder = path.parent().unwrap_or(path).to_path_buf();
            return Some((
                resources.first()?.join(path),
                resources.first()?.join(folder),
            ));
        }
    }
    for root in resources {
        for extension in CHANGELOG_EXTENSIONS {
            let candidate = root.join(format!("{LIQUIBASE_CHANGELOG}.{extension}"));
            if candidate.is_file() {
                return Some((candidate, root.clone()));
            }
        }
    }
    None
}

/// The kind of a build tool the project runs the plugin with.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Build {
    Pom,
    Gradle,
    None,
}

fn build_of(dir: &Path) -> (Build, Option<PathBuf>) {
    let pom = dir.join("pom.xml");
    if pom.is_file() {
        return (Build::Pom, Some(pom));
    }
    for name in ["build.gradle", "build.gradle.kts"] {
        let path = dir.join(name);
        if path.is_file() {
            return (Build::Gradle, Some(path));
        }
    }
    (Build::None, None)
}

const FLYWAY_PLUGIN: &[&str] = &["flyway-maven-plugin", "org.flywaydb.flyway"];
const LIQUIBASE_PLUGIN: &[&str] = &[
    "liquibase-maven-plugin",
    "org.liquibase.gradle",
    "liquibase-gradle-plugin",
];

fn has_plugin(build: &[u8], names: &[&str]) -> bool {
    let text = String::from_utf8_lossy(build);
    names.iter().any(|name| text.contains(name))
}

/// The actions the build tool of the project offers, and why the others do not run.
fn actions_of(dir: &Path, tools: &[Tool]) -> Vec<Action> {
    let (build, file) = build_of(dir);
    let text = file
        .as_deref()
        .and_then(|path| fs::read(path).ok())
        .unwrap_or_default();
    let mut actions = Vec::new();
    for tool in tools {
        let (names, goals, tasks) = match tool {
            Tool::Flyway => (
                FLYWAY_PLUGIN,
                [
                    "flyway:migrate",
                    "flyway:info",
                    "flyway:validate",
                    "flyway:clean",
                ],
                [
                    "flywayMigrate",
                    "flywayInfo",
                    "flywayValidate",
                    "flywayClean",
                ],
            ),
            Tool::Liquibase => (
                LIQUIBASE_PLUGIN,
                [
                    "liquibase:update",
                    "liquibase:status",
                    "liquibase:validate",
                    "liquibase:dropAll",
                ],
                ["update", "status", "validate", "dropAll"],
            ),
        };
        let missing = if !has_plugin(&text, names) {
            Some(format!(
                "{} is not a plugin of the build of this project",
                tool.name()
            ))
        } else if build == Build::None {
            Some("The project has no build file to run the plugin with".to_owned())
        } else {
            None
        };
        for (index, kind) in [
            ActionKind::Migrate,
            ActionKind::Info,
            ActionKind::Validate,
            ActionKind::Clean,
        ]
        .into_iter()
        .enumerate()
        {
            actions.push(Action {
                tool: *tool,
                kind,
                goal: (missing.is_none() && build == Build::Pom).then(|| goals[index].to_owned()),
                task: (missing.is_none() && build == Build::Gradle)
                    .then(|| tasks[index].to_owned()),
                unavailable: missing.clone(),
            });
        }
    }
    actions
}

/// Reads the migrations of the project: the files of Flyway, the changelogs of
/// Liquibase, the checks over them and the way the build tool runs the plugins.
pub fn scan(root: &Path, model: Option<&ProjectModel>) -> Model {
    let mut found = Model::default();
    let modules = modules_of(root, model);
    let mut detected: BTreeSet<Tool> = BTreeSet::new();
    let mut first_locations: Option<PathBuf> = None;
    let mut first_module = true;
    for (id, dir, resources) in modules {
        let frameworks = model
            .and_then(|model| model.dependencies_of(&id))
            .map_or(crate::frameworks::Frameworks::NONE, frameworks_of);
        let settings = settings_of(&resources);
        // A project without a model still shows the folders its tools read.
        let locations = flyway_locations(dir, &resources, &settings);
        let changelog = liquibase_changelog(&resources, &settings);
        let flyway = frameworks.contains(Framework::Flyway)
            || (model.is_none() && locations.iter().any(|folder| folder.is_dir()));
        let liquibase =
            frameworks.contains(Framework::Liquibase) || (model.is_none() && changelog.is_some());
        if flyway {
            detected.insert(Tool::Flyway);
            let folder = flyway_folder(&locations);
            if first_module {
                first_locations = Some(folder);
            }
            scan_flyway(&id, &locations, &mut found);
            found.histories.push(flyway_history(&settings));
            for folder in &locations {
                found.folders.push(folder.clone());
            }
        }
        if liquibase {
            detected.insert(Tool::Liquibase);
            if let Some((path, search_root)) = changelog {
                read_changelog(&id, &path, &search_root, &mut found);
                found.histories.push(liquibase_history(&settings));
                found.folders.push(path);
            }
        }
        if !detected.is_empty() {
            let tools: Vec<Tool> = detected.iter().copied().collect();
            let mut actions = actions_of(dir, &tools);
            if first_module {
                found.actions.append(&mut actions);
            }
        }
        first_module = false;
    }
    found.tools = detected.into_iter().collect();
    let now = |file: &File| match file.kind {
        Kind::Baseline => 0,
        Kind::Versioned => 1,
        Kind::Undo => 2,
        Kind::Repeatable => 3,
        Kind::Changelog => 0,
        Kind::ChangeSet => 1,
        Kind::Include => 2,
    };
    found.files.sort_by(|left, right| {
        let tool = left.tool.cmp(&right.tool);
        if tool != std::cmp::Ordering::Equal {
            return tool;
        }
        // Liquibase runs the changesets in the order their changelogs name them, which
        // is the order the walk found them.
        if left.tool == Tool::Liquibase {
            return std::cmp::Ordering::Equal;
        }
        now(left)
            .cmp(&now(right))
            .then_with(|| {
                let left_version = MigrationVersion::parse(&left.version);
                let right_version = MigrationVersion::parse(&right.version);
                left_version.cmp(&right_version)
            })
            .then_with(|| left.path.cmp(&right.path))
            .then_with(|| left.label.cmp(&right.label))
    });
    // Changesets and includes stay right below their changelog, whatever the sort of
    // the files themselves.
    found.files = in_tree_order(&found.files);
    found.next = first_locations.map(|folder| Next {
        folder,
        version: next_version(&found.files),
    });
    found
}

/// The rows in the order of the tree: every changelog, then the changesets and includes
/// it names, and the changelog of an include below the include itself.
fn in_tree_order(files: &[File]) -> Vec<File> {
    fn append(files: &[File], index: usize, taken: &mut [bool], rows: &mut Vec<File>) {
        if taken[index] {
            return;
        }
        taken[index] = true;
        rows.push(files[index].clone());
        if files[index].kind != Kind::Changelog {
            return;
        }
        let path = files[index].path.clone();
        let depth = files[index].depth;
        for child in 0..files.len() {
            let parent = files[child].parent.as_deref();
            if !taken[child] && parent == Some(path.as_path()) && files[child].depth > depth {
                append(files, child, taken, rows);
            }
        }
    }
    let mut taken = vec![false; files.len()];
    let mut rows = Vec::with_capacity(files.len());
    for index in 0..files.len() {
        if files[index].parent.is_none() {
            append(files, index, &mut taken, &mut rows);
        }
    }
    // A row whose changelog did not reach the tree, such as the changeset of an include
    // that was read before its changelog, keeps its place at the end.
    for index in 0..files.len() {
        append(files, index, &mut taken, &mut rows);
    }
    rows
}

/// The folder a new migration goes into: the first location that exists, or the first
/// one the tool would read.
fn flyway_folder(locations: &[PathBuf]) -> PathBuf {
    locations
        .iter()
        .find(|folder| folder.is_dir())
        .or_else(|| locations.first())
        .cloned()
        .unwrap_or_else(|| PathBuf::from(FLYWAY_LOCATION))
}

/// The version after the last one the files hold, or the first one of a project without
/// versioned files.
fn next_version(files: &[File]) -> String {
    let mut last: Option<MigrationVersion> = None;
    for file in files.iter().filter(|file| file.tool == Tool::Flyway) {
        if file.kind != Kind::Versioned {
            continue;
        }
        let Some(version) = MigrationVersion::parse(&file.version) else {
            continue;
        };
        if last.as_ref().is_none_or(|last| version > *last) {
            last = Some(version);
        }
    }
    last.map_or_else(|| "1".to_owned(), |version| version.next().to_text())
}

fn scan_flyway(module: &str, locations: &[PathBuf], found: &mut Model) {
    let mut by_version: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();
    for folder in locations {
        let Ok(entries) = fs::read_dir(folder) else {
            continue;
        };
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| path.is_file())
            .collect();
        files.sort();
        for path in files {
            let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
                continue;
            };
            if !name.to_ascii_lowercase().ends_with(".sql") {
                continue;
            }
            match parse_name(name) {
                Ok(parsed) => {
                    if parsed.kind == Kind::Versioned {
                        by_version
                            .entry(
                                parsed
                                    .version
                                    .as_ref()
                                    .map(MigrationVersion::canonical)
                                    .unwrap_or_default(),
                            )
                            .or_default()
                            .push(path.clone());
                    }
                    found.files.push(File {
                        path: path.clone(),
                        module: module.to_owned(),
                        tool: Tool::Flyway,
                        kind: parsed.kind,
                        version: parsed
                            .version
                            .as_ref()
                            .map(MigrationVersion::to_text)
                            .unwrap_or_default(),
                        description: parsed.description,
                        label: name.to_owned(),
                        depth: 0,
                        parent: None,
                        target: None,
                        state: State::Pending,
                    });
                }
                Err(error) => found.issues.push(Issue {
                    path: path.clone(),
                    line: 0,
                    id: INVALID_NAME,
                    message: format!("{name}: {}", error.text()),
                }),
            }
        }
    }
    for (version, paths) in by_version {
        if paths.len() < 2 {
            continue;
        }
        for path in paths {
            let label = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            found.issues.push(Issue {
                path,
                line: 0,
                id: DUPLICATE_VERSION,
                message: format!("Version {version} is not unique: {label}"),
            });
        }
    }
}

fn flyway_history(settings: &[(String, String)]) -> History {
    let table = setting(settings, "spring.flyway.table")
        .or_else(|| setting(settings, "flyway.table"))
        .map(str::trim)
        .filter(|table| !table.is_empty())
        .unwrap_or(FLYWAY_TABLE)
        .to_owned();
    History {
        tool: Tool::Flyway,
        sql: format!(
            "SELECT version, description, checksum, success FROM {table} ORDER BY installed_rank"
        ),
        table,
    }
}

fn liquibase_history(settings: &[(String, String)]) -> History {
    let table = setting(settings, "spring.liquibase.database-change-log-table")
        .map(str::trim)
        .filter(|table| !table.is_empty())
        .unwrap_or(LIQUIBASE_TABLE)
        .to_owned();
    History {
        tool: Tool::Liquibase,
        sql: format!(
            "SELECT ID, AUTHOR, FILENAME, MD5SUM, EXECTYPE FROM {table} \
             ORDER BY DATEEXECUTED, ORDEREXECUTED"
        ),
        table,
    }
}

/// The top of a changelog: the changesets and the includes it names, with the file of
/// each change.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Changelog {
    pub changesets: Vec<ChangeSet>,
    pub includes: Vec<Include>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangeSet {
    pub id: String,
    pub author: String,
    /// The line, from one; zero when the format does not give one.
    pub line: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Include {
    /// The file or folder as the changelog writes it.
    pub path: String,
    pub line: usize,
    pub all: bool,
    pub relative: bool,
}

fn read_changelog(module: &str, path: &Path, search_root: &Path, found: &mut Model) {
    let mut visited = BTreeSet::new();
    read_changelog_file(module, path, search_root, &mut visited, found, 0, None);
}

#[allow(clippy::too_many_arguments)]
fn read_changelog_file(
    module: &str,
    path: &Path,
    search_root: &Path,
    visited: &mut BTreeSet<PathBuf>,
    found: &mut Model,
    depth: usize,
    parent: Option<&Path>,
) {
    if !visited.insert(path.to_path_buf()) {
        return;
    }
    let Ok(text) = fs::read_to_string(path) else {
        found.issues.push(Issue {
            path: path.to_path_buf(),
            line: 0,
            id: MISSING_INCLUDE,
            message: format!("Cannot read the changelog {}", path.display()),
        });
        return;
    };
    let changelog = match parse_changelog(path, &text) {
        Ok(changelog) => changelog,
        Err(error) => {
            found.issues.push(Issue {
                path: path.to_path_buf(),
                line: 0,
                id: INVALID_NAME,
                message: error,
            });
            return;
        }
    };
    let label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    found.files.push(File {
        path: path.to_path_buf(),
        module: module.to_owned(),
        tool: Tool::Liquibase,
        kind: Kind::Changelog,
        version: String::new(),
        description: label.clone(),
        label,
        depth,
        parent: parent.map(Path::to_path_buf),
        target: None,
        state: State::Pending,
    });
    let mut seen: BTreeMap<(String, String), usize> = BTreeMap::new();
    for changeset in &changelog.changesets {
        let key = (changeset.id.clone(), changeset.author.clone());
        if let Some(first) = seen.insert(key, changeset.line) {
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
            found.issues.push(Issue {
                path: path.to_path_buf(),
                line: changeset.line,
                id: DUPLICATE_CHANGESET,
                message,
            });
        }
        found.files.push(File {
            path: path.to_path_buf(),
            module: module.to_owned(),
            tool: Tool::Liquibase,
            kind: Kind::ChangeSet,
            version: String::new(),
            description: format!("{}:{}", changeset.id, changeset.author),
            label: format!("{}:{}", changeset.id, changeset.author),
            depth: depth + 1,
            parent: Some(path.to_path_buf()),
            target: None,
            state: State::Pending,
        });
    }
    for include in &changelog.includes {
        let target = include_target(path, &include.path, include.relative, search_root);
        let exists = if include.all {
            target.is_dir()
        } else {
            target.is_file()
        };
        if !exists {
            found.issues.push(Issue {
                path: path.to_path_buf(),
                line: include.line,
                id: MISSING_INCLUDE,
                message: format!(
                    "{} names {}, which does not exist",
                    if include.all { "includeAll" } else { "include" },
                    target.display()
                ),
            });
        }
        let label = target
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| include.path.clone());
        found.files.push(File {
            path: path.to_path_buf(),
            module: module.to_owned(),
            tool: Tool::Liquibase,
            kind: Kind::Include,
            version: String::new(),
            description: include.path.clone(),
            label,
            depth: depth + 1,
            parent: Some(path.to_path_buf()),
            target: Some(target.clone()),
            state: State::Pending,
        });
        if !exists {
            continue;
        }
        if include.all {
            let mut children: Vec<PathBuf> = fs::read_dir(&target)
                .map(|entries| {
                    entries
                        .flatten()
                        .map(|entry| entry.path())
                        .filter(|path| path.is_file() && is_changelog(path))
                        .collect()
                })
                .unwrap_or_default();
            children.sort();
            for child in children {
                read_changelog_file(
                    module,
                    &child,
                    search_root,
                    visited,
                    found,
                    depth + 2,
                    Some(path),
                );
            }
        } else {
            read_changelog_file(
                module,
                &target,
                search_root,
                visited,
                found,
                depth + 2,
                Some(path),
            );
        }
    }
}

fn is_changelog(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            CHANGELOG_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

/// Where an include points: next to the changelog, or from the root of the search path.
fn include_target(from: &Path, path: &str, relative: bool, search_root: &Path) -> PathBuf {
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
    let from_root = search_root.join(path);
    if from_root.exists() {
        return from_root;
    }
    beside
}

/// Reads the changesets and the includes of a changelog of any of the four formats.
pub fn parse_changelog(path: &Path, text: &str) -> Result<Changelog, String> {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "xml" => parse_xml(text),
        "yaml" | "yml" => Ok(parse_yaml(text)),
        "json" => parse_json(text),
        "sql" => Ok(parse_sql(text)),
        _ => Err(format!("{} is not a changelog", path.display())),
    }
}

fn parse_xml(text: &str) -> Result<Changelog, String> {
    let document = roxmltree::Document::parse(text)
        .map_err(|error| format!("The changelog is not XML: {error}"))?;
    let root = document.root_element();
    if root.tag_name().name() != "databaseChangeLog" {
        return Err("The changelog has no databaseChangeLog element".to_owned());
    }
    let mut changelog = Changelog::default();
    for node in root.children().filter(|node| node.is_element()) {
        let line = document.text_pos_at(node.range().start).row as usize;
        match node.tag_name().name() {
            "changeSet" => changelog.changesets.push(ChangeSet {
                id: node.attribute("id").unwrap_or_default().to_owned(),
                author: node.attribute("author").unwrap_or_default().to_owned(),
                line,
            }),
            "include" => {
                let Some(path) = node.attribute("file") else {
                    continue;
                };
                changelog.includes.push(Include {
                    path: path.to_owned(),
                    line,
                    all: false,
                    relative: node
                        .attribute("relativeToChangelogFile")
                        .is_some_and(|value| value == "true"),
                });
            }
            "includeAll" => {
                let Some(path) = node.attribute("path") else {
                    continue;
                };
                changelog.includes.push(Include {
                    path: path.to_owned(),
                    line,
                    all: true,
                    relative: node
                        .attribute("relativeToChangelogFile")
                        .is_some_and(|value| value == "true"),
                });
            }
            _ => {}
        }
    }
    Ok(changelog)
}

fn parse_json(text: &str) -> Result<Changelog, String> {
    let value: serde_json::Value = serde_json::from_str(text)
        .map_err(|error| format!("The changelog is not JSON: {error}"))?;
    let Some(entries) = value.get("databaseChangeLog").and_then(|it| it.as_array()) else {
        return Err("The changelog has no databaseChangeLog list".to_owned());
    };
    let mut changelog = Changelog::default();
    for entry in entries {
        let Some(entry) = entry.as_object() else {
            continue;
        };
        if let Some(changeset) = entry.get("changeSet").and_then(|it| it.as_object()) {
            changelog.changesets.push(ChangeSet {
                id: scalar(changeset.get("id")),
                author: scalar(changeset.get("author")),
                line: 0,
            });
        }
        if let Some(include) = entry.get("include").and_then(|it| it.as_object()) {
            changelog.includes.push(Include {
                path: scalar(include.get("file")),
                line: 0,
                all: false,
                relative: include
                    .get("relativeToChangelogFile")
                    .and_then(|it| it.as_bool())
                    .unwrap_or(false),
            });
        }
        if let Some(include) = entry.get("includeAll").and_then(|it| it.as_object()) {
            changelog.includes.push(Include {
                path: scalar(include.get("path")),
                line: 0,
                all: true,
                relative: include
                    .get("relativeToChangelogFile")
                    .and_then(|it| it.as_bool())
                    .unwrap_or(false),
            });
        }
    }
    Ok(changelog)
}

fn scalar(value: Option<&serde_json::Value>) -> String {
    match value {
        Some(serde_json::Value::String(text)) => text.clone(),
        Some(serde_json::Value::Number(number)) => number.to_string(),
        Some(serde_json::Value::Bool(flag)) => flag.to_string(),
        _ => String::new(),
    }
}

/// The markers of a formatted SQL changelog.
fn parse_sql(text: &str) -> Changelog {
    let mut changelog = Changelog::default();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        let Some(comment) = line.strip_prefix("--") else {
            continue;
        };
        let comment = comment.trim();
        if let Some(rest) = comment.strip_prefix("changeset") {
            let rest = rest.trim();
            let (author, rest) = rest.split_once(':').unwrap_or(("", rest));
            let id = rest.split_whitespace().next().unwrap_or_default();
            changelog.changesets.push(ChangeSet {
                id: id.trim().to_owned(),
                author: author.trim().to_owned(),
                line: index + 1,
            });
        } else if let Some(rest) = comment.strip_prefix("includeAll") {
            changelog.includes.push(Include {
                path: attribute(rest, "path"),
                line: index + 1,
                all: true,
                relative: attribute(rest, "relativeToChangelogFile") == "true",
            });
        } else if let Some(rest) = comment.strip_prefix("include") {
            changelog.includes.push(Include {
                path: attribute(rest, "file"),
                line: index + 1,
                all: false,
                relative: attribute(rest, "relativeToChangelogFile") == "true",
            });
        }
    }
    changelog
}

/// The value of `name:value` or `name=value` in the attributes of a formatted SQL marker.
fn attribute(text: &str, name: &str) -> String {
    for word in text.split_whitespace() {
        let Some((key, value)) = word.split_once([':', '=']) else {
            continue;
        };
        if key.eq_ignore_ascii_case(name) {
            return value.trim_matches(['"', '\'']).to_owned();
        }
    }
    String::new()
}

/// A reader of the part of YAML a changelog uses: block mappings and block sequences
/// with plain scalars. Flows, anchors and multi-line scalars are left to the tool.
fn parse_yaml(text: &str) -> Changelog {
    let lines: Vec<YamlLine> = text
        .lines()
        .enumerate()
        .filter_map(|(index, line)| {
            let without_comment = strip_yaml_comment(line);
            let trimmed = without_comment.trim_end();
            if trimmed.trim().is_empty() || trimmed.trim_start() == "---" {
                return None;
            }
            let indent = trimmed.len() - trimmed.trim_start().len();
            Some(YamlLine {
                indent,
                text: trimmed.trim_start().to_owned(),
                line: index + 1,
            })
        })
        .collect();
    let mut changelog = Changelog::default();
    if lines.is_empty() {
        return changelog;
    }
    let mut at = 0;
    // The top of the file is the `databaseChangeLog:` key with the list under it.
    while at < lines.len() {
        let line = &lines[at];
        if line.indent == 0 && line.text.starts_with("databaseChangeLog") {
            at += 1;
            break;
        }
        at += 1;
    }
    let Some(child_indent) = lines.get(at).map(|line| line.indent) else {
        return changelog;
    };
    while at < lines.len() {
        let line = &lines[at];
        if line.indent != child_indent || !line.text.starts_with('-') {
            at += 1;
            continue;
        }
        let content = line.text[1..].trim_start();
        let (key, value) = split_yaml_key(content);
        let line_number = line.line;
        if key == "changeSet" {
            let mut changeset = ChangeSet {
                id: String::new(),
                author: String::new(),
                line: line_number,
            };
            if !value.is_empty() {
                at += 1;
            } else {
                at += 1;
                at = yaml_keys(&lines, at, line.indent, |name, value| match name {
                    "id" => changeset.id = value,
                    "author" => changeset.author = value,
                    _ => {}
                });
            }
            changelog.changesets.push(changeset);
        } else if key == "include" || key == "includeAll" {
            let mut include = Include {
                path: String::new(),
                line: line_number,
                all: key == "includeAll",
                relative: false,
            };
            at += 1;
            at = yaml_keys(&lines, at, line.indent, |name, value| match name {
                "file" | "path" => include.path = value,
                "relativeToChangelogFile" => include.relative = value == "true",
                _ => {}
            });
            changelog.includes.push(include);
        } else {
            at += 1;
        }
    }
    changelog
}

struct YamlLine {
    indent: usize,
    text: String,
    line: usize,
}

fn strip_yaml_comment(line: &str) -> &str {
    let mut quote = None;
    for (index, character) in line.char_indices() {
        match (quote, character) {
            (None, '"') | (None, '\'') => quote = Some(character),
            (Some(open), found) if open == found => quote = None,
            (None, '#') if index == 0 || line[..index].ends_with(char::is_whitespace) => {
                return &line[..index];
            }
            _ => {}
        }
    }
    line
}

fn split_yaml_key(text: &str) -> (String, String) {
    let Some((key, value)) = text.split_once(':') else {
        return (text.trim().to_owned(), String::new());
    };
    (
        key.trim().to_owned(),
        value.trim().trim_matches(['"', '\'']).to_owned(),
    )
}

/// Reads the keys of a mapping that starts after `at`, deeper than `parent`, and hands
/// each `name: value` pair to `found`. Returns the line after the mapping.
fn yaml_keys(
    lines: &[YamlLine],
    mut at: usize,
    parent: usize,
    mut found: impl FnMut(&str, String),
) -> usize {
    let Some(indent) = lines
        .get(at)
        .map(|line| line.indent)
        .filter(|it| *it > parent)
    else {
        return at;
    };
    while at < lines.len() {
        let line = &lines[at];
        if line.indent != indent || line.text.starts_with('-') {
            break;
        }
        let (key, value) = split_yaml_key(&line.text);
        if !key.is_empty() {
            found(&key, value);
        }
        at += 1;
    }
    at
}

#[cfg(test)]
mod tests {
    use super::*;
    use project_model::{Dependency, DependencyScope, ModuleDependencies, Unit};

    fn write(path: &Path, text: &str) {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(path, text).unwrap();
    }

    fn model_with(root: &Path, coordinates: &[&str]) -> ProjectModel {
        let dependencies: Vec<Dependency> = coordinates
            .iter()
            .map(|coordinates| Dependency {
                coordinates: (*coordinates).to_owned(),
                ..Dependency::default()
            })
            .collect();
        ProjectModel {
            root: root.to_path_buf(),
            modules: vec![Module {
                id: "app".into(),
                name: "app".into(),
                dir: root.to_path_buf(),
                units: vec![Unit {
                    name: "main".into(),
                    resource_roots: vec![root.join("src/main/resources")],
                    ..Unit::default()
                }],
                ..Module::default()
            }],
            dependencies: vec![ModuleDependencies {
                module: "app".into(),
                scopes: vec![DependencyScope {
                    name: "compile".into(),
                    roots: dependencies,
                }],
                ..ModuleDependencies::default()
            }],
            resolved: true,
            ..ProjectModel::default()
        }
    }

    #[test]
    fn versions_compare_by_number_and_repeatables_come_last() {
        let mut versions: Vec<MigrationVersion> = ["1", "1.1", "1.10", "2", "1.2"]
            .iter()
            .map(|text| MigrationVersion::parse(text).unwrap())
            .collect();
        versions.sort();
        let text: Vec<String> = versions.iter().map(MigrationVersion::to_text).collect();
        assert_eq!(text, ["1", "1.1", "1.2", "1.10", "2"]);
        // Flyway reads `1` and `1.0` as one version.
        assert_eq!(
            MigrationVersion::parse("1").unwrap(),
            MigrationVersion::parse("1.0").unwrap()
        );
        assert_eq!(MigrationVersion::parse("1.0").unwrap().canonical(), "1");
        assert_eq!(
            MigrationVersion::parse("1.10").unwrap().next().to_text(),
            "1.11"
        );
        assert_eq!(MigrationVersion::parse("2").unwrap().next().to_text(), "3");
        // A repeatable migration has no version and sorts after every numbered one.
        let repeatable = parse_name("R__views.sql").unwrap();
        assert_eq!(repeatable.kind, Kind::Repeatable);
        assert_eq!(repeatable.version, None);
    }

    #[test]
    fn the_name_of_a_file_gives_its_kind_version_and_description() {
        let versioned = parse_name("V1.1__add_owner.sql").unwrap();
        assert_eq!(versioned.kind, Kind::Versioned);
        assert_eq!(versioned.version.unwrap().to_text(), "1.1");
        assert_eq!(versioned.description, "add_owner");
        let undo = parse_name("U2__undo_owner.sql").unwrap();
        assert_eq!(undo.kind, Kind::Undo);
        assert_eq!(parse_name("B1__baseline.sql").unwrap().kind, Kind::Baseline);
        assert_eq!(
            parse_name("V1_init.sql"),
            Err(NameError::Separator),
            "one underscore is not the separator"
        );
        assert_eq!(parse_name("init.sql"), Err(NameError::Prefix));
        assert_eq!(parse_name("V1__.sql"), Err(NameError::Description));
        assert_eq!(parse_name("Vx__x.sql"), Err(NameError::Version));
        assert_eq!(parse_name("R1__x.sql"), Err(NameError::Version));
    }

    #[test]
    fn the_next_version_counts_up_and_the_description_becomes_snake_case() {
        let folder = test_support::TempDir::new("migrations-next");
        let root = folder.path();
        write(
            &root.join("src/main/resources/db/migration/V1__init.sql"),
            "",
        );
        write(
            &root.join("src/main/resources/db/migration/V1.2__owners.sql"),
            "",
        );
        write(
            &root.join("src/main/resources/db/migration/R__views.sql"),
            "",
        );
        let model = model_with(root, &["org.flywaydb:flyway-core:10.20.0"]);

        let found = scan(root, Some(&model));

        let next = found.next.expect("the project can take a migration");
        assert_eq!(next.version, "1.3");
        assert_eq!(next.folder, root.join("src/main/resources/db/migration"));
        assert_eq!(
            new_file_name("1.3", "Add Pet Owners"),
            "V1.3__add_pet_owners.sql"
        );
        assert_eq!(snake_case("  Add  pet-owners  "), "add_pet_owners");
        assert_eq!(snake_case(""), "migration");
    }

    #[test]
    fn a_project_without_migrations_starts_at_the_first_version() {
        let folder = test_support::TempDir::new("migrations-first");
        let root = folder.path();
        let model = model_with(root, &["org.flywaydb:flyway-core:10.20.0"]);
        let found = scan(root, Some(&model));
        assert_eq!(found.next.unwrap().version, "1");
    }

    fn flyway_model(files: &[(&str, &str)]) -> (test_support::TempDir, Model) {
        let folder = test_support::TempDir::new("migrations-flyway");
        let root = folder.path();
        let resources = root.join("src/main/resources");
        let model = model_with(root, &["org.flywaydb:flyway-core:10.20.0"]);
        for (name, text) in files {
            write(&resources.join("db/migration").join(name), text);
        }
        let found = scan(root, Some(&model));
        (folder, found)
    }

    #[test]
    fn the_files_come_in_the_order_flyway_runs_them() {
        let (_folder, found) = flyway_model(&[
            ("R__views.sql", "CREATE VIEW v AS SELECT 1;"),
            ("V2__owners.sql", "CREATE TABLE owners (id INT);"),
            ("V1.10__later.sql", ""),
            ("V1.1__early.sql", ""),
            ("U2__undo.sql", ""),
            ("V1__init.sql", ""),
        ]);
        let order: Vec<&str> = found.files.iter().map(|file| file.label.as_str()).collect();
        assert_eq!(
            order,
            [
                "V1__init.sql",
                "V1.1__early.sql",
                "V1.10__later.sql",
                "V2__owners.sql",
                "U2__undo.sql",
                "R__views.sql",
            ]
        );
        assert!(found.issues.is_empty(), "{:?}", found.issues);
    }

    #[test]
    fn a_repeated_version_is_reported_on_every_file_of_it() {
        let (_folder, found) = flyway_model(&[("V1__init.sql", ""), ("V1__owners.sql", "")]);
        let issues: Vec<&Issue> = found
            .issues
            .iter()
            .filter(|issue| issue.id == DUPLICATE_VERSION)
            .collect();
        assert_eq!(issues.len(), 2, "{:?}", found.issues);
        assert!(issues[0].message.contains("Version 1"));
    }

    #[test]
    fn a_file_without_the_double_underscore_is_reported() {
        let (_folder, found) = flyway_model(&[("V1_init.sql", "")]);
        assert!(
            found.issues.iter().any(|issue| issue.id == INVALID_NAME),
            "{:?}",
            found.issues
        );
        assert!(found.files.is_empty());
    }

    #[test]
    fn versions_that_differ_in_their_zero_parts_are_the_same_version() {
        let (_folder, found) = flyway_model(&[("V1__init.sql", ""), ("V1.0__owners.sql", "")]);
        let issues = found
            .issues
            .iter()
            .filter(|issue| issue.id == DUPLICATE_VERSION)
            .count();
        assert_eq!(issues, 2, "{:?}", found.issues);
    }

    #[test]
    fn a_gap_between_versions_is_not_a_problem() {
        let (_folder, found) = flyway_model(&[("V1__init.sql", ""), ("V4__later.sql", "")]);
        assert!(found.issues.is_empty(), "{:?}", found.issues);
        assert_eq!(found.next.unwrap().version, "5");
    }

    #[test]
    fn the_locations_of_the_configuration_replace_the_default_folder() {
        let folder = test_support::TempDir::new("migrations-locations");
        let root = folder.path();
        let resources = root.join("src/main/resources");
        write(
            &resources.join("application.properties"),
            "spring.flyway.locations=classpath:db/scripts,classpath:db/extra\n\
             spring.flyway.table=history\n",
        );
        write(&resources.join("db/scripts/V1__init.sql"), "");
        write(&resources.join("db/extra/V2__more.sql"), "");
        write(&resources.join("db/migration/V9__ignored.sql"), "");
        let model = model_with(root, &["org.flywaydb:flyway-core:10.20.0"]);

        let found = scan(root, Some(&model));

        let labels: Vec<&str> = found.files.iter().map(|file| file.label.as_str()).collect();
        assert_eq!(labels, ["V1__init.sql", "V2__more.sql"]);
        assert_eq!(found.histories[0].table, "history");
        assert!(found.histories[0].sql.contains("FROM history"));
    }

    #[test]
    fn a_yaml_configuration_names_the_locations_and_the_table() {
        let folder = test_support::TempDir::new("migrations-yaml-config");
        let root = folder.path();
        let resources = root.join("src/main/resources");
        write(
            &resources.join("application.yml"),
            "spring:\n  flyway:\n    locations: classpath:db/move\n    table: flyway_log\n",
        );
        write(&resources.join("db/move/V1__init.sql"), "");
        let model = model_with(root, &["org.flywaydb:flyway-core:10.20.0"]);

        let found = scan(root, Some(&model));

        assert_eq!(found.files.len(), 1);
        assert_eq!(found.histories[0].table, "flyway_log");
    }

    #[test]
    fn a_pom_without_the_plugin_has_no_actions() {
        let folder = test_support::TempDir::new("migrations-no-plugin");
        let root = folder.path();
        write(&root.join("pom.xml"), "<project></project>");
        let model = model_with(root, &["org.flywaydb:flyway-core:10.20.0"]);
        let found = scan(root, Some(&model));
        assert_eq!(found.actions.len(), 4);
        assert!(
            found
                .actions
                .iter()
                .all(|action| action.unavailable.is_some())
        );
    }

    #[test]
    fn the_plugin_of_a_pom_gives_the_goals_of_the_actions() {
        let folder = test_support::TempDir::new("migrations-plugin");
        let root = folder.path();
        write(
            &root.join("pom.xml"),
            "<project><build><plugins><plugin>\
             <artifactId>flyway-maven-plugin</artifactId>\
             </plugin></plugins></build></project>",
        );
        let model = model_with(root, &["org.flywaydb:flyway-core:10.20.0"]);

        let found = scan(root, Some(&model));

        let migrate = found
            .actions
            .iter()
            .find(|action| action.kind == ActionKind::Migrate)
            .unwrap();
        assert_eq!(migrate.goal.as_deref(), Some("flyway:migrate"));
        assert_eq!(migrate.task, None);
        assert!(migrate.unavailable.is_none());
    }

    #[test]
    fn the_plugin_of_a_gradle_build_gives_the_tasks_of_the_actions() {
        let folder = test_support::TempDir::new("migrations-gradle-plugin");
        let root = folder.path();
        write(
            &root.join("build.gradle"),
            "plugins { id 'org.flywaydb.flyway' version '10.20.0' }\n",
        );
        let model = model_with(root, &["org.flywaydb:flyway-core:10.20.0"]);

        let found = scan(root, Some(&model));

        let clean = found
            .actions
            .iter()
            .find(|action| action.kind == ActionKind::Clean)
            .unwrap();
        assert_eq!(clean.task.as_deref(), Some("flywayClean"));
    }

    #[test]
    fn the_xml_changelog_gives_its_changesets_and_its_includes() {
        let text = r#"<?xml version="1.0" encoding="UTF-8"?>
<databaseChangeLog xmlns="http://www.liquibase.org/xml/ns/dbchangelog">
    <changeSet id="1" author="bob">
        <createTable tableName="owners"/>
    </changeSet>
    <include file="db/changelog/owners.xml"/>
    <includeAll path="db/changelog/extra" relativeToChangelogFile="true"/>
</databaseChangeLog>"#;
        let changelog = parse_changelog(Path::new("db.changelog-master.xml"), text).unwrap();
        assert_eq!(changelog.changesets.len(), 1);
        assert_eq!(changelog.changesets[0].id, "1");
        assert_eq!(changelog.changesets[0].author, "bob");
        assert_eq!(changelog.changesets[0].line, 3);
        assert_eq!(changelog.includes.len(), 2);
        assert!(!changelog.includes[0].all);
        assert_eq!(changelog.includes[0].path, "db/changelog/owners.xml");
        assert!(changelog.includes[1].all);
        assert!(changelog.includes[1].relative);
    }

    #[test]
    fn the_yaml_changelog_gives_its_changesets_and_its_includes() {
        let text = "\
databaseChangeLog:
  - changeSet:
      id: 1
      author: bob
      changes:
        - createTable:
            tableName: owners
  - include:
      file: db/changelog/owners.yaml
      relativeToChangelogFile: true
  - includeAll:
      path: extra/
";
        let changelog = parse_changelog(Path::new("db.changelog-master.yaml"), text).unwrap();
        assert_eq!(changelog.changesets.len(), 1);
        assert_eq!(changelog.changesets[0].id, "1");
        assert_eq!(changelog.changesets[0].author, "bob");
        assert_eq!(changelog.includes.len(), 2);
        assert_eq!(changelog.includes[0].path, "db/changelog/owners.yaml");
        assert!(changelog.includes[0].relative);
        assert!(changelog.includes[1].all);
        assert_eq!(changelog.includes[1].path, "extra/");
    }

    #[test]
    fn the_json_changelog_gives_its_changesets_and_its_includes() {
        let text = r#"{"databaseChangeLog": [
            {"changeSet": {"id": "1", "author": "bob", "changes": []}},
            {"include": {"file": "owners.json", "relativeToChangelogFile": true}}
        ]}"#;
        let changelog = parse_changelog(Path::new("db.changelog-master.json"), text).unwrap();
        assert_eq!(changelog.changesets.len(), 1);
        assert_eq!(changelog.changesets[0].id, "1");
        assert_eq!(changelog.includes.len(), 1);
        assert!(changelog.includes[0].relative);
    }

    #[test]
    fn the_formatted_sql_changelog_gives_its_changesets_and_includes() {
        let text = "\
--liquibase formatted sql
--changeset bob:1
CREATE TABLE owners (id INT);
--changeset alice:2 runOnChange:true
CREATE TABLE vets (id INT);
--include file:sql/more.sql relativeToChangelogFile:true
--includeAll path:sql/extra/ relativeToChangelogFile:true
";
        let changelog = parse_changelog(Path::new("db.changelog-master.sql"), text).unwrap();
        assert_eq!(changelog.changesets.len(), 2);
        assert_eq!(changelog.changesets[1].author, "alice");
        assert_eq!(changelog.changesets[1].line, 4);
        assert_eq!(changelog.includes.len(), 2);
        assert!(!changelog.includes[0].all);
        assert_eq!(changelog.includes[0].path, "sql/more.sql");
        assert!(changelog.includes[1].all);
    }

    fn liquibase_project(files: &[(&str, &str)]) -> (test_support::TempDir, Model) {
        let folder = test_support::TempDir::new("migrations-liquibase");
        let root = folder.path();
        let resources = root.join("src/main/resources");
        let model = model_with(root, &["org.liquibase:liquibase-core:4.29.0"]);
        for (name, text) in files {
            write(&resources.join(name), text);
        }
        let found = scan(root, Some(&model));
        (folder, found)
    }

    #[test]
    fn an_include_is_followed_and_a_missing_one_is_reported() {
        let (_folder, found) = liquibase_project(&[
            (
                "db/changelog/db.changelog-master.xml",
                "<databaseChangeLog>\
                 <changeSet id=\"1\" author=\"bob\"/>\
                 <include file=\"owners.xml\" relativeToChangelogFile=\"true\"/>\
                 <include file=\"nope.xml\" relativeToChangelogFile=\"true\"/>\
                 </databaseChangeLog>",
            ),
            (
                "db/changelog/owners.xml",
                "<databaseChangeLog><changeSet id=\"2\" author=\"bob\"/></databaseChangeLog>",
            ),
        ]);
        let labels: Vec<&str> = found.files.iter().map(|file| file.label.as_str()).collect();
        assert_eq!(
            labels,
            [
                "db.changelog-master.xml",
                "1:bob",
                "owners.xml",
                "owners.xml",
                "2:bob",
                "nope.xml",
            ]
        );
        let missing = found
            .issues
            .iter()
            .filter(|issue| issue.id == MISSING_INCLUDE)
            .count();
        assert_eq!(missing, 1, "{:?}", found.issues);
        assert_eq!(found.next, None, "Liquibase takes no versioned names");
    }

    #[test]
    fn include_all_reads_every_changelog_of_the_folder() {
        let (_folder, found) = liquibase_project(&[
            (
                "db/changelog/db.changelog-master.yaml",
                "databaseChangeLog:\n  - includeAll:\n      path: extra\n",
            ),
            (
                "db/changelog/extra/a.yaml",
                "databaseChangeLog:\n  - changeSet:\n      id: a\n      author: bob\n",
            ),
            (
                "db/changelog/extra/b.yaml",
                "databaseChangeLog:\n  - changeSet:\n      id: b\n      author: alice\n",
            ),
            ("db/changelog/extra/notes.txt", "not a changelog"),
        ]);
        let changesets: Vec<&str> = found
            .files
            .iter()
            .filter(|file| file.kind == Kind::ChangeSet)
            .map(|file| file.label.as_str())
            .collect();
        assert_eq!(changesets, ["a:bob", "b:alice"]);
        assert!(found.issues.is_empty(), "{:?}", found.issues);
    }

    #[test]
    fn a_changelog_of_an_include_sits_under_the_include() {
        let (_folder, found) = liquibase_project(&[
            (
                "db/changelog/db.changelog-master.xml",
                "<databaseChangeLog>\
                 <changeSet id=\"1\" author=\"bob\"/>\
                 <include file=\"owners.xml\" relativeToChangelogFile=\"true\"/>\
                 </databaseChangeLog>",
            ),
            (
                "db/changelog/owners.xml",
                "<databaseChangeLog><changeSet id=\"2\" author=\"bob\"/></databaseChangeLog>",
            ),
        ]);
        let rows: Vec<(&str, usize)> = found
            .files
            .iter()
            .map(|file| (file.label.as_str(), file.depth))
            .collect();
        assert_eq!(
            rows,
            [
                ("db.changelog-master.xml", 0),
                ("1:bob", 1),
                ("owners.xml", 1),
                ("owners.xml", 2),
                ("2:bob", 3),
            ]
        );
        let master = found.files[0].path.clone();
        let included = found
            .files
            .iter()
            .find(|file| file.kind == Kind::Changelog && file.depth == 2)
            .unwrap();
        assert_eq!(included.parent.as_deref(), Some(master.as_path()));
    }

    #[test]
    fn a_repeated_changeset_of_one_file_is_reported() {
        let (_folder, found) = liquibase_project(&[(
            "db/changelog/db.changelog-master.xml",
            "<databaseChangeLog>\
             <changeSet id=\"1\" author=\"bob\"/>\
             <changeSet id=\"1\" author=\"bob\"/>\
             </databaseChangeLog>",
        )]);
        let issues = found
            .issues
            .iter()
            .filter(|issue| issue.id == DUPLICATE_CHANGESET)
            .count();
        assert_eq!(issues, 1, "{:?}", found.issues);
    }

    #[test]
    fn a_changelog_of_the_configuration_is_read_instead_of_the_default() {
        let folder = test_support::TempDir::new("migrations-changelog-config");
        let root = folder.path();
        let resources = root.join("src/main/resources");
        write(
            &resources.join("application.properties"),
            "spring.liquibase.change-log=classpath:db/own.xml\n",
        );
        write(
            &resources.join("db/own.xml"),
            "<databaseChangeLog><changeSet id=\"7\" author=\"ann\"/></databaseChangeLog>",
        );
        write(
            &resources.join("db/changelog/db.changelog-master.xml"),
            "<databaseChangeLog/>",
        );
        let model = model_with(root, &["org.liquibase:liquibase-core:4.29.0"]);

        let found = scan(root, Some(&model));

        assert!(found.files.iter().any(|file| file.label == "own.xml"));
        assert!(
            !found
                .files
                .iter()
                .any(|file| file.label == "db.changelog-master.xml")
        );
    }

    fn flyway_file(name: &str, version: &str, description: &str, path: &Path) -> File {
        File {
            path: path.to_path_buf(),
            module: "app".into(),
            tool: Tool::Flyway,
            kind: if version.is_empty() {
                Kind::Repeatable
            } else {
                Kind::Versioned
            },
            version: version.to_owned(),
            description: description.to_owned(),
            label: name.to_owned(),
            depth: 0,
            parent: None,
            target: None,
            state: State::Pending,
        }
    }

    #[test]
    fn the_history_says_which_migrations_are_applied_pending_or_failed() {
        let folder = test_support::TempDir::new("migrations-history");
        let root = folder.path();
        let applied = root.join("V1__init.sql");
        write(&applied, "CREATE TABLE owners (id INT);\n");
        let pending = root.join("V2__owners.sql");
        write(&pending, "CREATE TABLE vets (id INT);\n");
        let failed = root.join("V3__bad.sql");
        write(&failed, "NOT SQL;\n");
        let changed = root.join("V4__changed.sql");
        write(&changed, "CREATE TABLE pets (id INT);\n");
        let files = vec![
            flyway_file("V1__init.sql", "1", "init", &applied),
            flyway_file("V2__owners.sql", "2", "owners", &pending),
            flyway_file("V3__bad.sql", "3", "bad", &failed),
            flyway_file("V4__changed.sql", "4", "changed", &changed),
        ];
        let text = fs::read_to_string(&changed).unwrap();
        let other = i64::from(crate::migrations::checksum(
            "CREATE TABLE other (id INT);\n",
        ));
        let history = HistoryRows {
            tool: Tool::Flyway,
            columns: vec![
                "version".into(),
                "description".into(),
                "checksum".into(),
                "success".into(),
            ],
            rows: vec![
                vec![
                    "1".into(),
                    "init".into(),
                    crate::migrations::checksum(&fs::read_to_string(&applied).unwrap()).to_string(),
                    "TRUE".into(),
                ],
                vec!["3".into(), "bad".into(), "123".into(), "FALSE".into()],
                vec![
                    "4".into(),
                    "changed".into(),
                    other.to_string(),
                    "TRUE".into(),
                ],
            ],
        };

        let states = states(&files, &[history]);

        assert_eq!(states[&applied], State::Applied);
        assert_eq!(states.get(&pending), None, "no row means pending");
        assert_eq!(states[&failed], State::Failed);
        assert_eq!(states[&changed], State::Changed);
        // The checksum of a file leaves the ends of line and the byte order mark out.
        assert_eq!(
            crate::migrations::checksum("a\r\nb\r\n"),
            crate::migrations::checksum("a\nb\n")
        );
        assert_eq!(
            crate::migrations::checksum("\u{feff}a"),
            crate::migrations::checksum("a")
        );
        // The usual CRC32 check value of `123456789`.
        assert_eq!(
            crate::migrations::checksum("123456789"),
            0xcbf4_3926u32 as i32
        );
        let _ = text;
    }

    #[test]
    fn a_repeatable_migration_is_matched_by_its_description() {
        let folder = test_support::TempDir::new("migrations-repeatable");
        let root = folder.path();
        let views = root.join("R__views.sql");
        write(&views, "CREATE VIEW v AS SELECT 1;\n");
        let file = flyway_file("R__views.sql", "", "views", &views);
        let history = HistoryRows {
            tool: Tool::Flyway,
            columns: vec![
                "version".into(),
                "description".into(),
                "checksum".into(),
                "success".into(),
            ],
            rows: vec![vec![
                String::new(),
                "views".into(),
                crate::migrations::checksum(&fs::read_to_string(&views).unwrap()).to_string(),
                "TRUE".into(),
            ]],
        };

        let states = states(&[file], &[history]);

        assert_eq!(states[&views], State::Applied);
    }

    #[test]
    fn the_history_of_flyway_writes_the_underscores_of_a_description_as_spaces() {
        let folder = test_support::TempDir::new("migrations-description");
        let root = folder.path();
        let path = root.join("V1__add_owner.sql");
        write(&path, "ALTER TABLE owners ADD COLUMN name VARCHAR(80);\n");
        let file = flyway_file("V1__add_owner.sql", "1", "add_owner", &path);
        let history = HistoryRows {
            tool: Tool::Flyway,
            columns: vec![
                "version".into(),
                "description".into(),
                "checksum".into(),
                "success".into(),
            ],
            rows: vec![vec![
                "1".into(),
                "add owner".into(),
                crate::migrations::checksum(&fs::read_to_string(&path).unwrap()).to_string(),
                "TRUE".into(),
            ]],
        };

        let states = states(&[file], &[history]);

        assert_eq!(states[&path], State::Applied);
    }

    #[test]
    fn the_liquibase_history_matches_the_id_the_author_and_the_file() {
        let folder = test_support::TempDir::new("migrations-liquibase-history");
        let root = folder.path();
        let path = root.join("db/changelog/owners.xml");
        write(&path, "<databaseChangeLog/>");
        let mut file = flyway_file("2:bob", "", "", &path);
        file.tool = Tool::Liquibase;
        file.kind = Kind::ChangeSet;
        file.label = "2:bob".into();
        let history = HistoryRows {
            tool: Tool::Liquibase,
            columns: vec![
                "ID".into(),
                "AUTHOR".into(),
                "FILENAME".into(),
                "MD5SUM".into(),
                "EXECTYPE".into(),
            ],
            rows: vec![
                vec![
                    "2".into(),
                    "bob".into(),
                    "db/changelog/owners.xml".into(),
                    "9:abc".into(),
                    "EXECUTED".into(),
                ],
                vec![
                    "3".into(),
                    "bob".into(),
                    "db/changelog/owners.xml".into(),
                    String::new(),
                    "FAILED".into(),
                ],
            ],
        };

        let states = states(&[file], &[history]);

        assert_eq!(states[&path], State::Applied);
    }

    #[test]
    fn a_project_without_the_model_still_finds_the_migration_folders() {
        let folder = test_support::TempDir::new("migrations-no-model");
        let root = folder.path();
        write(
            &root.join("src/main/resources/db/migration/V1__init.sql"),
            "",
        );
        write(
            &root.join("src/main/resources/db/changelog/db.changelog-master.xml"),
            "<databaseChangeLog/>",
        );

        let found = scan(root, None);

        assert_eq!(found.tools, [Tool::Flyway, Tool::Liquibase]);
        assert_eq!(found.files.len(), 2);
        assert_eq!(found.histories.len(), 2);
    }
}

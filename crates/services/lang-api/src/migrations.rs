//! The schema migrations of a project: the files of its migration tool, the checks over
//! their names and includes, the statement that reads the history of a database, and the
//! actions its build tool runs the migration plugin with.
//!
//! The core knows no tool and no database product: it shows the rows a language reports
//! and hands the rows of a history table back to the language, which matches them.

use std::path::PathBuf;

/// A tool that runs the migrations of a project.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MigrationTool {
    Flyway,
    Liquibase,
}

impl MigrationTool {
    pub fn name(self) -> &'static str {
        match self {
            Self::Flyway => "Flyway",
            Self::Liquibase => "Liquibase",
        }
    }
}

/// What one row of the migration node is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MigrationKind {
    Baseline,
    Versioned,
    Undo,
    Repeatable,
    Changelog,
    ChangeSet,
    Include,
}

/// The state of one file against the history table of a database.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum MigrationState {
    /// The database has no row for it.
    #[default]
    Pending,
    Applied,
    Failed,
    /// The database ran another text of the same version.
    Changed,
}

impl MigrationState {
    /// The mark of the tree.
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

/// One row of the migration node of a tree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationRow {
    /// The file the row opens.
    pub path: PathBuf,
    /// The name the tree shows.
    pub label: String,
    pub tool: MigrationTool,
    pub kind: MigrationKind,
    /// The version of a versioned file, empty for the others.
    pub version: String,
    pub description: String,
    /// How far the row is indented: a changeset or an include sits under its changelog.
    pub depth: usize,
    /// The changelog a row below one belongs to.
    pub parent: Option<PathBuf>,
    /// The file an include opens, which may not exist.
    pub target: Option<PathBuf>,
    pub state: MigrationState,
}

/// A problem of one file of the migrations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationIssue {
    pub path: PathBuf,
    /// The line, from one; zero when the format does not give one.
    pub line: usize,
    /// The name of the check in the settings.
    pub id: String,
    pub message: String,
}

/// The statement that reads the history of one tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationHistory {
    pub tool: MigrationTool,
    pub table: String,
    pub sql: String,
}

/// What an action of the build tool runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MigrationActionKind {
    Migrate,
    Info,
    Validate,
    Clean,
}

impl MigrationActionKind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Migrate => "Migrate",
            Self::Info => "Info",
            Self::Validate => "Validate",
            Self::Clean => "Clean",
        }
    }
}

/// One action of the build tool, with its line of goals or its task of the build tool.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationAction {
    pub tool: MigrationTool,
    pub kind: MigrationActionKind,
    /// The line of goals of the build tool.
    pub goal: Option<String>,
    /// The task of the build tool.
    pub task: Option<String>,
    /// Set when the project cannot run the action, with the reason for the hint.
    pub unavailable: Option<String>,
}

/// The file the next versioned migration goes into.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NextMigration {
    pub folder: PathBuf,
    pub version: String,
}

/// Everything the window shows about the migrations of a project.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MigrationModel {
    pub tools: Vec<MigrationTool>,
    pub rows: Vec<MigrationRow>,
    pub issues: Vec<MigrationIssue>,
    pub histories: Vec<MigrationHistory>,
    pub actions: Vec<MigrationAction>,
    /// The folders and changelogs the tools read.
    pub folders: Vec<PathBuf>,
    pub next: Option<NextMigration>,
}

impl MigrationModel {
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// The first problem of one file, which the tree underlines.
    pub fn issue_of(&self, path: &std::path::Path) -> Option<&MigrationIssue> {
        self.issues.iter().find(|issue| issue.path == path)
    }
}

/// The rows of one history table, as the window of the database reads them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MigrationHistoryRows {
    pub tool: MigrationTool,
    pub columns: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

/// The migrations of the project, and the state of their files against a database.
pub trait MigrationSupport: Send + Sync {
    /// The migrations of the project, with every file pending.
    fn model(&self) -> MigrationModel;

    /// The same migrations with the state the rows of a history table name.
    fn with_history(&self, histories: &[MigrationHistoryRows]) -> MigrationModel;

    /// The name of the next versioned file from the description the user typed.
    fn new_migration_name(&self, description: &str) -> Option<String>;
}

//! The live sections of a program that a configuration starts: the data an application
//! publishes over HTTP about itself while it runs. The language knows what the sections
//! are and how to read them; this module only describes them for the window.

use std::path::PathBuf;

/// One section of the live tab, in the order the window shows them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum LiveSection {
    #[default]
    Health,
    Beans,
    Mappings,
    Environment,
}

impl LiveSection {
    pub const ALL: [Self; 4] = [Self::Health, Self::Beans, Self::Mappings, Self::Environment];

    /// The name of the section in the window.
    pub fn title(self) -> &'static str {
        match self {
            Self::Health => "Health",
            Self::Beans => "Beans",
            Self::Mappings => "Mappings",
            Self::Environment => "Environment",
        }
    }

    /// The place of the section in an array of the four.
    pub fn index(self) -> usize {
        match self {
            Self::Health => 0,
            Self::Beans => 1,
            Self::Mappings => 2,
            Self::Environment => 3,
        }
    }
}

/// Where the live sections of an application are read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveEndpoint {
    pub host: String,
    pub port: u16,
}

/// How the first cells of a row are drawn: a state the application reported as good, one
/// it reported as bad, or plain text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum LiveTone {
    #[default]
    Normal,
    Good,
    Bad,
    Muted,
}

/// The place in the sources a row opens.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LiveTarget {
    /// A type, by its qualified name.
    Class(String),
    /// A method of a type.
    Method { class: String, method: String },
}

/// The request a row becomes in the HTTP client.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LiveRequest {
    pub method: String,
    pub path: String,
}

/// One row of a section: the cells of a table row, with an indentation for a tree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveRow {
    pub cells: Vec<String>,
    /// How deep the row is in its tree, zero at the top.
    pub depth: usize,
    pub tone: LiveTone,
    /// A heading of a group whose rows follow.
    pub group: bool,
    pub target: Option<LiveTarget>,
    pub request: Option<LiveRequest>,
}

/// What one section shows: its columns and its rows.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveTable {
    pub columns: Vec<String>,
    pub rows: Vec<LiveRow>,
}

impl LiveTable {
    pub fn new(columns: &[&str]) -> Self {
        Self {
            columns: columns.iter().map(|column| (*column).to_owned()).collect(),
            rows: Vec::new(),
        }
    }

    pub fn row(mut self, cells: &[&str]) -> Self {
        self.rows.push(LiveRow {
            cells: cells.iter().map(|cell| (*cell).to_owned()).collect(),
            ..LiveRow::default()
        });
        self
    }

    /// Adds a row with the parts of the tree and of a navigation target it carries.
    pub fn push(&mut self, row: LiveRow) {
        self.rows.push(row);
    }
}

/// A message of the tab with the file that may fix it, such as a setting to add.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LiveNote {
    pub text: String,
    pub file: Option<PathBuf>,
    /// The label of the button that opens `file`.
    pub label: Option<String>,
}

/// The live data of one running application.
pub trait LiveApplication: Send + Sync {
    /// The title of the tab.
    fn title(&self) -> String;

    /// Reads one line of the console. Answers whether the address, the readiness or the
    /// reason the sections cannot be read changed.
    fn observe(&self, line: &str) -> bool;

    /// The application reported that it started.
    fn started(&self) -> bool;

    /// Where the sections are read from, once the console or the settings named it.
    fn endpoint(&self) -> Option<LiveEndpoint>;

    /// The HTTP path of a section on the endpoint.
    fn section_path(&self, section: LiveSection) -> Option<String>;

    /// The URL of a path of the application itself, for a request of the HTTP client.
    /// The management endpoint may listen on another port.
    fn app_url(&self, path: &str) -> Option<String>;

    /// Why the sections cannot be read at all, such as a dependency the build file
    /// lacks. `None` when they are expected.
    fn missing(&self) -> Option<LiveNote> {
        None
    }

    /// The message of an endpoint the server answers 404 for: it exists but the
    /// application does not expose it.
    fn not_exposed(&self) -> LiveNote;

    /// Reads the body of a section.
    fn read(&self, section: LiveSection, body: &str) -> Result<LiveTable, String>;
}

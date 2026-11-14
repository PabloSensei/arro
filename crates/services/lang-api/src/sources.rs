//! The sources of a class that comes from a library rather than from the project.
//!
//! A window shows such a class read only; the language says what it is, and it is the
//! language that knows where its sources live. No build tool is named here.

use std::path::PathBuf;

use base::Cancel;

use crate::Provider;

/// A class of a library behind a read-only tab.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LibraryClass {
    /// What the line above the tab names, usually the artifact with its version.
    pub artifact: String,
    /// A second fact the language may add, such as a bytecode version.
    pub detail: Option<String>,
    /// The sources are attached, so the line above the tab is not needed.
    pub has_source: bool,
}

/// The origin of a read-only tab. The text of a file never asks this question.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryClassQuery(pub String);

/// Which class of a library a tab shows, and whether its sources are attached.
pub type LibraryClassProvider = dyn Provider<LibraryClassQuery, Option<LibraryClass>>;

/// What to fetch, or which file the user picked for a class.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourcesKind {
    /// The sources of the class behind this origin.
    Class(String),
    /// The sources of every library of the project.
    All,
    /// Attach the jar, zip or folder the user picked to the class behind this origin.
    Attach { uri: String, path: PathBuf },
}

/// One request of the window. Its own token stops the work; the provider also watches
/// the token of the request.
#[derive(Clone, Debug)]
pub struct SourcesRequest {
    pub kind: SourcesKind,
    pub cancel: Cancel,
}

/// What the window hears while the sources are fetched.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourcesEvent {
    /// The work began. `total` files may be fetched; already present ones are not counted.
    Started { total: usize },
    /// One file was fetched.
    Progress {
        done: usize,
        total: usize,
        name: String,
    },
    /// The class has its sources now, and the tab can read it again.
    Attached { uri: String },
    /// The work of [`SourcesKind::All`] ended.
    Summary {
        fetched: usize,
        skipped: usize,
        failed: usize,
    },
    /// No repository had the file.
    Missing { name: String },
    /// Anything else, in words the strip can show.
    Failed { message: String },
    /// The user stopped the work. What was fetched stays.
    Cancelled,
}

/// Fetches and attaches the sources of library classes.
pub type SourcesProvider = dyn Provider<SourcesRequest, SourcesEvent>;

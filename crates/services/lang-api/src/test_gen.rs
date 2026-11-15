//! Create Test: the libraries a project can write tests for, the form the window shows,
//! and the file the language makes for it. The window knows no library and no build
//! tool: every name and every path comes from the language.

use std::path::PathBuf;

use crate::{DocumentSpot, Provider, TextPoint, WorkspaceChange};

/// A library a test class may be written for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestFramework {
    /// The provider's own value for the library, given back unchanged.
    pub handle: String,
    /// The name the window shows, such as the name of the library.
    pub name: String,
    /// Whether the project already has the library.
    pub available: bool,
    /// What the window says when it has not, and nothing when it has.
    pub note: Option<String>,
}

/// A folder a new test may go into, with the name the window shows for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestFolder {
    pub path: PathBuf,
    pub name: String,
}

/// One method a test class may get, with the value the provider expects back.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestChoice {
    pub name: String,
    /// The signature under the name, `find(int id)` for one.
    pub detail: Option<String>,
    /// Whether the answer starts with the row marked.
    pub checked: bool,
    /// The method comes from a superclass rather than from the class itself.
    pub inherited: bool,
    pub handle: String,
}

/// The filled defaults of the Create Test window. The window edits them and sends them
/// back with [`TestCreateRequest`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestForm {
    /// The class the test is made for, without its package.
    pub source_class: String,
    pub frameworks: Vec<TestFramework>,
    /// The index of the library the form starts with.
    pub framework: usize,
    pub class_name: String,
    pub superclass: String,
    pub package: String,
    pub folders: Vec<TestFolder>,
    /// The index of the folder the form starts with.
    pub folder: usize,
    pub setup: bool,
    pub teardown: bool,
    pub methods: Vec<TestChoice>,
    /// Whether the language can add a missing library to the build file.
    pub can_fix: bool,
    /// The reason there is nothing to fill, such as a file that declares no class.
    pub error: Option<String>,
}

/// Create Test for the class at a place.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestFormQuery(pub DocumentSpot);

/// Everything the window got from the user of Create Test.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCreateRequest {
    pub spot: DocumentSpot,
    /// The handle of the chosen [`TestFramework`].
    pub framework: String,
    pub class_name: String,
    /// Empty when the test class extends nothing.
    pub superclass: String,
    pub package: String,
    pub folder: PathBuf,
    pub setup: bool,
    pub teardown: bool,
    /// The handles of the marked [`TestChoice`]s, in the order of the table.
    pub methods: Vec<String>,
}

/// The test file a language made.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestFile {
    pub path: PathBuf,
    /// The text of a new file. `None` when the file is already there: the window asks
    /// whether to open it.
    pub text: Option<String>,
    /// Where the caret goes in the new file, the first generated method for one.
    pub caret: Option<TextPoint>,
}

/// The file of a new test, or the reason the language made none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestCreateOutcome {
    pub file: Option<TestFile>,
    pub error: Option<String>,
}

/// Add the library of a [`TestFramework`] to the build file of the module.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestFixRequest {
    pub spot: DocumentSpot,
    pub framework: String,
}

/// The edits that add the library, as one step of undo in the open build file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TestFixOutcome {
    pub change: WorkspaceChange,
    pub error: Option<String>,
}

pub type TestFormProvider = dyn Provider<TestFormQuery, TestForm>;
pub type TestCreateProvider = dyn Provider<TestCreateRequest, TestCreateOutcome>;
pub type TestFixProvider = dyn Provider<TestFixRequest, TestFixOutcome>;

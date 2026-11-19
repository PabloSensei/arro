use std::path::PathBuf;

use crate::Provider;

/// What a mark in the gutter starts: a program or a test.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RunPointKind {
    Program,
    Test,
}

impl RunPointKind {
    pub fn name(self) -> &'static str {
        match self {
            Self::Program => "program",
            Self::Test => "test",
        }
    }
}

/// A place a program or a test can be started from, as the gutter marks it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunPoint {
    pub kind: RunPointKind,
    /// The name the agent sees: a class, a method or a case.
    pub name: String,
    /// The binary name of the type that holds the point.
    pub container: String,
    pub file: PathBuf,
    /// 1-based.
    pub line: u32,
    /// A program a framework starts on its own, such as a bootable server.
    pub framework: bool,
}

/// The file whose points the agent wants.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunPointQuery(pub PathBuf);

pub type RunPointsProvider = dyn Provider<RunPointQuery, Vec<RunPoint>>;

use std::fmt;

/// How far an answer can be trusted. A language backed by a server answers `Certain`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    Certain,
    Partial,
    Unknown,
}

/// Who produced an answer, shown to the user and to agents next to the answer.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct SourceId(&'static str);

impl SourceId {
    pub const fn new(name: &'static str) -> Self {
        Self(name)
    }

    pub const fn name(self) -> &'static str {
        self.0
    }
}

impl fmt::Display for SourceId {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(self.0)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Answer<T> {
    pub value: T,
    pub confidence: Confidence,
    pub source: SourceId,
    /// A remark of the language about the answer, in words for the user: the part of it
    /// the language could not reach, for one. The window shows it under the answer.
    pub note: Option<String>,
}

impl<T> Answer<T> {
    pub fn new(value: T, confidence: Confidence, source: SourceId) -> Self {
        Self {
            value,
            confidence,
            source,
            note: None,
        }
    }

    /// Adds a remark about the answer, in words for the user.
    pub fn with_note(mut self, note: impl Into<String>) -> Self {
        self.note = Some(note.into());
        self
    }

    pub fn map<U>(self, change: impl FnOnce(T) -> U) -> Answer<U> {
        Answer {
            value: change(self.value),
            confidence: self.confidence,
            source: self.source,
            note: self.note,
        }
    }
}

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
}

impl<T> Answer<T> {
    pub fn new(value: T, confidence: Confidence, source: SourceId) -> Self {
        Self {
            value,
            confidence,
            source,
        }
    }

    pub fn map<U>(self, change: impl FnOnce(T) -> U) -> Answer<U> {
        Answer {
            value: change(self.value),
            confidence: self.confidence,
            source: self.source,
        }
    }
}

//! What the editors show: the active one, carets, selections and the visible lines. The
//! views write it, agents and tests read it without a window.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;

use platform::CanonPath;

use crate::DocumentId;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Selection {
    /// Where the selection started, bytes.
    pub anchor: usize,
    /// The caret, bytes.
    pub head: usize,
}

impl Selection {
    pub fn caret(offset: usize) -> Self {
        Self {
            anchor: offset,
            head: offset,
        }
    }

    pub fn range(&self) -> Range<usize> {
        self.anchor.min(self.head)..self.anchor.max(self.head)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditorSnapshot {
    pub document: DocumentId,
    pub path: CanonPath,
    /// The version of the document these offsets belong to.
    pub version: u64,
    /// The primary selection first.
    pub selections: Vec<Selection>,
    /// 0-based lines on screen, end excluded.
    pub visible_lines: Range<u32>,
}

/// A place to come back to with Back and Forward.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Place {
    pub path: CanonPath,
    pub offset: usize,
}

/// How many places Back keeps, and how many files Recent Files lists.
const HISTORY: usize = 100;
const RECENT: usize = 50;

#[derive(Clone, Debug, Default)]
pub struct Editors {
    editors: HashMap<DocumentId, EditorSnapshot>,
    active: Option<DocumentId>,
    back: VecDeque<Place>,
    forward: Vec<Place>,
    recent: VecDeque<CanonPath>,
}

impl Editors {
    pub fn new() -> Self {
        Self::default()
    }

    /// An editor shows a document now, or its carets or scroll moved.
    pub fn update(&mut self, snapshot: EditorSnapshot) {
        self.editors.insert(snapshot.document, snapshot);
    }

    pub fn close(&mut self, document: DocumentId) {
        self.editors.remove(&document);
        if self.active == Some(document) {
            self.active = None;
        }
    }

    /// Focus moved to the editor of `document`. It goes to the top of Recent Files.
    pub fn activate(&mut self, document: DocumentId) {
        self.active = Some(document);
        if let Some(path) = self
            .editors
            .get(&document)
            .map(|editor| editor.path.clone())
        {
            self.recent.retain(|known| *known != path);
            self.recent.push_front(path);
            self.recent.truncate(RECENT);
        }
    }

    pub fn active(&self) -> Option<&EditorSnapshot> {
        self.editors.get(&self.active?)
    }

    pub fn get(&self, document: DocumentId) -> Option<&EditorSnapshot> {
        self.editors.get(&document)
    }

    /// The last active first: Ctrl+E lists them, Ctrl+Tab starts on the second.
    pub fn recent(&self) -> impl Iterator<Item = &CanonPath> {
        self.recent.iter()
    }

    /// Before a jump to another place: `from` is where the caret was.
    pub fn jumped_from(&mut self, from: Place) {
        if self.back.back() == Some(&from) {
            return;
        }
        self.back.push_back(from);
        if self.back.len() > HISTORY {
            self.back.pop_front();
        }
        self.forward.clear();
    }

    /// Where Back goes from `here`. `here` becomes the place Forward returns to.
    pub fn back(&mut self, here: Place) -> Option<Place> {
        let to = self.back.pop_back()?;
        self.forward.push(here);
        Some(to)
    }

    pub fn forward(&mut self, here: Place) -> Option<Place> {
        let to = self.forward.pop()?;
        self.back.push_back(here);
        Some(to)
    }
}

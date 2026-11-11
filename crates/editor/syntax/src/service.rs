//! The syntax thread. The UI thread posts text snapshots and receives finished results;
//! a newer snapshot replaces one that is still waiting.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use streaming_iterator::StreamingIterator;
use text_core::{Change, Edit, RopeStore, TextStore};
use tree_sitter::{InputEdit, Parser, Point, Query, QueryCursor, Tree};

use crate::highlight::{Capture, HighlightKind, HighlightSpan, flatten};
use crate::language::LanguageConfig;
use crate::outline::{FoldRange, folds, region_folds};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DocumentId(pub u64);

/// A finished parse of one version of a document.
pub struct SyntaxSnapshot {
    pub version: u64,
    pub tree: Tree,
    pub highlights: Vec<HighlightSpan>,
    pub folds: Vec<FoldRange>,
    pub indents: Arc<Query>,
}

type Listener = Box<dyn Fn(SyntaxSnapshot) + Send>;

struct Job {
    version: u64,
    text: RopeStore,
    steps: Vec<Vec<Edit>>,
}

#[derive(Default)]
struct Queue {
    pending: HashMap<DocumentId, Job>,
    order: VecDeque<DocumentId>,
    opened: Vec<(DocumentId, LanguageConfig, Listener)>,
    closed: Vec<DocumentId>,
    stopped: bool,
}

#[derive(Default)]
struct Shared {
    queue: Mutex<Queue>,
    changed: Condvar,
}

/// Handle to the syntax thread. The thread ends when the last handle is dropped.
#[derive(Clone)]
pub struct SyntaxService {
    shared: Arc<Stop>,
}

struct Stop(Arc<Shared>);

impl Drop for Stop {
    fn drop(&mut self) {
        self.0.queue.lock().unwrap().stopped = true;
        self.0.changed.notify_one();
    }
}

impl Default for SyntaxService {
    fn default() -> Self {
        Self::new()
    }
}

impl SyntaxService {
    pub fn new() -> Self {
        let shared = Arc::new(Shared::default());
        let worker = shared.clone();
        std::thread::Builder::new()
            .name("syntax".to_owned())
            .spawn(move || run(&worker))
            .expect("start the syntax thread");
        Self {
            shared: Arc::new(Stop(shared)),
        }
    }

    /// Registers a document. `listener` runs on the syntax thread.
    pub fn open(
        &self,
        id: DocumentId,
        language: LanguageConfig,
        listener: impl Fn(SyntaxSnapshot) + Send + 'static,
    ) {
        let shared = &self.shared.0;
        shared
            .queue
            .lock()
            .unwrap()
            .opened
            .push((id, language, Box::new(listener)));
        shared.changed.notify_one();
    }

    /// Asks for a parse of `text`, the state of the document after `change`.
    pub fn update(&self, id: DocumentId, version: u64, text: RopeStore, change: &Change) {
        let shared = &self.shared.0;
        let mut queue = shared.queue.lock().unwrap();
        match queue.pending.get_mut(&id) {
            Some(job) => {
                job.version = version;
                job.text = text;
                job.steps.extend_from_slice(&change.steps);
            }
            None => {
                queue.pending.insert(
                    id,
                    Job {
                        version,
                        text,
                        steps: change.steps.clone(),
                    },
                );
                queue.order.push_back(id);
            }
        }
        shared.changed.notify_one();
    }

    pub fn close(&self, id: DocumentId) {
        let shared = &self.shared.0;
        shared.queue.lock().unwrap().closed.push(id);
        shared.changed.notify_one();
    }
}

struct Session {
    language: LanguageConfig,
    parser: Parser,
    highlights: Query,
    folds: Query,
    indents: Arc<Query>,
    capture_kinds: Vec<Option<HighlightKind>>,
    tree: Option<Tree>,
    /// The text `tree` was parsed from.
    text: RopeStore,
    listener: Listener,
}

impl Session {
    fn new(language: LanguageConfig, listener: Listener) -> Option<Self> {
        let mut parser = Parser::new();
        parser.set_language(&language.grammar).ok()?;
        let highlights = Query::new(&language.grammar, language.highlights).ok()?;
        let folds = Query::new(&language.grammar, language.folds).ok()?;
        let indents = Arc::new(Query::new(&language.grammar, language.indents).ok()?);
        let capture_kinds = highlights
            .capture_names()
            .iter()
            .map(|name| HighlightKind::from_capture(name))
            .collect();
        Some(Self {
            language,
            parser,
            highlights,
            folds,
            indents,
            capture_kinds,
            tree: None,
            text: RopeStore::default(),
            listener,
        })
    }

    fn parse(&mut self, job: Job) {
        if let Some(tree) = &mut self.tree {
            let mut text = self.text.clone();
            for step in &job.steps {
                for edit in step.iter().rev() {
                    tree.edit(&input_edit(&text, edit));
                    text.replace(edit.range.clone(), &edit.text);
                }
            }
        }
        let source = job.text.text();
        let Some(tree) = self.parser.parse(&source, self.tree.as_ref()) else {
            return;
        };
        let highlights = self.highlights(&tree, source.as_bytes());
        let folds = {
            let mut found = folds(
                &tree,
                &self.folds,
                source.as_bytes(),
                self.language.fold_styles,
            );
            found.extend(region_folds(&source, self.language.line_comment));
            found
        };
        self.tree = Some(tree.clone());
        self.text = job.text;
        (self.listener)(SyntaxSnapshot {
            version: job.version,
            tree,
            highlights,
            folds,
            indents: self.indents.clone(),
        });
    }

    fn highlights(&self, tree: &Tree, source: &[u8]) -> Vec<HighlightSpan> {
        let mut cursor = QueryCursor::new();
        let mut captures = Vec::new();
        let mut matches = cursor.matches(&self.highlights, tree.root_node(), source);
        while let Some(found) = matches.next() {
            for capture in found.captures() {
                if let Some(kind) = self.capture_kinds[capture.index as usize] {
                    captures.push(Capture {
                        range: capture.node.byte_range(),
                        pattern: found.pattern_index,
                        kind,
                    });
                }
            }
        }
        flatten(captures)
    }
}

fn point(store: &RopeStore, offset: usize) -> Point {
    let row = store.offset_to_line(offset);
    Point {
        row,
        column: offset - store.line_to_offset(row),
    }
}

fn input_edit(store: &RopeStore, edit: &Edit) -> InputEdit {
    let start_position = point(store, edit.range.start);
    let new_lines = edit.text.bytes().filter(|byte| *byte == b'\n').count();
    let new_end_position = match edit.text.rfind('\n') {
        Some(at) => Point {
            row: start_position.row + new_lines,
            column: edit.text.len() - at - 1,
        },
        None => Point {
            row: start_position.row,
            column: start_position.column + edit.text.len(),
        },
    };
    InputEdit {
        start_byte: edit.range.start,
        old_end_byte: edit.range.end,
        new_end_byte: edit.range.start + edit.text.len(),
        start_position,
        old_end_position: point(store, edit.range.end),
        new_end_position,
    }
}

fn run(shared: &Shared) {
    let mut sessions: HashMap<DocumentId, Session> = HashMap::new();
    loop {
        let (job, opened, closed) = {
            let mut queue = shared.queue.lock().unwrap();
            loop {
                if queue.stopped {
                    return;
                }
                let waiting =
                    !queue.order.is_empty() || !queue.opened.is_empty() || !queue.closed.is_empty();
                if waiting {
                    break;
                }
                queue = shared.changed.wait(queue).unwrap();
            }
            let job = queue
                .order
                .pop_front()
                .and_then(|id| queue.pending.remove(&id).map(|job| (id, job)));
            (
                job,
                std::mem::take(&mut queue.opened),
                std::mem::take(&mut queue.closed),
            )
        };
        for (id, language, listener) in opened {
            if let Some(session) = Session::new(language, listener) {
                sessions.insert(id, session);
            }
        }
        if let Some((id, job)) = job
            && let Some(session) = sessions.get_mut(&id)
        {
            session.parse(job);
        }
        for id in closed {
            sessions.remove(&id);
        }
    }
}

//! The syntax thread. The UI thread posts text snapshots and receives finished results;
//! a newer snapshot replaces one that is still waiting.

use std::collections::{HashMap, VecDeque};
use std::ops::Range;
use std::sync::{Arc, Condvar, Mutex};

use streaming_iterator::StreamingIterator;
use text_core::{Change, Edit, RopeStore, TextStore};
use tree_sitter::{InputEdit, Language, Parser, Point, Query, QueryCursor, Tree};

use crate::highlight::{Capture, HighlightKind, HighlightSpan, flatten};
use crate::language::{Injection, LanguageConfig};
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
    /// A parse of one injected language over all its ranges of the document.
    layers: HashMap<(Language, &'static str), LayerSession>,
    /// The text `tree` was parsed from.
    text: RopeStore,
    listener: Listener,
}

/// The parser and the query of one language injected into a document.
struct LayerSession {
    parser: Parser,
    query: Query,
    capture_kinds: Vec<Option<HighlightKind>>,
    tree: Option<Tree>,
}

impl Session {
    fn new(language: LanguageConfig, listener: Listener) -> Option<Self> {
        let mut parser = Parser::new();
        parser.set_language(&language.grammar).ok()?;
        let highlights = Query::new(&language.grammar, language.highlights).ok()?;
        let folds = Query::new(&language.grammar, language.folds).ok()?;
        let indents = Arc::new(Query::new(&language.grammar, language.indents).ok()?);
        let capture_kinds = capture_kinds(&highlights);
        Some(Self {
            language,
            parser,
            highlights,
            folds,
            indents,
            capture_kinds,
            tree: None,
            layers: HashMap::new(),
            text: RopeStore::default(),
            listener,
        })
    }

    fn parse(&mut self, job: Job) {
        if self.tree.is_some() {
            let mut text = self.text.clone();
            for step in &job.steps {
                for edit in step.iter().rev() {
                    let input = input_edit(&text, edit);
                    if let Some(tree) = &mut self.tree {
                        tree.edit(&input);
                    }
                    // A layer spans the whole document, so an edit of the text is the
                    // same edit for every layer tree.
                    for layer in self.layers.values_mut() {
                        if let Some(tree) = &mut layer.tree {
                            tree.edit(&input);
                        }
                    }
                    text.replace(edit.range.clone(), &edit.text);
                }
            }
        }
        let source = job.text.text();
        let Some(tree) = self.parser.parse(&source, self.tree.as_ref()) else {
            return;
        };
        let mut captures = query_captures(
            &self.highlights,
            &self.capture_kinds,
            &tree,
            source.as_bytes(),
            0,
        );
        let replaced = self.layer_captures(&tree, source.as_bytes(), &mut captures);
        if !replaced.is_empty() {
            captures.retain(|capture| !replaced.contains(&capture.range));
        }
        let highlights = flatten(captures);
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

    /// Parses every injected layer over all its ranges at once and adds its captures.
    /// Returns the ranges the layers replace in the outer highlights.
    fn layer_captures(
        &mut self,
        tree: &Tree,
        source: &[u8],
        captures: &mut Vec<Capture>,
    ) -> Vec<Range<usize>> {
        let mut groups: HashMap<(Language, &'static str), Vec<tree_sitter::Range>> = HashMap::new();
        let mut replaced = Vec::new();
        let mut stack = vec![tree.root_node()];
        while let Some(node) = stack.pop() {
            for injection in &self.language.injections {
                if node.kind() != injection.node {
                    continue;
                }
                let text = match injection.text_node {
                    Some(kind) => match child_of_kind(node, kind) {
                        Some(child) => child,
                        None => continue,
                    },
                    None => node,
                };
                let range = text.range();
                if range.start_byte == range.end_byte {
                    continue;
                }
                let Some((language, highlights)) = injection_language(injection, node, source)
                else {
                    continue;
                };
                if injection.replace {
                    replaced.push(range.start_byte..range.end_byte);
                }
                groups
                    .entry((language, highlights))
                    .or_default()
                    .extend(text_ranges(text, injection.skip_nodes, source));
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }

        let base = self.highlights.pattern_count();
        for ((language, highlights), mut ranges) in groups {
            ranges.sort_by_key(|range| range.start_byte);
            let mut free = 0;
            let ranges: Vec<_> = ranges
                .into_iter()
                .filter(|range| {
                    if range.start_byte < free {
                        return false;
                    }
                    free = range.end_byte;
                    true
                })
                .collect();
            let Some(layer) = self.layer_session(language, highlights) else {
                continue;
            };
            if layer.parser.set_included_ranges(&ranges).is_err() {
                continue;
            }
            let old = layer.tree.take();
            let Some(parsed) = layer.parser.parse(source, old.as_ref()) else {
                layer.tree = old;
                continue;
            };
            captures.extend(query_captures(
                &layer.query,
                &layer.capture_kinds,
                &parsed,
                source,
                base,
            ));
            layer.tree = Some(parsed);
        }
        replaced
    }

    /// The session of an injected language, with its parser, its query and its tree.
    fn layer_session(
        &mut self,
        language: Language,
        highlights: &'static str,
    ) -> Option<&mut LayerSession> {
        let key = (language.clone(), highlights);
        if !self.layers.contains_key(&key) {
            let mut parser = Parser::new();
            parser.set_language(&language).ok()?;
            let query = Query::new(&language, highlights).ok()?;
            let capture_kinds = capture_kinds(&query);
            self.layers.insert(
                key.clone(),
                LayerSession {
                    parser,
                    query,
                    capture_kinds,
                    tree: None,
                },
            );
        }
        self.layers.get_mut(&key)
    }
}

/// The capture of every name of `query` in `tree`, with `pattern_base` added to the
/// pattern number so that a layer wins ties over the query of the outer language.
fn query_captures(
    query: &Query,
    capture_kinds: &[Option<HighlightKind>],
    tree: &Tree,
    source: &[u8],
    pattern_base: usize,
) -> Vec<Capture> {
    let mut cursor = QueryCursor::new();
    let mut captures = Vec::new();
    let mut matches = cursor.matches(query, tree.root_node(), source);
    while let Some(found) = matches.next() {
        for capture in found.captures() {
            if let Some(kind) = capture_kinds[capture.index as usize] {
                captures.push(Capture {
                    range: capture.node.byte_range(),
                    pattern: pattern_base + found.pattern_index,
                    kind,
                });
            }
        }
    }
    captures
}

fn capture_kinds(query: &Query) -> Vec<Option<HighlightKind>> {
    query
        .capture_names()
        .iter()
        .map(|name| HighlightKind::from_capture(name))
        .collect()
}

/// The language an injection parses and the query that colours it.
fn injection_language(
    injection: &Injection,
    node: tree_sitter::Node,
    source: &[u8],
) -> Option<(Language, &'static str)> {
    match injection.name_node {
        Some(kind) => {
            let name = child_of_kind(node, kind)?;
            let name = source.get(name.byte_range())?;
            let name = std::str::from_utf8(name).ok()?;
            let name = name.split_whitespace().next()?;
            let config = (injection.resolve?)(name)?;
            Some((config.grammar, config.highlights))
        }
        None => Some((injection.language.clone()?, injection.highlights?)),
    }
}

fn child_of_kind<'a>(node: tree_sitter::Node<'a>, kind: &str) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    node.children(&mut cursor)
        .find(|child| child.kind() == kind)
}

/// The text of `node` without the children that hold container syntax, such as the
/// marks each line of a block carries in its own language.
fn text_ranges(node: tree_sitter::Node, skip: &[&str], source: &[u8]) -> Vec<tree_sitter::Range> {
    let range = node.range();
    let mut cuts: Vec<Range<usize>> = Vec::new();
    if !skip.is_empty() {
        let mut cursor = node.walk();
        for child in node.children(&mut cursor) {
            if skip.contains(&child.kind()) {
                let cut = child.byte_range();
                if cut.start < cut.end {
                    cuts.push(cut);
                }
            }
        }
    }
    if cuts.is_empty() {
        return vec![range];
    }
    cuts.sort_by_key(|cut| cut.start);
    let mut ranges = Vec::with_capacity(cuts.len() + 1);
    let mut at = range.start_byte;
    let mut point = range.start_point;
    for cut in cuts {
        let start = cut.start.clamp(at, range.end_byte);
        if start > at {
            let start_point = point;
            advance(source, at, start, &mut point);
            ranges.push(tree_sitter::Range {
                start_byte: at,
                end_byte: start,
                start_point,
                end_point: point,
            });
            at = start;
        }
        let end = cut.end.clamp(at, range.end_byte);
        advance(source, at, end, &mut point);
        at = end;
    }
    if at < range.end_byte {
        let start_point = point;
        advance(source, at, range.end_byte, &mut point);
        ranges.push(tree_sitter::Range {
            start_byte: at,
            end_byte: range.end_byte,
            start_point,
            end_point: point,
        });
    }
    ranges
}

/// Moves `point` over the bytes `source[from..to]`.
fn advance(source: &[u8], from: usize, to: usize, point: &mut Point) {
    for &byte in &source[from..to] {
        if byte == b'\n' {
            point.row += 1;
            point.column = 0;
        } else {
            point.column += 1;
        }
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

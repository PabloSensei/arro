//! Rendered documentation comments in the text. The language gives the comments and
//! their markup; the editor turns them into rows of the display map and toggles them
//! back to text. The buffer itself never changes.

use std::ops::Range;

use lang_api::{DocumentationBlock, DocumentationComment, DocumentationInline};
use text_core::{TextStore, transform_offset};
use ui_kit::*;

use crate::view::EditorView;
use display_map::{DocComment, DocLine, DocRun, DocRunKind};

/// The documentation comments of the open text and the choices the user made for each.
#[derive(Default)]
pub(crate) struct RenderedDocs {
    pub comments: Vec<DocumentationComment>,
    /// Grows with every question, so a late answer is dropped.
    pub asked: u64,
    /// The comments the user showed while the setting was off.
    pub shown: Vec<Range<usize>>,
    /// The comments the user hid while the setting was on.
    pub hidden: Vec<Range<usize>>,
}

impl EditorView {
    /// Asks the language for the documentation comments of the text. The parse runs off
    /// the UI thread; a newer question drops the answer of an older one.
    pub(crate) fn request_doc_comments(&mut self, cx: &mut Context<Self>) {
        let (Some(state), Some(lsp)) = (&self.state, &self.lsp) else {
            return;
        };
        if lsp.support.documentation_comments().is_none() {
            self.docs = RenderedDocs::default();
            if let Some(state) = &mut self.state {
                state.set_doc_comments(Vec::new());
            }
            cx.notify();
            return;
        }
        let support = lsp.support.clone();
        let text = state.text();
        let version = state.version();
        self.docs.asked = self.docs.asked.wrapping_add(1);
        let asked = self.docs.asked;
        let task = cx.background_spawn(async move {
            support
                .documentation_comments()
                .map(|provider| provider.documentation_comments(&text))
                .unwrap_or_default()
        });
        cx.spawn(async move |this, cx| {
            let comments = task.await;
            this.update(cx, |this, cx| {
                if this.docs.asked != asked
                    || this.state.as_ref().map(|state| state.version()) != Some(version)
                {
                    return;
                }
                let Some(text) = this.state.as_ref().map(|state| state.text()) else {
                    return;
                };
                this.docs.comments = comments
                    .into_iter()
                    .filter_map(|comment| {
                        let range = whole_line_range(&text, comment.range.clone())?;
                        Some(DocumentationComment {
                            range,
                            blocks: comment.blocks,
                        })
                    })
                    .collect();
                this.apply_doc_comments(cx);
            })
            .ok();
        })
        .detach();
    }

    /// Rebuilds the rows of the display map from the comments and the current view.
    pub(crate) fn apply_doc_comments(&mut self, cx: &mut Context<Self>) {
        let mut docs = Vec::new();
        if let Some(state) = &self.state {
            let store = state.store();
            for comment in &self.docs.comments {
                if !self.doc_is_rendered(comment.range.clone()) {
                    continue;
                }
                let line = store.offset_to_line(comment.range.start.min(store.len()));
                let indent = indent_columns(&store.line_text(line));
                let lines = comment_lines(&comment.blocks, indent);
                if !lines.is_empty() {
                    docs.push(DocComment {
                        range: comment.range.clone(),
                        lines,
                    });
                }
            }
        }
        if let Some(state) = &mut self.state {
            let rendered: Vec<Range<usize>> = docs.iter().map(|doc| doc.range.clone()).collect();
            state.set_doc_comments(docs);
            let store = state.store();
            let head = state.selections().primary().head;
            if let Some(range) = rendered
                .iter()
                .find(|range| range.start <= head && head < range.end)
            {
                let line = store.offset_to_line(range.end.saturating_sub(1).min(store.len()));
                let next = (line + 1).min(store.line_count().saturating_sub(1));
                let target = store.line_to_offset(next);
                state.select_range(target..target);
            }
        }
        cx.notify();
    }

    fn doc_is_rendered(&self, range: Range<usize>) -> bool {
        let hidden = self.docs.hidden.iter().any(|known| covers(known, &range));
        let shown = self.docs.shown.iter().any(|known| covers(known, &range));
        if self.options.render_doc_comments {
            !hidden
        } else {
            shown
        }
    }

    /// The comment that starts on `line` and whether it is rendered, for the gutter icon.
    pub(crate) fn rendered_doc_at_line(&self, line: usize) -> Option<bool> {
        self.doc_range_at_line(line)
            .map(|range| self.doc_is_rendered(range))
    }

    /// The range of the comment that starts on `line`.
    pub(crate) fn doc_range_at_line(&self, line: usize) -> Option<Range<usize>> {
        let state = self.state.as_ref()?;
        let store = state.store();
        self.docs.comments.iter().find_map(|comment| {
            (store.offset_to_line(comment.range.start.min(store.len())) == line)
                .then(|| comment.range.clone())
        })
    }

    /// A click on a link of a rendered comment: a web or a file address opens the way a
    /// link of the quick documentation popup does, a name goes to its declaration.
    pub(crate) fn open_doc_link(&mut self, target: String, cx: &mut Context<Self>) {
        if target.is_empty() {
            return;
        }
        if target.starts_with("http://")
            || target.starts_with("https://")
            || target.starts_with("file:")
            || target.starts_with("jdt:")
        {
            self.open_quick_doc_link(target, cx);
            return;
        }
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.workspace_symbols())
        else {
            return;
        };
        let name = target
            .split(['#', '.', '('])
            .next()
            .unwrap_or(target.as_str())
            .to_owned();
        let replies = lang_api::ask(provider, lang_api::SymbolQuery(name));
        cx.spawn(async move |this, cx| {
            let answer = crate::semantic::first(replies).await;
            let Some((entries, _)) = answer else {
                return;
            };
            let Some(place) = entries.into_iter().find_map(|entry| entry.place) else {
                return;
            };
            this.update(cx, |this, cx| this.go_to_location(&place, cx))
                .ok();
        })
        .detach();
    }

    /// Ctrl+Alt+Q: the documentation comment under the caret becomes rendered text or
    /// goes back to its source.
    pub(crate) fn toggle_rendered_doc_at_caret(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(state) = &self.state else {
            return false;
        };
        let store = state.store();
        let caret = state.selections().primary().head;
        let line = store.offset_to_line(caret);
        let range = self
            .docs
            .comments
            .iter()
            .find(|comment| comment.range.start <= caret && caret <= comment.range.end)
            .or_else(|| {
                self.docs.comments.iter().find(|comment| {
                    store.offset_to_line(comment.range.end.saturating_sub(1).min(store.len())) + 1
                        == line
                })
            })
            .map(|comment| comment.range.clone());
        let Some(range) = range else {
            return false;
        };
        self.toggle_doc_range(range, cx);
        true
    }

    /// A click on the icon of a comment or on its rendered rows.
    pub(crate) fn toggle_rendered_doc_at(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        if self.doc_is_rendered(range.clone()) {
            self.docs.hidden.push(range.clone());
            self.docs.shown.retain(|known| !covers(known, &range));
        } else {
            self.docs.shown.push(range.clone());
            self.docs.hidden.retain(|known| !covers(known, &range));
        }
        self.apply_doc_comments(cx);
    }

    fn toggle_doc_range(&mut self, range: Range<usize>, cx: &mut Context<Self>) {
        self.toggle_rendered_doc_at(range, cx);
    }

    /// A find match or any other selection inside a rendered comment brings the text
    /// back, so the caret and the match are visible.
    pub(crate) fn reveal_rendered_doc(
        &mut self,
        range: Range<usize>,
        cx: &mut Context<Self>,
    ) -> bool {
        let touched: Vec<Range<usize>> = self
            .docs
            .comments
            .iter()
            .filter(|comment| {
                self.doc_is_rendered(comment.range.clone())
                    && comment.range.start <= range.end
                    && range.start <= comment.range.end
            })
            .map(|comment| comment.range.clone())
            .collect();
        if touched.is_empty() {
            return false;
        }
        for range in touched {
            self.docs.hidden.push(range.clone());
            self.docs.shown.retain(|known| !covers(known, &range));
        }
        self.apply_doc_comments(cx);
        true
    }

    /// Keeps the choices of the user on the text after an edit. An edit that touches a
    /// comment drops its choice, and the comment shows as text again.
    pub(crate) fn shift_doc_comments(&mut self, change: &text_core::Change) {
        for step in &change.steps {
            let mut touched = Vec::new();
            for comment in &self.docs.comments {
                if !step
                    .iter()
                    .any(|edit| overlaps(&edit.range, &comment.range))
                {
                    continue;
                }
                let mut range = comment.range.clone();
                for edit in step {
                    range.start = transform_offset(range.start, std::slice::from_ref(edit));
                    range.end = transform_offset(range.end, std::slice::from_ref(edit));
                }
                touched.push(range);
            }
            for range in touched {
                self.docs.hidden.push(range.clone());
                self.docs.shown.retain(|known| !covers(known, &range));
            }
            let keeps = |range: &Range<usize>| {
                !step
                    .iter()
                    .any(|edit| edit.range.end < range.start || edit.range.start > range.end)
            };
            self.docs.shown.retain(&keeps);
            self.docs.hidden.retain(&keeps);
            for range in self
                .docs
                .shown
                .iter_mut()
                .chain(self.docs.hidden.iter_mut())
            {
                for edit in step {
                    range.start = transform_offset(range.start, std::slice::from_ref(edit));
                    range.end = transform_offset(range.end, std::slice::from_ref(edit));
                }
            }
            self.docs.comments.retain(|comment| keeps(&comment.range));
            for comment in &mut self.docs.comments {
                for edit in step {
                    comment.range.start =
                        transform_offset(comment.range.start, std::slice::from_ref(edit));
                    comment.range.end =
                        transform_offset(comment.range.end, std::slice::from_ref(edit));
                }
            }
        }
    }

    /// The rendered rows, with the line each speaks for and its text, for tests.
    pub fn rendered_doc_rows(&self) -> Vec<(usize, String)> {
        let Some(state) = &self.state else {
            return Vec::new();
        };
        let display = state.display();
        (0..display.row_count())
            .filter_map(|row| {
                display
                    .doc_row(row)
                    .map(|line| (display.row_to_line(row), line.text()))
            })
            .collect()
    }

    /// The gutter icons of the comments: the line and whether the comment is rendered.
    pub fn rendered_doc_icons(&self) -> Vec<(usize, bool)> {
        let Some(state) = &self.state else {
            return Vec::new();
        };
        let store = state.store();
        self.docs
            .comments
            .iter()
            .map(|comment| {
                (
                    store.offset_to_line(comment.range.start.min(store.len())),
                    self.doc_is_rendered(comment.range.clone()),
                )
            })
            .collect()
    }
}

fn covers(known: &Range<usize>, range: &Range<usize>) -> bool {
    known.start <= range.start && range.end <= known.end
}

fn overlaps(left: &Range<usize>, right: &Range<usize>) -> bool {
    left.start <= right.end && right.start <= left.end
}

/// The comment takes whole lines: the editor may take them away and put its rows there.
fn whole_line_range(text: &str, range: Range<usize>) -> Option<Range<usize>> {
    let start = text[..range.start.min(text.len())]
        .rfind('\n')
        .map_or(0, |at| at + 1);
    let after = range.end.min(text.len());
    let line_end = text[after..].find('\n').map_or(text.len(), |at| after + at);
    if text[start..range.start].trim().is_empty() && text[after..line_end].trim().is_empty() {
        Some(start..line_end)
    } else {
        None
    }
}

fn indent_columns(text: &str) -> usize {
    let mut columns = 0;
    for character in text.chars() {
        match character {
            ' ' => columns += 1,
            '\t' => columns += 4 - columns % 4,
            _ => break,
        }
    }
    columns
}

fn comment_lines(blocks: &[DocumentationBlock], indent: usize) -> Vec<DocLine> {
    let mut lines = Vec::new();
    for block in blocks {
        match block {
            DocumentationBlock::Paragraph(inlines) => {
                push_inlines(&mut lines, indent, &[], inlines);
            }
            DocumentationBlock::List { items, .. } => {
                for item in items {
                    push_inlines(&mut lines, indent, &[DocRun::text("\u{2022} ")], item);
                }
            }
            DocumentationBlock::Code { text, .. } => {
                for line in text.lines() {
                    lines.push(DocLine {
                        indent,
                        runs: vec![DocRun {
                            text: line.to_owned(),
                            kind: DocRunKind::Code,
                            target: None,
                        }],
                    });
                }
            }
            DocumentationBlock::Tag {
                name,
                subject,
                description,
            } => {
                let mut prefix = vec![DocRun {
                    text: format!("@{name}"),
                    kind: DocRunKind::Strong,
                    target: None,
                }];
                if let Some(subject) = subject {
                    prefix.push(DocRun::text(" "));
                    prefix.push(DocRun {
                        text: subject.clone(),
                        kind: DocRunKind::Code,
                        target: None,
                    });
                }
                prefix.push(DocRun::text("  "));
                push_inlines(&mut lines, indent, &prefix, description);
            }
        }
    }
    lines
}

fn push_inlines(
    lines: &mut Vec<DocLine>,
    indent: usize,
    prefix: &[DocRun],
    inlines: &[DocumentationInline],
) {
    let mut runs = prefix.to_vec();
    flatten(inlines, &mut runs);
    let mut current: Vec<DocRun> = Vec::new();
    for run in runs {
        let mut pieces = run.text.split('\n');
        if let Some(first) = pieces.next()
            && !first.is_empty()
        {
            current.push(DocRun {
                text: first.to_owned(),
                ..run.clone()
            });
        }
        for piece in pieces {
            lines.push(DocLine {
                indent,
                runs: std::mem::take(&mut current),
            });
            if !piece.is_empty() {
                current.push(DocRun {
                    text: piece.to_owned(),
                    ..run.clone()
                });
            }
        }
    }
    if !current.is_empty() {
        lines.push(DocLine {
            indent,
            runs: current,
        });
    }
}

fn flatten(inlines: &[DocumentationInline], out: &mut Vec<DocRun>) {
    for inline in inlines {
        match inline {
            DocumentationInline::Text(text) => out.push(DocRun::text(text.clone())),
            DocumentationInline::Code(text) => out.push(DocRun {
                text: text.clone(),
                kind: DocRunKind::Code,
                target: None,
            }),
            DocumentationInline::Strong(children) => {
                let before = out.len();
                flatten(children, out);
                for run in &mut out[before..] {
                    run.kind = DocRunKind::Strong;
                }
            }
            DocumentationInline::Emphasis(children) => {
                let before = out.len();
                flatten(children, out);
                for run in &mut out[before..] {
                    run.kind = DocRunKind::Emphasis;
                }
            }
            DocumentationInline::Link { text, target } => out.push(DocRun {
                text: text.clone(),
                kind: DocRunKind::Link,
                target: Some(target.clone()),
            }),
        }
    }
}

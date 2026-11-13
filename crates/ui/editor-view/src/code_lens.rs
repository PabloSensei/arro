//! The labels a language puts above declarations and what a click on one opens.
//!
//! The editor asks for the labels of the whole document, keeps the ones of the visible
//! rows and resolves the commands of those it shows. The labels live in the block layer
//! of the display map, so they take rows and no byte of the text.

use std::collections::BTreeMap;
use std::ops::Range;
use std::time::Duration;

use futures_util::StreamExt;
use lang_api::{
    CodeLensCommand, CodeLensEntry, CodeLensKind, CodeLensQuery, CodeLensResolveQuery, Confidence,
    NavigationKind, NavigationQuery, ask,
};
use text_core::{RopeStore, TextStore};
use ui_kit::*;

use crate::semantic::{first, offset_at, point_at};
use crate::view::{EditorEvent, EditorView, Usage, UsagesUpdate};

/// One label of the server, with its place in this text.
#[derive(Clone, Debug)]
pub(crate) struct CodeLensItem {
    pub offset: usize,
    pub title: String,
    pub kind: CodeLensKind,
    pub resolve: Option<String>,
    pub command: Option<CodeLensCommand>,
}

impl CodeLensItem {
    fn from_entry(entry: CodeLensEntry, store: &RopeStore) -> Self {
        let title = if entry.title.trim().is_empty() {
            entry
                .command
                .as_ref()
                .map(|command| command.title.clone())
                .unwrap_or_default()
        } else {
            entry.title
        };
        let command = entry.command;
        let kind = if entry.kind == CodeLensKind::Other {
            lang_api::kind_of(
                &title,
                command.as_ref().map(|command| command.command.as_str()),
            )
        } else {
            entry.kind
        };
        let offset = offset_at(store, entry.span.start);
        Self {
            offset,
            title,
            kind,
            resolve: entry.resolve,
            command,
        }
    }
}

impl EditorView {
    /// Asks the server for the labels of the document after `delay`. A newer question
    /// replaces the older one, and its answer is dropped.
    pub(crate) fn request_code_lens(&mut self, delay: Duration, cx: &mut Context<Self>) {
        self.code_lens_task = None;
        if !self.options.code_vision_usages && !self.options.code_vision_implementations {
            if let Some(state) = &mut self.state {
                state.set_labels(Vec::new());
            }
            self.code_lens.clear();
            self.code_vision_labels.clear();
            cx.notify();
            return;
        }
        let (query, version, support) = {
            let Some(state) = &self.state else {
                return;
            };
            let Some(lsp) = &self.lsp else {
                return;
            };
            if state.is_read_only() || lsp.support.code_lens().is_none() {
                return;
            }
            (
                CodeLensQuery {
                    uri: lsp.uri.clone(),
                    version: state.version() as i32,
                },
                state.version() as i32,
                lsp.support.clone(),
            )
        };
        self.code_lens_asked = self.code_lens_asked.wrapping_add(1);
        let generation = self.code_lens_asked;
        self.code_lens_task = Some(cx.spawn(async move |this, cx| {
            if !delay.is_zero() {
                cx.background_executor().timer(delay).await;
            }
            let Some(provider) = support.code_lens() else {
                return;
            };
            let mut replies = ask(provider, query);
            while let Some(answer) = replies.next().await {
                if answer.confidence == Confidence::Unknown {
                    continue;
                }
                let applied = this.update(cx, |this, cx| {
                    if this.code_lens_asked != generation
                        || this.state.as_ref().map(|state| state.version() as i32) != Some(version)
                    {
                        return false;
                    }
                    this.apply_code_lens(answer.value, version, cx);
                    true
                });
                if !matches!(applied, Ok(true)) {
                    return;
                }
            }
        }));
    }

    fn apply_code_lens(
        &mut self,
        entries: Vec<CodeLensEntry>,
        version: i32,
        cx: &mut Context<Self>,
    ) {
        let Some(state) = &self.state else {
            return;
        };
        if state.version() as i32 != version {
            return;
        }
        let store = state.store();
        let mut fresh: Vec<CodeLensItem> = entries
            .into_iter()
            .map(|entry| CodeLensItem::from_entry(entry, &store))
            .collect();
        keep_shown_titles(&self.code_lens, &mut fresh);
        self.code_lens = fresh;
        self.code_lens_resolved.clear();
        self.refresh_code_vision(cx);
        self.resolve_visible_code_lens(cx);
    }

    /// Moves the labels along the edits, so the rows stay above their declarations
    /// until the server answers about the new text. A label inside a replaced piece
    /// goes away.
    pub(crate) fn shift_code_lens(&mut self, change: &text_core::Change, cx: &mut Context<Self>) {
        for step in &change.steps {
            for edit in step {
                self.code_lens.retain(|item| {
                    !(edit.range.start < item.offset && item.offset < edit.range.end)
                });
            }
            for item in &mut self.code_lens {
                item.offset = text_core::transform_offset(item.offset, step);
            }
        }
        self.code_lens_resolved.clear();
        self.refresh_code_vision(cx);
    }

    /// Rebuilds the block rows from the labels the editor keeps and the settings.
    fn refresh_code_vision(&mut self, cx: &mut Context<Self>) {
        let usages = self.options.code_vision_usages;
        let implementations = self.options.code_vision_implementations;
        let Some(state) = &mut self.state else {
            return;
        };
        let store = state.store();
        let tab_size = self.options.tab_size.max(1);
        let mut rows: BTreeMap<usize, (Option<String>, Option<String>)> = BTreeMap::new();
        for item in &self.code_lens {
            let Some((kind, label)) = code_vision_label(item, usages, implementations) else {
                continue;
            };
            let line = store.offset_to_line(item.offset.min(store.len()));
            let entry = rows.entry(line).or_default();
            match kind {
                CodeLensKind::Usages => entry.0 = Some(label),
                CodeLensKind::Implementations | CodeLensKind::Inheritors => {
                    entry.1 = Some(label);
                }
                CodeLensKind::Other => {}
            }
        }
        let mut blocks = Vec::with_capacity(rows.len());
        let mut labels = Vec::with_capacity(rows.len());
        for (line, (usages, implementations)) in rows {
            let parts: Vec<(CodeLensKind, String)> = [
                (CodeLensKind::Usages, usages),
                (CodeLensKind::Implementations, implementations),
            ]
            .into_iter()
            .filter_map(|(kind, label)| label.map(|label| (kind, label)))
            .collect();
            if parts.is_empty() {
                continue;
            }
            let text = parts
                .iter()
                .map(|(_, label)| label.as_str())
                .collect::<Vec<_>>()
                .join("  ");
            let indent = indent_columns(&store.line_text(line), tab_size);
            blocks.push(display_map::Block {
                offset: store.line_to_offset(line),
                text,
                indent,
                after: false,
            });
            labels.push((line, parts));
        }
        state.set_labels(blocks);
        self.code_vision_labels = labels;
        self.code_vision_hovered = None;
        cx.notify();
    }

    /// The parts of the block row above `line`, in the order the row draws them.
    pub(crate) fn code_vision_labels(&self, line: usize) -> &[(CodeLensKind, String)] {
        self.code_vision_labels
            .iter()
            .find(|(at, _)| *at == line)
            .map_or(&[], |(_, labels)| labels.as_slice())
    }

    /// The label rows the editor shows, for a test: the line and its text.
    pub fn code_vision_texts(&self) -> Vec<(usize, String)> {
        self.state
            .as_ref()
            .map(|state| {
                state
                    .blocks()
                    .iter()
                    .map(|block| {
                        (
                            state.store().offset_to_line(block.offset),
                            block.text.clone(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolves the labels of the visible rows that the server sent without a command.
    pub(crate) fn resolve_visible_code_lens(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let Some(lsp) = &self.lsp else {
            return;
        };
        if lsp.support.code_lens_resolve().is_none() {
            return;
        }
        let visible = self.visible_lines(state);
        let version = state.version() as i32;
        let store = state.store();
        let wanted: Vec<(usize, CodeLensResolveQuery)> = self
            .code_lens
            .iter()
            .filter(|item| item.command.is_none() && item.resolve.is_some())
            .filter(|item| !self.code_lens_resolved.contains(&item.offset))
            .filter(|item| {
                let line = store.offset_to_line(item.offset.min(store.len()));
                visible.contains(&line)
            })
            .map(|item| {
                (
                    item.offset,
                    CodeLensResolveQuery {
                        uri: lsp.uri.clone(),
                        version,
                        lens: CodeLensEntry {
                            span: lang_api::TextSpan {
                                start: point_at(&store, item.offset.min(store.len())),
                                end: point_at(&store, item.offset.min(store.len())),
                            },
                            title: item.title.clone(),
                            kind: item.kind,
                            resolve: item.resolve.clone(),
                            command: item.command.clone(),
                        },
                    },
                )
            })
            .collect();
        if wanted.is_empty() {
            return;
        }
        let support = lsp.support.clone();
        let offsets: Vec<usize> = wanted.iter().map(|(offset, _)| *offset).collect();
        self.code_lens_resolved.extend(offsets);
        cx.spawn(async move |this, cx| {
            for (offset, query) in wanted {
                let Some(provider) = support.code_lens_resolve() else {
                    return;
                };
                let mut replies = ask(provider, query);
                let Some(answer) = replies.next().await else {
                    continue;
                };
                if answer.confidence == Confidence::Unknown {
                    continue;
                }
                let applied = this.update(cx, |this, cx| {
                    if this.state.as_ref().map(|state| state.version() as i32) != Some(version) {
                        return false;
                    }
                    let Some(state) = &this.state else {
                        return false;
                    };
                    let store = state.store();
                    let resolved = CodeLensItem::from_entry(answer.value, &store);
                    let Some(item) = this.code_lens.iter_mut().find(|item| item.offset == offset)
                    else {
                        return false;
                    };
                    *item = resolved;
                    this.refresh_code_vision(cx);
                    true
                });
                if !matches!(applied, Ok(true)) {
                    return;
                }
            }
        })
        .detach();
    }

    /// The buffer lines of the rows on the screen.
    fn visible_lines(&self, state: &crate::state::EditorState) -> Range<usize> {
        let display = state.display();
        let line_height = self.style.line_height.max(1.);
        let shown = (f32::from(self.viewport.height) / line_height).ceil() as usize;
        let rows = if shown == 0 { 40 } else { shown };
        let first = (self.scroll_top / line_height).floor().max(0.) as usize;
        let last = (first + rows).min(display.row_count().saturating_sub(1));
        let start = display
            .line(first.min(display.row_count().saturating_sub(1)))
            .start_offset;
        let end = display.line(last).end_offset;
        let store = state.store();
        store.offset_to_line(start)..(store.offset_to_line(end) + 1)
    }

    /// A click on a label: Show Usages for the counts, the implementations otherwise.
    pub(crate) fn activate_code_lens(
        &mut self,
        line: usize,
        kind: CodeLensKind,
        cx: &mut Context<Self>,
    ) {
        let Some(item) = self
            .code_lens
            .iter()
            .find(|item| {
                matches!(item.kind, CodeLensKind::Usages) == matches!(kind, CodeLensKind::Usages)
                    && self.state.as_ref().is_some_and(|state| {
                        state
                            .store()
                            .offset_to_line(item.offset.min(state.store().len()))
                            == line
                    })
            })
            .cloned()
        else {
            return;
        };
        if let Some(state) = &mut self.state {
            state.select_range(item.offset..item.offset);
        }
        match kind {
            CodeLensKind::Usages => self.usage_search(true, cx),
            CodeLensKind::Implementations | CodeLensKind::Inheritors => {
                self.show_implementations(item.offset, cx);
            }
            CodeLensKind::Other => {}
        }
    }

    fn show_implementations(&mut self, offset: usize, cx: &mut Context<Self>) {
        let Some((spot, _)) = self.spot() else {
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.navigation()) else {
            return;
        };
        let name = self.word_at_caret(offset);
        let replies = ask(
            provider,
            NavigationQuery {
                kind: NavigationKind::Implementation,
                spot,
            },
        );
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((spans, _)) if spans.len() == 1 => this.go_to_location(&spans[0], cx),
                Some((spans, _)) if !spans.is_empty() => {
                    cx.emit(EditorEvent::ShowUsages(UsagesUpdate {
                        name,
                        usages: spans
                            .into_iter()
                            .map(|target| Usage {
                                name: String::new(),
                                target,
                            })
                            .collect(),
                        done: true,
                    }));
                }
                Some((_, Confidence::Unknown)) => {
                    this.show_note(offset, crate::semantic::NO_ANSWER, cx)
                }
                Some(_) => this.show_note(offset, "No implementations found", cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }
}

/// A server may answer every edit with labels that have no title yet and resolve them
/// one by one later; a row without a title has no text, so it would leave the screen
/// and come back, and the code under it would jump on every key. Until its resolve
/// answers, such a label keeps the title the label at the same place had: the row stays,
/// and only its count changes when the new one arrives. The command is not taken over,
/// so the label is still resolved.
fn keep_shown_titles(shown: &[CodeLensItem], fresh: &mut [CodeLensItem]) {
    let mut taken = vec![false; shown.len()];
    for item in fresh.iter_mut().filter(|item| item.title.trim().is_empty()) {
        let at_place = |index: &usize| {
            !taken[*index]
                && shown[*index].offset == item.offset
                && !shown[*index].title.trim().is_empty()
        };
        let same_kind = (0..shown.len())
            .filter(at_place)
            .find(|index| item.kind != CodeLensKind::Other && shown[*index].kind == item.kind);
        let Some(index) = same_kind.or_else(|| (0..shown.len()).find(at_place)) else {
            continue;
        };
        taken[index] = true;
        item.title = shown[index].title.clone();
        if item.kind == CodeLensKind::Other {
            item.kind = shown[index].kind;
        }
    }
}

/// The label of a row, with the wording IDEA uses. `None` for a label Settings hides
/// or a label that says nothing about a count.
fn code_vision_label(
    item: &CodeLensItem,
    usages: bool,
    implementations: bool,
) -> Option<(CodeLensKind, String)> {
    let title = item.title.trim();
    if title.is_empty() {
        return None;
    }
    let count = leading_count(title);
    match item.kind {
        CodeLensKind::Usages if usages => Some((
            CodeLensKind::Usages,
            match count {
                Some(0) => "no usages".to_owned(),
                Some(1) => "1 usage".to_owned(),
                Some(count) => format!("{count} usages"),
                None => title.to_owned(),
            },
        )),
        CodeLensKind::Implementations if implementations => Some((
            CodeLensKind::Implementations,
            match count {
                Some(1) => "1 implementation".to_owned(),
                Some(count) => format!("{count} implementations"),
                None => title.to_owned(),
            },
        )),
        CodeLensKind::Inheritors if implementations => Some((
            CodeLensKind::Inheritors,
            match count {
                Some(1) => "1 inheritor".to_owned(),
                Some(count) => format!("{count} inheritors"),
                None => title.to_owned(),
            },
        )),
        _ => None,
    }
}

fn leading_count(title: &str) -> Option<u32> {
    title
        .split_whitespace()
        .next()
        .and_then(|word| word.parse().ok())
}

/// The leading columns of a line, a tab taking the next stop.
fn indent_columns(text: &str, tab_size: usize) -> usize {
    let mut columns = 0;
    for character in text.chars() {
        match character {
            ' ' => columns += 1,
            '\t' => columns += tab_size - columns % tab_size,
            _ => break,
        }
    }
    columns
}

#[cfg(test)]
mod tests {
    use lang_api::{CodeLensCommand, CodeLensKind};

    use super::{CodeLensItem, keep_shown_titles};

    fn item(offset: usize, title: &str, kind: CodeLensKind, resolved: bool) -> CodeLensItem {
        CodeLensItem {
            offset,
            title: title.to_owned(),
            kind,
            resolve: Some("data".to_owned()),
            command: resolved.then(|| CodeLensCommand {
                title: title.to_owned(),
                command: "show".to_owned(),
                arguments: None,
            }),
        }
    }

    /// The answer to an edit comes without titles; the rows keep their text until the
    /// labels resolve, so the code under them does not jump.
    #[test]
    fn an_unresolved_label_keeps_the_title_shown_at_its_place() {
        let shown = [
            item(10, "2 usages", CodeLensKind::Usages, true),
            item(10, "0 implementations", CodeLensKind::Implementations, true),
            item(40, "1 usage", CodeLensKind::Usages, true),
        ];
        let mut fresh = vec![
            item(10, "", CodeLensKind::Other, false),
            item(10, "", CodeLensKind::Other, false),
            item(40, "", CodeLensKind::Usages, false),
            item(70, "", CodeLensKind::Other, false),
        ];

        keep_shown_titles(&shown, &mut fresh);

        let titles: Vec<(&str, CodeLensKind)> = fresh
            .iter()
            .map(|item| (item.title.as_str(), item.kind))
            .collect();
        assert_eq!(
            titles,
            [
                ("2 usages", CodeLensKind::Usages),
                ("0 implementations", CodeLensKind::Implementations),
                ("1 usage", CodeLensKind::Usages),
                ("", CodeLensKind::Other),
            ],
            "a label at a new place has nothing to keep"
        );
        assert!(
            fresh.iter().all(|item| item.command.is_none()),
            "the labels are still resolved"
        );
    }

    /// A label the server already resolved keeps its own title.
    #[test]
    fn a_resolved_label_keeps_its_own_title() {
        let shown = [item(10, "2 usages", CodeLensKind::Usages, true)];
        let mut fresh = vec![item(10, "3 usages", CodeLensKind::Usages, true)];

        keep_shown_titles(&shown, &mut fresh);

        assert_eq!(fresh[0].title, "3 usages");
    }

    /// The kind decides which shown title a label of a known kind takes.
    #[test]
    fn a_label_of_a_known_kind_takes_the_title_of_that_kind() {
        let shown = [
            item(10, "2 usages", CodeLensKind::Usages, true),
            item(10, "1 implementation", CodeLensKind::Implementations, true),
        ];
        let mut fresh = vec![item(10, "", CodeLensKind::Implementations, false)];

        keep_shown_titles(&shown, &mut fresh);

        assert_eq!(fresh[0].title, "1 implementation");
    }
}

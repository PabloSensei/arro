//! Live templates in the editor: Tab after an abbreviation or a postfix key, Ctrl+J for the
//! list, Ctrl+Alt+T to surround the selected lines, Tab and Shift+Tab through the fields.
//! [`LiveTemplates`] works on [`EditorState`] alone; the view adds keys and the list.

use std::ops::Range;

use keymap::{Keystroke, editor as actions};
use templates::{
    Expansion, Kind, Session, Step, Template, TemplateLanguage, context_at, expand, postfix_at,
    surround, word_before,
};
use text_core::{Change, Edit, EditKind, Selection, SelectionSet, TextStore};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::element::{GUTTER_WIDTH, TEXT_INSET};
use crate::state::EditorState;
use crate::view::EditorView;

const SHOWN: usize = 12;

/// The list Ctrl+J or Ctrl+Alt+T opened.
pub struct TemplateList {
    pub kind: Kind,
    pub items: Vec<&'static Template>,
    pub chosen: usize,
}

/// What a key did.
#[derive(Debug, PartialEq, Eq)]
pub enum KeyUse {
    /// Not a key of the list or of the fields: the editor handles it.
    Unused,
    Used(Option<Change>),
}

/// The template state of one editor.
pub struct LiveTemplates {
    language: Option<&'static TemplateLanguage>,
    session: Option<Session>,
    list: Option<TemplateList>,
    /// The change that started the session, which the session already knows about.
    started: bool,
}

/// The text of the caret's line up to the caret, and where that line starts.
fn line_before(state: &EditorState, caret: usize) -> (String, usize) {
    let store = state.store();
    let start = store.line_to_offset(store.offset_to_line(caret));
    (store.slice(start..caret), start)
}

fn indent_of(line: &str) -> &str {
    &line[..line.len() - line.trim_start().len()]
}

/// A tab when the line is indented with tabs, else the unit of the open file.
fn unit_for(indent: &str, state: &EditorState) -> String {
    if indent.starts_with('\t') {
        "\t".to_owned()
    } else {
        state.indent().unit.clone()
    }
}

/// The single empty caret, the only place templates expand.
fn lone_caret(state: &EditorState) -> Option<usize> {
    let selections = state.selections();
    let primary = selections.primary();
    (selections.len() == 1 && primary.is_empty()).then_some(primary.head)
}

fn context(
    language: &'static TemplateLanguage,
    state: &EditorState,
    offset: usize,
) -> &'static str {
    let text = state.text();
    context_at(language, state.tree(), text.as_bytes(), offset)
}

impl LiveTemplates {
    pub fn new(language: Option<&'static TemplateLanguage>) -> Self {
        Self {
            language,
            session: None,
            list: None,
            started: false,
        }
    }

    pub fn list(&self) -> Option<&TemplateList> {
        self.list.as_ref()
    }

    /// A template is being filled in.
    pub fn is_active(&self) -> bool {
        self.session.is_some()
    }

    /// Replaces `range` with the expansion and selects its first field, as one step of undo.
    fn insert(
        &mut self,
        state: &mut EditorState,
        range: Range<usize>,
        expansion: &Expansion,
    ) -> Option<Change> {
        let at = range.start;
        let session = Session::start(expansion, at);
        let after = match &session {
            Some(session) => Selection::new(session.current().start, session.current().end),
            None => Selection::caret(at + expansion.end),
        };
        let edit = Edit::new(range, expansion.text.clone());
        let change =
            state.apply_template_edits(vec![edit], SelectionSet::single(after), EditKind::Command);
        self.session = change.as_ref().and(session);
        self.started = self.session.is_some();
        change
    }

    /// The indentation of the line with `at` and the unit a tab of a snippet means there.
    pub fn snippet_indent(&self, state: &EditorState, at: usize) -> (String, String) {
        let (line, _) = line_before(state, at.min(state.text().len()));
        let indent = indent_of(&line).to_string();
        let unit = unit_for(&indent, state);
        (indent, unit)
    }

    /// Starts walking the fields of `expansion`, inserted at `at`.
    pub fn start_session(&mut self, expansion: &Expansion, at: usize) {
        self.session = Session::start(expansion, at);
        self.started = self.session.is_some();
    }

    /// Tab in an open session: the next field, or its end.
    pub fn next_field(&mut self, state: &mut EditorState) -> Option<Option<Change>> {
        let head = state.selections().primary().head;
        let step = {
            let session = self.session.as_mut()?;
            if !session.contains(head) {
                self.session = None;
                return None;
            }
            session.advance()
        };
        Some(self.apply_step(state, step))
    }

    /// Shift+Tab in an open session: the previous field.
    pub fn previous_field(&mut self, state: &mut EditorState) -> Option<Option<Change>> {
        let head = state.selections().primary().head;
        let step = {
            let session = self.session.as_mut()?;
            if !session.contains(head) {
                self.session = None;
                return None;
            }
            session.back()
        };
        Some(self.apply_step(state, step))
    }

    fn apply_step(&mut self, state: &mut EditorState, step: Step) -> Option<Change> {
        match step {
            Step::Select(range) => state.select_range(range),
            Step::Finish(end) => {
                state.select_range(end..end);
                self.session = None;
            }
        }
        None
    }

    /// Runs an editor action. `None` when the action is not a template one or nothing fits
    /// here, so the editor runs it as usual.
    pub fn action(&mut self, state: &mut EditorState, action: &str) -> Option<Option<Change>> {
        let language = self.language?;
        match action {
            actions::TAB => self.expand_at_caret(language, state),
            actions::INSERT_TEMPLATE => {
                let caret = lone_caret(state)?;
                let (line, _) = line_before(state, caret);
                let prefix = word_before(&line)
                    .map_or("", |word| &line[word])
                    .to_lowercase();
                let place = context(language, state, caret - prefix.len());
                let fitting = language.fitting(Kind::Abbreviation, place);
                let items =
                    fitting.filter(|template| template.key.to_lowercase().starts_with(&prefix));
                self.open(Kind::Abbreviation, items.collect());
                Some(None)
            }
            actions::SURROUND_WITH => {
                let head = state.selections().primary().head;
                let place = context(language, state, head);
                let items = language.fitting(Kind::Surround, place).collect();
                self.open(Kind::Surround, items);
                Some(None)
            }
            actions::SURROUND_WITH_LIVE_TEMPLATE => {
                let head = state.selections().primary().head;
                let place = context(language, state, head);
                let items = language
                    .fitting(Kind::Surround, place)
                    .chain(language.fitting(Kind::SurroundLive, place))
                    .filter(|template| template.body.contains("$SELECTION$"))
                    .collect();
                self.open(Kind::Surround, items);
                Some(None)
            }
            _ => None,
        }
    }

    /// A postfix key or an abbreviation right before the caret, expanded.
    fn expand_at_caret(
        &mut self,
        language: &'static TemplateLanguage,
        state: &mut EditorState,
    ) -> Option<Option<Change>> {
        let caret = lone_caret(state)?;
        let (line, start) = line_before(state, caret);
        let indent = indent_of(&line).to_string();
        let unit = unit_for(&indent, state);
        if let Some((receiver, key)) = postfix_at(&line, language.receiver_prefixes) {
            let place = context(language, state, start + receiver.start);
            if let Some(template) = language.find(Kind::Postfix, key, place) {
                let expression = &line[receiver.clone()];
                let expansion = expand(template, &indent, &unit, None, Some(expression));
                return Some(self.insert(state, start + receiver.start..caret, &expansion));
            }
        }
        let word = word_before(&line)?;
        let place = context(language, state, start + word.start);
        let template = language.find(Kind::Abbreviation, &line[word.clone()], place)?;
        let expansion = expand(template, &indent, &unit, None, None);
        Some(self.insert(state, start + word.start..caret, &expansion))
    }

    fn open(&mut self, kind: Kind, items: Vec<&'static Template>) {
        self.list = (!items.is_empty()).then_some(TemplateList {
            kind,
            items,
            chosen: 0,
        });
    }

    /// Puts `template` in: an abbreviation in place of the word before the caret, a surround
    /// template around the selected lines, or the current line without a selection.
    pub fn apply(
        &mut self,
        state: &mut EditorState,
        template: &'static Template,
    ) -> Option<Change> {
        self.language?;
        match template.kind {
            Kind::Surround | Kind::SurroundLive => {
                let store = state.store();
                let selection = state.selections().primary();
                let first = store.offset_to_line(selection.start());
                let mut last = store.offset_to_line(selection.end());
                if last > first && store.line_to_offset(last) == selection.end() {
                    last -= 1;
                }
                let range = store.line_to_offset(first)..store.line_end(last);
                let lines = store.slice(range.clone());
                let unit = unit_for(indent_of(&lines), state);
                let expansion = surround(template, &lines, &unit);
                self.insert(state, range, &expansion)
            }
            _ => {
                let caret = lone_caret(state)?;
                let (line, start) = line_before(state, caret);
                let word = word_before(&line).unwrap_or(line.len()..line.len());
                let indent = indent_of(&line);
                let expansion = expand(template, indent, &unit_for(indent, state), None, None);
                self.insert(state, start + word.start..caret, &expansion)
            }
        }
    }

    /// Keys of the open list and of the fields: arrows, Enter, Tab, Shift+Tab, Escape.
    pub fn key(&mut self, state: &mut EditorState, stroke: &Keystroke) -> KeyUse {
        let plain = !stroke.ctrl && !stroke.alt;
        if let Some(list) = &mut self.list {
            let count = list.items.len().min(SHOWN);
            match stroke.key.as_str() {
                "up" if plain => list.chosen = (list.chosen + count - 1) % count,
                "down" if plain => list.chosen = (list.chosen + 1) % count,
                "enter" | "tab" if plain && !stroke.shift => {
                    let template = list.items[list.chosen];
                    self.list = None;
                    return KeyUse::Used(self.apply(state, template));
                }
                "escape" => self.list = None,
                _ => {
                    self.list = None;
                    return KeyUse::Unused;
                }
            }
            return KeyUse::Used(None);
        }
        let Some(session) = &mut self.session else {
            return KeyUse::Unused;
        };
        if !session.contains(state.selections().primary().head) {
            self.session = None;
            return KeyUse::Unused;
        }
        let step = match stroke.key.as_str() {
            "tab" if plain && stroke.shift => session.back(),
            "tab" if plain => session.advance(),
            "enter" if plain && !stroke.shift => session.advance(),
            "escape" => {
                self.session = None;
                return KeyUse::Used(None);
            }
            _ => return KeyUse::Unused,
        };
        match step {
            Step::Select(range) => state.select_range(range),
            Step::Finish(end) => {
                state.select_range(end..end);
                self.session = None;
            }
        }
        KeyUse::Used(None)
    }

    /// Keeps the fields in place across a change and copies a typed field to its copies.
    /// Returns the change with the copying added.
    pub fn follow(&mut self, state: &mut EditorState, change: Option<Change>) -> Option<Change> {
        let mut change = change?;
        if std::mem::take(&mut self.started) {
            return Some(change);
        }
        let Some(session) = &mut self.session else {
            return Some(change);
        };
        if !session.follow(&change) {
            self.session = None;
            return Some(change);
        }
        let mirrors = session.mirror_edits(|range| state.store().slice(range));
        if mirrors.is_empty() {
            return Some(change);
        }
        // The carets move along with copies in front of them.
        let selections = state.selections().transformed(&mirrors);
        if let Some(mirrored) = state.apply_template_edits(mirrors, selections, EditKind::Typing) {
            if !session.follow(&mirrored) {
                self.session = None;
            }
            change.steps.extend(mirrored.steps);
        }
        Some(change)
    }

    /// Keeps the fields in place across an edit that arrives after the insertion, such
    /// as the import of a late resolve. The edit may be outside the fields.
    pub fn follow_late(&mut self, change: Option<Change>) -> Option<Change> {
        let change = change?;
        if let Some(session) = &mut self.session {
            session.follow_around(&change);
        }
        Some(change)
    }

    /// The templates that fit at the caret, for a completion list next to the server's
    /// answers.
    pub fn suggestions(&self, state: &EditorState) -> Vec<&'static Template> {
        let (Some(language), Some(caret)) = (self.language, lone_caret(state)) else {
            return Vec::new();
        };
        let (line, _) = line_before(state, caret);
        let prefix = word_before(&line).map_or("", |word| &line[word]);
        let place = context(language, state, caret - prefix.len());
        let fitting = language.fitting(Kind::Abbreviation, place);
        fitting
            .filter(|template| template.key.starts_with(prefix))
            .collect()
    }
}

impl EditorView {
    pub(crate) fn template_action(&mut self, action: &str) -> Option<Option<Change>> {
        let state = self.state.as_mut()?;
        let change = self.live.action(state, action)?;
        Some(change)
    }

    /// `true` when the key went to the list or the fields.
    pub(crate) fn template_key(&mut self, stroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        let Some(state) = &mut self.state else {
            return false;
        };
        let had_list = self.live.list().is_some();
        match self.live.key(state, stroke) {
            KeyUse::Unused => {
                if had_list {
                    cx.notify();
                }
                let plain = !stroke.ctrl && !stroke.alt;
                if plain && stroke.key == "tab" {
                    return self.parameter_field(!stroke.shift, cx);
                }
                false
            }
            KeyUse::Used(change) => {
                self.changed(change, cx);
                true
            }
        }
    }

    /// Moves to the next or previous field of an open session. `None` when no session
    /// runs, so the key keeps its usual meaning.
    pub(crate) fn template_field(&mut self, forward: bool) -> Option<Option<Change>> {
        let state = self.state.as_mut()?;
        if forward {
            self.live.next_field(state)
        } else {
            self.live.previous_field(state)
        }
    }

    pub(crate) fn follow_template(&mut self, change: Option<Change>) -> Option<Change> {
        match &mut self.state {
            Some(state) => self.live.follow(state, change),
            None => change,
        }
    }

    /// The template list under the caret.
    pub(crate) fn template_popup(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let list = self.live.list()?;
        let theme = *cx.global::<Theme>();
        let position = self.caret_point().unwrap_or_default();
        let mut shown = ui_kit::surface(&theme)
            .debug_selector(|| "template-popup".to_owned())
            .min_w(px(280.))
            .py(px(4.));
        for (index, template) in list.items.iter().copied().take(SHOWN).enumerate() {
            let row = div()
                .id(("template", index))
                .flex()
                .gap(px(12.))
                .px(px(10.))
                .py(px(3.))
                .when(index == list.chosen, |row| row.bg(theme.selection))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, _, _, cx| {
                        let change = this.state.as_mut().and_then(|state| {
                            this.live.list = None;
                            this.live.apply(state, template)
                        });
                        this.changed(change, cx);
                    }),
                )
                .child(div().text_color(theme.text).child(template.key))
                .child(
                    div()
                        .text_color(theme.text_secondary)
                        .child(template.description),
                );
            shown = shown.child(row);
        }
        let placed = anchored()
            .position(position)
            .snap_to_window_with_margin(px(SPACE_M))
            .child(shown);
        Some(deferred(placed).with_priority(1).into_any_element())
    }

    /// The window position under the primary caret, from the last painted frame.
    pub fn caret_point(&self) -> Option<Point<Pixels>> {
        let layout = self.layout.as_ref()?;
        let state = self.state.as_ref()?;
        let head = state.selections().primary().head;
        let row = state.display().to_display(head);
        let shown = layout
            .rows
            .iter()
            .find(|laid_out| laid_out.row == row.row)?;
        let x = layout.bounds.left()
            + px(GUTTER_WIDTH + TEXT_INSET)
            + shown.text.x_for_index(row.column)
            - px(self.scroll_left);
        Some(point(x, shown.y + px(self.style.line_height)))
    }

    /// The templates that fit at the caret, key and description, for a completion list next
    /// to the server's answers.
    pub fn template_suggestions(&self) -> Vec<(&'static str, &'static str)> {
        let Some(state) = &self.state else {
            return Vec::new();
        };
        let found = self.live.suggestions(state).into_iter();
        found
            .map(|template| (template.key, template.description))
            .collect()
    }

    /// Expands the template `key` in place of the word before the caret, as a completion
    /// list does when one of [`Self::template_suggestions`] is picked.
    pub fn insert_template(&mut self, key: &str, cx: &mut Context<Self>) {
        let Some(state) = &mut self.state else {
            return;
        };
        let found = self.live.suggestions(state).into_iter();
        let template = found.into_iter().find(|template| template.key == key);
        if let Some(template) = template {
            let change = self.live.apply(state, template);
            self.changed(change, cx);
        }
    }
}

//! Live templates in the editor: Tab after an abbreviation or a postfix key, Ctrl+J for the
//! list, Ctrl+Alt+T to surround the selected lines, Tab and Shift+Tab through the fields.
//! [`LiveTemplates`] works on [`EditorState`] alone; the view adds keys and the list.

use std::ops::Range;

use keymap::{Keystroke, editor as actions};
use templates::{
    ExpandWith, Expansion, ExpansionSite, Kind, ResolvedTemplate, Session, Step, Template,
    TemplateLanguage, Variables, context_at, declarations, expand_fields, postfix_at,
    surround_fields, word_before,
};
use text_core::{Change, Edit, EditKind, Selection, SelectionSet, TextStore};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::element::{GUTTER_WIDTH, TEXT_INSET};
use crate::state::EditorState;
use crate::view::{EditorView, TemplateSettings};

const SHOWN: usize = 12;

/// The list Ctrl+J or Ctrl+Alt+T opened.
pub struct TemplateList {
    pub kind: Kind,
    pub items: Vec<Template>,
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
    /// The built-in templates with the changes of the user, resolved once per change.
    templates: Vec<ResolvedTemplate>,
    /// The generation of the changes of the user the templates came from.
    resolved: Option<usize>,
    session: Option<Session>,
    list: Option<TemplateList>,
    /// The change that started the session, which the session already knows about.
    started: bool,
    /// The name of the open file, for the variables of a template.
    file_name: String,
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
            templates: language.map_or_else(Vec::new, |language| {
                language
                    .templates
                    .iter()
                    .map(|template| ResolvedTemplate {
                        template: *template,
                        expand_with: ExpandWith::Tab,
                        fields: template
                            .fields
                            .iter()
                            .map(|(name, default)| templates::TemplateField {
                                name: (*name).to_owned(),
                                default: (*default).to_owned(),
                                ..templates::TemplateField::default()
                            })
                            .collect(),
                    })
                    .collect()
            }),
            resolved: None,
            session: None,
            list: None,
            started: false,
            file_name: String::new(),
        }
    }

    /// The name of the open file, for `fileName()` and the names of the language.
    pub fn set_file_name(&mut self, name: &str) {
        if self.file_name != name {
            self.file_name.clear();
            self.file_name.push_str(name);
        }
    }

    /// Takes the templates of the user over the built-in set once the file changed.
    pub fn set_overrides(&mut self, overrides: &templates::TemplateOverrides, generation: usize) {
        if self.resolved == Some(generation) {
            return;
        }
        self.templates = self
            .language
            .map_or_else(Vec::new, |language| language.resolved(overrides));
        self.resolved = Some(generation);
    }

    fn find(&self, kind: Kind, key: &str, context: &str) -> Option<&ResolvedTemplate> {
        self.templates.iter().find(|entry| {
            entry.template.kind == kind && entry.template.key == key && entry.template.fits(context)
        })
    }

    fn fitting(&self, kind: Kind, context: &str) -> impl Iterator<Item = &ResolvedTemplate> {
        self.templates
            .iter()
            .filter(move |entry| entry.template.kind == kind && entry.template.fits(context))
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

    /// Drops the open session: its ranges point into text this view no longer has.
    pub fn abandon(&mut self) {
        self.session = None;
        self.list = None;
        self.started = false;
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

    /// The values of the moment and the names of the language for the variables of a
    /// template that expands at `offset`.
    fn variables(&self, state: &EditorState, offset: usize, cx: Option<&App>) -> Variables {
        let text = state.text();
        let declarations = self.language.map_or_else(Default::default, |language| {
            declarations(language, &text, state.tree(), offset)
        });
        let clipboard = cx
            .and_then(|cx| cx.read_from_clipboard())
            .and_then(|item| item.text())
            .unwrap_or_default();
        let line_number = state.store().offset_to_line(offset) + 1;
        Variables::now(declarations, &self.file_name, line_number, &clipboard)
    }

    /// Runs an editor action. `None` when the action is not a template one or nothing fits
    /// here, so the editor runs it as usual.
    pub fn action(&mut self, state: &mut EditorState, action: &str) -> Option<Option<Change>> {
        self.action_app(state, action, None)
    }

    /// [`Self::action`] with the clipboard of the window for `clipboard()`.
    pub fn action_with(
        &mut self,
        state: &mut EditorState,
        action: &str,
        cx: &App,
    ) -> Option<Option<Change>> {
        self.action_app(state, action, Some(cx))
    }

    fn action_app(
        &mut self,
        state: &mut EditorState,
        action: &str,
        cx: Option<&App>,
    ) -> Option<Option<Change>> {
        let language = self.language?;
        match action {
            actions::TAB => self.expand_at_caret(language, state, ExpandWith::Tab, cx),
            actions::INSERT_TEMPLATE => {
                let caret = lone_caret(state)?;
                let (line, _) = line_before(state, caret);
                let prefix = word_before(&line)
                    .map_or("", |word| &line[word])
                    .to_lowercase();
                let place = context(language, state, caret - prefix.len());
                let fitting = self.fitting(Kind::Abbreviation, place);
                let items = fitting
                    .filter(|entry| entry.template.key.to_lowercase().starts_with(&prefix))
                    .map(|entry| entry.template)
                    .collect();
                self.open(Kind::Abbreviation, items);
                Some(None)
            }
            actions::SURROUND_WITH => {
                let head = state.selections().primary().head;
                let place = context(language, state, head);
                let items = self
                    .fitting(Kind::Surround, place)
                    .map(|entry| entry.template)
                    .collect();
                self.open(Kind::Surround, items);
                Some(None)
            }
            actions::SURROUND_WITH_LIVE_TEMPLATE => {
                let head = state.selections().primary().head;
                let place = context(language, state, head);
                let items = self
                    .fitting(Kind::Surround, place)
                    .chain(self.fitting(Kind::SurroundLive, place))
                    .filter(|entry| entry.template.body.contains("$SELECTION$"))
                    .map(|entry| entry.template)
                    .collect();
                self.open(Kind::Surround, items);
                Some(None)
            }
            _ => None,
        }
    }

    /// A postfix key or an abbreviation right before the caret, expanded by `key`.
    fn expand_at_caret(
        &mut self,
        language: &'static TemplateLanguage,
        state: &mut EditorState,
        key: ExpandWith,
        cx: Option<&App>,
    ) -> Option<Option<Change>> {
        let caret = lone_caret(state)?;
        let (line, start) = line_before(state, caret);
        let indent = indent_of(&line).to_string();
        let unit = unit_for(&indent, state);
        if key == ExpandWith::Tab
            && let Some((receiver, postfix_key)) = postfix_at(&line, language.receiver_prefixes)
        {
            let place = context(language, state, start + receiver.start);
            if let Some(entry) = self.find(Kind::Postfix, postfix_key, place) {
                let expression = line[receiver.clone()].to_string();
                let site = ExpansionSite {
                    indent: &indent,
                    unit: &unit,
                    selection: None,
                    expression: Some(&expression),
                    variables: self.variables(state, caret, cx),
                };
                let expansion = expand_fields(&entry.template, &entry.fields, &site);
                return Some(self.insert(state, start + receiver.start..caret, &expansion));
            }
        }
        let word = word_before(&line)?;
        let place = context(language, state, start + word.start);
        let entry = self.find(Kind::Abbreviation, &line[word.clone()], place)?;
        if entry.expand_with != key {
            return None;
        }
        let site = ExpansionSite {
            indent: &indent,
            unit: &unit,
            selection: None,
            expression: None,
            variables: self.variables(state, caret, cx),
        };
        let expansion = expand_fields(&entry.template, &entry.fields, &site);
        Some(self.insert(state, start + word.start..caret, &expansion))
    }

    /// Enter or Space at the caret: the template whose abbreviation ends here and whose
    /// Expand with is `key`. `None` when nothing fits, so the key keeps its meaning.
    pub fn expand_key(
        &mut self,
        state: &mut EditorState,
        key: ExpandWith,
        cx: &App,
    ) -> Option<Option<Change>> {
        if key == ExpandWith::Tab || self.list.is_some() || self.session.is_some() {
            return None;
        }
        let language = self.language?;
        self.expand_at_caret(language, state, key, Some(cx))
    }

    fn open(&mut self, kind: Kind, items: Vec<Template>) {
        self.list = (!items.is_empty()).then_some(TemplateList {
            kind,
            items,
            chosen: 0,
        });
    }

    /// The template of the resolved set with the kind and key of `template`.
    fn entry(&self, template: &Template) -> Option<&ResolvedTemplate> {
        self.templates.iter().find(|entry| {
            entry.template.kind == template.kind && entry.template.key == template.key
        })
    }

    /// Puts `template` in: an abbreviation in place of the word before the caret, a surround
    /// template around the selected lines, or the current line without a selection.
    pub fn apply(&mut self, state: &mut EditorState, template: &Template) -> Option<Change> {
        self.apply_app(state, template, None)
    }

    /// [`Self::apply`] with the clipboard of the window for `clipboard()`.
    pub fn apply_with(
        &mut self,
        state: &mut EditorState,
        template: &Template,
        cx: &App,
    ) -> Option<Change> {
        self.apply_app(state, template, Some(cx))
    }

    fn apply_app(
        &mut self,
        state: &mut EditorState,
        template: &Template,
        cx: Option<&App>,
    ) -> Option<Change> {
        self.language?;
        let entry = self.entry(template)?;
        let entry = entry.clone();
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
                let variables = self.variables(state, range.start, cx);
                let expansion =
                    surround_fields(&entry.template, &entry.fields, &lines, &unit, variables);
                self.insert(state, range, &expansion)
            }
            _ => {
                let caret = lone_caret(state)?;
                let (line, start) = line_before(state, caret);
                let word = word_before(&line).unwrap_or(line.len()..line.len());
                let indent = indent_of(&line);
                let unit = unit_for(indent, state);
                let site = ExpansionSite {
                    indent,
                    unit: &unit,
                    selection: None,
                    expression: None,
                    variables: self.variables(state, caret, cx),
                };
                let expansion = expand_fields(&entry.template, &entry.fields, &site);
                self.insert(state, start + word.start..caret, &expansion)
            }
        }
    }

    /// Keys of the open list and of the fields: arrows, Enter, Tab, Shift+Tab, Escape.
    pub fn key(&mut self, state: &mut EditorState, stroke: &Keystroke) -> KeyUse {
        self.key_app(state, stroke, None)
    }

    /// [`Self::key`] with the clipboard of the window for `clipboard()`.
    pub fn key_with(&mut self, state: &mut EditorState, stroke: &Keystroke, cx: &App) -> KeyUse {
        self.key_app(state, stroke, Some(cx))
    }

    fn key_app(&mut self, state: &mut EditorState, stroke: &Keystroke, cx: Option<&App>) -> KeyUse {
        let plain = !stroke.ctrl && !stroke.alt;
        if let Some(list) = &mut self.list {
            let count = list.items.len().min(SHOWN);
            match stroke.key.as_str() {
                "up" if plain => list.chosen = (list.chosen + count - 1) % count,
                "down" if plain => list.chosen = (list.chosen + 1) % count,
                "enter" | "tab" if plain && !stroke.shift => {
                    let template = list.items[list.chosen];
                    self.list = None;
                    return KeyUse::Used(self.apply_app(state, &template, cx));
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
    pub fn suggestions(&self, state: &EditorState) -> Vec<&Template> {
        self.suggestion_entries(state)
            .into_iter()
            .map(|entry| &entry.template)
            .collect()
    }

    /// The templates in force that fit at the caret, with what the expander reads.
    pub fn suggestion_entries(&self, state: &EditorState) -> Vec<&ResolvedTemplate> {
        let (Some(language), Some(caret)) = (self.language, lone_caret(state)) else {
            return Vec::new();
        };
        let (line, _) = line_before(state, caret);
        let prefix = word_before(&line).map_or("", |word| &line[word]);
        let place = context(language, state, caret - prefix.len());
        let fitting = self.fitting(Kind::Abbreviation, place);
        fitting
            .filter(|entry| entry.template.key.starts_with(prefix))
            .collect()
    }
}

impl EditorView {
    /// Puts the templates of the user in place when `templates.json` changed.
    pub(crate) fn sync_templates(&mut self, cx: &App) {
        if let Some(name) = self.path.file_name().map(|name| name.to_string_lossy()) {
            self.live.set_file_name(&name);
        }
        if let Some(settings) = cx.try_global::<TemplateSettings>() {
            self.live
                .set_overrides(&settings.overrides, settings.generation);
        }
    }

    pub(crate) fn template_action(&mut self, action: &str, cx: &App) -> Option<Option<Change>> {
        self.sync_templates(cx);
        let state = self.state.as_mut()?;
        let change = self.live.action_with(state, action, cx)?;
        Some(change)
    }

    /// `true` when the key went to the list or the fields.
    pub(crate) fn template_key(&mut self, stroke: &Keystroke, cx: &mut Context<Self>) -> bool {
        self.catch_up(cx);
        self.sync_templates(cx);
        let Some(state) = &mut self.state else {
            return false;
        };
        let had_list = self.live.list().is_some();
        match self.live.key_with(state, stroke, cx) {
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

    /// Enter or Space expands the template that chose it, when its abbreviation ends at
    /// the caret and no completion list is open. `false` leaves the key its usual text.
    pub(crate) fn template_expand_key(&mut self, key: ExpandWith, cx: &mut Context<Self>) -> bool {
        self.catch_up(cx);
        if self
            .lsp
            .as_ref()
            .is_some_and(|lsp| lsp.completion.is_some())
        {
            return false;
        }
        self.sync_templates(cx);
        let Some(state) = &mut self.state else {
            return false;
        };
        match self.live.expand_key(state, key, cx) {
            Some(change) => {
                self.changed(change, cx);
                true
            }
            None => false,
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
        for (index, template) in list.items.iter().take(SHOWN).enumerate() {
            let chosen = *template;
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
                            this.live.apply_with(state, &chosen, cx)
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
        self.sync_templates(cx);
        let Some(state) = &mut self.state else {
            return;
        };
        let found = self.live.suggestion_entries(state);
        let template = found
            .into_iter()
            .find(|entry| entry.template.key == key)
            .map(|entry| entry.template);
        if let Some(template) = template {
            let change = self.live.apply_with(state, &template, cx);
            self.changed(change, cx);
        }
    }
}

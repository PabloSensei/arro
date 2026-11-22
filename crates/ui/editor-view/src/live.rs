//! Live templates in the editor: Tab after an abbreviation or a postfix key, Ctrl+J for the
//! list, Ctrl+Alt+T to surround the selected lines, Tab and Shift+Tab through the fields.
//! [`LiveTemplates`] works on [`EditorState`] alone; the view adds keys and the list.

use std::ops::Range;

use keymap::{Keystroke, editor as actions};
use lang_api::{CompletionEntry, CompletionKind, CompletionLabelDetails, TextSpan};
use templates::{
    ExpandWith, Expansion, ExpansionSite, Kind, Receiver, ResolvedTemplate, Session, Step,
    Template, TemplateLanguage, Variables, context_at, declarations, expand_fields, postfix_typed,
    snippet_expansion, snippet_of, surround_fields, word_before,
};
use text_core::{Change, Edit, EditKind, Selection, SelectionSet, TextStore};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::element::TEXT_INSET;
use crate::semantic::{identifier_after, point_at};
use crate::state::EditorState;
use crate::view::{EditorView, TemplateSettings};

const SHOWN: usize = 12;
/// The Surround With list shows every standard template of IDEA and the live templates
/// that follow them.
const SURROUND_SHOWN: usize = 16;
/// The keys the standard Surround With entries answer to, in the order of the list.
const SURROUND_KEYS: &[&str] = &["1", "2", "3", "4", "5", "6", "7", "8", "9", "0", "A"];
/// The note at the caret when no live template fits the place Ctrl+J is pressed at.
const NO_APPLICABLE_TEMPLATES: &str = "No applicable live templates";

/// The list Ctrl+J or Ctrl+Alt+T opened, or the expressions a postfix template can
/// follow when its type is not enough to choose one.
pub struct TemplateList {
    pub kind: Kind,
    pub items: Vec<Template>,
    /// The key left of each item that picks it without Enter.
    pub keys: Vec<String>,
    pub chosen: usize,
    /// The expressions of a postfix key, longest first, when several fit.
    pub expressions: Vec<Range<usize>>,
    /// The postfix template the expressions belong to.
    pub template: Option<Template>,
    /// The items go in at the caret and leave the word around it alone.
    pub at_caret: bool,
    /// The letters typed into the list, which keep the abbreviations and the descriptions
    /// they match. Only an abbreviation list takes them.
    pub filter: String,
}

/// Whether the abbreviation of `template` contains the typed letters.
fn key_matches(template: &Template, typed: &str) -> bool {
    template.key.to_lowercase().contains(typed)
}

/// Whether a word of the description of `template` starts with the typed letters.
fn word_matches(template: &Template, typed: &str) -> bool {
    template
        .description
        .split(|character: char| !character.is_alphanumeric())
        .any(|word| word.to_lowercase().starts_with(typed))
}

impl TemplateList {
    /// The items the list shows, as indexes into `items`, in the order it shows them. An
    /// abbreviation list keeps what the filter matches: first the abbreviations that
    /// contain the typed letters, then the templates with a description word that starts
    /// with them.
    fn rows(&self) -> Vec<usize> {
        let cap = if self.kind == Kind::Surround {
            SURROUND_SHOWN
        } else {
            SHOWN
        };
        let typed = self.filter.to_lowercase();
        let items = self.items.iter().enumerate();
        if self.kind != Kind::Abbreviation || typed.is_empty() {
            return items.map(|(index, _)| index).take(cap).collect();
        }
        let by_key = items
            .clone()
            .filter(|(_, template)| key_matches(template, &typed))
            .map(|(index, _)| index);
        let by_word = items
            .filter(|(_, template)| {
                !key_matches(template, &typed) && word_matches(template, &typed)
            })
            .map(|(index, _)| index);
        by_key.chain(by_word).take(cap).collect()
    }
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
    /// Ctrl+J found no template that fits the place; the view says so at the caret.
    no_applicable: bool,
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

/// A letter or a digit that an abbreviation list takes into its filter. A digit typed with
/// Shift is a symbol and stays out of it.
fn is_filter_key(stroke: &Keystroke) -> bool {
    match stroke.key.as_bytes() {
        [byte] => byte.is_ascii_alphabetic() || (byte.is_ascii_digit() && !stroke.shift),
        _ => false,
    }
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
            no_applicable: false,
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

    /// The kind of the expression in `range`, as the language reads it.
    fn receiver(&self, text: &str, tree: Option<&syntax::Tree>, range: Range<usize>) -> Receiver {
        match self.language.and_then(|language| language.postfix) {
            Some(rules) => (rules.receiver)(text, tree, range),
            None => Receiver::default(),
        }
    }

    /// Whether the language lets `template` follow an expression of this kind.
    fn applies(&self, template: &Template, receiver: Receiver) -> bool {
        self.language
            .and_then(|language| language.postfix)
            .is_none_or(|rules| (rules.applies)(template, receiver))
    }

    /// The expressions before a postfix key that `template` can follow, the longest
    /// first, with the kind of each one.
    fn postfix_expressions(
        &self,
        state: &EditorState,
        line: &str,
        line_start: usize,
        template: &Template,
    ) -> Vec<(Range<usize>, Receiver)> {
        let Some(language) = self.language else {
            return Vec::new();
        };
        let Some((receivers, _, _)) = postfix_typed(line, language.receiver_prefixes) else {
            return Vec::new();
        };
        let text = state.text();
        let tree = state.tree();
        receivers
            .into_iter()
            .map(|range| line_start + range.start..line_start + range.end)
            .map(|range| {
                let kind = self.receiver(&text, tree, range.clone());
                (range, kind)
            })
            .filter(|(_, kind)| self.applies(template, *kind))
            .collect()
    }

    /// The expression to take when the caret stops after a postfix key: the only one, or
    /// the longest when the language did not prove the kinds. `None` when several kinds
    /// fit and the user has to choose.
    fn chosen_expression(expressions: &[(Range<usize>, Receiver)]) -> Option<Range<usize>> {
        if expressions.len() <= 1 {
            return expressions.first().map(|(range, _)| range.clone());
        }
        let proven = expressions.iter().filter(|(_, kind)| kind.known).count();
        (proven <= 1).then(|| expressions[0].0.clone())
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
                let matching: Vec<Template> = self
                    .fitting(Kind::Abbreviation, place)
                    .filter(|entry| entry.template.key.to_lowercase().starts_with(&prefix))
                    .map(|entry| entry.template)
                    .collect();
                // A word that matches no abbreviation is not a prefix: the list then
                // offers every template of the place and inserts one at the caret.
                if matching.is_empty() {
                    let items: Vec<Template> = self
                        .fitting(Kind::Abbreviation, place)
                        .map(|entry| entry.template)
                        .collect();
                    // No template fits the place: no list, and the caret says why.
                    self.no_applicable = items.is_empty();
                    self.open_list(Kind::Abbreviation, items, true);
                } else {
                    self.open(Kind::Abbreviation, matching);
                }
                Some(None)
            }
            actions::SURROUND_WITH => {
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
            && let Some((receivers, postfix_key, _)) =
                postfix_typed(&line, language.receiver_prefixes)
            && !postfix_key.is_empty()
        {
            let place = context(language, state, start + receivers[0].start);
            if let Some(entry) = self.find(Kind::Postfix, postfix_key, place).cloned() {
                let expressions = self.postfix_expressions(state, &line, start, &entry.template);
                if expressions.len() > 1 && Self::chosen_expression(&expressions).is_none() {
                    self.open_expressions(
                        entry.template,
                        expressions.into_iter().map(|(range, _)| range).collect(),
                    );
                    return Some(None);
                }
                if let Some(receiver) = Self::chosen_expression(&expressions) {
                    return Some(self.expand_postfix(state, &entry, receiver, cx));
                }
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

    /// Replaces the receiver of `receiver` and the postfix key with the expansion of
    /// `entry`, as a snippet would.
    fn expand_postfix(
        &mut self,
        state: &mut EditorState,
        entry: &ResolvedTemplate,
        receiver: Range<usize>,
        cx: Option<&App>,
    ) -> Option<Change> {
        let caret = lone_caret(state)?;
        let (line, _) = line_before(state, caret);
        let indent = indent_of(&line).to_string();
        let unit = unit_for(&indent, state);
        let expression = state.store().slice(receiver.clone());
        let site = ExpansionSite {
            indent: &indent,
            unit: &unit,
            selection: None,
            expression: Some(&expression),
            variables: self.variables(state, caret, cx),
        };
        let expansion = expand_fields(&entry.template, &entry.fields, &site);
        self.insert(state, receiver.start..caret, &expansion)
    }

    /// Expands the postfix template of an expression picked from the list.
    fn apply_expression(
        &mut self,
        state: &mut EditorState,
        template: &Template,
        receiver: Range<usize>,
        cx: Option<&App>,
    ) -> Option<Change> {
        let entry = self.entry(template)?.clone();
        self.expand_postfix(state, &entry, receiver, cx)
    }

    fn open(&mut self, kind: Kind, items: Vec<Template>) {
        self.open_list(kind, items, false);
    }

    fn open_list(&mut self, kind: Kind, items: Vec<Template>, at_caret: bool) {
        let keys = if kind == Kind::Surround {
            items
                .iter()
                .enumerate()
                .map(|(index, template)| match template.kind {
                    Kind::SurroundLive => template.key.to_owned(),
                    _ => SURROUND_KEYS
                        .get(index)
                        .map_or_else(String::new, |key| (*key).to_owned()),
                })
                .collect()
        } else {
            Vec::new()
        };
        self.list = (!items.is_empty()).then_some(TemplateList {
            kind,
            items,
            keys,
            chosen: 0,
            expressions: Vec::new(),
            template: None,
            at_caret,
            filter: String::new(),
        });
    }

    fn open_expressions(&mut self, template: Template, expressions: Vec<Range<usize>>) {
        self.list = Some(TemplateList {
            kind: Kind::Postfix,
            items: Vec::new(),
            keys: Vec::new(),
            chosen: 0,
            expressions,
            template: Some(template),
            at_caret: false,
            filter: String::new(),
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
            Kind::Postfix => {
                let caret = lone_caret(state)?;
                let (line, start) = line_before(state, caret);
                let expressions = self.postfix_expressions(state, &line, start, &entry.template);
                let receiver = Self::chosen_expression(&expressions)?;
                self.expand_postfix(state, &entry, receiver, cx)
            }
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
                self.expand_abbreviation(state, &entry, start + word.start..caret, caret, cx)
            }
        }
    }

    /// Inserts an abbreviation at the caret, leaving the word around it as it is.
    fn apply_at_caret(
        &mut self,
        state: &mut EditorState,
        template: &Template,
        cx: Option<&App>,
    ) -> Option<Change> {
        self.language?;
        let entry = self.entry(template)?.clone();
        let caret = lone_caret(state)?;
        self.expand_abbreviation(state, &entry, caret..caret, caret, cx)
    }

    fn expand_abbreviation(
        &mut self,
        state: &mut EditorState,
        entry: &ResolvedTemplate,
        range: Range<usize>,
        at: usize,
        cx: Option<&App>,
    ) -> Option<Change> {
        let (line, _) = line_before(state, range.start);
        let indent = indent_of(&line);
        let unit = unit_for(indent, state);
        let site = ExpansionSite {
            indent,
            unit: &unit,
            selection: None,
            expression: None,
            variables: self.variables(state, at, cx),
        };
        let expansion = expand_fields(&entry.template, &entry.fields, &site);
        self.insert(state, range, &expansion)
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
            if !list.expressions.is_empty() {
                let count = list.expressions.len().min(SHOWN);
                match stroke.key.as_str() {
                    "up" if plain => list.chosen = (list.chosen + count - 1) % count,
                    "down" if plain => list.chosen = (list.chosen + 1) % count,
                    "enter" | "tab" if plain && !stroke.shift => {
                        let receiver = list.expressions[list.chosen].clone();
                        let Some(template) = list.template else {
                            self.list = None;
                            return KeyUse::Unused;
                        };
                        self.list = None;
                        return KeyUse::Used(self.apply_expression(state, &template, receiver, cx));
                    }
                    "escape" => self.list = None,
                    _ => {
                        self.list = None;
                        return KeyUse::Unused;
                    }
                }
                return KeyUse::Used(None);
            }
            let rows = list.rows();
            let count = rows.len();
            match stroke.key.as_str() {
                "up" if plain && count > 0 => list.chosen = (list.chosen + count - 1) % count,
                "down" if plain && count > 0 => list.chosen = (list.chosen + 1) % count,
                // A filter that matches no row leaves nothing for Enter to take.
                "enter" | "tab" if plain && !stroke.shift => {
                    let Some(&item) = rows.get(list.chosen) else {
                        return KeyUse::Used(None);
                    };
                    let template = list.items[item];
                    let at_caret = list.at_caret;
                    self.list = None;
                    let change = if at_caret {
                        self.apply_at_caret(state, &template, cx)
                    } else {
                        self.apply_app(state, &template, cx)
                    };
                    return KeyUse::Used(change);
                }
                "escape" => self.list = None,
                // A keystroke without a key name, or a modifier key on its own, leaves the
                // list and its filter as they are. It must not reach the Surround arm below,
                // where an empty name would match a multi-letter key.
                "" | "shift" | "control" | "alt" | "platform" | "function" => {
                    return KeyUse::Unused;
                }
                "backspace" if plain && !list.filter.is_empty() => {
                    list.filter.pop();
                    list.chosen = 0;
                }
                _ if plain && list.kind == Kind::Surround => {
                    let typed = stroke.key.to_lowercase();
                    let chosen = list.keys.iter().position(|key| {
                        let key = key.to_lowercase();
                        key == typed || (key.len() > 1 && key.starts_with(&typed))
                    });
                    let Some(chosen) = chosen else {
                        self.list = None;
                        return KeyUse::Unused;
                    };
                    let template = list.items[chosen];
                    self.list = None;
                    return KeyUse::Used(self.apply_app(state, &template, cx));
                }
                _ if plain && list.kind == Kind::Abbreviation && is_filter_key(stroke) => {
                    list.filter.push_str(&stroke.key);
                    list.chosen = 0;
                }
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

    /// The postfix templates that fit after the expression before the caret, as entries
    /// of the completion list. Each one replaces the expression and the typed key with
    /// its expansion and carries the description and a preview of that expansion. Nothing
    /// when a name starts right after the caret: the list then shows the members only.
    pub fn postfix_completions(&self, state: &EditorState) -> Vec<CompletionEntry> {
        let Some(language) = self.language else {
            return Vec::new();
        };
        let Some(caret) = lone_caret(state) else {
            return Vec::new();
        };
        if identifier_after(&state.store(), caret) {
            return Vec::new();
        }
        let (line, start) = line_before(state, caret);
        let Some((receivers, typed, _)) = postfix_typed(&line, language.receiver_prefixes) else {
            return Vec::new();
        };
        let place = context(language, state, start + receivers[0].start);
        let mut entries = Vec::new();
        for entry in self
            .fitting(Kind::Postfix, place)
            .filter(|entry| entry.template.key.starts_with(typed))
        {
            let expressions = self.postfix_expressions(state, &line, start, &entry.template);
            let Some((receiver, _)) = expressions.first() else {
                continue;
            };
            let expression = state.store().slice(receiver.clone());
            let snippet = snippet_of(&entry.template, &expression);
            let preview = snippet_expansion(&snippet, "", language.indent_unit).text;
            entries.push(CompletionEntry {
                label: entry.template.key.to_owned(),
                label_details: Some(CompletionLabelDetails {
                    detail: None,
                    description: Some(entry.template.description.to_owned()),
                }),
                detail: None,
                filter_text: Some(format!("{expression}.{}", entry.template.key)),
                sort_text: None,
                insert_text: Some(snippet),
                span: Some(TextSpan {
                    start: point_at(&state.store(), receiver.start),
                    end: point_at(&state.store(), caret),
                }),
                snippet: true,
                additional_edits: Vec::new(),
                command: None,
                resolve: None,
                kind: CompletionKind::Snippet,
                deprecated: false,
                documentation: Some(format!("```\n{preview}\n```")),
            });
        }
        entries
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

    /// The note at the caret when Ctrl+J found no template that fits the place.
    pub(crate) fn template_note(&mut self, cx: &mut Context<Self>) {
        if !std::mem::take(&mut self.live.no_applicable) {
            return;
        }
        let Some(at) = self
            .state
            .as_ref()
            .map(|state| state.selections().primary().head)
        else {
            return;
        };
        self.show_transient_note(at, NO_APPLICABLE_TEMPLATES, cx);
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
            // The description of an abbreviation is cut at the end of the list, so the list
            // has a width of its own.
            .when(list.kind == Kind::Abbreviation, |popup| {
                popup.max_w(px(420.))
            })
            .py(px(4.));
        if list.expressions.is_empty() {
            for (row_index, item) in list.rows().into_iter().enumerate() {
                let template = list.items[item];
                let chosen = template;
                let at_caret = list.at_caret;
                let key = list.keys.get(item).cloned().unwrap_or_default();
                let name = template.key;
                let mut row = div()
                    .id(("template", row_index))
                    .debug_selector(move || format!("template-item-{name}"))
                    .flex()
                    .items_center()
                    .gap(px(12.))
                    .px(px(10.))
                    .py(px(3.))
                    .when(row_index == list.chosen, |row| row.bg(theme.selection))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            let change = this.state.as_mut().and_then(|state| {
                                this.live.list = None;
                                if at_caret {
                                    this.live.apply_at_caret(state, &chosen, Some(cx))
                                } else {
                                    this.live.apply_with(state, &chosen, cx)
                                }
                            });
                            this.changed(change, cx);
                        }),
                    );
                if list.kind == Kind::Abbreviation {
                    // The abbreviation comes first, in the text colour; the description
                    // follows in gray and is cut with an ellipsis, as the right part of a
                    // completion row is.
                    row = row
                        .child(
                            div()
                                .flex_none()
                                .text_color(theme.text)
                                .debug_selector(move || format!("template-key-{name}"))
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme.text_muted)
                                .debug_selector(move || format!("template-description-{name}"))
                                .child(template.description),
                        );
                } else {
                    if !key.is_empty() {
                        row = row.child(div().text_color(theme.text_secondary).child(key));
                    }
                    row = row.child(
                        div()
                            .flex_1()
                            .text_color(theme.text)
                            .child(template.description),
                    );
                }
                shown = shown.child(row);
            }
        } else {
            for (index, receiver) in list.expressions.iter().take(SHOWN).enumerate() {
                let receiver = receiver.clone();
                let chosen = receiver.clone();
                let template = list.template;
                let expression = self
                    .state
                    .as_ref()
                    .map(|state| state.store().slice(receiver))
                    .unwrap_or_default();
                let row = div()
                    .id(("template-expression", index))
                    .debug_selector(move || format!("template-expression-{index}"))
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
                                let template = template?;
                                this.live.apply_expression(
                                    state,
                                    &template,
                                    chosen.clone(),
                                    Some(cx),
                                )
                            });
                            this.changed(change, cx);
                        }),
                    )
                    .child(div().text_color(theme.text).child(expression));
                shown = shown.child(row);
            }
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
            + px(self.gutter_room() + TEXT_INSET + self.center_inset)
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

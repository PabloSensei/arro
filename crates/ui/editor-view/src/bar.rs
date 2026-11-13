//! The bar over the text for Find (Ctrl+F), Replace (Ctrl+R) and Go to Line (Ctrl+G), and
//! what marks the matches in the text.

use std::ops::Range;

use text_core::TextStore;
use text_core::find::{FindOptions, find_all, replacements};
use ui_kit::metrics::CONTROL_RADIUS;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::state::EditorState;
use crate::view::EditorView;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BarKind {
    Find,
    Replace,
    Goto,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Query,
    Replacement,
}

pub(crate) struct Bar {
    pub kind: BarKind,
    field: Field,
    pub query: TextField,
    pub replacement: TextField,
    pub options: FindOptions,
    pub matches: Vec<Range<usize>>,
    pub current: Option<usize>,
    error: Option<String>,
    /// The caret before the search began; the first match is the one at or after it.
    origin: usize,
    /// The keys go to the bar, not to the text.
    pub active: bool,
}

/// The last search, for F3 and for the next time the bar opens.
#[derive(Clone, Default)]
pub(crate) struct LastFind {
    query: String,
    options: FindOptions,
}

impl EditorView {
    pub(crate) fn bar_active(&self) -> bool {
        self.bar.as_ref().is_some_and(|bar| bar.active)
    }

    /// Ctrl+F and Ctrl+R. A selection on one line becomes the query.
    pub(crate) fn open_find(&mut self, replace: bool, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let selection = state.selections().primary();
        let selected = (!selection.is_empty())
            .then(|| state.store().slice(selection.range()))
            .filter(|text| !text.contains('\n'));
        let remembered = self.last_find.clone();
        let kind = if replace {
            BarKind::Replace
        } else {
            BarKind::Find
        };
        let mut bar = match self.bar.take() {
            Some(bar) if bar.kind != BarKind::Goto => bar,
            _ => Bar {
                kind,
                field: Field::Query,
                query: TextField::from_text(remembered.query),
                replacement: TextField::default(),
                options: remembered.options,
                matches: Vec::new(),
                current: None,
                error: None,
                origin: selection.start(),
                active: true,
            },
        };
        bar.kind = kind;
        bar.active = true;
        bar.field = Field::Query;
        bar.origin = selection.start();
        if let Some(text) = selected {
            bar.query = TextField::from_text(text);
        }
        self.bar = Some(bar);
        self.refresh_matches(true, cx);
    }

    /// Ctrl+G.
    pub(crate) fn open_goto(&mut self, cx: &mut Context<Self>) {
        if self.state.is_none() {
            return;
        }
        self.bar = Some(Bar {
            kind: BarKind::Goto,
            field: Field::Query,
            query: TextField::default(),
            replacement: TextField::default(),
            options: FindOptions::default(),
            matches: Vec::new(),
            current: None,
            error: None,
            origin: 0,
            active: true,
        });
        cx.notify();
    }

    pub(crate) fn close_bar(&mut self, cx: &mut Context<Self>) {
        if let Some(bar) = self.bar.take()
            && bar.kind != BarKind::Goto
        {
            self.last_find = LastFind {
                query: bar.query.text,
                options: bar.options,
            };
        }
        cx.notify();
    }

    /// Looks for the query again. `reselect` also moves the caret to the match at or after
    /// the place the search began.
    pub(crate) fn refresh_matches(&mut self, reselect: bool, cx: &mut Context<Self>) {
        let (Some(bar), Some(state)) = (&mut self.bar, &self.state) else {
            return;
        };
        if bar.kind == BarKind::Goto {
            return;
        }
        let text = state.text();
        match find_all(&text, &bar.query.text, bar.options) {
            Ok(found) => {
                bar.matches = found;
                bar.error = None;
            }
            Err(message) => {
                bar.matches.clear();
                bar.error = Some(message);
            }
        }
        let from = if reselect {
            bar.origin
        } else {
            state.selections().primary().start()
        };
        let at = bar.matches.iter().position(|found| found.start >= from);
        bar.current = at.or((!bar.matches.is_empty()).then_some(0));
        if reselect {
            self.show_current(cx);
        }
        cx.notify();
    }

    fn show_current(&mut self, cx: &mut Context<Self>) {
        let range = self
            .bar
            .as_ref()
            .and_then(|bar| bar.current.and_then(|at| bar.matches.get(at)))
            .cloned();
        if let Some(range) = &range {
            self.reveal_rendered_doc(range.clone(), cx);
        }
        if let (Some(range), Some(state)) = (range, &mut self.state) {
            state.select_range(range);
            self.autoscroll = true;
            self.caret_on = true;
        }
        cx.notify();
    }

    /// Ctrl+F3 and Ctrl+Shift+F3: the word under the caret becomes the search, then the
    /// caret goes to its next or previous occurrence.
    pub(crate) fn find_word_at_caret(&mut self, forward: bool, cx: &mut Context<Self>) {
        let Some(state) = &self.state else {
            return;
        };
        let selection = state.selections().primary();
        let word = state.caret_word().or_else(|| {
            (!selection.is_empty())
                .then(|| text_core::commands::word_at(&state.store(), selection.start()))
                .filter(|word| !word.is_empty())
        });
        let Some(word) = word else {
            return;
        };
        let query = state.store().slice(word.clone());
        match find_all(&state.text(), &query, FindOptions::default()) {
            Ok(found) => {
                self.last_find = LastFind {
                    query,
                    options: FindOptions::default(),
                };
                self.bar = Some(Bar {
                    kind: BarKind::Find,
                    field: Field::Query,
                    query: TextField::from_text(self.last_find.query.clone()),
                    replacement: TextField::default(),
                    options: FindOptions::default(),
                    matches: found,
                    current: None,
                    error: None,
                    origin: word.start,
                    active: false,
                });
                if let Some(bar) = &mut self.bar {
                    bar.current = bar.matches.iter().position(|found| found.start >= word.end);
                }
                self.step_match(forward, (word.start, word.end), cx);
            }
            Err(message) => {
                if let Some(bar) = &mut self.bar {
                    bar.error = Some(message);
                }
            }
        }
        cx.notify();
    }

    /// Ctrl+Alt+Down and Ctrl+Alt+Up: the next or previous occurrence of the find that is
    /// open, or of the usages under the caret. False when there is no list to step in.
    pub(crate) fn occurrence_step(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
        if self.usages.is_some() {
            self.step_usage(forward, cx);
            return true;
        }
        let Some(bar) = &self.bar else {
            return false;
        };
        if bar.matches.is_empty() {
            return false;
        }
        let selection = self
            .state
            .as_ref()
            .map(EditorState::selections)
            .map(|selections| selections.primary())
            .map_or((0, 0), |selection| (selection.start(), selection.end()));
        self.step_match(forward, selection, cx);
        true
    }

    /// F3 and Shift+F3, with the bar closed too.
    pub(crate) fn find_step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let selection = self
            .state
            .as_ref()
            .map(|state| state.selections().primary());
        let (start, end) = selection.map_or((0, 0), |s| (s.start(), s.end()));
        if self
            .bar
            .as_ref()
            .is_none_or(|bar| bar.kind == BarKind::Goto)
        {
            let remembered = self.last_find.clone();
            if remembered.query.is_empty() {
                self.open_find(false, cx);
                if let Some(bar) = &mut self.bar {
                    bar.active = true;
                }
                return;
            }
            // The query stays the one of the last search, not the selected text, and the
            // step goes from the caret that was here before the bar opened.
            self.bar = Some(Bar {
                kind: BarKind::Find,
                field: Field::Query,
                query: TextField::from_text(remembered.query),
                replacement: TextField::default(),
                options: remembered.options,
                matches: Vec::new(),
                current: None,
                error: None,
                origin: start,
                active: false,
            });
            self.refresh_matches(false, cx);
        }
        self.step_match(forward, (start, end), cx);
    }

    /// Moves the current match one step from a selection, wrapping at the ends.
    fn step_match(&mut self, forward: bool, (start, end): (usize, usize), cx: &mut Context<Self>) {
        let Some(bar) = &mut self.bar else {
            return;
        };
        let Some(count) = bar.matches.len().checked_sub(1) else {
            return;
        };
        let next = if forward {
            bar.matches.iter().position(|found| found.start >= end)
        } else {
            bar.matches.iter().rposition(|found| found.end <= start)
        };
        bar.current = Some(next.unwrap_or(if forward { 0 } else { count }));
        self.show_current(cx);
    }

    fn replace_current(&mut self, cx: &mut Context<Self>) {
        self.catch_up(cx);
        let Some((edits, at)) = self.replace_edits(true) else {
            return;
        };
        if let Some(state) = &mut self.state {
            let change = state.apply_edits(edits);
            self.changed(change, cx);
        }
        if let Some(bar) = &mut self.bar {
            bar.origin = at;
        }
        self.refresh_matches(true, cx);
    }

    fn replace_all(&mut self, cx: &mut Context<Self>) {
        self.catch_up(cx);
        let Some((edits, _)) = self.replace_edits(false) else {
            return;
        };
        if edits.is_empty() {
            return;
        }
        if let Some(state) = &mut self.state {
            let change = state.apply_edits(edits);
            self.changed(change, cx);
        }
        self.refresh_matches(false, cx);
    }

    /// The edits of Replace or Replace All and the offset after the first of them.
    fn replace_edits(&self, only_current: bool) -> Option<(Vec<text_core::Edit>, usize)> {
        let (bar, state) = (self.bar.as_ref()?, self.state.as_ref()?);
        let range = bar.current.and_then(|at| bar.matches.get(at)).cloned();
        let only = if only_current {
            Some(range.as_ref()?)
        } else {
            None
        };
        let text = state.text();
        let edits = replacements(
            &text,
            &bar.query.text,
            bar.options,
            &bar.replacement.text,
            only,
        )
        .ok()?;
        let after = edits
            .first()
            .map_or(0, |edit| edit.range.start + edit.text.len());
        Some((edits, after))
    }

    fn submit_goto(&mut self, cx: &mut Context<Self>) {
        let Some(bar) = &self.bar else {
            return;
        };
        let mut parts = bar.query.text.trim().split([':', ' ', ',']);
        let line = parts.next().and_then(|part| part.parse::<usize>().ok());
        let column = parts.next().and_then(|part| part.parse::<usize>().ok());
        let Some(line) = line.filter(|line| *line > 0) else {
            if let Some(bar) = &mut self.bar {
                bar.error = Some("Type a line number, or line:column".into());
            }
            cx.notify();
            return;
        };
        self.bar = None;
        self.go_to(
            crate::view::Spot::Line {
                line: line - 1,
                column: column.map_or(0, |column| column.saturating_sub(1)),
            },
            cx,
        );
    }

    /// Keys while the bar is active. `false` leaves the key to the keymap.
    pub(crate) fn bar_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let Some(bar) = &mut self.bar else {
            return false;
        };
        let modifiers = event.keystroke.modifiers;
        let (control, alt, shift) = (modifiers.control, modifiers.alt, modifiers.shift);
        let kind = bar.kind;
        match event.keystroke.key.as_str() {
            "escape" => {
                self.close_bar(cx);
                return true;
            }
            "enter" if control && kind == BarKind::Replace => self.replace_all(cx),
            "enter" => match kind {
                BarKind::Goto => self.submit_goto(cx),
                BarKind::Replace if bar.field == Field::Replacement => self.replace_current(cx),
                _ => self.find_step(!shift, cx),
            },
            "tab" if kind == BarKind::Replace => {
                bar.field = match bar.field {
                    Field::Query => Field::Replacement,
                    Field::Replacement => Field::Query,
                };
                cx.notify();
            }
            "c" | "w" | "x" if alt && kind != BarKind::Goto => {
                match event.keystroke.key.as_str() {
                    "c" => bar.options.case_sensitive = !bar.options.case_sensitive,
                    "w" => bar.options.whole_word = !bar.options.whole_word,
                    _ => bar.options.regex = !bar.options.regex,
                }
                self.refresh_matches(true, cx);
            }
            _ => {
                let reselect = bar.field == Field::Query;
                if !bar.field_mut().key_with_clipboard(event, false, cx) {
                    return false;
                }
                bar.error = None;
                self.refresh_matches(reselect, cx);
            }
        }
        true
    }

    /// The marks over the text in `visible`: matches of the search, the other occurrences
    /// of the word at the caret and the brackets around the caret.
    pub(crate) fn highlights(
        &self,
        state: &EditorState,
        visible: Range<usize>,
    ) -> Vec<(Range<usize>, Hsla)> {
        let style = &self.style;
        let mut marks = Vec::new();
        if let Some(bar) = &self.bar {
            let first = bar
                .matches
                .partition_point(|found| found.end <= visible.start);
            for (at, found) in bar.matches.iter().enumerate().skip(first) {
                if found.start >= visible.end {
                    break;
                }
                let color = if bar.current == Some(at) {
                    style.find_current
                } else {
                    style.find_match
                };
                marks.push((found.clone(), color));
            }
        }
        // The occurrences of the server know the symbol; the text ones show until it answers.
        let semantic = self.usages.is_some()
            || self
                .lsp
                .as_ref()
                .is_some_and(|lsp| !lsp.highlights.is_empty());
        if let Some(usages) = &self.usages {
            for range in usages {
                if range.end <= visible.start || range.start >= visible.end {
                    continue;
                }
                marks.push((range.clone(), style.occurrence));
            }
        } else if !semantic && let Some(word) = state.caret_word() {
            let store = state.store();
            let needle = store.slice(word);
            let text = store.slice(visible.clone());
            let is_word = |c: char| c.is_alphanumeric() || c == '_';
            for (at, _) in text.match_indices(&needle) {
                let end = at + needle.len();
                let clean = !text[..at].chars().next_back().is_some_and(is_word)
                    && !text[end..].chars().next().is_some_and(is_word);
                if clean {
                    marks.push((visible.start + at..visible.start + end, style.occurrence));
                }
            }
        }
        if let Some((open, close)) = state.bracket_pair() {
            for at in [open, close] {
                if visible.contains(&at) {
                    marks.push((at..at + 1, style.bracket));
                }
            }
        }
        marks
    }

    /// The bar as an element, `None` while it is closed.
    pub(crate) fn render_bar(&self, cx: &mut Context<Self>) -> Option<Div> {
        let bar = self.bar.as_ref()?;
        let style = self.style.clone();
        let theme = *cx.global::<Theme>();
        let field =
            |id: &'static str, value: &TextField, on: bool, which: Field, hint: &'static str| {
                let placeholder = value.text.is_empty() && !on;
                let color = if placeholder {
                    style.line_number
                } else {
                    style.code
                };
                div()
                    .id(id)
                    .flex_1()
                    .min_w(px(120.))
                    .h(px(24.))
                    .px(px(8.))
                    .flex()
                    .items_center()
                    .rounded(px(CONTROL_RADIUS))
                    .bg(style.background)
                    .border_1()
                    .border_color(if on { style.accent } else { style.indent_guide })
                    .text_color(color)
                    .overflow_hidden()
                    .whitespace_nowrap()
                    .when(placeholder, |this| this.child(hint))
                    .when(!placeholder, |this| {
                        this.children(text_field_children(
                            value,
                            style.code,
                            style.code,
                            style.selection,
                            style.caret,
                            16.,
                            on,
                        ))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(bar) = &mut this.bar {
                            bar.field = which;
                            bar.active = true;
                            cx.notify();
                        }
                    }))
            };
        let chip = |id: &'static str, label: &'static str, on: bool| {
            div()
                .id(id)
                .h(px(22.))
                .min_w(px(24.))
                .px(px(4.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(CONTROL_RADIUS))
                .cursor_pointer()
                .text_color(if on { style.code } else { style.line_number })
                .when(on, |this| this.bg(style.selection))
                .child(label)
        };
        let active = bar.active;
        let mut row = div()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(32.))
            .px(px(10.));
        if bar.kind == BarKind::Goto {
            let hint = bar.error.clone().unwrap_or_else(|| "Line:Column".into());
            let invalid = bar.error.is_some();
            row = row
                .child(div().text_color(style.line_number).child("Go to"))
                .child(field(
                    "bar-goto",
                    &bar.query,
                    active,
                    Field::Query,
                    "Line:Column",
                ))
                .child(
                    div()
                        .text_color(if invalid {
                            style.error
                        } else {
                            style.line_number
                        })
                        .child(SharedString::from(hint)),
                );
        } else {
            let query_on = active && bar.field == Field::Query;
            let toggle = |id: &'static str,
                          name: &'static str,
                          label: &'static str,
                          key: &'static str,
                          on: bool| {
                icon_button(id, name, label, Some(key.into()), &theme)
                    .when(on, |this| this.bg(style.selection))
            };
            let count: SharedString =
                match (&bar.error, bar.query.text.is_empty(), bar.matches.len()) {
                    (Some(error), _, _) => error.clone().into(),
                    (None, true, _) => "".into(),
                    (None, false, 0) => "No matches".into(),
                    (None, false, total) => {
                        format!("{} of {total}", bar.current.map_or(0, |at| at + 1)).into()
                    }
                };
            let bad = bar.error.is_some() || (!bar.query.text.is_empty() && bar.matches.is_empty());
            row = row
                .child(field(
                    "bar-query",
                    &bar.query,
                    query_on,
                    Field::Query,
                    "Find",
                ))
                .child(
                    toggle(
                        "bar-case",
                        "general/caseSensitive",
                        "Match Case",
                        "Alt+C",
                        bar.options.case_sensitive,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_option(0, cx))),
                )
                .child(
                    toggle(
                        "bar-word",
                        "general/wholeWord",
                        "Whole Words",
                        "Alt+W",
                        bar.options.whole_word,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_option(1, cx))),
                )
                .child(
                    toggle(
                        "bar-regex",
                        "general/regex",
                        "Regex",
                        "Alt+X",
                        bar.options.regex,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_option(2, cx))),
                )
                .child(
                    div()
                        .min_w(px(70.))
                        .text_color(if bad { style.error } else { style.line_number })
                        .child(count),
                )
                .child(
                    icon_button(
                        "bar-prev",
                        "general/arrowUp",
                        "Previous Match",
                        Some("Shift+F3".into()),
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.find_step(false, cx))),
                )
                .child(
                    icon_button(
                        "bar-next",
                        "general/arrowDown",
                        "Next Match",
                        Some("F3".into()),
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.find_step(true, cx))),
                );
        }
        row = row.child(
            icon_button(
                "bar-close",
                "general/close",
                "Close",
                Some("Esc".into()),
                &theme,
            )
            .on_click(cx.listener(|this, _, _, cx| this.close_bar(cx))),
        );
        let mut bar_view = div()
            .flex()
            .flex_col()
            .flex_none()
            .w_full()
            .bg(style.panel)
            .border_b_1()
            .border_color(style.panel_border)
            .text_size(px(13.))
            .child(row);
        if bar.kind == BarKind::Replace {
            let on = active && bar.field == Field::Replacement;
            let line = div()
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(32.))
                .px(px(10.))
                .child(field(
                    "bar-replacement",
                    &bar.replacement,
                    on,
                    Field::Replacement,
                    "Replace",
                ))
                .child(
                    chip("bar-replace", "Replace", false)
                        .px(px(8.))
                        .on_click(cx.listener(|this, _, _, cx| this.replace_current(cx))),
                )
                .child(
                    chip("bar-replace-all", "Replace All", false)
                        .px(px(8.))
                        .on_click(cx.listener(|this, _, _, cx| this.replace_all(cx))),
                );
            bar_view = bar_view.child(line);
        }
        Some(bar_view)
    }

    fn toggle_option(&mut self, which: usize, cx: &mut Context<Self>) {
        if let Some(bar) = &mut self.bar {
            match which {
                0 => bar.options.case_sensitive = !bar.options.case_sensitive,
                1 => bar.options.whole_word = !bar.options.whole_word,
                _ => bar.options.regex = !bar.options.regex,
            }
            bar.active = true;
        }
        self.refresh_matches(true, cx);
    }
}

impl Bar {
    fn field_mut(&mut self) -> &mut TextField {
        match self.field {
            Field::Query => &mut self.query,
            Field::Replacement => &mut self.replacement,
        }
    }
}

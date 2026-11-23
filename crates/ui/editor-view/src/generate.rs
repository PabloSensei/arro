//! Alt+Insert: the Source Generate list and the fields or methods it offers.

use std::path::Path;

use lang_api::{
    Confidence, DocumentSpot, GenerateApplyRequest, GenerateChoice, GenerateKind, GenerateQuery,
    Generation, Symbol, SymbolKind, SymbolProvider, ask,
};
use ui_kit::*;

use crate::actions::Pick;
use crate::semantic::{NO_ANSWER, first};
use crate::view::{EditorEvent, EditorView};

/// The status bar line of a row that asks the language server while the server is not ready.
const SERVER_NOT_READY: &str = "The language server is not ready yet";

/// The popup with a checkbox for each field or method a generation may include.
pub(crate) struct GeneratePopup {
    pub at: usize,
    spot: DocumentSpot,
    pub kind: GenerateKind,
    pub choices: Vec<GenerateChoice>,
    /// The choices that pass the filter, as indices into `choices`.
    pub shown: Vec<usize>,
    pub selected: usize,
    pub checked: Vec<bool>,
    pub text: String,
}

impl GeneratePopup {
    fn new(
        at: usize,
        spot: DocumentSpot,
        kind: GenerateKind,
        choices: Vec<GenerateChoice>,
    ) -> Self {
        let checked = choices.iter().map(|choice| choice.checked).collect();
        let mut popup = Self {
            at,
            spot,
            kind,
            choices,
            shown: Vec::new(),
            selected: 0,
            checked,
            text: String::new(),
        };
        popup.filter();
        popup
    }

    fn filter(&mut self) {
        let query = self.text.to_lowercase();
        self.shown = self
            .choices
            .iter()
            .enumerate()
            .filter(|(_, choice)| {
                query.is_empty()
                    || choice.label.to_lowercase().contains(&query)
                    || choice
                        .detail
                        .as_deref()
                        .is_some_and(|detail| detail.to_lowercase().contains(&query))
            })
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
    }

    fn toggle_selected(&mut self) {
        if let Some(index) = self.shown.get(self.selected).copied() {
            self.checked[index] = !self.checked[index];
            self.selected = (self.selected + 1).min(self.shown.len().saturating_sub(1));
        }
    }

    fn check_all(&mut self) {
        for index in &self.shown {
            self.checked[*index] = true;
        }
    }

    /// The marked choices, or the one under the cursor when none is marked.
    fn chosen(&self) -> Vec<String> {
        let marked: Vec<String> = self
            .checked
            .iter()
            .enumerate()
            .filter(|(_, checked)| **checked)
            .map(|(index, _)| self.choices[index].handle.clone())
            .collect();
        if !marked.is_empty() {
            return marked;
        }
        self.shown
            .get(self.selected)
            .map(|index| self.choices[*index].handle.clone())
            .into_iter()
            .collect()
    }
}

fn is_typed_key(key: &str) -> bool {
    key.chars().count() == 1
        || matches!(
            key,
            "space"
                | "comma"
                | "period"
                | "minus"
                | "equal"
                | "slash"
                | "backslash"
                | "semicolon"
                | "quote"
                | "backquote"
                | "bracketleft"
                | "bracketright"
        )
}

impl EditorView {
    /// The pick list of the Generate menu, under the heading "Generate". An empty list shows
    /// the note of `empty` and leaves the open list alone.
    fn show_generate_picks(
        &mut self,
        at: usize,
        rows: Vec<Pick>,
        empty: &str,
        cx: &mut Context<Self>,
    ) {
        let listed = !rows.is_empty();
        self.show_picks(at, rows, empty, cx);
        if listed && let Some(menu) = self.lsp.as_mut().and_then(|lsp| lsp.actions.as_mut()) {
            menu.title = Some("Generate");
        }
    }

    /// Alt+Insert: the list of what applies here. The server says which parts exist; where
    /// it does not know, the symbols of the class say. A row is left out only on positive
    /// evidence, so a language that answers nothing keeps the full list. The source actions
    /// of the server that the rows do not give follow the rows.
    pub(crate) fn generate_menu(&mut self, cx: &mut Context<Self>) {
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(support) = self.lsp.as_ref().map(|lsp| lsp.support.clone()) else {
            return;
        };
        if support.generate().is_none() {
            self.generate_source_actions(cx);
            return;
        }
        let Some(state) = &self.state else {
            return;
        };
        let text = state.text();
        let path = self.path().to_path_buf();
        // A server that imports or has failed answers late or not at all, so its questions are
        // not asked then: the rows come from the symbols of the class, as when it does not know.
        let ready = support.server_ready();
        let version = spot.version;
        // The source actions of the server join the fixed rows, asked for the same place.
        let source = self.selection_span().map(|span| lang_api::ActionQuery {
            uri: spot.uri.clone(),
            version,
            span,
            only: Some(lang_api::ActionKind::Source),
        });
        cx.spawn(async move |this, cx| {
            // The symbols block, so they are read off the UI thread.
            let place = {
                let (language, path, text) = (support.clone(), path.clone(), text.clone());
                cx.background_executor()
                    .spawn(async move { Place::of(language.symbols(), &path, &text, caret) })
                    .await
            };
            let kinds: &[GenerateKind] = if place.in_class {
                &CLASS_PROBES
            } else {
                &OUTSIDE_PROBES
            };
            let Some(provider) = support.generate() else {
                return;
            };
            // Every question starts before the first answer is awaited, so they run together.
            let asked: Vec<_> = if ready {
                kinds
                    .iter()
                    .map(|kind| {
                        let query = GenerateQuery {
                            spot: spot.clone(),
                            kind: *kind,
                        };
                        (*kind, ask(provider, query))
                    })
                    .collect()
            } else {
                Vec::new()
            };
            // The source actions are asked together with the parts, and only while the server
            // answers: the rows from the symbols need no server.
            let source = match (ready, source, support.actions()) {
                (true, Some(query), Some(actions)) => Some(ask(actions, query)),
                _ => None,
            };
            let mut answers = Vec::with_capacity(asked.len());
            for (kind, replies) in asked {
                answers.push((kind, known(first(replies).await)));
            }
            let mut rows: Vec<Pick> = crate::delete::generate_rows()
                .into_iter()
                .filter(|pick| match pick {
                    Pick::Generate { kind, .. } => shows(*kind, &place, &answers),
                    _ => true,
                })
                .collect();
            // The source actions the fixed rows do not give come after them.
            if let Some(replies) = source {
                let answer = first(replies).await;
                rows.extend(crate::actions::source_rows(
                    crate::actions::answered_entries(answer),
                    true,
                ));
            }
            this.update(cx, |this, cx| {
                // The answers are for the caret and the text they were asked about.
                let current = this.state.as_ref().is_some_and(|state| {
                    state.version() as i32 == version && state.selections().primary().head == caret
                });
                if current {
                    this.show_generate_picks(caret, rows, "Nothing to generate", cx);
                }
            })
            .ok();
        })
        .detach();
    }

    /// Alt+Insert for a language that only lists its source actions: the actions
    /// themselves are the rows, as they were before the fixed Source Generate list.
    fn generate_source_actions(&mut self, cx: &mut Context<Self>) {
        let Some(span) = self.selection_span() else {
            return;
        };
        let only = Some(lang_api::ActionKind::Source);
        self.ask_actions(span, only, cx, |this, _, caret, entries, cx| {
            let rows = crate::actions::source_rows(entries, false);
            this.show_generate_picks(caret, rows, "Nothing to generate here", cx);
        });
    }

    /// Whether the language server answers now. A row that asks it says so while it cannot.
    pub(crate) fn server_ready(&self) -> bool {
        self.lsp
            .as_ref()
            .is_some_and(|lsp| lsp.support.server_ready())
    }

    /// A row of the Generate list is chosen.
    pub(crate) fn start_generation(&mut self, kind: GenerateKind, cx: &mut Context<Self>) {
        if !self.server_ready() {
            self.report_status(SERVER_NOT_READY, cx);
            return;
        }
        if kind == GenerateKind::OverrideMethods {
            return self.override_methods(false, cx);
        }
        if kind == GenerateKind::ImplementMethods {
            return self.implement_methods_of_generate(cx);
        }
        let Some((spot, caret)) = self.spot() else {
            return;
        };
        let Some(provider) = self.lsp.as_ref().and_then(|lsp| lsp.support.generate()) else {
            self.show_note(caret, "This language cannot generate sources", cx);
            return;
        };
        let replies = ask(
            provider,
            GenerateQuery {
                spot: spot.clone(),
                kind,
            },
        );
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((generation, _)) => this.generation_answer(spot, kind, caret, generation, cx),
                None => {}
            })
            .ok();
        })
        .detach();
    }

    fn generation_answer(
        &mut self,
        spot: DocumentSpot,
        kind: GenerateKind,
        caret: usize,
        generation: Generation,
        cx: &mut Context<Self>,
    ) {
        if let Some(error) = generation.error {
            self.show_note(caret, error, cx);
            return;
        }
        if !generation.choices.is_empty() {
            let Some(lsp) = &mut self.lsp else {
                return;
            };
            lsp.completion = None;
            lsp.actions = None;
            lsp.note = None;
            lsp.rename = None;
            lsp.move_to = None;
            lsp.overrides = None;
            lsp.definition = None;
            lsp.safe_delete = None;
            lsp.generation = Some(GeneratePopup::new(caret, spot, kind, generation.choices));
            cx.notify();
            return;
        }
        if !generation.files.is_empty() {
            cx.emit(EditorEvent::CreateFiles(generation.files));
            return;
        }
        if let Some(change) = generation.change
            && !change.is_empty()
        {
            let version = spot.version;
            self.apply_change(version, change, cx);
            return;
        }
        // A class without fields has no part to choose, yet IDEA still writes the
        // no-argument constructor: the server is asked for that edit with no part chosen.
        if kind == GenerateKind::Constructor
            && self
                .lsp
                .as_ref()
                .is_some_and(|lsp| lsp.support.generate_apply().is_some())
        {
            self.apply_generation(spot, kind, Vec::new(), cx);
            return;
        }
        if let Some(text) = nothing_to_generate(kind) {
            self.report_status(text, cx);
            return;
        }
        self.show_note(caret, "Nothing to generate here", cx);
    }

    /// Typed text filters an open Generate popup. Returns whether it took the text.
    pub(crate) fn generation_typed(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(popup) = self.lsp.as_mut().and_then(|lsp| lsp.generation.as_mut()) else {
            return false;
        };
        popup.text.push_str(text.trim_end_matches(['\r', '\n']));
        popup.filter();
        cx.notify();
        true
    }

    /// Keys of an open Generate popup. Returns whether the key was taken.
    pub(crate) fn generation_key(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) -> bool {
        let key = event.keystroke.key.to_lowercase();
        let modifiers = event.keystroke.modifiers;
        let Some(lsp) = &mut self.lsp else {
            return false;
        };
        let Some(popup) = &mut lsp.generation else {
            return false;
        };
        let rows = popup.shown.len();
        match key.as_str() {
            "escape" => lsp.generation = None,
            "up" if rows > 0 => popup.selected = (popup.selected + rows - 1) % rows,
            "down" if rows > 0 => popup.selected = (popup.selected + 1) % rows,
            "space" if !modifiers.control && !modifiers.alt => popup.toggle_selected(),
            "a" if modifiers.control && !modifiers.alt => popup.check_all(),
            "backspace" => {
                popup.text.pop();
                popup.filter();
            }
            "enter" => {
                let chosen = popup.chosen();
                let (spot, kind) = (popup.spot.clone(), popup.kind);
                lsp.generation = None;
                if !chosen.is_empty() {
                    self.apply_generation(spot, kind, chosen, cx);
                }
            }
            other => {
                if is_typed_key(other) && !modifiers.control && !modifiers.alt {
                    return false;
                }
                return true;
            }
        }
        cx.notify();
        true
    }

    fn apply_generation(
        &mut self,
        spot: DocumentSpot,
        kind: GenerateKind,
        handles: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        if !self.server_ready() {
            self.report_status(SERVER_NOT_READY, cx);
            return;
        }
        let Some(provider) = self
            .lsp
            .as_ref()
            .and_then(|lsp| lsp.support.generate_apply())
        else {
            return;
        };
        let version = spot.version;
        let replies = ask(
            provider,
            GenerateApplyRequest {
                spot,
                kind,
                handles,
            },
        );
        let caret = self
            .state
            .as_ref()
            .map_or(0, |state| state.selections().primary().head);
        cx.spawn(async move |this, cx| {
            let answer = first(replies).await;
            this.update(cx, |this, cx| match answer {
                Some((_, Confidence::Unknown)) => this.show_note(caret, NO_ANSWER, cx),
                Some((generation, _)) if generation.error.is_some() => {
                    let error = generation.error.unwrap_or_default();
                    this.show_note(caret, error, cx);
                }
                Some((generation, _)) if !generation.files.is_empty() => {
                    cx.emit(EditorEvent::CreateFiles(generation.files));
                }
                Some((generation, _)) => match generation.change {
                    Some(change) if !change.is_empty() => this.apply_change(version, change, cx),
                    _ => this.show_note(caret, "Nothing to generate here", cx),
                },
                None => {}
            })
            .ok();
        })
        .detach();
    }
}

/// The kinds the server is asked about inside a class: equals() and hashCode() (its answer
/// also decides toString()), Delegate Methods and Test. The accessors and Implement Methods
/// are always listed in a class, so no question about them is needed for the list.
const CLASS_PROBES: [GenerateKind; 3] = [
    GenerateKind::EqualsAndHashCode,
    GenerateKind::Delegate,
    GenerateKind::Test,
];

/// The status bar message of an accessor row that has nothing to write, or `None` for the
/// rows that answer for themselves.
fn nothing_to_generate(kind: GenerateKind) -> Option<&'static str> {
    match kind {
        GenerateKind::Getter => Some("No fields without getter were found"),
        GenerateKind::Setter => Some("No fields without setter were found"),
        GenerateKind::GetterAndSetter => Some("No fields without getter or setter were found"),
        _ => None,
    }
}

/// Outside a class the list is short: Test is the one row that can apply there.
const OUTSIDE_PROBES: [GenerateKind; 1] = [GenerateKind::Test];

/// Where the caret is, as far as the symbols of the file say.
struct Place {
    /// False outside a class body: in an interface, or where no type encloses the caret.
    in_class: bool,
    /// The fields that directly belong to the class around the caret, when the language has
    /// symbols.
    members: Option<Members>,
}

struct Members {
    fields: Vec<String>,
}

impl Place {
    fn of(symbols: Option<&dyn SymbolProvider>, path: &Path, text: &str, caret: usize) -> Self {
        match symbols {
            // Without symbols the server decides, and the caret counts as inside a class.
            None => Self {
                in_class: true,
                members: None,
            },
            Some(symbols) => Self::from_symbols(&symbols.file_symbols(path, text), caret),
        }
    }

    /// The innermost class, enum or interface around `caret`. Its members are matched by
    /// depth and range, not by container: the container names differ between languages.
    fn from_symbols(symbols: &[Symbol], caret: usize) -> Self {
        let owner = symbols
            .iter()
            .filter(|symbol| {
                matches!(
                    symbol.kind,
                    SymbolKind::Class | SymbolKind::Enum | SymbolKind::Interface
                ) && symbol.range.contains(&caret)
            })
            .max_by_key(|symbol| symbol.depth);
        let Some(owner) = owner.filter(|owner| owner.kind != SymbolKind::Interface) else {
            return Self {
                in_class: false,
                members: None,
            };
        };
        let direct = |symbol: &&Symbol| {
            symbol.depth == owner.depth + 1
                && symbol.range.start >= owner.range.start
                && symbol.range.end <= owner.range.end
        };
        let names = |kind: SymbolKind| -> Vec<String> {
            symbols
                .iter()
                .filter(|symbol| symbol.kind == kind)
                .filter(direct)
                .map(|symbol| symbol.name.clone())
                .collect()
        };
        Self {
            in_class: true,
            members: Some(Members {
                fields: names(SymbolKind::Field),
            }),
        }
    }
}

/// The answer of a server that knows the question; `None` when it does not know it or did
/// not answer.
fn known<T>(answer: Option<(T, Confidence)>) -> Option<T> {
    answer.and_then(|(value, confidence)| (confidence != Confidence::Unknown).then_some(value))
}

/// Whether a row of the Generate list applies at the caret. Inside a class the accessors and
/// Implement Methods always stay: a row that has nothing to write says so when it is chosen,
/// as IDEA does.
fn shows(
    kind: GenerateKind,
    place: &Place,
    answers: &[(GenerateKind, Option<Generation>)],
) -> bool {
    match kind {
        GenerateKind::Test => server_or_symbols(kind, place, answers),
        _ if !place.in_class => false,
        GenerateKind::Constructor
        | GenerateKind::OverrideMethods
        | GenerateKind::Getter
        | GenerateKind::Setter
        | GenerateKind::GetterAndSetter
        | GenerateKind::ImplementMethods => true,
        _ => server_or_symbols(kind, place, answers),
    }
}

fn server_or_symbols(
    kind: GenerateKind,
    place: &Place,
    answers: &[(GenerateKind, Option<Generation>)],
) -> bool {
    // toString() needs the same fields as equals() and hashCode(), so one answer serves both.
    let asked = if kind == GenerateKind::ToString {
        GenerateKind::EqualsAndHashCode
    } else {
        kind
    };
    match answers.iter().find(|(probe, _)| *probe == asked) {
        Some((_, Some(generation))) => server_applies(generation),
        _ => place
            .members
            .as_ref()
            .is_none_or(|members| symbol_applies(kind, members)),
    }
}

/// Whether a server answer offers something to generate.
fn server_applies(generation: &Generation) -> bool {
    generation.error.is_none()
        && (!generation.choices.is_empty()
            || !generation.files.is_empty()
            || generation
                .change
                .as_ref()
                .is_some_and(|change| !change.is_empty()))
}

/// The rule of the symbols, used only when the server did not answer: equals(), hashCode()
/// and toString() need a field. The symbols cannot judge the other rows, so they stay.
fn symbol_applies(kind: GenerateKind, members: &Members) -> bool {
    match kind {
        GenerateKind::EqualsAndHashCode | GenerateKind::ToString => !members.fields.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use std::ops::Range;

    use lang_api::{GenerateKind, Symbol, SymbolKind};

    use super::{Members, Place, symbol_applies};

    fn symbol(name: &str, kind: SymbolKind, depth: u32, range: Range<usize>) -> Symbol {
        Symbol {
            name: name.to_owned(),
            kind,
            container: None,
            depth,
            name_range: range.clone(),
            range,
        }
    }

    #[test]
    fn the_innermost_class_gives_its_direct_members() {
        // class A { int f; class B { int g; } }
        let symbols = vec![
            symbol("A", SymbolKind::Class, 0, 0..40),
            symbol("f", SymbolKind::Field, 1, 10..16),
            symbol("B", SymbolKind::Class, 1, 20..38),
            symbol("g", SymbolKind::Field, 2, 26..32),
        ];
        let inside_b = Place::from_symbols(&symbols, 30);
        assert!(inside_b.in_class);
        assert_eq!(inside_b.members.expect("B has members").fields, ["g"]);
        let inside_a = Place::from_symbols(&symbols, 12);
        assert_eq!(inside_a.members.expect("A has members").fields, ["f"]);
        let interface = [symbol("I", SymbolKind::Interface, 0, 0..20)];
        assert!(!Place::from_symbols(&interface, 5).in_class);
    }

    #[test]
    fn the_symbols_drop_equals_and_tostring_only_without_fields() {
        let with_field = Members {
            fields: vec!["name".to_owned()],
        };
        assert!(symbol_applies(GenerateKind::EqualsAndHashCode, &with_field));
        assert!(symbol_applies(GenerateKind::ToString, &with_field));
        let bare = Members { fields: Vec::new() };
        assert!(!symbol_applies(GenerateKind::EqualsAndHashCode, &bare));
        assert!(!symbol_applies(GenerateKind::ToString, &bare));
        // The accessors are not judged by the symbols: their rows always stay in a class.
        assert!(symbol_applies(GenerateKind::Getter, &bare));
    }
}

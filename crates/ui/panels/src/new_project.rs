//! The New Project wizard: the generators the application supplies on the left, the
//! fields of the chosen one on the right, and the folder the project is written into.
//! The form itself knows no language and no build tool: a generator answers the fields.

use std::path::PathBuf;
use std::sync::Arc;

use project_model::scaffold::{self, Field, Generator, Kind, Request, Values};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;

use crate::prompt::TextField;
use crate::start::StartScreen;
use crate::tree_ops::check_name;

/// What a key or a click asks the screen to do. The wizard never starts work itself: the
/// screen owns the window and the program of the project.
pub(crate) enum Step {
    None,
    /// The form changed and needs a frame.
    Changed,
    /// The choices of the generator have to be asked again, away from this thread.
    Load,
    Create(Plan),
}

/// A project to write: the generator, and everything the user filled in.
pub(crate) struct Plan {
    pub generator: Arc<dyn Generator>,
    pub request: Request,
}

/// The input of the form that takes the keys.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Active {
    Name,
    Location,
    Row(usize),
}

/// Where the message under a field belongs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Where {
    Name,
    Location,
    Row(usize),
    Create,
}

/// One row of the form: the field of the generator and what the user typed into it.
struct Row {
    field: Field,
    /// The text of a text field, or the query of the list of a many field.
    text: TextField,
    /// The value of a choice field.
    selected: String,
    /// The values of a many field, in the order the user chose them.
    many: Vec<String>,
    /// The match of a searchable list that Enter picks.
    highlight: usize,
    on: bool,
    /// Whether the user typed into a text field of their own.
    touched: bool,
}

impl Row {
    fn new(field: Field) -> Self {
        let selected = field.default.clone();
        let on = field.default == "true";
        let text = match field.kind {
            Kind::Text => TextField::from_text(&field.default),
            _ => TextField::default(),
        };
        Self {
            field,
            text,
            selected,
            many: Vec::new(),
            highlight: 0,
            on,
            touched: false,
        }
    }

    /// Takes the choices of a newer answer of the generator, keeping what the user typed.
    fn merge(&mut self, field: Field) {
        match field.kind {
            Kind::Text => {
                if !self.touched {
                    self.text = TextField::from_text(&field.default);
                }
            }
            Kind::Choice => {
                if !field
                    .choices
                    .iter()
                    .any(|choice| choice.value == self.selected)
                {
                    self.selected = field.default.clone();
                }
            }
            Kind::Many => {
                if !field.choices.is_empty() {
                    self.many
                        .retain(|value| field.choices.iter().any(|choice| choice.value == *value));
                }
            }
            Kind::Toggle => {}
        }
        self.field = field;
    }

    fn visible(&self, rows: &[Row]) -> bool {
        let Some((key, value)) = &self.field.shown_when else {
            return true;
        };
        rows.iter()
            .find(|row| row.field.key == *key)
            .is_some_and(|row| row.selected == *value)
    }
}

/// The form of a new project.
pub(crate) struct NewProject {
    generators: Vec<Arc<dyn Generator>>,
    chosen: usize,
    rows: Vec<Row>,
    name: TextField,
    location: TextField,
    git: bool,
    active: Active,
    error: Option<(Where, SharedString)>,
    /// What the server answered, shown under a field without keeping the project back.
    notice: Option<(Where, SharedString)>,
    /// The answer of the server is on its way.
    loading: bool,
    /// The number of the last load, so an answer for another generator is dropped.
    generation: u64,
    scroll: ScrollHandle,
}

impl NewProject {
    pub fn new(generators: Vec<Arc<dyn Generator>>, location: PathBuf) -> Self {
        let mut wizard = Self {
            generators,
            chosen: 0,
            rows: Vec::new(),
            name: TextField::default(),
            location: TextField::from_text(location.display().to_string()),
            git: false,
            active: Active::Name,
            error: None,
            notice: None,
            loading: false,
            generation: 0,
            scroll: ScrollHandle::new(),
        };
        wizard.rows = wizard.fields(0);
        wizard.validate();
        wizard
    }

    /// The fields of one generator, taken on the thread that shows them.
    fn fields(&self, index: usize) -> Vec<Row> {
        self.generators
            .get(index)
            .map(|generator| generator.fields())
            .unwrap_or_default()
            .into_iter()
            .map(Row::new)
            .collect()
    }

    pub fn has_generators(&self) -> bool {
        !self.generators.is_empty()
    }

    /// Whether the choices of the generator come from outside and can be asked again.
    pub fn reloads(&self) -> bool {
        self.generators
            .get(self.chosen)
            .is_some_and(|generator| generator.loads())
    }

    /// The folder the project goes into.
    pub fn folder(&self) -> PathBuf {
        PathBuf::from(self.location.text.trim()).join(self.name.text.trim())
    }

    /// The values of the fields as the load of a generator reads them.
    pub fn values_for_load(&self) -> Values {
        self.values()
    }

    fn values(&self) -> Values {
        let mut values = Values::default();
        for row in &self.rows {
            match row.field.kind {
                Kind::Text => values.set(&row.field.key, row.text.text.clone()),
                Kind::Choice => values.set(&row.field.key, row.selected.clone()),
                Kind::Many => values.set_many(&row.field.key, row.many.clone()),
                Kind::Toggle => values.set_flag(&row.field.key, row.on),
            }
        }
        values
    }

    fn request(&self) -> Request {
        Request {
            name: self.name.text.trim().to_owned(),
            folder: self.folder(),
            values: self.values(),
            git: self.git,
        }
    }

    /// Fills the message the form shows under a field, and answers whether the project
    /// can be written: a name of its own in a folder that is empty or does not exist.
    fn validate(&mut self) {
        self.error = None;
        if self.generators.is_empty() {
            self.error = Some((Where::Create, "No project generators are registered".into()));
            return;
        }
        let name = self.name.text.trim();
        if let Err(error) = check_name(name) {
            self.error = Some((Where::Name, error.into()));
            return;
        }
        if let Some(problem) = scaffold::folder_problem(&self.folder()) {
            self.error = Some((Where::Location, problem.into()));
        }
    }

    pub fn can_create(&self) -> bool {
        self.error.is_none() && self.has_generators()
    }

    /// The message under the Create button or a field, for a test without pixels.
    #[cfg(test)]
    pub fn error_text(&self) -> Option<String> {
        self.error.as_ref().map(|(_, text)| text.to_string())
    }

    /// What the server answered, for a test without pixels.
    #[cfg(test)]
    pub fn notice_text(&self) -> Option<String> {
        self.notice.as_ref().map(|(_, text)| text.to_string())
    }

    pub fn generator_index(&self) -> usize {
        self.chosen
    }

    /// The value of a field as the project is written.
    #[cfg(test)]
    pub fn value(&self, key: &str) -> Option<String> {
        let row = self.rows.iter().find(|row| row.field.key == key)?;
        Some(match row.field.kind {
            Kind::Text => row.text.text.clone(),
            Kind::Choice => row.selected.clone(),
            Kind::Many => row.many.join(", "),
            Kind::Toggle => row.on.to_string(),
        })
    }

    #[cfg(test)]
    pub fn choices(&self, key: &str) -> Vec<String> {
        self.rows
            .iter()
            .find(|row| row.field.key == key)
            .map(|row| {
                row.field
                    .choices
                    .iter()
                    .map(|choice| choice.value.clone())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The query of a searchable list, for a test.
    #[cfg(test)]
    pub fn search_text(&self, key: &str) -> Option<String> {
        self.rows
            .iter()
            .find(|row| row.field.key == key)
            .map(|row| row.text.text.clone())
    }

    #[cfg(test)]
    pub fn chosen(&self, key: &str) -> Vec<String> {
        self.rows
            .iter()
            .find(|row| row.field.key == key)
            .map(|row| row.many.clone())
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub fn loading(&self) -> bool {
        self.loading
    }

    #[cfg(test)]
    pub fn set_fields_for_test(&mut self, name: &str, location: &str) {
        self.name.text = name.to_owned();
        self.location.text = location.to_owned();
        self.validate();
    }

    /// A click on a generator: the fields of that generator and the choices it has now.
    pub fn choose_generator(&mut self, index: usize) -> Step {
        if index == self.chosen && !self.rows.is_empty() {
            self.notice = None;
            return Step::Load;
        }
        self.chosen = index;
        self.rows = self.fields(index);
        self.active = Active::Name;
        self.notice = None;
        self.validate();
        Step::Load
    }

    /// Marks a load as started and answers its number.
    pub fn begin_load(&mut self) -> u64 {
        self.generation += 1;
        self.loading = true;
        self.generation
    }

    /// The answer of the server, if it belongs to the generator the form shows now.
    pub fn loaded(&mut self, generation: u64, answer: Result<Vec<Field>, String>) {
        if generation != self.generation {
            return;
        }
        self.loading = false;
        match answer {
            Ok(fields) => {
                self.notice = None;
                for field in fields {
                    if let Some(row) = self.rows.iter_mut().find(|row| row.field.key == field.key) {
                        row.merge(field);
                    } else {
                        self.rows.push(Row::new(field));
                    }
                }
                self.validate();
            }
            Err(error) => {
                let at = self
                    .rows
                    .iter()
                    .position(|row| row.field.kind == Kind::Many)
                    .map_or(Where::Create, Where::Row);
                self.notice = Some((at, error.into()));
            }
        }
    }

    fn next_input(&mut self, back: bool) {
        let inputs: Vec<Active> = std::iter::once(Active::Name)
            .chain(std::iter::once(Active::Location))
            .chain(
                self.rows
                    .iter()
                    .enumerate()
                    .filter(|(_, row)| matches!(row.field.kind, Kind::Text | Kind::Many))
                    .map(|(index, _)| Active::Row(index)),
            )
            .collect();
        let at = inputs.iter().position(|input| *input == self.active);
        let next = match (at, back) {
            (Some(0), true) => inputs.len() - 1,
            (Some(index), true) => index - 1,
            (Some(index), false) => (index + 1) % inputs.len(),
            (None, _) => 0,
        };
        self.active = inputs[next];
    }

    fn active_field(&mut self) -> Option<&mut TextField> {
        match self.active {
            Active::Name => Some(&mut self.name),
            Active::Location => Some(&mut self.location),
            Active::Row(index) => self.rows.get_mut(index).map(|row| &mut row.text),
        }
    }

    /// Moves the keys of the form. `escape` stays with the screen, which leaves the mode.
    pub fn key(&mut self, event: &KeyDownEvent, cx: &mut App) -> Step {
        let mut taken = false;
        let key = event.keystroke.key.as_str();
        if let Active::Row(index) = self.active
            && self.rows[index].field.kind == Kind::Many
            && !self.rows[index].text.text.is_empty()
        {
            match key {
                "down" => return self.move_match(index, 1),
                "up" => return self.move_match(index, -1),
                "enter" => return self.take_match(index),
                _ => {}
            }
        }
        match key {
            "tab" => {
                self.next_input(event.keystroke.modifiers.shift);
                taken = true;
            }
            "enter" => {
                taken = true;
                self.validate();
                if self.can_create() {
                    return Step::Create(self.plan());
                }
            }
            _ => {
                if let Active::Row(index) = self.active
                    && self.rows[index].field.kind == Kind::Text
                {
                    self.rows[index].touched = true;
                }
                if let Some(field) = self.active_field() {
                    taken = field.key_with_clipboard(event, false, cx);
                }
                if taken
                    && let Active::Row(index) = self.active
                    && self.rows[index].field.kind == Kind::Many
                {
                    self.rows[index].highlight = 0;
                }
            }
        }
        if !taken {
            return Step::None;
        }
        if self.active == Active::Name {
            let artifact = project_model::scaffold::artifact_name(self.name.text.trim());
            for row in &mut self.rows {
                if row.field.from_name && !row.touched {
                    row.text = TextField::from_text(&artifact);
                }
            }
        }
        self.validate();
        Step::Changed
    }

    /// The plan of the project the form describes.
    pub fn plan(&self) -> Plan {
        let generator = self
            .generators
            .get(self.chosen)
            .cloned()
            .expect("a plan has a generator");
        Plan {
            generator,
            request: self.request(),
        }
    }

    /// The Create button pressed by its key or by the test of the window.
    pub fn create_step(&mut self) -> Step {
        self.validate();
        if self.can_create() {
            Step::Create(self.plan())
        } else {
            Step::Changed
        }
    }

    /// The values of a searchable list that match its query, as the list shows them.
    fn matches(&self, row: usize) -> Vec<String> {
        let row = &self.rows[row];
        let query = row.text.text.to_lowercase();
        row.field
            .choices
            .iter()
            .filter(|choice| {
                query.is_empty()
                    || choice.label.to_lowercase().contains(&query)
                    || choice.value.to_lowercase().contains(&query)
            })
            .take(60)
            .map(|choice| choice.value.clone())
            .collect()
    }

    fn move_match(&mut self, row: usize, step: isize) -> Step {
        let count = self.matches(row).len();
        if count == 0 {
            return Step::None;
        }
        let row = &mut self.rows[row];
        let at = (row.highlight as isize + step).clamp(0, count as isize - 1);
        row.highlight = at as usize;
        Step::Changed
    }

    /// Enter of a searchable list picks the highlighted match.
    fn take_match(&mut self, row: usize) -> Step {
        let matches = self.matches(row);
        let Some(value) = matches
            .get(
                self.rows[row]
                    .highlight
                    .min(matches.len().saturating_sub(1)),
            )
            .cloned()
        else {
            return Step::None;
        };
        self.toggle_many(row, &value)
    }

    fn choose(&mut self, row: usize, value: &str) -> Step {
        if let Some(row) = self.rows.get_mut(row) {
            row.selected = value.to_owned();
        }
        self.validate();
        Step::Changed
    }

    fn toggle(&mut self, row: usize) -> Step {
        if let Some(found) = self.rows.get_mut(row) {
            found.on = !found.on;
        }
        Step::Changed
    }

    fn toggle_many(&mut self, row: usize, value: &str) -> Step {
        if let Some(found) = self.rows.get_mut(row) {
            if let Some(at) = found.many.iter().position(|chosen| chosen == value) {
                found.many.remove(at);
            } else {
                found.many.push(value.to_owned());
            }
        }
        Step::Changed
    }

    fn error_at(&self, at: Where) -> Option<SharedString> {
        self.error
            .as_ref()
            .filter(|(where_, _)| *where_ == at)
            .map(|(_, text)| text.clone())
    }

    fn notice_at(&self, at: Where) -> Option<SharedString> {
        self.notice
            .as_ref()
            .filter(|(where_, _)| *where_ == at)
            .map(|(_, text)| text.clone())
    }

    /// The message a field shows under itself: a wrong value, or what the server said.
    fn message_at(&self, at: Where) -> Option<SharedString> {
        self.error_at(at).or_else(|| self.notice_at(at))
    }

    fn visible_rows(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.visible(&self.rows))
            .map(|(index, _)| index)
            .collect()
    }

    pub fn render(&mut self, window: &mut Window, cx: &mut Context<StartScreen>) -> AnyElement {
        let theme = *cx.global::<Theme>();
        let visible = self.visible_rows();
        let folder = self.folder();
        let active = self.active;
        let (accent, over, pressed, hover, muted) = (
            theme.accent,
            theme.over_accent,
            theme.pressed,
            theme.hover,
            theme.text_muted,
        );
        let generators: Vec<AnyElement> = self
            .generators
            .iter()
            .enumerate()
            .map(|(index, generator)| {
                let on = index == self.chosen;
                let label = generator.label().to_owned();
                div()
                    .id(("new-project-generator", index))
                    .debug_selector(move || format!("new-project-generator-{index}"))
                    .flex()
                    .items_center()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(on, |this| this.bg(accent).text_color(over))
                    .when(!on, |this| this.hover(move |style| style.bg(hover)))
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let step = this.wizard.choose_generator(index);
                        this.run_wizard_step(step, window, cx);
                    }))
                    .into_any_element()
            })
            .collect();

        let mut content = div()
            .id("new-project-fields")
            .flex()
            .flex_col()
            .gap(px(DIALOG_GAP))
            .p(px(SPACE_S))
            .child(label_text(
                "Name",
                None,
                self.message_at(Where::Name),
                active == Active::Name,
                &theme,
            ))
            .child(
                text_box(
                    FieldMouse::new(cx, |this| Some(&mut this.name)),
                    "new-project-name",
                    &self.name,
                    active == Active::Name,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.wizard.active = Active::Name;
                    this.run_wizard_step(Step::Changed, window, cx);
                })),
            )
            .child(
                label_text(
                    "Location",
                    None,
                    self.message_at(Where::Location),
                    active == Active::Location,
                    &theme,
                )
                .child(
                    div()
                        .debug_selector(|| "new-project-path".to_owned())
                        .text_color(theme.text_secondary)
                        .child(format!("Project will be created in: {}", folder.display())),
                ),
            )
            .child(
                text_box(
                    FieldMouse::new(cx, |this| Some(&mut this.location)),
                    "new-project-location",
                    &self.location,
                    active == Active::Location,
                    &theme,
                )
                .on_click(cx.listener(|this, _, window, cx| {
                    this.wizard.active = Active::Location;
                    this.run_wizard_step(Step::Changed, window, cx);
                })),
            )
            .child(
                div()
                    .id("new-project-git")
                    .debug_selector(|| "new-project-git".to_owned())
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .h(px(ROW_HEIGHT))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.wizard.git = !this.wizard.git;
                        this.run_wizard_step(Step::Changed, window, cx);
                    }))
                    .child(checkbox(self.git, &theme))
                    .child("Create Git repository"),
            );

        for index in visible {
            let row = &self.rows[index];
            let kind = row.field.kind.clone();
            let key = row.field.key.clone();
            let label = row.field.label.clone();
            let hint = row.field.hint.clone();
            let text = row.text.clone();
            let empty_query = row.text.text.is_empty();
            let error = self.message_at(Where::Row(index));
            let active = self.active == Active::Row(index);
            content = content.child(label_text(&label, hint, error, active, &theme));
            match kind {
                Kind::Text => {
                    content = content.child(
                        text_box(
                            FieldMouse::new(cx, move |this| {
                                this.wizard.rows.get_mut(index).map(|row| &mut row.text)
                            }),
                            &format!("new-project-field-{key}"),
                            &text,
                            active,
                            &theme,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.wizard.active = Active::Row(index);
                                this.run_wizard_step(Step::Changed, window, cx);
                            },
                        )),
                    );
                }
                Kind::Choice => content = content.child(self.choice_row(index, &theme, cx)),
                Kind::Toggle => content = content.child(self.toggle_row(index, &theme, cx)),
                Kind::Many => {
                    content = content
                        .child(self.chips_row(index, &theme, cx))
                        .child(
                            div()
                                .id(format!("new-project-search-{key}"))
                                .debug_selector({
                                    let selector = format!("new-project-search-{key}");
                                    move || selector.clone()
                                })
                                .when(empty_query && !active, |this| {
                                    this.child(
                                        div()
                                            .px(px(SPACE_M))
                                            .h(px(32.))
                                            .flex()
                                            .items_center()
                                            .rounded(px(CONTROL_RADIUS))
                                            .border_2()
                                            .border_color(theme.border)
                                            .text_color(muted)
                                            .child("Search dependencies"),
                                    )
                                })
                                .when(!empty_query || active, |this| {
                                    this.child(text_box(
                                        FieldMouse::new(cx, move |this| {
                                            this.wizard.rows.get_mut(index).map(|row| &mut row.text)
                                        }),
                                        &format!("new-project-search-{key}-field"),
                                        &text,
                                        active,
                                        &theme,
                                    ))
                                })
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.wizard.active = Active::Row(index);
                                    this.run_wizard_step(Step::Changed, window, cx);
                                })),
                        )
                        .child(self.dependency_list(index, &theme, cx));
                }
            }
        }

        let enabled = self.can_create();
        let loading = self.loading;
        let create = div()
            .id("new-project-create")
            .debug_selector(|| "new-project-create".to_owned())
            .flex()
            .items_center()
            .justify_center()
            .h(px(CONTROL_HEIGHT))
            .px(px(SPACE_L))
            .rounded(px(CONTROL_RADIUS))
            .when(enabled, |this| {
                this.bg(accent)
                    .text_color(over)
                    .hover(move |style| style.bg(hover))
                    .on_click(cx.listener(|this, _, window, cx| {
                        let step = this.wizard.create_step();
                        this.run_wizard_step(step, window, cx);
                    }))
            })
            .when(!enabled, |this| this.bg(pressed).text_color(muted))
            .child(if loading { "Loading..." } else { "Create" });
        let reload = self.reloads().then(|| {
            div()
                .id("new-project-reload")
                .debug_selector(|| "new-project-reload".to_owned())
                .flex()
                .items_center()
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .bg(pressed)
                .hover(move |style| style.bg(hover))
                .child("Reload metadata")
                .on_click(cx.listener(|this, _, window, cx| {
                    this.run_wizard_step(Step::Load, window, cx);
                }))
        });
        let mut footer = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .px(px(SPACE_S))
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(SPACE_M))
                    .child(create)
                    .children(reload),
            );
        if let Some((Where::Create, text)) = self.error.clone() {
            footer = footer.child(
                div()
                    .debug_selector(|| "new-project-error".to_owned())
                    .text_color(theme.stripe_error)
                    .child(text),
            );
        }
        if loading && self.error.is_none() {
            footer = footer.child(
                div()
                    .text_color(theme.text_secondary)
                    .child("Loading the metadata of the server..."),
            );
        }

        div()
            .id("new-project")
            .flex()
            .flex_1()
            .gap(px(SPACE_XL))
            .min_h_0()
            .child(
                div()
                    .flex()
                    .flex_col()
                    .w(px(180.))
                    .gap(px(SPACE_XS))
                    .child(div().font_weight(FontWeight::MEDIUM).child("New Project"))
                    .children(generators),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .gap(px(SPACE_M))
                    .child(
                        scrollable(
                            "new-project-form",
                            &self.scroll,
                            div()
                                .id("new-project-form")
                                .flex()
                                .flex_col()
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .child(content),
                            &theme,
                            window,
                            cx,
                        )
                        .flex_1()
                        .min_h_0()
                        .min_w_0(),
                    )
                    .child(footer),
            )
            .into_any_element()
    }

    fn choice_row(&mut self, index: usize, theme: &Theme, cx: &mut Context<StartScreen>) -> Div {
        let row = &self.rows[index];
        let key = row.field.key.clone();
        let selected = row.selected.clone();
        let choices: Vec<(usize, String, String)> = row
            .field
            .choices
            .iter()
            .enumerate()
            .map(|(at, choice)| (at, choice.value.clone(), choice.label.clone()))
            .collect();
        let (accent, over, pressed, hover, muted) = (
            theme.accent,
            theme.over_accent,
            theme.pressed,
            theme.hover,
            theme.text_muted,
        );
        let empty = choices.is_empty();
        let pills: Vec<AnyElement> = choices
            .into_iter()
            .map(|(at, value, label)| {
                let on = value == selected;
                let selector_key = key.clone();
                let selector_value = value.clone();
                div()
                    .id(format!("new-project-choice-{index}-{at}"))
                    .debug_selector(move || {
                        format!("new-project-choice-{selector_key}-{selector_value}")
                    })
                    .flex()
                    .items_center()
                    .h(px(CONTROL_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .when(on, |this| this.bg(accent).text_color(over))
                    .when(!on, |this| {
                        this.bg(pressed).hover(move |style| style.bg(hover))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let step = this.wizard.choose(index, &value);
                        this.run_wizard_step(step, window, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        div()
            .flex()
            .flex_wrap()
            .gap(px(SPACE_S))
            .when(empty, |this| {
                this.child(div().text_color(muted).child("No values"))
            })
            .children(pills)
    }

    fn toggle_row(
        &mut self,
        index: usize,
        theme: &Theme,
        cx: &mut Context<StartScreen>,
    ) -> Stateful<Div> {
        let on = self.rows[index].on;
        let label = self.rows[index].field.label.clone();
        div()
            .id(format!("new-project-toggle-{index}"))
            .debug_selector(move || format!("new-project-toggle-{index}"))
            .flex()
            .items_center()
            .gap(px(SPACE_M))
            .h(px(ROW_HEIGHT))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, window, cx| {
                let step = this.wizard.toggle(index);
                this.run_wizard_step(step, window, cx);
            }))
            .child(checkbox(on, theme))
            .child(label)
    }

    fn chips_row(&mut self, index: usize, theme: &Theme, cx: &mut Context<StartScreen>) -> Div {
        let row = &self.rows[index];
        let labels: Vec<(String, String)> = row
            .many
            .iter()
            .map(|value| {
                let label = row
                    .field
                    .choices
                    .iter()
                    .find(|choice| choice.value == *value)
                    .map(|choice| choice.label.clone())
                    .unwrap_or_else(|| value.clone());
                (value.clone(), label)
            })
            .collect();
        let (accent, over) = (theme.accent, theme.over_accent);
        let chips: Vec<AnyElement> = labels
            .into_iter()
            .map(|(value, label)| {
                let selector_value = value.clone();
                div()
                    .id(format!("new-project-chip-{index}-{value}"))
                    .debug_selector(move || format!("new-project-chip-{selector_value}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .h(px(ROW_HEIGHT))
                    .px(px(SPACE_M))
                    .rounded(px(TAB_RADIUS))
                    .bg(accent)
                    .text_color(over)
                    .child(label)
                    .child(icon_sized("general/closeSmall", 12., theme))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let step = this.wizard.toggle_many(index, &value);
                        this.run_wizard_step(step, window, cx);
                    }))
                    .into_any_element()
            })
            .collect();
        div().flex().flex_wrap().gap(px(SPACE_S)).children(chips)
    }

    fn dependency_list(
        &mut self,
        index: usize,
        theme: &Theme,
        cx: &mut Context<StartScreen>,
    ) -> Div {
        let row = &self.rows[index];
        let query = row.text.text.to_lowercase();
        let chosen = row.many.clone();
        let highlight = row.highlight;
        let mut found = 0usize;
        let matches: Vec<(String, String, Option<String>, bool)> = row
            .field
            .choices
            .iter()
            .filter(|choice| {
                query.is_empty()
                    || choice.label.to_lowercase().contains(&query)
                    || choice.value.to_lowercase().contains(&query)
            })
            .take(60)
            .map(|choice| {
                let at = found;
                found += 1;
                (
                    choice.value.clone(),
                    choice.label.clone(),
                    choice.group.clone(),
                    at == highlight,
                )
            })
            .collect();
        let (hover, muted, accent, over) = (
            theme.hover,
            theme.text_muted,
            theme.accent,
            theme.over_accent,
        );
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut group: Option<String> = None;
        for (value, label, at_group, highlighted) in matches {
            if at_group != group {
                if let Some(name) = &at_group {
                    rows.push(
                        div()
                            .mt(px(SPACE_S))
                            .text_size(px(TEXT_SMALL))
                            .text_color(muted)
                            .child(name.clone())
                            .into_any_element(),
                    );
                }
                group = at_group;
            }
            let on = chosen.contains(&value);
            let selector_value = value.clone();
            rows.push(
                div()
                    .id(format!("new-project-dependency-{index}-{value}"))
                    .debug_selector(move || format!("new-project-dependency-{selector_value}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(on, |this| this.bg(accent).text_color(over))
                    .when(!on && highlighted, |this| {
                        this.bg(theme.selection).text_color(theme.text)
                    })
                    .when(!on && !highlighted, |this| {
                        this.hover(move |style| style.bg(hover))
                    })
                    .child(label)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        let step = this.wizard.toggle_many(index, &value);
                        this.run_wizard_step(step, window, cx);
                    }))
                    .into_any_element(),
            );
        }
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .when(rows.is_empty(), |this| {
                this.child(div().text_color(muted).child(if query.is_empty() {
                    "Reload the metadata of the server to list its dependencies"
                } else {
                    "No dependency matches"
                }))
            })
            .children(rows)
    }
}

/// A label over a field, with an optional hint and the message of a wrong value.
fn label_text(
    label: &str,
    hint: Option<String>,
    error: Option<SharedString>,
    active: bool,
    theme: &Theme,
) -> Div {
    let mut column = div().flex().flex_col().gap(px(SPACE_XS)).child(
        div()
            .text_color(if active {
                theme.accent
            } else {
                theme.text_secondary
            })
            .child(label.to_owned()),
    );
    if let Some(hint) = hint {
        column = column.child(
            div()
                .text_color(theme.text_muted)
                .text_size(px(TEXT_SMALL))
                .child(hint),
        );
    }
    if let Some(text) = error {
        column = column.child(
            div()
                .debug_selector(|| "new-project-error".to_owned())
                .text_color(theme.stripe_error)
                .child(text),
        );
    }
    column
}

/// The frame of one line of text, drawn by the one renderer of `ui-kit`.
fn text_box(
    mouse: FieldMouse,
    selector: &str,
    field: &TextField,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let debug = selector.to_owned();
    div()
        .id(selector.to_owned())
        .debug_selector(move || debug.clone())
        .flex()
        .items_center()
        .h(px(32.))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_2()
        .border_color(if active { theme.accent } else { theme.border })
        .children(text_field_children_named(
            mouse,
            "new-project-input",
            field,
            theme.text,
            theme.text,
            theme.selection,
            theme.caret,
            18.,
            active,
        ))
}

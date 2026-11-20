//! Create Test: the form of a new test class. The window asks the language for the
//! libraries, the defaults and the methods, then sends the values back and shows the file
//! it made. No library, package or build tool is known here.

use editor_view::{EditorView, Semantic};
use futures_util::StreamExt;
use lang_api::{
    Answer, Confidence, TestCreateOutcome, TestCreateRequest, TestFile, TestFixRequest, TestForm,
    ask,
};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::{Prompt, TextField};
use crate::tree_ops::check_name;

const WIDTH: f32 = 560.;
const FIELD_HEIGHT: f32 = 26.;
const NO_ANSWER: &str = "The language did not answer";
const NO_SUPPORT: &str = "This language cannot create tests";
const NO_FILE: &str = "The language made no test file";

/// A row of the form the keys move between.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Row {
    Framework(usize),
    ClassName,
    Superclass,
    Package,
    Folder(usize),
    Setup,
    Teardown,
    ShowInherited,
    Method(usize),
}

/// The Create Test window: the library, the names and places of the new test, and the
/// methods it gets.
pub(crate) struct CreateTestDialog {
    spot: lang_api::DocumentSpot,
    form: Option<TestForm>,
    class_name: TextField,
    superclass: TextField,
    package: TextField,
    show_inherited: bool,
    rows: Vec<Row>,
    active: usize,
    scroll: ScrollHandle,
    loading: bool,
    busy: bool,
    status: Option<String>,
    error: Option<String>,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
}

impl CreateTestDialog {
    pub(crate) fn show(
        spot: lang_api::DocumentSpot,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            spot,
            form: None,
            class_name: TextField::from_text(""),
            superclass: TextField::from_text(""),
            package: TextField::from_text(""),
            show_inherited: false,
            rows: Vec::new(),
            active: 0,
            scroll: ScrollHandle::new(),
            loading: true,
            busy: false,
            status: None,
            error: None,
            focus: cx.focus_handle(),
            workspace: weak,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.clone().into(), focus, window, cx);
        view.update(cx, |this, cx| this.load(cx));
    }

    /// Asks the language for the defaults of the form. The window is on screen before the
    /// answer arrives.
    fn load(&mut self, cx: &mut Context<Self>) {
        let Some(provider) = cx
            .try_global::<Semantic>()
            .and_then(|semantic| semantic.0.test_form())
        else {
            self.loading = false;
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let mut replies = ask(provider, lang_api::TestFormQuery(self.spot.clone()));
        cx.spawn(async move |this, cx| {
            let answer = replies.next().await;
            this.update(cx, |this, cx| {
                this.loading = false;
                match answer {
                    Some(answer) if answer.confidence == Confidence::Unknown => {
                        this.error = Some(NO_ANSWER.to_owned());
                    }
                    Some(answer) => this.form_arrived(answer.value, cx),
                    None => this.error = Some(NO_ANSWER.to_owned()),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    fn form_arrived(&mut self, form: TestForm, cx: &mut Context<Self>) {
        if let Some(error) = &form.error {
            self.error = Some(error.clone());
            self.loading = false;
            return;
        }
        self.class_name = TextField::from_text(form.class_name.clone());
        self.superclass = TextField::from_text(form.superclass.clone());
        self.package = TextField::from_text(form.package.clone());
        self.show_inherited = false;
        self.form = Some(form);
        self.rebuild_rows();
        cx.notify();
    }

    fn frameworks(&self) -> usize {
        self.form.as_ref().map_or(0, |form| form.frameworks.len())
    }

    fn folders(&self) -> usize {
        self.form.as_ref().map_or(0, |form| form.folders.len())
    }

    fn rebuild_rows(&mut self) {
        let mut rows = Vec::new();
        rows.extend((0..self.frameworks()).map(Row::Framework));
        rows.push(Row::ClassName);
        rows.push(Row::Superclass);
        rows.push(Row::Package);
        rows.extend((0..self.folders()).map(Row::Folder));
        rows.push(Row::Setup);
        rows.push(Row::Teardown);
        rows.push(Row::ShowInherited);
        if let Some(form) = &self.form {
            rows.extend(
                form.methods
                    .iter()
                    .enumerate()
                    .filter(|(_, method)| self.show_inherited || !method.inherited)
                    .map(|(index, _)| Row::Method(index)),
            );
        }
        self.rows = rows;
        self.active = self.active.min(self.rows.len().saturating_sub(1));
    }

    fn next_row(&self, step: i32) -> usize {
        if self.rows.is_empty() {
            return 0;
        }
        let len = self.rows.len() as i32;
        (self.active as i32 + step).rem_euclid(len) as usize
    }

    fn activate(&mut self, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(self.active).copied() else {
            return;
        };
        let Some(form) = &mut self.form else {
            return;
        };
        match row {
            Row::Framework(index) => form.framework = index,
            Row::Folder(index) => form.folder = index,
            Row::Setup => form.setup = !form.setup,
            Row::Teardown => form.teardown = !form.teardown,
            Row::ShowInherited => {
                self.show_inherited = !self.show_inherited;
                self.rebuild_rows();
            }
            Row::Method(index) => {
                if let Some(method) = form.methods.get_mut(index) {
                    method.checked = !method.checked;
                }
            }
            Row::ClassName | Row::Superclass | Row::Package => return,
        }
        self.error = None;
        cx.notify();
    }

    fn field_row(&self) -> Option<Row> {
        match self.rows.get(self.active).copied() {
            Some(row @ (Row::ClassName | Row::Superclass | Row::Package)) => Some(row),
            _ => None,
        }
    }

    /// Keys of the form: the arrows move between rows, Space marks a row, text goes to
    /// the field under the focus, Enter makes the test.
    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if key == "escape" {
            self.close(window, cx);
            cx.stop_propagation();
            return;
        }
        if key == "enter" && !self.busy && !self.loading {
            self.create(window, cx);
            cx.stop_propagation();
            return;
        }
        if modifiers.alt || modifiers.control || modifiers.platform {
            return;
        }
        match key {
            "up" => {
                self.active = self.next_row(-1);
                cx.notify();
                cx.stop_propagation();
                return;
            }
            "down" | "tab" => {
                let step = if modifiers.shift { -1 } else { 1 };
                self.active = self.next_row(step);
                cx.notify();
                cx.stop_propagation();
                return;
            }
            "space" if self.field_row().is_none() => {
                self.activate(cx);
                cx.stop_propagation();
                return;
            }
            _ => {}
        }
        let typed = match self.field_row() {
            Some(Row::ClassName) => self.class_name.key_with_clipboard(event, false, cx),
            Some(Row::Superclass) => self.superclass.key_with_clipboard(event, false, cx),
            Some(Row::Package) => self.package.key_with_clipboard(event, false, cx),
            _ => false,
        };
        if typed {
            self.error = None;
            cx.notify();
            cx.stop_propagation();
        }
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let Some(form) = &self.form else {
            return;
        };
        let class_name = self.class_name.text.trim().to_owned();
        if let Err(error) = check_name(&class_name) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        if class_name.contains(['/', '\\', '.']) {
            self.error = Some("A class name has no slashes or dots".to_owned());
            cx.notify();
            return;
        }
        let Some(framework) = form.frameworks.get(form.framework) else {
            self.error = Some("The chosen test library is not known".to_owned());
            cx.notify();
            return;
        };
        if !framework.available {
            self.error = Some(match &framework.note {
                Some(note) => note.clone(),
                None => format!("{} is not in the module", framework.name),
            });
            cx.notify();
            return;
        }
        let Some(folder) = form.folders.get(form.folder) else {
            self.error = Some("The destination folder is not known".to_owned());
            cx.notify();
            return;
        };
        let methods: Vec<String> = form
            .methods
            .iter()
            .filter(|method| method.checked && (self.show_inherited || !method.inherited))
            .map(|method| method.handle.clone())
            .collect();
        let request = TestCreateRequest {
            spot: self.spot.clone(),
            framework: framework.handle.clone(),
            class_name,
            superclass: self.superclass.text.trim().to_owned(),
            package: self.package.text.trim().to_owned(),
            folder: folder.path.clone(),
            setup: form.setup,
            teardown: form.teardown,
            methods,
        };
        let Some(provider) = cx
            .try_global::<Semantic>()
            .and_then(|semantic| semantic.0.create_test())
        else {
            self.error = Some(NO_SUPPORT.to_owned());
            cx.notify();
            return;
        };
        let mut replies = ask(provider, request);
        self.busy = true;
        self.error = None;
        self.status = Some("Making the test class".to_owned());
        cx.notify();
        let Some(handle) = window.window_handle().downcast::<Workspace>() else {
            self.busy = false;
            self.status = None;
            cx.notify();
            return;
        };
        cx.spawn(async move |this, cx| {
            let answer = replies.next().await;
            this.update(cx, |this, cx| this.created(answer, handle, cx))
                .ok();
        })
        .detach();
    }

    fn created(
        &mut self,
        answer: Option<Answer<TestCreateOutcome>>,
        handle: WindowHandle<Workspace>,
        cx: &mut Context<Self>,
    ) {
        self.busy = false;
        self.status = None;
        let Some(answer) = answer else {
            self.error = Some(NO_ANSWER.to_owned());
            cx.notify();
            return;
        };
        if answer.confidence == Confidence::Unknown {
            self.error = Some(NO_ANSWER.to_owned());
            cx.notify();
            return;
        }
        if let Some(error) = answer.value.error {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let Some(file) = answer.value.file else {
            self.error = Some(NO_FILE.to_owned());
            cx.notify();
            return;
        };
        if file.text.is_some() {
            self.write_and_open(file, handle, cx);
        } else {
            self.ask_to_open(file, handle, cx);
        }
    }

    /// Writes the new file off the window thread, then shows it with the caret in the
    /// first generated method.
    fn write_and_open(
        &mut self,
        file: TestFile,
        handle: WindowHandle<Workspace>,
        cx: &mut Context<Self>,
    ) {
        let Some(text) = file.text else {
            return;
        };
        let path = file.path.clone();
        let caret = file.caret;
        self.busy = true;
        self.status = Some("Writing the test class".to_owned());
        cx.notify();
        let target = path.clone();
        let task = cx.background_executor().spawn(async move {
            if let Some(parent) = target.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let written = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&target)
                .and_then(|mut out| std::io::Write::write_all(&mut out, text.as_bytes()));
            written.is_ok() || target.exists()
        });
        cx.spawn(async move |this, cx| {
            let written = task.await;
            let open = this
                .update(cx, |this, cx| {
                    this.busy = false;
                    this.status = None;
                    cx.notify();
                })
                .is_ok();
            if !open {
                return;
            }
            handle
                .update(cx, |workspace, window, cx| {
                    workspace.dismiss(window, cx);
                    if !written {
                        workspace.notify_user(
                            "Create Test",
                            "The test file could not be written",
                            cx,
                        );
                        return;
                    }
                    workspace.open_file(path.clone(), window, cx);
                    if let Some(point) = caret
                        && let Some(view) = workspace
                            .view_of(&path)
                            .and_then(|view| view.downcast::<EditorView>().ok())
                    {
                        view.update(cx, |view, cx| {
                            view.restore_caret(point.line as usize, point.character as usize, cx)
                        });
                    }
                })
                .ok();
        })
        .detach();
    }

    /// The file is already there: the window asks whether to open it.
    fn ask_to_open(
        &mut self,
        file: TestFile,
        handle: WindowHandle<Workspace>,
        cx: &mut Context<Self>,
    ) {
        let path = file.path;
        let question = format!(
            "{} already exists. Open it?",
            path.file_name().unwrap_or_default().to_string_lossy()
        );
        handle
            .update(cx, |workspace, window, cx| {
                workspace.dismiss(window, cx);
                let open = handle;
                let target = path.clone();
                Prompt::ask(
                    "Create Test",
                    &question,
                    move |cx| {
                        let (open, target) = (open, target.clone());
                        cx.defer(move |cx| {
                            open.update(cx, |workspace, window, cx| {
                                workspace.open_file(target.clone(), window, cx);
                            })
                            .ok();
                        });
                    },
                    workspace,
                    window,
                    cx,
                );
            })
            .ok();
    }

    /// The Fix button: the language adds the missing library to the build file, and the
    /// form marks the library as available.
    fn fix_library(&mut self, handle: WindowHandle<Workspace>, cx: &mut Context<Self>) {
        if self.busy || self.loading {
            return;
        }
        let Some(form) = &self.form else {
            return;
        };
        let Some(framework) = form.frameworks.get(form.framework) else {
            return;
        };
        if framework.available {
            return;
        }
        let framework = framework.handle.clone();
        let Some(provider) = cx
            .try_global::<Semantic>()
            .and_then(|semantic| semantic.0.test_framework_fix())
        else {
            self.error = Some("This language cannot add a library".to_owned());
            cx.notify();
            return;
        };
        let mut replies = ask(
            provider,
            TestFixRequest {
                spot: self.spot.clone(),
                framework,
            },
        );
        self.busy = true;
        self.error = None;
        self.status = Some("Adding the library".to_owned());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let answer = replies.next().await;
            this.update(cx, |this, cx| {
                this.busy = false;
                this.status = None;
                match answer {
                    Some(answer) if answer.value.error.is_none() => {
                        let change = answer.value.change;
                        if !this.mark_fixed(cx) {
                            return;
                        }
                        handle
                            .update(cx, |workspace, window, cx| {
                                crate::changes::apply(workspace, change, window, cx);
                            })
                            .ok();
                    }
                    Some(answer) => {
                        this.error = answer.value.error;
                        cx.notify();
                    }
                    None => {
                        this.error = Some(NO_ANSWER.to_owned());
                        cx.notify();
                    }
                }
            })
            .ok();
        })
        .detach();
    }

    /// Clears the warning of the library the user just added.
    fn mark_fixed(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(form) = &mut self.form else {
            return false;
        };
        let index = form.framework;
        let Some(framework) = form.frameworks.get_mut(index) else {
            return false;
        };
        framework.available = true;
        framework.note = None;
        self.error = None;
        self.status = Some(format!("{} is in the module now", framework.name));
        cx.notify();
        true
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }
}

impl Render for CreateTestDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.10;
        let active = self.rows.get(self.active).copied();

        let mut rows = Vec::new();
        if let Some(form) = &self.form {
            for (index, framework) in form.frameworks.iter().enumerate() {
                let checked = index == form.framework;
                rows.push(
                    choice_row(
                        ("create-test-library", index),
                        format!("create-test-library-{index}"),
                        checked,
                        active == Some(Row::Framework(index)),
                        &theme,
                    )
                    .child(div().child(framework.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(form) = &mut this.form {
                            form.framework = index;
                        }
                        this.active = this
                            .rows
                            .iter()
                            .position(|row| *row == Row::Framework(index))
                            .unwrap_or(0);
                        this.error = None;
                        cx.notify();
                    }))
                    .into_any_element(),
                );
            }
            rows.push(self.text_row(Row::ClassName, "Class name", &theme, cx));
            rows.push(self.text_row(Row::Superclass, "Superclass", &theme, cx));
            rows.push(self.text_row(Row::Package, "Destination package", &theme, cx));
            for (index, folder) in form.folders.iter().enumerate() {
                let checked = index == form.folder;
                rows.push(
                    choice_row(
                        ("create-test-folder", index),
                        format!("create-test-folder-{index}"),
                        checked,
                        active == Some(Row::Folder(index)),
                        &theme,
                    )
                    .child(div().child(folder.name.clone()))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(form) = &mut this.form {
                            form.folder = index;
                        }
                        this.active = this
                            .rows
                            .iter()
                            .position(|row| *row == Row::Folder(index))
                            .unwrap_or(0);
                        cx.notify();
                    }))
                    .into_any_element(),
                );
            }
            let setup = form.setup;
            rows.push(self.check_row("create-test-setup", "setUp", setup, Row::Setup, &theme, cx));
            let teardown = form.teardown;
            rows.push(self.check_row(
                "create-test-teardown",
                "tearDown",
                teardown,
                Row::Teardown,
                &theme,
                cx,
            ));
            let inherited = self.show_inherited;
            rows.push(self.check_row(
                "create-test-inherited",
                "Show inherited methods",
                inherited,
                Row::ShowInherited,
                &theme,
                cx,
            ));
            for (index, method) in form.methods.iter().enumerate() {
                if method.inherited && !self.show_inherited {
                    continue;
                }
                let checked = method.checked;
                let detail = method.detail.clone().unwrap_or_default();
                rows.push(
                    choice_row(
                        ("create-test-method", index),
                        format!("create-test-method-{index}"),
                        checked,
                        active == Some(Row::Method(index)),
                        &theme,
                    )
                    .child(div().child(method.name.clone()))
                    .child(div().text_color(theme.text_muted).child(detail))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(form) = &mut this.form
                            && let Some(method) = form.methods.get_mut(index)
                        {
                            method.checked = !method.checked;
                        }
                        this.active = this
                            .rows
                            .iter()
                            .position(|row| *row == Row::Method(index))
                            .unwrap_or(0);
                        this.error = None;
                        cx.notify();
                    }))
                    .into_any_element(),
                );
            }
        }

        let warning = self.warning(cx);
        let note = if let Some(error) = &self.error {
            Some(
                div()
                    .debug_selector(|| "create-test-error".to_owned())
                    .text_color(theme.text)
                    .child(error.clone())
                    .into_any_element(),
            )
        } else if let Some((text, fixable)) = warning {
            Some(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .debug_selector(|| "create-test-warning".to_owned())
                            .text_color(theme.text)
                            .child(text),
                    )
                    .children(fixable.then(|| {
                        dialog_button(
                            "create-test-fix",
                            "create-test-fix".to_owned(),
                            "Fix",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            let Some(handle) = window.window_handle().downcast::<Workspace>()
                            else {
                                return;
                            };
                            this.fix_library(handle, cx);
                        }))
                        .into_any_element()
                    }))
                    .into_any_element(),
            )
        } else if self.loading {
            Some(
                div()
                    .debug_selector(|| "create-test-loading".to_owned())
                    .text_color(theme.text_muted)
                    .child("Reading the class")
                    .into_any_element(),
            )
        } else {
            self.status.as_ref().map(|status| {
                div()
                    .debug_selector(|| "create-test-status".to_owned())
                    .text_color(theme.text_muted)
                    .child(status.clone())
                    .into_any_element()
            })
        };

        let shell = surface(&theme)
            .id("create-test")
            .debug_selector(|| "create-test".to_owned())
            .key_context("CreateTest")
            .track_focus(&self.focus)
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Testing library"),
            )
            .child(scrollable(
                "create-test-rows",
                &self.scroll,
                div()
                    .id("create-test-rows")
                    .flex()
                    .flex_col()
                    .gap(px(DIALOG_GAP))
                    .max_h(viewport.height * 0.5)
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .children(rows),
                &theme,
                window,
                cx,
            ))
            .children(note)
            .child(
                DialogButtons::new()
                    .primary(
                        dialog_button(
                            "create-test-ok",
                            "create-test-ok".to_owned(),
                            "OK",
                            true,
                            !self.busy && !self.loading,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    )
                    .other(
                        dialog_button(
                            "create-test-cancel",
                            "create-test-cancel".to_owned(),
                            "Cancel",
                            false,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                    )
                    .into_row(),
            );

        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "create-test-dialog",
                title: "Create Test".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: None,
                },
                min: (400., 280.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

impl CreateTestDialog {
    /// The warning of the chosen library, and whether Fix can remove it.
    fn warning(&self, cx: &App) -> Option<(String, bool)> {
        let form = self.form.as_ref()?;
        let framework = form.frameworks.get(form.framework)?;
        if framework.available {
            return None;
        }
        let fixable = form.can_fix
            && cx
                .try_global::<Semantic>()
                .is_some_and(|semantic| semantic.0.test_framework_fix().is_some());
        Some((
            framework
                .note
                .clone()
                .unwrap_or_else(|| format!("{} is not in the module", framework.name)),
            fixable,
        ))
    }

    fn text_row(&self, row: Row, label: &str, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let field = match row {
            Row::ClassName => &self.class_name,
            Row::Superclass => &self.superclass,
            Row::Package => &self.package,
            _ => return div().into_any_element(),
        };
        let active = self.rows.get(self.active).copied() == Some(row);
        let value = div()
            .id(field_selector(row))
            .debug_selector({
                let selector = field_selector(row);
                move || selector.to_owned()
            })
            .flex()
            .items_center()
            .h(px(FIELD_HEIGHT))
            .flex_1()
            .px(px(SPACE_M))
            .rounded(px(CONTROL_RADIUS))
            .border_1()
            .border_color(if active {
                theme.accent
            } else {
                theme.control_border
            })
            .text_color(theme.text)
            .children(text_field_children_named(
                "create-test-text",
                field,
                theme.text,
                theme.text,
                theme.selection,
                theme.caret,
                18.,
                active,
            ));
        div()
            .flex()
            .items_center()
            .gap(px(SPACE_M))
            .child(
                div()
                    .w(px(140.))
                    .flex_none()
                    .text_color(theme.text_muted)
                    .child(label.to_owned()),
            )
            .child(value.on_click(cx.listener(move |this, _, window, cx| {
                this.active = this
                    .rows
                    .iter()
                    .position(|found| *found == row)
                    .unwrap_or(0);
                window.focus(&this.focus, cx);
                cx.notify();
            })))
            .into_any_element()
    }

    fn check_row(
        &self,
        selector: &'static str,
        label: &str,
        checked: bool,
        row: Row,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let active = self.rows.get(self.active).copied() == Some(row);
        choice_row(selector, selector.to_owned(), checked, active, theme)
            .child(div().child(label.to_owned()))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.active = this
                    .rows
                    .iter()
                    .position(|found| *found == row)
                    .unwrap_or(0);
                this.activate(cx);
            }))
            .into_any_element()
    }
}

fn field_selector(row: Row) -> &'static str {
    match row {
        Row::ClassName => "create-test-class-name",
        Row::Superclass => "create-test-superclass",
        Row::Package => "create-test-package",
        _ => "create-test-field",
    }
}

fn choice_row(
    id: impl Into<ElementId>,
    selector: String,
    checked: bool,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .gap(px(SPACE_M))
        .h(px(FIELD_HEIGHT))
        .px(px(SPACE_S))
        .rounded(px(CONTROL_RADIUS))
        .text_color(theme.text)
        .when(active, |row| row.bg(theme.selection))
        .child(
            div()
                .flex_none()
                .flex()
                .items_center()
                .justify_center()
                .w(px(14.))
                .h(px(14.))
                .rounded(px(3.))
                .border_1()
                .border_color(theme.control_border)
                .when(checked, |box_| box_.bg(theme.accent))
                .child(
                    div()
                        .text_size(px(10.))
                        .text_color(theme.over_accent)
                        .child(if checked { "✓" } else { "" }),
                ),
        )
}

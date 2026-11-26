//! The window without a project: recent projects, Open, New Project from a template and
//! Get from VCS. A project opens in a window of its own, a program of its own.

use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock};

use ::base::Cancel;

use project_model::scaffold::{self, Generator};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{OpenRequests, Request, StartContext, spawn_path_taking_over};

use crate::new_project::{NewProject, Plan, Step};
use crate::prompt::TextField;
use crate::tree_ops::check_name;

/// The generators of the wizard, made when the form opens: the application supplies
/// them, because a neutral crate knows no language and no build tool.
pub type Generators = Arc<dyn Fn() -> Vec<Arc<dyn Generator>> + Send + Sync>;

static GENERATORS: OnceLock<Generators> = OnceLock::new();

/// Registers the generators the application offers in New Project.
pub fn set_generators(generators: Generators) {
    let _ = GENERATORS.set(generators);
}

fn registered_generators() -> Vec<Arc<dyn Generator>> {
    GENERATORS.get().map(|make| make()).unwrap_or_default()
}

/// A kind of new project. The application gives the list: neutral crates know no build tool.
#[derive(Clone)]
pub struct ProjectTemplate {
    pub label: &'static str,
    /// The files of a project of that name, as paths inside its folder and their text.
    pub files: fn(&str) -> Vec<(String, String)>,
}

/// The folder a clone goes into: the last part of the address without `.git`.
pub fn clone_folder_name(url: &str) -> Option<String> {
    let url = url.trim().trim_end_matches(['/', '\\']);
    let last = url.rsplit(['/', '\\', ':']).next()?;
    let name = last.strip_suffix(".git").unwrap_or(last);
    check_name(name).ok().map(|()| name.to_string())
}

/// Writes the files of a new project into `folder`, which must not exist. Blocks.
pub fn write_project(folder: &Path, files: &[(String, String)]) -> std::io::Result<()> {
    if folder.exists() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            format!("{} already exists", folder.display()),
        ));
    }
    for (relative, text) in files {
        let path = folder.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
    }
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Home,
    New,
    Clone,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Active {
    Name,
    Location,
    Url,
}

pub struct StartScreen {
    recent: Vec<PathBuf>,
    state_dir: PathBuf,
    templates: Vec<ProjectTemplate>,
    generators: Vec<Arc<dyn Generator>>,
    pub(crate) wizard: NewProject,
    mode: Mode,
    template: usize,
    active: Active,
    pub(crate) name: TextField,
    pub(crate) location: TextField,
    url: TextField,
    message: Option<SharedString>,
    busy: bool,
    /// A window opened only for the wizard closes instead of showing the start screen.
    standalone: bool,
    focus: FocusHandle,
    /// The look of the system while Sync with OS is on.
    appearance: Option<Subscription>,
    /// Tests create projects without starting a program of their own.
    #[cfg(test)]
    no_spawn: bool,
}

/// Opens the start window and runs until it closes. `requests` brings the paths the
/// system opens and the requests of a second start: a path becomes a project of its own.
pub fn run(
    context: StartContext,
    state_dir: PathBuf,
    templates: Vec<ProjectTemplate>,
    generators: Vec<Arc<dyn Generator>>,
    background: bool,
    requests: OpenRequests,
) {
    let opened = requests.clone();
    let application = ui_kit::app();
    application.on_open_urls(move |urls| {
        for url in urls {
            if let Some(path) = ::platform::file_url_path(&url) {
                opened.push(Request::Open(path));
            }
        }
    });
    application.run(move |cx| {
        ui_kit::init(context.dark, cx);
        let ui_scale = context.settings.ui_scale;
        cx.set_global(context.themes.clone());
        cx.set_global(context.theme);
        cx.set_global(context.settings);
        ui_kit::set_ui_scale(ui_scale);
        // The size on screen stays the same as the interface grows.
        let scale = ui_kit::window_scale(ui_scale);
        let bounds = Bounds::centered(None, size(px(900. / scale), px(560. / scale)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: !background,
            app_id: Some("arro".into()),
            ..Default::default()
        };
        let recent = context.recent;
        let window = cx
            .open_window(options, move |window, cx| {
                window.set_window_title("Welcome to Arro");
                cx.new(|cx| {
                    let focus = cx.focus_handle();
                    window.focus(&focus, cx);
                    let mut screen = StartScreen::new(
                        recent,
                        state_dir,
                        templates,
                        generators,
                        focus,
                        Mode::Home,
                    );
                    screen.watch_appearance(window, cx);
                    screen
                })
            })
            .expect("open window");
        // The listener of the platform cannot touch the window, so the queue wakes the
        // screen and it hands the paths over until the first one closes it.
        cx.spawn(async move |cx| {
            while requests.wait_for_push().await {
                for request in requests.take() {
                    let opened = match request {
                        Request::Focus => {
                            cx.update(|cx| cx.activate(true));
                            Ok(())
                        }
                        Request::Open(path) => {
                            window.update(cx, |screen, window, cx| screen.open(path, window, cx))
                        }
                    };
                    if opened.is_err() {
                        return;
                    }
                }
            }
        })
        .detach();
    });
}

impl StartScreen {
    fn new(
        recent: Vec<PathBuf>,
        state_dir: PathBuf,
        templates: Vec<ProjectTemplate>,
        generators: Vec<Arc<dyn Generator>>,
        focus: FocusHandle,
        mode: Mode,
    ) -> Self {
        let home = ::base::program::home_dir().unwrap_or_else(std::env::temp_dir);
        let projects = home.join("ArroProjects");
        Self {
            recent,
            state_dir,
            templates,
            wizard: NewProject::new(generators.clone(), projects.clone()),
            generators,
            mode,
            template: 0,
            active: Active::Name,
            name: TextField::default(),
            location: TextField::from_text(projects.display().to_string()),
            url: TextField::default(),
            message: None,
            busy: false,
            standalone: false,
            focus,
            appearance: None,
            #[cfg(test)]
            no_spawn: false,
        }
    }

    /// Takes the theme the settings give the system, and keeps following the system while
    /// Sync with OS is on.
    fn watch_appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        workspace::follow_system_theme(window, cx);
        self.appearance = Some(cx.observe_window_appearance(window, |_, window, cx| {
            workspace::follow_system_theme(window, cx);
        }));
    }

    /// Builds the screen in a window of the test platform, without starting a project.
    #[cfg(test)]
    pub fn new_for_test(
        recent: Vec<PathBuf>,
        state_dir: PathBuf,
        templates: Vec<ProjectTemplate>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        workspace::follow_system_theme(window, cx);
        Self::new(recent, state_dir, templates, Vec::new(), focus, Mode::Home)
    }

    /// Hands the wizard the generators of the application, for a test of the form.
    #[cfg(test)]
    pub fn set_generators_for_test(&mut self, generators: Vec<Arc<dyn Generator>>) {
        self.generators = generators.clone();
        let location = PathBuf::from(self.location.text.trim());
        self.wizard = NewProject::new(generators, location);
    }

    /// Fills the fields of the wizard, for tests without a keyboard.
    #[cfg(test)]
    pub fn set_new_project_fields_for_test(&mut self, name: &str, location: &str) {
        self.wizard.set_fields_for_test(name, location);
    }

    /// The wizard of the form, for a test.
    #[cfg(test)]
    pub fn wizard_for_test(&self) -> &NewProject {
        &self.wizard
    }

    /// The line under the form, for a test without pixels.
    #[cfg(test)]
    pub fn message_for_test(&self) -> Option<String> {
        self.message.as_ref().map(|text| text.to_string())
    }

    /// The mode the screen shows, for tests.
    #[cfg(test)]
    pub fn mode_name(&self) -> &'static str {
        match self.mode {
            Mode::Home => "home",
            Mode::New => "new",
            Mode::Clone => "clone",
        }
    }

    /// Fills the fields of the screen, for tests without a keyboard.
    #[cfg(test)]
    pub fn set_fields_for_test(&mut self, name: &str, location: &str, url: &str) {
        self.name.text = name.to_owned();
        self.location.text = location.to_owned();
        self.url.text = url.to_owned();
    }

    /// Lets a test see the folder that would be opened instead of starting a program.
    #[cfg(test)]
    pub fn keep_window_for_test(&mut self) {
        self.no_spawn = true;
    }

    fn field(&mut self) -> &mut TextField {
        match self.active {
            Active::Name => &mut self.name,
            Active::Location => &mut self.location,
            Active::Url => &mut self.url,
        }
    }

    fn show(&mut self, mode: Mode, window: &mut Window, cx: &mut Context<Self>) {
        self.mode = mode;
        self.message = None;
        self.active = if mode == Mode::Clone {
            Active::Url
        } else {
            Active::Name
        };
        window.focus(&self.focus, cx);
        cx.notify();
    }

    fn fail(&mut self, text: String, cx: &mut Context<Self>) {
        self.busy = false;
        self.message = Some(text.into());
        cx.notify();
    }

    /// A project that Arro created or cloned at the request of the user is trusted: its
    /// code came from the user, not from a folder that the user only opened.
    fn remember_trust(&self, folder: &Path) {
        if let Err(error) = store::TrustList::new(&self.state_dir).trust(folder) {
            eprintln!("the project {} is not trusted: {error}", folder.display());
        }
    }

    /// Starts the program of the project and leaves this window.
    fn open(&mut self, folder: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        #[cfg(test)]
        if self.no_spawn {
            self.busy = false;
            self.message = Some(format!("Would open {}", folder.display()).into());
            cx.notify();
            return;
        }
        let state_dir = self.state_dir.clone();
        let start = cx
            .background_executor()
            .spawn(async move { spawn_path_taking_over(&state_dir, &folder) });
        cx.spawn_in(window, async move |this, cx| match start.await {
            Ok(()) => {
                this.update_in(cx, |_, window, _| window.remove_window())
                    .ok();
            }
            Err(error) => {
                let text = format!("Cannot open the project: {error}");
                this.update(cx, |this, cx| this.fail(text, cx)).ok();
            }
        })
        .detach();
    }

    fn choose_folder(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: None,
        });
        cx.spawn_in(window, async move |this, cx| {
            let chosen = paths.await.ok().and_then(Result::ok).flatten();
            if let Some(folder) = chosen.and_then(|paths| paths.into_iter().next()) {
                this.update_in(cx, |this, window, cx| this.open(folder, window, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn create(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.text.trim().to_string();
        if let Err(error) = check_name(&name) {
            return self.fail(error, cx);
        }
        let Some(template) = self.templates.get(self.template).cloned() else {
            return self.fail("There is no template".into(), cx);
        };
        let folder = PathBuf::from(self.location.text.trim()).join(&name);
        let files = (template.files)(&name);
        self.busy = true;
        self.message = Some("Creating...".into());
        let target = folder.clone();
        let write = cx
            .background_executor()
            .spawn(async move { write_project(&target, &files) });
        cx.spawn_in(window, async move |this, cx| match write.await {
            Ok(()) => {
                this.update_in(cx, |this, window, cx| {
                    this.remember_trust(&folder);
                    this.open(folder, window, cx)
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| this.fail(error.to_string(), cx))
                    .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn clone_repository(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let url = self.url.text.trim().to_string();
        let Some(name) = clone_folder_name(&url) else {
            return self.fail("Give the address of a repository".into(), cx);
        };
        let parent = PathBuf::from(self.location.text.trim());
        let folder = parent.join(&name);
        if folder.exists() {
            return self.fail(format!("{} already exists", folder.display()), cx);
        }
        self.busy = true;
        self.message = Some("Cloning...".into());
        // A password in the address would reach the command line of git and the configuration
        // of the clone, which keeps the address. Git asks for the credentials through its own
        // credential helper, so the address goes without them.
        let remote = vcs::url_without_credentials(&url);
        let task = cx.background_executor().spawn(async move {
            std::fs::create_dir_all(&parent)?;
            let git = vcs::GitRunner::from_env()
                .ok_or_else(|| std::io::Error::other("git is not found"))?;
            git.run(
                &parent,
                ["clone", "--end-of-options", remote.as_str(), name.as_str()],
            )
            .map(|_| ())
        });
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(()) => {
                this.update_in(cx, |this, window, cx| {
                    this.remember_trust(&folder);
                    this.open(folder, window, cx)
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    this.fail(format!("Clone failed: {error}"), cx)
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.mode == Mode::Home || self.busy {
            return;
        }
        let key = event.keystroke.key.as_str();
        if self.mode == Mode::New && !self.generators.is_empty() {
            if key == "escape" {
                if self.standalone {
                    window.remove_window();
                } else {
                    self.show(Mode::Home, window, cx);
                }
                cx.stop_propagation();
                return;
            }
            match self.wizard.key(event, cx) {
                Step::None => return,
                step => {
                    cx.stop_propagation();
                    self.run_wizard_step(step, window, cx);
                }
            }
            return;
        }
        if key == "tab" {
            self.active = match (self.mode, self.active) {
                (Mode::New, Active::Name) | (Mode::Clone, Active::Url) => Active::Location,
                (Mode::New, _) => Active::Name,
                _ => Active::Url,
            };
        } else if key == "enter" {
            match self.mode {
                Mode::New => self.create(window, cx),
                _ => self.clone_repository(window, cx),
            }
        } else if key == "escape" {
            self.show(Mode::Home, window, cx);
        } else if !self.field().key_with_clipboard(event, false, cx) {
            return;
        }
        cx.stop_propagation();
        cx.notify();
    }

    /// Runs what the wizard asked for: a frame, the metadata of a server, or the project.
    pub(crate) fn run_wizard_step(
        &mut self,
        step: Step,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match step {
            Step::None => {}
            Step::Changed => {
                self.message = None;
                cx.notify();
            }
            Step::Load => self.load_generator(window, cx),
            Step::Create(plan) => self.create_planned(plan, window, cx),
        }
    }

    /// Asks the generator for the choices that come from outside, away from this thread.
    fn load_generator(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let generation = self.wizard.begin_load();
        let values = self.wizard.values_for_load();
        let Some(generator) = self.generators.get(self.wizard.generator_index()) else {
            return;
        };
        let generator = Arc::clone(generator);
        let answer = cx
            .background_executor()
            .spawn(async move { generator.load(&values, &Cancel::new()) });
        cx.spawn_in(window, async move |this, cx| {
            let answer = answer.await;
            this.update(cx, |this, cx| {
                this.wizard.loaded(generation, answer);
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Writes the project away from this thread and opens it in a window of its own.
    fn create_planned(&mut self, plan: Plan, window: &mut Window, cx: &mut Context<Self>) {
        let Plan { generator, request } = plan;
        let folder = request.folder.clone();
        let opened = folder.clone();
        let git = request.git;
        self.busy = true;
        self.message = Some("Creating...".into());
        let write = cx.background_executor().spawn(async move {
            let cancel = Cancel::new();
            scaffold::prepare(&folder)?;
            let created = generator.create(&request, &cancel)?;
            scaffold::apply_created(&folder, &created)?;
            let mut note = match generator.finish(&request, &folder, &cancel) {
                Ok(note) => note.or(created.note.clone()),
                Err(error) => Some(error),
            };
            if git {
                let started = vcs::GitRunner::from_env()
                    .is_some_and(|git| git.run(&folder, ["init"]).is_ok());
                if !started {
                    note = Some(match note {
                        Some(note) => format!("{note} Git did not start in the folder."),
                        None => "The project is created, but Git did not start in the folder."
                            .to_owned(),
                    });
                }
            }
            Ok::<_, String>(note.unwrap_or_default())
        });
        cx.spawn_in(window, async move |this, cx| match write.await {
            Ok(note) => {
                this.update(cx, |this, _cx| {
                    if !note.is_empty() {
                        this.message = Some(note.into());
                    }
                })
                .ok();
                this.update_in(cx, |this, window, cx| {
                    this.remember_trust(&opened);
                    this.open(opened, window, cx)
                })
                .ok();
            }
            Err(error) => {
                this.update(cx, |this, cx| {
                    this.fail(format!("The project is not created: {error}"), cx)
                })
                .ok();
            }
        })
        .detach();
        cx.notify();
    }
}

/// Opens the New Project wizard in a window of its own, as File > New > Project does.
pub fn open_window(state_dir: PathBuf, cx: &mut App) {
    let generators = registered_generators();
    let bounds = Bounds::centered(None, size(px(900.), px(560.)), cx);
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(bounds)),
        ..Default::default()
    };
    cx.open_window(options, move |window, cx| {
        window.set_window_title("New Project");
        cx.new(|cx| {
            let focus = cx.focus_handle();
            window.focus(&focus, cx);
            let mut screen = StartScreen::new(
                Vec::new(),
                state_dir,
                Vec::new(),
                generators.clone(),
                focus,
                Mode::New,
            );
            screen.standalone = true;
            screen.watch_appearance(window, cx);
            screen
        })
    })
    .ok();
}

fn button(id: &'static str, label: &'static str, theme: &Theme) -> Stateful<Div> {
    let hover = theme.hover;
    div()
        .id(id)
        .debug_selector(move || id.to_owned())
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_L))
        .rounded(px(CONTROL_RADIUS))
        .bg(theme.pressed)
        .hover(move |style| style.bg(hover))
        .child(label)
}

fn text_field(
    mouse: FieldMouse,
    label: &'static str,
    field: &TextField,
    active: bool,
    theme: &Theme,
) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_S))
        .child(div().text_color(theme.text_secondary).child(label))
        .child(
            div()
                .flex()
                .items_center()
                .h(px(32.))
                .px(px(SPACE_M))
                .rounded(px(CONTROL_RADIUS))
                .border_2()
                .border_color(if active { theme.accent } else { theme.border })
                .children(text_field_children_named(
                    mouse,
                    "start-field",
                    field,
                    theme.text,
                    theme.text,
                    theme.selection,
                    theme.caret,
                    18.,
                    active,
                )),
        )
}

impl Render for StartScreen {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let actions = div()
            .flex()
            .flex_col()
            .gap(px(SPACE_M))
            .w(px(220.))
            .child(
                button("start-open", "Open...", &theme)
                    .on_click(cx.listener(|this, _, window, cx| this.choose_folder(window, cx))),
            )
            .child(
                button("start-new", "New Project...", &theme)
                    .on_click(cx.listener(|this, _, window, cx| this.show(Mode::New, window, cx))),
            )
            .child(
                button("start-clone", "Get from VCS...", &theme).on_click(
                    cx.listener(|this, _, window, cx| this.show(Mode::Clone, window, cx)),
                ),
            );

        let side: AnyElement = match self.mode {
            Mode::Home => {
                let rows = self.recent.iter().enumerate().map(|(index, project)| {
                    let name = project.file_name().unwrap_or_default().to_string_lossy();
                    let folder = project.clone();
                    div()
                        .id(("recent", index))
                        .flex()
                        .gap(px(SPACE_M))
                        .h(px(ROW_HEIGHT))
                        .items_center()
                        .px(px(ROW_PADDING_X))
                        .rounded(px(ROW_RADIUS))
                        .hover(|style| style.bg(theme.hover))
                        .child(div().child(name.into_owned()))
                        .child(
                            div()
                                .text_color(theme.text_secondary)
                                .child(project.display().to_string()),
                        )
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.open(folder.clone(), window, cx)
                        }))
                });
                let empty = self.recent.is_empty().then(|| {
                    div()
                        .text_color(theme.text_secondary)
                        .child("No projects yet.")
                });
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(px(SPACE_S))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .child("Recent Projects"),
                    )
                    .children(rows)
                    .children(empty)
                    .into_any_element()
            }
            Mode::New if !self.generators.is_empty() => self.wizard.render(window, cx),
            Mode::New => {
                let templates = self.templates.iter().enumerate().map(|(index, template)| {
                    let on = index == self.template;
                    div()
                        .id(("template", index))
                        .px(px(SPACE_M))
                        .h(px(CONTROL_HEIGHT))
                        .flex()
                        .items_center()
                        .rounded(px(CONTROL_RADIUS))
                        .when(on, |this| {
                            this.bg(theme.accent).text_color(theme.over_accent)
                        })
                        .when(!on, |this| this.bg(theme.pressed))
                        .child(template.label)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.template = index;
                            cx.notify();
                        }))
                });
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(px(DIALOG_GAP))
                    .child(div().font_weight(FontWeight::MEDIUM).child("New Project"))
                    .child(div().flex().gap(px(SPACE_M)).children(templates))
                    .child(text_field(
                        FieldMouse::new(cx, |this| Some(&mut this.name)),
                        "Name",
                        &self.name,
                        self.active == Active::Name,
                        &theme,
                    ))
                    .child(text_field(
                        FieldMouse::new(cx, |this| Some(&mut this.location)),
                        "Location",
                        &self.location,
                        self.active == Active::Location,
                        &theme,
                    ))
                    .child(
                        button("start-create", "Create", &theme)
                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    )
                    .into_any_element()
            }
            Mode::Clone => {
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(px(DIALOG_GAP))
                    .child(div().font_weight(FontWeight::MEDIUM).child("Get from VCS"))
                    .child(text_field(
                        FieldMouse::new(cx, |this| Some(&mut this.url)),
                        "URL",
                        &self.url,
                        self.active == Active::Url,
                        &theme,
                    ))
                    .child(text_field(
                        FieldMouse::new(cx, |this| Some(&mut this.location)),
                        "Directory",
                        &self.location,
                        self.active == Active::Location,
                        &theme,
                    ))
                    .child(button("start-do-clone", "Clone", &theme).on_click(
                        cx.listener(|this, _, window, cx| this.clone_repository(window, cx)),
                    ))
                    .into_any_element()
            }
        };
        let message = self
            .message
            .clone()
            .map(|text| div().text_color(theme.text_secondary).child(text));

        div()
            .id("start-screen")
            .key_context("StartScreen")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .size_full()
            .flex()
            .flex_col()
            .gap(px(DIALOG_GAP))
            .p(px(DIALOG_PADDING))
            .bg(theme.main_bg)
            .text_color(theme.text)
            .font_family(UI_FONT)
            .text_size(px(UI_SIZE))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(DIALOG_GAP))
                    .child(
                        img("brand/arro.svg")
                            .size(px(48.))
                            .flex_none()
                            .debug_selector(|| "start-logo".to_owned()),
                    )
                    .child(
                        div()
                            .text_size(px(22.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("Welcome to Arro"),
                    ),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(SPACE_XL))
                    .child(actions)
                    .child(side),
            )
            .children(message)
    }
}

#[cfg(test)]
mod tests {
    use super::{clone_folder_name, write_project};

    #[test]
    fn a_clone_goes_into_the_last_part_of_the_address() {
        let name = |url: &str| clone_folder_name(url);
        assert_eq!(name("https://host/user/shop.git"), Some("shop".into()));
        assert_eq!(name("https://host/user/shop/"), Some("shop".into()));
        assert_eq!(name("git@host:user/shop.git"), Some("shop".into()));
        assert_eq!(name("C:\\work\\shop"), Some("shop".into()));
        assert_eq!(name("   "), None);
        assert_eq!(name("https://host/user/?"), None);
    }

    #[test]
    fn a_new_project_is_written_with_its_folders_and_never_over_another() {
        let root = test_support::scratch("start-project");
        let folder = root.join("shop");
        let files = vec![
            ("a.txt".to_string(), "one".to_string()),
            ("src/deep/b.txt".to_string(), "two".to_string()),
        ];
        write_project(&folder, &files).unwrap();
        assert_eq!(
            std::fs::read_to_string(folder.join("src/deep/b.txt")).unwrap(),
            "two"
        );
        assert!(write_project(&folder, &files).is_err(), "the folder exists");
    }
}

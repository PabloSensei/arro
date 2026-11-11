//! The window without a project: recent projects, Open, New Project from a template and
//! Get from VCS. A project opens in a window of its own, a program of its own.

use std::path::{Path, PathBuf};

use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{StartContext, spawn_project};

use crate::prompt::TextField;
use crate::tree_ops::check_name;

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
    mode: Mode,
    template: usize,
    active: Active,
    name: TextField,
    location: TextField,
    url: TextField,
    message: Option<SharedString>,
    busy: bool,
    focus: FocusHandle,
    /// Tests create projects without starting a program of their own.
    #[cfg(test)]
    no_spawn: bool,
}

/// Opens the start window and runs until it closes.
pub fn run(
    context: StartContext,
    state_dir: PathBuf,
    templates: Vec<ProjectTemplate>,
    background: bool,
) {
    ui_kit::app().run(move |cx| {
        ui_kit::init(context.dark, cx);
        cx.set_global(context.settings);
        let bounds = Bounds::centered(None, size(px(900.), px(560.)), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            focus: !background,
            ..Default::default()
        };
        let recent = context.recent;
        cx.open_window(options, move |window, cx| {
            window.set_window_title("Welcome to Arro");
            cx.new(|cx| {
                let focus = cx.focus_handle();
                window.focus(&focus, cx);
                StartScreen::new(recent, state_dir, templates, focus)
            })
        })
        .expect("open window");
    });
}

impl StartScreen {
    fn new(
        recent: Vec<PathBuf>,
        state_dir: PathBuf,
        templates: Vec<ProjectTemplate>,
        focus: FocusHandle,
    ) -> Self {
        let home = std::env::var_os("USERPROFILE").map_or_else(std::env::temp_dir, PathBuf::from);
        Self {
            recent,
            state_dir,
            templates,
            mode: Mode::Home,
            template: 0,
            active: Active::Name,
            name: TextField::default(),
            location: TextField::from_text(home.join("ArroProjects").display().to_string()),
            url: TextField::default(),
            message: None,
            busy: false,
            focus,
            #[cfg(test)]
            no_spawn: false,
        }
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
        Self::new(recent, state_dir, templates, focus)
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
            .spawn(async move { spawn_project(&state_dir, &folder) });
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
                this.update_in(cx, |this, window, cx| this.open(folder, window, cx))
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
        let task = cx.background_executor().spawn(async move {
            std::fs::create_dir_all(&parent)?;
            let git = vcs::GitRunner::from_env()
                .ok_or_else(|| std::io::Error::other("git is not found"))?;
            git.run(&parent, ["clone", url.as_str(), name.as_str()])
                .map(|_| ())
        });
        cx.spawn_in(window, async move |this, cx| match task.await {
            Ok(()) => {
                this.update_in(cx, |this, window, cx| this.open(folder, window, cx))
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

fn text_field(label: &'static str, field: &TextField, active: bool, theme: &Theme) -> Div {
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
                .child(field.text.clone()),
        )
}

impl Render for StartScreen {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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

        let side = match self.mode {
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
            }
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
                        "Name",
                        &self.name,
                        self.active == Active::Name,
                        &theme,
                    ))
                    .child(text_field(
                        "Location",
                        &self.location,
                        self.active == Active::Location,
                        &theme,
                    ))
                    .child(
                        button("start-create", "Create", &theme)
                            .on_click(cx.listener(|this, _, window, cx| this.create(window, cx))),
                    )
            }
            Mode::Clone => div()
                .flex()
                .flex_col()
                .flex_1()
                .gap(px(DIALOG_GAP))
                .child(div().font_weight(FontWeight::MEDIUM).child("Get from VCS"))
                .child(text_field(
                    "URL",
                    &self.url,
                    self.active == Active::Url,
                    &theme,
                ))
                .child(text_field(
                    "Directory",
                    &self.location,
                    self.active == Active::Location,
                    &theme,
                ))
                .child(button("start-do-clone", "Clone", &theme).on_click(
                    cx.listener(|this, _, window, cx| this.clone_repository(window, cx)),
                )),
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
            .child(div().flex().gap(px(SPACE_XL)).child(actions).child(side))
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
        let root = std::env::temp_dir().join(format!("start-project-{}", std::process::id()));
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
        std::fs::remove_dir_all(&root).unwrap();
    }
}

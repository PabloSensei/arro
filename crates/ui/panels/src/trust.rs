//! The dialog that asks whether to trust a project. An agent runs commands in the project
//! folder, so the first session of a project asks first; `store::TrustList` keeps the answer.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use futures_channel::mpsc::UnboundedSender;
use futures_util::StreamExt;
use store::TrustList;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

const WIDTH: f32 = 460.;
const OPEN_WIDTH: f32 = 520.;

type OnTrust = Box<dyn FnOnce(&mut Window, &mut App)>;

pub struct TrustDialog {
    project: PathBuf,
    store: TrustList,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    on_trust: Option<OnTrust>,
}

impl TrustDialog {
    /// Shows the dialog; `on_trust` runs once the user trusts the project.
    pub fn show(
        project: PathBuf,
        store: TrustList,
        workspace: &mut Workspace,
        on_trust: impl FnOnce(&mut Window, &mut App) + 'static,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            project,
            store,
            focus: cx.focus_handle(),
            workspace: weak,
            on_trust: Some(Box::new(on_trust)),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn trust(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.store.trust(&self.project) {
            let text = format!("{}: {error}", self.project.display());
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.notify_error("Trust is not saved", text, cx)
                })
                .ok();
        }
        self.close(window, cx);
        if let Some(on_trust) = self.on_trust.take() {
            on_trust(window, cx);
        }
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => self.trust(window, cx),
            "escape" => self.close(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for TrustDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.25;
        let folder = self.project.display().to_string();
        let shell = surface(&theme)
            .id("trust-project")
            .key_context("TrustDialog")
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
            .child(div().text_color(theme.text_muted).child(folder))
            .child(div().text_color(theme.text_secondary).child(
                "An AI agent runs commands and edits files in this folder. \
                 Start agents only in projects you trust.",
            ))
            .child(
                DialogButtons::new()
                    .primary(
                        dialog_button(
                            "trust-accept",
                            "trust-accept".to_owned(),
                            "Trust Project",
                            true,
                            true,
                            &theme,
                        )
                        .on_click(cx.listener(|this, _, window, cx| this.trust(window, cx))),
                    )
                    .other(
                        dialog_button(
                            "trust-cancel",
                            "trust-cancel".to_owned(),
                            "Don't Trust",
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
                key: "trust",
                title: "Trust this project?".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: None,
                },
                min: (360., 200.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

/// What the window tells the language server of a project that opens. The server runs code
/// of the project while it imports the build, so it starts only when the project is trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerTrust {
    Trusted,
    Untrusted,
}

/// Starts the language server of the project. The application implements it: the window
/// knows no process and no language.
pub trait LanguageStart {
    /// Starts the server with `trust` and sends it once it is ready, or sends `None` when
    /// it cannot start. `Untrusted` starts nothing: it only says that nothing may run.
    fn start(&self, trust: ServerTrust, started: UnboundedSender<Option<crate::StartedServer>>);
}

/// Whether the project of the window may run code, and how its language server starts once
/// the user trusts it. The application sets it before the window opens; a window of a test
/// that sets nothing opens a trusted project.
#[derive(Clone)]
pub struct ProjectTrust {
    pub trusted: bool,
    pub start: Option<Rc<dyn LanguageStart>>,
}

impl ProjectTrust {
    pub fn trusted() -> Self {
        Self {
            trusted: true,
            start: None,
        }
    }

    pub fn untrusted(start: Rc<dyn LanguageStart>) -> Self {
        Self {
            trusted: false,
            start: Some(start),
        }
    }
}

impl Default for ProjectTrust {
    fn default() -> Self {
        Self::trusted()
    }
}

impl Global for ProjectTrust {}

/// The dialog of a project that opens for the first time. The project opens in Safe Mode
/// behind it: files can be read, but nothing of the project runs. Trust Project records the
/// answer in [`TrustList`] and starts the build and the language server without a restart;
/// the folder of the project can be trusted as a whole instead.
pub struct ProjectTrustDialog {
    project: PathBuf,
    parent: Option<PathBuf>,
    trust_parent: bool,
    list: TrustList,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
}

impl ProjectTrustDialog {
    /// Shows the dialog over the window of an untrusted project.
    pub fn show(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
        let project = workspace.root().to_path_buf();
        let state_dir = workspace.state_dir().to_path_buf();
        let parent = project
            .parent()
            .filter(|parent| *parent != project)
            .map(Path::to_path_buf);
        let list = TrustList::new(&state_dir);
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            project,
            parent,
            trust_parent: false,
            list,
            focus: cx.focus_handle(),
            workspace: weak,
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn trust(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let target = if self.trust_parent {
            self.parent.clone().unwrap_or_else(|| self.project.clone())
        } else {
            self.project.clone()
        };
        if let Err(error) = self.list.trust(&target) {
            let text = format!("{}: {error}", target.display());
            self.workspace
                .update(cx, |workspace, cx| {
                    workspace.notify_error("Trust is not saved", text, cx)
                })
                .ok();
        }
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.set_safe_mode(false, cx);
                workspace.dismiss(window, cx);
                if let Some(panel) = cx.try_global::<crate::script::ActiveRunPanel>() {
                    let panel = panel.0.clone();
                    panel.update(cx, |panel, cx| panel.trust_project(cx));
                }
            })
            .ok();
        cx.default_global::<ProjectTrust>().trusted = true;
        self.start_server(cx);
    }

    /// Starts the language server the window held back while the project was untrusted.
    fn start_server(&mut self, cx: &mut Context<Self>) {
        let Some(start) = cx
            .try_global::<ProjectTrust>()
            .and_then(|trust| trust.start.clone())
        else {
            return;
        };
        let (sender, receiver) = futures_channel::mpsc::unbounded();
        start.start(ServerTrust::Trusted, sender);
        let workspace = self.workspace.clone();
        cx.spawn(async move |_this, cx| {
            let mut receiver = receiver;
            while let Some(started) = receiver.next().await {
                let Some(started) = started else {
                    continue;
                };
                workspace
                    .update(cx, |workspace, cx| {
                        workspace.set_language_setup(None, false, cx);
                        crate::connect_server(workspace, started, cx);
                    })
                    .ok();
            }
        })
        .detach();
    }

    fn dont_open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                workspace.close_window(window, cx);
            })
            .ok();
    }

    fn toggle_parent(&mut self, cx: &mut Context<Self>) {
        self.trust_parent = !self.trust_parent;
        cx.notify();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => self.trust(window, cx),
            "escape" => self.close(window, cx),
            _ => return,
        }
        cx.stop_propagation();
    }
}

impl Render for ProjectTrustDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let width = px(OPEN_WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = viewport.height * 0.22;
        let name = self.project.file_name().map_or_else(
            || self.project.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        let title = format!("Trust and Open Project '{name}'?");
        let location = self.project.display().to_string();
        let parent = self.parent.clone();
        let parent_label = parent
            .as_ref()
            .map(|parent| format!("Trust projects in {}", parent.display()));
        let shell = surface(&theme)
            .id("trust-open-project")
            .key_context("ProjectTrustDialog")
            .track_focus(&self.focus)
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)));
        let mut contents = div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .child(div().text_color(theme.text_muted).child(location))
            .child(div().text_color(theme.text_secondary).child(
                "The project can run code while it builds: the build wrappers (mvnw, gradlew), \
                 build scripts, compiler plugins and its language server. In Safe Mode Arro \
                 opens the files without building and without a language server. Git keeps \
                 working.",
            ));
        if let Some(label) = parent_label {
            contents = contents.child(
                checkbox_row("trust-open-parent", label, self.trust_parent, &theme)
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_parent(cx))),
            );
        }
        contents = contents.child(
            DialogButtons::new()
                .primary(
                    dialog_button(
                        "trust-open-accept",
                        "trust-open-accept".to_owned(),
                        "Trust Project",
                        true,
                        true,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.trust(window, cx))),
                )
                .other(
                    dialog_button(
                        "trust-open-preview",
                        "trust-open-preview".to_owned(),
                        "Preview in Safe Mode",
                        false,
                        true,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                )
                .other(
                    dialog_button(
                        "trust-open-dont",
                        "trust-open-dont".to_owned(),
                        "Don't Open",
                        false,
                        true,
                        &theme,
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.dont_open(window, cx))),
                )
                .into_row(),
        );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "trust-open",
                title: title.into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: None,
                },
                min: (420., 240.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

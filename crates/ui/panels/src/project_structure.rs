//! Project Structure, Ctrl+Alt+Shift+S: the SDK of the project and the folders it may
//! use, the modules of its model with their sources and dependencies, and the language
//! level of the project and of each module. The window knows no language: everything it
//! shows and changes comes from [`project_model::EnvironmentProvider`].

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use editor_view::{EditorView, Spot};
use project_model::{
    Dependency, EnvironmentDetail, EnvironmentProvider, LanguageLevelEdit, LanguageLevelInfo,
    MAIN_UNIT, Module, ProjectEnvironmentInfo, ProjectModel, RuntimeInfo, RuntimeReason,
};
use ui_kit::base::InteractiveElementExt;
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::build::{declaration_offset, short_name};
use crate::navigation;

const WIDTH: f32 = 760.;
const BODY_HEIGHT: f32 = 380.;
/// The height on the first opening: the body and the room of the title bar, the buttons and
/// the paddings around them.
const DIALOG_HEIGHT: f32 = BODY_HEIGHT + 120.;
const SECTIONS_WIDTH: f32 = 160.;
const MODULES_WIDTH: f32 = 210.;
/// The language levels the Project page offers, as IDEA does.
const LANGUAGE_LEVELS: [&str; 5] = ["8", "11", "17", "21", "25"];
/// The item of the level list that follows the chosen SDK.
const SDK_DEFAULT: &str = "SDK default";
/// How many polls a build file gets to appear in an editor before the change gives up.
const EDITOR_POLLS: usize = 200;
const EDITOR_POLL: Duration = Duration::from_millis(10);
/// How deep a dependency tree of the model may go before it is a cycle.
const MAX_DEPENDENCY_DEPTH: usize = 64;

/// The environment of the project, for the windows that show and change it.
pub(crate) struct ProjectEnvironmentGlobal(pub Option<Arc<dyn EnvironmentProvider>>);

impl Global for ProjectEnvironmentGlobal {}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Project,
    Modules,
    Sdks,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ModuleTab {
    Sources,
    Dependencies,
}

/// Which page owns a language level field.
#[derive(Clone, Copy, PartialEq, Eq)]
enum LevelTarget {
    Project,
    Module,
}

/// Ctrl+Alt+Shift+S shows the window.
pub fn bind_actions(workspace: WeakEntity<Workspace>, window: AnyWindowHandle, cx: &mut App) {
    cx.on_action(move |_: &workspace::ProjectStructure, cx| {
        let workspace = workspace.clone();
        // The window is being updated while its action runs, so the rest waits.
        cx.defer(move |cx| {
            let show = move |_: AnyView, window: &mut Window, cx: &mut App| {
                let open = |workspace: &mut Workspace, cx: &mut Context<Workspace>| {
                    ProjectStructureDialog::show(workspace, window, cx)
                };
                workspace.update(cx, open).ok();
            };
            window.update(cx, show).ok();
        });
    });
}

/// The dialog over the workspace.
pub(crate) struct ProjectStructureDialog {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    environment: Option<Arc<dyn EnvironmentProvider>>,
    section: Section,
    runtimes: Vec<RuntimeInfo>,
    selected: Option<PathBuf>,
    reason: RuntimeReason,
    notice: Option<String>,
    language: Option<project_model::LanguageLevelInfo>,
    /// Read-only lines the language fills, such as a dependency it found.
    details: Vec<EnvironmentDetail>,
    /// The width of the label columns of the pages, measured at the last frame.
    label_width: f32,
    /// The model of the project, for the Modules page.
    model: Option<Arc<ProjectModel>>,
    /// The module the Modules page shows.
    selected_module: Option<String>,
    module_tab: ModuleTab,
    /// The level of the selected module, read from its own build file.
    module_language: Option<LanguageLevelInfo>,
    /// The module `module_language` belongs to.
    module_language_for: Option<String>,
    /// The levels the user picked but has not written yet.
    project_level: Option<String>,
    module_level: Option<String>,
    /// What the next OK adds or changes, under the level field.
    language_summary: Option<String>,
    module_language_summary: Option<String>,
    /// Modules whose children the user folded away.
    collapsed: HashSet<String>,
    /// The SDK list under the project field is open.
    sdk_open: bool,
    language_open: bool,
    module_language_open: bool,
    /// What went wrong with the last action, under the field that caused it.
    error: Option<String>,
    loading: bool,
    applying: bool,
    scroll: ScrollHandle,
}

impl ProjectStructureDialog {
    pub(crate) fn show(
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let weak = cx.weak_entity();
        let environment = cx
            .try_global::<ProjectEnvironmentGlobal>()
            .and_then(|global| global.0.clone());
        let view = cx.new(|cx| Self {
            workspace: weak,
            focus: cx.focus_handle(),
            environment: environment.clone(),
            section: Section::Project,
            runtimes: Vec::new(),
            selected: None,
            reason: RuntimeReason::Default,
            notice: None,
            language: None,
            details: Vec::new(),
            label_width: FORM_LABEL_MIN,
            model: None,
            selected_module: None,
            module_tab: ModuleTab::Sources,
            module_language: None,
            module_language_for: None,
            project_level: None,
            module_level: None,
            language_summary: None,
            module_language_summary: None,
            collapsed: HashSet::new(),
            sdk_open: false,
            language_open: false,
            module_language_open: false,
            error: None,
            loading: environment.is_some(),
            applying: false,
            scroll: ScrollHandle::new(),
        });
        if let Some(environment) = environment {
            let read = cx.background_executor().spawn(async move {
                let info = environment.describe();
                let details = environment.details();
                (info, details)
            });
            let dialog = view.downgrade();
            cx.spawn(async move |_, cx| {
                let (info, details) = read.await;
                dialog
                    .update(cx, |dialog, cx| dialog.loaded(info, details, cx))
                    .ok();
            })
            .detach();
        }
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    fn loaded(
        &mut self,
        info: ProjectEnvironmentInfo,
        details: Vec<EnvironmentDetail>,
        cx: &mut Context<Self>,
    ) {
        self.runtimes = info.runtimes;
        self.selected = info.selected;
        self.reason = info.reason;
        self.notice = info.notice;
        self.language = info.language;
        self.details = details;
        self.model = self
            .environment
            .as_ref()
            .and_then(|environment| environment.model());
        self.loading = false;
        self.ensure_module(cx);
        self.preview(LevelTarget::Project, cx);
        cx.notify();
    }

    fn selected_name(&self) -> String {
        self.selected
            .as_deref()
            .and_then(|home| self.runtimes.iter().find(|runtime| runtime.home == home))
            .map(|runtime| runtime.name.clone())
            .unwrap_or_else(|| SDK_DEFAULT.to_owned())
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtimes.get(index) else {
            return;
        };
        self.selected = Some(runtime.home.clone());
        self.sdk_open = false;
        self.error = None;
        cx.notify();
    }

    fn remove(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtimes.get(index) else {
            return;
        };
        if !runtime.added {
            return;
        }
        let removed = self.runtimes.remove(index);
        if self.selected.as_deref() == Some(removed.home.as_path()) {
            self.selected = self.runtimes.first().map(|runtime| runtime.home.clone());
        }
        self.error = None;
        cx.notify();
    }

    /// Adds a folder the user picked. A folder that is not an SDK is an error under the
    /// field, and nothing changes.
    pub(crate) fn add_folder(&mut self, folder: PathBuf, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            self.error = Some("No language of this project can choose an SDK".to_owned());
            cx.notify();
            return;
        };
        let read = folder.clone();
        let task = cx
            .background_executor()
            .spawn(async move { environment.runtime_at(&read) });
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| match outcome {
                Ok(runtime) => {
                    if !this.runtimes.iter().any(|known| known.home == runtime.home) {
                        this.runtimes.push(runtime.clone());
                    }
                    this.selected = Some(runtime.home.clone());
                    this.error = None;
                    cx.notify();
                }
                Err(error) => {
                    this.error = Some(error);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
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
                this.update(cx, |this, cx| this.add_folder(folder, cx)).ok();
            }
        })
        .detach();
    }

    fn open_build_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(language) = self.language.clone() else {
            return;
        };
        let Some(file) = language.file else {
            return;
        };
        let spot = language.line.map(|line| editor_view::Spot::Line {
            line: line as usize,
            column: 0,
        });
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                navigation::go_to(workspace, file, spot, true, window, cx);
            })
            .ok();
    }

    fn show_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        self.sdk_open = false;
        self.language_open = false;
        self.module_language_open = false;
        if section == Section::Modules {
            self.model = self
                .environment
                .as_ref()
                .and_then(|environment| environment.model());
            self.ensure_module(cx);
        }
        cx.notify();
    }

    /// The first module of the model is shown until the user picks another one.
    fn ensure_module(&mut self, cx: &mut Context<Self>) {
        let Some(model) = self.model.clone() else {
            return;
        };
        let current = self.selected_module.as_deref();
        let index = model
            .modules
            .iter()
            .position(|module| Some(module.id.as_str()) == current)
            .or_else(|| (!model.modules.is_empty()).then_some(0));
        let Some(index) = index else {
            return;
        };
        if Some(model.modules[index].id.as_str()) != current {
            self.select_module(index, cx);
        }
    }

    fn select_module(&mut self, index: usize, cx: &mut Context<Self>) {
        let Some(module) = self
            .model
            .as_ref()
            .and_then(|model| model.modules.get(index))
        else {
            return;
        };
        let id = module.id.clone();
        self.selected_module = Some(id.clone());
        self.module_tab = ModuleTab::Sources;
        self.module_language = None;
        self.module_level = None;
        self.module_language_summary = None;
        self.load_module_language(id, cx);
        cx.notify();
    }

    fn load_module_language(&mut self, id: String, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        self.module_language_for = Some(id.clone());
        let read = id.clone();
        let task = cx
            .background_executor()
            .spawn(async move { environment.module_language(&read) });
        cx.spawn(async move |this, cx| {
            let found = task.await;
            this.update(cx, |this, cx| {
                if this.module_language_for.as_deref() == Some(id.as_str()) {
                    this.module_language = found;
                    this.preview(LevelTarget::Module, cx);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }

    fn module(&self) -> Option<&Module> {
        let model = self.model.as_ref()?;
        let id = self.selected_module.as_deref()?;
        model.module(id)
    }

    fn file_of(&self, module: Option<&str>) -> Option<PathBuf> {
        match module {
            Some(id) => {
                let model = self.model.as_ref()?;
                let found = model.module(id)?;
                found.build_file.clone().or_else(|| {
                    (self.selected_module.as_deref() == Some(id))
                        .then(|| self.module_language.as_ref()?.file.clone())
                        .flatten()
                })
            }
            None => self.language.as_ref()?.file.clone(),
        }
    }

    /// The text of the build file as the user sees it, when its editor is open.
    fn open_text(&self, path: Option<&Path>, cx: &mut Context<Self>) -> Option<String> {
        let path = path?;
        let workspace = self.workspace.upgrade()?;
        let view = workspace
            .read(cx)
            .view_of(path)
            .and_then(|view| view.downcast::<EditorView>().ok())?;
        view.read(cx).text()
    }

    fn toggle_language(&mut self, target: LevelTarget, cx: &mut Context<Self>) {
        match target {
            LevelTarget::Project => self.language_open = !self.language_open,
            LevelTarget::Module => self.module_language_open = !self.module_language_open,
        }
        cx.notify();
    }

    fn choose_language(&mut self, target: LevelTarget, level: String, cx: &mut Context<Self>) {
        let level = if level == SDK_DEFAULT {
            // The SDK default is the level of the SDK in force: the build file cannot say
            // "whatever the SDK says", so the number is written as it is.
            match self.sdk_default_level() {
                Some(level) => level,
                None => {
                    self.language_open = false;
                    self.module_language_open = false;
                    cx.notify();
                    return;
                }
            }
        } else {
            level
        };
        match target {
            LevelTarget::Project => {
                self.project_level = Some(level);
                self.language_open = false;
            }
            LevelTarget::Module => {
                self.module_level = Some(level);
                self.module_language_open = false;
            }
        }
        self.preview(target, cx);
        cx.notify();
    }

    /// The language level of the SDK the project uses, when the SDK list knows it.
    fn sdk_default_level(&self) -> Option<String> {
        let selected = self.selected.as_deref()?;
        self.runtimes
            .iter()
            .find(|runtime| runtime.home == selected)
            .map(|runtime| runtime.feature.to_string())
    }

    /// Reads what OK would write, so the form can say it before the user presses it.
    fn preview(&mut self, target: LevelTarget, cx: &mut Context<Self>) {
        let Some(environment) = self.environment.clone() else {
            return;
        };
        let (module, level) = match target {
            LevelTarget::Project => (
                None,
                self.project_level.clone().or_else(|| {
                    let info = self.language.as_ref()?;
                    info.selected
                        .is_none()
                        .then(|| info.values.first().cloned())
                        .flatten()
                }),
            ),
            LevelTarget::Module => (
                self.selected_module.clone(),
                self.module_level.clone().or_else(|| {
                    let info = self.module_language.as_ref()?;
                    info.selected
                        .is_none()
                        .then(|| info.values.first().cloned())
                        .flatten()
                }),
            ),
        };
        let Some(level) = level else {
            return;
        };
        let text = self.open_text(self.file_of(module.as_deref()).as_deref(), cx);
        let chosen_level = level.clone();
        let preview_module = module.clone();
        let task = cx.background_executor().spawn(async move {
            environment.language_change(&level, module.as_deref(), text.as_deref())
        });
        let chosen = chosen_level;
        cx.spawn(async move |this, cx| {
            let outcome = task.await;
            this.update(cx, |this, cx| {
                if target == LevelTarget::Module && this.module_language_for != preview_module {
                    return;
                }
                let current = match target {
                    LevelTarget::Project => this.project_level.clone(),
                    LevelTarget::Module => this.module_level.clone(),
                }
                .or_else(|| match target {
                    LevelTarget::Project => this
                        .language
                        .as_ref()
                        .and_then(|info| info.selected.clone()),
                    LevelTarget::Module => this
                        .module_language
                        .as_ref()
                        .and_then(|info| info.selected.clone()),
                });
                if let Some(current) = current
                    && current != chosen
                {
                    return;
                }
                let summary = outcome.ok().map(|edit| edit.summary);
                match target {
                    LevelTarget::Project => this.language_summary = summary,
                    LevelTarget::Module => this.module_language_summary = summary,
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Stores the choice and the folders, writes the level and closes the window when
    /// that was OK. Nothing is written before this point, so Cancel changes nothing.
    fn apply(&mut self, close: bool, window: &mut Window, cx: &mut Context<Self>) {
        if self.applying || self.loading || self.error.is_some() {
            return;
        }
        let Some(environment) = self.environment.clone() else {
            self.error = Some("No language of this project can choose an SDK".to_owned());
            cx.notify();
            return;
        };
        let selected = self.selected.clone();
        let added: Vec<PathBuf> = self
            .runtimes
            .iter()
            .filter(|runtime| runtime.added)
            .map(|runtime| runtime.home.clone())
            .collect();
        let mut targets: Vec<(Option<String>, String)> = Vec::new();
        if let Some(level) = self.project_level.clone()
            && self
                .language
                .as_ref()
                .and_then(|info| info.selected.as_deref())
                != Some(level.as_str())
        {
            targets.push((None, level));
        }
        if let Some(level) = self.module_level.clone()
            && self
                .module_language
                .as_ref()
                .and_then(|info| info.selected.as_deref())
                != Some(level.as_str())
            && let Some(module) = self.selected_module.clone()
        {
            targets.push((Some(module), level));
        }
        // A parent module and the project may share one build file: the module, the
        // narrower setting, wins there.
        let mut unique: Vec<(Option<String>, String)> = Vec::new();
        for target in targets {
            let file = self.file_of(target.0.as_deref());
            let known = file.as_ref().and_then(|file| {
                unique
                    .iter()
                    .position(|(module, _)| self.file_of(module.as_deref()).as_ref() == Some(file))
            });
            match known {
                Some(index) => unique[index] = target,
                None => unique.push(target),
            }
        }
        let targets = unique;
        let texts: Vec<Option<String>> = targets
            .iter()
            .map(|(module, _)| self.open_text(self.file_of(module.as_deref()).as_deref(), cx))
            .collect();
        self.applying = true;
        let task = cx.background_executor().spawn(async move {
            let info = environment.apply(selected.as_deref(), &added)?;
            let details = environment.details();
            let mut edits = Vec::new();
            for ((module, level), text) in targets.into_iter().zip(texts) {
                edits.push(environment.language_change(
                    &level,
                    module.as_deref(),
                    text.as_deref(),
                )?);
            }
            Ok((info, details, edits))
        });
        cx.spawn_in(window, async move |this, cx| {
            let outcome = task.await;
            this.update_in(cx, |this, window, cx| match outcome {
                Err(error) => {
                    this.applying = false;
                    this.error = Some(error);
                    cx.notify();
                }
                Ok((info, details, edits)) => {
                    if edits.is_empty() {
                        this.applying = false;
                        this.loaded(info, details, cx);
                        if close {
                            this.close(window, cx);
                        }
                        return;
                    }
                    this.write_levels(edits, info, details, close, window, cx);
                }
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    /// Opens every build file the change reaches, so that one undo takes it back, then
    /// writes the edits through the same path the refactorings use.
    fn write_levels(
        &mut self,
        edits: Vec<LanguageLevelEdit>,
        info: ProjectEnvironmentInfo,
        details: Vec<EnvironmentDetail>,
        close: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let mut combined = lang_api::WorkspaceChange::default();
        let mut files = Vec::new();
        for edit in &edits {
            combined.edits.extend(edit.change.edits.iter().cloned());
            combined.renames.extend(edit.change.renames.iter().cloned());
            files.push(edit.file.clone());
        }
        let workspace = self.workspace.clone();
        for file in &files {
            workspace
                .update(cx, |workspace, cx| {
                    workspace.open_file(file.clone(), window, cx)
                })
                .ok();
        }
        cx.spawn_in(window, async move |this, cx| {
            let mut ready = false;
            for _ in 0..EDITOR_POLLS {
                ready = files.iter().all(|file| {
                    workspace
                        .read_with(cx, |workspace, cx| {
                            workspace
                                .view_of(file)
                                .and_then(|view| view.downcast::<EditorView>().ok())
                                .is_some_and(|view| view.read(cx).text().is_some())
                        })
                        .unwrap_or(false)
                });
                if ready {
                    break;
                }
                cx.background_executor().timer(EDITOR_POLL).await;
            }
            this.update_in(cx, |this, window, cx| {
                if ready {
                    if let Some(workspace) = this.workspace.upgrade() {
                        workspace.update(cx, |workspace, cx| {
                            crate::changes::apply(workspace, combined, window, cx);
                            for file in &files {
                                workspace.save_editor(file, cx);
                            }
                        });
                        // The build file changed, so the project is imported again as
                        // after any edit of a build script.
                        if let Some(panel) = cx.try_global::<crate::script::ActiveRunPanel>() {
                            let panel = panel.0.clone();
                            cx.defer(move |cx| {
                                panel.update(cx, |panel, cx| panel.model_file_changed(cx))
                            });
                        }
                    }
                } else {
                    this.error = Some("Could not open the build file to change it".to_owned());
                }
                this.applying = false;
                let project_level = this.project_level.clone();
                let module_level = this.module_level.clone();
                this.loaded(info, details, cx);
                if let Some(level) = project_level
                    && let Some(language) = this.language.as_mut()
                {
                    language.selected = Some(level);
                }
                if let Some(level) = module_level
                    && let Some(language) = this.module_language.as_mut()
                {
                    language.selected = Some(level);
                }
                this.language_summary = None;
                this.module_language_summary = None;
                if close && ready {
                    this.close(window, cx);
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
        cx.notify();
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    /// What went wrong with the last action, for the window tests.
    #[cfg(test)]
    pub(crate) fn error_text(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// The SDKs the dialog shows, for the window tests.
    #[cfg(test)]
    pub(crate) fn runtime_count(&self) -> usize {
        self.runtimes.len()
    }

    /// The levels the module page has read, for the window tests.
    #[cfg(test)]
    pub(crate) fn module_level_count(&self) -> usize {
        self.module_language
            .as_ref()
            .map_or(0, |info| info.values.len())
    }

    /// The modules the dialog holds, for the window tests.
    #[cfg(test)]
    pub(crate) fn module_count(&self) -> usize {
        self.model.as_ref().map_or(0, |model| model.modules.len())
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => self.close(window, cx),
            "enter" => self.apply(true, window, cx),
            _ => {}
        }
    }

    /// The module list of the Modules page, with the folders of the build tools'
    /// nesting. Modules folded by the user keep their children out of the list.
    fn module_rows(&self, theme: &Theme, cx: &mut Context<Self>) -> Vec<Stateful<Div>> {
        let Some(model) = &self.model else {
            return Vec::new();
        };
        let parents: Vec<Option<usize>> = (0..model.modules.len())
            .map(|index| parent_of(&model.modules, index))
            .collect();
        let mut rows = Vec::new();
        for (index, module) in model.modules.iter().enumerate() {
            let ancestors = ancestors_in(&parents, index);
            if ancestors
                .iter()
                .any(|parent| self.collapsed.contains(&model.modules[*parent].id))
            {
                continue;
            }
            let depth = ancestors.len();
            let children = parents.contains(&Some(index));
            let folded = self.collapsed.contains(&module.id);
            let chosen = self.selected_module.as_deref() == Some(module.id.as_str());
            let arrow = if !children {
                " "
            } else if folded {
                "▸"
            } else {
                "▾"
            };
            let arrow = div()
                .id(("project-structure-module-arrow", index))
                .debug_selector(move || format!("project-structure-module-arrow-{index}"))
                .flex()
                .items_center()
                .justify_center()
                .w(px(SPACE_XL))
                .flex_none()
                .child(arrow)
                .on_click(cx.listener(move |this, _, _, cx| {
                    let id = this
                        .model
                        .as_ref()
                        .and_then(|model| model.modules.get(index))
                        .map(|module| module.id.clone());
                    if let Some(id) = id
                        && !this.collapsed.remove(&id)
                    {
                        this.collapsed.insert(id);
                    }
                    cx.notify();
                }));
            let row = div()
                .id(("project-structure-module", index))
                .debug_selector(move || format!("project-structure-module-{index}"))
                .flex()
                .items_center()
                .gap(px(SPACE_XS))
                .h(px(CONTROL_HEIGHT))
                .pl(px(depth as f32 * SPACE_M))
                .pr(px(ROW_PADDING_X))
                .rounded(px(ROW_RADIUS))
                .when(chosen, |this| this.bg(theme.selection))
                .hover({
                    let hover = theme.hover;
                    move |style| style.bg(hover)
                })
                .text_color(if chosen {
                    theme.text
                } else {
                    theme.text_secondary
                })
                .child(arrow)
                .child(module.name.clone())
                .on_click(cx.listener(move |this, _, _, cx| this.select_module(index, cx)));
            rows.push(row);
        }
        rows
    }

    fn sources_tab(&self, module: &Module, theme: &Theme) -> Div {
        let build = module
            .build_file
            .as_ref()
            .or_else(|| {
                self.module_language
                    .as_ref()
                    .and_then(|info| info.file.as_ref())
            })
            .and_then(|file| file.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "the build file".to_owned());
        let mut rows: Vec<(&'static str, PathBuf)> = Vec::new();
        let mut seen = HashSet::new();
        let mut push =
            |label: &'static str, path: &Path, rows: &mut Vec<(&'static str, PathBuf)>| {
                if seen.insert(path.to_path_buf()) {
                    rows.push((label, path.to_path_buf()));
                }
            };
        for unit in &module.units {
            let label = if unit.name == MAIN_UNIT {
                "Sources"
            } else {
                "Tests"
            };
            for root in &unit.source_roots {
                if module
                    .generated_roots
                    .iter()
                    .any(|generated| generated == root)
                {
                    continue;
                }
                push(label, root, &mut rows);
            }
        }
        for unit in &module.units {
            for root in &unit.resource_roots {
                push("Resources", root, &mut rows);
            }
        }
        for root in &module.generated_roots {
            push("Generated", root, &mut rows);
        }
        for root in &module.excluded_roots {
            push("Excluded", root, &mut rows);
        }
        div()
            .debug_selector(|| "project-structure-module-sources".to_owned())
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .child(
                div()
                    .debug_selector(|| "project-structure-module-build".to_owned())
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child(format!("Folders from {build}, read only")),
            )
            .children(rows.into_iter().enumerate().map(|(index, (label, path))| {
                div()
                    .id(("project-structure-module-folder", index))
                    .debug_selector(move || format!("project-structure-module-folder-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .min_h(px(CONTROL_HEIGHT))
                    .child(form_label(self.label_width, label, theme))
                    .child(
                        div()
                            .min_w(px(0.))
                            .text_color(theme.text)
                            .child(path.display().to_string()),
                    )
            }))
    }

    fn dependencies_tab(&self, module: &Module, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let build = module
            .build_file
            .as_ref()
            .or_else(|| {
                self.module_language
                    .as_ref()
                    .and_then(|info| info.file.as_ref())
            })
            .and_then(|file| file.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "the build file".to_owned());
        let mut rows = Vec::new();
        if let Some(dependencies) = self
            .model
            .as_ref()
            .and_then(|model| model.dependencies_of(&module.id))
        {
            for scope in &dependencies.scopes {
                for root in &scope.roots {
                    collect_dependency(root, &scope.name, 0, &mut rows);
                }
            }
        }
        let empty = rows.is_empty().then(|| {
            div()
                .debug_selector(|| "project-structure-module-no-dependencies".to_owned())
                .text_color(theme.text_muted)
                .child("The build tool has not resolved the dependencies of the module yet.")
        });
        let module_id = module.id.clone();
        div()
            .debug_selector(|| "project-structure-module-dependencies".to_owned())
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .child(
                div()
                    .debug_selector(|| "project-structure-module-dependencies-build".to_owned())
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child(format!(
                        "Dependencies from {build}; a double click opens the declaration"
                    )),
            )
            .children(empty)
            .children(rows.into_iter().enumerate().map(|(index, row)| {
                let coordinates = row.coordinates.clone();
                let module_id = module_id.clone();
                div()
                    .id(("project-structure-module-dependency", index))
                    .debug_selector(move || format!("project-structure-module-dependency-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .h(px(CONTROL_HEIGHT))
                    .pl(px(row.depth as f32 * SPACE_M))
                    .pr(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .hover({
                        let hover = theme.hover;
                        move |style| style.bg(hover)
                    })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_color(theme.text)
                            .child(short_name(&row.coordinates)),
                    )
                    .child(
                        div()
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_muted)
                            .child(format!("[{}]", row.scope)),
                    )
                    .on_double_click(cx.listener(move |this, _, window, cx| {
                        this.open_dependency(module_id.clone(), coordinates.clone(), window, cx)
                    }))
            }))
    }

    /// A double click on a dependency: its declaration in the build file of the module,
    /// like the row of the Dependencies window.
    fn open_dependency(
        &mut self,
        module: String,
        coordinates: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(model) = self.model.clone() else {
            return;
        };
        let file = model
            .dependencies_of(&module)
            .and_then(|dependencies| dependencies.build_file.clone())
            .or_else(|| {
                model
                    .module(&module)
                    .and_then(|module| module.build_file.clone())
            })
            .or_else(|| {
                (self.selected_module.as_deref() == Some(module.as_str()))
                    .then(|| self.module_language.as_ref()?.file.clone())
                    .flatten()
            });
        let Some(file) = file else {
            self.error = Some(format!(
                "The module {} does not name the build file it is declared in",
                module
            ));
            cx.notify();
            return;
        };
        let text = self
            .open_text(Some(&file), cx)
            .or_else(|| std::fs::read_to_string(&file).ok());
        let Some(text) = text else {
            self.error = Some(format!("Cannot read {}", file.display()));
            cx.notify();
            return;
        };
        let Some(_offset) = declaration_offset(&text, &coordinates) else {
            self.error = Some(format!("{} does not declare {coordinates}", file.display()));
            cx.notify();
            return;
        };
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                workspace.open_file(file.clone(), window, cx);
                cx.spawn_in(window, async move |workspace, cx| {
                    for _ in 0..EDITOR_POLLS {
                        let done = workspace
                            .update(cx, |workspace, cx| {
                                let editor = workspace
                                    .view_of(&file)
                                    .and_then(|view| view.downcast::<EditorView>().ok());
                                let Some(editor) = editor else {
                                    return false;
                                };
                                let Some(text) = editor.read(cx).text() else {
                                    return false;
                                };
                                if let Some(offset) = declaration_offset(&text, &coordinates) {
                                    editor.update(cx, |editor, cx| {
                                        editor.go_to(Spot::Offset(offset), cx)
                                    });
                                }
                                true
                            })
                            .unwrap_or(true);
                        if done {
                            break;
                        }
                        cx.background_executor().timer(EDITOR_POLL).await;
                    }
                })
                .detach();
            })
            .ok();
    }

    fn module_page(&self, module: &Module, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let sdk = self.selected_name();
        let tabs: Vec<Stateful<Div>> = [
            ("sources", "Sources", ModuleTab::Sources),
            ("dependencies", "Dependencies", ModuleTab::Dependencies),
        ]
        .into_iter()
        .map(|(suffix, label, tab)| {
            let selector = format!("project-structure-module-tab-{suffix}");
            let id = format!("project-structure-module-tab-{suffix}");
            let chosen = self.module_tab == tab;
            div()
                .id(id)
                .debug_selector(move || selector)
                .flex()
                .items_center()
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .rounded(px(TAB_RADIUS))
                .when(chosen, |this| {
                    this.bg(theme.selection).text_color(theme.text)
                })
                .when(!chosen, |this| {
                    let hover = theme.hover;
                    this.text_color(theme.text_secondary)
                        .hover(move |style| style.bg(hover))
                })
                .child(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.module_tab = tab;
                    cx.notify();
                }))
        })
        .collect();
        let level = self.module_language.clone();
        let level_field = self.language_field(
            "project-structure-module-language",
            LevelTarget::Module,
            level.as_ref(),
            theme,
            cx,
        );
        let content = match self.module_tab {
            ModuleTab::Sources => self.sources_tab(module, theme),
            ModuleTab::Dependencies => self.dependencies_tab(module, theme, cx),
        };
        div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .gap(px(SPACE_M))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(
                        div()
                            .text_size(px(TEXT_TITLE))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(module.name.clone()),
                    )
                    .child(
                        div()
                            .debug_selector(|| "project-structure-module-sdk".to_owned())
                            .text_size(px(TEXT_SMALL))
                            .text_color(theme.text_muted)
                            .child(format!("SDK: {sdk}")),
                    ),
            )
            .child(level_field)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_XS))
                    .border_b_1()
                    .border_color(theme.control_border)
                    .children(tabs),
            )
            .child(content)
    }

    /// The field of one language level: the value and the levels under it, and what the
    /// next OK writes.
    fn language_field(
        &self,
        prefix: &'static str,
        target: LevelTarget,
        info: Option<&LanguageLevelInfo>,
        theme: &Theme,
        cx: &mut Context<Self>,
    ) -> Div {
        let picked = match target {
            LevelTarget::Project => self.project_level.clone(),
            LevelTarget::Module => self.module_level.clone(),
        };
        let selected = picked
            .clone()
            .or_else(|| info.and_then(|info| info.selected.clone()))
            .unwrap_or_else(|| "Not set".to_owned());
        let open = match target {
            LevelTarget::Project => self.language_open,
            LevelTarget::Module => self.module_language_open,
        };
        let summary = match target {
            LevelTarget::Project => self.language_summary.clone(),
            LevelTarget::Module => self.module_language_summary.clone(),
        };
        let file = info
            .and_then(|info| info.file.as_ref())
            .and_then(|file| file.file_name())
            .map(|name| name.to_string_lossy().into_owned());
        let can_choose = info.is_some_and(|info| info.file.is_some());
        let field = select_field(format!("{prefix}-field"), selected, open, theme)
            .debug_selector(move || prefix.to_owned())
            .flex_1()
            .when(!can_choose, |this| this.cursor_default())
            .on_click(cx.listener(move |this, _, _, cx| {
                if can_choose {
                    this.toggle_language(target, cx);
                }
            }));
        let items = open.then(|| {
            let mut menu = select_menu(theme).debug_selector({
                let prefix = prefix.to_owned();
                move || format!("{prefix}-menu")
            });
            for value in level_values(info) {
                let chosen = info
                    .and_then(|info| info.selected.as_deref())
                    .is_some_and(|selected| selected == value)
                    || picked.as_deref() == Some(value.as_str());
                let level = value.clone();
                let element_id = format!("{prefix}-{value}");
                let selection = value.clone();
                let selector = {
                    let prefix = prefix.to_owned();
                    move || format!("{prefix}-item-{selection}")
                };
                menu = menu.child(
                    select_option(element_id, value, chosen, chosen, theme)
                        .debug_selector(selector)
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.choose_language(target, level.clone(), cx)
                        })),
                );
            }
            menu = menu.child(
                select_option(
                    format!("{prefix}-{SDK_DEFAULT}"),
                    SDK_DEFAULT,
                    false,
                    false,
                    theme,
                )
                .debug_selector({
                    let prefix = prefix.to_owned();
                    move || format!("{prefix}-item-sdk-default")
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.choose_language(target, SDK_DEFAULT.to_owned(), cx)
                })),
            );
            let note = div()
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_muted)
                .child(match (&file, target) {
                    (Some(file), LevelTarget::Project) => {
                        format!("The level of the whole project, written to {file}")
                    }
                    (Some(file), LevelTarget::Module) => {
                        format!("The level of this module, written to {file}")
                    }
                    (None, _) => "The build file names no place for the level".to_owned(),
                });
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_XS))
                .child(menu)
                .child(note)
                .into_any_element()
        });
        let summary = summary.map(|summary| {
            div()
                .debug_selector({
                    let prefix = prefix.to_owned();
                    move || format!("{prefix}-summary")
                })
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_secondary)
                .child(summary)
        });
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(form_label(self.label_width, "Language level", theme))
                    .child(field),
            )
            .children(items)
            .children(summary)
    }

    fn project_page(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let name = self.selected_name();
        let dropdown = select_field("project-structure-sdk", name, self.sdk_open, theme)
            .debug_selector(|| "project-structure-sdk".to_owned())
            .flex_1()
            .on_click(cx.listener(|this, _, _, cx| {
                this.sdk_open = !this.sdk_open;
                cx.notify();
            }));

        let items = self.sdk_open.then(|| {
            let mut menu =
                select_menu(theme).debug_selector(|| "project-structure-sdk-menu".to_owned());
            menu = menu.child(
                select_option(
                    "project-structure-sdk-default",
                    SDK_DEFAULT,
                    self.selected.is_none(),
                    self.selected.is_none(),
                    theme,
                )
                .debug_selector(|| "project-structure-sdk-default".to_owned())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.selected = None;
                    this.sdk_open = false;
                    this.error = None;
                    cx.notify();
                })),
            );
            for (index, runtime) in self.runtimes.iter().enumerate() {
                let label = format!("{}  {}", runtime.name, runtime.version);
                let chosen = self.selected.as_deref() == Some(runtime.home.as_path());
                menu = menu.child(
                    select_option(
                        ("project-structure-sdk-item", index),
                        label,
                        chosen,
                        chosen,
                        theme,
                    )
                    .debug_selector(move || format!("project-structure-sdk-item-{index}"))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(index, cx))),
                );
            }
            let add = div()
                .id("project-structure-add-jdk")
                .debug_selector(|| "project-structure-add-jdk".to_owned())
                .flex()
                .items_center()
                .h(px(CONTROL_HEIGHT))
                .px(px(SPACE_M))
                .text_color(theme.text)
                .hover({
                    let hover = theme.hover;
                    move |style| style.bg(hover)
                })
                .child("Add JDK...")
                .on_click(cx.listener(|this, _, window, cx| this.choose_folder(window, cx)));
            div()
                .flex()
                .flex_col()
                .gap(px(SPACE_XS))
                .child(menu.child(add))
                .into_any_element()
        });

        let open = self
            .language
            .as_ref()
            .and_then(|language| language.file.as_ref())
            .map(|_| {
                small_button(
                    "project-structure-open-build",
                    "project-structure-open-build".to_owned(),
                    "Open Build File",
                    theme,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_build_file(window, cx)))
            });
        let level = self.language.clone();
        let level_field = self.language_field(
            "project-structure-language",
            LevelTarget::Project,
            level.as_ref(),
            theme,
            cx,
        );

        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_M))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(form_label(self.label_width, "Project SDK", theme))
                    .child(dropdown),
            )
            .children(items)
            .child(
                div()
                    .debug_selector(|| "project-structure-reason".to_owned())
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child(reason_text(self.reason)),
            )
            .child(level_field)
            .children(open.map(|button| div().flex().child(button)))
            .children(self.details.iter().enumerate().map(|(index, detail)| {
                div()
                    .id(("project-structure-detail", index))
                    .debug_selector(move || format!("project-structure-detail-{index}"))
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .child(form_label(self.label_width, &detail.label, theme))
                    .child(
                        div()
                            .debug_selector(move || {
                                format!("project-structure-detail-value-{index}")
                            })
                            .child(detail.value.clone()),
                    )
            }))
    }

    fn modules_page(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let Some(model) = self.model.clone() else {
            return div()
                .debug_selector(|| "project-structure-modules-empty".to_owned())
                .text_color(theme.text_muted)
                .child("The project model is not ready yet. It appears after the import.");
        };
        let rows = self.module_rows(theme, cx);
        let list = div()
            .w(px(MODULES_WIDTH))
            .flex_none()
            .flex()
            .flex_col()
            .gap(px(SPACE_XS))
            .child(
                div()
                    .text_size(px(TEXT_SMALL))
                    .text_color(theme.text_secondary)
                    .child(format!("{} modules of {}", model.modules.len(), model.tool)),
            )
            .children(rows);
        let details = match self.module() {
            Some(module) => self.module_page(module, theme, cx).into_any_element(),
            None => div()
                .debug_selector(|| "project-structure-module-none".to_owned())
                .text_color(theme.text_muted)
                .child("The project has no modules to show")
                .into_any_element(),
        };
        div()
            .debug_selector(|| "project-structure-modules".to_owned())
            .flex()
            .gap(px(DIALOG_GAP))
            .min_w(px(0.))
            .child(list)
            .child(details)
    }

    fn sdks_page(&self, theme: &Theme, cx: &mut Context<Self>) -> Div {
        let rows = self.runtimes.iter().enumerate().map(|(index, runtime)| {
            let remove = runtime.added.then(|| {
                small_button(
                    ("project-structure-remove", index),
                    format!("project-structure-remove-{index}"),
                    "Remove",
                    theme,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.remove(index, cx)))
            });
            div()
                .id(("project-structure-sdk-row", index))
                .debug_selector(move || format!("project-structure-sdk-row-{index}"))
                .flex()
                .items_center()
                .justify_between()
                .gap(px(SPACE_M))
                .min_h(px(CONTROL_HEIGHT))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w(px(0.))
                        .child(
                            div()
                                .text_color(theme.text)
                                .child(format!("{} {}", runtime.name, runtime.version)),
                        )
                        .child(
                            div()
                                .text_size(px(TEXT_SMALL))
                                .text_color(theme.text_muted)
                                .child(runtime.home.display().to_string()),
                        ),
                )
                .children(remove)
        });
        let add = small_button(
            "project-structure-add",
            "project-structure-add".to_owned(),
            "Add",
            theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.choose_folder(window, cx)));
        div()
            .flex()
            .flex_col()
            .gap(px(SPACE_S))
            .child(
                div()
                    .text_color(theme.text_muted)
                    .child("SDKs of the project. Only the ones added here can be removed."),
            )
            .children(rows)
            .child(add)
    }
}

/// One dependency of a tree of the model, as the Modules page shows it.
struct DependencyRow {
    coordinates: String,
    scope: String,
    depth: usize,
}

fn collect_dependency(node: &Dependency, scope: &str, depth: usize, rows: &mut Vec<DependencyRow>) {
    if depth > MAX_DEPENDENCY_DEPTH {
        return;
    }
    rows.push(DependencyRow {
        coordinates: node.coordinates.clone(),
        scope: scope.to_owned(),
        depth,
    });
    for child in &node.children {
        collect_dependency(child, scope, depth + 1, rows);
    }
}

/// The nearest module the folder of `index` is under.
fn parent_of(modules: &[Module], index: usize) -> Option<usize> {
    let dir = &modules[index].dir;
    let by_dir = modules
        .iter()
        .enumerate()
        .filter(|(other, module)| {
            *other != index
                && !module.dir.as_os_str().is_empty()
                && module.dir != *dir
                && dir.starts_with(&module.dir)
        })
        .max_by_key(|(_, module)| module.dir.components().count())
        .map(|(other, _)| other);
    if by_dir.is_some() {
        return by_dir;
    }
    let (parent, _) = modules[index].id.rsplit_once(':')?;
    (!parent.is_empty())
        .then(|| modules.iter().position(|module| module.id == parent))
        .flatten()
}

/// Every module above `index`, nearest first.
fn ancestors_in(parents: &[Option<usize>], index: usize) -> Vec<usize> {
    let mut out = Vec::new();
    let mut at = index;
    while let Some(parent) = parents.get(at).copied().flatten() {
        if out.contains(&parent) || parent == index {
            break;
        }
        out.push(parent);
        at = parent;
    }
    out
}

fn reason_text(reason: RuntimeReason) -> &'static str {
    match reason {
        RuntimeReason::User => "Chosen in Project Structure",
        RuntimeReason::Project => "Requested by the project",
        RuntimeReason::Default => "The default SDK",
    }
}

/// The levels the field offers: the ones IDEA lists, and the level of the build file when
/// it is not among them, so that the current value stays visible.
fn level_values(info: Option<&LanguageLevelInfo>) -> Vec<String> {
    let mut values: Vec<String> = LANGUAGE_LEVELS
        .iter()
        .map(|level| (*level).to_owned())
        .collect();
    if let Some(selected) = info.and_then(|info| info.selected.as_deref())
        && !values.iter().any(|value| value == selected)
    {
        values.push(selected.to_owned());
    }
    values
}

fn section_row(
    id: &'static str,
    selector: &'static str,
    title: &'static str,
    chosen: bool,
    theme: &Theme,
    cx: &mut Context<ProjectStructureDialog>,
    section: Section,
) -> Stateful<Div> {
    div()
        .id(id)
        .debug_selector(move || selector.to_owned())
        .flex()
        .items_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(ROW_PADDING_X))
        .rounded(px(CONTROL_RADIUS))
        .text_color(if chosen {
            theme.text
        } else {
            theme.text_secondary
        })
        .when(chosen, |this| this.bg(theme.selection))
        .when(!chosen, |this| {
            let hover = theme.hover;
            this.hover(move |style| style.bg(hover))
        })
        .child(title)
        .on_click(cx.listener(move |this, _, _, cx| this.show_section(section, cx)))
}

fn small_button(
    id: impl Into<ElementId>,
    selector: String,
    label: &str,
    theme: &Theme,
) -> Stateful<Div> {
    let (hover, pressed) = (theme.hover, theme.pressed);
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .justify_center()
        .h(px(CONTROL_HEIGHT))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(theme.control_border)
        .text_color(theme.text)
        .hover(move |style| style.bg(hover))
        .active(move |style| style.bg(pressed))
        .child(label.to_owned())
}

impl Render for ProjectStructureDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        // The label column of every page fits the widest label of the pages: the fixed labels,
        // and the labels of the details the language reads.
        let mut labels: Vec<String> = [
            "Sources",
            "Tests",
            "Resources",
            "Generated",
            "Excluded",
            "Language level",
            "Project SDK",
        ]
        .iter()
        .map(|label| label.to_string())
        .collect();
        labels.extend(self.details.iter().map(|detail| detail.label.clone()));
        let refs: Vec<&str> = labels.iter().map(String::as_str).collect();
        self.label_width = form_label_width(window, &refs);
        let viewport = window.viewport_size();
        let width = px(WIDTH);
        let left = (viewport.width - width) / 2.;
        let top = (viewport.height - px(BODY_HEIGHT)) * 0.2;

        let sections = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .w(px(SECTIONS_WIDTH))
            .flex_none()
            .child(section_row(
                "project-structure-section-project",
                "project-structure-section-project",
                "Project",
                self.section == Section::Project,
                &theme,
                cx,
                Section::Project,
            ))
            .child(section_row(
                "project-structure-section-modules",
                "project-structure-section-modules",
                "Modules",
                self.section == Section::Modules,
                &theme,
                cx,
                Section::Modules,
            ))
            .child(section_row(
                "project-structure-section-sdks",
                "project-structure-section-sdks",
                "SDKs",
                self.section == Section::Sdks,
                &theme,
                cx,
                Section::Sdks,
            ));

        let body: AnyElement = match self.section {
            Section::Project => self.project_page(&theme, cx).into_any_element(),
            Section::Modules => self.modules_page(&theme, cx).into_any_element(),
            Section::Sdks => self.sdks_page(&theme, cx).into_any_element(),
        };

        let can_apply = !self.applying && !self.loading && self.error.is_none();
        let ok = dialog_button(
            "project-structure-ok",
            "project-structure-ok".to_owned(),
            "OK",
            true,
            can_apply,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.apply(true, window, cx)));
        let apply = dialog_button(
            "project-structure-apply",
            "project-structure-apply".to_owned(),
            "Apply",
            false,
            can_apply,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.apply(false, window, cx)));
        let cancel = dialog_button(
            "project-structure-cancel",
            "project-structure-cancel".to_owned(),
            "Cancel",
            false,
            true,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx)));

        let error = self.error.clone().map(|error| {
            div()
                .debug_selector(|| "project-structure-error".to_owned())
                .text_color(theme.text)
                .child(error)
        });
        let notice = self.notice.clone().map(|notice| {
            div()
                .debug_selector(|| "project-structure-notice".to_owned())
                .text_size(px(TEXT_SMALL))
                .text_color(theme.text_secondary)
                .child(notice)
        });

        let shell = surface(&theme)
            .id("project-structure")
            .debug_selector(|| "project-structure".to_owned())
            .key_context("ProjectStructure")
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
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .gap(px(DIALOG_GAP))
                    .child(sections)
                    .child(
                        scrollable(
                            "project-structure-body",
                            &self.scroll,
                            div()
                                .id("project-structure-body")
                                .flex()
                                .flex_col()
                                .gap(px(SPACE_M))
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .child(body)
                                .children(error)
                                .children(notice),
                            &theme,
                            window,
                            cx,
                        )
                        .flex_1()
                        .min_w(px(0.)),
                    ),
            )
            .child(
                DialogButtons::new()
                    .primary(ok)
                    .other(cancel)
                    .apply(apply)
                    .into_row(),
            );
        let close = cx.listener(|this, _: &ClickEvent, window, cx| this.close(window, cx));
        dialog_window(
            DialogSpec {
                key: "project-structure",
                title: "Project Structure".into(),
                extra: None,
                place: DialogPlace {
                    left: f32::from(left),
                    top: f32::from(top),
                    width: f32::from(width),
                    height: Some(DIALOG_HEIGHT),
                },
                min: (560., 360.),
            },
            shell,
            window,
            cx,
            close,
            contents,
        )
    }
}

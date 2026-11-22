//! Small popups of the window: the file path, the navigation bar, the scheme switch
//! and the choice of a language for a scratch file.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use editor_view::{EditorView, Spot};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::{Dismiss, UI_SCALE_STEP, Workspace};

use crate::navigation;

const WIDTH: f32 = 520.;
const ROWS_HEIGHT: f32 = 320.;

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(1);

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |name| name.to_string_lossy().into_owned(),
    )
}

/// Ctrl+Alt+F12: the folders of the active file, from the file up to the drive root.
pub struct FilePathPopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    rows: Vec<(String, PathBuf)>,
    selected: usize,
}

pub(crate) fn show_file_path(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(file) = workspace.active_path() else {
        return;
    };
    let rows: Vec<(String, PathBuf)> = file
        .ancestors()
        .map(|path| (path.display().to_string(), path.to_path_buf()))
        .collect();
    if rows.is_empty() {
        return;
    }
    // The folder of the file is the useful place to open first.
    let selected = usize::from(rows.len() > 1);
    let weak = cx.weak_entity();
    let view = cx.new(|cx| FilePathPopup {
        workspace: weak,
        focus: cx.focus_handle(),
        rows,
        selected,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

impl FilePathPopup {
    fn pick(&mut self, index: usize, path: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = index;
        ui_kit::reveal_path(cx, &path);
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "up" | "left" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                cx.notify();
            }
            "down" | "right" => {
                self.selected = (self.selected + 1).min(self.rows.len() - 1);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                if let Some((_, path)) = self.rows.get(self.selected) {
                    ui_kit::reveal_path(cx, path);
                }
                self.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
                cx.stop_propagation();
            }
            "escape" => {
                self.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for FilePathPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.2;
        let rows: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, (label, path))| {
                let selected = index == self.selected;
                let path = path.clone();
                div()
                    .id(("file-path-row", index))
                    .debug_selector(move || format!("file-path-row-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(SharedString::from(label.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick(index, path.clone(), window, cx)
                    }))
            })
            .collect();
        let body = surface(&theme)
            .id("file-path-popup")
            .debug_selector(|| "file-path-popup".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_h(px(ROWS_HEIGHT))
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
            }))
            .child(
                div()
                    .px(px(ROW_PADDING_X))
                    .h(px(CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("File Path"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .overflow_hidden()
                    .children(rows),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("file-path-enter", body))
    }
}

/// One step of the navigation bar: a folder, the file, or a declaration of the file.
#[derive(Clone)]
struct NavRow {
    label: String,
    path: PathBuf,
    offset: Option<usize>,
    folder: bool,
}

/// Alt+Home: the chain of packages, the file, and the declaration at the caret.
pub struct NavBarPopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    rows: Vec<NavRow>,
    selected: usize,
}

pub(crate) fn show_nav_bar(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(file) = workspace.active_path() else {
        return;
    };
    let mut rows: Vec<NavRow> = file
        .ancestors()
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .map(|path| NavRow {
            label: file_name(path),
            path: path.to_path_buf(),
            offset: None,
            folder: path != file,
        })
        .collect();
    if let Some(view) = workspace
        .view_of(&file)
        .and_then(|view| view.downcast::<EditorView>().ok())
    {
        let view = view.read(cx);
        let caret = view.head_offset().unwrap_or(0);
        for item in view.outline() {
            if item.range.contains(&caret) {
                rows.push(NavRow {
                    label: item.name.clone(),
                    path: file.clone(),
                    offset: Some(item.range.start),
                    folder: false,
                });
            }
        }
    }
    if rows.is_empty() {
        return;
    }
    let selected = rows.len() - 1;
    let weak = cx.weak_entity();
    let view = cx.new(|cx| NavBarPopup {
        workspace: weak,
        focus: cx.focus_handle(),
        rows,
        selected,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

impl NavBarPopup {
    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };
        self.selected = index;
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                if row.folder {
                    ui_kit::reveal_path(cx, &row.path);
                } else if let Some(offset) = row.offset {
                    navigation::go_to(
                        workspace,
                        row.path.clone(),
                        Some(Spot::Offset(offset)),
                        false,
                        window,
                        cx,
                    );
                } else {
                    workspace.open_file(row.path.clone(), window, cx);
                }
            })
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "left" | "up" => {
                self.selected = self.selected.saturating_sub(1);
                cx.stop_propagation();
                cx.notify();
            }
            "right" | "down" => {
                self.selected = (self.selected + 1).min(self.rows.len() - 1);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                self.pick(self.selected, window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for NavBarPopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.12;
        let rows: Vec<_> = self
            .rows
            .iter()
            .enumerate()
            .map(|(index, row)| {
                let selected = index == self.selected;
                div()
                    .id(("nav-bar-row", index))
                    .debug_selector(move || format!("nav-bar-row-{index}"))
                    .flex()
                    .items_center()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X + SPACE_L * row.folder as u32 as f32))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(SharedString::from(row.label.clone()))
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(index, window, cx)))
            })
            .collect();
        let body = surface(&theme)
            .id("nav-bar-popup")
            .debug_selector(|| "nav-bar-popup".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_h(px(ROWS_HEIGHT))
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
            }))
            .child(
                div()
                    .px(px(ROW_PADDING_X))
                    .h(px(CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("Navigation Bar"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_h_0()
                    .overflow_hidden()
                    .children(rows),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("nav-bar-enter", body))
    }
}

/// Ctrl+Backquote: a scheme of the window. The first rows are the theme, the keymap and
/// the scale, then every theme of the catalog. Arrows show a theme at once, Enter keeps
/// it, Esc brings the theme of the open back.
pub struct ChangeSchemePopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    selected: usize,
    scale: f32,
    /// The theme the window showed when the list opened.
    original: Theme,
    original_name: String,
    themes: Vec<ThemeRow>,
}

/// One theme of the list, with the colours its row previews.
struct ThemeRow {
    id: String,
    name: String,
    theme: Theme,
}

/// The rows before the themes: the theme of the window, the keymap and the scale.
const SCHEME_ROWS: usize = 3;

pub(crate) fn show_change_scheme(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let weak = cx.weak_entity();
    let original = *cx.global::<Theme>();
    let scale = workspace.ui_scale(cx);
    let library = ThemeCatalog::library_of(cx);
    let original_name = library.get(original.id()).map_or_else(
        || Theme::label(original.id()),
        |definition| definition.name.clone(),
    );
    let themes = library
        .themes()
        .iter()
        .map(|definition| ThemeRow {
            id: definition.id.clone(),
            name: definition.name.clone(),
            theme: Theme::from_definition(definition),
        })
        .collect();
    let view = cx.new(|cx| ChangeSchemePopup {
        workspace: weak,
        focus: cx.focus_handle(),
        selected: 0,
        scale,
        original,
        original_name,
        themes,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

impl ChangeSchemePopup {
    /// The value of one of the three rows before the themes.
    fn value(&self, index: usize) -> String {
        match index {
            0 => self.original_name.clone(),
            1 => "IntelliJ IDEA (Windows)".to_owned(),
            _ => format!("{}%", (self.scale * 100.).round()),
        }
    }

    /// The title of one of the three rows before the themes.
    fn title(index: usize) -> &'static str {
        match index {
            0 => "Theme",
            1 => "Keymap",
            _ => "Scale",
        }
    }

    /// The theme the list opened with, for a click outside it.
    fn theme_of_open(&self) -> Theme {
        self.original
    }

    /// The theme under the selection, when a theme row has it.
    fn theme_at(&self, index: usize) -> Option<&ThemeRow> {
        self.themes.get(index.checked_sub(SCHEME_ROWS)?)
    }

    /// Shows the theme under the selection, or the theme of the open on the rows above
    /// the themes. Nothing is saved.
    fn preview(&mut self, window: &Window, cx: &mut Context<Self>) {
        let _ = window;
        let original = self.original;
        let id = self.theme_at(self.selected).map(|row| row.id.clone());
        self.workspace
            .update(cx, |workspace, cx| match id {
                Some(id) => workspace.preview_theme(&id, cx),
                None => workspace.restore_theme(original, cx),
            })
            .ok();
    }

    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        self.selected = index;
        let theme = self.theme_at(index).map(|row| (row.id.clone(), row.theme));
        self.workspace
            .update(cx, |workspace, cx| {
                match index {
                    0 => workspace.toggle_theme(cx),
                    1 => workspace.notify_user(
                        "Keymap",
                        "IntelliJ IDEA (Windows) is the only keymap",
                        cx,
                    ),
                    2 => workspace.zoom_ide(UI_SCALE_STEP, cx),
                    _ => {
                        if let Some((id, _)) = &theme {
                            workspace.choose_theme(id, cx);
                        }
                    }
                }
                workspace.dismiss(window, cx);
            })
            .ok();
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let last = SCHEME_ROWS + self.themes.len().saturating_sub(1);
        match event.keystroke.key.as_str() {
            "up" | "left" => {
                self.selected = self.selected.saturating_sub(1);
                self.preview(window, cx);
                cx.stop_propagation();
                cx.notify();
            }
            "down" | "right" => {
                self.selected = (self.selected + 1).min(last);
                self.preview(window, cx);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                self.pick(self.selected, window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                let original = self.original;
                self.workspace
                    .update(cx, |workspace, cx| {
                        workspace.restore_theme(original, cx);
                        workspace.dismiss(window, cx);
                    })
                    .ok();
                cx.stop_propagation();
            }
            _ => {}
        }
    }
}

impl Render for ChangeSchemePopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.18;
        let mut rows: Vec<AnyElement> = (0..SCHEME_ROWS)
            .map(|index| {
                let selected = index == self.selected;
                div()
                    .id(("scheme-row", index))
                    .debug_selector(move || format!("scheme-row-{index}"))
                    .flex()
                    .items_center()
                    .justify_between()
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(Self::title(index))
                    .child(
                        div()
                            .text_color(theme.text_secondary)
                            .child(SharedString::from(self.value(index))),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| this.pick(index, window, cx)))
                    .into_any_element()
            })
            .collect();
        for (index, row) in self.themes.iter().enumerate() {
            let selected = SCHEME_ROWS + index == self.selected;
            let current = row.theme.id() == theme.id();
            let row_theme = row.theme;
            rows.push(
                div()
                    .id(SharedString::from(format!("scheme-theme-{}", row.id)))
                    .debug_selector({
                        let id = row.id.clone();
                        move || format!("scheme-theme-{}", id)
                    })
                    .flex()
                    .items_center()
                    .gap(px(SPACE_M))
                    .flex_none()
                    .h(px(ROW_HEIGHT))
                    .px(px(ROW_PADDING_X))
                    .rounded(px(ROW_RADIUS))
                    .when(selected, |this| this.bg(theme.selection))
                    .when(!selected, |this| {
                        this.hover(|style| style.bg(theme.selection_hover))
                    })
                    .child(
                        div()
                            .flex()
                            .gap(px(SPACE_XS))
                            .child(swatch(row_theme.main_bg))
                            .child(swatch(row_theme.text))
                            .child(swatch(row_theme.accent)),
                    )
                    .child(SharedString::from(row.name.clone()))
                    .when(current, |this| {
                        this.child(div().text_color(theme.accent).child("\u{2713}"))
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.pick(SCHEME_ROWS + index, window, cx)
                    }))
                    .into_any_element(),
            );
        }
        let body = surface(&theme)
            .id("scheme-popup")
            .debug_selector(|| "scheme-popup".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_h(px(ROWS_HEIGHT + ROW_HEIGHT * (SCHEME_ROWS as f32)))
            .overflow_hidden()
            .p(px(POPUP_PADDING))
            .capture_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
                let original = this.theme_of_open();
                this.workspace
                    .update(cx, |workspace, cx| {
                        workspace.restore_theme(original, cx);
                        workspace.dismiss(window, cx);
                    })
                    .ok();
                cx.stop_propagation();
            }))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                let original = this.theme_of_open();
                this.workspace
                    .update(cx, |workspace, cx| {
                        workspace.restore_theme(original, cx);
                        workspace.dismiss(window, cx);
                    })
                    .ok();
            }))
            .child(
                div()
                    .px(px(ROW_PADDING_X))
                    .h(px(CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("Scheme"),
            )
            .children(rows);
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("scheme-enter", body))
    }
}

/// A little colour strip of a theme row.
fn swatch(color: Hsla) -> Div {
    div().w(px(14.)).h(px(10.)).rounded(px(2.)).bg(color)
}

/// A type of file the New Scratch File menu offers: the extension of a new file and the
/// name used when no language resolves it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ScratchFileType {
    pub extension: &'static str,
    pub label: &'static str,
}

/// The file types the application adds to the neutral formats of the editor, such as the
/// language it knows itself.
#[derive(Clone, Debug, Default)]
pub struct ScratchFileTypes(pub Vec<ScratchFileType>);

impl Global for ScratchFileTypes {}

/// The formats of the editor with the extension of a new file and the name a type shows
/// when no language resolves it. The application adds the types it knows itself.
const SCRATCH_TYPES: &[ScratchFileType] = &[
    scratch_file("kt", "Kotlin"),
    scratch_file("groovy", "Groovy"),
    scratch_file("json", "JSON"),
    scratch_file("xml", "XML"),
    scratch_file("yaml", "YAML"),
    scratch_file("properties", "Properties"),
    scratch_file("md", "Markdown"),
    scratch_file("sql", "SQL"),
    scratch_file("http", "HTTP Request"),
    scratch_file("sh", "Shell Script"),
    scratch_file("txt", "Plain text"),
    scratch_file("toml", "TOML"),
    scratch_file("html", "HTML"),
    scratch_file("css", "CSS"),
    scratch_file("js", "JavaScript"),
    scratch_file("ts", "TypeScript"),
    scratch_file("py", "Python"),
    scratch_file("c", "C"),
    scratch_file("cpp", "C++"),
    scratch_file("cs", "C#"),
    scratch_file("go", "Go"),
    scratch_file("rs", "Rust"),
    scratch_file("scala", "Scala"),
    scratch_file("php", "PHP"),
    scratch_file("rb", "Ruby"),
    scratch_file("swift", "Swift"),
    scratch_file("lua", "Lua"),
    scratch_file("ps1", "PowerShell"),
    scratch_file("proto", "Protobuf"),
    scratch_file("ini", "INI"),
    scratch_file("diff", "Diff"),
    scratch_file("mk", "Makefile"),
    scratch_file("bat", "Windows Batch"),
    scratch_file("dockerfile", "Dockerfile"),
];

/// One row of the built-in list of the menu.
const fn scratch_file(extension: &'static str, label: &'static str) -> ScratchFileType {
    ScratchFileType { extension, label }
}

/// The key of the state that remembers the last type of a scratch file.
const SCRATCH_LAST: &str = "scratch-language";

/// The rows the menu shows before the list scrolls.
const SCRATCH_SHOWN: usize = 10;

/// One row of the New Scratch File menu.
struct ScratchType {
    label: String,
    extension: &'static str,
    icon: &'static str,
}

/// The name a language shows in the menu, where it spells it differently from the editor.
fn scratch_name(name: &str) -> &str {
    match name {
        "Shell" => "Shell Script",
        name => name,
    }
}

/// The types with the name and the icon of their language, by name. The types of the
/// application come after the formats of the editor.
fn scratch_types(workspace: &Workspace, cx: &App) -> Vec<ScratchType> {
    let languages = cx.try_global::<editor_view::Languages>();
    let extra = cx
        .try_global::<ScratchFileTypes>()
        .map_or(&[][..], |types| types.0.as_slice());
    let mut known: Vec<&'static str> = Vec::new();
    let mut types = Vec::new();
    for file in SCRATCH_TYPES.iter().chain(extra) {
        if known.contains(&file.extension) {
            continue;
        }
        known.push(file.extension);
        let sample = PathBuf::from(format!("scratch.{}", file.extension));
        let label = languages
            .and_then(|languages| (languages.resolve)(&sample))
            .map_or(file.label, |config| scratch_name(config.name))
            .to_owned();
        types.push(ScratchType {
            label,
            extension: file.extension,
            icon: workspace.icons().icon_for(&sample),
        });
    }
    types.sort_by_cached_key(|kind| kind.label.to_lowercase());
    types
}

/// The name of the language of `path`, as the menu spells it.
fn scratch_name_of(
    path: &Path,
    languages: Option<&editor_view::Languages>,
    types: &[ScratchType],
) -> Option<String> {
    if let Some(languages) = languages
        && let Some(config) = (languages.resolve)(path)
    {
        return Some(scratch_name(config.name).to_owned());
    }
    let extension = path.extension()?.to_str()?.to_ascii_lowercase();
    let extension = match extension.as_str() {
        "rest" => "http",
        "yml" => "yaml",
        "markdown" => "md",
        "kts" => "kt",
        "bash" => "sh",
        "htm" => "html",
        "jsonc" => "json",
        "mjs" | "cjs" => "js",
        "pyi" => "py",
        "psm1" => "ps1",
        other => other,
    };
    types
        .iter()
        .find(|kind| kind.extension == extension)
        .map(|kind| kind.label.clone())
}

/// Ctrl+Alt+Shift+Insert: the language of a new scratch file. The language of the active
/// editor comes first, then the one chosen last time, then the rest by name.
pub struct ScratchLanguagePopup {
    workspace: WeakEntity<Workspace>,
    focus: FocusHandle,
    types: Vec<ScratchType>,
    /// The rows the query keeps, as indices of `types`.
    visible: Vec<usize>,
    query: TextField,
    scroll: ScrollHandle,
    selected: usize,
}

pub(crate) fn show_scratch_languages(
    workspace: &mut Workspace,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let languages = cx.try_global::<editor_view::Languages>();
    let mut types = scratch_types(workspace, cx);
    let active = workspace
        .active_path()
        .and_then(|path| scratch_name_of(&path, languages, &types))
        .unwrap_or_else(|| "Plain text".to_owned());
    let last = workspace
        .store()
        .and_then(|store| store.get(SCRATCH_LAST).ok().flatten());
    let mut ordered = Vec::with_capacity(types.len());
    let mut take = |matching: &dyn Fn(&ScratchType) -> bool| {
        if let Some(index) = types.iter().position(matching) {
            ordered.push(types.remove(index));
        }
    };
    take(&|kind| kind.label == active);
    if let Some(last) = last.as_deref() {
        take(&|kind| kind.extension == last);
    }
    ordered.extend(types);
    let visible = (0..ordered.len()).collect();
    let weak = cx.weak_entity();
    let view = cx.new(|cx| ScratchLanguagePopup {
        workspace: weak,
        focus: cx.focus_handle(),
        types: ordered,
        visible,
        query: TextField::default(),
        scroll: ScrollHandle::new(),
        selected: 0,
    });
    let focus = view.read(cx).focus.clone();
    workspace.show_overlay(view.into(), focus, window, cx);
}

impl ScratchLanguagePopup {
    fn pick(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        let Some(kind) = self
            .visible
            .get(index)
            .and_then(|index| self.types.get(*index))
        else {
            return;
        };
        let extension = kind.extension;
        self.selected = index;
        let serial = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
        self.workspace
            .update(cx, |workspace, cx| {
                workspace.dismiss(window, cx);
                let dir = workspace.state_dir().join("scratches");
                let path = dir.join(format!("scratch_{serial}.{extension}"));
                let store = workspace.store();
                let write = cx.background_executor().spawn(async move {
                    if let Some(store) = store {
                        let _ = store.put(SCRATCH_LAST, extension);
                    }
                    std::fs::create_dir_all(&dir)?;
                    std::fs::write(&path, "")?;
                    Ok::<PathBuf, std::io::Error>(path)
                });
                cx.spawn_in(window, async move |workspace, cx| {
                    let Ok(path) = write.await else {
                        return;
                    };
                    workspace
                        .update_in(cx, |workspace, window, cx| {
                            workspace.open_file(path, window, cx)
                        })
                        .ok();
                })
                .detach();
            })
            .ok();
    }

    /// The rows the query keeps, the first one selected.
    fn filter(&mut self) {
        let query = self.query.text.to_lowercase();
        self.visible = self
            .types
            .iter()
            .enumerate()
            .filter(|(_, kind)| query.is_empty() || kind.label.to_lowercase().contains(&query))
            .map(|(index, _)| index)
            .collect();
        self.selected = 0;
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match key {
            "up" => {
                self.selected = self.selected.saturating_sub(1);
                self.scroll.scroll_to_item(self.selected);
                cx.stop_propagation();
                cx.notify();
            }
            "down" => {
                self.selected = (self.selected + 1).min(self.visible.len().saturating_sub(1));
                self.scroll.scroll_to_item(self.selected);
                cx.stop_propagation();
                cx.notify();
            }
            "enter" => {
                self.pick(self.selected, window, cx);
                cx.stop_propagation();
            }
            "escape" => {
                self.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
                cx.stop_propagation();
            }
            _ => {
                if self.query.key_with_clipboard(event, false, cx) {
                    self.filter();
                    cx.stop_propagation();
                    cx.notify();
                }
            }
        }
    }

    /// The names of the rows the query keeps, for the tests.
    #[cfg(test)]
    pub(crate) fn labels(&self) -> Vec<String> {
        self.visible
            .iter()
            .filter_map(|index| self.types.get(*index))
            .map(|kind| kind.label.clone())
            .collect()
    }
}

impl Render for ScratchLanguagePopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let viewport = window.viewport_size();
        let left = (viewport.width - px(WIDTH)) / 2.;
        let top = viewport.height * 0.18;
        let rows: Vec<_> = self
            .visible
            .iter()
            .enumerate()
            .filter_map(|(index, type_index)| {
                let kind = self.types.get(*type_index)?;
                let selected = index == self.selected;
                let label = kind.label.clone();
                let file_icon = kind.icon;
                Some(
                    div()
                        .id(("scratch-language-row", index))
                        .debug_selector(move || format!("scratch-language-row-{index}"))
                        .flex()
                        .items_center()
                        .gap(px(SPACE_M))
                        .flex_none()
                        .h(px(ROW_HEIGHT))
                        .px(px(ROW_PADDING_X))
                        .rounded(px(ROW_RADIUS))
                        .when(selected, |this| this.bg(theme.selection))
                        .when(!selected, |this| {
                            this.hover(|style| style.bg(theme.selection_hover))
                        })
                        .child(icon(file_icon, &theme))
                        .child(SharedString::from(label))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.pick(index, window, cx)),
                        ),
                )
            })
            .collect();
        let list_height = self.visible.len().min(SCRATCH_SHOWN) as f32 * ROW_HEIGHT;
        let body = surface(&theme)
            .id("scratch-language-popup")
            .debug_selector(|| "scratch-language-popup".to_owned())
            .track_focus(&self.focus)
            .occlude()
            .w(px(WIDTH))
            .max_h(px(ROWS_HEIGHT))
            .p(px(POPUP_PADDING))
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.dismiss(window, cx))
                    .ok();
            }))
            .child(
                div()
                    .px(px(ROW_PADDING_X))
                    .h(px(CONTROL_HEIGHT))
                    .flex()
                    .items_center()
                    .text_color(theme.text_secondary)
                    .child("New Scratch File"),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .flex_none()
                    .debug_selector(|| "scratch-language-input".to_owned())
                    .gap(px(SPACE_S))
                    .h(px(CONTROL_HEIGHT))
                    .mb(px(POPUP_PADDING))
                    .px(px(SPACE_M))
                    .rounded(px(CONTROL_RADIUS))
                    .border_2()
                    .border_color(theme.accent)
                    .child(icon("general/search", &theme))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .children(text_field_children(
                                FieldMouse::new(cx, |this| Some(&mut this.query)),
                                &self.query,
                                theme.text,
                                theme.text,
                                theme.selection,
                                theme.caret,
                                18.,
                                true,
                            )),
                    ),
            )
            .child(
                scrollable(
                    "scratch-language-rows",
                    &self.scroll,
                    div()
                        .id("scratch-language-rows")
                        .flex()
                        .flex_col()
                        .overflow_y_scroll()
                        .track_scroll(&self.scroll)
                        .children(rows),
                    &theme,
                    window,
                    cx,
                )
                .h(px(list_height))
                .min_h_0(),
            );
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("scratch-language-enter", body))
    }
}

/// The editor of the active tab, when it has one.
fn active_editor(workspace: &Workspace, _cx: &App) -> Option<Entity<EditorView>> {
    workspace.active_view()?.downcast::<EditorView>().ok()
}

/// The menu of the editor at the caret, for the Menu key.
pub(crate) fn show_editor_menu(
    workspace: &mut Workspace,
    _window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    let Some(view) = active_editor(workspace, cx) else {
        return;
    };
    let position = view.read(cx).context_menu_position(cx);
    let model = crate::editor_menu::MenuModel::of(workspace);
    let weak = cx.weak_entity();
    crate::editor_menu::show_now(workspace, view, weak, model, position, cx);
}

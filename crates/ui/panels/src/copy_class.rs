//! Copy Class of the project tree: the name and the folder of a copy, and the text the
//! language makes for that place. The neutral code knows no language: a language that
//! gives no name to a file keeps the plain copy of the tree.

use std::path::{Path, PathBuf};
use std::rc::Rc;

use editor_view::{EditorView, Semantic};
use ui_kit::metrics::*;
use ui_kit::prelude::*;
use ui_kit::*;
use workspace::Workspace;

use crate::prompt::TextField;
use crate::tree_ops::check_name;

const NO_LANGUAGE: &str = "The language gives no name to this file";

type Submit = Rc<dyn Fn(String, PathBuf, &mut App)>;

/// Which field takes the typed text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Field {
    Name,
    Destination,
}

/// The Copy Class window: the name of the copy, the package or folder it goes into, and
/// the text the language makes for the new place.
pub(crate) struct CopyClassDialog {
    title: SharedString,
    source: PathBuf,
    root: PathBuf,
    name: TextField,
    destination: TextField,
    active: Field,
    error: Option<SharedString>,
    focus: FocusHandle,
    workspace: WeakEntity<Workspace>,
    submit: Submit,
}

/// The text of a file of the window: the buffer when it is open and has unsaved edits,
/// and the disk otherwise.
pub(crate) fn source_text(workspace: &Workspace, path: &Path, cx: &App) -> Option<String> {
    let open = workspace
        .view_of(path)
        .and_then(|view| view.downcast::<EditorView>().ok());
    if let Some(view) = open {
        let view = view.read(cx);
        if view.is_dirty()
            && let Some(text) = view.text()
        {
            return Some(text);
        }
    }
    std::fs::read_to_string(path).ok()
}

/// The name a copy of `stem` gets in `folder`: the same name when it is free, else
/// `Name2`, `Name3` and so on, as IDEA offers.
pub(crate) fn default_name(
    folder: &Path,
    stem: &str,
    extension: Option<&std::ffi::OsStr>,
) -> String {
    let exists = |name: &str| {
        let mut file = folder.join(name);
        if let Some(extension) = extension {
            file.set_extension(extension);
        }
        file.exists()
    };
    if !exists(stem) {
        return stem.to_owned();
    }
    for serial in 2..=10_000 {
        let name = format!("{stem}{serial}");
        if !exists(&name) {
            return name;
        }
    }
    format!("{stem}Copy")
}

/// The file name of a copy of `source` named `name`.
pub(crate) fn file_name(source: &Path, name: &str) -> String {
    match source.extension() {
        Some(extension) if Path::new(name).extension().is_none() => {
            format!("{name}.{}", extension.to_string_lossy())
        }
        _ => name.to_owned(),
    }
}

/// The text of a copy named `name` that goes into `folder` of the project.
fn copy_text(
    support: Option<&Semantic>,
    source: &Path,
    text: &str,
    name: &str,
    root: &Path,
    folder: &Path,
) -> String {
    let Some(support) = support.filter(|support| support.0.matches(source)) else {
        return text.to_owned();
    };
    support
        .0
        .copy_element_in_folder(text, name, root, folder)
        .or_else(|| support.0.copy_element_text(text, name))
        .unwrap_or_else(|| text.to_owned())
}

/// Whether the language of `source` names its declaration, so the copy goes through the
/// Copy Class window rather than a plain file copy.
pub(crate) fn knows_declaration(
    support: Option<&Semantic>,
    source: &Path,
    text: &str,
    stem: &str,
) -> bool {
    support
        .filter(|support| support.0.matches(source))
        .and_then(|support| support.0.copy_element_text(text, stem))
        .is_some()
}

/// Writes a copy of `source` named `name` into `folder`. Blocks.
pub(crate) fn copy_file(
    workspace: &Workspace,
    source: &Path,
    name: &str,
    folder: &Path,
    cx: &App,
) -> Result<PathBuf, String> {
    let name = name.trim();
    check_name(name)?;
    if name.contains(['/', '\\']) {
        return Err("A class name has no slashes".into());
    }
    if !folder.is_dir() {
        return Err(format!("{} is not a folder", folder.display()));
    }
    let target = folder.join(file_name(source, name));
    if target.exists() {
        return Err(format!(
            "{} already exists",
            target.file_name().unwrap_or_default().to_string_lossy()
        ));
    }
    let text = source_text(workspace, source, cx).ok_or(NO_LANGUAGE)?;
    let support = cx.try_global::<Semantic>();
    let root = workspace.root();
    let text = copy_text(support, source, &text, name, root, folder);
    write_new(&target, &text)?;
    Ok(target)
}

/// Moves `source` to `folder` under `name` with its declaration and package changed by
/// the language, for a file whose language has no server that updates the references.
pub(crate) fn move_file(
    workspace: &Workspace,
    source: &Path,
    name: &str,
    folder: &Path,
    cx: &App,
) -> Result<PathBuf, String> {
    let target = copy_file(workspace, source, name, folder, cx)?;
    std::fs::remove_file(source).map_err(|error| format!("{}: {error}", source.display()))?;
    Ok(target)
}

fn write_new(target: &Path, text: &str) -> Result<(), String> {
    let written = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(target)
        .and_then(|mut file| std::io::Write::write_all(&mut file, text.as_bytes()));
    written.map_err(|error| error.to_string())
}

impl CopyClassDialog {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn show(
        source: &Path,
        folder: &Path,
        default_name: &str,
        cut: bool,
        submit: impl Fn(String, PathBuf, &mut App) + 'static,
        workspace: &mut Workspace,
        window: &mut Window,
        cx: &mut Context<Workspace>,
    ) {
        let root = workspace.root().to_path_buf();
        let destination = match folder.strip_prefix(&root) {
            // The root itself is the empty relative path, which no field shows well.
            Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
            Ok(relative) => relative.to_string_lossy().replace('\\', "/"),
            Err(_) => folder.to_string_lossy().replace('\\', "/"),
        };
        let title = if cut { "Move Class" } else { "Copy Class" };
        let weak = cx.weak_entity();
        let view = cx.new(|cx| Self {
            title: title.into(),
            source: source.to_path_buf(),
            root,
            name: TextField::from_text(default_name.to_owned()),
            destination: TextField::from_text(destination),
            active: Field::Name,
            error: None,
            focus: cx.focus_handle(),
            workspace: weak,
            submit: Rc::new(submit),
        });
        let focus = view.read(cx).focus.clone();
        workspace.show_overlay(view.into(), focus, window, cx);
    }

    /// The folder the destination field names, or the reason it names none.
    fn folder(&self) -> Result<PathBuf, String> {
        let text = self.destination.text.trim();
        if text.is_empty() {
            return Err("The destination is empty".into());
        }
        let folder = PathBuf::from(text);
        let folder = if folder.is_absolute() {
            folder
        } else {
            self.root.join(folder)
        };
        if !folder.is_dir() {
            return Err(format!("{} is not a folder", folder.display()));
        }
        Ok(folder)
    }

    fn finish(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.text.trim().to_owned();
        if let Err(error) = check_name(&name) {
            self.error = Some(error.into());
            cx.notify();
            return;
        }
        if name.contains(['/', '\\']) {
            self.error = Some("A class name has no slashes".into());
            cx.notify();
            return;
        }
        let folder = match self.folder() {
            Ok(folder) => folder,
            Err(error) => {
                self.error = Some(error.into());
                cx.notify();
                return;
            }
        };
        let target = folder.join(file_name(&self.source, &name));
        if target.exists() {
            self.error = Some(
                format!(
                    "{} already exists",
                    target.file_name().unwrap_or_default().to_string_lossy()
                )
                .into(),
            );
            cx.notify();
            return;
        }
        (self.submit)(name, folder, cx);
        self.close(window, cx);
    }

    fn key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if key == "escape" {
            self.close(window, cx);
            cx.stop_propagation();
            return;
        }
        if key == "enter" {
            self.finish(window, cx);
            cx.stop_propagation();
            return;
        }
        if key == "tab" && !event.keystroke.modifiers.control {
            self.active = match (self.active, event.keystroke.modifiers.shift) {
                (Field::Name, false) | (Field::Destination, true) => Field::Destination,
                _ => Field::Name,
            };
            window.focus(&self.focus, cx);
            cx.notify();
            cx.stop_propagation();
            return;
        }
        let field = match self.active {
            Field::Name => &mut self.name,
            Field::Destination => &mut self.destination,
        };
        if field.key_with_clipboard(event, false, cx) {
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.workspace
            .update(cx, |workspace, cx| workspace.dismiss(window, cx))
            .ok();
    }
}

impl Render for CopyClassDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.global::<Theme>();
        let width = px(460.);
        let left = (window.viewport_size().width - width) / 2.;
        let top = window.viewport_size().height * 0.22;
        let active = self.active;
        let name = field_box(
            "copy-class-name-field",
            "copy-class-name".to_owned(),
            &self.name.text,
            active == Field::Name,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.active = Field::Name;
            window.focus(&this.focus, cx);
            cx.notify();
        }));
        let destination = field_box(
            "copy-class-destination-field",
            "copy-class-destination".to_owned(),
            &self.destination.text,
            active == Field::Destination,
            &theme,
        )
        .on_click(cx.listener(|this, _, window, cx| {
            this.active = Field::Destination;
            window.focus(&this.focus, cx);
            cx.notify();
        }));
        let error = self.error.clone().map(|error| {
            div()
                .debug_selector(|| "copy-class-error".to_owned())
                .text_color(theme.text)
                .child(error)
                .into_any_element()
        });
        let body = surface(&theme)
            .id("copy-class")
            .debug_selector(|| "copy-class".to_owned())
            .key_context("CopyClass")
            .track_focus(&self.focus)
            .occlude()
            .w(width)
            .p(px(DIALOG_PADDING))
            .gap(px(DIALOG_GAP))
            .text_size(px(UI_SIZE))
            .text_color(theme.text)
            .on_key_down(cx.listener(|this, event, window, cx| this.key(event, window, cx)))
            .on_mouse_down_out(cx.listener(|this, _, window, cx| this.close(window, cx)))
            .child(
                div()
                    .text_size(px(TEXT_TITLE))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.title.clone()),
            )
            .child(labelled("Name", name, &theme))
            .child(labelled(
                "Destination package or folder",
                destination,
                &theme,
            ))
            .children(error);
        div()
            .absolute()
            .left(left)
            .top(top)
            .child(enter("copy-class-enter", body))
    }
}

fn labelled(label: &str, field: Stateful<Div>, theme: &Theme) -> Div {
    div()
        .flex()
        .flex_col()
        .gap(px(SPACE_S))
        .child(
            div()
                .text_size(px(UI_SIZE))
                .text_color(theme.text_secondary)
                .child(label.to_owned()),
        )
        .child(field)
}

fn field_box(
    id: impl Into<ElementId>,
    selector: String,
    text: &str,
    active: bool,
    theme: &Theme,
) -> Stateful<Div> {
    let color = if active { theme.text } else { theme.text_muted };
    div()
        .id(id)
        .debug_selector(move || selector)
        .flex()
        .items_center()
        .h(px(30.))
        .px(px(SPACE_M))
        .rounded(px(CONTROL_RADIUS))
        .border_1()
        .border_color(if active {
            theme.accent
        } else {
            theme.control_border
        })
        .text_color(color)
        .child(text.to_owned())
}

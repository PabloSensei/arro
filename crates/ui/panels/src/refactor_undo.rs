//! One step of the window's history for a refactoring that reached several files: an
//! open buffer goes back through its own undo, a closed file is written back, a new file
//! is taken away. Ctrl+Shift+Z applies all of it again.

use std::path::PathBuf;

use editor_view::EditorView;
use ui_kit::*;
use workspace::Workspace;

use crate::changes::{WrittenFile, open_view};

/// The refactoring of several files as the window keeps it.
pub(crate) struct Group {
    /// The file whose Ctrl+Z takes the whole change back.
    pub origin: Option<PathBuf>,
    /// The open files the change applied to.
    pub open: Vec<PathBuf>,
    /// The closed files it wrote.
    pub closed: Vec<WrittenFile>,
}

struct Stored {
    id: u64,
    group: Group,
}

/// The steps of the window's history that reach several files.
#[derive(Default)]
pub struct RefactoringUndo {
    next: u64,
    undo: Vec<Stored>,
    redo: Vec<Stored>,
}

impl Global for RefactoringUndo {}

/// Puts the change on the history of the window and tells the file it started in that
/// Ctrl+Z belongs to that step now.
pub(crate) fn push(
    workspace: &mut Workspace,
    group: Group,
    origin: Option<Entity<EditorView>>,
    cx: &mut Context<Workspace>,
) {
    let origin_path = group.origin.clone();
    let id = {
        let history = cx.default_global::<RefactoringUndo>();
        history.next += 1;
        let id = history.next;
        history.redo.clear();
        history.undo.push(Stored { id, group });
        id
    };
    let origin = origin.or_else(|| {
        let path = origin_path?;
        open_view(workspace, &path)
    });
    if let Some(view) = origin {
        view.update(cx, |view, _| {
            view.set_workspace_undo(Some(id));
            view.set_workspace_redo(None);
        });
    }
}

/// Takes the whole change back: open files through their own undo, closed files from the
/// text they held before, a file the change created away.
pub(crate) fn undo(workspace: &mut Workspace, id: u64, cx: &mut Context<Workspace>) {
    let stored = {
        let history = cx.default_global::<RefactoringUndo>();
        let Some(at) = history.undo.iter().position(|stored| stored.id == id) else {
            return;
        };
        history.undo.remove(at)
    };
    let origin_path = stored.group.origin.clone();
    for file in &stored.group.closed {
        restore_before(file);
    }
    for path in &stored.group.open {
        if let Some(view) = open_view(workspace, path) {
            view.update(cx, |view, cx| view.undo(cx));
        }
    }
    cx.default_global::<RefactoringUndo>().redo.push(stored);
    let origin = origin_path
        .as_deref()
        .and_then(|path| open_view(workspace, path));
    if let Some(view) = origin {
        view.update(cx, |view, _| {
            view.set_workspace_undo(None);
            view.set_workspace_redo(Some(id));
        });
    }
}

/// Applies the change again after its undo, with every file it touched.
pub(crate) fn redo(workspace: &mut Workspace, id: u64, cx: &mut Context<Workspace>) {
    let stored = {
        let history = cx.default_global::<RefactoringUndo>();
        let Some(at) = history.redo.iter().position(|stored| stored.id == id) else {
            return;
        };
        history.redo.remove(at)
    };
    let origin_path = stored.group.origin.clone();
    for path in &stored.group.open {
        if let Some(view) = open_view(workspace, path) {
            view.update(cx, |view, cx| view.redo(cx));
        }
    }
    for file in &stored.group.closed {
        restore_after(file);
    }
    cx.default_global::<RefactoringUndo>().undo.push(stored);
    let origin = origin_path
        .as_deref()
        .and_then(|path| open_view(workspace, path));
    if let Some(view) = origin {
        view.update(cx, |view, _| {
            view.set_workspace_undo(Some(id));
            view.set_workspace_redo(None);
        });
    }
}

/// Writes the text a closed file held before the change, or takes a file the change
/// created away.
fn restore_before(file: &WrittenFile) {
    match &file.before {
        Some(text) => {
            let _ = text_core::write_file(&file.path, text, &file.before_format);
        }
        None => {
            let _ = std::fs::remove_file(&file.path);
        }
    }
}

/// Writes the text a closed file got from the change, and creates a file the change
/// brought into being.
fn restore_after(file: &WrittenFile) {
    if let Some(parent) = file.path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = text_core::write_file(&file.path, &file.after, &file.after_format);
}

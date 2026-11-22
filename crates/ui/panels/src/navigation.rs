//! Where the user has been: recent files, Back and Forward, and the jump to a place.
//! The state is `workspace_core::Editors`, so a test or an agent reads the same.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use ::base::IdSource;
use ::platform::CanonPath;
use editor_view::{EditorView, Spot};
use ui_kit::*;
use workspace::Workspace;
use workspace_core::{Document, DocumentId, EditorSnapshot, Editors, Place, Selection};

use crate::places;

#[derive(Default)]
pub struct Navigator {
    editors: Editors,
    ids: HashMap<CanonPath, DocumentId>,
    source: IdSource<Document>,
}

impl Global for Navigator {}

impl Navigator {
    fn id_of(&mut self, path: &CanonPath) -> DocumentId {
        if let Some(id) = self.ids.get(path) {
            return *id;
        }
        let id = self.source.next();
        self.ids.insert(path.clone(), id);
        id
    }

    /// A tab became the active one.
    pub fn activated(&mut self, path: &Path) {
        let Ok(path) = CanonPath::new(path) else {
            return;
        };
        let document = self.id_of(&path);
        if self.editors.get(document).is_none() {
            self.editors.update(EditorSnapshot {
                document,
                path,
                version: 0,
                selections: vec![Selection::caret(0)],
                visible_lines: 0..0,
            });
        }
        self.editors.activate(document);
    }

    /// Recent files, the active one first.
    pub fn recent(&self) -> Vec<PathBuf> {
        let paths = self.editors.recent();
        paths.map(|path| path.as_path().to_path_buf()).collect()
    }

    /// Files of an earlier run and the tabs it restores go behind the files used so far.
    /// A file that is gone is left out.
    fn restore_recent<'a>(&mut self, paths: impl IntoIterator<Item = &'a PathBuf>) {
        for path in paths {
            if !path.is_file() {
                continue;
            }
            if let Ok(path) = CanonPath::new(path) {
                self.editors.add_recent(path);
            }
        }
    }

    fn place(path: &Path, offset: usize) -> Option<Place> {
        let path = CanonPath::new(path).ok()?;
        Some(Place { path, offset })
    }

    pub fn jumped_from(&mut self, path: &Path, offset: usize) {
        if let Some(place) = Self::place(path, offset) {
            self.editors.jumped_from(place);
        }
    }

    pub fn back(&mut self, path: &Path, offset: usize) -> Option<Place> {
        self.editors.back(Self::place(path, offset)?)
    }

    pub fn forward(&mut self, path: &Path, offset: usize) -> Option<Place> {
        self.editors.forward(Self::place(path, offset)?)
    }
}

fn editor_of(workspace: &Workspace, path: &Path) -> Option<Entity<EditorView>> {
    workspace.view_of(path)?.downcast::<EditorView>().ok()
}

/// The active file and where its caret is.
fn here(workspace: &Workspace, cx: &App) -> Option<(PathBuf, usize)> {
    let path = workspace.active_path()?;
    let caret = editor_of(workspace, &path)
        .and_then(|editor| editor.read(cx).head_offset())
        .unwrap_or(0);
    Some((path, caret))
}

/// Opens `path` and moves the caret to `spot`. A jump leaves the place it came from for
/// Back, unless it is Back or Forward itself.
pub fn go_to(
    workspace: &mut Workspace,
    path: PathBuf,
    spot: Option<Spot>,
    remember: bool,
    window: &mut Window,
    cx: &mut Context<Workspace>,
) {
    if remember && let Some((from, caret)) = here(workspace, cx) {
        cx.default_global::<Navigator>().jumped_from(&from, caret);
        if let Some(editor) = editor_of(workspace, &from) {
            places::remember(&editor, workspace, cx);
        }
    }
    workspace.open_file(path.clone(), window, cx);
    cx.default_global::<Navigator>().activated(&path);
    if let (Some(spot), Some(editor)) = (spot, editor_of(workspace, &path)) {
        editor.update(cx, |editor, cx| editor.go_to(spot, cx));
    }
}

pub fn back(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some((path, caret)) = here(workspace, cx) else {
        return;
    };
    let place = cx.default_global::<Navigator>().back(&path, caret);
    if let Some(place) = place {
        let path = place.path.as_path().to_path_buf();
        go_to(
            workspace,
            path,
            Some(Spot::Offset(place.offset)),
            false,
            window,
            cx,
        );
    }
}

pub fn forward(workspace: &mut Workspace, window: &mut Window, cx: &mut Context<Workspace>) {
    let Some((path, caret)) = here(workspace, cx) else {
        return;
    };
    let place = cx.default_global::<Navigator>().forward(&path, caret);
    if let Some(place) = place {
        let path = place.path.as_path().to_path_buf();
        go_to(
            workspace,
            path,
            Some(Spot::Offset(place.offset)),
            false,
            window,
            cx,
        );
    }
}

/// The files recently active, the current one first.
pub fn recent(cx: &mut App) -> Vec<PathBuf> {
    cx.default_global::<Navigator>().recent()
}

/// Makes the layout of `workspace` carry Recent Files. The layout the window opened with
/// gives back its files and its tabs; each layout written later keeps the files under the
/// root of the project.
pub fn bind_recent(workspace: &mut Workspace, cx: &mut App) {
    let root = CanonPath::new(workspace.root()).map_or_else(
        |_| workspace.root().to_path_buf(),
        |root| root.as_path().to_path_buf(),
    );
    workspace.set_recent_files_source(move |app| recent_in(app, &root));
    let layout = workspace.layout();
    let saved = layout
        .recent_files
        .iter()
        .chain(layout.panes.iter().flat_map(|pane| &pane.tabs));
    cx.default_global::<Navigator>().restore_recent(saved);
}

/// The files of Recent Files under `root`, the latest first: the list a layout saves.
fn recent_in(app: &App, root: &Path) -> Vec<PathBuf> {
    let Some(navigator) = app.try_global::<Navigator>() else {
        return Vec::new();
    };
    navigator
        .recent()
        .into_iter()
        .filter(|path| path.starts_with(root))
        .collect()
}

#[cfg(test)]
mod tests {
    use ::platform::CanonPath;

    use super::Navigator;

    #[test]
    fn back_and_forward_walk_the_places_jumped_from() {
        let (first, second) = (
            test_support::scratch("nav-back-forward"),
            std::env::current_dir().unwrap(),
        );
        let mut navigator = Navigator::default();
        navigator.activated(&first);
        navigator.jumped_from(&first, 10);
        navigator.activated(&second);

        let back = navigator.back(&second, 99).unwrap();
        let first_canon = CanonPath::new(&first).unwrap();
        assert_eq!(
            (back.path.as_path(), back.offset),
            (first_canon.as_path(), 10)
        );
        let forward = navigator.forward(&first, 10).unwrap();
        assert_eq!(forward.offset, 99);
        assert!(navigator.forward(&second, 99).is_none());
    }

    #[test]
    fn recent_files_put_the_last_active_first() {
        let (first, second) = (
            test_support::scratch("nav-recent"),
            std::env::current_dir().unwrap(),
        );
        let mut navigator = Navigator::default();
        navigator.activated(&first);
        navigator.activated(&second);
        navigator.activated(&first);
        let recent = navigator.recent();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0], CanonPath::new(&first).unwrap().as_path());
    }
}
